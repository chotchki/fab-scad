# Tasks

## TA. Windows packaging: the build that stopped fitting in a runner

- [x] TA.1 Windows release builds exceed the 6-hour ceiling since the band became real
  - ROOT CAUSE, measured on a real Windows box (64 GB, rustc 1.97.1 / LLVM 22.1.6 — the toolchain
    CI resolves) after the CI probe could only ever report silence. It was never a hang:
    - `fab_bosl2`'s rustc ran **4h17m at exactly 1.000 core sustained**, growing to and plateauing
      at **28.4 GB resident**, and had still not finished. `readMB`/`writeMB` never moved after the
      first minute — not I/O, not paging (26 GB stayed free).
    - `cargo check --release -p fab-bosl2` = **2m03s**. The frontend was never the cost; 100% of it
      is LLVM codegen.
    - The band is 7.01 MB of generated Rust, and `regular_polyhedron_info` alone transpiled to a
      **1,213,327-byte function whose body was a single 1,029,620-byte expression** — 17% of the
      whole band in one body. **A function is the atomic unit of LLVM optimisation**: it cannot be
      split across codegen units or across threads, so no `codegen-units` setting and no number of
      cores can divide it. LLVM went superlinear on it.
  - THE FIX (`lib/src/emit.rs`): the emitter now measures every sub-expression and, past 32 KB,
    hoists it into an `#[inline(never)]` helper `fn` taking the in-scope locals by value. Greedy and
    bottom-up, so a subtree that crossed the threshold is already a short call by the time its parent
    is measured. `#[inline(never)]` is the load-bearing half — a single-call-site helper is exactly
    what LLVM's inliner folds straight back in, which would reassemble the huge body and leave only
    the clones behind. Outlining fires 32 times across the whole band.
    - Largest function **1,213,327 → 171,567 B** (7.1x); longest line 1,029,620 → 170,505.
      Functions ≥200 KB: 2 → 0. Coverage unchanged: 1322 of 1329 compiled, same 7 declines.
    - `fab_bosl2` rustc: **4h17m unfinished / 28.4 GB → ~530s CPU / 1,970 MB peak**. macOS builds
      the same crate in 816.5s, so Windows is now FASTER than macOS on it, at 1/8th the runner's
      memory. Full `cargo build --release` of the crate: **7m11s**.
    - Acceptance suite green after the change — 27 passed / 0 failed across all 7 bosl2 test
      binaries, `dispatch_diff` + `surface_diff` + the generated-program differential included. The
      AR.2 contract ("compiled tier == interpreter") still holds.
  - CORRECTED, because the earlier entry asserted these and they are wrong:
    - NOT built twice per release. That reading came from the probe running two separate `cargo`
      steps, which cannot share a unit; `release-native` uses ONE invocation with both `-p` flags.
    - The "70x+" gap was inferred from the release job's silence, not measured. The probe bounds it
      only at ≥2.9x. The real factor is unbounded — the build never terminated.
    - "THE ANOMALY: 4 cores to macOS's 3, so Windows should be FASTER" dissolves entirely. Core
      count is irrelevant when the critical path is one LLVM function on one thread.
    - Windows Defender is EXONERATED by measurement, not by argument: 763.9s of user CPU across the
      whole 4.5-hour window, and rustc's I/O counters were frozen throughout. No exclusion needed.
    - Memory pressure was the right instinct at the wrong magnitude — not paging within 16 GB, but a
      process that wanted 28.4 GB.
    - NOT a v1.2.0→v1.3.0 regression. v1.2.0 passed in 5m30s because that workflow lacked
      `submodules:`, so BOSL2 was empty and the band compiled nothing. v1.3.0 is the first Windows
      build that ever compiled a real band.
  - `opt-level`/`codegen-units` on fab-bosl2 were never reached, and should stay unreached: they
    trade away the transpiler's entire point, and the defect was in what we handed LLVM.
  - SECOND Windows-only defect, found by the same box and fixed alongside: `fab-mcad`'s build script
    died with STATUS_STACK_OVERFLOW (0xc00000fd). A build script runs on the process's MAIN thread,
    whose stack is fixed at LINK time — 1 MiB on `x86_64-pc-windows-msvc` against 8 MiB on macOS and
    Linux — and the transpiler walks the AST by recursion. So it had always been running on an
    eighth of the headroom on exactly one platform. Everything else in the workspace that recurses
    deeply already runs on an explicit stack (`fab_scad::EVAL_STACK`, 64 MiB, fourteen call sites);
    build scripts were the layer nobody had reached. `fab_lib::build::transpile` now spawns the work
    on a scoped 64 MiB thread, so every library crate inherits it rather than each build script
    remembering. TA.1's outlining made it worse (`expr` -> `expr_inner` adds a frame per nesting
    level) but did not create it — which is why it reproduced intermittently, not always.
