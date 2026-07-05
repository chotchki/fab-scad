# The float-discipline JIT recipe (I.8)

The `fab-jit` spike JITs a numeric OpenSCAD function through Cranelift and proves it BIT-IDENTICAL to
the interpreter — `fast == JIT`, the sibling of `fast == slow`. This doc banks the recipe that made
that hold, because it's the evidence the Phase-L JIT-vs-intrinsics PROMOTE decision runs on.

## What the spike measured

On a Horner-form degree-5 polynomial, 2M calls: the JIT ran **~189x** faster than the interpreter
(native code vs. a tree-walk that boxes every intermediate into a `Value` and threads a scope). That's
the ceiling for a hot numeric inner loop — the kind BOSL2's VNF/bezier math is full of — not a
whole-program number (geometry dominates those). And every call was bit-for-bit equal to the
interpreter across the corpus (polynomials, all four arithmetic ops, unary neg, nesting, `%`/`^`,
2-param functions) plus a coefficient-proptest over generated quadratics.

## The recipe — four rules that keep the bits identical

1. **No auto-FMA.** Cranelift does NOT contract `fmul`+`fadd` into a fused-multiply-add — that fusing
   is an LLVM fast-math behavior, and Cranelift emits the instructions it's asked for, in order. So
   `a*b + c` compiles to a separate multiply then add, exactly like the interpreter. This is the rule
   most likely to silently break determinism elsewhere, and `fast == JIT` is its PROOF: a fused
   `a*b+c` rounds once instead of twice, which flips a bit on some inputs and fails the test.

2. **Fixed evaluation order.** The IR mirrors the interpreter — left operand then right, no
   reassociation. (For vectors the 4-lane fixed-accumulation order carries over the same way; the
   scalar spike doesn't exercise it yet.) The operation is what matters, not which subexpression is
   evaluated first — `fadd(a, b)` is deterministic given `a` and `b` — but reassociating a SUM would
   change the rounding, so the compiler never does.

3. **Ops with no deterministic native instruction → CALLS into OUR Rust math.** `%` and `^` have no
   Cranelift float instruction (float remainder and pow are library routines), so they compile to
   calls into `jit_fmod`/`jit_powf` — which run the interpreter's EXACT ops (`a % b`, `a.powf(b)` from
   `ops.rs`). Never a Cranelift intrinsic, never the platform libm with its own rounding. The
   transcendentals (sin/cos/… when they land) follow the same rule: route them to `trig.rs`, don't let
   codegen pick a vectorized approximation. This is the whole reason the non-arithmetic ops stay
   bit-identical.

4. **One unsafe seam.** The ONLY `unsafe` is calling the finalized code pointer (`JitFn::call`).
   Everything else — building IR, defining the function, finalizing — is safe. That's why the JIT is a
   separate native crate: fab-lang stays `unsafe_code = forbid`, and the browser (which can't JIT
   in-sandbox anyway) never links this.

## Cranelift usage notes (for the next person)

- Depend on the umbrella `cranelift` crate with `features = ["jit", "module", "native"]`, not the
  individual sub-crates — the prelude re-exports the ergonomic surface. In particular the flags type
  the instruction builders take is **`MemFlagsData`** (`::trusted()` / `::new()`), re-exported from
  `cranelift::prelude`; the bare `cranelift_codegen::ir::MemFlags` is an internal packed handle with no
  `new()`, which is a confusing dead-end if you import it directly.
- The generated function is `extern "C" fn(*const f64) -> f64` (params passed as an array pointer, one
  uniform signature for any arity); parameter `i` is a load at offset `i*8`.

## Scope + what it declines

The compiler handles number literals, parameter reads, unary `-`/`+`, and `+ - * / % ^`. Anything else
— a call, a ternary, indexing, a free (non-parameter) variable — returns `JitError::Unsupported`. It
DECLINES; it never emits a wrong answer. A real integration falls back to the interpreter for those.

## What this feeds (Phase L)

The promote question is JIT vs. hand-written intrinsics for the hot numeric paths. This spike settles
the JIT side's two open risks: it's determinism-safe (the recipe above, proven bit-identical) and the
speedup on hot numerics is large (~189x). What it does NOT settle — and Phase L must weigh — is that
the JIT is native-only (the wasm/browser target still runs the interpreter, so a JIT'd path is a
second implementation to keep in sync), and that per-function compile latency + the leaked `JITModule`
per function need a real caching/pooling story before this is more than a spike.
