# Phase AO - Generated-model PERF differential vs OpenSCAD (the heavy lane — today's corpus is too trivial to time)

## Why

gen-diff's programs are cheap on purpose, so the oracle's wall time is mostly its ~50 ms process fork and the startup-adjusted median saturated to zero in AK.2. That leaves no honest answer to "how fast is fab against OpenSCAD on geometry". AO's answer is a heavy generated lane. It sweeps fixed dials 2/4/8/16 (rendering dominates only at 8 and 16; the fork is still 50%/33% at 2/4), times programs cold, counts only those that agree with the oracle and did real work, and publishes a scaling curve rather than one median. The lane stays separate from the real-model baseline (`perf/baseline.json`): generated programs buy unbiased shape coverage and scaling, while real models buy representativeness.

## What Changes

- Shipped: AO.1–AO.8, AO.10–AO.13 and AO.17. Most of it landed on 2026-07-25:
  - Generator side: `4fd68c6d` freezes `cheap` and adds `heavy(dial)`. `e296b924` adds AO.3's generator guards (depth cap 6, minkowski pin) and fixes `heavy(1)` coming out lighter than `cheap`. The dial now lands on the mesh (`373eccac`, which took dial 16's fork share from 34% to 8%), and the surface is declared with names and domains (`gen/src/lib.rs:194`, `lang/src/surface.rs:43`/`:212`).
  - The lane: `fab gen-perf` (`b43f1516`, `src/genperf.rs`) reports raw and fork-adjusted times with the fork %, gates on agreement and a 32-tri work floor, and counts rejects. AO.8 made it a weekly Sunday step (`37822b20`).
  - AO.11: the search() abort is written up (`47d4d441`), plus `Report::crashed`. AO.13 triaged the ~70 to zero (`6dd0036b`, 07-26). AO.17's `flip_tris` fix (`5f319348`) shipped in v1.4.0.
- AO.16 (OPEN, unblocked): the prediction held. The 9/20 sustain run (35508622587, pre-fix head `44ca2862`) still panicked at `boolean_result.rs:834:45`. The 9/27 run (36318646195) ran gen-perf for its full 10-min budget with no panic and published the first post-fix table (2/7/3/1 disagreements at dials 2/4/8/16). The 10/4 run (37203195228, same head `9f19da59`, oracle OpenSCAD-2026.10.03) added seed 128 at dial 8 and seed 19 at dial 16: 15 rows (2/7/4/2), 14 distinct once seed 51's repeat at dials 4 and 8 is folded. The ±2^63 family survives (dial 4 seed 379, `9.22337e+18` vs `-9.22337e+18`); seed 45 does not reappear. Nothing commits the table: once the 10/11 run overwrites issue #1's body, it survives only in the issue's edit history and this run's job summary (until log retention), so copy it out before triaging.
- AO.9 (OPEN, all but calibration): the knob landed in `fed162b8`: `FUZZ_BUDGET_S`, a `SHARE_*` percent table, and a step that fails CI unless the shares sum to 100 (`fuzz.yml:37-86`). The heavy-lane half was born wall-clock (`--budget`, `src/main.rs:128-130`; `sustain.yml:173` passes 600).
  - What's left is the calibration, and the data is in. All 37 green nightlies from 8/26 to 10/4 at 2400 s took 44–46 min, not ~60. Run 37200196953's step timings put the non-fuzz overhead at ~4.5 min, not the ~20 the comment guessed (`fuzz.yml:31-36`). A ~1 h job means about 3300. `fuzz.yml:9` still says to edit `-max_total_time`.
- AO.14 (OPEN) looks done as a record. `9a488664` retracted the parity claim (`fab render` defaults to `--engine openscad`, so it timed OpenSCAD against itself) and recorded 2.6x/3.9x/10.7x on three real models. `docs/transpiler-design.md:92-100` carries the finding, and the box names no further work. The numbers are a 07-25 snapshot taken before the transpiler went live (AR.26).
- AO.15 (OPEN): `c4997401` proved that gen-perf/gen-diff never shell out on our leg (227 oracle spawns for 220 seeds). It chose to make the engine self-announcing rather than change the default. That fix is not built:
  - The oracle path prints `render X -> Y` with no engine name (`src/main.rs:938`); only scad-rs names itself (`:911`).
  - The `Render` help line (`:221`) has not changed since 2026-06-30.
  - The trap recurred two days after filing: SY.1 (`d9423584`, comment at `:890-895`) found a color comparison that was the oracle against itself.
