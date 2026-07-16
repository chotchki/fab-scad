//! BU.7 — the GeoNode→Solid redundancy probe: how much KERNEL wall goes to re-rendering subtrees
//! that already rendered this build? That number is the ceiling of the P.2 kernel-level cache
//! (per-`GeoNode` content-addressed `Solid` memo) — layer (c) of the BU.7 decomposition, measured
//! ABOVE the (now default-on) evaluator-level CSG memo, which already collapses module-call
//! redundancy before the tree reaches the backend. Whatever this probe still finds is redundancy
//! only the kernel layer (or a CSG optimizer, layer (d)) can reclaim.
//!
//! Dev probe: off unless `FAB_GEO_REDUNDANCY=1`; per-[`build_geo`](crate::backend::build_geo) run,
//! stderr report. Waste is attributed at the OUTERMOST repeated node (its inclusive render time is
//! what a cache hit would have skipped; nested repeats inside it don't double-count). Subtree
//! identity is a 64-bit structural content hash (f64s by bits, children memoized by node address —
//! O(tree) total). The 2D lowering inside `build_2d` is not separately instrumented: a repeated
//! `Extrude` node carries its profile in its own hash, and pure-2D outputs are cheap.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use fab_lang::{ExtrudeKind, GeoNode, Shape2D};

struct State {
    /// Node address → subtree hash (valid for the tree's lifetime within one build).
    memo: BTreeMap<usize, u64>,
    /// Subtree hash → (times rendered, first-render inclusive ns — the sanity anchor: a repeat of
    /// identical content should cost ≈ its first render; a big mismatch means bad attribution).
    seen: BTreeMap<u64, (u64, u128)>,
    /// Subtree hash → (node kind, repeat renders, wasted ns) — outermost repeats only.
    waste: BTreeMap<u64, (&'static str, u64, u128)>,
    nodes: u64,
    repeats: u64,
    wasted_ns: u128,
    /// >0 while inside a repeated subtree — only the outermost repeat books waste.
    repeat_depth: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn enabled() -> bool {
    std::env::var_os("FAB_GEO_REDUNDANCY").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// Arm the probe for one build (no-op unless `FAB_GEO_REDUNDANCY=1`).
pub(crate) fn reset() {
    STATE.with(|s| {
        *s.borrow_mut() = enabled().then(|| State {
            memo: BTreeMap::new(),
            seen: BTreeMap::new(),
            waste: BTreeMap::new(),
            nodes: 0,
            repeats: 0,
            wasted_ns: 0,
            repeat_depth: 0,
        });
    });
}

/// Per-node timing guard, handed out by [`enter`]; books waste on drop.
pub(crate) struct Guard {
    start: Instant,
    hash: u64,
    kind: &'static str,
    was_repeat: bool,
    outermost: bool,
}

/// Called at the top of every [`build`](crate::backend::build) recursion. `None` when disarmed.
pub(crate) fn enter(node: &GeoNode) -> Option<Guard> {
    STATE.with(|s| {
        let mut slot = s.borrow_mut();
        let st = slot.as_mut()?;
        let hash = hash_node(node, &mut st.memo);
        let prior = st.seen.entry(hash).or_insert((0, 0));
        let was_repeat = prior.0 > 0;
        prior.0 += 1;
        st.nodes += 1;
        let outermost = was_repeat && st.repeat_depth == 0;
        if was_repeat {
            st.repeats += 1;
            st.repeat_depth += 1;
        }
        Some(Guard {
            start: Instant::now(),
            hash,
            kind: kind_of(node),
            was_repeat,
            outermost,
        })
    })
}

impl Drop for Guard {
    fn drop(&mut self) {
        let dt = self.start.elapsed().as_nanos();
        STATE.with(|s| {
            let mut slot = s.borrow_mut();
            let Some(st) = slot.as_mut() else { return };
            if !self.was_repeat {
                if let Some(e) = st.seen.get_mut(&self.hash) {
                    e.1 = dt; // first-render anchor
                }
                return;
            }
            st.repeat_depth -= 1;
            if self.outermost {
                st.wasted_ns += dt;
                let e = st.waste.entry(self.hash).or_insert((self.kind, 0, 0));
                e.1 += 1;
                e.2 += dt;
            }
        });
    }
}

/// Print the build's redundancy report and disarm (call with the build's total wall).
#[allow(
    clippy::cast_precision_loss,
    reason = "stderr percentages over probe counters — never near 2^52"
)]
pub(crate) fn report(total: std::time::Duration) {
    STATE.with(|s| {
        let Some(st) = s.borrow_mut().take() else {
            return;
        };
        if st.nodes == 0 {
            return;
        }
        let total_ns = total.as_nanos().max(1);
        eprintln!(
            "[geo-redundancy] nodes {}  distinct {}  repeated-renders {}  build {:.2}s  WASTED {:.2}s ({:.1}% of build) — the P.2 kernel-cache ceiling",
            st.nodes,
            st.seen.len(),
            st.repeats,
            total.as_secs_f64(),
            st.wasted_ns as f64 / 1e9,
            100.0 * st.wasted_ns as f64 / total_ns as f64,
        );
        let mut top: Vec<_> = st.waste.iter().collect();
        top.sort_by_key(|(_, (_, _, ns))| std::cmp::Reverse(*ns));
        for (hash, (kind, count, ns)) in top.iter().take(8) {
            let first_ns = st.seen.get(hash).map_or(0, |e| e.1);
            eprintln!(
                "[geo-redundancy]   {kind:<12} x{count:<4} re-rendered  {:.3}s wasted (first render {:.3}s)",
                *ns as f64 / 1e9,
                first_ns as f64 / 1e9,
            );
        }
    });
}

fn kind_of(node: &GeoNode) -> &'static str {
    match node {
        GeoNode::Empty => "Empty",
        GeoNode::Leaf(_) => "Leaf",
        GeoNode::Transform { .. } => "Transform",
        GeoNode::Union(_) => "Union",
        GeoNode::Difference(_) => "Difference",
        GeoNode::Intersection(_) => "Intersection",
        GeoNode::Hull(_) => "Hull",
        GeoNode::Minkowski(_) => "Minkowski",
        GeoNode::Extrude { .. } => "Extrude",
        GeoNode::Color { .. } => "Color",
    }
}

