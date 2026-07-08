# M.3 — explicit-stack eval assembly — SPEC

Status: **ALIGNED + REVIEWED** — chotchki decisions folded in 2026-07-07 (§Decisions); a 4-lens adversarial
design review (`m3-design-review` workflow) pressure-tested the encoding against the real code and CORRECTED two
load-bearing errors — see §DECISION below, which is the AUTHORITATIVE implementation checklist (the prose above
it is the pre-review reasoning, kept for context; where they disagree, §DECISION wins). This is the "how" for
the fix the M.2 assessment (`docs/heap-bounded-eval.md`) said is needed: get statement/geometry eval off the
host stack so eval depth is memory-bound, the harnesses drop the 1 GiB reserve, and the wasm target stops being
a stack-size gamble.

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

3. **Module depth DEMOTES from crash-guard to runaway-detector.** Once eval is memory-bound, the ONLY hard
   limit left is heap/OOM (chotchki's Q2 — confirmed: post-M.3 there is no non-heap ceiling; Drop is iterative
   too, so even tearing down a giant tree is fine). `MAX_MODULE_DEPTH` stops being load-bearing crash-safety and
   becomes purely a runaway DETECTOR — it turns infinite recursion (`module r(){r();}`) into a fast LOUD error
   instead of a slow crawl to OOM. KEEP one for that UX (OpenSCAD does, same reason), but it's now a policy
   knob, not a safety wall. Raising it is safe post-M.3; a memory/step budget could replace it later.

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

## Decisions (chotchki, 2026-07-07)

1. **Incremental, dual-path behind a switch → YES.** "This is where our test harness is going to save us" — land
   the driver alongside the recursive code, convert arm-by-arm, and diff each increment against the recursive
   path on the corpus + models. The differential is the safety net; lean on it rather than a big-bang cut.
