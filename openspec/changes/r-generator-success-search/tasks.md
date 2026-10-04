# Tasks

## R. Generator + success-function search (perf + correctness fitness)

- [x] R.1 v0 perf success function: generate → rank programs by deterministic eval-cost → worst-case report
  - [x] R.1.1 surface a deterministic eval-cost metric (eval_steps) from fab_lang — a metered-eval entry that returns (result, steps)
  - [x] R.1.2 scad-gen: capture per-program eval-cost into the manifest (cost field), rank, expose a worst-case list
  - [x] R.1.3 perf report artifact (eval-cost histogram + top-N worst-case seeds) + a smoke test
- [ ] R.2 correctness differential: scad-rs vs OpenSCAD reference (success = divergence) — values/echo first, geometry gated on J.4.5 determinism
- [ ] R.3 v1 closed loop: score-guided search (evolve seeds/grammar-choices toward high-scoring inputs) — sampling → guided search
