//! The geometry backend trait (J.1) — the CSG op vocabulary the geometry lowering (J.2+) targets.
//!
//! Abstracted for ONE reason: the interface suite has to run under BOTH miri and ASAN, and neither can
//! do the job alone. miri can't cross the Manifold C++ FFI boundary, so it runs on a pure-Rust MOCK;
//! ASAN instruments the real binary (FFI included), so it runs on real Manifold and catches the memory
//! bugs miri structurally can't see. That split replaces the impossible "miri directly on the FFI".
//!
//! Empty geometry is a first-class value here (a degenerate primitive, an empty union): the backend
//! solid is empty-aware, and the ops encode the empty CSG algebra — ∅∪x = x, ∅−x = ∅, x−∅ = x,
//! ∅∩x = ∅ — so a lowered CSG tree behaves the same whether a subtree collapsed to nothing or not.

use fab_lang::{
    Affine, Affine2, ExtrudeKind, Geo, GeoNode, Join2D, Mesh, Rgba, Shape2D, Tri, Vec2, Vec3,
};

/// A geometry backend: tessellated meshes → solids, combined via CSG + affine transforms. `Solid` is
/// the backend's opaque handle (real Manifold's is `!Send`; the mock's is inert data).
///
/// The 2D subsystem (J.3) rides the same trait: `Shape` is the backend's 2D-region handle (Manifold
/// `CrossSection`), a SECOND associated type so no op is ever dimension-polymorphic — 2D and 3D are
/// distinct types end to end (see SPEC "2D subsystem"). The two dimensions meet only at the bridges:
/// [`extrude`](GeometryBackend::extrude) (2D→3D) and [`projection`](GeometryBackend::projection)
/// (3D→2D).
pub trait GeometryBackend {
    /// The backend's solid handle.
    type Solid;
    /// The backend's 2D-region handle (Manifold `CrossSection`).
    type Shape;

    /// A tessellated mesh (a fab-lang primitive) → a backend solid. An empty mesh → the empty solid.
    fn leaf(&self, mesh: &Mesh) -> Self::Solid;
    /// Boolean union.
    fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Boolean difference (`a − b`).
    fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Boolean intersection.
    fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Convex hull of the operands COMBINED (`hull()`) — N-ary, not a pairwise fold. An empty list, or
    /// all-empty operands, → the empty solid.
    fn hull(&self, solids: &[Self::Solid]) -> Self::Solid;
    /// An affine transform (OpenSCAD `multmatrix`, covering translate / rotate / scale / mirror).
    fn transform(&self, s: &Self::Solid, m: &Affine) -> Self::Solid;
    /// Set the solid's color (`color()`) — sets EVERY vertex, so outermost `color()` wins (J.2.9).
    fn color(&self, s: &Self::Solid, rgba: Rgba) -> Self::Solid;
    /// Extract the result as a triangle mesh (the empty solid → an empty mesh).
    fn to_mesh(&self, s: &Self::Solid) -> Mesh;
    /// Whether the solid is empty (no geometry) — the differential's `Empty` outcome.
    fn is_empty(&self, s: &Self::Solid) -> bool;

    // ── 2D surface (J.3) ─────────────────────────────────────────────────────────────────────────

    /// Closed contours (outer boundary + holes) → a 2D region — the `square`/`circle`/`polygon` leaf.
    /// No contours → the empty region.
    fn leaf_2d(&self, contours: &[Vec<[f64; 2]>]) -> Self::Shape;
    /// 2D boolean union.
    fn union_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape;
    /// 2D boolean difference (`a − b`).
    fn difference_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape;
    /// 2D boolean intersection.
    fn intersection_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape;
    /// `offset()` — inflate by `delta` (negative shrinks), finishing convex corners per `join`.
    /// `segments` is the `Round` join's facet count (`$fn`-resolved upstream; ignored by miter/bevel).
    fn offset_2d(&self, s: &Self::Shape, delta: f64, join: Join2D, segments: u32) -> Self::Shape;
    /// A 2D affine transform (translate / rotate / scale / mirror on a 2D shape).
    fn transform_2d(&self, s: &Self::Shape, m: &Affine2) -> Self::Shape;
    /// The 2D→3D bridge — sweep a region into a solid (`linear_extrude` / `rotate_extrude`). An empty
    /// region → the empty solid.
    fn extrude(&self, s: &Self::Shape, kind: &ExtrudeKind) -> Self::Solid;
    /// The 3D→2D bridge — flatten a solid to a region (`projection`). `cut` slices at `z = 0`; else the
    /// shadow. An empty solid → the empty region.
    fn projection(&self, s: &Self::Solid, cut: bool) -> Self::Shape;
    /// Extract the region as closed point contours (the 2D differential's data).
    fn to_polygons(&self, s: &Self::Shape) -> Vec<Vec<[f64; 2]>>;
    /// Whether the region is empty (no area).
    fn is_empty_2d(&self, s: &Self::Shape) -> bool;
}

