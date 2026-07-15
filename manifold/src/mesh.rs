//! The mesh spine — Manifold's `Manifold::Impl`, here `Mesh` (a half-edge mesh).
//!
//! The structure everything mutates: vertices, the half-edge connectivity (`CreateHalfedges`), the
//! bounding box, and the property (color) channels threaded through booleans. Round-trips to/from
//! `MeshGl` (the flat vert + index + property buffer). Answers `is_manifold` (the validity gate) and
//! `volume`/`surface_area` (the K.0 differential targets). No booleans here — this is the spine the
//! boolean reassembly writes onto (R1+).
//!
//! REPRESENTATION vs Manifold: Manifold's `Halfedges` is an SoA that DERIVES `endVert` from the next
//! half-edge in the triangle (`End(e) = Start(NextHalfedge(e))`). We mirror that exactly — a half-edge
//! stores only `(start_vert, paired_halfedge, prop_vert)`, and `end(e)` derives — so `CheckHalfedges`
//! transliterates 1:1 and the boolean port reads `Start/End/Pair/Prop` unchanged. Faces are 3
//! consecutive half-edges ([`TriId::halfedge`]), CCW from outside; `NextHalfedge`/`PrevHalfedge` live on
//! [`HalfedgeId`].
//!
//! TYPED INDICES: every index is a [`VertId`]/[`HalfedgeId`]/[`TriId`] ([`crate::mesh_ids`]), NOT a raw
//! `i32` — so a vertex can't be passed where a half-edge is expected. Zero runtime cost
//! (`#[repr(transparent)]`), so the K.0 output stays bit-identical.

use crate::linalg::{Box3, Vec3};
use crate::mesh_ids::{HalfedgeId, TriId, VertId};

/// A single half-edge. `end` is DERIVED (see the module doc), so only these three fields are stored;
/// [`VertId::NONE`]/[`HalfedgeId::NONE`] (`-1`) is the removed/unpaired sentinel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Halfedge {
    /// The vertex this half-edge starts at, or [`VertId::NONE`] if removed.
    pub start_vert: VertId,
    /// The opposite half-edge, or [`HalfedgeId::NONE`] if unpaired.
    pub paired_halfedge: HalfedgeId,
    /// The property-vertex (== `start_vert` in the 1:1 MeshGL model).
    pub prop_vert: VertId,
}

/// The half-edge mesh — Manifold's `Impl`. Position + connectivity + bounds; the boolean core (R1+)
/// grows this.
#[derive(Clone, Debug)]
pub struct Mesh {
    /// Vertex positions (Manifold `vertPos_`).
    pub vert_pos: Vec<Vec3>,
    /// The half-edges, 3 per triangle (Manifold `halfedge_`).
    pub halfedge: Vec<Halfedge>,
    /// Properties per vertex, `>= 3` (position is the first 3, NOT stored here — `vert_pos` holds it);
    /// `num_prop == 3` means position-only.
    pub num_prop: usize,
    /// The EXTRA (non-position) properties, interleaved with stride `num_prop - 3`, one row per vert.
    /// Empty when `num_prop == 3`. Carried verbatim for the round-trip.
    pub props_extra: Vec<f64>,
    /// Axis-aligned bounding box (Manifold `bBox_`).
    pub b_box: Box3,
    /// Per-triangle face normals (Manifold `faceNormal_`) — the perturbation vectors the boolean's
    /// symbolic tie-break reads. Empty until [`Mesh::calculate_face_normals`] runs.
    pub face_normal: Vec<Vec3>,
    /// Per-vertex angle-weighted pseudo-normals (Manifold `vertNormal_`) — the other perturbation
    /// input, consulted by `Shadow01` at exact-coordinate ties. Empty until
    /// [`Mesh::calculate_vert_normals`] runs.
    pub vert_normal: Vec<Vec3>,
    /// The mesh's length-scale epsilon (Manifold `epsilon_`); `-1` = unset. Set by [`Mesh::set_epsilon`].
    pub epsilon: f64,
    /// The merge/collinearity tolerance (Manifold `tolerance_`); `-1` = unset. Monotone-nondecreasing
    /// under [`Mesh::set_epsilon`] (it only ever `max`es up, never shrinks a user-supplied tolerance).
    pub tolerance: f64,
}

impl Default for Mesh {
    fn default() -> Self {
        // `epsilon`/`tolerance` default to Manifold's `-1` "unset" sentinel, NOT `0.0` — a real
        // computed epsilon is always `>= 0`, so `-1` is an unambiguous "SetEpsilon hasn't run".
        Self {
            vert_pos: Vec::new(),
            halfedge: Vec::new(),
            num_prop: 0,
            props_extra: Vec::new(),
            b_box: Box3::default(),
            face_normal: Vec::new(),
            vert_normal: Vec::new(),
            epsilon: -1.0,
            tolerance: -1.0,
        }
    }
}

impl Mesh {
    /// Number of triangles (`halfedge.len() / 3`).
    #[inline]
    pub fn num_tri(&self) -> usize {
        self.halfedge.len() / 3
    }

