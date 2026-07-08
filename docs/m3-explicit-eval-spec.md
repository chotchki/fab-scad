# M.3 — explicit-stack eval assembly — SPEC (DRAFT, for alignment)

Status: **DRAFT** — chotchki to redline before any code. This is the "how" for the fix the M.2 assessment
(`docs/heap-bounded-eval.md`) said is needed: get statement/geometry eval off the host stack so eval depth is
memory-bound, the harnesses drop the 1 GiB reserve, and the wasm target stops being a stack-size gamble.

## The goal, stated as a contract

Evaluating a geometry program must NOT consume host stack proportional to the program's runtime tree depth —
same guarantee the expression evaluator already gives (Phase I) and tree `Drop` now gives (M.1/M.1b). After
M.3: a 255-deep recursive module evaluates on a default 2 MiB stack (and a wasm-small stack), `MAX_MODULE_DEPTH`
stops being coupled to a giant reserve, and `fab_scad::EVAL_STACK` drops to a default. Bit-identical output to
today — this is a control-flow rewrite, NOT a semantics change (the differential + corpus stay green).

## What actually recurses (the target surface)

The geometry pipeline is a uniform POST-ORDER tree walk. Every geometry-producing arm has the same shape:
evaluate the children, THEN wrap the result. The chain is:

```
eval_nodes → eval_geometry → eval_stmt → eval_stmt_dispatch → [geometry arm] → eval_nodes → …
                                                                  └─ eval_nodes(children); wrap(children)
```

- `eval_nodes(stmts, scope, global, island)` — hoist a fresh scope, push scope-local module defs, run the body,
  pop. Returns `Vec<Geo>`.
- `eval_geometry` — loop the stmts, `eval_stmt` each, accumulate into `nodes`.
- `eval_stmt_dispatch` — one arm per `StmtKind` / builtin module. The recursive arms (transform, boolean, hull,
  minkowski, block, echo/assert-passthrough, offset/extrude/projection, `if`, `for`, user-module call) ALL do
  `let kids = eval_nodes(children)?; nodes.push(wrap(kids))`.
- `call_user_module` — the one arm that also pushes/pops the children stack + module stack + module depth around
  the body eval, and binds a fresh param scope.

So the whole thing is: **descend into a stmt list, collect child `Geo`s, apply ONE combinator.** That
uniformity is what makes an explicit stack tractable — most arms differ only in the combinator.

## Proposed encoding — a work-stack of frames + a value stack of results

Mirror the expression machine. Two stacks:

- **Work stack** of `GTask` (geometry tasks), popped to drive the walk.
- **Result stack** of `Geo` (or `Vec<Geo>` batches), the post-order outputs a `Combine` consumes.

```
enum GTask<'a> {
    // Expand a stmt list into: (its own scope prep) + a Descend per non-assignment stmt + a Collect that
    // pops the N child results and pushes ONE grouped Geo (or a raw batch for the parent Combine).
    EvalNodes { stmts: &'a [&'a Stmt], scope: Scope, global: Scope, island: usize, local_mods_pushed: bool },
    // One statement. For a leaf (primitive) → eval inline, push a Geo. For a recursive arm → push a Combine
    // continuation, then an EvalNodes for its children.
    Stmt { stmt: &'a Stmt, scope: Scope, global: Scope, island: usize },
    // Post-order: pop `arity` child results, apply the combinator, push the resulting Geo.
    Combine(Combinator),
    // LIFO bookkeeping unwinds — fire the matching pop even on the error path.
    PopLocalModules,
    PopModuleFrame,          // children_stack + module_stack + module_depth, as call_user_module does today
    CaptureRoot { mark: usize },  // the `!` root-override split_off
}

enum Combinator {
    Transform(Affine), Boolean(BoolKind), Hull, Minkowski, Union, Intersection,
    Offset(..), Extrude(..), Projection(..), UserModule(..), …  // one per wrap() today
}
```

