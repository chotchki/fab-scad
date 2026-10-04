# Phase TA - Windows packaging: the build that stopped fitting in a runner

## Why

The transpiled BOSL2 band stalled the Windows release build. v1.3.0's leg hit GitHub's 6-hour runner ceiling (run 30874936197) and v1.3.1's was cancelled by hand after an hour (run 30915865919), so neither shipped the .exe that every release from v1.0.2 to v1.2.0 had carried. The cause was one 1.2 MB generated function that LLVM cannot split across threads, and only the Windows build choked on it. Fixing it surfaced a second Windows-only defect, a build script recursing on windows-msvc's 1 MiB main-thread stack, fixed alongside in `43a2e5ae`. No CI job compiled on Windows, so a change got its first Windows build only after it was tagged.

## What Changes

- Shipped: TA.1–TA.3.
  - TA.1 (`80c1f609`, `43a2e5ae`, golden refresh `a155fdfa`) moves any sub-expression past 32 KB into an `#[inline(never)]` helper (`OUTLINE_THRESHOLD`, `lib/src/emit.rs:675`). It also runs every library transpile on a scoped 64 MiB thread (`TRANSPILE_STACK`, `lib/src/build.rs:43-65`). v1.3.2 was tagged on `43a2e5ae` and brought the .exe back (release-native run 31060862019).
  - TA.2 (`3c78c557`) dedupes helpers by signature+body and hoists closed constants into a `thread_local!` `OnceCell` (`emit.rs:771-842`).
  - TA.3 (`3479387d`) added the `windows` job (`.github/workflows/ci.yml:87-117`) and added it to release-native's gate (`release-native.yml:42`).
- Still holding:
  - `windows` has passed on all 11 main CI runs since it first ran (`0d5f4156` through `9f19da59`). Each took 7-18 min against its 60-minute limit.
  - v1.3.4, v1.3.5 and v1.4.0 all carry `fab-gui_*_x64-setup.exe`.
  - HEAD's band is the debug `OUT_DIR` built 2026-09-20 17:35, two minutes after `9f19da59`. It has 27 outlined helpers, 3 of them const-hoisted, and its largest function is 102,562 B, with none at or over 200 KB. TA.1's "fires 32 times" and TA.2's "32 -> 24" count the 2026-08-05 band, not today's.
- TA.4 (OPEN): ruleset 19646314 still requires only `build`/`kani`/`miri`/`asan`/`boot-gate` (`gh api .../rulesets/19646314`, last updated 2026-07-23). It is the repo's only ruleset, and main has no branch protection.
  - Today a red or still-running `windows` does not stop the tag push. release-web's gate checks only the five (`release-web.yml:39`), so it publishes a release with just the web bundle and marks it Latest.
  - release-native's single gate fronts both matrix legs, so it blocks the dmg, the .exe AND latest.json. The macOS updater then sees a 404, which it treats as "no update" (`gui/src/update.rs:9`).
  - This is a real race as well as a red-job risk. `windows` finished before `build` on 9 of those 11 runs, but on v1.4.0 it finished 63 s after it (`build` 00:57:34, `windows` 00:58:37 UTC). A re-run of release-native recovers it once `windows` is green.
  - It is not GitHub-only: `docs/packaging.md:52-62` is the ruleset's re-create spec ("re-create from this section") and lists five checks for both layers, so it has to name `windows` too, or a rebuilt repo loses it again. The box now says so.
- Stale text, no box:
  - `release-native.yml:84-108` still opens "WINDOWS IS BROKEN HERE". It was written in `c52cb317` the day before TA.1 found the root cause, and it repeats three claims TA.1's CORRECTED list retracts:
    - the band built TWICE per release (:94-95), against the single invocation at :160
    - the 70x gap and the 4-core anomaly (:102-103)
    - Windows Defender, then paging, as the leads (:104-107)

    It also cites the deleted `win-probe.yml` (:92). TA.3 changed only the gate list in that file.
  - The "a red tag can't even be PUSHED" headers (`release-native.yml:20`, `release-web.yml:20`) hold only for the five checks until TA.4 lands.
  - `lib/src/build.rs:42` still says CI has never built on Windows.
