//! M.1 — iterative `Drop` for the geometry trees. `GeoNode` ↔ `Shape2D` are MUTUALLY recursive (`Extrude` →
//! `Shape2D`, `Projection` → `GeoNode`) and a recursive module builds them RUNTIME-deep, so the derived
//! recursive `Drop` recurses one host-stack frame per level and overflows unwinding it — the stack-overflow
//! class the explicit-stack evaluator exists to kill (and the reason the harnesses reserved 1 GiB stacks).
//!
//! The fix mirrors `Frame::Drop`: unlink every descendant into ONE flat worklist and drop iteratively. The
//! trick that stops recursion — take a node's children BEFORE it drops (`mem::replace`/`mem::take`), so when
//! the node itself finally drops it's childless and its own `Drop` is a no-op. Heap-bounded, wasm-safe.

use super::geo::GeoNode;
use super::geo2d::Shape2D;

/// A pending subtree to drop — either dimension, since the two trees interleave.
enum Sub {
    D3(GeoNode),
    D2(Shape2D),
}

/// Move a `GeoNode`'s direct children OUT (leaving it childless) into `work`.
fn drain_geonode(node: &mut GeoNode, work: &mut Vec<Sub>) {
    match node {
        GeoNode::Transform { child, .. }
        | GeoNode::Color { child, .. }
        | GeoNode::Resize { child, .. } => {
            work.push(Sub::D3(std::mem::replace(&mut **child, GeoNode::Empty)));
        }
        GeoNode::Union(v)
        | GeoNode::Difference(v)
        | GeoNode::Intersection(v)
        | GeoNode::Hull(v)
        | GeoNode::Minkowski(v) => work.extend(std::mem::take(v).into_iter().map(Sub::D3)),
        GeoNode::Extrude { child, .. } => {
            work.push(Sub::D2(std::mem::replace(&mut **child, Shape2D::Empty)));
        }
        GeoNode::Leaf(_) | GeoNode::Empty => {}
    }
}

/// Move a `Shape2D`'s direct children OUT (leaving it childless) into `work`.
fn drain_shape2d(shape: &mut Shape2D, work: &mut Vec<Sub>) {
    match shape {
        Shape2D::Offset { child, .. }
        | Shape2D::Transform { child, .. }
        | Shape2D::Color { child, .. } => {
            work.push(Sub::D2(std::mem::replace(&mut **child, Shape2D::Empty)));
        }
        Shape2D::Union(v)
        | Shape2D::Difference(v)
        | Shape2D::Intersection(v)
        | Shape2D::Hull(v) => {
            work.extend(std::mem::take(v).into_iter().map(Sub::D2));
        }
        Shape2D::Projection { child, .. } => {
            work.push(Sub::D3(std::mem::replace(&mut **child, GeoNode::Empty)));
        }
        Shape2D::Polygon(_) | Shape2D::Empty => {}
    }
}

/// Drain the worklist to empty. Each popped node has its children taken (pushed back onto `work`) and then
/// drops CHILDLESS at the end of the arm → its own `Drop` early-returns, so nothing recurses.
fn drop_iter(mut work: Vec<Sub>) {
    while let Some(item) = work.pop() {
        match item {
            Sub::D3(mut n) => drain_geonode(&mut n, &mut work),
            Sub::D2(mut s) => drain_shape2d(&mut s, &mut work),
        }
    }
}

impl Drop for GeoNode {
    fn drop(&mut self) {
        // The childless leaves dominate (every primitive) — don't allocate a worklist for them.
        if matches!(self, GeoNode::Empty | GeoNode::Leaf(_)) {
            return;
        }
        let mut work = Vec::new();
        drain_geonode(self, &mut work);
        drop_iter(work);
    }
}

impl Drop for Shape2D {
    fn drop(&mut self) {
        if matches!(self, Shape2D::Empty | Shape2D::Polygon(_)) {
            return;
        }
        let mut work = Vec::new();
        drain_shape2d(self, &mut work);
        drop_iter(work);
    }
}