2. **`MAX_MODULE_DEPTH` demotes to a runaway-detector** (see hard-part #3 + Q2). Post-M.3 heap is the only limit,
   so the guard is no longer crash-safety — keep a limit for the fast-LOUD-error UX, value revisitable/raisable.
3. **M.3 stays IN Phase M** (the tail box), not its own phase — "we just add on to M."

---

## DECISION — reviewed encoding (2026-07-07, `m3-design-review` — AUTHORITATIVE)

Four adversarial reviews (error-unwind / children-latebinding / scope-order-root / mirror-exprmachine) trilaterated
the SAME three corrections to the draft. Spec SHAPE confirmed (a PEER driver mirroring the expression machine,
post-order push-continuation-then-children); encoding revised where flagged. Line refs are `lang/src/eval/mod.rs`.

### Two peer drivers, two stacks — do NOT merge
`run_geometry(root) -> Result<Vec<Geo>>` is a PEER of the expr driver, not a reuse: its own `Vec<GTask>` work
stack + `Vec<Geo>` result stack. Geometry-node ARGS stay synchronous nested expr-machine runs (`eval_args`,
`bind_module_scope`, the `if`-cond, the `for`-iterable) — host-recursion bounded by 1, not tree depth, so args are
NOT the target surface. Do NOT unify Value/Geo into one stack. Result stack holds ONE `Geo` per push (NOT
`Vec<Geo>` batches — batches reintroduce the arity ambiguity the mark exists to kill).

### The GTask enum
```rust
enum GTask<'a> {
    // pop → hoist EAGERLY (hoist_scope, non-publishing); push-if-any local mods; record mark=results.len();
    // push Collect{mark,comb} FIRST, then child Stmt tasks in REVERSE source order, then PopLocalModules IFF pushed.
    EvalNodes { stmts: &'a [&'a Stmt], scope: Scope, global: Scope, island: usize },
    Stmt      { stmt: &'a Stmt, scope: Scope, global: Scope, island: usize },
    Collect   { mark: usize, comb: Combinator },   // WORK: drains results.split_off(mark), applies comb, pushes ONE Geo
    CaptureRoot { mark: usize },                    // WORK: `!` — drains split_off(mark) INTO ctx.root_override (consumes)
    PopLocalModules,                                // CLEANUP
    PopModuleFrame { depth: usize },                // CLEANUP: set(depth); module_stack.pop(); children_stack.pop()
    RestoreChildrenFrame(ChildrenFrame<'a>),        // CLEANUP: re-PUSHES the owned frame
}
```
`Combinator` carries RESOLVED data (Affine matrix, BoolKind, resolved offset/extrude params, `UserModule`) — the
analogue of the expr machine's `Apply` carrying bound arg values; args eval in the caller scope BEFORE children.

### CORRECTION 1 — arity by MARK, not static count (the draft's biggest error)
A geometry `Stmt` pushes **0 OR 1** `Geo`: assignments/empties/defs no-op (@2102), `*`/`%` disabled push nothing
(@2123), childless echo/assert push nothing (@2137), `!` DIVERTS (@2072). So N ≠ stmt count. Every `EvalNodes`
records `mark = results.len()` AT POP TIME (after the eager hoist, so sibling/parent results already sit below);
its `Collect` does `results.split_off(mark)` — exactly what THIS block pushed. A count-based pop steals the
parent frame's Geo → silent CSG corruption. Marks are read at task-POP time, never at scheduling time.

### CORRECTION 2 — two-class error drain, not "fire the pop-tasks"
Key error handling on task CATEGORY, never a name/heuristic:
- **WORK** = {EvalNodes, Stmt, Collect, CaptureRoot} — may eval/emit/`Err`.
- **CLEANUP** = {PopLocalModules, PopModuleFrame, RestoreChildrenFrame} — pure ctx side-effects, infallible.

Driver holds `first_err: Option<Error>`. Happy path: pop, dispatch; CLEANUP runs its side-effect inline. On the
FIRST `Err` from a WORK task: capture once, enter DRAIN. DRAIN: keep popping; run CLEANUP side-effects inline in
stack order (LIFO → innermost-first); DISCARD every WORK task WITHOUT executing (no handler re-entry, no expr
eval, no `ctx.messages` write); stack empty → return `first_err`. This is first-error-wins for the Err VALUE **and
the message stream** — a re-dispatching drain would emit phantom echoes + evaluate later asserts → a DIFFERENT
error, observable TODAY. The expr driver's bare-`?`-and-drop is safe ONLY because it mutates no LIFO ctx
(`ctx.closures` is append-only); the geometry machine can't inherit that shortcut.

`PopModuleFrame` restores via the pre-call `depth` SNAPSHOT (read @1644 before the +1) → `module_depth.set(depth)`,
NEVER `set(get()-1)` (`module_depth` is a `Cell`, @150 — a decrement mid-drain underflows `usize`). `PopLocalModules`
is queued from an `EvalNodes` ONLY when that block actually pushed local modules (mirror `pushed` @1308).

### CORRECTION 3 — child ORDER: reverse-push (mirror `VectorSplice` @578)
`EvalNodes` pushes `Collect` FIRST (bottom), then child `Stmt`s in REVERSE source order, so they resolve
front-to-back and land on the result stack in source order; `Collect` recovers via split_off. Forward push
silently inverts every ordered combinator — `difference()` becomes last-minus-first. Same for `for`/`intersection_for`.

### children() — dedicated path, NOT a generic combinator (increment 2)
The `children` Stmt handler is eager up to the pop, then schedules: (1) eval the index arg in the CURRENT scope;
(2) POP the frame off `children_stack` into a local (@1703 — the transient pop is what makes a nested `children()`
bind to the ENCLOSING frame; skip it and `outer(){inner() children();}` infinitely regresses); (3) compute
`selected`; (4) push bottom→top: `RestoreChildrenFrame(frame)`, `Collect{mark, Union}`, `EvalNodes{ selected,
scope: Scope::call_frame(&frame.scope, current_scope), global: CURRENT global, island: frame.island }`. Three
DISTINCT threaded fields: render scope = `call_frame(frame.scope, current)` (frame's scope LEXICAL parent, current
scope the DYNAMIC `$`-overlay so `$parent_geom` reaches children); global = the CURRENT global (ChildrenFrame has
NO global field — do NOT source it from frame/callee); island = `frame.island`. `RestoreChildrenFrame` is CLEANUP
so it re-pushes on both paths, keeping `children_stack` balanced under error.

### call_user_module — setup EAGER, three-frame pop as ONE CLEANUP (increment 2)
Keep the setup eager + ordering-sensitive: bind `$children`, push `ChildrenFrame` (@1668), bind `$parent_modules`
from `module_stack.len()` read BEFORE the self-push (@1676), `module_stack.push`, bump `module_depth`. THEN push
bottom→top: `PopModuleFrame{depth}`, `Collect{mark, UserModule}`, `Stmt(body, call, home_global, home)` (the body
is a single Stmt @1683, carrying home/home_global — asymmetric to the caller-scope stashed frame). Do NOT defer the
module_stack push or the `$`-binds — they're ordering-sensitive reads that must precede the body.

### `!` root modifier
In the Stmt handler, a `Module` with `modifiers.root`: read `mark = results.len()`, push `CaptureRoot{mark}` FIRST,
then the normal dispatch expansion on top. LIFO → subtree resolves + pushes its Geo, then CaptureRoot drains
split_off(mark) into `root_override`, so the parent Collect's mark legitimately sees zero there (matching the
untouched `nodes` today). CaptureRoot is WORK — discarded on error before the subtree resolves (mirrors the
`?`-before-split_off @2071). `run_stmts`' `split_off(0)`/`is_empty` swap @1285 is unchanged.

