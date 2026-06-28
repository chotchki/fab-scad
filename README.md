# fab-scad

A Rust workflow tool that wraps OpenSCAD with the lifecycle it lacks — render, slice,
output, publish — plus the shared SCAD toolkit my designs lean on. OpenSCAD is a great
geometry engine with no workflow story; `fab-scad` IS the workflow, and it owns it (this
repo is the superproject root).

**WIP.** The foundation (Phase 3) has landed: the `fab` binary, the OpenSCAD wrap, the
minimal manifest, and the focus + scaffolding workflow. `SPEC.md` / `PLAN.md` live here at
the root now. Next up is the linear slicer (Phase 4).

## Layout

- `src/` — the `fab` binary (`doctor`, `new`, `focus`, `render`)
- `scad-lib/` — my shared SCAD modules (MIT): the linear slicer + connector lib, version
  stamping, part numbering
- `libs/` — third-party OpenSCAD deps as PINNED submodules (BOSL2, ...)
- `printers.toml` — printer / bed profiles
- `models/` — the `scad-models` designs repo, pinned as a submodule; CC BY-NC-SA

## Commands

`fab` finds the workspace root (the dir with `printers.toml` + `scad-lib/`) by walking up
from the cwd, so it runs from anywhere in the tree.

- `fab doctor` — env preflight: OpenSCAD + Manifold backend, submodules, scad-lib,
  OPENSCADPATH, NAS mount.
- `fab new <name>` — scaffold a project under `models/<name>/` (minimal `project.toml` +
  starter `src/<name>.scad`) and focus it.
- `fab focus [<project>]` — set the active project, or show it with no arg, so later
  commands need no name. Recorded per-user in `.fab/focus` (gitignored).
- `fab render <file.scad> [--png]` — render geometry via Manifold, OPENSCADPATH injected so
  `<BOSL2/...>` and scad-lib includes resolve. File-level for now; project/DAG-aware in
  Phase 6.
- `fab slice <part.scad> [--spread N] [--png]` — apply the project's `[slicing]` spec (cuts +
  connectors): freeze the source, generate the slicer driver, render the pieces. The headless
  path the Phase-5 GUI drives.
- `fab plan --size WxHxD [--printer NAME]` — fit a part on the bed (from `printers.toml`):
  orient it, rotate it diagonally, or — last resort — report the fewest cuts + the
  `slice(cuts=…)` to feed the slicer.
- `fab coupon --type pin|insert [--screw M3] [--slops …]` — emit + render a printable
  tolerance-test coupon (a joint swept across slop values) to dial in fit before a full print.
- `fab publish` — stubbed (Phase 7).

Opening designs in the OpenSCAD GUI by hand needs OPENSCADPATH in your shell — see
`docs/openscad-libraries.md`.

## License

The tool + `scad-lib` are **MIT** (`LICENSE`). The designs in `models/` are a separate repo
under **CC BY-NC-SA 4.0** — different repo, different license, on purpose (keeps the slicer
upstreamable without entangling the designs' terms).
