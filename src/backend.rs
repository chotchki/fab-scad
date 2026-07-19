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

use std::path::Path;

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
    /// The backend's solid handle. `Clone` MUST be cheap (a handle/Rc copy, not a mesh copy) — the
    /// P.2 memo serves repeated subtrees by cloning the stored solid.
    type Solid: Clone;
    /// The backend's 2D-region handle (Manifold `CrossSection`).
    type Shape;

    /// A tessellated mesh (a fab-lang primitive) → a backend solid. An empty mesh → the empty solid.
    fn leaf(&self, mesh: &Mesh) -> Self::Solid;
    /// A served P.2-memo copy of `s` — a backend with provenance IDs (Manifold `mesh_id`/`same_face`)
    /// must RE-MINT them here so a cached copy is ID-distinct from its source, exactly like a fresh
    /// render (else copies coplanar-merge with each other in union trees — the silverwear class).
    /// Backends without instance IDs keep the default clone.
    fn fresh_instance(&self, s: &Self::Solid) -> Self::Solid {
        s.clone()
    }
    /// Boolean union.
    fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// N-ary union. The default is the pairwise fold; a real kernel overrides with its batch
    /// strategy (Manifold's `BatchBoolean` smallest-first heap — the fold is O(n²) on n heavy
    /// children, the M.7.3.2 outlet runaway).
    fn batch_union(&self, solids: Vec<Self::Solid>) -> Self::Solid {
        let mut it = solids.into_iter();
        match it.next() {
            Some(first) => it.fold(first, |acc, s| self.union(&acc, &s)),
            None => self.leaf(&Mesh::new()),
        }
    }
    /// Boolean difference (`a − b`).
    fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Boolean intersection.
    fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid;
    /// Convex hull of the operands COMBINED (`hull()`) — N-ary, not a pairwise fold. An empty list, or
    /// all-empty operands, → the empty solid.
    fn hull(&self, solids: &[Self::Solid]) -> Self::Solid;
    /// Minkowski sum of the operands (`minkowski()`) — an N-ary LEFT FOLD of the binary sum. Unlike hull,
    /// empty is an ANNIHILATOR: `A ⊕ ∅ = ∅`, so ANY empty operand (or an empty list) → the empty solid.
    fn minkowski(&self, solids: &[Self::Solid]) -> Self::Solid;
    /// An affine transform (OpenSCAD `multmatrix`, covering translate / rotate / scale / mirror).
    fn transform(&self, s: &Self::Solid, m: &Affine) -> Self::Solid;
    /// Set the solid's color (`color()`) — sets EVERY vertex, so outermost `color()` wins (J.2.9).
    fn color(&self, s: &Self::Solid, rgba: Rgba) -> Self::Solid;
    /// Extract the result as a triangle mesh (the empty solid → an empty mesh).
    fn to_mesh(&self, s: &Self::Solid) -> Mesh;
    /// Whether the solid is empty (no geometry) — the differential's `Empty` outcome.
    fn is_empty(&self, s: &Self::Solid) -> bool;
    /// Approximate triangle count — the X.1 persistent cache's LRU size proxy (cheap: a kernel with a
    /// native count overrides; the default extracts the mesh, so only pay it on a backend without one).
    fn approx_tris(&self, s: &Self::Solid) -> usize {
        self.to_mesh(s).tris.len()
    }
    /// The axis-aligned bounding box `(lo, hi)` of the solid, or `None` when it's empty. `resize()` needs
    /// the child's MEASURED extent to fix its scale factors — unlike a plain transform it can't fold to an
    /// `Affine` at tree-build time (L.5.1).
    fn bbox(&self, s: &Self::Solid) -> Option<(Vec3, Vec3)>;

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
    build_geo_gated(geo, backend, geo_cache_enabled(), None)
}

/// [`build_geo`] threading the X.1 PERSISTENT cross-render cache — the render arms' entry, so a live
/// customizer reuses subtrees unchanged since the last render instead of recomputing them.
pub fn build_geo_cached<B: GeometryBackend>(
    geo: &Geo,
    backend: &B,
    cache: &mut GeoCache<B::Solid>,
) -> B::Solid {
    build_geo_gated(geo, backend, geo_cache_enabled(), Some(cache))
}

/// The P.2 gate read once per build: on unless `FAB_GEO_CACHE=0`.
fn geo_cache_enabled() -> bool {
    std::env::var_os("FAB_GEO_CACHE").as_deref() != Some(std::ffi::OsStr::new("0"))
}

/// Bit-exact mesh compare for the verify mode (`PartialEq` would pass `-0.0 == 0.0`; the kernel's
/// symbolic-perturbation predicates read the BITS, so sign-blind equality is not render-identity).
fn meshes_bit_eq(a: &Mesh, b: &Mesh) -> bool {
    a.verts.len() == b.verts.len()
        && a.tris == b.tris
        && a.verts.iter().zip(&b.verts).all(|(p, q)| {
            p.x.to_bits() == q.x.to_bits()
                && p.y.to_bits() == q.y.to_bits()
                && p.z.to_bits() == q.z.to_bits()
        })
}

/// Debug hunt mode (`FAB_GEO_CACHE=verify`): every memo HIT also re-renders and byte-compares the
/// meshes — the first divergent serve panics with the node's identity. Slow; a diagnosis tool.
fn geo_cache_verify() -> bool {
    std::env::var_os("FAB_GEO_CACHE").as_deref() == Some(std::ffi::OsStr::new("verify"))
}

