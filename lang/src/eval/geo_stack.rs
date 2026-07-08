//! M.3 — the explicit-stack GEOMETRY-eval driver. A PEER of the expression machine (`eval_with_global`): its
//! own work stack of [`GTask`] + result stack of [`Geo`], so statement/geometry eval depth is heap-bound, not
//! host-stack-bound (the M.2 exposure — see `docs/heap-bounded-eval.md`). The full encoding, the two
//! load-bearing corrections a 4-lens design review caught (arity-by-MARK not count; a two-class error DRAIN),
//! and the increment plan live in `docs/m3-explicit-eval-spec.md` §DECISION.
//!
//! INCREMENT 1a (this file, so far): the driver SKELETON + the fallthrough SHIM. Every statement is still
//! dispatched through the recursive [`eval_stmt`], so the driver is a transparent pass-through — the dual-path
//! bridge that lets each dispatch arm convert to a native work-stack push (A1+) independently, each gated on the
//! differential. Host recursion only actually disappears once the last recursive arm is converted.

use std::sync::OnceLock;

use super::{Ctx, Geo, Scope, Stmt, eval_stmt};
use crate::parser::StmtKind;

/// Is the explicit-stack geometry driver the active eval path? Default ON — the driver (all-shim until arms
/// convert) is behaviorally identical to the recursive path, so it is the tested path. `FAB_GEO_DRIVER=0` forces
/// the recursive path for an A/B differential. Read once (the switch can't change mid-run).
pub(super) fn driver_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("FAB_GEO_DRIVER").map_or(true, |v| v != "0"))
}

/// A resolved N-ary/unary combinator a [`GTask::Collect`] applies to its drained child `Geo`s to build ONE node
/// — the geometry analogue of the expression machine's `Apply` carrying bound arg values. Populated arm-by-arm
/// as the dispatch converts (A2+).
enum Combinator {
    /// Union the children — a bare block / implicit group. `union_of` handles the null / one / many collapse
    /// (`{}` → `Empty`, one → itself, many → `Union`) and the 2D/3D mixing resolution.
    Union,
}

impl Combinator {
    /// Apply this combinator to the child `Geo`s a [`GTask::Collect`] drained (in source order), producing ONE
    /// node — reusing the same wrap helpers the recursive dispatch uses, so the result is bit-identical.
    fn apply(self, children: Vec<Geo>, ctx: &Ctx<'_>) -> Geo {
        match self {
            Combinator::Union => super::union_of(children, ctx),
        }
    }
}

/// One step on the geometry driver's work stack. WORK tasks may eval / emit / return `Err`; CLEANUP tasks are
/// infallible ctx side-effects that MUST run on both the happy AND the error-drain path (LIFO), so the driver
/// keys its error handling on this WORK/CLEANUP split, never on a name or a push/pop heuristic.
#[allow(dead_code, reason = "M.3 increment 1a: only Stmt is constructed yet; the rest land with A1+ arms")]
enum GTask<'a> {
    /// WORK — dispatch ONE statement (increment 1a: always the shim → recursive `eval_stmt`).
    Stmt {
        stmt: &'a Stmt,
        scope: Scope,
        global: Scope,
        island: usize,
    },
    /// WORK — expand a statement list under a fresh mark: hoist, push-if-any local modules, record
    /// `mark = results.len()`, push the paired `Collect{mark, comb}`, then the child `Stmt`s in REVERSE source
    /// order (so they land in source order), then `PopLocalModules` iff pushed. `stmts` is the raw AST slice
    /// (children are `Vec<Stmt>` in the AST, lifetime `'a`), collected to `&[&Stmt]` only for the hoist helpers.
    EvalNodes {
        stmts: &'a [Stmt],
        scope: Scope,
        global: Scope,
        island: usize,
        comb: Combinator,
    },
    /// WORK — drain `results.split_off(mark)` (exactly what this block pushed, 0-or-1 per child stmt) and apply
    /// `comb`, pushing ONE `Geo`. (A2+.)
    Collect { mark: usize, comb: Combinator },
    /// WORK — the `!` root modifier: drain `results.split_off(mark)` INTO `ctx.root_override` (consumes), so the
    /// parent's `Collect` legitimately sees zero there. Discarded on the error path. (A7.)
    CaptureRoot { mark: usize },
    /// CLEANUP — pop a scope-local module store pushed by an `EvalNodes` that had local defs. (A2+.)
    PopLocalModules,
    /// CLEANUP — the three-frame user-module pop: restore `module_depth` from the pre-call SNAPSHOT (never a
    /// decrement — it's a `Cell`), then pop `module_stack` + `children_stack`. (Increment 2.)
    PopModuleFrame { depth: usize },
    /// CLEANUP — re-push a `ChildrenFrame` that `children()` transiently popped, keeping `children_stack`
    /// balanced across both paths. (Increment 2.)
    RestoreChildrenFrame(super::ChildrenFrame<'a>),
}

