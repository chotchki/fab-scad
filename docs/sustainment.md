# Sustainment (Phase SU): staying at parity with upstream OpenSCAD + BOSL2

We hand-recreated OpenSCAD (fab-lang) and lean on BOSL2 as a library we ALSO partially
reimplemented (the O-phase intrinsics). Both upstreams keep moving; parity decays silently unless
something watches. This is that something: a nightly job that notices upstream movement, re-proves
parity against the CANDIDATE version, and reports — a human merges pins, never a robot.

## Decisions (chotchki, 2026-07-22)

- **Report-only.** The nightly maintains ONE rolling GitHub issue. No auto-PR, no auto-merge —
  pin bumps happen by hand with the report in hand (the same pin-churn wariness that deferred the
  BOSL2 transpiler).
- **OpenSCAD: track `main`, watch only the corpus.** No stable release in 5 years, so version
  polling is meaningless. Most main-branch churn is also irrelevant to us — what matters is their
  `testdata/` + `examples/` trees (new/changed .scad = new parity obligations). We diff those
  paths, not the repo.
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

1. **Intrinsic matrix** (SU.2): fingerprint audit against the candidate BOSL2 checkout →
   per-intrinsic status JSON + table. The same tool runs in normal CI against the COMMITTED pin,
   where anything but 100% matched fails the build (guards accidental libs/ edits + registry
   drift). This is the K.4 "intrinsic matrix" artifact.
2. **BOSL2 corpus** (SU.3): harvest the CANDIDATE checkout's `tests/` (assertion tests — the
   values bar) + `examples/` (render-clean + non-empty geometry). A reasoned skip-list (2D-only,
   font-dependent, etc.) is logged in the report, never silent. The harness runs committed AND
   candidate pins in the same job — a REGRESSION is fails-on-candidate-only; upstream files that
   fail on both are pre-existing gaps, listed separately, not noise.
3. **OpenSCAD corpus** (SU.4): only the NEW/CHANGED files under `testdata/` + `examples/` since
   the watermark, same bar. Churn outside those paths short-circuits to a no-op.

## Report shape (the rolling issue body)

Per upstream: version delta (pinned → candidate), intrinsic matrix delta (changed/missing only —
matched is the quiet default), corpus regression table (file, stage failed, one-line error),
pre-existing-failure count, skip-list. Nothing moved ⇒ the nightly leaves the issue untouched.

## Adoption (manual, deliberate)

Reading the report, chotchki bumps the `libs/BOSL2` submodule (and/or refreshes intrinsics whose
fingerprints changed), commits, and normal CI — now carrying the SU.2 matrix gate + the corpus
harness — proves the bump. The loop closes at SU.7 with the first real hand-bump merging green.