/// Lower a dimension-tagged [`Geo`] result to a backend SOLID — the top-level lowering entry a consumer
/// reaches for after [`fab_lang::evaluate_geometry`]. A 3D tree lowers via [`build`]; a 2D result has no
/// 3D solid, so on this axis it lowers to the empty solid (its 2D region is reached by matching the
/// [`Geo::D2`] and calling [`build_2d`] on the `Shape2D` — the extrude/projection path, J.3).
pub fn build_geo<B: GeometryBackend>(geo: &Geo, backend: &B) -> B::Solid {
    match geo {
        Geo::D3(node) => build(node, backend),
        Geo::D2(_) => backend.leaf(&Mesh::new()),
    }
}

/// Lower a fab-lang CSG tree ([`GeoNode`], J.2) to a backend solid — the geometry lowering. This is
/// the integration seam: fab-lang builds the backend-agnostic tree, the backend does the real CSG.
/// Recursion is bounded by the tree depth (the parser's `MAX_DEPTH`), so it can't overflow the stack.
pub fn build<B: GeometryBackend>(node: &GeoNode, backend: &B) -> B::Solid {
    match node {
        GeoNode::Empty => backend.leaf(&Mesh::new()),
        GeoNode::Leaf(mesh) => backend.leaf(mesh),
        GeoNode::Transform { matrix, child } => backend.transform(&build(child, backend), matrix),
        GeoNode::Union(kids) => reduce(kids, backend, |b, x, y| b.union(x, y)),
        GeoNode::Difference(kids) => reduce(kids, backend, |b, x, y| b.difference(x, y)),
        GeoNode::Intersection(kids) => reduce(kids, backend, |b, x, y| b.intersection(x, y)),
        // hull is N-ary — the backend hulls the whole operand set at once (not a pairwise fold).
        GeoNode::Hull(kids) => {
            backend.hull(&kids.iter().map(|k| build(k, backend)).collect::<Vec<_>>())
        }
        // The 2D→3D bridge: lower the 2D child to a Shape, then sweep it into a Solid (J.3.4/J.3.5).
        GeoNode::Extrude { kind, child } => backend.extrude(&build_2d(child, backend), kind),
        // Color sets EVERY vertex of the child subtree (J.2.9). Outermost `color()` wins because the
        // enclosing node's color op overwrites any inner one; distinct colors survive a union.
        GeoNode::Color { color, child } => backend.color(&build(child, backend), *color),
    }
}

/// Lower a fab-lang 2D tree ([`Shape2D`], J.3) to a backend region — the 2D half of the geometry
/// lowering, mutually recursive with [`build`] across the `Projection` (3D→2D) bridge. Bounded by tree
/// depth (parser `MAX_DEPTH`), like `build`.
pub fn build_2d<B: GeometryBackend>(shape: &Shape2D, backend: &B) -> B::Shape {
    match shape {
        Shape2D::Empty => backend.leaf_2d(&[]),
        Shape2D::Polygon(contours) => {
            let raw: Vec<Vec<[f64; 2]>> = contours
                .iter()
                .map(|c| c.iter().map(|p| p.to_array()).collect())
                .collect();
            backend.leaf_2d(&raw)
        }
        Shape2D::Union(kids) => reduce_2d(kids, backend, B::union_2d),
        Shape2D::Difference(kids) => reduce_2d(kids, backend, B::difference_2d),
        Shape2D::Intersection(kids) => reduce_2d(kids, backend, B::intersection_2d),
        Shape2D::Offset {
            delta,
            join,
            segments,
            child,
        } => backend.offset_2d(&build_2d(child, backend), *delta, *join, *segments),
        Shape2D::Transform { matrix, child } => {
            backend.transform_2d(&build_2d(child, backend), matrix)
        }
        // The 3D→2D bridge: lower the 3D child to a Solid, then flatten it to a region (J.3.6).
        Shape2D::Projection { cut, child } => backend.projection(&build(child, backend), *cut),
    }
}

