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

use std::sync::atomic::{AtomicI32, Ordering};

use crate::boolean::vocab::TriRef;
use crate::linalg::{Box3, Mat3x4, Vec3};
use crate::mesh_ids::{HalfedgeId, TriId, VertId};
use crate::status::Error;

/// The global mesh-instance ID counter (Manifold's `Impl::meshIDCounter_`, starting at 1). Each freshly
/// constructed original reserves a unique ID via [`reserve_ids`]; the boolean offsets Q's IDs above P's
/// by the counter's current value. Only ID EQUALITY matters (for `TriRef::same_face`), so the absolute
/// values — and any cross-test interleaving of this shared counter — never affect the geometry.
static MESH_ID_COUNTER: AtomicI32 = AtomicI32::new(1);

/// Reserve `n` consecutive mesh-instance IDs, returning the first (Manifold's `ReserveIDs`).
pub fn reserve_ids(n: i32) -> i32 {
    MESH_ID_COUNTER.fetch_add(n, Ordering::Relaxed)
}

/// The current mesh-instance ID counter value — the boolean's `offsetQ` (Manifold reads `meshIDCounter_`
/// directly), which shifts Q's IDs above every ID reserved so far so P/Q never collide.
pub fn mesh_id_counter() -> i32 {
    MESH_ID_COUNTER.load(Ordering::Relaxed)
}

/// A single half-edge. `end` is DERIVED (see the module doc), so only these three fields are stored;
/// [`VertId::NONE`]/[`HalfedgeId::NONE`] (`-1`) is the removed/unpaired sentinel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Halfedge {
    /// The vertex this half-edge starts at, or [`VertId::NONE`] if removed.
    pub start_vert: VertId,
    /// The opposite half-edge, or [`HalfedgeId::NONE`] if unpaired.
    pub paired_halfedge: HalfedgeId,
    /// The property-vertex — an index into [`Mesh::properties`], DECOUPLED from `start_vert` (Manifold
    /// `Halfedges::propVert_`). Equals `start_vert` in the position-only / freshly-ingested 1:1 case, but
    /// `CreateProperties` and `CollapseEdge` split it off at property seams: two corners at the same
    /// geometric vertex can carry different property rows (a UV/color seam), so `prop_vert` roams its own
    /// index space (`0..num_prop_vert`), not `vert_pos`'s.
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
    /// Count of EXTRA (non-position) properties per prop-vertex — Manifold `Impl::numProp_`, position
    /// EXCLUDED (it lives in `vert_pos`). `num_prop == 0` means position-only. NOTE the convention split:
    /// this is the `Impl` count, whereas [`MeshGl::num_prop`] is the interchange count (`num_prop + 3`,
    /// position included). Keeping our field faithful to `numProp_` lets the property ports read verbatim.
    pub num_prop: usize,
    /// The EXTRA properties, flat, stride `num_prop`, indexed by PROP-VERTEX (Manifold `properties_`) —
    /// NOT by geometric vertex. Its row count is [`Mesh::num_prop_vert`] (`len / num_prop`), which decouples
    /// from `vert_pos.len()` once a boolean splits prop-verts at a seam. Empty when `num_prop == 0`.
    pub properties: Vec<f64>,
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
    /// This mesh's ORIGINAL mesh-instance ID (Manifold `meshRelation_.originalID`); `-1` until
    /// [`Mesh::initialize_original`] runs. Not the per-triangle id — see [`Mesh::tri_ref`].
    pub mesh_id: i32,
    /// Per-triangle provenance (Manifold `meshRelation_.triRef`) — which input mesh/coplanar-face each
    /// output triangle came from. Empty until [`Mesh::initialize_original`]; the coplanar-group IDs are
    /// filled by [`Mesh::set_normals_and_coplanar`], and the boolean threads it through so
    /// [`crate::boolean::edge_op`]'s `CollapseColinearEdges` knows which edges are safe to collapse.
    pub tri_ref: Vec<TriRef>,
}

