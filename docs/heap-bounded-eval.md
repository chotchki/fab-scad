# Heap-bounded eval (Phase M) — what's done, and the one thing that isn't

Phase M finishes the recursion-removal the explicit-stack expression evaluator started: get EVERY host-stack
consumer in the eval pipeline off the host stack, so a runtime-deep program can't SIGABRT the process. This
matters most for the web — browser stacks are tiny, and a stack-overflow class can't ship in the bet's #1
differentiator (one implementation everywhere).

There are three host-stack consumers. Two are now heap-bounded. One isn't — and the M.2 assessment is about
being honest which, and why the fix is bigger than the phase first implied.

## The three consumers

| consumer | status | mechanism |
|---|---|---|
| **Expression eval** (`a + b`, calls, comprehensions) | heap-bounded (Phase I) | explicit work-stack; depth is memory-bound, not stack-bound |
| **Tree `Drop`** (`GeoNode` / `Shape2D` / `Value`) | heap-bounded (M.1 / M.1b) | iterative drain into one worklist; children taken BEFORE the node drops, so nothing recurses |
| **Statement / geometry eval assembly** (`eval_stmt` → `eval_geometry` → `call_user_module` → `children`) | STILL host-recursive | bounded only by two guards; see below |

M.1 killed the deep-`GeoNode`/`Shape2D` Drop overflow; M.1b did the same for deep `Value` lists via a
`ValueList` newtype (the `Drop` lives on the payload, not on `Value`, so the arithmetic hot path keeps moving
`Value` fields by value). Both are proven on a 512 KiB stack at depth 200 000.

## M.2 — the assessment: eval assembly is host-recursive, and it's NOT cheap

A recursive MODULE builds geometry by recursing through the host stack, one (or more) frame per level:
`translate() r(n-1)` evaluates the transform, then recurses into `r(n-1)` WHILE the transform's frame is still
parked. Two guards bound how deep this goes:

- **`MAX_MODULE_DEPTH` = 256** — nested user-module calls. A runaway `module r() { r(); }` bails LOUD here
  instead of crashing.
- **`MAX_DEPTH` = 64** (parser) — source-nesting levels, so a single body can stack ~64 transforms.

So the worst case the guards ALLOW is ~256 × 64 ≈ 15 000 nested host frames. That's bounded — it can't run
away — but it is NOT small. Measured (eval + drop of `module r(n){ if(n>0) translate()xN r(n-1); else cube(1);}
r(255)`), smallest surviving stack:

| case (module depth × nesting) | debug | release |
|---|---|---|
| 255 × 1 (a plain deep recursion) | 32 MiB | 4 MiB |
| 255 × 8 (moderate nesting) | 64 MiB | 8 MiB |
| 255 × 60 (near the guard limit) | > 512 MiB | > 128 MiB |
| 100 × 1 (shallow) | — | 2 MiB |

The headline: this — NOT `Drop` — is what the harnesses' 1 GiB eval-stack reserve was actually protecting. The
old comments blamed "deep-tree Drop"; that was incomplete (and, post-M.1, wrong: Drop is iterative now). The
reserve stands because deep eval-assembly recursion genuinely needs ~½ GiB in debug at the guard limit, and it
happens the 1 GiB reserve is exactly the right size to cover it. See `fab_scad::EVAL_STACK`.

### The coupling the numbers expose

`MAX_MODULE_DEPTH` = 256 only SAVES you on a big stack. On a default 2 MiB thread a debug build overflows
~15 levels in — long before the guard at 256 is ever reached. So the guard and the reserve are COUPLED: the
guard converts a runaway into a LOUD error ONLY when the stack can hold its full depth. On a wasm-small stack
the guard doesn't help at all — you overflow first. That coupling is the real problem, and the reason M.2
cannot simply "drop the 1 GiB hacks": dropping the reserve today would REGRESS deep-program safety, not fix it.

## M.3 — the real fix (DONE)

Decoupling meant removing host recursion from the geometry pipeline: `eval_geometry` now runs on an explicit
work-stack driver (`lang/src/eval/geo_stack.rs`), the same treatment the expression evaluator got at Phase I.
The recursive tree-walk (`eval_stmt` / `eval_stmt_dispatch` / `call_user_module` / `eval_children` /
`for_product` / `eval_nodes`) is retired. Eval depth is now memory-bound: a 10 000-deep recursive module
evaluates on a **512 KiB stack** (`module_recursion_bound.rs`), where it needed ~32 MiB pre-M.3.

It was built and aligned as its own spec (`docs/m3-explicit-eval-spec.md`), design-reviewed by a 4-lens
adversarial pass (which caught two load-bearing encoding errors before any code), then landed incrementally —
every arm converted behind a shim and gated on the differential, culminating in an A/B soak (driver vs the
recursive path) that matched **bit-for-bit across the full BOSL2 corpus AND the models oracle-differential**
before the reference path was deleted.

Consequences that fell out:
- **`MAX_MODULE_DEPTH` demoted** from crash-safety to a runaway detector, and RAISED 256 → 100 000. The old 256
  was ~20× stricter than OpenSCAD's own ~5–8 k module-recursion limit (a compat gap); now we accept recursion
  **deeper than OpenSCAD** — it's host-stack-bound, we're heap-bounded. Same language, deeper limit.
- **`EVAL_STACK` dropped 1 GiB → 64 MiB** — eval no longer needs any reserve (heap-bounded); the modest
  remainder is courtesy headroom for the native Manifold render path, a separate subsystem.

**M.2 delivered** the assessment above + the corrected reserve rationale + the guard regression test; **M.3**
delivered the driver, the retirement, and these consequences. The wasm target is no longer a stack-size gamble.