/// Fold 2D children left-to-right with `combine` (the empty algebra lives in the backend ops). An empty
/// child list → the empty region.
fn reduce_2d<B: GeometryBackend>(
    kids: &[Shape2D],
    backend: &B,
    combine: impl Fn(&B, &B::Shape, &B::Shape) -> B::Shape,
) -> B::Shape {
    let mut shapes = kids.iter().map(|k| build_2d(k, backend));
    match shapes.next() {
        Some(first) => shapes.fold(first, |acc, s| combine(backend, &acc, &s)),
        None => backend.leaf_2d(&[]),
    }
}

/// Fold children left-to-right with `combine` (the empty algebra lives in the backend ops). An empty
/// child list → the empty solid; for `difference` the fold is `first − rest`.
fn reduce<B: GeometryBackend>(
    kids: &[GeoNode],
    backend: &B,
    combine: impl Fn(&B, &B::Solid, &B::Solid) -> B::Solid,
) -> B::Solid {
    let mut solids = kids.iter().map(|k| build(k, backend));
    match solids.next() {
        Some(first) => solids.fold(first, |acc, s| combine(backend, &acc, &s)),
        None => backend.leaf(&Mesh::new()),
    }
}

// ─────────────────────────────── the real backend (Manifold) ───────────────────────────────────

/// The real backend — Manifold via [`kernel::Solid`](crate::kernel::Solid). The solid is
/// `Option`-wrapped so empty geometry (`None`) is representable without a Manifold empty-constructor,
/// and so the ops can encode the empty algebra directly. This is the ASAN-tested path.
#[cfg(feature = "kernel")]
pub struct ManifoldBackend;

#[cfg(feature = "kernel")]
impl GeometryBackend for ManifoldBackend {
    type Solid = Option<crate::kernel::Solid>;
    // `Section` (Manifold `CrossSection`) is empty-aware natively (`empty()` / `is_empty()`), so — unlike
    // `Solid` — it needs no `Option` wrapper to represent the empty region.
    type Shape = crate::kernel::Section;

    fn leaf(&self, mesh: &Mesh) -> Self::Solid {
        // `from_indexed` rejects an empty mesh (→ None); a non-manifold mesh also → None (polyhedron
        // validation tightens at J.2 — for now the lowering feeds valid tessellations).
        crate::kernel::Solid::from_indexed(&mesh.verts, &mesh.tris).ok()
    }

    fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        match (a.as_ref(), b.as_ref()) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (Some(x), None) | (None, Some(x)) => Some(x.clone()),
            (None, None) => None,
        }
    }

    fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        match (a.as_ref(), b.as_ref()) {
            (Some(a), Some(b)) => Some(a.difference(b)),
            (Some(x), None) => Some(x.clone()), // x − ∅ = x
            (None, _) => None,                  // ∅ − x = ∅
        }
    }

    fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        match (a.as_ref(), b.as_ref()) {
            (Some(a), Some(b)) => Some(a.intersection(b)),
            _ => None, // ∅ ∩ x = ∅
        }
    }

    fn hull(&self, solids: &[Self::Solid]) -> Self::Solid {
        // Hull the NON-empty operands combined; an empty operand contributes no vertices (all-empty → ∅).
        let present: Vec<crate::kernel::Solid> = solids.iter().flatten().cloned().collect();
        (!present.is_empty()).then(|| crate::kernel::Solid::batch_hull(&present))
    }

    fn transform(&self, s: &Self::Solid, m: &Affine) -> Self::Solid {
        // Solid::transform owns the row→column-major transpose now (it re-transposes to Manifold's
        // layout), so this just forwards the Affine.
        s.as_ref().map(|s| s.transform(m))
    }

    fn to_mesh(&self, s: &Self::Solid) -> Mesh {
        match s {
            Some(s) => {
                let (verts, tris) = s.to_indexed();
                Mesh { verts, tris }
            }
            None => Mesh::new(),
        }
    }

    fn color(&self, s: &Self::Solid, rgba: Rgba) -> Self::Solid {
        s.as_ref().map(|s| s.with_color(rgba))
    }

    fn is_empty(&self, s: &Self::Solid) -> bool {
        s.as_ref().is_none_or(crate::kernel::Solid::is_empty)
    }

    // ── 2D surface (J.3) — delegates to kernel::Section (Manifold CrossSection) ───────────────────

    fn leaf_2d(&self, contours: &[Vec<[f64; 2]>]) -> Self::Shape {
        crate::kernel::Section::from_polygons(contours)
    }

    fn union_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        a.union(b)
    }

    fn difference_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        a.difference(b)
    }

    fn intersection_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        a.intersection(b)
    }

    fn offset_2d(&self, s: &Self::Shape, delta: f64, join: Join2D, segments: u32) -> Self::Shape {
        s.offset(delta, join, i32::try_from(segments).unwrap_or(i32::MAX))
    }

    fn transform_2d(&self, s: &Self::Shape, m: &Affine2) -> Self::Shape {
        s.transform(m)
    }

    fn extrude(&self, s: &Self::Shape, kind: &ExtrudeKind) -> Self::Solid {
        // An empty profile sweeps to nothing (Manifold would build a degenerate manifold otherwise).
        (!s.is_empty()).then(|| s.extrude(kind))
    }

    fn projection(&self, s: &Self::Solid, cut: bool) -> Self::Shape {
        s.as_ref()
            .map_or_else(crate::kernel::Section::empty, |s| s.project_2d(cut))
    }

    fn to_polygons(&self, s: &Self::Shape) -> Vec<Vec<[f64; 2]>> {
        s.to_polygons()
    }

    fn is_empty_2d(&self, s: &Self::Shape) -> bool {
        s.is_empty()
    }
}

