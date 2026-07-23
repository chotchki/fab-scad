//! M.3 — the explicit-stack GEOMETRY-eval driver. A PEER of the expression machine (`eval_with_global`): its
//! own work stack of [`GTask`] + result stack of [`Geo`], so statement/geometry eval depth is heap-bound, not
//! host-stack-bound (the M.2 exposure — see `docs/heap-bounded-eval.md`). The full encoding, the two
//! load-bearing corrections a 4-lens design review caught (arity-by-MARK not count; a two-class error DRAIN),
//! and the increment plan live in `docs/m3-explicit-eval-spec.md` §DECISION.
//!
//! Every geometry-eval arm dispatches HERE — bare block, `if`, `let`, echo/assert passthrough, transforms,
//! booleans, hull, minkowski, offset, the extrudes, projection, color, the `*`/`%`/`!` modifiers, user-module
//! calls, `children()`, and `for`/`intersection_for`. This is the SOLE geometry evaluator; the former recursive
//! tree-walk was retired at M.3's finish once the driver proved bit-identical across the corpus + models.

use std::collections::BTreeMap;

use super::geo::GeoNode;
use super::geo2d::Shape2D;
use super::{
    Children, Ctx, ExtrudeKind, Geo, Join2D, Scope, Stmt, Value, boolean_of, check_assert,
    emit_echo, eval_with_ctx, force_2d, force_3d, geo, intersection_of, module, partition_children,
    transform_of, union_of,
};
use crate::geom::{Affine, Rgba};
use crate::parser::StmtKind;

/// Which CSG boolean — a name-free tag so [`Combinator`] carries no lifetime.
#[derive(Clone, Copy)]
enum BoolKind {
    Union,
    Difference,
    Intersection,
}

impl BoolKind {
    fn name(self) -> &'static str {
        match self {
            BoolKind::Union => "union",
            BoolKind::Difference => "difference",
            BoolKind::Intersection => "intersection",
        }
    }
}

/// A RESOLVED unary/N-ary combinator a [`GTask::Collect`] applies to its drained child `Geo`s to build ONE node
/// — the geometry analogue of the expression machine's `Apply` carrying bound arg values. Every variant reuses
/// the SAME wrap helper the recursive dispatch uses, so the produced node is bit-identical. Args that don't
/// depend on the evaluated children resolve EAGERLY at dispatch (the payloads here); `RotateExtrude` is the one
/// that needs the child first (its segment count reads the profile's `max_x`), so it carries the raw args.
enum Combinator {
    /// Bare block / implicit group — `union_of` handles the null/one/many collapse + the 2D/3D mixing.
    Union,
    /// `intersection_for`'s per-iteration collapse — intersect the iterations.
    Intersection,
    /// An affine transform wrapping the union of its children.
    Transform(Affine),
    /// A CSG boolean over its children (difference = first minus rest, etc.).
    Boolean(BoolKind),
    /// The convex hull of the children (3D only; 2D is LOUD-deferred).
    Hull,
    /// The Minkowski sum of the children (both dimensions since AC.2).
    Minkowski,
    /// `offset()` — grow/shrink the 2D outline of its (force-2D'd) child.
    Offset {
        delta: f64,
        join: Join2D,
        segments: u32,
    },
    /// `linear_extrude()` — sweep the (force-2D'd) child up +Z.
    LinearExtrude(ExtrudeKind),
    /// `rotate_extrude()` — revolve the (force-2D'd) child; the kind resolves in `apply` off the child's `max_x`.
    RotateExtrude {
        positional: Vec<Value>,
        named: BTreeMap<String, Value>,
        child_scope: Scope,
    },
    /// `projection()` — flatten the (force-3D'd) child to 2D (`cut` = slice at z=0, else shadow).
    Projection { cut: bool },
    /// `color()` — tag the child's subtree; `None` (invalid color) inherits (no node).
    Color(Option<Rgba>),
    /// `resize()` — scale the (force-3D'd) child so its bbox hits `newsize` per axis; the backend measures
    /// the built child (a `0` axis is kept, or auto-scaled proportionally).
    Resize { newsize: [f64; 3], auto: [bool; 3] },
}

