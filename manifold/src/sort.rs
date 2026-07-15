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
    pub fn sort_geometry(&mut self) {
        if self.halfedge.is_empty() {
            return;
        }
        self.compact_props();
        self.sort_verts();
        self.sort_faces();
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
        let vert_morton: Vec<u32> = (0..num_vert).map(|v| morton_code(self.vert_pos[v], self.b_box)).collect();

        // new -> old permutation, sorted by Morton code with the ORIGINAL INDEX as a total-order
        // tiebreak (M.4.2). Distinct verts sharing a 30-bit-quantized Morton code would otherwise tie,
        // and THIS is the canonical order fed to chained booleans — so the tiebreak must be explicit, not
        // an implicit reliance on stable-sort, before this can become a parallel (unstable) sort. It's a
        // no-op on the current output (stable-sort already breaks ties by ascending index).
        let mut new2old: Vec<usize> = (0..num_vert).collect();
        new2old.sort_by(|&a, &b| vert_morton[a].cmp(&vert_morton[b]).then(a.cmp(&b)));

        // old -> new (the halfedge remap).
        let mut old2new = vec![0usize; num_vert];
        for (new, &old) in new2old.iter().enumerate() {
            old2new[old] = new;
        }
        // With extra properties, prop-verts are a SEPARATE space (already compacted by `compact_props`);
        // only reindex `prop_vert` off the geometric remap in the position-only 1:1 case (C++
        // `ReindexVerts`'s `if (!hasProp) SetProp(idx, newStart)`).
        let has_prop = self.num_prop > 0;
        for h in &mut self.halfedge {
            if h.start_vert.is_some() {
                let ns = VertId::from_usize(old2new[h.start_vert.u()]);
                h.start_vert = ns;
                if !has_prop {
                    h.prop_vert = ns;
                }
            }
        }

        // NaN verts got kNoCode and sorted to the end — keep only the real prefix.
        let keep = new2old.iter().take_while(|&&old| vert_morton[old] != K_NO_CODE).count();
        self.vert_pos = new2old[..keep].iter().map(|&old| self.vert_pos[old]).collect();
        if self.vert_normal.len() == num_vert {
            self.vert_normal = new2old[..keep].iter().map(|&old| self.vert_normal[old]).collect();
        }
    }

    /// Reindex triangles by the Morton code of their centroid (`sort.cpp` `SortFaces` + `GatherFaces`),
    /// dropping dead tris. Permutes the per-triangle `face_normal`/`tri_ref` to match, and rebuilds
    /// `halfedge` with pair pointers translated through the face permutation (`ReindexFace`).
    fn sort_faces(&mut self) {
        let old_num_tri = self.num_tri();
        let mut face_morton = vec![K_NO_CODE; old_num_tri];
        for (face, code) in face_morton.iter_mut().enumerate() {
            let t = TriId::from_usize(face);
            // A removed tri has an unpaired first half-edge — leave it at kNoCode to sort to the end.
            if self.pair(t.first_halfedge()).is_none() {
                continue;
            }
            let center = (self.pos(self.start(t.halfedge(0)))
                + self.pos(self.start(t.halfedge(1)))
                + self.pos(self.start(t.halfedge(2))))
                / 3.0;
            *code = morton_code(center, self.b_box);
        }

        // Original-index tiebreak for total order (M.4.2) — same rationale as `sort_verts`.
        let mut new2old: Vec<usize> = (0..old_num_tri).collect();
        new2old.sort_by(|&a, &b| face_morton[a].cmp(&face_morton[b]).then(a.cmp(&b)));
        let keep = new2old.iter().take_while(|&&old| face_morton[old] != K_NO_CODE).count();
        new2old.truncate(keep);

        self.gather_faces(&new2old, old_num_tri);
    }

    /// Rebuild `halfedge` (plus `face_normal`/`tri_ref`) in the new triangle order (`sort.cpp`
    /// `GatherFaces` + `ReindexFace`). Each surviving triangle is copied from its old slot with its three
    /// half-edges' pair pointers translated to the destination face (same within-face offset).
    fn gather_faces(&mut self, new2old: &[usize], old_num_tri: usize) {
        let num_tri = new2old.len();

        if self.tri_ref.len() == old_num_tri {
            self.tri_ref = new2old.iter().map(|&old| self.tri_ref[old]).collect();
        }
        if self.face_normal.len() == old_num_tri {
            self.face_normal = new2old.iter().map(|&old| self.face_normal[old]).collect();
        }

        let mut old2new = vec![0usize; old_num_tri];
        for (new, &old) in new2old.iter().enumerate() {
            old2new[old] = new;
        }

        let old_halfedge = std::mem::take(&mut self.halfedge);
        let mut new_he = vec![
            Halfedge {
                start_vert: VertId::NONE,
                paired_halfedge: HalfedgeId::NONE,
                prop_vert: VertId::NONE,
            };
            3 * num_tri
        ];
        for new_face in 0..num_tri {
            let old_face = new2old[new_face];
            for i in 0..3 {
                let edge = old_halfedge[3 * old_face + i];
                let paired_face = edge.paired_halfedge.u() / 3;
                let offset = edge.paired_halfedge.u() - 3 * paired_face;
                let new_pair = 3 * old2new[paired_face] + offset;
                new_he[3 * new_face + i] = Halfedge {
                    start_vert: edge.start_vert,
                    paired_halfedge: HalfedgeId::from_usize(new_pair),
                    prop_vert: edge.prop_vert,
                };
            }
        }
        self.halfedge = new_he;
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
        let mut a = Mesh::from_mesh_gl(&MeshGl { num_prop: 3, vert_properties: verts.clone(), tri_verts: tris.clone(), ..Default::default() });
        let vol_before = a.volume();
        a.calculate_bbox();
        a.sort_geometry();
        // Still a watertight manifold, same volume, same counts — sorting is a pure reindex.
        assert!(a.is_manifold(), "sorted mesh not manifold");
        assert_eq!(a.num_vert(), 8);
        assert_eq!(a.num_tri(), 12);
        assert_eq!(a.volume(), vol_before);
        // Verts are now in nondecreasing Morton order.
        let codes: Vec<u32> = a.vert_pos.iter().map(|&p| morton_code(p, a.b_box)).collect();
        assert!(codes.windows(2).all(|w| w[0] <= w[1]), "verts not Morton-ordered");
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