// ─────────────────────────────── the mock backend (pure Rust) ──────────────────────────────────

/// A pure-Rust MOCK solid. It does NOT compute real booleans (miri can't call Manifold) — it tracks a
/// representative mesh plus an operation count, so the interface suite exercises the SAME Rust-side
/// plumbing (mesh clone / append / index arithmetic, the affine math, trait dispatch) that the real
/// path uses, under miri. It DOES honor the empty algebra so the suite's `is_empty` assertions hold on
/// both backends. Empty ⇔ no vertices.
#[derive(Clone, Default)]
pub struct MockSolid {
    mesh: Mesh,
    /// CSG ops applied — lets a test confirm the tree was actually walked, not short-circuited.
    ops: u32,
}

impl MockSolid {
    fn is_empty(&self) -> bool {
        self.mesh.verts.is_empty()
    }
}

/// Append two meshes (offsetting the second's indices) — exercises the mesh-append + reindex path.
fn append(a: &Mesh, b: &Mesh) -> Mesh {
    let offset = u32::try_from(a.verts.len()).unwrap_or(u32::MAX);
    let mut verts = a.verts.clone();
    verts.extend_from_slice(&b.verts);
    let mut tris = a.tris.clone();
    tris.extend(b.tris.iter().map(|t| {
        let [x, y, z] = t.indices();
        Tri::new(
            x.saturating_add(offset),
            y.saturating_add(offset),
            z.saturating_add(offset),
        )
    }));
    Mesh { verts, tris }
}

/// A pure-Rust MOCK 2D region — the 2D counterpart of [`MockSolid`]. Tracks its contours plus an op
/// count; it does NOT compute real 2D booleans/offsets (miri can't call Manifold), it exercises the
/// same Rust-side dispatch + point arithmetic the real path uses. Empty ⇔ no points.
#[derive(Clone, Default)]
pub struct MockShape {
    contours: Vec<Vec<[f64; 2]>>,
    /// 2D ops applied — lets a test confirm the tree was actually walked.
    ops: u32,
}

impl MockShape {
    fn is_empty(&self) -> bool {
        self.contours.iter().all(Vec::is_empty)
    }
}

/// The mock geometry backend — the miri-tested path.
pub struct MockBackend;

impl GeometryBackend for MockBackend {
    type Solid = MockSolid;
    type Shape = MockShape;

    fn leaf(&self, mesh: &Mesh) -> Self::Solid {
        MockSolid {
            mesh: mesh.clone(),
            ops: 0,
        }
    }

    fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        if a.is_empty() {
            return b.clone();
        }
        if b.is_empty() {
            return a.clone();
        }
        MockSolid {
            mesh: append(&a.mesh, &b.mesh),
            ops: a.ops + b.ops + 1,
        }
    }

    fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        if a.is_empty() {
            return MockSolid::default(); // ∅ − x = ∅
        }
        // x − y ⊆ x; the mock can't carve, so it keeps a's mesh (structure, not geometry).
        MockSolid {
            mesh: a.mesh.clone(),
            ops: a.ops + b.ops + 1,
        }
    }

    fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
        if a.is_empty() || b.is_empty() {
            return MockSolid::default(); // ∅ ∩ x = ∅
        }
        MockSolid {
            mesh: a.mesh.clone(),
            ops: a.ops + b.ops + 1,
        }
    }

    fn hull(&self, solids: &[Self::Solid]) -> Self::Solid {
        // The mock can't compute a real hull — it appends the NON-empty operands' meshes (structure, not
        // geometry) + bumps ops, honoring the empty algebra (all-empty → empty). The real hull lives in
        // ManifoldBackend; this just walks the op so the interface suite exercises the dispatch under miri.
        let mesh = solids
            .iter()
            .filter(|s| !s.is_empty())
            .fold(Mesh::new(), |acc, s| append(&acc, &s.mesh));
        let ops = solids.iter().map(|s| s.ops).sum::<u32>() + 1;
        MockSolid { mesh, ops }
    }

    fn transform(&self, s: &Self::Solid, m: &Affine) -> Self::Solid {
        let verts = s.mesh.verts.iter().map(|&v| m.apply(v)).collect();
        MockSolid {
            mesh: Mesh {
                verts,
                tris: s.mesh.tris.clone(),
            },
            ops: s.ops + 1,
        }
    }

    fn color(&self, s: &Self::Solid, _rgba: Rgba) -> Self::Solid {
        // The mock doesn't model color VALUES (that's the real kernel's SetProperties, tested via a unit
        // test + the differential) — it just walks the op so the interface suite exercises the dispatch
        // under miri. Geometry is unchanged (color moves no vertices).
        MockSolid {
            mesh: s.mesh.clone(),
            ops: s.ops + 1,
        }
    }

    fn to_mesh(&self, s: &Self::Solid) -> Mesh {
        s.mesh.clone()
    }

    fn is_empty(&self, s: &Self::Solid) -> bool {
        s.is_empty()
    }

    // ── 2D surface (J.3) — structure-only, honoring the empty algebra so both backends' is_empty agree ──

    fn leaf_2d(&self, contours: &[Vec<[f64; 2]>]) -> Self::Shape {
        MockShape {
            contours: contours.to_vec(),
            ops: 0,
        }
    }

    fn union_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        if a.is_empty() {
            return b.clone();
        }
        if b.is_empty() {
            return a.clone();
        }
        let mut contours = a.contours.clone();
        contours.extend(b.contours.iter().cloned());
        MockShape {
            contours,
            ops: a.ops + b.ops + 1,
        }
    }

    fn difference_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        if a.is_empty() {
            return MockShape::default(); // ∅ − x = ∅
        }
        // x − y ⊆ x; the mock can't carve, so it keeps a's contours (structure, not geometry).
        MockShape {
            contours: a.contours.clone(),
            ops: a.ops + b.ops + 1,
        }
    }

    fn intersection_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
        if a.is_empty() || b.is_empty() {
            return MockShape::default(); // ∅ ∩ x = ∅
        }
        MockShape {
            contours: a.contours.clone(),
            ops: a.ops + b.ops + 1,
        }
    }

    fn offset_2d(
        &self,
        s: &Self::Shape,
        _delta: f64,
        _join: Join2D,
        _segments: u32,
    ) -> Self::Shape {
        if s.is_empty() {
            return MockShape::default();
        }
        // The mock can't inflate — it keeps the contours + bumps ops (real offset lives in ManifoldBackend).
        MockShape {
            contours: s.contours.clone(),
            ops: s.ops + 1,
        }
    }

    fn transform_2d(&self, s: &Self::Shape, m: &Affine2) -> Self::Shape {
        let contours = s
            .contours
            .iter()
            .map(|c| {
                c.iter()
                    .map(|&p| m.apply(Vec2::from_array(p)).to_array())
                    .collect()
            })
            .collect();
        MockShape {
            contours,
            ops: s.ops + 1,
        }
    }

    fn extrude(&self, s: &Self::Shape, _kind: &ExtrudeKind) -> Self::Solid {
        if s.is_empty() {
            return MockSolid::default(); // an empty profile sweeps to nothing
        }
        // The mock can't sweep — it lifts the contour points to z=0 verts so is_empty()/dispatch hold.
        let verts = s
            .contours
            .iter()
            .flatten()
            .map(|&[x, y]| Vec3::new(x, y, 0.0))
            .collect();
        MockSolid {
            mesh: Mesh {
                verts,
                tris: Vec::new(),
            },
            ops: s.ops + 1,
        }
    }

    fn projection(&self, s: &Self::Solid, _cut: bool) -> Self::Shape {
        if s.is_empty() {
            return MockShape::default();
        }
        // The mock flattens the solid's verts to one XY contour (structure, not a real slice/shadow).
        let contour: Vec<[f64; 2]> = s.mesh.verts.iter().map(|v| [v.x, v.y]).collect();
        MockShape {
            contours: vec![contour],
            ops: s.ops + 1,
        }
    }

    fn to_polygons(&self, s: &Self::Shape) -> Vec<Vec<[f64; 2]>> {
        s.contours.clone()
    }

    fn is_empty_2d(&self, s: &Self::Shape) -> bool {
        s.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{GeometryBackend, MockBackend};

    /// Drive the WHOLE op surface of a backend and assert the invariants that hold for ANY correct
    /// backend (the exact geometry is the backend's business; the sanitizers are the real oracle). Run
    /// under miri on the mock and under ASAN on real Manifold — same logic, two memory models.
    pub fn exercise<B: GeometryBackend>(b: &B) {
        let cube = fab_lang::evaluate("cube(10);").expect("cube tessellates");
        let sphere = fab_lang::evaluate("sphere(6, $fn = 16);").expect("sphere tessellates");
        let cube = b.leaf(&cube);
        let sphere = b.leaf(&sphere);
        assert!(!b.is_empty(&cube));
        assert!(!b.is_empty(&sphere));

        // Every op runs + yields extractable mesh data. cube(10)=[0,10]³ and sphere(6) centered at the
        // origin overlap but neither contains the other, so all three booleans are non-empty.
        let u = b.union(&cube, &sphere);
        let d = b.difference(&cube, &sphere);
        let i = b.intersection(&cube, &sphere);
        let moved = b.transform(
            &cube,
            &fab_lang::Affine::row_major([
                1.0, 0.0, 0.0, 5.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
            ]),
        );
        for s in [&u, &d, &i, &moved] {
            assert!(!b.is_empty(s));
            let _ = b.to_mesh(s).tri_count(); // extract path exercised
        }

        // hull() — N-ary; the convex hull of the operand set (J.4.1). Fresh leaves (the earlier meshes
        // were shadowed into solids), then the empty algebra: all-empty → ∅, [x, ∅] → non-empty.
        let a = b.leaf(&fab_lang::evaluate("cube(4);").expect("cube"));
        let c = b.leaf(&fab_lang::evaluate("sphere(3, $fn = 8);").expect("sphere"));
        let h = b.hull(&[a, c]);
        assert!(!b.is_empty(&h));
        let _ = b.to_mesh(&h).tri_count(); // extract path exercised
        assert!(b.is_empty(&b.hull(&[]))); // hull of nothing → ∅
        assert!(b.is_empty(&b.hull(&[b.leaf(&fab_lang::Mesh::new())]))); // hull of ∅ → ∅
        assert!(!b.is_empty(&b.hull(&[
            b.leaf(&fab_lang::evaluate("cube(4);").expect("cube")),
            b.leaf(&fab_lang::Mesh::new()),
        ]))); // hull of [x, ∅] = hull(x)

        // The empty algebra — must hold identically on both backends.
        let empty = b.leaf(&fab_lang::Mesh::new());
        assert!(b.is_empty(&empty));
        assert!(b.is_empty(&b.intersection(&cube, &empty))); // x ∩ ∅ = ∅
        assert!(b.is_empty(&b.difference(&empty, &cube))); // ∅ − x = ∅
        assert!(!b.is_empty(&b.union(&cube, &empty))); // x ∪ ∅ = x
        assert!(!b.is_empty(&b.difference(&cube, &empty))); // x − ∅ = x

        exercise_2d(b); // the 2D surface + both dimension bridges, same two memory models
    }

    /// Drive the WHOLE 2D op surface + both dimension bridges (J.3), asserting the invariants that hold
    /// for ANY correct backend. Same discipline as [`exercise`]: miri on the mock, ASAN on Manifold.
    pub fn exercise_2d<B: GeometryBackend>(b: &B) {
        use fab_lang::{Affine2, ExtrudeKind, Join2D};
        // A 2×2 square leaf + the empty region.
        let square = |b: &B| b.leaf_2d(&[vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]]);
        let empty = b.leaf_2d(&[]);
        assert!(b.is_empty_2d(&empty));
        let sq = square(b);
        assert!(!b.is_empty_2d(&sq));

        // Booleans + the empty algebra (identical on both backends).
        assert!(!b.is_empty_2d(&b.union_2d(&sq, &square(b))));
        assert!(!b.is_empty_2d(&b.union_2d(&sq, &empty))); // x ∪ ∅ = x
        assert!(!b.is_empty_2d(&b.difference_2d(&sq, &empty))); // x − ∅ = x
        assert!(b.is_empty_2d(&b.difference_2d(&empty, &sq))); // ∅ − x = ∅
        assert!(b.is_empty_2d(&b.intersection_2d(&sq, &empty))); // x ∩ ∅ = ∅
        assert!(!b.is_empty_2d(&b.intersection_2d(&sq, &square(b)))); // sq ∩ sq = sq

        // Offset (grow), transform (translate +x 3), and the extract path.
        assert!(!b.is_empty_2d(&b.offset_2d(&sq, 1.0, Join2D::Round, 16)));
        let moved = b.transform_2d(&sq, &Affine2::row_major([1.0, 0.0, 3.0, 0.0, 1.0, 0.0]));
        assert!(!b.is_empty_2d(&moved));
        assert!(!b.to_polygons(&sq).is_empty());
        assert!(b.to_polygons(&empty).is_empty());

        // The two bridges + both extrude kinds. Linear (resting) → box; its shadow (cut=false) is 2D.
        let lin = ExtrudeKind::Linear {
            height: 5.0,
            twist: 0.0,
            scale: [1.0, 1.0],
            slices: 1,
            facets: 0,
            center: false,
        };
        let box3d = b.extrude(&sq, &lin);
        assert!(!b.is_empty(&box3d));
        assert!(b.is_empty(&b.extrude(&empty, &lin))); // extrude of ∅ = ∅
        assert!(!b.is_empty_2d(&b.projection(&box3d, false))); // shadow
        assert!(b.is_empty_2d(&b.projection(&b.extrude(&empty, &lin), false))); // projection of ∅ = ∅
        // A CENTERED extrude straddles z=0, so the cut=true slice is a real cross-section.
        let centered = b.extrude(
            &sq,
            &ExtrudeKind::Linear {
                height: 5.0,
                twist: 0.0,
                scale: [1.0, 1.0],
                slices: 1,
                facets: 0,
                center: true,
            },
        );
        assert!(!b.is_empty_2d(&b.projection(&centered, true)));
        // Rotate extrude of a profile offset from the axis → a ring solid.
        let ring = b.extrude(
            &moved,
            &ExtrudeKind::Rotate {
                angle: 360.0,
                segments: 32,
            },
        );
        assert!(!b.is_empty(&ring));
    }

    #[test]
    fn mock_backend_interface() {
        exercise(&MockBackend);
    }

    #[cfg(feature = "kernel")]
    #[test]
    fn manifold_backend_interface() {
        exercise(&super::ManifoldBackend);
    }

    // J.3.1 — the tree-walker lowers a hand-built 2D tree through BOTH dimension bridges on the REAL
    // Manifold backend, checked by exact volume + area. (The evaluator that PRODUCES Shape2D is J.3.2+;
    // this pins the seam itself.) A 2×2 square extruded 5 tall → vol 20; its shadow back to 2D → area 4.
    #[cfg(feature = "kernel")]
    #[test]
    fn extrude_and_projection_lowering() {
        use fab_lang::{ExtrudeKind, GeoNode, Shape2D, Vec2};
        let square = Shape2D::Polygon(vec![vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(0.0, 2.0),
        ]]);
        let extruded = GeoNode::Extrude {
            kind: ExtrudeKind::Linear {
                height: 5.0,
                twist: 0.0,
                scale: [1.0, 1.0],
                slices: 1,
                facets: 0,
                center: false,
            },
            child: Box::new(square),
        };
        let solid = super::build(&extruded, &super::ManifoldBackend).expect("extrude → a solid");
        assert!((solid.volume() - 20.0).abs() < 1e-6); // 2·2·5

        // projection(cut=false) of that box back to 2D → the 2×2 shadow, area 4.
        let projected = Shape2D::Projection {
            cut: false,
            child: Box::new(extruded),
        };
        let region = super::build_2d(&projected, &super::ManifoldBackend);
        assert!((region.area() - 4.0).abs() < 1e-6);
    }

    // J.3.3 — 2D booleans + offset lower through the REAL Manifold `CrossSection`, checked by exact AREA.
    // The miter/shrink/boolean cases are EXACT (axis-aligned squares → integer areas); the round + bevel
    // cases are pinned to what OpenSCAD 2026.06.12 renders (both engines share Clipper2, so they agree).
    #[cfg(feature = "kernel")]
    #[test]
    fn offset_and_2d_booleans_measure_correct_areas() {
        use fab_lang::{Geo, evaluate_geometry};
        let area = |scad: &str| -> f64 {
            match evaluate_geometry(scad).expect("evaluates") {
                Geo::D2(shape) => super::build_2d(&shape, &super::ManifoldBackend).area(),
                other => panic!("expected a 2D result, got {other:?}"),
            }
        };
        // offset: delta (miter) grows to sharp corners; a negative r shrinks with sharp inner corners.
        assert!((area("offset(delta = 2) square(5);") - 81.0).abs() < 1e-9); // (5 + 2·2)²
        assert!((area("offset(-1) square(5);") - 9.0).abs() < 1e-9); // (5 − 2·1)²
        // round (r) + bevel (chamfer) match the oracle at a fixed $fn (Clipper2 on both sides).
        assert!((area("offset(2, $fn = 64) square(5);") - 77.5462).abs() < 1e-3); // rounded corners
        assert!((area("offset(delta = 2, chamfer = true) square(5);") - 78.2548).abs() < 1e-3); // bevel
        // 2D booleans over two 4×4 squares — [0,4]² and [2,6]², overlap [2,4]² (area 4).
        assert!((area("square(4); translate([2, 2]) square(4);") - 28.0).abs() < 1e-9); // union 16+16−4
        assert!(
            (area("difference() { square(4); translate([2, 2]) square(4); }") - 12.0).abs() < 1e-9
        ); // 16 − 4
        assert!(
            (area("intersection() { square(4); translate([2, 2]) square(4); }") - 4.0).abs() < 1e-9
        ); // the overlap
    }

    // J.2.3/J.2.7 — the tree-walker lowers CSG booleans + transforms through the REAL Manifold backend
    // correctly, checked by exact VOLUME (no oracle re-import, which the harness can't do for boolean
    // meshes yet). cube(5) sits inside cube(10)'s corner (both [0,size]³), so the results are exact.
    #[cfg(feature = "kernel")]
    #[test]
    fn boolean_and_transform_lowering_volumes() {
        let vol = |scad: &str| -> f64 {
            match super::build_geo(
                &fab_lang::evaluate_geometry(scad).expect("evaluates"),
                &super::ManifoldBackend,
            ) {
                Some(s) => s.volume(),
                None => 0.0,
            }
        };
        assert!((vol("cube(10);") - 1000.0).abs() < 1e-6);
        assert!((vol("difference() { cube(10); cube(5); }") - 875.0).abs() < 1e-6); // 1000 − 125
        assert!((vol("union() { cube(10); cube(5); }") - 1000.0).abs() < 1e-6); // cube(5) ⊂ cube(10)
        assert!((vol("intersection() { cube(10); cube(5); }") - 125.0).abs() < 1e-6); // = cube(5)
        // hull() of a convex solid is the solid itself → cube(10) stays 1000; a single child hulls too.
        assert!((vol("hull() cube(10);") - 1000.0).abs() < 1e-6);
        // hull() of two separated cubes bridges them → a convex prism, strictly bigger than either.
        assert!(vol("hull() { cube(2); translate([10, 0, 0]) cube(2); }") > 8.0);
        // difference with a subtrahend moved fully clear removes nothing (transform composes into it):
        assert!(
            (vol("difference() { cube(10); translate([20, 0, 0]) cube(5); }") - 1000.0).abs()
                < 1e-6
        );
        // a transform preserves volume:
        assert!((vol("translate([100, 0, 0]) rotate([30, 20, 10]) cube(3);") - 27.0).abs() < 1e-6);
        // control flow (I.3.3): `if` picks a branch, `for` unions its iterations.
        assert!((vol("if (true) cube(2);") - 8.0).abs() < 1e-6);
        assert!((vol("if (false) cube(2);")).abs() < 1e-6); // empty
        assert!((vol("for (i = [0:2]) translate([i * 10, 0, 0]) cube(2);") - 24.0).abs() < 1e-6); // 3×8
    }
}