### for_product (increment 2)
Keep the loop-var recursion on the host (source-bounded, pure expr evals). At the leaves, eagerly build the Vec of
per-iteration child scopes, then push ONE `Collect{mark, Union|Intersection}` + N `EvalNodes` in REVERSE product
order — the body eval MUST go through the work stack or `for(i=[0:254]) rec(255)` reopens the host-stack hole.

### Scope threading
Per-task `scope: Scope` BY VALUE is faithful — the `&mut scope` in eval_geometry/eval_stmt is vestigial (binds go
into child scopes; assignments are hoisted, so siblings never observe each other's mutation). `Scope` is `Rc<Frame>`
(cheap clone). Use `hoist_scope` (non-publishing) for blocks; `hoist_scope_publishing` stays ONLY at island roots
(@1277) — a publishing hoist mid-block corrupts `island_globals`.

## Increment plan (ordered, each step independently differential-testable)

**Scaffolding first** (the mark / taxonomy / reverse / drain disciplines are STRUCTURAL — must exist before any arm):
- **S1** — `GTask` / `Combinator` enums; `Vec<Geo>` result stack, one Geo per push.
- **S2** — driver `run_geometry`: pop/dispatch, `first_err`, DRAIN mode, CLEANUP-on-both-paths, `split_off(mark)`
  Collect, a reverse-push expansion helper. No arms yet.
- **S3** — fallthrough SHIM: an unconverted `StmtKind` calls the existing recursive `eval_stmt` into a scratch
  `Vec<Geo>` and pushes the results — the dual-path bridge that lets each arm convert independently.
- **S4** — route `run_stmts`/`eval_nodes` through the driver behind an internal switch (env/cfg); wire the
  differential to run switch-OFF vs switch-ON and compare whole-program output + message stream + Err string on
  corpus + models. Green = scaffolding + shim are transparent.

**Then arm conversions, leaf → simple** (each: flip one arm from shim to native push, re-run the differential):
- **A1** — no-op + leaf arms (Empty/FunctionDef/ModuleDef/Assignment/disable+background = 0 push; builtin-primitive
  `eval_module` = 0 or 1). Establishes the 0-or-1-push reality against the mark.
- **A2** — bare `Block` → `EvalNodes + Collect{Union}`, `PopLocalModules` gated on local-mod presence.
- **A3** — echo/assert passthrough → side-effect inline, then `EvalNodes+Collect{Union}` IFF children non-empty
  (exercises childless-0-push + first-error-wins with no cleanup parked — simplest drain).
- **A4** — Transform → `EvalNodes+Collect{Transform(matrix)}` (matrix resolved eagerly).
- **A5** — Boolean (union/difference/intersection) + hull + minkowski — the REVERSE-push order gate.
- **A6** — offset / linear_extrude / rotate_extrude / projection / color / `let`-stmt / `if` → single-child
  `EvalNodes+Collect{...}` with eagerly-resolved params / branch selection.
- **A7** — `!` root modifier → `CaptureRoot{mark}` before the dispatch expansion.

Increment 1 EXCLUDES call_user_module, children(), for/intersection_for (increment 2: `PopModuleFrame`,
`RestoreChildrenFrame`, eager product expansion, the `module_recursion_bound.rs` cases on a default stack).

## Top-5 invariants to assert (from the highest-confidence findings)

1. **Mark-drain arity** — `union(){ x=1; *cube(1); echo("h"); sphere(2); }` → the union's Collect drains exactly
   ONE Geo; bit-identical to recursive.
2. **Ordered-combinator source order** — `difference(){ cube(10); translate([5,0,0]) cube(10); }` bit-identical
   (first-minus-rest, not last-minus-first); same for a >1-child union/hull.
3. **Frame balance across error** — `module r(n){ if(n>0) r(n-1); else assert(false); } r(200);` → after the drain
   `module_depth==0`, children/module stacks empty, no panic; plus late-bind non-regress
   `module inner(){children();} module outer(){inner() children();} outer() cube();` → exactly one cube.
4. **First-error-wins, no phantom side-effects** — `module a(){ assert(false,"first"); } union(){ a(); echo("leaked");
   assert(false,"second"); }` → Err is "first", "leaked" NEVER appears in messages.
5. **`!` root divert + error skip** — `translate([10,0,0]){ cube(1); !sphere(2); }` → output is the UNtransformed
   sphere only; and `!union(){ cube(1); assert(false); }` → Err with `root_override` left empty.
