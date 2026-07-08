//! M.1 — deep-tree `Drop` must be HEAP-bounded, not host-stack-bounded. A recursive module builds a
//! RUNTIME-deep `GeoNode` / `Shape2D` / `Value` RESULT tree (runtime depth, so NOT capped by the parser's
//! `MAX_DEPTH` — that only bounds source nesting). The default derived `Drop` recurses one host-stack frame
//! per level and overflows unwinding it — exactly the stack-overflow class the explicit-stack evaluator
//! exists to kill (it's what forced the 1 GiB-stack hacks in the harnesses). These build a very deep tree on
//! a deliberately SMALL stack and assert `Drop` returns without overflowing.

#![allow(clippy::unwrap_used, reason = "test harness: unwrap IS the assertion")]

use fab_lang::{GeoNode, Shape2D, Value};

/// Run `f` on a thread with a small 512 KiB stack — a recursive `Drop` overflows here even at modest depth,
/// where a default ~2 MiB test-thread stack could mask a shallow regression. Iterative `Drop` returns fine.
fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name("deep-drop".into())
        .stack_size(512 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

const DEPTH: usize = 200_000;

#[test]
fn deep_geonode_drop_is_heap_bounded() {
    on_small_stack(|| {
        let mut g = GeoNode::Empty;
        for _ in 0..DEPTH {
            g = GeoNode::Union(vec![g]);
        }
        drop(g); // must not overflow the 512 KiB stack
    });
}

#[test]
fn deep_shape2d_drop_is_heap_bounded() {
    on_small_stack(|| {
        let mut s = Shape2D::Empty;
        for _ in 0..DEPTH {
            s = Shape2D::Union(vec![s]);
        }
        drop(s);
    });
}

#[test]
fn deep_value_list_drop_is_heap_bounded() {
    on_small_stack(|| {
        let mut v = Value::Undef;
        for _ in 0..DEPTH {
            v = Value::list([v]);
        }
        drop(v);
    });
}
