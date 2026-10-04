# Phase TE - Care-and-feeding 2026-09-20: land the backlog, bump EVERY pin, then drive back to zero

## Why

The 2026-09-20 sustainment sweep, read end to end, found six problems. BOSL2 753 changed `tube(wall=)` geometry where no guard looks. `import()` accepts what upstream refuses (it warns that `type=` isn't a parameter, then parses anyway). The nightly report cannot tell a carried section from a fresh one. Every non-SCAD pin is watched only by memory. The AO.17 corruption is invisible to both the validator and the golden fingerprint. And two fuzz nights were lost without a trophy. chotchki's direction (TE.0) is to bump every pin and drive back to zero errors and divergences, in an order that keeps every divergence attributable to a bump rather than to unlanded work: ship the landed backlog first, write the zero down before the first bump, and revert any bump that cannot be driven back to it.

## What Changes

- Shipped: TE.7, v1.4.0 on the landed work (`9f19da59`, re-scoped in `df126f3a`).
  - CI went green on that commit (run 35548486189) before the tag run.
  - The release (2026-09-21) carries the dmg, `fab-scad.app.tar.gz`, the NSIS `.exe`, `fab-gui-1.4.0.tar.gz` and `latest.json`. It is still Latest: no `web-v*` tag has been pushed since web-v0.28.0 (2026-07-21).
  - TE.0's toolchain note holds: local `rustc -V` is 1.98.1. There is still no `rust-toolchain.toml`.
- TE.7 is ticked, but two of its traps never cleared and a third can't be verified:
  - `windows` is still not in ruleset 19646314, which requires only build/kani/miri/asan/boot-gate (`fuzz-check` isn't required either). TA.4 owns it now.
  - hotchkiss-io's `build.rs:33-34` still pins `1.3.2`, and no box in either repo tracks the bump. v1.3.2's bundle has 73 downloads and v1.4.0's has 0.
  - The key backup can't be verified from here. The `CARGO_PACKAGER_SIGN_PRIVATE_KEY` secret exists (set 2026-08-12). TB.2 has been ticked since `8a7a9c2f` (2026-08-12), so TE.7's "never ticks it" was wrong when written (unless it meant the backup sub-step), but TB.2's text still names the password-manager copy as the step left.
- TB.5 closed 2026-10-04: chotchki's installed app updated itself to v1.4.0 through the in-app updater, so TE.0's "the TB.5 chain has never once run live" is history (it was true when written; v1.4.0 was the newer release it said the test needed). The download counters couldn't show it, since a counter can't attribute a fetch.
- TE.0 (direction, not exit): the release half is done, and the bump campaign hasn't started.
  - No pin has moved since `9f19da59`: BOSL2 v2.0.752, binaryen `version_130`, `nightly-2026-08-12`, bevy 0.19.0, and the Manifold oracle at v3.5.1 (`mesh.rs:1366`).
  - The zero lanes still hold in today's nightly (run 37203195228): gen-diff 1000/1000 against the 2026.10.03 oracle, corpus tests 907/907 and examples at their 8/10 baseline (both from the 10-03 eval).
  - CI was last green on HEAD 2026-09-21, so clippy under a newer CI `@stable` is unverified.
  - A lane the criterion never counted has opened. gen-perf crashed on 09-20 with the AO.17 panic (`boolean_result.rs:834:45`, in the run log). Since 09-27 it completes, and it reports 15 echo disagreement rows across dials 2/4/8/16 (14 distinct; seed 51 repeats at dials 4 and 8). They are untriaged. AO.16 (open) owns them, but TE.0's zero criterion doesn't count the lane: its "zero today" list was written while the lane was dark.
- TE.1 (OPEN, not started):
  - `lang/src/eval/mod.rs:2022` still binds only `["file"]`.
  - The extension gate (`:2067` derives the extension, `:2072` tests it) still runs after `request_data` (`:2058`). No code emits "Unsupported file extension".
  - The builtin lane has tripped again since, on `bde97fbcb` (2026-10-02, textmetrics un-experimentalized, #7063). No box covers it, and what it changes for fab is unverified.
- TE.2 (OPEN, target moved):
  - The pin and the surface floor (`bosl2/tests/surface_diff.rs:686`, 1354) are unchanged.
  - Upstream is now v2.0.766. The 10-03 eval against it gives 82 of 1418 drifted (at 757 the nightly said 21, which the box reduced to eight residual), corpus 976/976 with 0 regressions and examples 9/10. The box's 757 arithmetic (eight residual rows, 1373) is stale, so the target has to be picked again.
  - The `circle_loom` render-diff precondition stands: `circle_loom.scad:15,32` still calls `tube(..., wall=10)`, and :15 is disabled by `*`.
- TE.3 (OPEN, none of (a)-(f) landed):
  - `sustain.yml:272` carves with no marker.
  - The watermark (`:265`) has no `bosl2_pinned`, and `:97` still gates on `bosl2_latest != seen_tag`.
  - The probe (`:80`) hashes two corpus paths while `:200` sparse-checks five, and `:84` watches two builtin files.
  - `:167-175` still nests the `|| echo` inside `{ … } > file`.
  - Today's report is a live (a): three of five sections sit unmarked under "Last evaluation: 2026-10-04". BOSL2 dates from 10-03, builtins from 10-02 and the openscad corpus from 09-30.
  - The crash behind (e) was the AO.17 panic in four runs (the box's "four Sundays" is the 08-29 dispatch plus Sundays 09-06, 09-13 and 09-20), and it stopped once `5f319348` landed. The silence that hid it is unchanged.
- TE.4 (OPEN):
  - `.github/` still holds only `workflows/`, and `actions/checkout` and `upload-artifact` are still `@v4` against v7.0.1.
  - Upstream moved further: binaryen is at `version_133` (09-21) and bevy at 0.20.0-rc.2 (09-28).
  - Manifold v3.5.4 (09-25) still lacks `422ab6fce` (27 commits behind it), and master runs 79 ahead / 9 behind v3.5.4 across 185 files. That makes the case for watching master stronger.
- TE.4a (OPEN):
  - The lock still has bevy 0.19.0 and bevy_egui 0.41.0 (0.42.0 is out). wasm-bindgen 0.2.126 against 0.2.129 is still harmless.
  - Two gaps the box doesn't name. There are FOUR fuzz manifests: `bosl2/fuzz` (AR.28, `2dcd2820`) is compiled by neither `ci.yml`'s `fuzz-check` (`:83-85`) nor the nightly. And `cargo-packager-updater = "0.2.3"` (`gui/Cargo.toml:63`) is caret-ranged, so a plain `cargo update` can move the app-side half of the TB.2 update contract. Whether a newer 0.2.x exists is unchecked.
- TE.5 (OPEN):
  - `Mesh::is_manifold` (`manifold/src/mesh.rs:496`) checks halfedges only, and `golden::mesh` (`golden.rs:41-58`) still skips `prop_vert`.
  - Design point the box misses: the pinned C++ `IsManifold` (`properties.cpp:102-107`) doesn't check prop refs either, and the Rust doc claims parity with it. The bounds check belongs in its own validator, not inside the ported predicate.
- TE.6 (OPEN, now three nights):
  - `lang/fuzz/TROPHIES.md` has no September entry.
  - The box's explanation for the missing 09-15 reproducer is stale. That night's minimize and upload steps did run (`if: always()`, `fuzz.yml:142-151`). The reproducer is in crash artifact 10395237258 (live until 2026-12-14) rather than the corpus because libFuzzer writes crashers to `artifacts/`.
  - A third night: 09-26 (run 36237271323), an `eval` timeout at 11 s that skipped all five later targets (artifact 10904319868).
  - Of TE.6's three nights, the first to expire is 09-01's `intrinsics_dispatch_diff` timeout (9798693430, 2026-11-30). Outside its window, two untrophied `gen_diff` timeouts expire sooner: 07-28 (8682428832, 10-26) and 08-07 (8986161964, 11-05).
- TE then reduces to TE.1–TE.6 and TE.4a, plus two boxes that don't exist yet: a docs box, and the v1.5.0 release that TE.0 names and TE.7 calls "tracked separately" (nothing tracks it). TE.0 ticks when that release ships converged.

## Capabilities

### New Capabilities

None declared yet. This change came over from PLAN.md on 2026-10-04 as a task list, not a spec, so `.openspec.yaml` carries `skip_specs: true`. When work resumes on a box that changes behavior, name the capability here, write its delta under `specs/` and drop the flag.

### Modified Capabilities

None yet.

## Impact

- Code and config, by box:
  - TE.1: `lang/src/eval/mod.rs` (`run_file_fn`).
  - TE.2: `libs/BOSL2`, `bosl2/tests/surface_diff.rs`, `models/circle_loom/circle_loom.scad`.
  - TE.3: `.github/workflows/sustain.yml`.
  - TE.4: `release-web.yml` (`:72` nightly, `:82` binaryen, `:97-103` the `-O1`), `ci.yml` (`:194` boot-gate nightly, `:70-85` fuzz-check), the `actions/*@v4` uses, and a `.github/dependabot.yml` or Manifold-master lane that doesn't exist yet.
  - TE.4a: `Cargo.lock`, `gui/Cargo.toml` (`:23` bevy, `:43` bevy_egui, `:63` the updater crate), and the four fuzz manifests.
  - TE.5: `manifold/src/mesh.rs`, `manifold/src/golden.rs`.
  - TE.6: `.github/workflows/fuzz.yml`, `lang/fuzz/TROPHIES.md`.
- Tests:
  - TE.2: `tests/intrinsic_matrix.rs`, `tests/bosl2_corpus.rs`, `tests/bosl2_examples.rs`, `bosl2/tests/surface_diff.rs`.
  - TE.5: `mirrored_boolean_output_keeps_prop_refs_valid` (`boolean_result.rs:1257`), `is_manifold_hand_built_edge_cases` (`mesh.rs:2224`) and `fingerprints_are_bit_sensitive` (`golden.rs:89`). The fingerprint change also moves every `golden::mesh` hash: 20 of the 33 rows in the inline table at `m6_native_wasm_golden.rs:326` (the `f64s` and `cross_section` rows stay) and all 18 `ours_fingerprint` rows in `manifold/goldens/oracle_goldens.json` that `m7_golden_mode.rs:314` checks. The C++ freeze that wrote those rows was cut at M.7.4, so they are re-recorded from Rust alone.
  - The nightly lanes (`fab gen-diff`, `gen-perf`, `scad-sweep`, `intrinsics`, `corpus-diff`) are the convergence oracle.
  - TE.1 has no local test, so the upstream `import-json` golden runs only in the nightly sweep. Nothing tests `sustain.yml` (TE.3) short of a `workflow_dispatch` run.
- Source of truth:
  - `docs/sustainment.md` (the lanes, carry-over at :125-130, hand-bump adoption) for TE.2/TE.3.
  - `docs/packaging.md` (release, update-signing ceremony at :178) for TE.7.
  - `docs/transpiler-design.md` for what a BOSL2 bump recompiles.
  - `SPEC_manifold-rs.md` for TE.5.
  - `lang/fuzz/TROPHIES.md` is TE.6's ledger.
  - No doc owns the toolchain/binaryen/nightly pins. Their expiry conditions live only in workflow comments (`ci.yml:187-191`, `release-web.yml:65-68`, `:97-103`).
- Release reach:
  - Ships, and so reaches users only via a `v*` tag (v1.5.0 per TE.0): TE.1, TE.2, TE.4a, TE.5 and the binaryen/nightly half of TE.4. That tag puts the dmg/.app, the Windows installer and the web bundle on one release and sends `latest.json` to the macOS updater.
  - CI-only: TE.3, TE.6 and the actions bumps.
  - hotchkiss.io moves only with its `build.rs` pin, which is four releases behind (v1.3.2).
  - TE.1 changes observable behavior against the oracle (it refuses what fab parses today), so it is the box that drops `skip_specs`.
  - TE.2 changes the rendered geometry of every `tube(wall=)` caller.
- One-way doors:
  - v1.5.0's `latest.json` reaches every installed app that checks.
  - The signing key can't rotate without a bridge release.
  - `cargo-packager` stays at 0.11.8 (`release-native.yml:172`), and its tar and signature formats are a contract with installed apps (TB.2).
  - A v1.5.0 whose own updater is broken strands every app that takes it.
- External:
  - OpenSCAD master (`bf926f947`, `bde97fbcb`) and its nightly AppImage (the gen-diff oracle).
  - BOSL2 tags (752 pinned, 766 upstream).
  - Manifold master vs the 3.5.x releases.
  - binaryen, bevy and bevy_egui releases.
  - GitHub ruleset 19646314 (TA.4) and the 90-day crash-artifact retention.
  - hotchkiss.io's pin.