- Ticked boxes whose text the code doesn't match:
  - AO.3: `--eval-budget` exists (`src/main.rs:134-136`, where 0 means unlimited), but `sustain.yml:173` never passes it. sustain.yml also has no `timeout-minutes`, and the budget is checked between seeds, not within one (`src/genperf.rs:92`), so a runaway on our side is capped only by GitHub's 6 h job limit.
  - AO.4: dial 16 hits the ≥1 s / <5% fork target only intermittently (8/23 1208 ms raw at 4%, 9/27 1554 ms at 3%, 10/4 on the same head 752 ms at 7%). At ~20 seeds per 150 s the row is noise, so the gap is variance, not reach, and a dial 32 would time even fewer.
  - AO.5: "FAB_CSG_CACHE=0" was deliberately not done. The within-program memo stays on, and cross-seed coldness comes from a fresh `Ctx` per seed plus `build_geo_cold` (`src/genperf.rs:14-18`, `:145-151`). That is better than the box text.
  - AO.6: the box says a timed seed must agree on "echo + geometry residual", but `one_seed` gates on `first_echo_divergence` only (`src/genperf.rs:181-190`). No geometry is compared, so a seed with the right echo and wrong mesh is timed.
  - AO.7: the "K.4 per-run artifacts" clause rides on K.4, which is still open (`k-differential-harness/tasks.md:10`). No trend or artifact is published; each week's curve survives in issue #1's edit history and its run's job summary, enough to back-fill a trend without K.4.
- Doc drift, no box yet:
  - `docs/sustainment.md:100-105` and `sustain.yml:154-159` still call the timing provisional "until AO.2". No AO.2 commit (`373eccac`, the `9a488664` tick, AR.3/AR.4's surface work) touched either; blame is still all `37822b20`.
  - `docs/openscad-search-crash.md:17-20` calls openscad#5017 open, but upstream closed it on 2026-07-26.
  - The curve has moved: 10/4 read 2.24x/1.14x/0.98x/0.75x (fab loses from dial 8 up), against `docs/transpiler-design.md:94`'s 4.13x/1.20x/1.08x. The cause is unverified, because the oracle is a new nightly every week and TC.5 moved the generator's RNG stream.
- AO's open boxes reduce to triaging AO.16's 10/4 list, AO.15's CLI change, AO.9's one-number calibration, and chotchki's call on ticking AO.14. The AO.3/AO.4/AO.6/AO.7 mismatches and the three doc-drift fixes need a box or a `backlog.md` line first, or archiving drops them.

## Capabilities

### New Capabilities

None declared yet. This change came over from PLAN.md on 2026-10-04 as a task list, not a spec, so `.openspec.yaml` carries `skip_specs: true`. When work resumes on a box that changes behavior, name the capability here, write its delta under `specs/` and drop the flag.

### Modified Capabilities

None yet.

## Impact

- The generator and its surface:
  - `gen/src/lib.rs` (`Profile`, `heavy` `:134`, `BUILTINS` `:194`, `Surface` `:258`)
  - `gen/src/main.rs` (`scad-gen --replay <seed> --dial <n>`)
  - `lang/src/surface.rs` (`Domain`, `LibrarySurface`)
- The lane's runner and CLI:
  - `src/genperf.rs`; `src/gendiff.rs` (shared flag negotiation and echo comparison)
  - `src/openscad.rs:71-75` (`crashed`); `src/backend.rs:130` (`build_geo_cold`)
  - `src/main.rs`: `gen-perf` at `:119-140` (dispatch `:328-340`); `render` at `:221-249`, `:911` and `:938` for AO.15
- The kernel: `manifold/src/mesh.rs:1342-1378` (`flip_tris`). TE.5 owns AO.17's class-level follow-up (a `prop_vert` validator check and fingerprint).
- Workflows: `.github/workflows/sustain.yml:149-175` (the weekly step) and `.github/workflows/fuzz.yml:26-86` (the budget).
- Tests:
  - `gen/tests/cheap_profile_is_frozen.rs` covers the frozen cheap digest, dial monotonicity, the minkowski pin, `the_dial_lands_on_the_primitives`, domain-typed calls and the surface shape.
  - `mirrored_boolean_output_keeps_prop_refs_valid` (`boolean_result.rs:1257`) and fuzz.yml's share-sum step.
  - `src/genperf.rs` has no unit tests; the weekly run is its only exercise.
- Source of truth: `docs/sustainment.md` lane 5 (`:96-110`). AO.14's finding lives in `docs/transpiler-design.md` §"Why the generated corpus is the WRONG instrument here", and AO.11 in `docs/openscad-search-crash.md`.
- Release reach:
  - AO.9 and the sustain.yml wiring are CI-only, live once on main. `src/genperf.rs` is not: it's in the default `native` build, so a change there (wiring AO.3's eval budget, say) ships in `fab` on the next `v*` tag like AO.15, though no user is expected to run gen-perf.
  - AO.15 changes the shipped `fab` CLI, which rides in the macOS .app (`Packager.toml:2`) and the Windows installer (`release-native.yml:160` builds it). It reaches users on the next `v*` tag.
  - An AO.16 fix in fab-lang or the kernel reaches the desktop and web bundle by tag. hotchkiss.io moves only when its build.rs pin moves, and that pin is still `v1.3.2`, which predates AO.17 too.
  - No one-way door beyond the release's own `latest.json`.
- External:
  - The oracle is the newest OpenSCAD Linux nightly, unpinned (`sustain.yml:135-139`), so the disagreement list moves without our code moving; record the oracle name with any list you triage.
  - Upstream Manifold's `422ab6fce` (the #1781 fix AO.17 matches) is in no release (TE.4).
  - The BOSL2 pin doesn't touch the heavy lane, which generates against builtins only.
