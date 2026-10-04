# Tasks

## K. scad-rs: differential harness + semantics corpus

- [ ] K.1 Harness v1: both engines, metric gate per model class, corpus tiers 1-3 wired in CI (OpenSCAD suite, BOSL2 tests, models/)
  - [x] K.1.1 BOSL2 test corpus tier: sweep the .scadtest suite through scad-rs
  - [x] K.1.2 Perf tier: scad-rs vs OpenSCAD full-pipeline wall-time on geometry
- [ ] K.2 semantics/ segmentation formalized: naming + provenance conventions; G.3/I tests migrated in
- [x] K.3 ChaCha8-seeded grammar-directed program generator v0; seed logged per run; one-command failure replay
- [ ] K.4 Published artifacts per run: divergence report + the (initially empty) intrinsic matrix — the trend line starts before the intrinsics do