/// The top-level geometry entry: drive a PRE-HOISTED statement list (as [`eval_geometry`](super::eval_geometry)
/// is always called — `run_stmts` publishes the global, `eval_nodes` hoists the child scope) and return the RAW
/// top nodes (the caller — `run_stmts` or a combinator — applies any union / root-override). Increment 1a: each
/// statement shims to the recursive `eval_stmt`, so this is a transparent peer of the recursive loop.
pub(super) fn eval_geometry_driver<'a>(
    stmts: &[&'a Stmt],
    scope: &Scope,
    global: &Scope,
    island: usize,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Geo>> {
    let mut work: Vec<GTask<'a>> = Vec::with_capacity(stmts.len());
    // Reverse-push so the FIRST statement is on top of the stack and pops first → source order preserved.
    for stmt in stmts.iter().rev() {
        work.push(GTask::Stmt {
            stmt,
            scope: scope.clone(),
            global: global.clone(),
            island,
        });
    }
    let mut results: Vec<Geo> = Vec::new();
    drive(work, &mut results, ctx)?;
    Ok(results)
}

/// The driver loop. Pops tasks to empty. CLEANUP tasks run their ctx side-effect on BOTH paths (LIFO). On the
/// FIRST `Err` from a WORK task the driver captures it and enters DRAIN: it keeps popping so the parked CLEANUP
/// tasks still fire (balancing ctx), but DISCARDS every WORK task without executing it — reproducing the
/// recursive path's "first `?` wins, no later side effect" (a re-dispatching drain would emit phantom echoes /
/// run later asserts → a different error + message stream, observable today).
fn drive<'a>(
    mut work: Vec<GTask<'a>>,
    results: &mut Vec<Geo>,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
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
        GTask::PopModuleFrame { depth } => {
            // Restore from the SNAPSHOT — never `set(get()-1)`: module_depth is a Cell<usize>, and a decrement
            // during a mis-built drain would underflow.
            ctx.module_depth.set(depth);
            ctx.module_stack.borrow_mut().pop();
            ctx.children_stack.borrow_mut().pop();
        }
        GTask::RestoreChildrenFrame(frame) => {
            ctx.children_stack.borrow_mut().push(frame);
        }
        _ => unreachable!("run_cleanup only reached for CLEANUP tasks"),
    }
}

/// Dispatch one WORK task. Increment 1a: only [`GTask::Stmt`] is ever constructed, handled by the shim; the
/// native expansions (`EvalNodes`/`Collect`/`CaptureRoot`) land as their dispatch arms convert (A1+).
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
            let refs: Vec<&Stmt> = stmts.iter().collect();
            let hoisted = super::hoist_scope(&refs, &scope, ctx)?;
            let local_mods = super::collect_module_defs(&refs);
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
            for stmt in stmts.iter().rev() {
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
        GTask::CaptureRoot { .. } => {
            unimplemented!("M.3 A7: the `!` root modifier converts here")
        }
        GTask::PopLocalModules | GTask::PopModuleFrame { .. } | GTask::RestoreChildrenFrame(_) => {
            unreachable!("CLEANUP tasks are handled in the driver loop, not dispatch_work")
        }
    }
}

/// Dispatch ONE statement: a converted arm pushes native work-stack tasks; every still-recursive arm falls
/// through to the [`shim_stmt`] bridge. Arms convert leaf → simple (A1+), each gated on the differential.
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
                stmts: stmts.as_slice(),
                scope,
                global,
                island,
                comb: Combinator::Union,
            });
            Ok(())
        }
        _ => shim_stmt(stmt, scope, &global, island, ctx, results),
    }
}

/// The fallthrough SHIM: run ONE statement through the existing recursive `eval_stmt` into a scratch vec, then
/// push its result(s) onto the shared result stack. A geometry statement produces 0 or 1 `Geo` (defs /
/// assignments / disabled-`*%` / childless echo-assert push nothing; a `!`-node diverts into `root_override`
/// inside `eval_stmt`, so the scratch stays empty for it). This is the dual-path bridge — behaviorally
/// identical to the recursive `eval_geometry` loop, so an unconverted arm keeps the exact recursive semantics.
fn shim_stmt<'a>(
    stmt: &'a Stmt,
    scope: Scope,
    global: &Scope,
    island: usize,
    ctx: &Ctx<'a>,
    results: &mut Vec<Geo>,
) -> crate::Result<()> {
    // Skip assignments the way eval_geometry does (they hoist; eval_stmt no-ops them anyway — this just avoids
    // the call). Everything else dispatches recursively into a scratch vec.
    if matches!(stmt.kind, StmtKind::Assignment { .. }) {
        return Ok(());
    }
    let mut scope = scope;
    let mut scratch: Vec<Geo> = Vec::new();
    eval_stmt(stmt, &mut scope, global, island, ctx, &mut scratch)?;
    results.extend(scratch);
    Ok(())
}