impl Combinator {
    /// Apply this combinator to the child `Geo`s a [`GTask::Collect`] drained (in source order), producing ONE
    /// node. Infallible since AC.2 closed the last LOUD-deferral (2D minkowski).
    fn apply(self, children: Vec<Geo>, ctx: &Ctx<'_>) -> Geo {
        match self {
            Combinator::Union => union_of(children, ctx),
            Combinator::Intersection => intersection_of(children, ctx),
            Combinator::Transform(matrix) => transform_of(matrix, union_of(children, ctx)),
            Combinator::Boolean(kind) => boolean_of(kind.name(), children, ctx),
            Combinator::Hull => match partition_children(children, ctx) {
                Children::D3(kids) => Geo::D3(GeoNode::Hull(kids)),
                // 2D hull pools every child's contour points through Manifold's `CrossSection::hull_of`
                // (X.4) — the same monotone-chain the 3D path uses one dimension up.
                Children::D2(kids) => Geo::D2(Shape2D::Hull(kids)),
            },
            Combinator::Minkowski => match partition_children(children, ctx) {
                Children::D3(kids) => Geo::D3(GeoNode::Minkowski(kids)),
                // 2D minkowski (AC.2): the kernel's tiered hull+union, one dimension down.
                Children::D2(kids) => Geo::D2(Shape2D::Minkowski(kids)),
            },
            Combinator::Offset {
                delta,
                join,
                segments,
            } => Geo::D2(Shape2D::Offset {
                delta,
                join,
                segments,
                child: Box::new(force_2d(union_of(children, ctx), ctx)),
            }),
            Combinator::LinearExtrude(kind) => Geo::D3(GeoNode::Extrude {
                kind,
                child: Box::new(force_2d(union_of(children, ctx), ctx)),
            }),
            Combinator::RotateExtrude {
                positional,
                named,
                child_scope,
            } => {
                let shape = force_2d(union_of(children, ctx), ctx);
                let kind = geo::resolve_rotate_extrude(
                    &positional,
                    &named,
                    &child_scope,
                    shape.max_x().unwrap_or(0.0),
                );
                Geo::D3(GeoNode::Extrude {
                    kind,
                    child: Box::new(shape),
                })
            }
            Combinator::Projection { cut } => Geo::D2(Shape2D::Projection {
                cut,
                child: Box::new(force_3d(union_of(children, ctx), ctx)),
            }),
            Combinator::Color(color) => match (union_of(children, ctx), color) {
                (Geo::D3(node), Some(color)) => Geo::D3(GeoNode::Color {
                    color,
                    child: Box::new(node),
                }),
                (Geo::D2(shape), Some(color)) => Geo::D2(Shape2D::Color {
                    color,
                    child: Box::new(shape),
                }),
                // invalid color → inherit: the child unchanged, either dimension.
                (child, None) => child,
            },
            Combinator::Resize { newsize, auto } => Geo::D3(GeoNode::Resize {
                newsize,
                auto,
                child: Box::new(force_3d(union_of(children, ctx), ctx)),
            }),
        }
    }
}

/// One step on the geometry driver's work stack. WORK tasks may eval / emit / return `Err`; CLEANUP tasks are
/// infallible ctx side-effects that MUST run on both the happy AND the error-drain path (LIFO), so the driver
/// keys its error handling on this WORK/CLEANUP split, never on a name or a push/pop heuristic.
enum GTask<'a> {
    /// CLEANUP — drop every result above `mark` (AK.3 `%` background): the subtree EVALUATED (its
    /// echoes/asserts/rands fired, matching upstream), but its geometry is excluded from output.
    DiscardAbove { mark: usize },
    /// WORK — dispatch ONE statement (native arm, or the shim for a still-recursive arm).
    Stmt {
        stmt: &'a Stmt,
        scope: Scope,
        global: Scope,
        island: usize,
    },
    /// WORK — expand a statement list under a fresh mark: hoist, push-if-any local modules, record
    /// `mark = results.len()`, push the paired `Collect{mark, comb}`, then the child `Stmt`s in REVERSE source
    /// order (so they land in source order), then `PopLocalModules` iff pushed. `stmts` is a ref list (a `Vec`
    /// so `children()` can pass a SELECTED subset, and the recursive arms already `collect()` their children).
    EvalNodes {
        stmts: Vec<&'a Stmt>,
        scope: Scope,
        global: Scope,
        island: usize,
        comb: Combinator,
    },
    /// WORK — drain `results.split_off(mark)` (exactly what this block pushed, 0-or-1 per child stmt) and apply
    /// `comb`, pushing ONE `Geo`.
    Collect { mark: usize, comb: Combinator },
    /// WORK — the `!` root modifier: drain `results.split_off(mark)` INTO `ctx.root_override` (consumes), so the
    /// parent's `Collect` legitimately sees zero there. Discarded on the error path.
    CaptureRoot { mark: usize },
    /// WORK — J.5.2a CSG memo: peek the just-produced module node (`results.last()`, like the eval cache's
    /// `CacheStore` — pushed below the body's `Collect` so it fires the instant the node lands) and store it
    /// under `key` IFF the body left NO observable side effect (the `snap` counters are unmoved). WORK, never
    /// CLEANUP: it must not fire on the error drain (an errored body is structurally uncacheable).
    CacheStoreModule {
        key: super::mod_cache::ModKey,
        snap: super::PuritySnap,
        /// The call frame's boundary id — the open capture this store reads its observed `$`-read set from.
        entry: u64,
    },
    /// CLEANUP — pop a scope-local module store pushed by an `EvalNodes` that had local defs.
    PopLocalModules,
    /// CLEANUP — the three-frame user-module pop: restore `module_depth` from the pre-call SNAPSHOT (never a
    /// decrement — it's a `Cell`), then pop `module_stack` + `children_stack`. (Increment 2.) `capture` is
    /// the read-capture token to close, when this call opened one (rung 2b) — closed HERE, not at
    /// `CacheStoreModule`, because CLEANUP runs on the error drain too (an errored body must not leak its
    /// capture; the store, being WORK, is skipped there).
    PopModuleFrame { depth: usize, capture: Option<u64> },
    /// CLEANUP — re-push a `ChildrenFrame` that `children()` transiently popped, keeping `children_stack`
    /// balanced across both paths. (Increment 2.)
    RestoreChildrenFrame(super::ChildrenFrame<'a>),
}