The driver loop pops a `GTask`, and where the current code recurses it instead PUSHES (the combinator first,
then the children task, so the children resolve before the combine — LIFO). Expression eval stays as-is (it's
already explicit-stack and runs to completion inside a frame — args are expressions, so `eval_args` needs no
change).

## The hard parts (where the alignment matters)

1. **LIFO bookkeeping must survive errors.** `call_user_module` and `eval_nodes` push onto `ctx` (children
   stack, module stack/depth, local modules) and pop after — TODAY guaranteed by running the pop with no `?`
   before it. On an explicit stack the pop is a `PopModuleFrame` / `PopLocalModules` task queued AFTER the
   body. On error we must drain the work stack running ONLY the pop tasks (skip the rest) so `ctx` is restored.
   Proposal: on `Err`, unwind the work stack firing pop-tasks and discarding the others, then return the error.
   This is the fiddliest invariant — it's the thing most likely to leak `ctx` state if done wrong.

2. **Late-bound `children()`.** `eval_children` renders the caller's stashed children LATE, POPPING the current
   children frame for the duration (so a `children()` inside rendered children refers to the ENCLOSING call),
   then restoring. This is a recursion INTO a different scope/island mid-combinator. It stays expressible (push
   an EvalNodes for the stashed stmts with the stashed scope/island + a frame save/restore pair), but it's the
   subtlest control flow and needs its own test focus.

3. **Module depth as the safety net becomes redundant — but keep it.** Once eval is memory-bound,
   `MAX_MODULE_DEPTH` no longer prevents a crash (there's no crash to prevent). It still bounds runaway
   recursion into a LOUD error rather than an OOM, so KEEP it as a policy limit — but its value can be revisited
   (OpenSCAD's is higher). Decision for chotchki: keep 256, or raise it now that it's cheap?

4. **`$`-context + island threading.** Each frame carries `scope` / `global` / `island`; user-module calls
   swap in the home-island global and a fresh param scope, transforms drop the child scope. All of that is
   per-frame data on the `GTask`, so it threads fine — but it's a lot of state to move by value (Scope is an
   `Rc<Frame>` clone, cheap; still, verify no accidental deep clone).

## Strategy — incremental, differential-gated

Do NOT big-bang the whole dispatcher. Proposed order:

1. Land the driver loop + `GTask`/`Combine` scaffolding alongside the recursive code, behind an internal switch,
   so both can run and be compared.
2. Convert the LEAF + simple-combinator arms first (primitives, transform, union/boolean, block). Diff against
   the recursive path on the corpus + models after each.
3. Convert `call_user_module` + `children()` (the hard part) with dedicated tests, incl. the M.2
   `module_recursion_bound.rs` cases now passing on a DEFAULT stack.
4. Delete the recursive path + the switch. Drop `EVAL_STACK` to a default; delete the 1 GiB reserves.
5. Exit: `module_recursion_bound.rs` (and a new deep-nesting case) pass on a 2 MiB stack AND a wasm-small stack;
   differential + corpus bit-identical; harnesses on a default stack.

## Alternatives considered (and why not)

- **Just lower `MAX_MODULE_DEPTH` to fit a small stack.** Rejected — the product `MAX_MODULE_DEPTH × MAX_DEPTH`
  would have to drop so far (~15 levels to fit wasm) it breaks legal BOSL2 recursion. Doesn't fix the class.
- **Raise the wasm stack at link time.** Rejected — even the release near-limit worst case is >128 MiB; you
  can't reserve that in a browser, and it only moves the cliff.
- **Leave it; rely on the guard + reserve.** Rejected — that's the status quo, and it makes the web target
  (the bet's #1 differentiator) a stack-size gamble. M.2 exists to say this isn't good enough.

## Open questions for chotchki

1. Incremental-with-a-switch vs a clean cut — worth the temporary dual-path complexity, or convert in one PR
   behind heavy differential coverage?
2. `MAX_MODULE_DEPTH` — keep 256 as the policy limit, or raise it once crash-safety no longer depends on it?
3. Is M.3 its own phase (it's sizeable), or does it stay the tail of Phase M? (Phase M can't formally exit with
   an open box.)