    /// Number of vertices.
    #[inline]
    pub fn num_vert(&self) -> usize {
        self.vert_pos.len()
    }

    /// Number of undirected edges (`halfedge.len() / 2`).
    #[inline]
    pub fn num_edge(&self) -> usize {
        self.halfedge.len() / 2
    }

    /// Empty mesh (no half-edges)?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.halfedge.is_empty()
    }

    /// Every half-edge id, in order — `0..halfedge.len()` typed.
    #[inline]
    pub fn halfedge_ids(&self) -> impl Iterator<Item = HalfedgeId> {
        (0..self.halfedge.len()).map(HalfedgeId::from_usize)
    }

    // --- Half-edge accessors, mirroring Manifold's `Halfedges` (end is DERIVED). ---

    /// Position of vertex `v`.
    #[inline]
    pub fn pos(&self, v: VertId) -> Vec3 {
        self.vert_pos[v.u()]
    }

    /// Start vertex of half-edge `e`.
    #[inline]
    pub fn start(&self, e: HalfedgeId) -> VertId {
        self.halfedge[e.u()].start_vert
    }

    /// End vertex of half-edge `e` — the start of the NEXT half-edge in the triangle (derived).
    #[inline]
    pub fn end(&self, e: HalfedgeId) -> VertId {
        self.start(e.next())
    }

    /// Paired (opposite) half-edge of `e`, or [`HalfedgeId::NONE`].
    #[inline]
    pub fn pair(&self, e: HalfedgeId) -> HalfedgeId {
        self.halfedge[e.u()].paired_halfedge
    }

    /// Property vertex of half-edge `e`.
    #[inline]
    pub fn prop(&self, e: HalfedgeId) -> VertId {
        self.halfedge[e.u()].prop_vert
    }

    // --- Mutators the boolean assembly writes through (Manifold's `Halfedges::Set*`). The result mesh's
    // `halfedge` is pre-sized, then `Face2Tri` fills start/prop/pair per output half-edge. `end` stays
    // derived, so writing the three starts of a triangle in CCW order fixes its ends for free. ---

    /// Set the start vertex of half-edge `e`.
    #[inline]
    pub fn set_start(&mut self, e: HalfedgeId, v: VertId) {
        self.halfedge[e.u()].start_vert = v;
    }

    /// Set the paired half-edge of `e`.
    #[inline]
    pub fn set_pair(&mut self, e: HalfedgeId, p: HalfedgeId) {
        self.halfedge[e.u()].paired_halfedge = p;
    }

    /// Set the property vertex of `e`.
    #[inline]
    pub fn set_prop(&mut self, e: HalfedgeId, p: VertId) {
        self.halfedge[e.u()].prop_vert = p;
    }

    /// Set the END vertex of `e` — since `end` is derived (`= Start(next(e))`), this writes the START of
    /// the next half-edge in the triangle (Manifold's `Halfedges::SetEnd`). The topology-surgery in
    /// [`crate::boolean::edge_op`] leans on this heavily (edge collapse/swap repoint verts by their ends).
    #[inline]
    pub fn set_end(&mut self, e: HalfedgeId, v: VertId) {
        self.set_start(e.next(), v);
    }

    /// Set all three fields of half-edge `e` at once (Manifold's `Halfedges::Set`). `edge_op` uses this
    /// to MARK a half-edge removed (`Set(e, NONE, NONE, …)`), the collapse/dedup sentinel.
    #[inline]
    pub fn set_halfedge(&mut self, e: HalfedgeId, start: VertId, pair: HalfedgeId, prop: VertId) {
        self.halfedge[e.u()] = Halfedge {
            start_vert: start,
            paired_halfedge: pair,
            prop_vert: prop,
        };
    }

    /// Drop vertices referenced by no half-edge, COMPACTING `vert_pos` and reindexing the half-edges
    /// (Manifold's `RemoveUnreferencedVerts` only NaNs them in place, leaning on the later `SortGeometry`
    /// to compact — we skip `SortGeometry` for GATE-A, so we compact here directly). Same final vertex
    /// SET; the order is arbitrary either way (we also skip the Morton reindex), which the
    /// order-independent gates (`volume`/`genus`/residual/`is_manifold`) don't care about. Keeps
    /// [`crate::check::genus`] exact — it counts `vert_pos.len()`, so a stray dangling vert would skew χ.
    pub fn remove_unreferenced_verts(&mut self) {
        let n = self.num_vert();
        let mut keep = vec![false; n];
        for h in &self.halfedge {
            if h.start_vert.is_some() {
                keep[h.start_vert.u()] = true;
            }
        }
        let mut remap = vec![VertId::NONE; n];
        let mut new_pos = Vec::new();
        for (old, &k) in keep.iter().enumerate() {
            if k {
                remap[old] = VertId::from_usize(new_pos.len());
                new_pos.push(self.vert_pos[old]);
            }
        }
        for h in &mut self.halfedge {
            if h.start_vert.is_some() {
                h.start_vert = remap[h.start_vert.u()];
            }
            // prop_vert == start_vert in the 1:1 MeshGL model, so it remaps the same way.
            if h.prop_vert.is_some() && h.prop_vert.u() < n {
                h.prop_vert = remap[h.prop_vert.u()];
            }
        }
        self.vert_pos = new_pos;
    }

    /// Drop triangles the topology-surgery marked removed, compacting `halfedge` + `face_normal` and
    /// reindexing every surviving pair pointer ([`crate::boolean::edge_op`] cleanup tail). Manifold marks
    /// a removed triangle by setting all three of its half-edges to the `NONE` sentinel (`vertPos` NaN'd
    /// separately) and defers compaction to a later `SortGeometry`/`Finish`; we skip `SortGeometry`, so we
    /// compact here. A triangle is dead iff its first half-edge's `start` is `NONE` (Manifold marks the
    /// whole triple together). The collapse/dedup passes keep the survivors' pairs pointing only at other
    /// survivors, so the reindexed pairing stays a valid manifold; a `NONE` pair is carried through.
    pub fn remove_dead_triangles(&mut self) {
        let old_len = self.halfedge.len();
        let num_tri = old_len / 3;
        // old half-edge index → new (NONE for a dead triangle's half-edges).
        let mut he_remap = vec![HalfedgeId::NONE; old_len];
        let mut next = 0i32;
        for tri in 0..num_tri {
            if self.halfedge[3 * tri].start_vert.is_none() {
                continue; // dead triangle
            }
            for i in 0..3 {
                he_remap[3 * tri + i] = HalfedgeId::new(next + i as i32);
            }
            next += 3;
        }
        let has_normals = !self.face_normal.is_empty();
        let mut new_he = Vec::with_capacity(next as usize);
        let mut new_fn = Vec::with_capacity(next as usize / 3);
        for tri in 0..num_tri {
            if self.halfedge[3 * tri].start_vert.is_none() {
                continue;
            }
            for i in 0..3 {
                let h = self.halfedge[3 * tri + i];
                let pair = if h.paired_halfedge.is_none() {
                    HalfedgeId::NONE
                } else {
                    he_remap[h.paired_halfedge.u()]
                };
                new_he.push(Halfedge {
                    start_vert: h.start_vert,
                    paired_halfedge: pair,
                    prop_vert: h.prop_vert,
                });
            }
            if has_normals {
                new_fn.push(self.face_normal[tri]);
            }
        }
        self.halfedge = new_he;
        if has_normals {
            self.face_normal = new_fn;
        }
    }

    /// Build the half-edge connectivity from triangle vertex indices, pairing opposite half-edges.
    ///
    /// Deterministic clean-mesh pairing: group the two directed half-edges of every undirected edge
    /// `{min,max}` and link them. A manifold edge has exactly one `a→b` and one `b→a` → unique pairing;
    /// anything else (boundary, >2 incident, same-direction duplicate) is left `NONE`, which
    /// [`Mesh::is_manifold`] then rejects. (Opposed-triangle removal is R1 — see the module doc.)
    pub fn create_halfedges(&mut self, tri_verts: &[[u32; 3]]) {
        use std::collections::BTreeMap;

        let num_he = 3 * tri_verts.len();
        let mut he = Vec::with_capacity(num_he);
        // (start, end) of each half-edge, kept locally for keying — `end` isn't stored on Halfedge.
        let mut ends: Vec<VertId> = Vec::with_capacity(num_he);
        for tri in tri_verts {
            for i in 0..3 {
                let start = VertId::new(tri[i] as i32);
                let end = VertId::new(tri[(i + 1) % 3] as i32);
                he.push(Halfedge {
                    start_vert: start,
                    paired_halfedge: HalfedgeId::NONE,
                    prop_vert: start,
                });
                ends.push(end);
            }
        }

        // Group half-edge indices by undirected-edge key. BTreeMap = deterministic iteration.
        let mut groups: BTreeMap<(VertId, VertId), Vec<usize>> = BTreeMap::new();
        for (idx, h) in he.iter().enumerate() {
            let (a, b) = (h.start_vert, ends[idx]);
            let key = if a < b { (a, b) } else { (b, a) };
            groups.entry(key).or_default().push(idx);
        }
        for idxs in groups.values() {
            if idxs.len() == 2 {
                let (e0, e1) = (idxs[0], idxs[1]);
                // Only a genuine reverse pair (opposite directions) links; a same-direction
                // duplicate is non-manifold and stays unpaired.
                if he[e0].start_vert == ends[e1] && ends[e0] == he[e1].start_vert {
                    he[e0].paired_halfedge = HalfedgeId::from_usize(e1);
                    he[e1].paired_halfedge = HalfedgeId::from_usize(e0);
                }
            }
        }
        self.halfedge = he;
    }

    /// Recompute the bounding box from `vert_pos`, ignoring verts whose x is NaN (Manifold
    /// `CalculateBBox`, whose reduce skips `isnan(a.x)`).
    pub fn calculate_bbox(&mut self) {
        let mut min = Vec3::splat(f64::INFINITY);
        let mut max = Vec3::splat(f64::NEG_INFINITY);
        for &p in &self.vert_pos {
            if p.x.is_nan() {
                continue;
            }
            min = min.cmin(p);
            max = max.cmax(p);
        }
        self.b_box = Box3 { min, max };
    }

    /// Is this an oriented manifold with consistent data structures? (Manifold `IsManifold`.)
    /// Empty is manifold; a non-multiple-of-3 half-edge count is not; else every half-edge passes
    /// the [`Mesh::check_halfedge`] pair-consistency predicate.
    pub fn is_manifold(&self) -> bool {
        if self.halfedge.is_empty() {
            return true;
        }
        if !self.halfedge.len().is_multiple_of(3) {
            return false;
        }
        self.halfedge_ids().all(|e| self.check_halfedge(e))
    }

    /// The per-half-edge manifold predicate (Manifold `CheckHalfedges`): a removed triple is fine;
    /// otherwise the pair must be mutual and reverse each other (`start == End(pair)`, `end ==
    /// Start(pair)`, `start != end`).
    fn check_halfedge(&self, edge: HalfedgeId) -> bool {
        let start = self.start(edge);
        let end = self.end(edge);
        let pair = self.pair(edge);
        if start.is_none() && end.is_none() && pair.is_none() {
            return true;
        }
        if self.start(edge.next()).is_none() || self.start(edge.next().next()).is_none() {
            return false;
        }
        if pair.is_none() {
            return false;
        }
        let mut good = true;
        good &= self.pair(pair) == edge;
        good &= start != end;
        good &= start == self.end(pair);
        good &= end == self.start(pair);
        good
    }

    /// Signed volume via the divergence theorem, Kahan-summed for determinism (Manifold
    /// `GetProperty(Volume)`): per triangle `dot(cross(v1 − v0, v2 − v0), v0) / 6`, compensated sum.
    pub fn volume(&self) -> f64 {
        if self.is_empty() {
            return 0.0;
        }
        let mut value = 0.0;
        let mut comp = 0.0;
        for tri in 0..self.num_tri() {
            let t = TriId::from_usize(tri);
            let v = self.pos(self.start(t.halfedge(0)));
            let e1 = self.pos(self.start(t.halfedge(1))) - v;
            let e2 = self.pos(self.start(t.halfedge(2))) - v;
            let value1 = e1.cross(e2).dot(v) / 6.0;
            let t = value + value1;
            comp += (value - t) + value1;
            value = t;
        }
        value + comp
    }

    /// Surface area, Kahan-summed (Manifold `GetProperty(SurfaceArea)`): per triangle
    /// `length(cross(v1 − v0, v2 − v0)) / 2`.
    pub fn surface_area(&self) -> f64 {
        if self.is_empty() {
            return 0.0;
        }
        let mut value = 0.0;
        let mut comp = 0.0;
        for tri in 0..self.num_tri() {
            let t = TriId::from_usize(tri);
            let v = self.pos(self.start(t.halfedge(0)));
            let e1 = self.pos(self.start(t.halfedge(1))) - v;
            let e2 = self.pos(self.start(t.halfedge(2))) - v;
            let value1 = e1.cross(e2).length() / 2.0;
            let t = value + value1;
            comp += (value - t) + value1;
            value = t;
        }
        value + comp
    }

    /// Ingest a `MeshGl` (flat buffers) into the spine: extract positions, carry extra properties,
    /// build connectivity, compute the bbox. Panics if `num_prop < 3` or the buffers are ragged.
    pub fn from_mesh_gl(m: &MeshGl) -> Mesh {
        assert!(
            m.num_prop >= 3,
            "MeshGl.num_prop must be >= 3 (got {})",
            m.num_prop
        );
        assert!(
            m.vert_properties.len().is_multiple_of(m.num_prop),
            "vert_properties not a multiple of num_prop"
        );
        assert!(
            m.tri_verts.len().is_multiple_of(3),
            "tri_verts not a multiple of 3"
        );

        let n_vert = m.vert_properties.len() / m.num_prop;
        let extra = m.num_prop - 3;
        let mut vert_pos = Vec::with_capacity(n_vert);
        let mut props_extra = Vec::with_capacity(n_vert * extra);
        for v in 0..n_vert {
            let o = v * m.num_prop;
            vert_pos.push(Vec3::new(
                m.vert_properties[o],
                m.vert_properties[o + 1],
                m.vert_properties[o + 2],
            ));
            props_extra.extend_from_slice(&m.vert_properties[o + 3..o + m.num_prop]);
        }
        let tri_verts: Vec<[u32; 3]> = m
            .tri_verts
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();

        let mut mesh = Mesh {
            vert_pos,
            num_prop: m.num_prop,
            props_extra,
            ..Default::default()
        };
        mesh.create_halfedges(&tri_verts);
        mesh.calculate_bbox();
        mesh
    }

    /// Export the spine back to a `MeshGl`: re-interleave position + extra properties, and emit each
    /// triangle's three start vertices. The inverse of [`Mesh::from_mesh_gl`] for a well-formed mesh.
    pub fn to_mesh_gl(&self) -> MeshGl {
        let extra = self.num_prop - 3;
        let mut vert_properties = Vec::with_capacity(self.num_vert() * self.num_prop);
        for (v, p) in self.vert_pos.iter().enumerate() {
            vert_properties.push(p.x);
            vert_properties.push(p.y);
            vert_properties.push(p.z);
            if extra > 0 {
                vert_properties.extend_from_slice(&self.props_extra[v * extra..(v + 1) * extra]);
            }
        }
        let mut tri_verts = Vec::with_capacity(self.num_tri() * 3);
        for tri in 0..self.num_tri() {
            let t = TriId::from_usize(tri);
            for i in 0..3 {
                tri_verts.push(self.start(t.halfedge(i)).raw() as u32);
            }
        }
        MeshGl {
            num_prop: self.num_prop,
            vert_properties,
            tri_verts,
        }
    }

    // --- Perturbation inputs (R1) — the data the boolean's symbolic tie-break consumes. ---

    /// Visit every out-going half-edge of the vertex that `he` starts at, in fan order (Manifold's
    /// `Impl::ForVert`): `current = next(pair(current))` until it cycles back to `he`, which is the
    /// LAST half-edge visited. Requires a fully-paired (manifold) one-ring — an unpaired `NONE` pair
    /// walks off the mesh.
    pub fn for_vert(&self, he: HalfedgeId, mut func: impl FnMut(HalfedgeId)) {
        let mut current = he;
        loop {
            current = self.pair(current).next();
            func(current);
            if current == he {
                break;
            }
        }
    }

    /// Compute per-triangle face normals into [`Mesh::face_normal`] — the perturbation vectors. This is
    /// the face-normal loop of Manifold's `SetNormalsAndCoplanar` (the coplanar-ID flooding it also does
    /// is deferred to M.1.4). `normalize(cross(b − a, c − a))` where `(a,b,c)` are the triangle's verts;
    /// a degenerate (zero-area) triangle normalizes to NaN and snaps to `(0,0,1)` verbatim; a removed
    /// triangle (`start` NONE) keeps the `(0,0,0)` default.
    pub fn calculate_face_normals(&mut self) {
        let num_tri = self.num_tri();
        self.face_normal = vec![Vec3::ZERO; num_tri];
        for tri in 0..num_tri {
            let t = TriId::from_usize(tri);
            if self.start(t.halfedge(0)).is_none() {
                continue;
            }
            let v = self.pos(self.start(t.halfedge(0)));
            let n = (self.pos(self.end(t.halfedge(0))) - v).cross(self.pos(self.end(t.halfedge(1))) - v);
            let mut normal = n.normalize();
            if normal.x.is_nan() {
                normal = Vec3::new(0.0, 0.0, 1.0);
            }
            self.face_normal[tri] = normal;
        }
    }

    /// Compute per-vertex angle-weighted pseudo-normals into [`Mesh::vert_normal`] (Manifold's
    /// `CalculateVertNormals`). Each incident triangle contributes its face normal weighted by the
    /// interior ANGLE `phi` at the vertex, then the sum is `SafeNormalize`d. The angle is
    /// `acos(-dot(prevEdge, currEdge))` over the vertex's [`Mesh::for_vert`] ring, and — critically —
    /// it uses [`crate::mathf::acos`] (Manifold's own `math::acos`), NOT platform `f64::acos`. That's
    /// why this is bit-exact vs the C++ oracle WITHOUT the `libm` crate: the C++ kernel already uses a
    /// deterministic acos, and `mathf` is its transliteration. Requires [`Mesh::calculate_face_normals`]
    /// to have run. Degenerate incident edges are excluded; an unreferenced vertex gets `(0,0,0)`.
    pub fn calculate_vert_normals(&mut self) {
        let num_vert = self.num_vert();
        self.vert_normal = vec![Vec3::ZERO; num_vert];
        // The smallest half-edge id starting at each vertex — a deterministic ForVert seed (Manifold's
        // atomic vertHalfedgeMap min-reduction, serialized). `None` = not yet referenced.
        let mut vert_halfedge: Vec<Option<HalfedgeId>> = vec![None; num_vert];
        for e in self.halfedge_ids() {
            let v = self.start(e);
            if v.is_some() {
                let slot = &mut vert_halfedge[v.u()];
                if slot.is_none_or(|cur| e < cur) {
                    *slot = Some(e);
                }
            }
        }
        for (vert, &first) in vert_halfedge.iter().enumerate() {
            let Some(first_edge) = first else {
                continue; // not referenced ⇒ stays (0,0,0)
            };
            // Collect the one-ring first (keeps the borrow of `self` inside for_vert from tangling
            // with the per-edge reads below).
            let mut ring = Vec::new();
            self.for_vert(first_edge, |e| ring.push(e));
            let mut normal = Vec3::ZERO;
            for edge in ring {
                let tv0 = self.start(edge);
                let tv1 = self.end(edge);
                let tv2 = self.end(edge.next());
                let curr_edge = (self.pos(tv1) - self.pos(tv0)).normalize();
                let prev_edge = (self.pos(tv0) - self.pos(tv2)).normalize();
                // A degenerate incident triangle (zero-length edge ⇒ NaN) is excluded.
                if !curr_edge.x.is_finite() || !prev_edge.x.is_finite() {
                    continue;
                }
                let dot = -prev_edge.dot(curr_edge);
                let phi = if dot >= 1.0 {
                    0.0
                } else if dot <= -1.0 {
                    crate::mathf::PI
                } else {
                    crate::mathf::acos(dot)
                };
                normal += phi * self.face_normal[edge.tri().u()];
            }
            self.vert_normal[vert] = crate::boolean::predicates::safe_normalize(normal);
        }
    }

    /// Set `epsilon`/`tolerance` from the bounding box (Manifold `Impl::SetEpsilon`). `epsilon =
    /// MaxEpsilon(min_epsilon, bBox)`; `tolerance` only ever grows (`max` against its prior value), so a
    /// user-supplied tolerance is never shrunk below what the geometry demands. `use_single` folds in
    /// the `f32` epsilon for a single-precision kernel — ours is `f64`, so callers pass `false`.
    /// Requires [`Mesh::calculate_bbox`] to have run.
    pub fn set_epsilon(&mut self, min_epsilon: f64, use_single: bool) {
        self.epsilon = crate::boolean::predicates::max_epsilon(min_epsilon, self.b_box);
        let mut min_tol = self.epsilon;
        if use_single {
            min_tol = min_tol.max(f32::EPSILON as f64 * self.b_box.scale());
        }
        self.tolerance = self.tolerance.max(min_tol);
    }
}