impl Default for Mesh {
    fn default() -> Self {
        // `epsilon`/`tolerance` default to Manifold's `-1` "unset" sentinel, NOT `0.0` — a real
        // computed epsilon is always `>= 0`, so `-1` is an unambiguous "SetEpsilon hasn't run".
        Self {
            vert_pos: Vec::new(),
            halfedge: Vec::new(),
            num_prop: 0,
            properties: Vec::new(),
            b_box: Box3::default(),
            face_normal: Vec::new(),
            vert_normal: Vec::new(),
            epsilon: -1.0,
            tolerance: -1.0,
            mesh_id: -1,
            tri_ref: Vec::new(),
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

    /// Number of PROP-vertices — rows in [`Mesh::properties`] (Manifold `Impl::NumPropVert`). When
    /// position-only (`num_prop == 0`) every geometric vertex is its own trivial prop-vert, so this is
    /// `num_vert`; otherwise it's `properties.len() / num_prop`, which grows independently of `num_vert`
    /// as booleans/collapses split prop-verts at seams.
    #[inline]
    pub fn num_prop_vert(&self) -> usize {
        // C++ `NumProp() == 0 ? NumVert() : properties_.size() / NumProp()` — the division is guarded
        // against a zero stride, falling back to the geometric vert count (every vert its own prop-vert).
        self.properties
            .len()
            .checked_div(self.num_prop)
            .unwrap_or_else(|| self.num_vert())
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
            // In the 1:1 case (`prop_vert == start_vert`, the geometric remap fits) this repoints props
            // too. The DECOUPLED case — prop-verts in their own index space, `properties` compacted
            // independently (C++ `RemoveUnreferencedVerts` + `CompactProps`) — is M.3.4b.5; guarded by
            // `< n` so a decoupled high prop-vert is left untouched rather than mis-remapped here.
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
        let has_refs = !self.tri_ref.is_empty();
        let mut new_he = Vec::with_capacity(next as usize);
        let mut new_fn = Vec::with_capacity(next as usize / 3);
        let mut new_ref = Vec::with_capacity(next as usize / 3);
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
            if has_refs {
                new_ref.push(self.tri_ref[tri]);
            }
        }
        self.halfedge = new_he;
        if has_normals {
            self.face_normal = new_fn;
        }
        if has_refs {
            self.tri_ref = new_ref;
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

    /// The bounding box (Manifold's `Manifold::BoundingBox`). Kept current by [`Mesh::calculate_bbox`];
    /// on an empty mesh it's the inverted/empty box (`min = +∞`, `max = −∞`).
    #[inline]
    pub fn bounding_box(&self) -> Box3 {
        self.b_box
    }

    /// Every vertex position is finite — no NaN/inf (Manifold's `Impl::IsFinite`, a `transform_reduce`
    /// over `vertPos_`). This is the input-validity query (distinct from [`Mat3x4::is_finite`], which
    /// checks a transform MATRIX); an empty mesh is vacuously finite.
    pub fn is_finite(&self) -> bool {
        self.vert_pos.iter().all(|p| p.is_finite())
    }

    /// The axis-aligned box primitive (`Manifold::Cube`, M.3.5): the canonical unit `Shape::Cube`
    /// (8 verts, 12 tris — verbatim vertex + triangle ORDER from `impl.cpp`) scaled by `size` and, if
    /// `center`, shifted so its centroid is the origin. Runs the full primitive constructor sequence
    /// (`CreateHalfedges` → `InitializeOriginal` → `CalculateBBox` → `SetEpsilon` → `SortGeometry` →
    /// `SetNormalsAndCoplanar`), so the result is boolean-ready and Morton-canonical like every C++
    /// primitive. A negative or zero-length `size` is `Err(InvalidConstruction)` (C++ returns
    /// `Invalid()`); used internally by [`Mesh::split_by_plane`]/[`Mesh::trim_by_plane`] to build the
    /// cutting half-space.
    pub fn cube(size: Vec3, center: bool) -> Result<Mesh, Error> {
        if size.x < 0.0 || size.y < 0.0 || size.z < 0.0 || size.length() == 0.0 {
            return Err(Error::InvalidConstruction);
        }
        // Canonical `Shape::Cube` — corners in 000,001,010,011,100,101,110,111 order.
        #[rustfmt::skip]
        let base = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 1.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 1.0),
        ];
        #[rustfmt::skip]
        let tris: [[u32; 3]; 12] = [
            [1, 0, 4], [2, 4, 0], [1, 3, 0], [3, 1, 5],
            [3, 2, 0], [3, 7, 2], [5, 4, 6], [5, 1, 4],
            [6, 4, 2], [7, 6, 2], [7, 3, 5], [7, 5, 6],
        ];
        // m = scale(size) with translation (center ? -size/2 : 0); `v = m * vec4(v, 1)`.
        let m = Mat3x4 {
            x: Vec3::new(size.x, 0.0, 0.0),
            y: Vec3::new(0.0, size.y, 0.0),
            z: Vec3::new(0.0, 0.0, size.z),
            w: if center { size * -0.5 } else { Vec3::ZERO },
        };
        let mut mesh = Mesh {
            vert_pos: base.iter().map(|&v| m.transform_point(v)).collect(),
            num_prop: 0,
            ..Default::default()
        };
        mesh.create_halfedges(&tris);
        mesh.initialize_original();
        mesh.calculate_bbox();
        mesh.set_epsilon(-1.0, false);
        mesh.sort_geometry();
        mesh.set_normals_and_coplanar();
        Ok(mesh)
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

        // MeshGl carries position IN `num_prop` (>= 3); our `Mesh::num_prop` is the C++ `numProp_` count
        // with position EXCLUDED. 1:1 ingest for now — the faithful position-dedup that splits prop-verts
        // from geometric verts (mirroring C++'s merge-vector/coincident-vert quotient) is M.3.4b.2.
        let n_vert = m.vert_properties.len() / m.num_prop;
        let num_prop = m.num_prop - 3;
        let mut vert_pos = Vec::with_capacity(n_vert);
        let mut properties = Vec::with_capacity(n_vert * num_prop);
        for v in 0..n_vert {
            let o = v * m.num_prop;
            vert_pos.push(Vec3::new(
                m.vert_properties[o],
                m.vert_properties[o + 1],
                m.vert_properties[o + 2],
            ));
            properties.extend_from_slice(&m.vert_properties[o + 3..o + m.num_prop]);
        }
        let tri_verts: Vec<[u32; 3]> = m
            .tri_verts
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();

        let mut mesh = Mesh {
            vert_pos,
            num_prop,
            properties,
            ..Default::default()
        };
        mesh.create_halfedges(&tri_verts);
        mesh.calculate_bbox();
        mesh
    }

    /// Export the spine back to a `MeshGl`: re-interleave position + extra properties, and emit each
    /// triangle's three start vertices. The inverse of [`Mesh::from_mesh_gl`] for a well-formed mesh.
    pub fn to_mesh_gl(&self) -> MeshGl {
        // 1:1 emit (one interchange row per geometric vert). Correct for position-only + the freshly-
        // ingested/`set_properties` case where `prop_vert == start_vert`. Emitting one row per PROP-vert
        // (so a seam-split property survives the round-trip) is M.3.4b.3; the merge-vectors that let a
        // chained op re-ingest those split prop-verts are M.3.4b.7.
        let num_prop = self.num_prop;
        let mesh_gl_prop = num_prop + 3;
        let mut vert_properties = Vec::with_capacity(self.num_vert() * mesh_gl_prop);
        for (v, p) in self.vert_pos.iter().enumerate() {
            vert_properties.push(p.x);
            vert_properties.push(p.y);
            vert_properties.push(p.z);
            if num_prop > 0 {
                vert_properties.extend_from_slice(&self.properties[v * num_prop..(v + 1) * num_prop]);
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
            num_prop: mesh_gl_prop,
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

    /// The normal of triangle `t`, computed on the fly — `normalize(cross(v1 − v0, v2 − v0))` with a
    /// degenerate (zero-area) triangle snapping to `(0,0,1)`. Identical formula to
    /// [`Mesh::calculate_face_normals`] (so it equals the cached `face_normal` bit-for-bit), but
    /// self-contained: callers that only need a few normals — [`Mesh::is_convex`], Minkowski's
    /// coplanar test — don't have to run the full face-normal pass first.
    pub(crate) fn tri_normal(&self, t: TriId) -> Vec3 {
        let v = self.pos(self.start(t.halfedge(0)));
        let n = (self.pos(self.end(t.halfedge(0))) - v).cross(self.pos(self.end(t.halfedge(1))) - v);
        let normal = n.normalize();
        if normal.x.is_nan() {
            Vec3::new(0.0, 0.0, 1.0)
        } else {
            normal
        }
    }

    /// Is this a convex solid? Manifold's `Impl::IsConvex`: genus 0 (Euler characteristic 2) AND every
    /// dihedral edge turns the same way (`dot(edgeVec, cross(n0, n1)) > 0`, coplanar faces exempt). The
    /// Minkowski dispatch keys on this to pick the fast convex×convex tier. Self-contained — recomputes
    /// face normals via [`Mesh::tri_normal`], so it needs no prior normal pass. Each undirected edge is
    /// visited once (via the lower-indexed half-edge, which is orientation-symmetric for this test).
    pub fn is_convex(&self) -> bool {
        let chi = self.num_vert() as i64 - self.num_edge() as i64 + self.num_tri() as i64;
        if 1 - chi / 2 != 0 {
            return false;
        }
        for idx in 0..self.halfedge.len() {
            let he = HalfedgeId::from_usize(idx);
            let pair = self.pair(he);
            // Boundary edge (non-manifold) — skip; and process each undirected edge just once.
            if pair.is_none() || idx >= pair.u() {
                continue;
            }
            let normal0 = self.tri_normal(he.tri());
            let normal1 = self.tri_normal(pair.tri());
            if normal0 == normal1 {
                continue;
            }
            let edge_vec = self.pos(self.end(he)) - self.pos(self.start(he));
            if edge_vec.dot(normal0.cross(normal1)) <= 0.0 {
                return false;
            }
        }
        true
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

    /// Canonicalize the within-triangle half-edge order (Manifold's `ReorderHalfedges`), run AFTER
    /// `Face2Tri` and BEFORE `SimplifyTopology`. C++ adds a face's half-edges in nondeterministic
    /// (parallel) order and reorders here for determinism; ours is deterministic but a DIFFERENT order,
    /// so we apply the same canonicalization — otherwise the collapse cascade in `SimplifyTopology`
    /// visits edges in a different sequence and gets stuck at a worse (higher-genus) fixed point than C++.
    ///
    /// Step 1 rotates each triangle's three half-edges so the one with the smallest `start` vertex is
    /// first (cyclic order preserved — `end` stays derived). Step 2 repairs each pair pointer by finding,
    /// in the opposite face, the half-edge whose `end` equals this one's `start`. Per-triangle data
    /// (`face_normal`/`tri_ref`) is untouched — the triangle INDEX doesn't move, only its internal order.
    pub fn reorder_halfedges(&mut self) {
        let num_tri = self.num_tri();
        // Step 1: rotate within each face so the smallest start vertex leads.
        for tri in 0..num_tri {
            let base = 3 * tri;
            let face = [self.halfedge[base], self.halfedge[base + 1], self.halfedge[base + 2]];
            if face[0].start_vert.is_none() {
                continue;
            }
            let mut index = 0;
            for i in [1usize, 2] {
                if face[i].start_vert < face[index].start_vert {
                    index = i;
                }
            }
            for i in 0..3 {
                self.halfedge[base + i] = face[(index + i) % 3];
            }
        }
        // Step 2: repair pair pointers (the pair's within-face position changed under step 1).
        for tri in 0..num_tri {
            for i in 0..3 {
                let curr = HalfedgeId::from_usize(3 * tri + i);
                let start_vert = self.start(curr);
                if start_vert.is_none() {
                    continue;
                }
                let opposite_face = self.pair(curr).tri();
                let mut index = -1i32;
                for j in 0..3 {
                    if start_vert == self.end(opposite_face.halfedge(j)) {
                        index = j as i32;
                    }
                }
                self.set_pair(curr, opposite_face.halfedge(index as usize));
            }
        }
    }

    /// Stamp this mesh as a fresh ORIGINAL (Manifold's `InitializeOriginal`): reserve a unique
    /// mesh-instance ID and give every triangle a [`TriRef`] `{mesh_id, original_id: mesh_id, face_id:
    /// -1, coplanar_id: -1}`. The `coplanar_id` is a placeholder here (C++ reads uninitialized memory,
    /// then overwrites it) — [`Mesh::set_normals_and_coplanar`] fills it. Call once per raw-geometry
    /// input; the boolean PROPAGATES `tri_ref` to its output, so an intermediate result is never
    /// re-initialized (that would erase the per-triangle provenance the fold relies on).
    pub fn initialize_original(&mut self) {
        let mesh_id = reserve_ids(1);
        self.mesh_id = mesh_id;
        self.tri_ref = vec![
            TriRef {
                mesh_id,
                original_id: mesh_id,
                face_id: -1,
                coplanar_id: -1,
            };
            self.num_tri()
        ];
    }

    /// Compute face normals AND flood the coplanar-group IDs (Manifold's `SetNormalsAndCoplanar`), then
    /// the vertex normals. Requires [`Mesh::initialize_original`] (for `tri_ref`) and
    /// [`Mesh::set_epsilon`] (for `tolerance`, the coplanarity threshold) to have run.
    ///
    /// The flood processes triangles LARGEST-area first (a `stable_sort` on squared area, descending), so
    /// the biggest triangle of each flat region seeds it. A seed claims `coplanar_id = its own index`,
    /// then a stack-based traversal spreads that ID to every adjacent triangle whose far vertex lies
    /// within `tolerance` of the seed plane — SNAPPING each coplanar triangle's normal to the seed's
    /// exact normal (so a whole flat face shares one normal, byte-for-byte). This global (non-local)
    /// definition of "coplanar" is what keeps `CollapseColinearEdges` from stacking errors as verts move.
    pub fn set_normals_and_coplanar(&mut self) {
        let num_tri = self.num_tri();
        self.face_normal = vec![Vec3::ZERO; num_tri];

        // (area², tri) per triangle — the normal is computed here too; a removed/degenerate tri gets 0.
        let mut tri_priority: Vec<(f64, usize)> = Vec::with_capacity(num_tri);
        for tri in 0..num_tri {
            self.tri_ref[tri].coplanar_id = -1;
            let t = TriId::from_usize(tri);
            if self.start(t.halfedge(0)).is_none() {
                tri_priority.push((0.0, tri));
                continue;
            }
            let v = self.pos(self.start(t.halfedge(0)));
            let n = (self.pos(self.end(t.halfedge(0))) - v).cross(self.pos(self.end(t.halfedge(1))) - v);
            let mut normal = n.normalize();
            if normal.x.is_nan() {
                normal = Vec3::new(0.0, 0.0, 1.0);
            }
            self.face_normal[tri] = normal;
            tri_priority.push((n.length2(), tri));
        }

        // Largest area first (stable, descending) — the seed of each coplanar region is its biggest tri.
        tri_priority.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));

        let mut interior: Vec<HalfedgeId> = Vec::new();
        for &(_, seed_tri) in &tri_priority {
            if self.tri_ref[seed_tri].coplanar_id >= 0 {
                continue;
            }
            self.tri_ref[seed_tri].coplanar_id = seed_tri as i32;
            let t = TriId::from_usize(seed_tri);
            if self.start(t.halfedge(0)).is_none() {
                continue;
            }
            let base = self.pos(self.start(t.halfedge(0)));
            let normal = self.face_normal[seed_tri];
            interior.clear();
            interior.push(t.halfedge(0));
            interior.push(t.halfedge(1));
            interior.push(t.halfedge(2));
            while let Some(&back) = interior.last() {
                let h = self.pair(back).next();
                interior.pop();
                if self.tri_ref[h.tri().u()].coplanar_id >= 0 {
                    continue;
                }
                let v = self.pos(self.end(h));
                if (v - base).dot(normal).abs() < self.tolerance {
                    let tri = h.tri().u();
                    self.tri_ref[tri].coplanar_id = seed_tri as i32;
                    self.face_normal[tri] = normal;
                    // Stack bookkeeping (verbatim): don't double-push the edge we just arrived across.
                    if interior.is_empty() || h != self.pair(*interior.last().unwrap()) {
                        interior.push(h);
                    } else {
                        interior.pop();
                    }
                    interior.push(h.next());
                }
            }
        }

        self.calculate_vert_normals();
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

    /// Apply an affine transform (`Manifold::Impl::Transform`, M.3.1/M.3.2). Positions map through `m`;
    /// face + vertex normals through the inverse-transpose ([`Mat3::normal_transform`]); a MIRROR (negative
    /// linear determinant) flips every triangle's winding ([`Mesh::flip_tris`]) so the surface stays
    /// outward-oriented. The bbox is recomputed and epsilon scaled by the spectral norm. Topology
    /// (halfedge order) is preserved — no re-sort. Identity is a cheap clone.
    ///
    /// A non-finite `m` (NaN/inf entry) is `Err(Error::NonFiniteVertex)` — the eager translation of
    /// Manifold's `MakeEmpty(NonFiniteVertex)`; the C++ status-propagation branch that precedes it
    /// (`if status_ != NoError`) is gone because eager `?` at the call site replaces it (see
    /// [`crate::status`]).
    pub fn transform(&self, m: Mat3x4) -> Result<Mesh, Error> {
        if m == Mat3x4::IDENTITY {
            return Ok(self.clone());
        }
        if !m.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        let linear = m.linear();
        let normal_t = linear.normal_transform();
        // `TransformNormals`: normalize, and a degenerate (NaN) normal collapses to zero.
        let sn = |v: Vec3| {
            let u = normal_t.mul_vec(v).normalize();
            if u.x.is_nan() { Vec3::ZERO } else { u }
        };

        let mut result = self.clone();
        for p in &mut result.vert_pos {
            *p = m.transform_point(*p);
        }
        for n in &mut result.face_normal {
            *n = sn(*n);
        }
        for n in &mut result.vert_normal {
            *n = sn(*n);
        }
        if linear.determinant() < 0.0 {
            result.flip_tris();
        }
        result.calculate_bbox();
        // Scale epsilon by the 3×3 spectral norm, then re-floor against the new bbox (`SetEpsilon`).
        result.epsilon *= linear.spectral_norm();
        result.set_epsilon(result.epsilon, false);
        Ok(result)
    }

    /// Reverse the winding of every triangle in place (`mesh_fixes.h` `FlipTris`) — used by a mirror
    /// transform to keep normals pointing outward. Each triangle's three half-edges are rewritten in
    /// reversed order (each new start is the old edge's END), with every pair pointer remapped through
    /// [`flip_halfedge`] to follow its now-flipped neighbour.
    fn flip_tris(&mut self) {
        let num_tri = self.num_tri();
        let old = self.halfedge.clone();
        for t in 0..num_tri {
            let base = 3 * t;
            // New slot i is sourced from old slot [2,1,0][i] with start/end swapped.
            let src = [base + 2, base + 1, base];
            for (i, &s) in src.iter().enumerate() {
                // End of half-edge `s` = start of the next half-edge in its (same) triangle.
                let end_vert = old[base + (s - base + 1) % 3].start_vert;
                self.halfedge[base + i] = Halfedge {
                    start_vert: end_vert,
                    paired_halfedge: flip_halfedge(old[s].paired_halfedge),
                    prop_vert: end_vert,
                };
            }
        }
    }

    /// Recompute every vertex's EXTRA properties from a callback (`Manifold::SetProperties`, M.3.4a) —
    /// the mechanism OpenSCAD's `color()` uses to stamp RGBA onto a mesh.
    ///
    /// `num_prop` is the EXTRA-property count, EXCLUDING the 3 position channels (matching the C++
    /// `SetProperties(numProp, …)` parameter, and the existing fab-scad `with_color`'s `set_properties(4,
    /// …)` for RGBA). It becomes `Mesh::num_prop` directly (our field IS the C++ `numProp_`); `num_prop ==
    /// 0` strips properties back to position-only. Do NOT pass the interchange total (position-inclusive).
    ///
    /// `prop_fn(new, position, old)` fills `new` (this vertex's `num_prop` fresh extra values) given its
    /// read-only `position` and its `old` extra values (`self.num_prop` of them; empty if the source was
    /// position-only). Position is never touched — it lives in `vert_pos`, exactly as the C++ keeps it out
    /// of `properties_`. Applied once per triangle CORNER (verbatim), but each vertex's row is idempotent,
    /// so the redundant writes are harmless. Runs serially — deterministic by construction.
    ///
    /// NOTE: a leaf mesh colored here keeps its color only until a boolean, which still forces
    /// position-only output — the `CreateProperties` interpolation that carries properties across a
    /// boolean seam is M.3.4b.
    pub fn set_properties(&self, num_prop: usize, prop_fn: impl Fn(&mut [f64], Vec3, &[f64])) -> Mesh {
        let mut result = self.clone();
        if num_prop == 0 {
            result.properties.clear();
            result.num_prop = 0;
            return result;
        }
        let old_extra = self.num_prop;
        let num_vert = self.num_vert();
        let mut new_props = vec![0.0; num_prop * num_vert];
        for tri in 0..self.num_tri() {
            let t = TriId::from_usize(tri);
            for i in 0..3 {
                let edge = t.halfedge(i);
                let vert = self.start(edge); // position source (vertPos_[vert])
                let prop_vert = self.prop(edge).u(); // property-row index
                let old_row = &self.properties[old_extra * prop_vert..old_extra * (prop_vert + 1)];
                let new_row = &mut new_props[num_prop * prop_vert..num_prop * (prop_vert + 1)];
                prop_fn(new_row, self.pos(vert), old_row);
            }
        }
        result.properties = new_props;
        result.num_prop = num_prop;
        result
    }

    /// Split into connected components (`Manifold::Decompose`, M.3.3): union-find over the forward
    /// half-edges labels each vertex, then every component is extracted as its own `Mesh` (its vertex
    /// subset + the triangles it owns, re-paired via [`Mesh::create_halfedges`] and canonicalized by
    /// [`Mesh::sort_geometry`], inheriting the source epsilon/tolerance). A single component returns a
    /// clone. The returned order is component-label order (a set, for the differential — not C++'s exact
    /// order). This is the inverse of `Compose`; enclosed cavities stay their own component (the W.4
    /// contract).
    pub fn decompose(&self) -> Vec<Mesh> {
        use crate::boolean::disjoint_sets::DisjointSets;
        let num_vert = self.num_vert();
        let mut uf = DisjointSets::new(num_vert);
        for e in 0..self.halfedge.len() {
            let he = HalfedgeId::from_usize(e);
            let (s, en) = (self.start(he), self.end(he));
            if s < en {
                uf.unite(s.u(), en.u());
            }
        }
        let (labels, num_comp) = uf.connected_components();
        if num_comp <= 1 {
            return vec![self.clone()];
        }

        let num_tri = self.num_tri();
        let mut meshes = Vec::new();
        for i in 0..num_comp {
            // Compact this component's verts (old → new), gathering positions.
            let mut old2new = vec![u32::MAX; num_vert];
            let mut vert_pos = Vec::new();
            for (v, &label) in labels.iter().enumerate() {
                if label == i {
                    old2new[v] = vert_pos.len() as u32;
                    vert_pos.push(self.vert_pos[v]);
                }
            }
            // Triangles owned by this component (first-vertex label), remapped to the new vert indices.
            let mut tris: Vec<[u32; 3]> = Vec::new();
            for f in 0..num_tri {
                let t = TriId::from_usize(f);
                if labels[self.start(t.halfedge(0)).u()] != i {
                    continue;
                }
                tris.push([
                    old2new[self.start(t.halfedge(0)).u()],
                    old2new[self.start(t.halfedge(1)).u()],
                    old2new[self.start(t.halfedge(2)).u()],
                ]);
            }
            if tris.is_empty() {
                continue;
            }
            let mut m = Mesh {
                vert_pos,
                num_prop: 0,
                epsilon: self.epsilon,
                tolerance: self.tolerance,
                ..Default::default()
            };
            m.create_halfedges(&tris);
            m.calculate_bbox();
            m.sort_geometry();
            meshes.push(m);
        }
        meshes
    }

    /// The big cutting cuboid covering the `+normal` side of the plane `dot(x, n̂) = origin_offset`
    /// (`Halfspace` in `manifold.cpp`, used by split/trim). A unit-ish `Cube(2, centered)` slab is scaled
    /// to `size` (large enough to enclose `b_box` past the plane), pushed to `origin_offset` along +x, and
    /// rotated so +x aligns with `n̂`. The four Translate/Scale/Rotate steps are FOLDED into one matrix
    /// (`Mat3x4 *`) and applied by a single [`Mesh::transform`], exactly as the lazy C++ composes them —
    /// so epsilon scales by the product's spectral norm, not each factor's. `asin`/`atan2` go through
    /// [`crate::mathf`] (native==wasm). Caller guarantees a non-empty (finite-bbox) mesh, so both the cube
    /// and the transform are valid — the `expect`s document that invariant.
    fn half_space(b_box: Box3, normal: Vec3, origin_offset: f64) -> Mesh {
        let normal = normal.normalize();
        // Base +x slab: Cube(2, centered) translated to [0,2]×[-1,1]×[-1,1].
        let cutter = Mesh::cube(Vec3::splat(2.0), true).expect("size 2 cube is valid");
        let size = (b_box.center() - normal * origin_offset).length() + 0.5 * b_box.size().length();
        let y_deg = crate::mathf::degrees(-crate::mathf::asin(normal.z));
        let z_deg = crate::mathf::degrees(crate::mathf::atan2(normal.y, normal.x));
        // Fold: Rotate ∘ Translate(offset) ∘ Scale(size) ∘ Translate(1,0,0), then transform once.
        let m = Mat3x4::rotate(0.0, y_deg, z_deg)
            * Mat3x4::translate(Vec3::new(origin_offset, 0.0, 0.0))
            * Mat3x4::scale(Vec3::splat(size))
            * Mat3x4::translate(Vec3::new(1.0, 0.0, 0.0));
        cutter.transform(m).expect("finite half-space transform")
    }

    /// Split by the plane `dot(x, n̂) = origin_offset` (`Manifold::SplitByPlane`, M.3.5), returning
    /// `(positive_side, negative_side)` — the pieces on the `+normal` and `−normal` sides. Builds the
    /// [`Mesh::half_space`] cutter from this mesh's own bbox, then one shared-`Boolean3`
    /// [`crate::boolean::boolean_result::split`]. An empty mesh yields two empties (verbatim early-out).
    /// `self` must be boolean-ready (epsilon/normals/`tri_ref` set).
    pub fn split_by_plane(&self, normal: Vec3, origin_offset: f64) -> (Mesh, Mesh) {
        if self.is_empty() {
            return (Mesh::default(), Mesh::default());
        }
        let cutter = Mesh::half_space(self.b_box, normal, origin_offset);
        crate::boolean::boolean_result::split(self, &cutter)
    }

    /// Trim to the `+normal` side of the plane (`Manifold::TrimByPlane`, M.3.5) — `self ∩ half_space`,
    /// i.e. exactly [`Mesh::split_by_plane`]'s first result but computed with a single Intersect boolean.
    /// An empty mesh stays empty. `self` must be boolean-ready.
    pub fn trim_by_plane(&self, normal: Vec3, origin_offset: f64) -> Mesh {
        if self.is_empty() {
            return Mesh::default();
        }
        let cutter = Mesh::half_space(self.b_box, normal, origin_offset);
        crate::boolean::boolean_result::boolean(self, &cutter, crate::boolean::OpType::Intersect)
    }
}

/// Remap a half-edge index to its position after its triangle is winding-flipped (`mesh_fixes.h`
/// `FlipHalfedge` = `3·(h/3) + (2 − h%3)`). The unpaired sentinel is preserved.
#[inline]
fn flip_halfedge(h: HalfedgeId) -> HalfedgeId {
    if h.is_none() {
        return h;
    }
    let u = h.u();
    HalfedgeId::from_usize(3 * (u / 3) + (2 - u % 3))
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

    /// M.3.1 — `transform`: translate/scale/rotate preserve manifoldness + scale volume by |det|; a
    /// MIRROR (det<0) must flip winding so the signed volume stays POSITIVE (the flip_tris gate).
    #[test]
    fn transform_moves_scales_and_mirrors() {
        let cube = Mesh::from_mesh_gl(&unit_cube()); // [0,1]³, volume 1
        let v3 = Vec3::new;

        // Translate: shape unchanged, bbox shifted.
        let t = Mat3x4 { x: v3(1., 0., 0.), y: v3(0., 1., 0.), z: v3(0., 0., 1.), w: v3(10., 20., 30.) };
        let moved = cube.transform(t).unwrap();
        assert!(moved.is_manifold());
        assert!((moved.volume() - 1.0).abs() < 1e-12);
        assert_eq!(moved.bounding_box().min, v3(10., 20., 30.));
        assert_eq!(moved.bounding_box().max, v3(11., 21., 31.));

        // Non-uniform scale: volume ×|det| = 2·3·4 = 24.
        let s = Mat3x4 { x: v3(2., 0., 0.), y: v3(0., 3., 0.), z: v3(0., 0., 4.), w: Vec3::ZERO };
        let scaled = cube.transform(s).unwrap();
        assert!(scaled.is_manifold());
        assert!((scaled.volume() - 24.0).abs() < 1e-12);

        // Mirror (det = −1): volume stays 1 AND POSITIVE — without the winding flip the signed volume
        // would be −1. Proves flip_tris re-oriented the surface + kept it manifold.
        let mirror = Mat3x4 { x: v3(-1., 0., 0.), y: v3(0., 1., 0.), z: v3(0., 0., 1.), w: Vec3::ZERO };
        let mirrored = cube.transform(mirror).unwrap();
        assert!(mirrored.is_manifold(), "mirror must stay manifold (flip_tris pairing)");
        assert!((mirrored.volume() - 1.0).abs() < 1e-12, "mirror volume {} != 1", mirrored.volume());
        assert_eq!(crate::check::genus(&mirrored), 0);

        // 90° rotation about z.
        let rot = Mat3x4 { x: v3(0., 1., 0.), y: v3(-1., 0., 0.), z: v3(0., 0., 1.), w: Vec3::ZERO };
        let rotated = cube.transform(rot).unwrap();
        assert!(rotated.is_manifold());
        assert!((rotated.volume() - 1.0).abs() < 1e-12);

        // Identity is an exact clone.
        assert_eq!(cube.transform(Mat3x4::IDENTITY).unwrap().volume(), cube.volume());
    }

    /// M.3.2 — a non-finite transform matrix is `Err(NonFiniteVertex)` (the eager translation of
    /// Manifold's `MakeEmpty(NonFiniteVertex)`), and the vert-position `is_finite` query agrees.
    #[test]
    fn transform_non_finite_is_an_error() {
        let cube = Mesh::from_mesh_gl(&unit_cube());
        assert!(cube.is_finite());
        let v3 = Vec3::new;
        let nan = Mat3x4 { x: v3(f64::NAN, 0., 0.), y: v3(0., 1., 0.), z: v3(0., 0., 1.), w: Vec3::ZERO };
        assert_eq!(cube.transform(nan).unwrap_err(), Error::NonFiniteVertex);
        let inf = Mat3x4 { x: v3(1., 0., 0.), y: v3(0., 1., 0.), z: v3(0., 0., 1.), w: v3(f64::INFINITY, 0., 0.) };
        assert_eq!(cube.transform(inf).unwrap_err(), Error::NonFiniteVertex);
        // A mesh with a NaN vertex fails the position-finiteness query.
        let mut bad = cube.clone();
        bad.vert_pos[0].x = f64::NAN;
        assert!(!bad.is_finite());
    }

    /// M.3.3 — `decompose`: two disjoint cubes split into two manifold parts of volume 1 each; a single
    /// cube stays one component.
    #[test]
    fn decompose_splits_disjoint_cubes() {
        let uc = unit_cube();
        // Two unit cubes 10 apart in x.
        let mut verts = uc.vert_properties.clone();
        for c in uc.vert_properties.chunks_exact(3) {
            verts.extend_from_slice(&[c[0] + 10.0, c[1], c[2]]);
        }
        let mut tris = uc.tri_verts.clone();
        for &idx in &uc.tri_verts {
            tris.push(idx + 8);
        }
        let mesh = Mesh::from_mesh_gl(&MeshGl { num_prop: 3, vert_properties: verts, tri_verts: tris });
        assert!(mesh.is_manifold());
        assert_eq!(mesh.num_tri(), 24);

        let parts = mesh.decompose();
        assert_eq!(parts.len(), 2, "two disjoint cubes → two components");
        for p in &parts {
            assert!(p.is_manifold(), "each part manifold");
            assert!((p.volume() - 1.0).abs() < 1e-12, "part volume {} != 1", p.volume());
            assert_eq!(p.num_tri(), 12);
            assert_eq!(p.num_vert(), 8);
        }
        let total: f64 = parts.iter().map(|p| p.volume()).sum();
        assert!((total - 2.0).abs() < 1e-12, "total volume preserved");

        // A single cube is one component (returned as a clone).
        assert_eq!(Mesh::from_mesh_gl(&unit_cube()).decompose().len(), 1);
    }

    /// M.3.4a — `set_properties`: stamp RGBA onto every vertex (the `color()` overwrite), confirm the
    /// stride grows 3→7 and each row carries the color; then read the old props back to double them; then
    /// strip to position-only.
    #[test]
    fn set_properties_stamps_color_and_reads_old() {
        let cube = Mesh::from_mesh_gl(&unit_cube()); // position-only, num_prop 0 (Impl `numProp_`)
        assert_eq!(cube.num_prop, 0);
        assert!(cube.properties.is_empty());

        // Stamp uniform RGBA (4 extra props). `old` is empty (source was position-only).
        let rgba = [0.2, 0.4, 0.6, 1.0];
        let red = cube.set_properties(4, |new, _pos, old| {
            assert!(old.is_empty());
            new.copy_from_slice(&rgba);
        });
        assert_eq!(red.num_prop, 4); // extras only; interchange (`to_mesh_gl`) would report 7
        assert_eq!(red.properties.len(), 8 * 4); // 8 prop-verts × 4 extras
        for row in red.properties.chunks_exact(4) {
            assert_eq!(row, rgba);
        }
        assert_eq!(red.volume(), 1.0); // positions untouched
        assert!(red.is_manifold());

        // Read the OLD props and double them (4 extras → 4 extras). Confirms `old` is wired.
        let doubled = red.set_properties(4, |new, _pos, old| {
            for (n, o) in new.iter_mut().zip(old) {
                *n = 2.0 * o;
            }
        });
        for row in doubled.properties.chunks_exact(4) {
            assert_eq!(row, [0.4, 0.8, 1.2, 2.0]);
        }

        // num_prop == 0 strips back to position-only.
        let stripped = red.set_properties(0, |_new, _pos, _old| unreachable!());
        assert_eq!(stripped.num_prop, 0);
        assert!(stripped.properties.is_empty());
        assert_eq!(stripped.to_mesh_gl(), unit_cube());
    }

    /// M.3.4b.1 — the property model is DECOUPLED: `prop_vert` indexes `properties` in its OWN space, so
    /// `num_prop_vert` can exceed `num_vert` (a seam-split vertex), and a half-edge reads its property row
    /// by `prop_vert`, independent of the geometric `start_vert`. Exercises the data-model invariant
    /// directly — no geometry op — which is what the downstream ports (4b.4/4b.5) will lean on.
    #[test]
    fn decoupled_prop_verts_index_their_own_space() {
        let (a, b, c) = (VertId::new(0), VertId::new(1), VertId::new(2));
        let he = |s: VertId, p: usize| Halfedge {
            start_vert: s,
            paired_halfedge: HalfedgeId::NONE,
            prop_vert: VertId::from_usize(p),
        };
        // 3 geometric verts, but 4 PROP-verts: the corner at `a` carries prop-row 3 (a color seam), not
        // row 0 — so `properties` has one more row than `vert_pos`.
        let mesh = Mesh {
            vert_pos: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)],
            halfedge: vec![he(a, 3), he(b, 1), he(c, 2)],
            num_prop: 2,
            #[rustfmt::skip]
            properties: vec![10.0, 11.0,  20.0, 21.0,  30.0, 31.0,  40.0, 41.0],
            ..Default::default()
        };
        assert_eq!(mesh.num_vert(), 3);
        assert_eq!(mesh.num_prop_vert(), 4, "prop-verts decouple from geometric verts");
        let corner_a = HalfedgeId::new(0);
        assert_eq!(mesh.start(corner_a), a, "geometrically at vert a");
        assert_eq!(mesh.prop(corner_a).u(), 3, "but reads the seam prop-row, not row 0");
        let pv = mesh.prop(corner_a).u();
        assert_eq!(&mesh.properties[pv * mesh.num_prop..][..mesh.num_prop], [40.0, 41.0]);

        // Position-only degenerates back to 1:1: num_prop_vert == num_vert.
        let plain = Mesh::from_mesh_gl(&unit_cube());
        assert_eq!(plain.num_prop, 0);
        assert_eq!(plain.num_prop_vert(), plain.num_vert());
    }

    /// M.3.5 — the `cube` primitive: centered/uncentered, volume = size product, manifold + genus 0,
    /// and the invalid-size guard.
    #[test]
    fn cube_primitive_geometry_and_guards() {
        let v3 = Vec3::new;
        // Centered 2-cube → [-1,1]³, volume 8.
        let c = Mesh::cube(Vec3::splat(2.0), true).unwrap();
        assert!(c.is_manifold());
        assert_eq!(c.num_tri(), 12);
        assert_eq!(c.num_vert(), 8);
        assert!((c.volume() - 8.0).abs() < 1e-12, "vol {}", c.volume());
        assert_eq!(crate::check::genus(&c), 0);
        assert_eq!(c.bounding_box().min, v3(-1.0, -1.0, -1.0));
        assert_eq!(c.bounding_box().max, v3(1.0, 1.0, 1.0));

        // Uncentered non-uniform box → [0,2]×[0,3]×[0,4], volume 24.
        let b = Mesh::cube(v3(2.0, 3.0, 4.0), false).unwrap();
        assert!(b.is_manifold());
        assert!((b.volume() - 24.0).abs() < 1e-12, "vol {}", b.volume());
        assert_eq!(b.bounding_box().min, v3(0.0, 0.0, 0.0));
        assert_eq!(b.bounding_box().max, v3(2.0, 3.0, 4.0));

        // Invalid sizes → InvalidConstruction (C++ Invalid()).
        assert_eq!(Mesh::cube(v3(-1.0, 1.0, 1.0), true).unwrap_err(), Error::InvalidConstruction);
        assert_eq!(Mesh::cube(Vec3::ZERO, false).unwrap_err(), Error::InvalidConstruction);
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
        assert_eq!(mesh.num_prop, 4); // Impl `numProp_` = interchange 7 − 3 position channels
        assert_eq!(mesh.properties.len(), 8 * 4);
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
