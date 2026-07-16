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
use std::time::Instant;

use fab_lang::GeoNode;

use crate::geo_hash::hash_node;

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
