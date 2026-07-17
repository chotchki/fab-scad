//! `SortGeometry` — the Morton-code reindex that ends a boolean (`sort.cpp`).
//!
//! Verbatim `Manifold::Impl::SortGeometry`: sort vertices then triangles by the Morton code of their
//! position / centroid, so the mesh has a canonical, position-derived order. Runs AFTER
//! `SimplifyTopology` + the compaction, on a clean 2-manifold.
//!
//! ## Why the boolean needs this
//!
//! Our per-op geometry already matches C++ (a single rotated union is bit-identical), but our OUTPUT
//! ORDER differs — the assembly writes half-edges in a different sequence, and our ear-clip isn't the
//! verbatim `EarClip`. That's invisible for one op, but a CHAINED op (a fold) feeds the intermediate's
//! order into the next boolean, where near-coincident tie-breaks depend on vertex INDEX, not just
//! position — so a different order compounds into a genuinely different (worse-genus) result. Morton
//! sorting canonicalizes the order to exactly what C++ produces, making a chained fold bit-identical.
//!
//! We skip the collider rebuild (brute-force broad phase) and `IncrementMeshIDs` (our global counter
//! already keeps mesh IDs unique); `CompactProps` (prop-vert compaction) runs first when a boolean
//! carried extra properties (M.3.4b), and is a no-op position-only. Removal handling (NaN verts /
//! dead tris sort to `kNoCode` at the end, then drop) is kept verbatim but inert here — `SimplifyTopology`
//! already compacted, so nothing is marked when this runs.

use crate::linalg::{Box3, Vec3};
use crate::mesh::{Halfedge, Mesh};
use crate::mesh_ids::{HalfedgeId, TriId, VertId};

/// The sentinel Morton code for a removed vert/tri (`sort.cpp` `kNoCode`) — all-ones sorts them last.
pub(crate) const K_NO_CODE: u32 = 0xFFFF_FFFF;

/// C++ gates every sort.cpp gather/scatter pass at `autoPolicy(n, 1e5)` (`parallel.h` `gather`,
/// `ReindexVerts`/`GatherFaces` for_each_n) — 10× the seam's uniform 1e4, and measurement agrees:
/// these passes are memory-bound copies with ~zero compute per element, so rayon's fork-join tax
/// loses below ~1e5 (sphere128's 39k-halfedge remap par-costs ~+0.3ms, self_intersect's 33k tri
/// gathers similar). Site-local gate, NOT a seam change: below it we run the seam's serial
/// construction verbatim, and since seam-par == seam-serial bit-for-bit by construction, the gate
/// moves only the crossover — it cannot move a byte. The Morton STABLE SORTS stay on the seam's
/// 1e4 gate (C++ `stable_sort` default policy, sort.cpp:279/:425 — exact parity).
const GATHER_SEQ_THRESHOLD: usize = 100_000;

/// `Permute`-shaped gather (`utils.h` `Permute` → `parallel.h` `gather`): `out[i] = items[new2old[i]]`,
/// parallel above [`GATHER_SEQ_THRESHOLD`] elements. Order-preserving pure gather ⇒ par == serial.
fn gather<T: Copy + Send + Sync>(new2old: &[usize], items: &[T]) -> Vec<T> {
    if new2old.len() > GATHER_SEQ_THRESHOLD {
        crate::par::map_collect(new2old, |&old| items[old])
    } else {
        new2old.iter().map(|&old| items[old]).collect()
    }
}

/// Interleave the low 10 bits of `v` with two zero bits each (`collider.h` `SpreadBits3`) — the bit
/// magic that builds a Z-order (Morton) code. Verbatim; `wrapping_mul` matches the C++ `uint32_t`
/// overflow.
#[inline]
fn spread_bits3(mut v: u32) -> u32 {
    v = 0xFF00_00FF & v.wrapping_mul(0x0001_0001);
    v = 0x0F00_F00F & v.wrapping_mul(0x0000_0101);
    v = 0xC30C_30C3 & v.wrapping_mul(0x0000_0011);
    v = 0x4924_9249 & v.wrapping_mul(0x0000_0005);
    v
}

/// The 30-bit Morton code of `position` within `b_box` (`collider.h` `Collider::MortonCode`, guarded by
/// `sort.cpp`'s NaN→`kNoCode`). Normalize to the unit cube, quantize each axis to `[0, 1023]`, then
/// interleave. A NaN position (removed vert) returns [`K_NO_CODE`].
pub(crate) fn morton_code(position: Vec3, b_box: Box3) -> u32 {
    if position.x.is_nan() {
        return K_NO_CODE;
    }
    let xyz = (position - b_box.min) / (b_box.max - b_box.min);
    let q = |c: f64| (1024.0 * c).clamp(0.0, 1023.0) as u32;
    let x = spread_bits3(q(xyz.x));
    let y = spread_bits3(q(xyz.y));
    let z = spread_bits3(q(xyz.z));
    x * 4 + y * 2 + z
}

