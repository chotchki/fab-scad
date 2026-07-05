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

use fab_lang::{GeoNode, Mesh};

/// A geometry backend: tessellated meshes → solids, combined via CSG + affine transforms. `Solid` is
/// the backend's opaque handle (real Manifold's is `!Send`; the mock's is inert data).
pub trait GeometryBackend {
    /// The backend's solid handle.
    type Solid;

    /// A tessellated mesh (a fab-lang primitive) → a backend solid. An empty mesh → the empty solid.
    fn leaf(&self, mesh: &Mesh) -> Self::Solid;
    /// Boolean union.
    fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Boolean difference (`a − b`).
    fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Boolean intersection.
    fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// An affine transform — a 3×4 row-major matrix (OpenSCAD `multmatrix`, covering
    /// translate / rotate / scale / mirror).
    fn transform(&self, s: &Self::Solid, m: &[f64; 12]) -> Self::Solid;
    /// Extract the result as a triangle mesh (the empty solid → an empty mesh).
    fn to_mesh(&self, s: &Self::Solid) -> Mesh;
    /// Whether the solid is empty (no geometry) — the differential's `Empty` outcome.
    fn is_empty(&self, s: &Self::Solid) -> bool;
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

/// Apply a 3×4 row-major affine `m` to a vertex.
fn affine(m: &[f64; 12], v: [f64; 3]) -> [f64; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2] + m[3],
        m[4] * v[0] + m[5] * v[1] + m[6] * v[2] + m[7],
        m[8] * v[0] + m[9] * v[1] + m[10] * v[2] + m[11],
    ]
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

    fn transform(&self, s: &Self::Solid, m: &[f64; 12]) -> Self::Solid {
        // geo.rs affines are 3x4 ROW-MAJOR (OpenSCAD `multmatrix` + the mock's convention); Manifold's
        // `transform` wants COLUMN-MAJOR (kernel.rs) — transpose the 3x4 (col c, row r) → (r, c).
        let cm = [
            m[0], m[4], m[8], m[1], m[5], m[9], m[2], m[6], m[10], m[3], m[7], m[11],
        ];
        s.as_ref().map(|s| s.transform(&cm))
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

    fn is_empty(&self, s: &Self::Solid) -> bool {
        s.as_ref().is_none_or(crate::kernel::Solid::is_empty)
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
        [
            t[0].saturating_add(offset),
            t[1].saturating_add(offset),
            t[2].saturating_add(offset),
        ]
    }));
    Mesh { verts, tris }
}

/// The mock geometry backend — the miri-tested path.
pub struct MockBackend;

impl GeometryBackend for MockBackend {
    type Solid = MockSolid;

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

    fn transform(&self, s: &Self::Solid, m: &[f64; 12]) -> Self::Solid {
        let verts = s.mesh.verts.iter().map(|&v| affine(m, v)).collect();
        MockSolid {
            mesh: Mesh {
                verts,
                tris: s.mesh.tris.clone(),
            },
            ops: s.ops + 1,
        }
    }

    fn to_mesh(&self, s: &Self::Solid) -> Mesh {
        s.mesh.clone()
    }

    fn is_empty(&self, s: &Self::Solid) -> bool {
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
            &[1.0, 0.0, 0.0, 5.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        );
        for s in [&u, &d, &i, &moved] {
            assert!(!b.is_empty(s));
            let _ = b.to_mesh(s).tri_count(); // extract path exercised
        }

        // The empty algebra — must hold identically on both backends.
        let empty = b.leaf(&fab_lang::Mesh::new());
        assert!(b.is_empty(&empty));
        assert!(b.is_empty(&b.intersection(&cube, &empty))); // x ∩ ∅ = ∅
        assert!(b.is_empty(&b.difference(&empty, &cube))); // ∅ − x = ∅
        assert!(!b.is_empty(&b.union(&cube, &empty))); // x ∪ ∅ = x
        assert!(!b.is_empty(&b.difference(&cube, &empty))); // x − ∅ = x
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

    // J.2.3/J.2.7 — the tree-walker lowers CSG booleans + transforms through the REAL Manifold backend
    // correctly, checked by exact VOLUME (no oracle re-import, which the harness can't do for boolean
    // meshes yet). cube(5) sits inside cube(10)'s corner (both [0,size]³), so the results are exact.
    #[cfg(feature = "kernel")]
    #[test]
    fn boolean_and_transform_lowering_volumes() {
        let vol = |scad: &str| -> f64 {
            match super::build(
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
        // difference with a subtrahend moved fully clear removes nothing (transform composes into it):
        assert!(
            (vol("difference() { cube(10); translate([20, 0, 0]) cube(5); }") - 1000.0).abs()
                < 1e-6
        );
        // a transform preserves volume:
        assert!((vol("translate([100, 0, 0]) rotate([30, 20, 10]) cube(3);") - 27.0).abs() < 1e-6);
    }
}
