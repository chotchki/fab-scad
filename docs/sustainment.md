# Sustainment (Phase SU): staying at parity with upstream OpenSCAD + BOSL2

We hand-recreated OpenSCAD (fab-lang) and lean on BOSL2 as a library we ALSO partially
reimplemented (the O-phase intrinsics). Both upstreams keep moving; parity decays silently unless
something watches. This is that something: a nightly job that notices upstream movement, re-proves
parity against the CANDIDATE version, and reports — a human merges pins, never a robot.

## Decisions (chotchki, 2026-07-22)

- **Report-only.** The nightly maintains ONE rolling GitHub issue. No auto-PR, no auto-merge —
  pin bumps happen by hand with the report in hand (the same pin-churn wariness that deferred the
  BOSL2 transpiler).
- **OpenSCAD: track `master`, watch only the corpus.** No stable release in 5 years, so version
  polling is meaningless. Most branch churn is also irrelevant to us — what matters is their
  corpus trees: **`tests/data/scad/` + `examples/`** (the design draft guessed `testdata/`;
  probing the real repo corrected it). We diff those paths' tree SHAs, not the repo.
- **BOSL2: track tags.** BOSL2 tags every revision (v2.0.746 = rev 746), so "new tag" fires
  often — fine under report-only: an evaluation is minutes and the issue just updates in place.
- **The bar is render-clean + values.** Every harvested corpus file must parse, eval, and render
  with no error and non-empty geometry; upstream's own assertion tests must pass their asserts.
  Mesh-level differential vs their binary stays Phase R.2's territory.

## The intrinsic-loss model (why "losing an intrinsic" is slow, not wrong)

Every O-phase intrinsic dispatches ONLY when the BOSL2 function's span-free AST fingerprint
(`lang/src/eval/intrinsics/fingerprint.rs`) matches the registry. A BOSL2 bump that restructures
a function ⇒ fingerprint mismatch ⇒ the intrinsic silently stops dispatching and the evaluator
falls back to interpreting the NEW upstream source. Correctness self-heals; performance regresses
silently (the shoe-model −70% class of win quietly evaporates). So the sustainment question is
mechanical: fingerprint the candidate BOSL2 against the registry and report per-intrinsic
`matched | changed | missing`. `changed` = re-derive the intrinsic against the new source (or
accept the interp fallback); `missing` = the function was renamed/removed upstream.

## State: the rolling issue IS the database

Report-only means the nightly must not commit. The watermark ("what did I last evaluate") lives
in the rolling issue body as a machine-readable block:

```html
<!-- sustain-state
{"bosl2_tag": "v2.0.746", "openscad_corpus_sha": "<tree-sha of testdata/+examples/ last evaluated>"}
-->
```

The workflow reads its own issue, compares against upstream, evaluates the delta, rewrites the
issue (state block + human report). Durable, zero repo churn, survives cache eviction; a human
mangling the block just triggers one redundant re-evaluation.

## What an evaluation runs

1. **Intrinsic matrix** (SU.2): `fab intrinsics --bosl2 <root> [--json|--md]` — fingerprint audit
   against the candidate BOSL2 checkout, per-intrinsic `matched|changed|missing`, non-matched
   exits 1. The same audit gates normal CI against the COMMITTED pin two ways:
   `tests/intrinsic_matrix.rs` (rides every `cargo test`) and a `$GITHUB_STEP_SUMMARY` step.
   This is the K.4 "intrinsic matrix" artifact.