/// The flat interchange buffer — Manifold's `MeshGL64` core (double precision, the format the kernel
/// works in). `num_prop >= 3`, first three properties are x,y,z; `tri_verts` is stride-3 CCW indices.
/// The optional merge/run/faceID/tangent channels are R3+ concerns and omitted here.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshGl {
    /// Properties per vertex, `>= 3` (first three are position).
    pub num_prop: usize,
    /// Interleaved vertex properties, stride `num_prop` (`vertProperties`).
    pub vert_properties: Vec<f64>,
    /// Triangle corner indices, stride 3, CCW from outside (`triVerts`).
    pub tri_verts: Vec<u32>,
}

impl MeshGl {
    /// Number of vertices.
    #[inline]
    pub fn num_vert(&self) -> usize {
        self.vert_properties.len() / self.num_prop
    }

    /// Number of triangles.
    #[inline]
    pub fn num_tri(&self) -> usize {
        self.tri_verts.len() / 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard axis-aligned unit cube `[0,1]³`: 8 verts, 12 outward-CCW triangles, volume 1.
    fn unit_cube() -> MeshGl {
        #[rustfmt::skip]
        let verts = vec![
            0.0, 0.0, 0.0, // 0
            1.0, 0.0, 0.0, // 1
            1.0, 1.0, 0.0, // 2
            0.0, 1.0, 0.0, // 3
            0.0, 0.0, 1.0, // 4
            1.0, 0.0, 1.0, // 5
            1.0, 1.0, 1.0, // 6
            0.0, 1.0, 1.0, // 7
        ];
        #[rustfmt::skip]
        let tris = vec![
            0, 2, 1,  0, 3, 2, // -Z
            4, 5, 6,  4, 6, 7, // +Z
            0, 1, 5,  0, 5, 4, // -Y
            2, 3, 7,  2, 7, 6, // +Y
            0, 4, 7,  0, 7, 3, // -X
            1, 2, 6,  1, 6, 5, // +X
        ];
        MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        }
    }