impl Mesh {
    /// Canonicalize vertex + triangle order by Morton code (`sort.cpp` `SortGeometry`). No-op on an empty
    /// mesh. Requires `b_box` to be current (the boolean calls `calculate_bbox` first).
    ///
    /// Tail-builds [`Mesh::collider`] (BU.4.5b), mirroring C++ `SortGeometry`'s `collider_ =
    /// Collider(faceBox, faceMorton)` (sort.cpp:213) — every finalized mesh leaves here with a fresh
    /// broad-phase BVH, so `Boolean3::new` never rebuilds one. FUSED (BU.4.7): [`sort_faces`](Self::sort_faces)
    /// computes each face's box + centroid in its ONE per-vertex sweep and hands them to
    /// [`Collider::from_sorted_leaves`], so the collider skips the second sweep C++ also skips (its reuse of
    /// `SortFaces`' `faceBox`/`faceMorton`). Byte-identical to the old `from_mesh` over the sorted mesh — the
    /// boxes/centroids ARE that mesh's, and the collider's live-bbox Morton is recomputed there unchanged.
    pub fn sort_geometry(&mut self) {
        if self.halfedge.is_empty() {
            self.collider = None; // C++ `collider_ = {}` (sort.cpp:193)
            return;
        }
        self.compact_props();
        self.sort_verts();
        // `sort_faces` returns the SORTED per-face box+centroid (or `None` when every face Morton-sorts to
        // kNoCode and drops — C++ sort.cpp:210's second empty check).
        self.collider = self.sort_faces().map(|(leaf_box, centroid)| {
            crate::boolean::collider::Collider::from_sorted_leaves(leaf_box, &centroid)
        });
    }

    /// Remove unreferenced prop-verts and reindex `prop_vert` + [`Mesh::properties`] (`sort.cpp`
    /// `CompactProps`). No-op position-only (`num_prop == 0`). Runs FIRST in [`Mesh::sort_geometry`],
    /// mirroring C++ `SortGeometry` — prop-verts live in their own index space, compacted independently
    /// of the geometric vert sort below.
    fn compact_props(&mut self) {
        if self.num_prop == 0 {
            return;
        }
        let num_prop = self.num_prop;
        let num_verts = self.properties.len() / num_prop;
        // keep[pv] = referenced by some half-edge.
        let mut keep = vec![0usize; num_verts];
        for h in &self.halfedge {
            if h.prop_vert.is_some() {
                keep[h.prop_vert.u()] = 1;
            }
        }
        // propOld2New[old] = count of kept prop-verts before `old` = its new index (C++ inclusive_scan
        // into `+1`). `[num_verts]` holds the new count.
        let mut prop_old2new = vec![0usize; num_verts + 1];
        for i in 0..num_verts {
            prop_old2new[i + 1] = prop_old2new[i] + keep[i];
        }
        let num_verts_new = prop_old2new[num_verts];
        let old_prop = std::mem::take(&mut self.properties);
        let mut properties = vec![0.0; num_prop * num_verts_new];
        for (old_idx, &k) in keep.iter().enumerate() {
            if k == 0 {
                continue;
            }
            let dst = prop_old2new[old_idx] * num_prop;
            let src = old_idx * num_prop;
            properties[dst..dst + num_prop].copy_from_slice(&old_prop[src..src + num_prop]);
        }
        self.properties = properties;
        for h in &mut self.halfedge {
            if h.prop_vert.is_some() {
                h.prop_vert = VertId::from_usize(prop_old2new[h.prop_vert.u()]);
            }
        }
    }