- [x] TA.3 CI HAD NO WINDOWS RUNNER, and that is why both TA.1 defects shipped
  - `ci.yml`'s five jobs were macos-latest, ubuntu-latest, ubuntu-latest, macos-latest,
    ubuntu-latest. Windows was compiled ONLY by `release-native`, which fires on a `v*` tag — so the
    first Windows compile of any change happened after it had already been tagged for release. Both
    TA.1 defects (the LLVM blowup and the 1 MiB build-script stack) are Windows-only, and neither
    was reachable by any gate.
  - The tag ruleset required `build`/`kani`/`miri`/`asan`/`boot-gate` green, which reads like
    protection and was not: none of those five ever touched Windows.
  - FIXED with a `windows` job on windows-latest running
    `cargo build --release -p fab-bosl2 -p fab-mcad --verbose`, added to `release-native`'s gate
    list alongside the other five.
    - RELEASE and a BUILD, deliberately: `cargo check` would have caught NEITHER defect — the
      blowup is codegen (check of the same crate is 2m03s) and the overflow is a build SCRIPT
      executing. It has to build, in the profile that breaks.
    - The two transpiled crates only. bevy would roughly triple the job and add nothing this is
      watching for; the rest of the tree is already covered by the macOS job.
    - `timeout-minutes: 60` is the ratchet — a TA.1-class regression fails in an hour rather than
      burning the 6-hour runner ceiling inside a release job.
    - Caching cannot mask either defect: fab-lib is a BUILD-dependency of both crates, so an
      emitter change rebuilds the build script and re-runs it, which is when the transpile happens.
  - STILL OPEN, and it needs repo-settings access rather than a commit: ruleset 19646314 names its
    own required checks GitHub-side. Until `windows` is added THERE too, a red Windows job blocks
    the release job but does not block the tag PUSH.
- [x] TA.2 Hoist the closed-constant helpers (follow-on to TA.1, runtime win — NOT needed for CI)
  - Of TA.1's 32 outlined helpers, **12 read none of their parameters and never touch `fx`** — they
    are closed constants. `regular_polyhedron_info__o0` took 34 parameters and used zero. That was
    67.3% of all helper text (1,641,392 of 2,436,943 B).
  - They were also DUPLICATED: `regular_polyhedron_info` emitted the same 170,505-byte body **7
    times** (one md5 across seven helpers), `isosurface` two distinct bodies twice each. 50.4% of
    helper text (1,227,750 B) was byte-identical repetition.
  - So BOSL2's polyhedron table — 3352 float literals — was rebuilt from scratch seven times on
    every single call. It is now emitted once and computed once per thread.
  - DONE, both halves, in `Emitter::outline`:
    - DEDUP by signature+body, because identical signature plus identical body IS the same
      function. Not redundant with the linker: MSVC `/OPT:ICF` folds identical bodies for SIZE,
      but only after LLVM has optimised all seven copies independently, so it cannot save a second
      of the compile.
    - CLOSED-CONSTANT hoisting to a `thread_local! OnceCell`, emitted as `fn name() -> rt::Value`
      taking nothing. `rt::Value` is `Rc`-based (`lang/src/eval/value.rs`) and so neither `Send`
      nor `Sync` — a plain `static OnceLock<Value>` does not compile, and `thread_local!` is the
      better vehicle anyway (no cross-thread contention).
    - THREE CONDITIONS, all failing safe: no `?`, no `fx`/`args`/`_depth` token, no in-scope local
      token. Token-boundary matching, not substring — BOSL2 ships the string `"prefix"`, out of
      which a naive scan reads `fx`. The one residual error direction (a literal that IS `"fx"`)
      only declines a hoist.
  - MEASURED: generated band 12,633,223 -> 11,391,310 B (-9.8%), outlined helpers 32 -> 24, four of
    them const-hoisted (the 12 closed constants dedupe to 4 unique bodies). Coverage unchanged at
    1322 of 1329, same 7 declines. Both checked-in goldens byte-identical, which is the module
    emitter's `outline_prefix: None` guard holding.
  - NOT claimed: a binary-size win. `/OPT:ICF` was very likely already folding the duplicates, so
    the size number was never the argument — compile time and per-call work were.
  - Meanwhile v1.3.0 and v1.3.1 both ship the web bundle + the macOS dmg; only the .exe is missing,
    and every release back to v1.1.0 had one.
- [ ] TA.4 Add `windows` to tag ruleset 19646314's required checks — split out of TA.3's STILL OPEN note at the OpenSpec migration (2026-10-04), because the ruleset STILL requires only `build`/`kani`/`miri`/`asan`/`boot-gate`: a red Windows job blocks `release-native`'s gate but not the tag push. GitHub-side (repo settings or `gh api`), plus `docs/packaging.md:52-62`, the ruleset's re-create spec, so a rebuilt repo doesn't drop it again