// ── structural content hashing (f64s by bits, children memoized by address) ─────────────────────

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

struct FnvHasher(u64);
impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        fnv(&mut self.0, bytes);
    }
}

fn hash_node(node: &GeoNode, memo: &mut BTreeMap<usize, u64>) -> u64 {
    let key = std::ptr::from_ref(node) as usize;
    if let Some(&h) = memo.get(&key) {
        return h;
    }
    let mut h = FnvHasher(0xcbf2_9ce4_8422_2325);
    hash_node_into(node, memo, &mut h);
    let out = h.finish();
    memo.insert(key, out);
    out
}

fn hash_node_into(node: &GeoNode, memo: &mut BTreeMap<usize, u64>, h: &mut FnvHasher) {
    std::mem::discriminant(node).hash(h);
    match node {
        GeoNode::Empty => {}
        GeoNode::Leaf(mesh) => {
            for v in &mesh.verts {
                for c in [v.x, v.y, v.z] {
                    h.write(&c.to_bits().to_le_bytes());
                }
            }
            for t in &mesh.tris {
                for i in t.0 {
                    h.write(&u64::from(i).to_le_bytes());
                }
            }
        }
        GeoNode::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node(child, memo).to_le_bytes());
        }
        GeoNode::Union(kids)
        | GeoNode::Difference(kids)
        | GeoNode::Intersection(kids)
        | GeoNode::Hull(kids)
        | GeoNode::Minkowski(kids) => {
            for k in kids {
                h.write(&hash_node(k, memo).to_le_bytes());
            }
        }
        GeoNode::Extrude { kind, child } => {
            match kind {
                ExtrudeKind::Linear {
                    height,
                    twist,
                    scale,
                    slices,
                    facets,
                    center,
                } => {
                    h.write(&height.to_bits().to_le_bytes());
                    h.write(&twist.to_bits().to_le_bytes());
                    h.write(&scale[0].to_bits().to_le_bytes());
                    h.write(&scale[1].to_bits().to_le_bytes());
                    h.write(&u64::from(*slices).to_le_bytes());
                    h.write(&u64::from(*facets).to_le_bytes());
                    h.write(&[u8::from(*center)]);
                }
                ExtrudeKind::Rotate { angle, segments } => {
                    h.write(&angle.to_bits().to_le_bytes());
                    h.write(&u64::from(*segments).to_le_bytes());
                }
            }
            hash_shape_into(child, h);
        }
        GeoNode::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node(child, memo).to_le_bytes());
        }
    }
}

/// 2D subtrees hash inline (no memo — profiles are small relative to 3D meshes).
fn hash_shape_into(shape: &Shape2D, h: &mut FnvHasher) {
    std::mem::discriminant(shape).hash(h);
    match shape {
        Shape2D::Empty => {}
        Shape2D::Polygon(contours) => {
            for c in contours {
                for p in c {
                    h.write(&p.x.to_bits().to_le_bytes());
                    h.write(&p.y.to_bits().to_le_bytes());
                }
            }
        }
        Shape2D::Union(kids) | Shape2D::Difference(kids) | Shape2D::Intersection(kids) => {
            for k in kids {
                hash_shape_into(k, h);
            }
        }
        Shape2D::Offset {
            delta,
            join,
            segments,
            child,
        } => {
            h.write(&delta.to_bits().to_le_bytes());
            std::mem::discriminant(join).hash(h);
            h.write(&u64::from(*segments).to_le_bytes());
            hash_shape_into(child, h);
        }
        Shape2D::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_into(child, h);
        }
        Shape2D::Projection { cut, child } => {
            h.write(&[u8::from(*cut)]);
            // A 3D child under a 2D projection: hash with a local memo (rare node, small subtree).
            let mut memo = BTreeMap::new();
            h.write(&hash_node(child, &mut memo).to_le_bytes());
        }
        Shape2D::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_into(child, h);
        }
    }
}
