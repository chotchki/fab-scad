# Tasks

## Y. Verification hardening: 100%-Rust re-derivation — shrink the unsafe surface, aim each tier where it uniquely covers

- [x] Y.1 Recon: the verification-tier map (Workflow, wide)
- [x] Y.2 Shrink the unsafe surface (delete-before-test)
- [x] Y.3 Resurrect the lang fuzz campaign
- [ ] Y.4 Re-aim ASan at the JIT (its unique target)
- [ ] Y.5 Widen miri to the kernel unsafe
- ~~Y.6 TSan / race detection for surviving Send/Sync + S.4~~ (deferred 2026-07-19; now a line in `openspec/backlog.md`)
- [x] Y.7 Fuzz the geometry-lowering seam (new target)
- [x] Y.8 Audit + wire the kernel fuzz coverage
- [ ] Y.9 Extend kernel fuzz coverage (csg_tree random-op + new op targets)
