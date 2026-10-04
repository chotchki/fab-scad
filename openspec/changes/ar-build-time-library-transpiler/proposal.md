# Phase AR - Build-time library transpiler (replaces intrinsics AND the JIT) - DIRECTION, post-AO

## Why

The interpreter had two hand-written fast tiers beside it. One was ~55 natives, each pinned to a BOSL2 definition by an AST fingerprint plus hand-kept guard lists (Phase AN catalogues the ways those broke). The other was a Cranelift JIT that only the desktop could run, because the browser cannot JIT in-sandbox. AR compiles the pinned OpenSCAD libraries to Rust at build time so both tiers can be deleted and the web runs the same tier as the desktop. chotchki settled it as a maintenance bet, judged on deleted hand-written surface and not on milliseconds (AR.1), and gated the deletion on compiling a fully working BOSL2.

## What Changes

- Shipped: AR.0, AR.2–AR.13 (with AR.3.x and AR.12.x), AR.14.1–.3, .5 and AR.14.4.1–.3/.5, AR.15–AR.18, AR.20.x, AR.21.x and AR.22–AR.37 (with AR.26.x and AR.37.x), from `aaffa2df` (2026-07-25) to `0b0ff43c` (2026-08-02). AR.0 and AR.4–AR.10 first shipped in v1.2.0, the rest in v1.3.0. `lib/` is fab-lib (`0eb937ea`). `bosl2/`, `mcad/` and `machineblocks/` transpile their pinned submodules into `OUT_DIR` through `fab_lib::build::transpile` (`378f0575`, `28926b17`), and the consumer accumulates a `Registry` and hands it in (`b93c3179`). At BOSL2 v2.0.752, 1345 of 1354 functions compile (`ecd7f623`). Modules were 402 of 414 at `0b0ff43c`. The ratchet floors are 1322 functions and 402 modules (`lib/src/emit.rs:4869`, `:4316`). The JIT is gone (7,661 lines, `cf0428c6`), and so are the last three hand-written natives (3,645 lines, `1ea52a5a`). Against OpenSCAD: 200/200 fuzzed BOSL2 programs (`b771794a`), 200/200 for MCAD (`28926b17`), and the BOSL2 corpus at 901/901 on the compiled tier (`b2aba04c`). machineblocks was only diffed tier against interpreter (500 seeds, 0 diverged), never against OpenSCAD.
- AR.1 (OPEN) is a direction box, and its end state has landed. Both hand tiers are deleted (AR.21). The browser runs the same tier as the desktop (`6f91c31c`, which found the web had been running 58 natives against native's 929). The acceptance suite now targets the band that does the deleting (AR.28, `bosl2/tests/surface_diff.rs`). The deletion's own gate ("a fully working BOSL2") was first measured against the shipped fab-bosl2 registry the evening AR.21 closed (`b2aba04c`), after `375349ca` had moved the module band out of `Registry::builtin()` and silently left the corpus gating the interpreter. It held at 901/901, as it had with the in-fab-lang module band live (`3dd0212a`, `a1d69b84`). Judged by AR.1's own measure (deleted hand-written surface), one piece is still left: AR.14.4's hand table, below.
- AR.14.2a looks done. `Registry::new().with(rows)` accumulates (`lang/src/registry.rs:288-319`), and evaluation takes it as a separate `&Registry`, which is the box's own answer to `Config` being `Copy`. `Config::intrinsics` stays as the per-eval toggle (`lang/src/eval/config.rs:36`). The process-lifetime `OnceLock`s became per-instance indexes (`registry.rs:228-238`), and the product accumulates fab-lang's rows, BOSL2 and MCAD (`src/import.rs:185-213`); machineblocks joins only under its opt-in feature, which no shipped build enables. This landed as AR.26.1 (`b93c3179`) + AR.26.4.3 (`375349ca`).
- AR.14.4 (OPEN): the crate shipped by a route the box did not name. It is neither checked in nor a proc macro: `build.rs` writes into `OUT_DIR` (AR.26, chotchki 2026-07-31). The checked-in stage happened and was later retired (AR.26.4.2, `375349ca`). The box asked for the expansion cost to be measured, and that cost turned out to be real: fab-bosl2 takes 816.5 s of the 918 s macOS release build step (`.github/workflows/release-native.yml:94`; the "TWICE per release" on the next line is the probe's artifact that TA.1 corrected, since the release is one `cargo build` with both `-p` flags at `:160`), and TA.1 was an LLVM blowup on a single 1.21 MB generated function (`80c1f609`, `ci.yml` TA.3 note). Its worry about a fixed file list is answered by watching the directory plus every `.scad` (`lib/src/build.rs`). What's left is an obligation the box's own text never states, which AR.14.3's ticked text and `lang/src/lib.rs:823-825` assign to it: the generated crate "replaces the hand table outright". That has not happened yet:
  - fab-lang still wires a 66-row hand table: 48 BOSL2 names plus 18 `_fab_poc_` rows (`lang/src/eval/intrinsics/mod.rs:482`), beside 34 PINS (`:90`) and 17 POC module rows (`:1960`). Its references are hand-transcribed and its guard lists are hand-kept.
  - Still present beside it: the checked-in `generated.rs` (2,590 lines; its own emitted 81-row table, 63 + 18, is unwired `dead_code` at `:609`) and `generated_modules.rs` (346), the `bootstrap_subjects`/`bootstrap_all` bridge (`lang/src/lib.rs:828`, `:858`) and the regen gates `generated_file_is_current` and `generated_modules_are_current` (`lib/src/emit.rs:5191`, `:5212`).
  - The product registers the 48 BOSL2 rows ahead of fab-bosl2's identical copies (`src/import.rs:196-197`), so they still answer dispatch. The comment at `:176-180` still expects AR.21 to delete them (its "66" counts the POC rows too).
  - AR.26.4's ticked text claims the bootstrap deletion. AR.21.2 (`1ea52a5a`) handed it back to AR.26 a day after AR.26 closed (`f6f165b5`), so no open box carries it in its own words. It wants an AR.14.4 sub-box: the hand table, the bootstrap bridge, both regen gates and the `bootstrap_all` reads (`lang/tests/registry_guard_audit.rs`, `lib/src/emit.rs`).
  - Open design question: where the POC rows (18 function, 17 module) live once the table goes. They test runtime capabilities inside fab-lang without BOSL2.
- AR.19 (OPEN), four parts, two partly done and two untouched:
  - Arming cost: the fingerprint memo landed (`56a2292e`, `FpMemo` at `lang/src/eval/mod.rs:3217`). At 1260 rows it cut the cost from +55 to +8.6 ms on a thin model and from +48.9 to +2.8 ms on a fat one. The build-time closure precompute that would reduce arming to a set intersection did not land. Every evaluation still resolves each defined function. The cost at today's row count is unmeasured; `bosl2/tests/arm_cost.rs` (`--ignored`) is the instrument.
  - `FALLBACK_SOURCES`: the per-thread parse trap is closed on the product path. AR.24 (`93341e06`) re-interprets in the live evaluator, which ignores the island (`lang/src/eval/module_rt.rs:980-987`), and only the evaluator-less `NoClosures` ctx (benches, oracles) still parses it (`lang/src/surface.rs:510-516`). Every native still references the island from its decline arm (`lib/src/emit.rs:516`), though, so the island still ships in the binary and in the full web worker even though the product never reads it. It is ~1.25 MB of verbatim BOSL2 in a local OUT_DIR built 2026-09-20.
  - The module fingerprint is still computed on every instantiation (`lang/src/registry.rs:618`). Unmeasured.
  - Compiled-to-compiled module dispatch still skips the CSG memo "while the ABI is still moving" (`module_rt.rs:591`). No commit since v1.3.0 has touched `registry.rs`, `eval/mod.rs` or `module_rt.rs`.
- Gaps with no box:
  - AR.28's continuous lane, `bosl2/fuzz` (`bosl2_dispatch_diff`), is neither run by `fuzz.yml` nor compiled by `ci.yml`'s fuzz-check (`:83-85`). That is the same unchecked-crate setup that kept the nightly red for 11 days (`4d642b0b`). The per-push `surface_diff.rs` (24 seeds) is what actually guards the band.
  - AR.37.3 (`0b0ff43c`, the public `rust_ident` mapper and collision guard) landed without a box.
- AR.14 closes with AR.14.2a and AR.14.4. The split happened under shorter names: fab-lib + fab-bosl2, not fab-lib-bosl2.
- AR then reduces to AR.19 and retiring AR.14.4's hand table, then ticking AR.1, AR.14 and AR.14.2a and refreshing `docs/transpiler-design.md`. That doc still opens with "Status: DESIGN, nothing built" (line 3), presents the proc macro as the plan (line 302), and lists modules and the fallback island as "Still open" (line 459).

## Capabilities

### New Capabilities

None declared yet. This change came over from PLAN.md on 2026-10-04 as a task list, not a spec, so `.openspec.yaml` carries `skip_specs: true`. When work resumes on a box that changes behavior, name the capability here, write its delta under `specs/` and drop the flag.

### Modified Capabilities

None yet.

## Impact

- Code:
  - The transpiler: `lib/src/{emit,library,build}.rs` (fab-lib), and `bosl2/`, `mcad/`, `machineblocks/` (a `build.rs` + `src/lib.rs` each).
  - Dispatch: `lang/src/registry.rs`, `lang/src/{rt,surface}.rs`, `lang/src/eval/mod.rs` (arming) and `lang/src/eval/module_rt.rs`.
  - The residual table: `lang/src/eval/intrinsics/` (`mod.rs`, `generated.rs`, `generated_modules.rs`, `native_rt.rs`).
  - Consumers: `src/import.rs` (the product registry) and `gen/src/lib.rs` (the surface the fuzzer targets).
- Tests:
  - `lib/src/emit.rs` holds `bosl2_codegen_coverage_holds_its_floor`, `bosl2_module_coverage_holds_its_floor`, `the_static_call_graph_is_measured` and `generated_file_is_current`.
  - `bosl2/tests/{dispatch_diff,surface_diff,module_band,accumulation,recursion_is_bounded}.rs`, plus `arm_cost.rs` (a measurement, not a gate).
  - `mcad/` and `machineblocks/` each have `tests/surface_diff.rs`.
  - `lang/tests/registry_guard_audit.rs` reads `bootstrap_all`, so it changes or goes with AR.14.4.
  - The nightly `intrinsics_dispatch_diff` (`fuzz.yml:119-121`), and CI's windows job, which builds fab-bosl2 + fab-mcad in release (`ci.yml:117`).
- Source of truth: `docs/transpiler-design.md` (stale, see above). `docs/blog-the-deletion.md` is the AR.21 writeup; `README.md:78` maps the crates.
- Release reach: everything ticked is in v1.3.0 and every release since. hotchkiss.io's pin (`FAB_GUI_VERSION` 1.3.2) already runs the transpiled tier in its full web worker. AR.19 and AR.14.4 change no answer by contract (dispatch is fingerprint-gated, and anything unproven interprets), so they ride the next v* tag to the dmg/.app, the Windows installer, the web bundle and `latest.json`. No one-way door beyond the standing one: a dispatch bug becomes a wrong answer on every macOS install that checks `latest.json`, which `dispatch_diff` and `surface_diff` exist to stop. Retiring fab-lang's 48 BOSL2 rows makes a build without the `bosl2` feature (the lean web worker among them) interpret those names, which costs speed and never changes an answer (`1ea52a5a`).
- External: the `libs/BOSL2` (v2.0.752), `libs/MCAD` (`bd0a7ba`) and `libs/machineblocks` (2.1.0) submodules. A bump re-transpiles at build time. `sustain.yml` watches BOSL2 (and OpenSCAD) upstream nightly; nothing watches the MCAD or machineblocks pins. No GitHub settings involved.