/// [`build_geo`] with the P.2 memo gate explicit — the A/B tests toggle it here instead of racing
/// the process-global env.
fn build_geo_gated<B: GeometryBackend>(
    geo: &Geo,
    backend: &B,
    cache: bool,
    persistent: Option<&mut GeoCache<B::Solid>>,
) -> B::Solid {
    // The redundancy probe owns its own wall clock, INSIDE its armed state — an unconditional
    // `Instant::now()` here panicked the wasm geom worker on every render (std::time is unsupported
    // on wasm32-unknown-unknown; the web-v0.13.0 boot gate caught it). Disarmed = no clock at all.
    crate::geo_redundancy::reset();
    let out = match geo {
        Geo::D3(node) => {
            let mut memo = GeoMemo::new(cache, persistent);
            memo.prepass(node);
            build_inner(node, backend, &mut memo)
        }
        Geo::D2(_) => backend.leaf(&Mesh::new()),
    };
    crate::geo_redundancy::report();
    out
}

/// Split a [`Geo`] result into its TOP-LEVEL PARTS — one backend solid per implicit-union child at
/// the root (T.2b). A model's top-level statements are implicitly unioned, so a `Geo::D3(Union(kids))`
/// root yields one part per top-level item (the `wall_sliced()` / `frame_sliced()` / … calls); each
/// part slices + orients + packs on its own, then all co-pack. Any other root — a lone statement, a
/// transform, a leaf, a 2D result — is ONE part.
///
/// LIMIT: the implicit top-level union and an EXPLICIT `union(){…}` written as the sole top-level
/// statement lower to the same `Union` node, so this splits BOTH — a wrap-everything-in-`union()`
/// model over-splits. Top-level module calls (the presliced legacy class) are the target; restructure
/// if the split is unwanted. Building a part per child (vs one merged solid) is the whole point: the
/// parts are then sliced/oriented independently, which a single `build_geo` merge would foreclose.
pub fn build_geo_parts<B: GeometryBackend>(geo: &Geo, backend: &B) -> Vec<B::Solid> {
    match geo {
        Geo::D3(GeoNode::Union(kids)) if kids.len() > 1 => {
            // ONE memo across every part: a sliced model shares its base subtree BETWEEN parts
            // (part = base ∩ half), which per-part memos would rebuild per part.
            let mut memo = GeoMemo::new(geo_cache_enabled(), None);
            for k in kids {
                memo.prepass(k);
            }
            kids.iter()
                .map(|k| build_inner(k, backend, &mut memo))
                .collect()
        }
        _ => vec![build_geo(geo, backend)],
    }
}

/// [`build_geo_parts`] threading the X.1 PERSISTENT cross-render cache (the render arms' per-part entry).
/// The single shared memo across parts already deduped WITHIN a render; the persistent cache extends that
/// reuse ACROSS renders, so a slider tick re-slices without recomputing every part's base geometry.
pub fn build_geo_parts_cached<B: GeometryBackend>(
    geo: &Geo,
    backend: &B,
    cache: &mut GeoCache<B::Solid>,
) -> Vec<B::Solid> {
    match geo {
        Geo::D3(GeoNode::Union(kids)) if kids.len() > 1 => {
            let mut memo = GeoMemo::new(geo_cache_enabled(), Some(cache));
            for k in kids {
                memo.prepass(k);
            }
            kids.iter()
                .map(|k| build_inner(k, backend, &mut memo))
                .collect()
        }
        _ => vec![build_geo_cached(geo, backend, cache)],
    }
}

/// Bind a `[[slicing.part]]` block's [`PartKey`](crate::manifest::PartKey) to a `build_geo_parts` part
/// index (U.3.14). `name` + `nth` is the primary key — it survives a reorder; `index` is the
/// authored-order fallback — it survives a name going anonymous (a part-count mismatch nulls EVERY
/// provenance name at once, so the fallback is all that's left). `names` MUST already carry that
/// count-match null. Returns `None` only when neither a matching name nor the index resolves — the GUI
/// warns + skips, the CLI bails (a silent mis-slice is worse than either).
pub fn resolve_part(names: &[Option<String>], key: &crate::manifest::PartKey) -> Option<usize> {
    if let Some(name) = &key.name {
        let mut nth = 0;
        for (i, n) in names.iter().enumerate() {
            if n.as_deref() == Some(name.as_str()) {
                if nth == key.nth {
                    return Some(i);
                }
                nth += 1;
            }
        }
    }
    (key.index < names.len()).then_some(key.index)
}

/// Builtin module names that WRAP a single geometry child (transforms, CSG ops, grouping) — the naming
/// walk descends past these to reach the user-level module that named the part (T.2b).
fn is_wrapper_module(name: &str) -> bool {
    matches!(
        name,
        "translate"
            | "rotate"
            | "scale"
            | "mirror"
            | "multmatrix"
            | "union"
            | "color"
            | "hull"
            | "minkowski"
            | "offset"
            | "render"
            | "let"
            | "for"
    )
}

/// The originating module/function NAME for each top-level part (T.2b provenance) — a STATIC walk of
/// the source AST, no evaluator involvement. Per geometry-bearing top-level statement (in source order)
/// it descends past single-child builtin wrappers (`translate() wall_sliced()` → "wall_sliced"; a bare
/// `cube()` → "cube"); a block / `if` at top level is unnamed (`None`). Callers apply these ONLY when
/// the count matches the actual `build_geo_parts` split — a wrong name is worse than a generic label,
/// and the alignment can drift on dropped-empties / `union(){…}` over-splits. Shared by the GUI's
/// `render_parts` and the CLI per-part slice ([`resolve_part`] binds a `[[slicing.part]]` block to one
/// of these by name+nth).
pub fn part_names(source: &Path) -> Vec<Option<String>> {
    match std::fs::read_to_string(source) {
        Ok(text) => part_names_of(&text),
        Err(_) => Vec::new(),
    }
}