    #[test]
    fn cube_ingests_and_is_manifold() {
        let mesh = Mesh::from_mesh_gl(&unit_cube());
        assert_eq!(mesh.num_vert(), 8);
        assert_eq!(mesh.num_tri(), 12);
        assert_eq!(mesh.halfedge.len(), 36);
        assert_eq!(mesh.num_edge(), 18); // Euler: V−E+F = 8−18+12 = 2 ✓
        assert!(mesh.is_manifold());
        // every half-edge is paired on a closed manifold
        assert!(mesh.halfedge.iter().all(|h| h.paired_halfedge.is_some()));
    }

    #[test]
    fn cube_volume_and_area_exact() {
        let mesh = Mesh::from_mesh_gl(&unit_cube());
        // outward winding ⇒ +1 exactly (the sign check: inward winding would give -1).
        assert_eq!(mesh.volume(), 1.0);
        // 6 faces × 1.0 = 6.0.
        assert_eq!(mesh.surface_area(), 6.0);
    }

    #[test]
    fn cube_volume_scales_and_translates() {
        // A 2× cube offset far from origin: analytic volume 8, area 24, independent of position
        // (divergence theorem). Area stays EXACT — it uses only coordinate differences, where the
        // offset cancels. Volume does NOT: `dot(cross(e1,e2), v0)` multiplies the large v0 (~300) and
        // cancels, so the result is ~8 minus a few ULP (7.999999999999972 here). That FP value is the
        // ALGORITHM's, which Manifold's C++ shares bit-for-bit — this is precisely why the K.0 gate
        // (M.0.6) compares against the C++ engine, not against the analytic 8.
        let base = unit_cube();
        let verts: Vec<f64> = base
            .vert_properties
            .iter()
            .enumerate()
            .map(|(i, &c)| c * 2.0 + [100.0, 200.0, 300.0][i % 3])
            .collect();
        let mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: base.tri_verts,
        });
        let v = mesh.volume();
        assert!((v - 8.0).abs() < 1e-9, "volume {v} !~ 8");
        assert_eq!(mesh.surface_area(), 24.0); // exact: differences only
    }

    #[test]
    fn mesh_gl_round_trips() {
        let cube = unit_cube();
        let mesh = Mesh::from_mesh_gl(&cube);
        let out = mesh.to_mesh_gl();
        assert_eq!(out, cube); // exact identity for a well-formed position-only mesh
    }

    #[test]
    fn round_trips_with_extra_properties() {
        // num_prop = 7 (xyz + RGBA): the extra props must survive the round-trip verbatim.
        let mut vp = Vec::new();
        for v in 0..8 {
            let base = [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
            ][v];
            vp.extend_from_slice(&base);
            vp.extend_from_slice(&[0.1 * v as f64, 0.2, 0.3, 1.0]); // RGBA
        }
        let cube = unit_cube();
        let m = MeshGl {
            num_prop: 7,
            vert_properties: vp,
            tri_verts: cube.tri_verts,
        };
        let mesh = Mesh::from_mesh_gl(&m);
        assert_eq!(mesh.num_prop, 7);
        assert_eq!(mesh.props_extra.len(), 8 * 4);
        assert_eq!(mesh.volume(), 1.0); // positions still the unit cube
        assert_eq!(mesh.to_mesh_gl(), m);
    }

    #[test]
    fn open_mesh_is_not_manifold() {
        // A single triangle has 3 boundary edges, all unpaired.
        let m = MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            tri_verts: vec![0, 1, 2],
        };
        let mesh = Mesh::from_mesh_gl(&m);
        assert!(!mesh.is_manifold());
    }

    #[test]
    fn non_manifold_edge_is_rejected() {
        // Three triangles sharing edge 0–1 (a "fin"): edge {0,1} has 3 incident half-edges, so the
        // clean pairing leaves them unpaired → not manifold.
        let m = MeshGl {
            num_prop: 3,
            vert_properties: vec![
                0.0, 0.0, 0.0, // 0
                1.0, 0.0, 0.0, // 1
                0.0, 1.0, 0.0, // 2
                0.0, 0.0, 1.0, // 3
                0.0, -1.0, 0.0, // 4
            ],
            tri_verts: vec![0, 1, 2, 0, 1, 3, 0, 1, 4],
        };
        let mesh = Mesh::from_mesh_gl(&m);
        assert!(!mesh.is_manifold());
    }

    #[test]
    fn empty_mesh_is_manifold_with_zero_volume() {
        let mesh = Mesh::default();
        assert!(mesh.is_empty());
        assert!(mesh.is_manifold());
        assert_eq!(mesh.volume(), 0.0);
        assert_eq!(mesh.surface_area(), 0.0);
    }

    #[test]
    fn bbox_from_cube() {
        let mesh = Mesh::from_mesh_gl(&unit_cube());
        assert_eq!(mesh.b_box.min, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(mesh.b_box.max, Vec3::new(1.0, 1.0, 1.0));
        assert_eq!(mesh.b_box.size(), Vec3::new(1.0, 1.0, 1.0));
    }

    #[test]
    fn accessors_and_meshgl_counts() {
        let mesh = Mesh::from_mesh_gl(&unit_cube());
        // prop() accessor: position-only ⇒ prop_vert == start_vert.
        assert_eq!(mesh.prop(HalfedgeId::new(0)), mesh.start(HalfedgeId::new(0)));
        assert_eq!(mesh.prop(HalfedgeId::new(17)), mesh.start(HalfedgeId::new(17)));
        // MeshGl count helpers.
        let gl = mesh.to_mesh_gl();
        assert_eq!(gl.num_vert(), 8);
        assert_eq!(gl.num_tri(), 12);
    }

    #[test]
    fn is_manifold_hand_built_edge_cases() {
        let he = |s: i32, p: i32| Halfedge {
            start_vert: VertId::new(s),
            paired_halfedge: HalfedgeId::new(p),
            prop_vert: VertId::new(s),
        };
        // Half-edge count not a multiple of 3 → not manifold.
        let two = Mesh {
            halfedge: vec![he(0, 1), he(1, 0)],
            ..Default::default()
        };
        assert!(!two.is_manifold());

        // A fully-removed triple (all NONE) is vacuously manifold (the removed-half-edge branch).
        let removed = Mesh {
            halfedge: vec![he(-1, -1), he(-1, -1), he(-1, -1)],
            ..Default::default()
        };
        assert!(removed.is_manifold());

        // A live half-edge whose next-in-triangle is removed → not manifold (returns before it would
        // dereference the dangling pair index, so no panic).
        let dangling = Mesh {
            halfedge: vec![he(0, 5), he(-1, -1), he(2, 4)],
            ..Default::default()
        };
        assert!(!dangling.is_manifold());
    }

    #[test]
    fn calculate_bbox_skips_nan_verts() {
        let mut m = Mesh::from_mesh_gl(&unit_cube());
        m.vert_pos.push(Vec3::new(f64::NAN, 50.0, 50.0)); // NaN x → skipped (Manifold's isnan(a.x))
        m.calculate_bbox();
        // the NaN vert is ignored; the bbox stays the unit cube's.
        assert_eq!(m.b_box.min, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(m.b_box.max, Vec3::new(1.0, 1.0, 1.0));
    }

    #[test]
    fn same_direction_duplicate_edge_is_unpaired() {
        // Two triangles share the DIRECTED edge 0→1 (not a reverse pair), so the len-2 group fails the
        // reverse check and both stay unpaired → not manifold.
        let m = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            tri_verts: vec![0, 1, 2, 0, 1, 3],
        });
        assert!(!m.is_manifold());
        // the shared 0→1 half-edges never linked
        assert!(
            m.halfedge
                .iter()
                .filter(|h| h.start_vert == VertId::new(0) && h.paired_halfedge.is_none())
                .count()
                >= 1
        );
    }

    // --- Perturbation inputs (R1 / M.1.0). ---

    #[test]
    fn unit_epsilon_and_tolerance() {
        let mut mesh = Mesh::from_mesh_gl(&unit_cube());
        // Fresh mesh: the -1 "unset" sentinel.
        assert_eq!(mesh.epsilon, -1.0);
        assert_eq!(mesh.tolerance, -1.0);
        // Scale of [0,1]³ is 1 ⇒ epsilon = kPrecision·1 = 1e-12; tolerance grows to match (was -1).
        mesh.set_epsilon(-1.0, false);
        assert_eq!(mesh.epsilon, crate::boolean::predicates::K_PRECISION);
        assert_eq!(mesh.tolerance, crate::boolean::predicates::K_PRECISION);
        // A previously-set larger tolerance is NOT shrunk by a later set_epsilon.
        mesh.tolerance = 0.5;
        mesh.set_epsilon(-1.0, false);
        assert_eq!(mesh.tolerance, 0.5);
    }

    #[test]
    fn cube_face_normals_are_axis_aligned() {
        let mut mesh = Mesh::from_mesh_gl(&unit_cube());
        mesh.calculate_face_normals();
        assert_eq!(mesh.face_normal.len(), 12);
        // Every face of an axis-aligned cube has a unit ±axis normal, and the pair of tris on each
        // face agree. The fixture's face order is -Z,-Z,+Z,+Z,-Y,-Y,+Y,+Y,-X,-X,+X,+X.
        let expect = [
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
        ];
        for (tri, n) in mesh.face_normal.iter().enumerate() {
            assert_eq!(*n, expect[tri / 2], "tri {tri}");
        }
    }

    #[test]
    fn degenerate_face_normal_snaps_to_z() {
        // A single zero-area (collinear) triangle: cross = 0, normalize = NaN ⇒ snaps to (0,0,1).
        let mut mesh = Mesh {
            vert_pos: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
            ],
            ..Default::default()
        };
        mesh.create_halfedges(&[[0, 1, 2]]);
        mesh.calculate_face_normals();
        assert_eq!(mesh.face_normal, vec![Vec3::new(0.0, 0.0, 1.0)]);
    }

    #[test]
    fn for_vert_orbits_the_one_ring() {
        // A closed octahedron gives every vertex a clean 4-edge fan. Walk vertex 0's ring and confirm
        // ForVert visits exactly the out-going half-edges (all starting at 0), each once, including the
        // seed half-edge last.
        let mut mesh = Mesh {
            vert_pos: vec![
                Vec3::new(0.0, 0.0, 1.0),  // 0 top
                Vec3::new(1.0, 0.0, 0.0),  // 1
                Vec3::new(0.0, 1.0, 0.0),  // 2
                Vec3::new(-1.0, 0.0, 0.0), // 3
                Vec3::new(0.0, -1.0, 0.0), // 4
                Vec3::new(0.0, 0.0, -1.0), // 5 bottom
            ],
            ..Default::default()
        };
        #[rustfmt::skip]
        let tris = [
            [0u32, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 1], // top fan
            [5, 2, 1], [5, 3, 2], [5, 4, 3], [5, 1, 4],     // bottom fan
        ];
        mesh.create_halfedges(&tris);
        assert!(mesh.is_manifold());
        // Seed = the first half-edge starting at vertex 0.
        let seed = mesh
            .halfedge_ids()
            .find(|&e| mesh.start(e) == VertId::new(0))
            .unwrap();
        let mut visited = Vec::new();
        mesh.for_vert(seed, |e| visited.push(e));
        // Vertex 0 touches 4 top triangles ⇒ 4 out-going half-edges.
        assert_eq!(visited.len(), 4);
        assert!(visited.iter().all(|&e| mesh.start(e) == VertId::new(0)));
        assert_eq!(*visited.last().unwrap(), seed); // seed is visited last
        // No repeats.
        let mut uniq = visited.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 4);
    }
}
