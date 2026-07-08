# fab-scad

A from-scratch Rust reimplementation of the OpenSCAD language evaluator + geometry pipeline
(over the Manifold kernel), plus the project workflow OpenSCAD lacks — render, slice, output,
publish. The aim: run the same OpenSCAD + BOSL2 designs at native speed AND, eventually, in the
browser — one implementation, everywhere — with results held to OpenSCAD's own output as the
reference.

**A derivative work, with full credit to the projects it stands on:**

- **[OpenSCAD](https://openscad.org)** — the language, and the reference behavior every result is
  tested against (it's the oracle in the differential harness).
- **[BOSL2](https://github.com/BelfrySCAD/BOSL2)** — the library the designs run on (pinned in `libs/`).
- **[Manifold](https://github.com/elalish/manifold)** — the geometry kernel (the same one OpenSCAD links).

It exists for what it solves, and it's here to be taken from — the parser, the differential test
harness, and (in progress) the performance tier are offered back upstream, license-matched (see below)
so they flow with zero friction if those projects ever find value in them.

**WIP, moving fast.** The evaluator runs the pinned BOSL2 test suite at 99.9% and renders real BOSL2
designs, with a differential harness comparing every result against OpenSCAD to hold the line. In
release it's already at rough speed parity with OpenSCAD on real models; the performance tier (the
web story + a desktop JIT) is next — design in `docs/perf-tier-spec.md`. `SPEC.md` / `PLAN.md` live
at the root; `docs/` holds the design writeups.

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
- `fab slice <part.scad> [--spread N] [--3mf] [--png] [--kernel]` — apply the project's `[slicing]`
  spec (cuts + connectors): freeze the source, then either generate the slicer driver and render the
  pieces (default), or with `--kernel` do the slice + connectors IN-PROCESS via the Manifold kernel
  (Track C) — OpenSCAD renders the base mesh once, no per-piece spawn. The headless path the Phase-5
  GUI drives.
- `fab plan --size WxHxD [--printer NAME]` — fit a part on the bed (from `printers.toml`):
  orient it, rotate it diagonally, or — last resort — report the fewest cuts + the
  `slice(cuts=…)` to feed the slicer.
- `fab coupon --type pin|insert [--screw M3] [--slops …]` — emit + render a printable
  tolerance-test coupon (a joint swept across slop values) to dial in fit before a full print.
- `fab publish` — stubbed (Phase 7).

Opening designs in the OpenSCAD GUI by hand needs OPENSCADPATH in your shell — see
`docs/openscad-libraries.md`.

## License

The tool is **GPL-2.0-or-later** (`LICENSE`) — a deliberate flip from MIT, made when the
scad-rs work began, and EXACTLY OpenSCAD's license on purpose. The Rust OpenSCAD
implementation derives its correctness from the OpenSCAD community's accumulated semantics,
tests + docs; taking that value while licensing around their GPL would be legal and wrong.
Matching their license byte-for-byte means anything here flows UPSTREAM with zero friction
if they ever find value in it — and lets us port from `src/core` directly instead of
clean-room guessing.

**In practice this codebase operates under GPLv3 rules.** Our dependency tree includes
Apache-2.0 code (Manifold, via manifold-csg — the same Manifold OpenSCAD itself links), and
Apache-2.0 is incompatible with GPLv2's terms but one-way compatible into GPLv3. The
`or-later` is what makes the combination legal: any distributed build takes the GPL grant at
its v3 option. (This is also, for the record, the mechanism that makes OpenSCAD+Manifold
legal — not just community respect.) The GRANT stays 2-or-later so upstream can take our
code on their terms; the effective rules you comply with when distributing are v3's.
`scad-lib` stays MIT. The designs in `models/` are a separate repo
under **CC BY-NC-SA 4.0** — different repo, different license, on purpose (keeps the slicer
upstreamable without entangling the designs' terms).