/// The AST walk behind [`part_names`], on already-read source — the pure, IO-free core (so its test
/// runs under miri, which can't touch the filesystem).
fn part_names_of(text: &str) -> Vec<Option<String>> {
    let Ok(prog) = fab_lang::parse(text) else {
        return Vec::new();
    };
    prog.stmts
        .iter()
        .filter_map(|s| match &s.kind {
            // A module call: descend single-child wrappers to the first non-wrapper name.
            fab_lang::StmtKind::Module(mi) => {
                let mut cur = mi;
                while is_wrapper_module(&cur.name) && cur.children.len() == 1 {
                    match &cur.children[0].kind {
                        fab_lang::StmtKind::Module(inner) => cur = inner,
                        _ => break,
                    }
                }
                Some(Some(cur.name.clone()))
            }
            // A bare block / `if` produces geometry but has no single name.
            fab_lang::StmtKind::Block(_) | fab_lang::StmtKind::If { .. } => Some(None),
            // Assignments, module/function defs, use/include, empty — no geometry.
            _ => None,
        })
        .collect()
}

// ─────────────────────────── P.2: the kernel-level Solid memo ──────────────────────────────────
//
// The BU.7 probe measured the tree the backend receives: with the evaluator's CSG cache ON, every
// memo hit splices a deep `Geo` clone, so the kernel re-renders identical content constantly —
// slice_parts spent 95% of its build on subtrees already rendered (53,382 nodes, 367 distinct).
// This memo is the fix: a per-build content-addressed `Solid` store. A PREPASS counts every
// subtree hash, so only subtrees that WILL recur are retained (a singleton renders plain, no clone,
// no memory); entries evict when their last expected use is served. Every hit is verified by a deep
// `PartialEq` compare against the stored node — a 64-bit hash collision can cost a re-render,
// never a wrong mesh. Bit-identity is by construction: the kernel is deterministic, so the clone a
// hit returns is byte-identical to what a re-render would produce (the models differential + the
// bitwise STL A/B are the standing gates). `FAB_GEO_CACHE=0` opts out.
//
// KNOWN SLACK, accepted: when a PARENT subtree hits, its children never re-visit, so their
// remaining-use counts stay high and their entries live to end-of-build — memory only, bounded by
// the model's distinct shared content.

use std::collections::{BTreeMap, HashMap};

// ─────────────────────────── X.1: the PERSISTENT cross-render Solid cache ──────────────────────────
//
// GeoMemo (above) dedupes WITHIN one build and dies with it, so a live customizer re-renders the whole
// model on every slider tick even though one param moved. GeoCache lives on the geom worker's SolidStore
// (one per execution context — native shard thread / wasm Worker, both !Send-local), so it SURVIVES
// across renders: an unchanged subtree serves instead of recomputing. Keyed by the 128-bit content hash
// (no deep-eq — a cross-build cache can't hold a borrowed &GeoNode verifier and an owned clone would
// duplicate every Leaf mesh); `fresh_instance` re-mints provenance on every serve; LRU-bounded by an
// approximate mesh-byte budget. Layered BEHIND GeoMemo and consulted only for EXPENSIVE ops.

/// ~bytes per triangle for the LRU budget (3 verts × 3 f64 + an index triple, rounded up) — a proxy,
/// not exact; the cache bounds RETAINED work, not RAM to the byte.
const CACHE_BYTES_PER_TRI: usize = 48;
/// Default cache budget: 128 MiB of approximate mesh bytes. Conservative enough for the wasm heap
/// ceiling; native could hold more, but one bound keeps both platforms identical (X.1.2 may tune).
const CACHE_CAP_BYTES: usize = 128 << 20;

struct GeoCacheEntry<S> {
    solid: S,
    bytes: usize,
    /// Monotonic access stamp — the LRU victim is the smallest.
    used: u64,
}

/// The persistent content-addressed Solid cache (X.1). `enabled` mirrors `FAB_GEO_CACHE`.
pub struct GeoCache<S> {
    map: HashMap<u128, GeoCacheEntry<S>>,
    bytes: usize,
    cap_bytes: usize,
    tick: u64,
    enabled: bool,
    hits: u64,
    misses: u64,
    stores: u64,
}