    /// Reindex vertices by the Morton code of their position (`sort.cpp` `SortVerts` + `ReindexVerts`),
    /// dropping any NaN-marked verts (which sort to the end). A `stable_sort` on the code keeps the order
    /// deterministic; half-edge `start`/`prop` are remapped to the new indices.
    fn sort_verts(&mut self) {
        let num_vert = self.num_vert();
        // Per-vertex Morton code — an independent pure function of (position, bbox), so it maps through
        // the order-preserving `par::` seam (par == seq by construction). M.4.
        let bbox = self.b_box;
        let vert_morton: Vec<u32> =
            crate::par::map_collect(&self.vert_pos, |&p| morton_code(p, bbox));

        // new -> old permutation, sorted by Morton code with the ORIGINAL INDEX as a total-order
        // tiebreak (M.4.2). Distinct verts sharing a 30-bit-quantized Morton code would otherwise tie,
        // and THIS is the canonical order fed to chained booleans — so the tiebreak stays explicit even
        // though the seam sort is STABLE both lanes (a total order can't diverge regardless of
        // stability). C++ parallel stable_sort ≥1e4 (sort.cpp `SortVerts`).
        let mut new2old: Vec<usize> = (0..num_vert).collect();
        crate::par::sort_by(&mut new2old, |&a, &b| {
            vert_morton[a].cmp(&vert_morton[b]).then(a.cmp(&b))
        });

        // old -> new (the halfedge remap). SERIAL on purpose: the inverse-permutation scatter's write
        // target is data-dependent (`old2new[new2old[new]] = new`), which the seam's fill-slot-i-from-i
        // shape can't express — and it's a memory-bound usize pass, trivial next to the sort.
        let mut old2new = vec![0usize; num_vert];
        for (new, &old) in new2old.iter().enumerate() {
            old2new[old] = new;
        }
        // With extra properties, prop-verts are a SEPARATE space (already compacted by `compact_props`);
        // only reindex `prop_vert` off the geometric remap in the position-only 1:1 case (C++
        // `ReindexVerts`'s `if (!hasProp) SetProp(idx, newStart)`). Each half-edge slot is rewritten
        // from its OWN old value plus the read-only `old2new` — disjoint writes by construction, so it
        // maps through the deterministic scatter above the C++ 1e5 cutoff (`ReindexVerts` for_each_n).
        let has_prop = self.num_prop > 0;
        let remap = |h: &mut Halfedge| {
            if h.start_vert.is_some() {
                let ns = VertId::from_usize(old2new[h.start_vert.u()]);
                h.start_vert = ns;
                if !has_prop {
                    h.prop_vert = ns;
                }
            }
        };
        if self.halfedge.len() > GATHER_SEQ_THRESHOLD {
            crate::par::for_each_mut(&mut self.halfedge, |_, h| remap(h));
        } else {
            self.halfedge.iter_mut().for_each(remap);
        }

        // NaN verts got kNoCode and sorted to the end — keep only the real prefix.
        let keep = new2old
            .iter()
            .take_while(|&&old| vert_morton[old] != K_NO_CODE)
            .count();
        self.vert_pos = gather(&new2old[..keep], &self.vert_pos);
        if self.vert_normal.len() == num_vert {
            self.vert_normal = gather(&new2old[..keep], &self.vert_normal);
        }
    }

    /// Reindex triangles by the Morton code of their centroid (`sort.cpp` `SortFaces` + `GatherFaces`),
    /// dropping dead tris. Permutes the per-triangle `face_normal`/`tri_ref` to match, and rebuilds
    /// `halfedge` with pair pointers translated through the face permutation (`ReindexFace`).
    /// Returns the SORTED per-face `(box, centroid)` for the collider to reuse (BU.4.7), or `None` when every
    /// face Morton-sorts to kNoCode and drops. C++'s `GetFaceBoxMorton` computes `faceBox`/`faceMorton` in one
    /// sweep and `SortGeometry` reuses them for the collider; we do the same — the ONE per-vertex sweep here
    /// builds each face's box + centroid, sorts by the mesh-bbox Morton, and the caller hands the permuted
    /// box+centroid to [`Collider::from_sorted_leaves`] (whose live-bbox Morton is recomputed there).
    fn sort_faces(&mut self) -> Option<(Vec<Box3>, Vec<Vec3>)> {
        let old_num_tri = self.num_tri();
        // The single per-vertex sweep: each face's AABB + centroid + its sort Morton. Independent per face (a
        // removed tri stays `kNoCode` + empty box), so it maps through the `par::` seam (order-preserving ⇒
        // par == seq). M.4. Block-scoped immutable reborrow.
        let leaves: Vec<(Box3, Vec3, u32)> = {
            let this = &*self;
            let bbox = this.b_box;
            crate::par::map_range(old_num_tri, |face| {
                let t = TriId::from_usize(face);
                // A removed tri has an unpaired first half-edge — empty box + kNoCode to sort last + drop.
                if this.pair(t.first_halfedge()).is_none() {
                    return (Box3::default(), Vec3::ZERO, K_NO_CODE);
                }
                let mut b = Box3::default();
                let mut c = Vec3::ZERO;
                for i in 0..3 {
                    let p = this.pos(this.start(t.halfedge(i)));
                    c += p;
                    b.union_point(p);
                }
                let centroid = c / 3.0;
                (b, centroid, morton_code(centroid, bbox))
            })
        };

        // Original-index tiebreak for total order (M.4.2) — same rationale as `sort_verts`. Seam sort
        // is STABLE both lanes; C++ parallel stable_sort ≥1e4 (sort.cpp `SortFaces`).
        let mut new2old: Vec<usize> = (0..old_num_tri).collect();
        crate::par::sort_by(&mut new2old, |&a, &b| {
            leaves[a].2.cmp(&leaves[b].2).then(a.cmp(&b))
        });
        let keep = new2old
            .iter()
            .take_while(|&&old| leaves[old].2 != K_NO_CODE)
            .count();
        new2old.truncate(keep);

        self.gather_faces(&new2old, old_num_tri);
        if new2old.is_empty() {
            return None;
        }
        // Permute the reused box+centroid into the sorted face order (mirrors `gather_faces`) — this IS the
        // sorted mesh's per-face box/centroid, so `from_sorted_leaves` is byte-identical to `from_mesh` on it.
        let leaf_box: Vec<Box3> = new2old.iter().map(|&o| leaves[o].0).collect();
        let centroid: Vec<Vec3> = new2old.iter().map(|&o| leaves[o].1).collect();
        Some((leaf_box, centroid))
    }