- TA.2's closing note ("v1.3.0 and v1.3.1 ... only the .exe is missing") is now history. v1.3.3 also lacks an .exe, but not because of TA: its Windows package job died downloading NSIS (`nsis-3.09.zip: Network Error: Unexpected EOF`, run 31617637625). v1.3.4 replaced it the same day, and nobody backfilled it.
- Gap found in review, no box yet: no unit test pins either fix. The three code commits added no `#[test]`, and nothing checks `outline`, `mentions_ident` (TA.2's `"prefix"` token-boundary case, `emit.rs:690-711`) or `TRANSPILE_STACK`. A test on the generated band, such as "no function over N bytes", would catch a TA.1-class regression on the macOS job, not only on Windows.
- TA then reduces to TA.4 (the ruleset plus `packaging.md`'s check list), a stale-comment pass no box covers yet (deleting `release-native.yml:84-108`, fixing `build.rs:42`), and a call on the unit-test gap: box it or descope it to `backlog.md`.

## Capabilities

### New Capabilities

None declared yet. This change came over from PLAN.md on 2026-10-04 as a task list, not a spec, so `.openspec.yaml` carries `skip_specs: true`. When work resumes on a box that changes behavior, name the capability here, write its delta under `specs/` and drop the flag.

### Modified Capabilities

None yet.

## Impact

- `lib/src/emit.rs`: `OUTLINE_THRESHOLD` (:675), `mentions_ident` (:690-711), `Emitter::outline` (:747-842).
- `lib/src/build.rs`: `TRANSPILE_STACK` and `transpile` (:31-65), which `bosl2/build.rs`, `mcad/build.rs` and `machineblocks/build.rs` all call. The band is written to `OUT_DIR`, not checked in. `windows` builds only fab-bosl2 and fab-mcad, the two the `libraries` default feature ships (`Cargo.toml:98`).
- `.github/workflows/ci.yml` (`windows`), `release-native.yml` (gate :42, the stale block :84-108), `release-web.yml` (gate :39). Ruleset 19646314 lives on GitHub, with no file behind it.
- Tests: none at unit level (the gap above). Coverage is end-to-end:
  - the bosl2 acceptance binaries TA.1 cites (`bosl2/tests/dispatch_diff.rs`, `bosl2/tests/surface_diff.rs`), plus `mcad/tests/surface_diff.rs`
  - the goldens `generated_file_is_current` and `generated_modules_are_current` (`emit.rs:5191`, `:5212`). The modules golden holds the module emitter's `outline_prefix: None` guard. The function golden runs with outlining on and pins that nothing in fab-lang's band crosses 32 KiB.
  - `bosl2_codegen_coverage_holds_its_floor` (`emit.rs:4717`)
  - the `windows` job, which builds but runs no tests. Running tests on Windows belongs to the backlog's tri-OS matrix (`openspec/backlog.md:38`).
- Source of truth: `docs/packaging.md` owns the Windows installer and the release gate, and is stale on the check list. `docs/transpiler-design.md` owns the emitter but has no word on outlining or `TRANSPILE_STACK`. No owning doc records either; the `emit.rs`/`build.rs` comments, this change's `tasks.md` and the TA commit bodies are the only record.
- Release reach:
  - TA.1 reached users in v1.3.2. TA.2 reached them in v1.3.3 on macOS and v1.3.4 on Windows. TA.3 is CI, and runs on every push.
  - TA.4 and the docs pass ship nothing, so under the archive rule TA archives from main, with no tag.
  - No one-way door: the ruleset edit is reversible and touches neither latest.json nor the signing key. One trap: the ruleset's context string must match the job's check-run name (`windows`) exactly. If the job is renamed later, every `v*` and `web-v*` tag push is rejected until the ruleset catches up.
- External: two things float, so a TA.1-class blowup can come back from outside the repo: the windows-latest image and `dtolnay/rust-toolchain@stable`. A BOSL2 pin bump is a deliberate in-repo commit, but it reshapes the band every time. `windows`'s 60-minute limit catches all three. The NSIS download during packaging is the v1.3.3 flake. hotchkiss.io pins v1.3.2 (`build.rs:33-34`) and takes only the web bundle, so TA.4 does not move it.