impl<S: Clone> GeoCache<S> {
    /// A cache with the default budget, gated on `FAB_GEO_CACHE` like the per-build memo.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            bytes: 0,
            cap_bytes: CACHE_CAP_BYTES,
            tick: 0,
            enabled: geo_cache_enabled(),
            hits: 0,
            misses: 0,
            stores: 0,
        }
    }

    /// `(hits, misses, stores, entries, bytes)` — the `[csg-cache]` readout + the tests' reuse gate.
    pub fn stats(&self) -> (u64, u64, u64, usize, usize) {
        (
            self.hits,
            self.misses,
            self.stores,
            self.map.len(),
            self.bytes,
        )
    }

    /// Serve a cached solid by content hash, refreshing its LRU stamp. `None` on a miss.
    fn get(&mut self, h: u128) -> Option<&S> {
        self.tick += 1;
        let t = self.tick;
        if let Some(e) = self.map.get_mut(&h) {
            e.used = t;
            self.hits += 1;
            Some(&e.solid)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Store a rendered solid, then evict least-recently-used entries until under the byte budget. A
    /// re-store of a live key is a no-op (the first render already banked it).
    fn insert(&mut self, h: u128, solid: S, tris: usize) {
        if self.map.contains_key(&h) {
            return;
        }
        let bytes = tris.saturating_mul(CACHE_BYTES_PER_TRI);
        self.tick += 1;
        let used = self.tick;
        self.map.insert(h, GeoCacheEntry { solid, bytes, used });
        self.bytes += bytes;
        self.stores += 1;
        while self.bytes > self.cap_bytes && self.map.len() > 1 {
            let victim = self.map.iter().min_by_key(|(_, e)| e.used).map(|(&k, _)| k);
            match victim {
                Some(k) => {
                    if let Some(e) = self.map.remove(&k) {
                        self.bytes -= e.bytes;
                    }
                }
                None => break,
            }
        }
    }
}

impl<S: Clone> Default for GeoCache<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Which nodes the persistent cache retains — the EXPENSIVE ops (real kernel work). Cheap wrappers
/// (Transform/Color) and leaves aren't cached: their child (if expensive) already is, and caching a
/// Leaf just duplicates a mesh we'd rebuild in microseconds.
fn cacheable(node: &GeoNode) -> bool {
    matches!(
        node,
        GeoNode::Union(_)
            | GeoNode::Difference(_)
            | GeoNode::Intersection(_)
            | GeoNode::Hull(_)
            | GeoNode::Minkowski(_)
            | GeoNode::Extrude { .. }
            | GeoNode::Resize { .. }
    )
}

struct GeoMemo<'t, 'c, B: GeometryBackend> {
    /// Subtree hash → REMAINING expected builds (prepass count, decremented per visit; entry + any
    /// stored solids evict at 0).
    counts: BTreeMap<u64, u32>,
    /// Subtree hash → rendered entries awaiting reuse (the node ref backs the deep-eq hit check).
    ready: BTreeMap<u64, Vec<(&'t GeoNode, B::Solid)>>,
    /// Node address → subtree hash (shared with the prepass; O(tree) hashing total).
    hashes: BTreeMap<usize, u64>,
    enabled: bool,
    /// X.1: node address → 128-bit hash memo for the persistent cache (per build, like `hashes`).
    hashes128: BTreeMap<usize, u128>,
    /// X.1: the persistent cross-render cache, threaded in from the SolidStore. `None` on the plain
    /// per-build path (slicing, tests) — then this behaves exactly like the P.2-only memo.
    persistent: Option<&'c mut GeoCache<B::Solid>>,
}

impl<'t, 'c, B: GeometryBackend> GeoMemo<'t, 'c, B> {
    fn new(enabled: bool, persistent: Option<&'c mut GeoCache<B::Solid>>) -> Self {
        Self {
            counts: BTreeMap::new(),
            ready: BTreeMap::new(),
            hashes: BTreeMap::new(),
            enabled,
            hashes128: BTreeMap::new(),
            persistent,
        }
    }

    /// Count every subtree hash under `node` — the reuse forecast the store/evict decisions key on.
    fn prepass(&mut self, node: &'t GeoNode) {
        if !self.enabled {
            return;
        }
        let h = crate::geo_hash::hash_node(node, &mut self.hashes);
        *self.counts.entry(h).or_insert(0) += 1;
        match node {
            GeoNode::Empty | GeoNode::Leaf(_) | GeoNode::Extrude { .. } => {}
            GeoNode::Transform { child, .. }
            | GeoNode::Color { child, .. }
            | GeoNode::Resize { child, .. } => self.prepass(child),
            GeoNode::Union(kids)
            | GeoNode::Difference(kids)
            | GeoNode::Intersection(kids)
            | GeoNode::Hull(kids)
            | GeoNode::Minkowski(kids) => {
                for k in kids {
                    self.prepass(k);
                }
            }
        }
    }
}

/// Lower a fab-lang CSG tree ([`GeoNode`], J.2) to a backend solid — the geometry lowering. This is
/// the integration seam: fab-lang builds the backend-agnostic tree, the backend does the real CSG.
/// Recursion is bounded by the tree depth (the parser's `MAX_DEPTH`), so it can't overflow the stack.
pub fn build<B: GeometryBackend>(node: &GeoNode, backend: &B) -> B::Solid {
    let mut memo = GeoMemo::new(geo_cache_enabled(), None);
    memo.prepass(node);
    build_inner(node, backend, &mut memo)
}

/// Lower a node: the X.1 persistent cross-render cache first (a subtree unchanged since the last render
/// serves instead of recomputing), then the per-build P.2 memo. Children recurse back through here, so
/// every expensive subtree gets both layers.
fn build_inner<'t, 'c, B: GeometryBackend>(
    node: &'t GeoNode,
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
) -> B::Solid {
    if let Some(out) = serve_persistent(node, backend, memo) {
        return out;
    }
    let out = build_inner_memo(node, backend, memo);
    store_persistent(node, &out, backend, memo);
    out
}

/// X.1: serve `node` from the persistent cache (expensive ops only), re-minting provenance. `None` on a
/// miss, a cheap node, a disabled/absent cache. `FAB_GEO_CACHE=verify` re-renders uncached and bitwise-
/// compares the served solid — the deterministic gate that a 128-bit hit is really the same geometry.
fn serve_persistent<'t, 'c, B: GeometryBackend>(
    node: &'t GeoNode,
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
) -> Option<B::Solid> {
    if !cacheable(node) || !memo.persistent.as_ref().is_some_and(|c| c.enabled) {
        return None;
    }
    let h = crate::geo_hash::hash_node_128(node, &mut memo.hashes128);
    let out = backend.fresh_instance(memo.persistent.as_deref_mut()?.get(h)?);
    if geo_cache_verify() {
        let fresh = {
            let mut scratch = GeoMemo::new(false, None);
            render_node(node, backend, &mut scratch)
        };
        assert!(
            meshes_bit_eq(&backend.to_mesh(&out), &backend.to_mesh(&fresh)),
            "FAB_GEO_CACHE=verify: X.1 cache serve != uncached fresh render (hash {h:#034x})"
        );
    }
    Some(out)
}

/// X.1: store a freshly-rendered expensive subtree into the persistent cache (a no-op for cheap nodes /
/// disabled cache; a re-store of a live key is ignored inside `insert`).
fn store_persistent<'t, 'c, B: GeometryBackend>(
    node: &'t GeoNode,
    out: &B::Solid,
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
) {
    if !cacheable(node) || !memo.persistent.as_ref().is_some_and(|c| c.enabled) {
        return;
    }
    let h = crate::geo_hash::hash_node_128(node, &mut memo.hashes128);
    let tris = backend.approx_tris(out);
    if let Some(pc) = memo.persistent.as_deref_mut() {
        pc.insert(h, out.clone(), tris);
    }
}

/// The memoizing recursion behind [`build`]: serve a repeated subtree from the P.2 memo (deep-eq
/// verified), render + store a first-of-many, render plain a singleton.
fn build_inner_memo<'t, 'c, B: GeometryBackend>(
    node: &'t GeoNode,
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
) -> B::Solid {
    if memo.enabled {
        let h = crate::geo_hash::hash_node(node, &mut memo.hashes);
        if let Some(rem) = memo.counts.get_mut(&h) {
            *rem = rem.saturating_sub(1);
            let more_coming = *rem > 0;
            if let Some(entries) = memo.ready.get(&h)
                && let Some((stored, solid)) = entries.iter().find(|(n, _)| *n == node)
            {
                let out = backend.fresh_instance(solid);
                if geo_cache_verify() {
                    let stored: &GeoNode = stored;
                    assert!(
                        std::ptr::eq(stored, node) || stored == node,
                        "verify: eq drifted mid-build"
                    );
                    // Fresh = a fully UNCACHED render in a throwaway memo (no bookkeeping
                    // perturbation, no circular serve reuse); compared BITWISE (PartialEq is
                    // sign-of-zero-blind, the kernel's shadow predicates are not).
                    let fresh = {
                        let mut scratch = GeoMemo::new(false, None);
                        render_node(node, backend, &mut scratch)
                    };
                    assert!(
                        meshes_bit_eq(&backend.to_mesh(&out), &backend.to_mesh(&fresh)),
                        "FAB_GEO_CACHE=verify: served solid != UNCACHED fresh render, bitwise (hash {h:#018x})"
                    );
                }
                if !more_coming {
                    memo.ready.remove(&h);
                    memo.counts.remove(&h);
                }
                return out;
            }
            let out = render_node(node, backend, memo);
            if more_coming {
                memo.ready.entry(h).or_default().push((node, out.clone()));
            }
            return out;
        }
    }
    render_node(node, backend, memo)
}

/// One node's actual backend lowering (the pre-P.2 `build` body); children recurse through the memo.
fn render_node<'t, 'c, B: GeometryBackend>(
    node: &'t GeoNode,
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
) -> B::Solid {
    // BU.7 probe (no-op unless FAB_GEO_REDUNDANCY=1): subtree hash + inclusive render time. Sits on
    // the RENDER path, so with the memo live it reports the RESIDUAL waste the memo didn't catch.
    let _probe = crate::geo_redundancy::enter(node);
    match node {
        GeoNode::Empty => backend.leaf(&Mesh::new()),
        GeoNode::Leaf(mesh) => backend.leaf(mesh),
        GeoNode::Transform { matrix, child } => {
            backend.transform(&build_inner(child, backend, memo), matrix)
        }
        // Union is N-ary via the backend's batch strategy; difference = first − union(rest) — the
        // same set (A−B−C ≡ A−(B∪C)) and the same shape the C++ csg tree evaluates, so one big
        // subtract replaces a quadratic fold over the accumulating base.
        GeoNode::Union(kids) => {
            backend.batch_union(kids.iter().map(|k| build_inner(k, backend, memo)).collect())
        }
        GeoNode::Difference(kids) => match kids.split_first() {
            None => backend.leaf(&Mesh::new()),
            Some((first, rest)) => {
                let base = build_inner(first, backend, memo);
                if rest.is_empty() {
                    base
                } else {
                    let cutter = backend
                        .batch_union(rest.iter().map(|k| build_inner(k, backend, memo)).collect());
                    backend.difference(&base, &cutter)
                }
            }
        },
        GeoNode::Intersection(kids) => reduce(kids, backend, memo, |b, x, y| b.intersection(x, y)),
        // hull is N-ary — the backend hulls the whole operand set at once (not a pairwise fold).
        GeoNode::Hull(kids) => backend.hull(
            &kids
                .iter()
                .map(|k| build_inner(k, backend, memo))
                .collect::<Vec<_>>(),
        ),
        // minkowski is an N-ary fold of the binary sum (J.4.4); the backend owns the empty-annihilator rule.
        GeoNode::Minkowski(kids) => backend.minkowski(
            &kids
                .iter()
                .map(|k| build_inner(k, backend, memo))
                .collect::<Vec<_>>(),
        ),
        // The 2D→3D bridge: lower the 2D child to a Shape, then sweep it into a Solid (J.3.4/J.3.5).
        GeoNode::Extrude { kind, child } => backend.extrude(&build_2d(child, backend), kind),
        // Color sets EVERY vertex of the child subtree (J.2.9). Outermost `color()` wins because the
        // enclosing node's color op overwrites any inner one; distinct colors survive a union.
        GeoNode::Color { color, child } => {
            backend.color(&build_inner(child, backend, memo), *color)
        }
        // resize() — build the child, measure its bbox, then pure-scale (about the origin, like OpenSCAD's
        // multmatrix) so each axis hits `newsize`. An empty child has no bbox → nothing to scale (L.5.1).
        GeoNode::Resize {
            newsize,
            auto,
            child,
        } => {
            let built = build_inner(child, backend, memo);
            match backend.bbox(&built) {
                Some((lo, hi)) => {
                    let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                    let s = resize_scale(*newsize, *auto, ext);
                    backend.transform(
                        &built,
                        &Affine::row_major([
                            s[0], 0.0, 0.0, 0.0, //
                            0.0, s[1], 0.0, 0.0, //
                            0.0, 0.0, s[2], 0.0,
                        ]),
                    )
                }
                None => built,
            }
        }
    }
}

/// `resize()`'s per-axis scale factors from the child's measured extent. A `newsize` axis of `0` (or an axis
/// the child is FLAT in, extent `0`) is kept at `1` — UNLESS `auto` is set for a `0`-newsize axis, which then
/// scales PROPORTIONALLY to the first axis that DID get a size (OpenSCAD's `Resize` autosize). All-zero
/// `newsize` → identity.
fn resize_scale(newsize: [f64; 3], auto: [bool; 3], ext: [f64; 3]) -> [f64; 3] {
    let mut scale = [1.0_f64; 3];
    let mut first_sized: Option<f64> = None;
    for i in 0..3 {
        if newsize[i] > 0.0 && ext[i] > 0.0 {
            scale[i] = newsize[i] / ext[i];
            if first_sized.is_none() {
                first_sized = Some(scale[i]);
            }
        }
    }
    if let Some(f) = first_sized {
        for i in 0..3 {
            if newsize[i] == 0.0 && auto[i] && ext[i] > 0.0 {
                scale[i] = f;
            }
        }
    }
    scale
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
        // `color()` on 2D carries the color on the tree only — the 2D kernel (CrossSection) has no color
        // property, so the geometry passes straight through (the GUI reads the color off the `Shape2D` node).
        Shape2D::Color { child, .. } => build_2d(child, backend),
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
fn reduce<'t, 'c, B: GeometryBackend>(
    kids: &'t [GeoNode],
    backend: &B,
    memo: &mut GeoMemo<'t, 'c, B>,
    combine: impl Fn(&B, &B::Solid, &B::Solid) -> B::Solid,
) -> B::Solid {
    let mut kids = kids.iter();
    match kids.next() {
        Some(first) => {
            let first = build_inner(first, backend, memo);
            kids.fold(first, |acc, k| {
                let s = build_inner(k, backend, memo);
                combine(backend, &acc, &s)
            })
        }
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

    fn fresh_instance(&self, s: &Self::Solid) -> Self::Solid {
        s.as_ref().map(crate::kernel::Solid::as_fresh_instance)
    }

    fn bbox(&self, s: &Self::Solid) -> Option<(Vec3, Vec3)> {
        s.as_ref().and_then(crate::kernel::Solid::bbox)
    }

    fn leaf(&self, mesh: &Mesh) -> Self::Solid {
        // `from_indexed` rejects an empty mesh (→ None); a non-manifold mesh also → None (polyhedron
        // validation tightens at J.2 — for now the lowering feeds valid tessellations).
        crate::kernel::Solid::from_indexed(&mesh.verts, &mesh.tris).ok()
    }

    fn batch_union(&self, solids: Vec<Self::Solid>) -> Self::Solid {
        // `None` children are empty geometry — the union identity, dropped.
        let live: Vec<crate::kernel::Solid> = solids.into_iter().flatten().collect();
        if live.is_empty() {
            return None;
        }
        Some(crate::kernel::Solid::batch_union(&live))
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

    fn minkowski(&self, solids: &[Self::Solid]) -> Self::Solid {
        // ANY empty operand annihilates (`A ⊕ ∅ = ∅`), so bail to ∅ on an empty list or any `None`. Else
        // left-fold the native `minkowski_sum` — one operand → itself, matching OpenSCAD's N-ary fold.
        if solids.is_empty() || solids.iter().any(Option::is_none) {
            return None;
        }
        let mut present = solids.iter().flatten().cloned();
        let first = present.next()?;
        Some(present.fold(first, |acc, s| acc.minkowski_sum(&s)))
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

    /// X.1 LRU size proxy — the kernel's own triangle count, no mesh extraction.
    fn approx_tris(&self, s: &Self::Solid) -> usize {
        s.as_ref().map_or(0, super::kernel::Solid::num_tri)
    }

    fn color(&self, s: &Self::Solid, rgba: Rgba) -> Self::Solid {
        s.as_ref().map(|s| s.with_color(rgba))
    }

    fn is_empty(&self, s: &Self::Solid) -> bool {
        s.as_ref().is_none_or(crate::kernel::Solid::is_empty)
    }

    // ── 2D surface (J.3) — delegates to kernel::Section (Manifold CrossSection) ───────────────────

    fn leaf_2d(&self, contours: &[Vec<[f64; 2]>]) -> Self::Shape {
        // `polygon()` primitives fill by even-odd nesting (OpenSCAD), not winding — so a clockwise
        // BOSL2 path (`star`/`hexagon`) fills instead of vanishing under the default `Positive` rule.
        crate::kernel::Section::polygon(contours)
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

    fn bbox(&self, s: &Self::Solid) -> Option<(Vec3, Vec3)> {
        let first = *s.mesh.verts.first()?;
        Some(s.mesh.verts.iter().fold((first, first), |(lo, hi), v| {
            (
                Vec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z)),
                Vec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z)),
            )
        }))
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

    fn minkowski(&self, solids: &[Self::Solid]) -> Self::Solid {
        // Mock can't sum geometry — but it honors the ANNIHILATOR algebra (any empty → empty) and appends
        // the meshes + bumps ops, so the interface suite walks the dispatch under miri. Real op is Manifold.
        if solids.is_empty() || solids.iter().any(MockSolid::is_empty) {
            return MockSolid {
                mesh: Mesh::new(),
                ops: solids.iter().map(|s| s.ops).sum::<u32>() + 1,
            };
        }
        let mesh = solids
            .iter()
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
    use super::{GeometryBackend, MockBackend, build_geo_parts, part_names_of};

    /// P.2 — the memo must (a) render a repeated subtree ONCE (the counting delegate proves the
    /// skip) and (b) change NOTHING about the output. The duplicated children are structurally
    /// identical but DISTINCT allocations — the deep-clone shape the evaluator's CSG cache splices
    /// into real trees. Gate toggled via `build_geo_gated`, not the process-global env.
    #[test]
    fn p2_memo_renders_repeated_subtrees_once_with_identical_output() {
        use std::cell::Cell;

        struct Counting<'a> {
            inner: MockBackend,
            leaves: &'a Cell<u32>,
        }
        impl GeometryBackend for Counting<'_> {
            type Solid = super::MockSolid;
            type Shape = super::MockShape;
            fn leaf(&self, mesh: &fab_lang::Mesh) -> Self::Solid {
                self.leaves.set(self.leaves.get() + 1);
                self.inner.leaf(mesh)
            }
            fn union(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
                self.inner.union(a, b)
            }
            fn difference(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
                self.inner.difference(a, b)
            }
            fn intersection(&self, a: &Self::Solid, b: &Self::Solid) -> Self::Solid {
                self.inner.intersection(a, b)
            }
            fn hull(&self, solids: &[Self::Solid]) -> Self::Solid {
                self.inner.hull(solids)
            }
            fn minkowski(&self, solids: &[Self::Solid]) -> Self::Solid {
                self.inner.minkowski(solids)
            }
            fn transform(&self, s: &Self::Solid, m: &fab_lang::Affine) -> Self::Solid {
                self.inner.transform(s, m)
            }
            fn bbox(&self, s: &Self::Solid) -> Option<(fab_lang::Vec3, fab_lang::Vec3)> {
                self.inner.bbox(s)
            }
            fn color(&self, s: &Self::Solid, rgba: fab_lang::Rgba) -> Self::Solid {
                self.inner.color(s, rgba)
            }
            fn to_mesh(&self, s: &Self::Solid) -> fab_lang::Mesh {
                self.inner.to_mesh(s)
            }
            fn is_empty(&self, s: &Self::Solid) -> bool {
                self.inner.is_empty(s)
            }
            fn leaf_2d(&self, contours: &[Vec<[f64; 2]>]) -> Self::Shape {
                self.inner.leaf_2d(contours)
            }
            fn union_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
                self.inner.union_2d(a, b)
            }
            fn difference_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
                self.inner.difference_2d(a, b)
            }
            fn intersection_2d(&self, a: &Self::Shape, b: &Self::Shape) -> Self::Shape {
                self.inner.intersection_2d(a, b)
            }
            fn offset_2d(
                &self,
                s: &Self::Shape,
                delta: f64,
                join: fab_lang::Join2D,
                segments: u32,
            ) -> Self::Shape {
                self.inner.offset_2d(s, delta, join, segments)
            }
            fn transform_2d(&self, s: &Self::Shape, m: &fab_lang::Affine2) -> Self::Shape {
                self.inner.transform_2d(s, m)
            }
            fn extrude(&self, s: &Self::Shape, kind: &fab_lang::ExtrudeKind) -> Self::Solid {
                self.inner.extrude(s, kind)
            }
            fn projection(&self, s: &Self::Solid, cut: bool) -> Self::Shape {
                self.inner.projection(s, cut)
            }
            fn to_polygons(&self, s: &Self::Shape) -> Vec<Vec<[f64; 2]>> {
                self.inner.to_polygons(s)
            }
            fn is_empty_2d(&self, s: &Self::Shape) -> bool {
                self.inner.is_empty_2d(s)
            }
        }

        // Two identical-content children (distinct allocations) + one singleton.
        let geo = fab_lang::evaluate_geometry(
            "module leaf(){ sphere(4, $fn=12); } leaf(); translate([9,0,0]) leaf(); cube(1);",
        )
        .expect("evaluates");
        let count = |cache: bool| {
            let leaves = Cell::new(0);
            let b = Counting {
                inner: MockBackend,
                leaves: &leaves,
            };
            let solid = super::build_geo_gated(&geo, &b, cache, None);
            (leaves.get(), solid.mesh)
        };
        let (leaves_off, mesh_off) = count(false);
        let (leaves_on, mesh_on) = count(true);
        assert!(
            leaves_on < leaves_off,
            "the repeated leaf must render once with the memo on ({leaves_on} vs {leaves_off})"
        );
        assert_eq!(
            mesh_on, mesh_off,
            "the memo must not change the output mesh"
        );
    }

    /// X.1: the persistent cache must not change the output — cache-on == cache-off, byte for byte.
    #[test]
    fn x1_persistent_cache_matches_uncached() {
        // A repeated EXPENSIVE subtree (a difference, not a bare leaf) — the cacheable regime.
        let geo = fab_lang::evaluate_geometry(
            "module w(){ difference(){ cube(10, center=true); sphere(6, $fn=16); } }\n\
             w(); translate([20,0,0]) w();",
        )
        .expect("evaluates");
        let off = super::build_geo(&geo, &MockBackend);
        let mut cache = super::GeoCache::new();
        let on = super::build_geo_cached(&geo, &MockBackend, &mut cache);
        assert_eq!(
            on.mesh, off.mesh,
            "X.1 cache must not alter the output mesh"
        );
    }

    /// X.1: a second render on the SAME cache reuses the first render's subtrees (the cross-render win).
    #[test]
    fn x1_persistent_cache_reuses_across_renders() {
        let geo = fab_lang::evaluate_geometry(
            "module w(){ difference(){ cube(10, center=true); sphere(6, $fn=16); } }\n\
             w(); translate([20,0,0]) w();",
        )
        .expect("evaluates");
        let mut cache = super::GeoCache::new();
        let cold = super::build_geo_cached(&geo, &MockBackend, &mut cache);
        let (hits_cold, _, _, _, _) = cache.stats();
        // A byte-identical re-render (a slider tick that changed nothing) must serve entirely from cache.
        let warm = super::build_geo_cached(&geo, &MockBackend, &mut cache);
        let (hits_warm, _, _, _, _) = cache.stats();
        assert!(
            hits_warm > hits_cold,
            "the warm re-render must hit the cache more than the cold one ({hits_warm} vs {hits_cold})"
        );
        assert_eq!(
            cold.mesh, warm.mesh,
            "warm re-render must match the cold one"
        );
    }

    #[test]
    fn part_names_descend_wrappers_and_flag_anonymous() {
        // a module DEF (no geometry), a wrapped call, a bare primitive, an anonymous `if` block.
        let src = "module wall() { cube(1); }\ntranslate([0,0,0]) wall();\ncube(2);\nif (true) { sphere(1); }\n";
        assert_eq!(
            part_names_of(src),
            vec![Some("wall".to_string()), Some("cube".to_string()), None]
        );
    }

    #[test]
    fn build_geo_parts_splits_top_level_items() {
        // Two top-level statements are implicitly unioned → two independent parts (T.2b keystone).
        let geo = fab_lang::evaluate_geometry("cube(10); translate([50,0,0]) sphere(6,$fn=16);")
            .expect("evaluates");
        let parts = build_geo_parts(&geo, &MockBackend);
        assert_eq!(parts.len(), 2, "two top-level items → two parts");
        for p in &parts {
            assert!(!MockBackend.is_empty(p), "each part carries geometry");
        }
        // A lone top-level item is ONE part — not split into its internal pieces.
        let one = fab_lang::evaluate_geometry("cube(10);").expect("evaluates");
        assert_eq!(
            build_geo_parts(&one, &MockBackend).len(),
            1,
            "single item → one part"
        );
    }

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
        // text() (J.4.3, bundled Liberation Sans): a glyph has real positive area…
        assert!(area("text(\"L\", size = 10);") > 5.0);
        // …empty text is a present-but-empty 2D leaf (no area)…
        assert!(area("text(\"\", size = 10);").abs() < 1e-9);
        // …two glyphs cover more than one (advance + a second outline)…
        assert!(area("text(\"LL\", size = 10);") > area("text(\"L\", size = 10);") + 1.0);
        // …and a glyph with a HOLE ('O') fills LESS than its bounding box — the even-odd rule cut the
        // counter out (a solid box of the same extent would be much larger), proving holes resolve.
        let o = area("text(\"O\", size = 10);");
        assert!(
            o > 5.0 && o < 60.0,
            "O area {o} should be a ring, not a filled box"
        );
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
        // minkowski() (J.4.4, native Manifold sum). A single child folds to itself.
        assert!((vol("minkowski() cube(10);") - 1000.0).abs() < 1e-6);
        // Two AXIS-ALIGNED boxes: [0,10]³ ⊕ [0,2]³ = [0,12]³ EXACTLY → 12³ = 1728 (oracle-free, deterministic).
        assert!((vol("minkowski() { cube(10); cube(2); }") - 1728.0).abs() < 1e-6);
        // The dominant use — rounding a cube by a sphere probe GROWS it past the bare cube (topology differs
        // from CGAL, so this is a shape check, not an exact-volume one; the residual harness owns exactness).
        assert!(vol("minkowski() { cube(10); sphere(r = 1, $fn = 16); }") > 1000.0);
    }
}
