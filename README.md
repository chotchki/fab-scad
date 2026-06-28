# fab-scad

A Rust workflow tool that wraps OpenSCAD with the lifecycle it lacks — render, slice,
output, publish — plus the shared SCAD toolkit my designs lean on. OpenSCAD is a great
geometry engine with no workflow story; `fab-scad` IS the workflow, and it owns it (this
repo is the superproject root).

**WIP.** Right now this is the skeleton; the plan and rationale live (for the moment) in
the `scad-models` designs repo's `SPEC.md` / `PLAN.md` and migrate here during the reorg.

## Layout

- `src/` — the `fab` binary (Phase 3; not here yet)
- `scad-lib/` — my shared SCAD modules (MIT): the linear slicer + connector lib, version
  stamping, part numbering
- `libs/` — third-party OpenSCAD deps as PINNED submodules (BOSL2, ...)
- `printers.toml` — printer / bed profiles
- `models/` — the `scad-models` designs repo, pinned as a submodule (Phase 3); CC BY-NC-SA

## License

The tool + `scad-lib` are **MIT** (`LICENSE`). The designs in `models/` are a separate repo
under **CC BY-NC-SA 4.0** — different repo, different license, on purpose (keeps the slicer
upstreamable without entangling the designs' terms).