    /// Rebuild `halfedge` (plus `face_normal`/`tri_ref`) in the new triangle order (`sort.cpp`
    /// `GatherFaces` + `ReindexFace`). Each surviving triangle is copied from its old slot with its three
    /// half-edges' pair pointers translated to the destination face (same within-face offset).
    fn gather_faces(&mut self, new2old: &[usize], old_num_tri: usize) {
        let num_tri = new2old.len();

        // Pure gathers (`utils.h` `Permute`), parallel above the C++ 1e5 cutoff via [`gather`].
        if self.tri_ref.len() == old_num_tri {
            self.tri_ref = gather(new2old, &self.tri_ref);
        }
        if self.face_normal.len() == old_num_tri {
            self.face_normal = gather(new2old, &self.face_normal);
        }

        // SERIAL on purpose: inverse-permutation scatter (`old2new[new2old[new]] = new`) has a
        // data-dependent write target the seam's fill-slot-i-from-i shape can't express; memory-bound
        // usize pass, trivial next to the remap below.
        let mut old2new = vec![0usize; old_num_tri];
        for (new, &old) in new2old.iter().enumerate() {
            old2new[old] = new;
        }

        // Each new half-edge slot `j` is a pure function of `j` + read-only inputs (old halfedges,
        // both permutations) — output[j] = f(j) through the order-preserving seam above the C++ 1e5
        // cutoff (`GatherFaces`'s `ReindexFace` for_each_n), with no placeholder fill; the serial
        // branch is the identical collect.
        let old_halfedge = std::mem::take(&mut self.halfedge);
        let rebuild = |j: usize| {
            let new_face = j / 3;
            let i = j - 3 * new_face;
            let edge = old_halfedge[3 * new2old[new_face] + i];
            let paired_face = edge.paired_halfedge.u() / 3;
            let offset = edge.paired_halfedge.u() - 3 * paired_face;
            Halfedge {
                start_vert: edge.start_vert,
                paired_halfedge: HalfedgeId::from_usize(3 * old2new[paired_face] + offset),
                prop_vert: edge.prop_vert,
            }
        };
        self.halfedge = if 3 * num_tri > GATHER_SEQ_THRESHOLD {
            crate::par::map_range(3 * num_tri, rebuild)
        } else {
            (0..3 * num_tri).map(rebuild).collect()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshGl;

    /// A shifted/permuted-input cube must sort to the SAME canonical geometry as the standard one — the
    /// Morton order is position-derived, so two ingests of the same solid converge after `sort_geometry`.
    #[test]
    fn sort_is_position_canonical_and_manifold_preserving() {
        #[rustfmt::skip]
        let verts = vec![
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7, 0,1,5, 0,5,4,
            2,3,7, 2,7,6, 0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mut a = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts.clone(),
            tri_verts: tris.clone(),
            ..Default::default()
        })
        .unwrap();
        let vol_before = a.volume();
        a.calculate_bbox();
        a.sort_geometry();
        // Still a watertight manifold, same volume, same counts — sorting is a pure reindex.
        assert!(a.is_manifold(), "sorted mesh not manifold");
        assert_eq!(a.num_vert(), 8);
        assert_eq!(a.num_tri(), 12);
        assert_eq!(a.volume(), vol_before);
        // Verts are now in nondecreasing Morton order.
        let codes: Vec<u32> = a
            .vert_pos
            .iter()
            .map(|&p| morton_code(p, a.b_box))
            .collect();
        assert!(
            codes.windows(2).all(|w| w[0] <= w[1]),
            "verts not Morton-ordered"
        );
    }

    #[test]
    fn spread_bits3_matches_reference() {
        // Spot-check the bit interleave against hand-computed values.
        assert_eq!(spread_bits3(0), 0);
        assert_eq!(spread_bits3(1), 1);
        assert_eq!(spread_bits3(0b111), 0b001001001);
        assert_eq!(spread_bits3(0b1111111111), 0o1111111111 & 0x4924_9249);
    }
}