/// The top-level geometry entry: drive a PRE-HOISTED statement list (as [`eval_geometry`](super::eval_geometry)
/// is always called — `run_stmts` publishes the global, `eval_nodes` hoists the child scope) and return the RAW
/// top nodes (the caller — `run_stmts` or a combinator — applies any union / root-override).
pub(super) fn eval_geometry_driver<'a>(
    stmts: &[&'a Stmt],
    scope: &Scope,
    global: &Scope,
    island: usize,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Geo>> {
    let mut results: Vec<Geo> = Vec::new();
    // Drive each TOP-LEVEL statement independently (they share no work-stack state — a statement's subtree
    // fully resolves before the next), which gives a clean per-statement boundary for the assert rule below.
    // Equivalent to one combined stack on the happy path (source order preserved, results accumulate in place).
    for stmt in stmts {
        let mark = results.len();
        let work = vec![GTask::Stmt {
            stmt,
            scope: scope.clone(),
            global: global.clone(),
            island,
        }];
        if let Err(e) = drive(work, &mut results, ctx) {
            match e {
                // L.5.8 — a failed `assert` is NOT fatal: OpenSCAD prints the ERROR but still exports the
                // top-level geometry accumulated BEFORE the failing statement. So DROP this statement's
                // partial subtree (truncate to its start mark), warn, and STOP — no later statement renders
                // either (verified vs the oracle: `cube(10); assert(false); cube(5);` → exports cube(10)).
                crate::Error::Assert(msg) => {
                    results.truncate(mark);
                    ctx.warn(msg);
                    break;
                }
                // A genuine evaluation/parse/lower fault stays LOUD — stamped with THIS top-level
                // statement's span (W.3.37) so the GUI points at the user's line. `at` no-ops if the
                // error already carries a span (an inner stamp wins).
                other => return Err(other.at(stmt.span.clone())),
            }
        }
    }
    Ok(results)
}

/// The driver loop. Pops tasks to empty. CLEANUP tasks run their ctx side-effect on BOTH paths (LIFO). On the
/// FIRST `Err` from a WORK task the driver captures it and enters DRAIN: it keeps popping so the parked CLEANUP
/// tasks still fire (balancing ctx), but DISCARDS every WORK task without executing it — reproducing the
/// recursive path's "first `?` wins, no later side effect" (a re-dispatching drain would emit phantom echoes /
/// run later asserts → a different error + message stream, observable today).
fn drive<'a>(mut work: Vec<GTask<'a>>, results: &mut Vec<Geo>, ctx: &Ctx<'a>) -> crate::Result<()> {
    let mut first_err: Option<crate::Error> = None;
    while let Some(task) = work.pop() {
        // CLEANUP — always run (happy path AND drain), then move on.
        if matches!(
            task,
            GTask::PopLocalModules | GTask::PopModuleFrame { .. } | GTask::RestoreChildrenFrame(_)
        ) {
            run_cleanup(task, ctx);
            continue;
        }
        // WORK — discard while draining; otherwise dispatch and latch the first error.
        if first_err.is_some() {
            continue;
        }
        if let Err(e) = dispatch_work(task, &mut work, results, ctx) {
            first_err = Some(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// Run a CLEANUP task's infallible ctx side-effect (both the happy path and the error drain reach here).
fn run_cleanup<'a>(task: GTask<'a>, ctx: &Ctx<'a>) {
    match task {
        GTask::PopLocalModules => {
            ctx.local_modules.borrow_mut().pop();
        }
        GTask::PopModuleFrame { depth, capture } => {
            // Restore from the SNAPSHOT — never `set(get()-1)`: module_depth is a Cell<usize>, and a decrement
            // during a mis-built drain would underflow.
            ctx.module_depth.set(depth);
            ctx.module_stack.borrow_mut().pop();
            ctx.children_stack.borrow_mut().pop();
            if let Some(entry) = capture {
                super::mod_cache::close_capture(entry);
            }
        }
        GTask::RestoreChildrenFrame(frame) => {
            ctx.children_stack.borrow_mut().push(frame);
        }
        // The driver loop partitions CLEANUP from WORK before routing; a WORK task here is a driver
        // bug — loud beats silently no-opping in the error drain.
        #[allow(clippy::unreachable, reason = "structurally unreachable — see comment")]
        _ => unreachable!("run_cleanup only reached for CLEANUP tasks"),
    }
}

/// Dispatch one WORK task.
fn dispatch_work<'a>(
    task: GTask<'a>,
    work: &mut Vec<GTask<'a>>,
    results: &mut Vec<Geo>,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    match task {
        GTask::Stmt {
            stmt,
            scope,
            global,
            island,
        } => dispatch_stmt(stmt, scope, global, island, work, results, ctx),
        // Expand a child statement list under a fresh mark. Push order is bottom→top: the `Collect` (fires
        // LAST, after every child + the local-module pop), then `PopLocalModules` iff this block pushed local
        // defs (fires after the children that may reference them), then the child `Stmt`s in REVERSE source
        // order (so they pop — and land on the result stack — in source order).
        GTask::EvalNodes {
            stmts,
            scope,
            global,
            island,
            comb,
        } => {
            let hoisted = super::hoist_scope(&stmts, &scope, ctx)?;
            let local_mods = super::collect_module_defs(&stmts);
            let pushed = !local_mods.is_empty();
            if pushed {
                ctx.local_modules
                    .borrow_mut()
                    .push((local_mods, hoisted.clone()));
            }
            let mark = results.len();
            work.push(GTask::Collect { mark, comb });
            if pushed {
                work.push(GTask::PopLocalModules);
            }
            for &stmt in stmts.iter().rev() {
                // Assignments hoist (bound above); skip them exactly as `eval_geometry` does.
                if matches!(stmt.kind, StmtKind::Assignment { .. }) {
                    continue;
                }
                work.push(GTask::Stmt {
                    stmt,
                    scope: hoisted.clone(),
                    global: global.clone(),
                    island,
                });
            }
            Ok(())
        }
        // Drain exactly what this block pushed — `results.split_off(mark)`, 0-or-1 per child stmt, in source
        // order — and apply the combinator, pushing ONE node.
        GTask::Collect { mark, comb } => {
            let children = results.split_off(mark);
            results.push(comb.apply(children, ctx));
            Ok(())
        }
        // The `!` root subtree resolved above `mark`; consume it into the program-global root override (so an
        // ancestor `Collect` sees zero there, and `run_stmts` renders ONLY the `!`-tagged subtrees).
        GTask::DiscardAbove { mark } => {
            results.truncate(mark);
            Ok(())
        }
        GTask::CaptureRoot { mark } => {
            let captured = results.split_off(mark);
            ctx.root_override.borrow_mut().extend(captured);
            Ok(())
        }
        // J.5.2a — memoize the module's node IFF its body was pure. Peek (never consume — the parent's `Collect`
        // reads it). An impure subtree (echo/warning, seedless `rands`, `parent_module`) re-runs every time
        // instead of serving a stale node; `$children == 0` (the eligibility gate) already fenced the children
        // hazard. This mirrors the eval cache's `Task::CacheStore` on the geometry side.
        GTask::CacheStoreModule { key, snap, entry } => {
            if let Some(geo) = results.last() {
                // Purity for a MODULE is narrower than for a function: the output is a `Geo` (mesh/transform/
                // boolean), NEVER a closure — so a closure created + used + discarded inside the body can't
                // reach the cached node (the function cache declines on `closures` only because a fn can RETURN
                // one). BOSL2 leaves mint function-literals constantly, so keying on `closures` here declined ~87%
                // of stores for zero correctness gain. What DOES matter: echo/warning output (messages), seedless
                // `rands` draw order (draws), and `parent_module` reads (impure_reads) — a hit that skips those
                // diverges the observable stream / returns a context-dependent node not in the key.
                let msg_moved = ctx.messages.borrow().len() != snap.messages;
                let draws_moved = ctx.rand_stream.borrow().draws() != snap.draws;
                let impure_moved = ctx.impure_reads.get() != snap.impure_reads;
                if msg_moved || draws_moved || impure_moved {
                    ctx.mod_cache
                        .borrow_mut()
                        .note_decline(msg_moved, draws_moved, impure_moved);
                } else if let Some(reads) = super::mod_cache::capture_reads(entry) {
                    // The capture (still open — `PopModuleFrame` closes it next) carries the body's
                    // observed `$`-read set: the entry's probe condition.
                    ctx.mod_cache.borrow_mut().put(
                        key,
                        geo.clone(),
                        reads,
                        ctx.config.csg_cache_keycap,
                    );
                }
            }
            Ok(())
        }
        // Mirror of run_cleanup's guard: the driver routes CLEANUP tasks before this dispatch, so a
        // cleanup arm here is a driver bug — loud beats returning a fabricated Err.
        #[allow(clippy::unreachable, reason = "structurally unreachable — see comment")]
        GTask::PopLocalModules | GTask::PopModuleFrame { .. } | GTask::RestoreChildrenFrame(_) => {
            unreachable!("CLEANUP tasks are handled in the driver loop, not dispatch_work")
        }
    }
}

/// Dispatch ONE statement: a converted arm pushes native work-stack tasks; every still-recursive arm falls
/// through to the [`shim_stmt`] bridge.
fn dispatch_stmt<'a>(
    stmt: &'a Stmt,
    scope: Scope,
    global: Scope,
    island: usize,
    work: &mut Vec<GTask<'a>>,
    results: &mut Vec<Geo>,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    match &stmt.kind {
        // A2 — a bare `{ … }` block groups its children into ONE implicit-union node in a fresh hoisted scope.
        StmtKind::Block(stmts) => {
            work.push(GTask::EvalNodes {
                stmts: stmts.iter().collect(),
                scope,
                global,
                island,
                comb: Combinator::Union,
            });
            Ok(())
        }
        // A6 — `if (cond) A else B` contributes the TAKEN branch's geometry (the untaken branch is inert).
        StmtKind::If {
            modifiers,
            cond,
            then,
            els,
        } => {
            // `if` is grammatically an instantiation, so its modifiers mirror `dispatch_module`
            // EXACTLY: `!` captures the subtree as the root override; `*` drops geometry AND side
            // effects (the condition never evaluates); `%` EVALUATES but excludes the geometry
            // (AK.3 — the AA.1 assumption that % matched * was oracle-refuted); `#` is
            // preview-only, a render no-op.
            if modifiers.root {
                work.push(GTask::CaptureRoot {
                    mark: results.len(),
                });
            }
            if modifiers.disable {
                return Ok(());
            }
            // `%` background: the CONDITION + taken branch still evaluate (upstream-probed — the
            // seed-209 gen-diff finding); only the geometry is excluded from output.
            if modifiers.background {
                work.push(GTask::DiscardAbove {
                    mark: results.len(),
                });
            }
            let branch = if eval_with_ctx(cond, &scope, ctx)?.is_truthy() {
                then
            } else {
                els
            };
            work.push(GTask::EvalNodes {
                stmts: branch.iter().collect(),
                scope,
                global,
                island,
                comb: Combinator::Union,
            });
            Ok(())
        }
        StmtKind::Module(mi) => dispatch_module(mi, scope, global, island, work, results, ctx),
        // Definitions + empties + assignments produce NO geometry: functions/modules register at `build_ctx`
        // (their own namespaces), and assignments are hoisted (bound before the geometry pass), so all four are
        // no-ops here. A `use`/`include` reaching eval is LOUD — the loader resolves top-level ones away, so one
        // here is either nested (not scanned) or a raw `eval_program` on an unloaded program.
        StmtKind::Empty
        | StmtKind::FunctionDef { .. }
        | StmtKind::ModuleDef { .. }
        | StmtKind::Assignment { .. } => Ok(()),
        StmtKind::Use(_) | StmtKind::Include(_) => Err(crate::Error::Unimplemented(
            "unresolved use/include (nested, or eval_program on an unloaded program — use evaluate_file/evaluate_with_base)",
        )),
    }
}

/// Dispatch a module INSTANTIATION. The `!`/`*`/`%` modifiers are handled HERE (so the shim path calls
/// `eval_stmt_dispatch`, which does NOT recheck them — each modifier is owned by exactly one place), then the
/// name routes to a native combinator or falls through to the shim (children / for / user-module / primitive).
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors eval_stmt_dispatch's threaded context (stmt/scope/global/island/work/results/ctx)"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one arm per module combinator — the dispatch match IS the routing table"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "GTasks store owned Scopes; callers hand this an owned clone, so by-value is the honest shape"
)]
fn dispatch_module<'a>(
    mi: &'a crate::parser::ModuleInstantiation,
    scope: Scope,
    global: Scope,
    island: usize,
    work: &mut Vec<GTask<'a>>,
    results: &mut Vec<Geo>,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    // `!` ROOT — capture this subtree into the root override. Push `CaptureRoot` FIRST (bottom): the body's
    // tasks (or the shim's synchronous push) land ABOVE the mark, then `CaptureRoot` drains exactly them.
    if mi.modifiers.root {
        work.push(GTask::CaptureRoot {
            mark: results.len(),
        });
    }
    // `*` DISABLE — no geometry AND no side effects, before ANY name dispatch. `%` BACKGROUND is
    // DIFFERENT (AK.3, oracle-probed): the subtree still EVALUATES (echoes fire, `%cube([echo(…)…`
    // prints upstream) — only its GEOMETRY is excluded from the render output.
    if mi.modifiers.disable {
        return Ok(());
    }
    if mi.modifiers.background {
        work.push(GTask::DiscardAbove {
            mark: results.len(),
        });
    }
    let name = mi.name.as_str();
    super::fnprofile::record_module(name); // dev probe (FAB_PROFILE_FNS): per-name module-call counts
    // A block's worth of children as a `comb`-combined `EvalNodes` in `scope` — the shared shape of the
    // transform / boolean / hull / offset / extrude / projection / color arms.
    let group = |work: &mut Vec<GTask<'a>>, comb, scope| {
        work.push(GTask::EvalNodes {
            stmts: mi.children.iter().collect(),
            scope,
            global: global.clone(),
            island,
            comb,
        });
    };
    match name {
        // A3 — echo/assert are PASSTHROUGH: the console side effect / hard check FIRST (can error), then the
        // children render as an implicit union (iff any).
        "echo" | "assert" => {
            if name == "echo" {
                emit_echo(&mi.args, &scope, &scope, ctx)?;
            } else {
                check_assert(&mi.args, &scope, &scope, ctx)?;
            }
            if !mi.children.is_empty() {
                group(work, Combinator::Union, scope);
            }
            Ok(())
        }
        // A4 — an affine transform wraps the union of its children ($-args don't reach it → child scope dropped).
        _ if geo::is_transform(name) => {
            let (positional, named, _child_scope) = module::eval_args(mi, &scope, ctx)?;
            let matrix = geo::transform_matrix(name, &positional, &named);
            group(work, Combinator::Transform(matrix), scope);
            Ok(())
        }
        // A5 — CSG booleans / hull / minkowski over the children.
        _ if geo::is_boolean(name) => {
            let kind = match name {
                "difference" => BoolKind::Difference,
                "intersection" => BoolKind::Intersection,
                _ => BoolKind::Union,
            };
            group(work, Combinator::Boolean(kind), scope);
            Ok(())
        }
        "hull" => {
            group(work, Combinator::Hull, scope);
            Ok(())
        }
        "minkowski" => {
            group(work, Combinator::Minkowski, scope);
            Ok(())
        }
        // group() — upstream's explicit no-op grouping module (AH.2.5; scope-assignment-tests uses
        // it as a scoping probe). A5' — render() forces a CGAL/nef evaluation in OpenSCAD; in our
        // Manifold-backed pipeline every node is already an exact manifold, so it's semantically
        // identity — group its children (its `convexity` arg is a render hint with no bearing on
        // geometry). Unblocks BOSL2's cubetruss_corner() + slicer_module.scad. Both = child union.
        "group" | "render" => {
            group(work, Combinator::Union, scope);
            Ok(())
        }
        // resize() scales the child so its bbox hits `newsize` per axis — the scale needs the BUILT child's
        // bbox, so it resolves in the backend (like RotateExtrude defers to apply). newsize/auto resolve eagerly.
        "resize" => {
            let (positional, named, _child_scope) = module::eval_args(mi, &scope, ctx)?;
            let (newsize, auto) = geo::resolve_resize(&positional, &named);
            group(work, Combinator::Resize { newsize, auto }, scope);
            Ok(())
        }
        // A6 — the fixed-dimension bridges + color, each resolving its params eagerly off the child scope.
        "offset" => {
            let (positional, named, child_scope) = module::eval_args(mi, &scope, ctx)?;
            let (delta, join, segments) = geo::resolve_offset(&positional, &named, &child_scope);
            group(
                work,
                Combinator::Offset {
                    delta,
                    join,
                    segments,
                },
                scope,
            );
            Ok(())
        }
        "linear_extrude" => {
            let (positional, named, child_scope) = module::eval_args(mi, &scope, ctx)?;
            let kind = geo::resolve_linear_extrude(&positional, &named, &child_scope);
            group(work, Combinator::LinearExtrude(kind), scope);
            Ok(())
        }
        "rotate_extrude" => {
            // The segment count needs the profile's `max_x`, so resolve the kind in `apply` (after the child).
            let (positional, named, child_scope) = module::eval_args(mi, &scope, ctx)?;
            group(
                work,
                Combinator::RotateExtrude {
                    positional,
                    named,
                    child_scope,
                },
                scope,
            );
            Ok(())
        }
        "projection" => {
            let (positional, named, _child_scope) = module::eval_args(mi, &scope, ctx)?;
            let cut = matches!(
                named.get("cut").or_else(|| positional.first()),
                Some(Value::Bool(true))
            );
            group(work, Combinator::Projection { cut }, scope);
            Ok(())
        }
        "color" => {
            let (positional, named, _child_scope) = module::eval_args(mi, &scope, ctx)?;
            let color = geo::resolve_color(&positional, &named);
            group(work, Combinator::Color(color), scope);
            Ok(())
        }
        // A6 — `let(a=…) children` binds SEQUENTIALLY into a child scope, then the children render there.
        "let" => {
            let mut child = scope.child();
            for arg in &mi.args {
                let value = eval_with_ctx(&arg.value, &child, ctx)?;
                child.bind(arg.name.as_deref().unwrap_or(""), value);
            }
            group(work, Combinator::Union, child);
            Ok(())
        }
        // B2 — children() renders the current module call's stashed call-site children, LATE, in the caller's
        // lexical scope + island with the CURRENT $-overlay. The frame is transiently POPPED (so a nested
        // children() inside the rendered children binds to the ENCLOSING call) and RESTORED by a CLEANUP task
        // after they resolve — balanced on the error path too (recursive restores before its `?`).
        "children" => {
            let (positional, _, _) = module::eval_args(mi, &scope, ctx)?;
            let Some(frame) = ctx.children_stack.borrow_mut().pop() else {
                results.push(Geo::D3(GeoNode::Empty)); // children() outside a module call → nothing
                return Ok(());
            };
            let selected: Vec<&Stmt> = match positional.first() {
                None => frame.stmts.clone(), // children() → all
                Some(Value::Num(i)) => super::child_at(*i)
                    .and_then(|i| frame.stmts.get(i).copied())
                    .into_iter()
                    .collect(),
                Some(Value::NumList(xs)) => xs
                    .iter()
                    .filter_map(|&i| super::child_at(i).and_then(|i| frame.stmts.get(i).copied()))
                    .collect(),
                // AH.2.5 (children-tests golden): `children([0:4])` selects by RANGE too — same
                // per-index bounds rules as the vector form; a wrong-way range yields nothing.
                Some(Value::Range { start, step, end }) => {
                    super::value::range_iter(*start, *step, *end)
                        .filter_map(|i| {
                            super::child_at(i).and_then(|i| frame.stmts.get(i).copied())
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            // The caller's LEXICAL scope (frame.scope) with the CURRENT dynamic $-context overlaid (`call_frame`,
            // by reference), the caller's module island, and the CURRENT global (where children() is written).
            let caller_island = frame.island;
            let mut child_scope = Scope::call_frame(&frame.scope, &scope);
            // `$children`/`$parent_modules` are per-CALL bookkeeping, not inheritable $-context
            // (AH.2.5, the variable-scope-tests golden): a rendered child sees the counts of the
            // module call it was WRITTEN in — the caller's — not the callee's, so re-pin them over
            // the dynamic overlay the L.5.2/L.2.8p rules otherwise inherit.
            for var in ["$children", "$parent_modules"] {
                if let Some(v) = frame.scope.lookup_opt(var) {
                    child_scope.bind(var, v);
                }
            }
            // L.5.2 — the block's sibling assignments bind into the child scope FIRST: prepend them so
            // `hoist_scope` (whole-scope, last-wins) picks them up, then the selected geometry renders SEEING
            // those locals (BOSL2logo's `path_sweep(bezpath_curve(sbez))` reads a sibling `sbez = …`). Empty
            // in the common block with no interleaved assignments — a plain clone-extend, no node.
            let mut render_stmts = frame.assigns.clone();
            render_stmts.extend(selected);
            work.push(GTask::RestoreChildrenFrame(frame));
            work.push(GTask::EvalNodes {
                stmts: render_stmts,
                scope: child_scope,
                global: global.clone(),
                island: caller_island,
                comb: Combinator::Union,
            });
            Ok(())
        }
        // B3 — for / intersection_for: bind the loop var(s) over their range/vector (the loop-var recursion
        // stays on the host — it's source-bounded, ~1-3 vars, pure expr evals), collecting the PER-ITERATION
        // scopes; then push one `EvalNodes{children, Union}` per iteration (REVERSE product order → source
        // order) under an outer `Collect` that unions (for) or intersects (intersection_for) the iterations.
        // The body eval goes through the work stack, so a recursion inside the loop is heap-bounded.
        "for" | "intersection_for" => {
            let mut scopes: Vec<Scope> = Vec::new();
            for_scopes(&mi.args, &scope, ctx, &mut scopes)?;
            let outer = if name == "intersection_for" {
                Combinator::Intersection
            } else {
                Combinator::Union
            };
            let child_refs: Vec<&Stmt> = mi.children.iter().collect();
            let mark = results.len();
            work.push(GTask::Collect { mark, comb: outer });
            for iter_scope in scopes.into_iter().rev() {
                work.push(GTask::EvalNodes {
                    stmts: child_refs.clone(),
                    scope: iter_scope,
                    global: global.clone(),
                    island,
                    comb: Combinator::Union,
                });
            }
            Ok(())
        }
        // B1 — a module INSTANTIATION resolves against the CURRENT island: a USER module runs its body on the
        // work stack (host recursion GONE — the payoff), everything else is a builtin PRIMITIVE (a synchronous
        // leaf, or a LOUD unknown). Mirrors the recursive fallthrough (trace + resolve + call/eval).
        _ => {
            super::trace::module(ctx.module_depth.get(), name);
            if let Some((def, home, base)) = ctx.resolve_module(island, name) {
                push_user_module(mi, def, home, base, scope, island, work, results, ctx)
            } else {
                results.push(module::eval_module(mi, &scope, ctx)?);
                Ok(())
            }
        }
    }
}

/// B1 — schedule a USER-module call on the work stack (the recursion-removing analogue of `call_user_module`).
/// The setup is EAGER + ordering-sensitive (the depth guard, the `$children`/`$parent_modules` binds, the three
/// `Ctx` frame pushes), exactly mirroring the recursive path; then it pushes bottom→top `PopModuleFrame{depth}`
/// (CLEANUP — restores the frames on BOTH paths), the body's `Collect{Union}`, and the body `Stmt`. LIFO → the
/// body runs, its 0-or-1 node unions to the module's result, then the frames pop. A `bind_module_scope` arg
/// error returns before the frame pushes (leaking `module_depth`, harmless — the whole eval aborts, as it does
/// recursively).
#[allow(
    clippy::too_many_arguments,
    reason = "the module-call context, mirroring call_user_module's threaded arguments"
)]
fn push_user_module<'a>(
    mi: &'a crate::parser::ModuleInstantiation,
    def: super::loader::ModDef<'a>,
    home: usize,
    base: Option<Scope>,
    caller: Scope,
    island: usize,
    work: &mut Vec<GTask<'a>>,
    results: &mut Vec<Geo>,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    let (params, body) = def;
    let depth = ctx.module_depth.get();
    if depth >= super::MAX_MODULE_DEPTH {
        // AD.5: upstream's verdict class ("Recursion detected calling module 'rec'"), not Unimplemented —
        // this guard is a deliberate semantic bound on a runaway recursive module, not a missing feature.
        return Err(crate::Error::Eval(format!(
            "Recursion detected calling module '{}'",
            mi.name
        )));
    }
    let body_ptr = std::ptr::from_ref(body).cast::<()>();
    // The body's lexical base is the module's HOME ISLAND global (a scope-local module carries its captured
    // defining scope as `base` instead); args, though, bind in the CALLER's scope.
    let home_global = base.unwrap_or_else(|| ctx.island_globals.borrow()[home].clone());
    let mut call = super::bind_module_scope(params, &mi.args, &caller, &home_global, ctx)?;
    // `$children` = the call-site child count; the children are stashed for `children()` to render LATE in the
    // CALLER's scope + island. Lone-`;` empties AND child-block assignments are NOT children (either would
    // misalign the count + `children(i)`); the assignments are kept separately so their bindings still reach
    // every geometry child (L.5.2).
    let child_stmts: Vec<&Stmt> = mi
        .children
        .iter()
        .filter(|s| !matches!(s.kind, StmtKind::Empty | StmtKind::Assignment { .. }))
        .collect();
    let child_assigns: Vec<&Stmt> = mi
        .children
        .iter()
        .filter(|s| matches!(s.kind, StmtKind::Assignment { .. }))
        .collect();
    let childless = child_stmts.is_empty();
    call.bind(
        "$children",
        Value::Num(super::child_count(child_stmts.len())),
    );
    // `$parent_modules` = the instantiation-stack size INCLUDING this call (AH.2.5, the
    // parent_module-tests golden: `print` called from `test` sees 2, so `[1:$parent_modules-1]`
    // reaches the outermost name). Self isn't pushed yet — the push happens on the MISS path
    // below, next to the children frame — hence the +1. Bound now so it's in the memo key.
    call.bind(
        "$parent_modules",
        Value::Num(super::child_count(ctx.module_stack.borrow().len() + 1)),
    );
    // Dev probe (off unless FAB_CSG_REDUNDANCY=1, J.5.1): the fully-bound `call` frame carries the params +
    // reaching $-context; count repeats vs distinct to gauge the memo ceiling. Suppressed: its `specials()`
    // walk is diagnostic, not a semantic read — it must not record into an enclosing capture.
    super::mod_cache::suppressed(|| {
        super::mod_redundancy::record(body_ptr, &home_global, mi.name.as_str(), params, &call);
    });
    // J.5.2 — the CSG memo. Eligible ONLY when child-less: a module that renders `children()` depends on its
    // call-site children (NOT in the key), but `$children == 0` ⇒ `children()` renders nothing ⇒ the result is
    // a pure function of (body, home, params) + its observed `$`-reads (rung 2b — see `mod_cache`). On a HIT,
    // push the cached `Geo` and skip the body, the frames, AND the depth bump — the redundant subtree never
    // runs (the whole point; `get` replays the entry's reads so an ENCLOSING capture still records them). On a
    // MISS, OPEN a read capture on the call frame, snapshot the purity counters, and queue a
    // `CacheStoreModule` that memoizes (node, observed reads) IFF the body was pure.
    let store = if childless && ctx.config.csg_cache {
        let param_vals: Vec<Value> = params.iter().map(|p| call.lookup(&p.name)).collect();
        if super::mod_cache::worth_caching(&param_vals, ctx.config.csg_cache_keycap) {
            let key = super::mod_cache::ModKey::new(body_ptr, &home_global, &param_vals);
            if let Some(geo) = ctx.mod_cache.borrow_mut().get(&key, &call) {
                results.push(geo);
                return Ok(());
            }
            let entry = call.boundary_id();
            super::mod_cache::open_capture(entry);
            Some((
                key,
                super::PuritySnap {
                    messages: ctx.messages.borrow().len(),
                    draws: ctx.rand_stream.borrow().draws(),
                    closures: ctx.closures.borrow().len(),
                    impure_reads: ctx.impure_reads.get(),
                },
                entry,
            ))
        } else {
            None
        }
    } else {
        None
    };
    // MISS (or ineligible): the full three-frame setup + body. The depth bump lands HERE (a hit is not a
    // recursion level). The children are stashed for `children()`; the module name pushed for `parent_module`.
    ctx.module_depth.set(depth + 1);
    ctx.children_stack.borrow_mut().push(super::ChildrenFrame {
        stmts: child_stmts,
        assigns: child_assigns,
        scope: caller,
        island,
    });
    ctx.module_stack.borrow_mut().push(&mi.name);
    // Push bottom→top: the frame pop (CLEANUP), the memo store (WORK — skipped on the error drain, so an errored
    // body never stores a partial node), the body's union, the body itself. LIFO → body runs, its node lands via
    // Collect at `mark`, `CacheStoreModule` peeks it, then the frames pop. The body resolves ITS module calls
    // against the DEFINITION island (`home`) with the home global.
    let mark = results.len();
    work.push(GTask::PopModuleFrame {
        depth,
        capture: store.as_ref().map(|(_, _, entry)| *entry),
    });
    if let Some((key, snap, entry)) = store {
        work.push(GTask::CacheStoreModule { key, snap, entry });
    }
    work.push(GTask::Collect {
        mark,
        comb: Combinator::Union,
    });
    work.push(GTask::Stmt {
        stmt: body,
        scope: call,
        global: home_global,
        island: home,
    });
    Ok(())
}

/// B3 — collect the PER-ITERATION scopes of a `for`/`intersection_for` (the Cartesian product of its loop
/// vars), mirroring `for_product` but WITHOUT eval'ing the body — the driver schedules each iteration as its
/// own `EvalNodes`. The recursion is loop-VAR-deep (source-bounded, ~1-3), NOT iteration-deep (the per-value
/// loops are flat), so it stays safely on the host stack.
fn for_scopes<'a>(
    args: &'a [crate::parser::Arg],
    scope: &Scope,
    ctx: &Ctx<'a>,
    out: &mut Vec<Scope>,
) -> crate::Result<()> {
    match args.split_first() {
        None => out.push(scope.clone()), // all vars bound → one iteration scope
        Some((arg, rest)) => {
            let name = arg.name.as_deref().unwrap_or("");
            let iterable = eval_with_ctx(&arg.value, scope, ctx)?;
            for value in super::iterate_values(&iterable, ctx) {
                // child(), not clone+COW — keeps the loop bind below any capture boundary (BU.8).
                let mut child = scope.child();
                child.bind(name, value);
                for_scopes(rest, &child, ctx, out)?;
            }
        }
    }
    Ok(())
}
