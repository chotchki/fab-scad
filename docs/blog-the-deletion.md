# I deleted the JIT and the renderer got faster

Last year I built a Cranelift JIT for my from-scratch OpenSCAD reimplementation, and
[part 1](https://hotchkiss.io/blog/you-just-turn-a-knob-and-it-goes-faster-right) was mostly me
admitting it barely earned its keep. This month I deleted it. I'm deleting the ~55 hand-written
native functions next, which were the OTHER fast tier and the one I'd actually sweated over.

Both got replaced by one thing: a transpiler that runs at build time.

## What replaced them

BOSL2 is the big geometry library everybody writing real OpenSCAD leans on — ~85K lines of
excellent, genuinely hairy parametric modeling code that I did not write and would never want to
reimplement. My `build.rs` now parses it and emits Rust into `OUT_DIR`. Nothing checked in, nothing
vendored, no generated files in the repo. Bump the submodule pin, rebuild, get a new library.

It currently compiles **1322 of BOSL2's 1329 functions** (99.4%).

## Why I bought it, which was NOT speed

I want to be clear about the criterion, because I set it before I had numbers and I'm not going to
retrofit a better-sounding one. The transpiler was a MAINTENANCE bet. Every hand-written native was
a second implementation of somebody else's function that I had to keep bit-identical by hand,
forever, across their upstream changes. That's a debt that only grows, and it had already gone bad
on me — an audit found three of those natives carrying WRONG guard lists, meaning they could fire in
situations where they shouldn't have.

The mechanism that makes the transpiled tier safe is the same one, generalized: a compiled function
only wires in if the AST fingerprint of the function actually loaded matches the reference it was
compiled from, byte for byte, plus its dependencies and the builtins it calls. Edit BOSL2, or shadow
one of its functions in your own file, and it silently declines to the interpreter. The worst case
is a missed speedup. It is never a wrong answer.

## What each tier is actually worth

109 real models, release build, EVAL span only (source → geometry tree — the whole span a native
tier can even affect). Totals over the 86 models every leg finished:

| tier | eval | vs interpreter |
|---|---|---|
| interpreter | 75.1 s | 1.00x |
| hand natives | 38.2 s | 1.96x |
| hand natives + JIT | 38.6 s | 1.95x |
| transpiler | 33.4 s | 2.25x |
| what ships today | 32.9 s | 2.28x |

The JIT is worth NOTHING here and it deserves to be said plainly: 38.6 s against 38.2 s without it.
That's inside the noise and if anything slightly negative. It confirms what I'd only been able to
infer before — the JIT was 34-334x faster per CALL and still a wash on a real render, because
geometry dominates and always did.

The completion counts are the better result and reporting only the ratio would have buried them.
Every leg got an identical deterministic budget (eval STEPS, not wall time, so a model fails at the
same point on every machine). Within it, the interpreter finishes 86 of 109 models, the hand tier
94, the transpiler 97. Twelve models are renderable with a compiled tier and simply are not without
one. Which also means the 23 dropped models are exactly where the natives help MOST, so that 2.25x
understates them.

For scale on the part a user actually waits for: same corpus end to end, my renderer 78.8 s against
OpenSCAD's 281.9 s.

## Checking it against OpenSCAD instead of against myself

Here's the hole I had to go plug before I could delete anything. Every differential test in the tree
compared my compiled tier against my own INTERPRETER. That catches a tier that disagrees with
itself, and it is completely blind to a bug both tiers share — which reads as agreement. I already
had a fuzzer that asked the real OpenSCAD binary, but it only generated programs over the BUILTIN
surface (`sin`, `len`, `concat`), so it had never once called a BOSL2 function.

So I pointed that lane at the transpiled band. 200 generated programs, run through both engines:
194 matched, 6 refused on both sides, **0 diverged**.

The first run also caught me. Four seeds came back "our side failed", which the harness documented as
something a generated program must never do. On a LIBRARY surface that's just false — BOSL2 asserts
its own preconditions and a generator feeding it arbitrary values trips them constantly. Worse, the
harness was returning on our failure WITHOUT asking OpenSCAD, so those four were never compared at
all. Four unclassified seeds that could have been hiding real divergences. It asks anyway now.

## What it doesn't do

Seven functions still don't compile. I looked at all seven: four of them are upstream BOSL2 bugs
(code that can't actually run as written), so the honest ceiling is about 1325, not 1329. The other
three are mine to fix.

The eval win is also a small slice of what you wait for. Geometry is most of a render, so halving
eval moves the wall clock by a few percent on a heavy model. That was true of the hand natives and
the JIT too, and it's why I stopped judging this on ms.

And the fingerprint gate cuts both ways — a library you've patched locally gets you the interpreter,
quietly. That's the trade I wanted (never wrong beats always fast), but it does mean the speedup is
something you have rather than something you're promised.

Deleting the JIT took out 7,661 lines. The hand natives took another 3,642, and that second number
was the surprise: I'd braced for a slog and found that only THREE of the 102 registered natives were
still hand-written. Earlier phases had already migrated the other 99 one cone at a time without me
tracking the running total. The seven support modules underneath them fell over the moment those
three went.

The tests were the part worth doing carefully. Most of those natives have a transpiled twin that
still ships, so their tests didn't get deleted — they got repointed at the generated version under
the same name, which is strictly better because now the thing under test is the thing that runs. I
did briefly delete a batch I shouldn't have, put them back, and got a divergence on the first re-run
because I'd reconstructed a dependency list from memory instead of from the file. Fair.

*(full disclosure AI use: this was written with Claude, as was most of the work it describes.)*