2. **BOSL2 corpus** (SU.3): `fab corpus-diff --candidate <root> [--md]` — the crash-isolated K.1
   sweep generalized over a `Lane`: `tests/` `.scadtest` assertions (the values bar) +
   `examples/*.scad` (render-clean: eval no-error AND non-null geometry; a missing-library
   warning buckets `load` so a vacuous empty-program pass is impossible). Committed AND candidate
   sweep in one job; **no static skip-list** — the committed run IS the baseline, so pre-existing
   failures (e.g. worldmap's heightfield) land in `still_failing` and never gate. What exits 1:
   REGRESSIONS (pass→fail) and NEW-FAILING (new upstream case we fail).
3. **OpenSCAD corpus** (SU.4): `fab scad-sweep --manifest <file> [--upstream <root>] [--md]` over
   only the NEW/CHANGED corpus files since the watermark (compare API → sparse checkout).
   Eval-clean is the whole bar — their corpus is full of 2D/echo-only files and carries no
   SUCCESS expectations we can hold ourselves to, so the sweep is REPORT-ONLY (exit 0); the
   report is the signal. Churn outside the corpus paths short-circuits to a no-op.
   `--upstream` (AE.1) filters on upstream's own MUST-FAIL verdicts so the failure table shows
   genuine divergence only: a failure classifies as expected (upstream parity) when the golden
   `tests/regression/echo/<stem>-expected.echo` contains an `ERROR:` line, the file is in
   `FAILING_FILES` (tests/CMakeLists.txt, `--retval=1` renders), the golden documents the same
   can't-open-library wall, or it's a `templates/` `configure_file` input never run raw. The
   workflow also clones MCAD (an openscad submodule the sparse clone skips) and exports
   `OPENSCADPATH=/tmp/openscad/libraries` — the corpus `include <MCAD/…>`s it, the fonts lesson
   again.
   The bar is a LADDER (AH): eval-clean < expected-classified < **echo-match** — a passing file
   under `tests/data/scad/` with a golden `tests/regression/echo/<stem>-expected.echo` must also
   MATCH its recorded `ECHO:` output (goldens with `ERROR:` lines are the classifier's; WARNING
   text stays v1-excluded; the sweep runs `preview: true` since upstream's echo tests never
   `--render`). Divergence buckets as `echo-diff` with the first differing line; the summary's
   "Echo goldens: N/M matched" is the lane's coverage. This bar is what caught two entirely
   unimplemented builtins "passing" eval-clean — and then a dozen real semantics bugs (AH.2).
   Full-census baseline (2026-07-23, post-AI): 551/576 eval clean, 24 expected, echo goldens
   94/94 matched — every open diff burned down (the AF/AG features + AI's value imports landed);
   the one genuine failure left is `misc/sub1/included.scad`, an include-FRAGMENT upstream never
   runs standalone.
4. **Generated-program differential** (AL): `fab gen-diff --seeds 1000 --md` — the AJ.8 oracle
   differential folded into the nightly, and the one lane that is NOT movement-gated: it runs
   EVERY night because its oracle is the newest upstream **Linux nightly AppImage** (picked
   newest-by-date from files.openscad.org/snapshots, `--appimage-extract`ed, wired in via the
   `OPENSCAD` env override, `xvfb-run` + `fonts-liberation` so both sides shape the same
   Liberation Sans). The Linux nightlies are FRESHER than any macOS install (their mac pipeline
   is the broken one), so this lane diffs fab-gen's whole-language programs against an oracle
   newer than local dev ever sees — the watch for holes that are non-obvious from their examples.
   Report-only (exit 0); the report header names the oracle version so skew rows date themselves.

## Report shape (the rolling issue body)

Per upstream: version delta (pinned → candidate), intrinsic matrix delta (changed/missing only —
matched is the quiet default), corpus regression table (file, stage failed, one-line error),
pre-existing-failure count, skip-list. The issue updates EVERY night since AL: the gen-diff
section is always fresh (new oracle nightly, new run), while corpus sections refresh only when
their upstream moved — an unmoved lane's section carries over verbatim from the previous body,
so the issue stays the complete database on quiet nights.

## Adoption (manual, deliberate)

Reading the report, chotchki bumps the `libs/BOSL2` submodule (and/or refreshes intrinsics whose
fingerprints changed), commits, and normal CI — now carrying the SU.2 matrix gate + the corpus
harness — proves the bump. The loop closes at SU.7 with the first real hand-bump merging green.
