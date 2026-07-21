# fab-scad

A from-scratch Rust reimplementation of OpenSCAD — the LANGUAGE and the geometry KERNEL — plus the
project workflow OpenSCAD leaves to you: render, slice to fit the bed, add connectors, pack the plates,
export, publish. Stock `.scad` + BOSL2 in, the same mesh OpenSCAD would make out — held to OpenSCAD's
own output as the reference. It runs as a desktop app AND in the browser, one codebase, at
[hotchkiss.io/3d/editor](https://hotchkiss.io/3d/editor).

The story of WHY (and why the JIT I built for it barely earned its keep) is in
[part 1](https://hotchkiss.io/blog/you-just-turn-a-knob-and-it-goes-faster-right) of a writeup.

**A derivative work, with loud credit to what it stands on:**

- **[OpenSCAD](https://openscad.org)** — the language, and the reference every result is differentially
  tested against. It's the ORACLE, not a runtime dependency — the GUI renders on its own.
- **[BOSL2](https://github.com/BelfrySCAD/BOSL2)** — the library the real designs run on (pinned in `libs/`).
- **[Manifold](https://github.com/elalish/manifold)** — Elalish's CSG kernel, which I PORTED to Rust
  (`manifold/`) so I could parallelize it deterministically and ship it to WASM with no C++ toolchain.
  None of the hard geometry math is mine — I re-typed it into a language I could thread everywhere.

## What it is now

Two reimplementations, and no OpenSCAD binary in the flagship render path:

- **`fab-lang`** — OpenSCAD the language, in Rust: a hand-written lexer/parser + an explicit-stack
  evaluator that tessellates to a mesh. It renders real BOSL2 designs, and a differential harness checks
  every result against stock OpenSCAD to hold the line (the pinned BOSL2 test corpus passes ~901/901).
- **`fab-manifold`** — a pure-Rust PORT of the Manifold CSG kernel. The C++ is gone entirely: no cmake,
  no clang, no FFI. Two pillars — deterministic parallelism (bit-identical output every run, native OR
  wasm) and portable transcendentals (so native and wasm agree to the bit).

The GUI and web app render ENTIRELY in-process — OpenSCAD is only the test oracle there, and the whole
production workflow (`make` / `slice` / `coupon` / `publish`) is pure-Rust too. The one verb that still
shells the binary by default is `fab render` — it's the differential ORACLE, so defaulting to the real
thing is the point (`--engine scad-rs` runs our pipeline). It's faster than OpenSCAD on nearly all of my
real models, and it renders heavy BOSL2 pieces that OpenSCAD times out on — the whole reason it exists.

## Using it

**The GUI** (`fab-gui`, Bevy + egui) is the main way in — the SAME app on the desktop and on the web. A
live editor (renders as you type) feeding a top-tab workflow:

- **Model** — edit; save; open in the external OpenSCAD if you want it.
- **Customize** — OpenSCAD-style top-level params become widgets (sliders/combos/checkboxes), spliced
  back into the source; appears only when the model exposes parameters.
- **Parts** — per top-level part: cuts + connectors, printer bed X/Y/Z, auto fit-to-bed. Connectors are
  onion pegs (support-free, glue-free alignment) or bolt joints (heat-set insert + bolt).
- **Orientation** — click a piece in the 3D view to set which face prints down; co-packed across parts.
- **Export** — a live plates·pieces·fill metric, a Bambu multi-plate `.3mf` export, and a push back to
  hotchkiss.io.

```sh
cargo run -p fab-gui -- part.scad          # the desktop app
```

The web build is `gui/web/build-wasm.sh` — served cross-origin-isolated (COOP/COEP) so the threaded
kernel gets its `SharedArrayBuffer`: `python3 packaging/web/dev-server.py gui/web 8080`.

**The `fab` CLI** is the headless twin:

- `fab make <model>` — the one-shot: render → auto-slice → auto-connect → orient → pack → Bambu `.3mf`.
- `fab render <file> [--engine scad-rs] [--all] [--check]` — render (defaults to the OpenSCAD oracle;
  `--engine scad-rs` for the in-process pure-Rust path; `--check` doubles a render as a differential
  datapoint; `--all` sweeps the project with an incremental cache).
- `fab slice / plan / coupon / publish / doctor / new / focus` — the rest of the workflow: slice to the
  bed, plan a fit, print a tolerance coupon, publish to hotchkiss.io, env preflight, scaffold + focus a
  project.

## Layout

- `src/` — **`fab-scad`**: the workflow layer + the `fab` bin (project manifest, printer/bed planning,
  slicing + connectors, Bambu + standard 3MF writers, the geometry SERVICE the GUI talks to, and the
  OpenSCAD oracle + differential harness).
- `lang/` `manifold/` `types/` — the language, the kernel port, and their shared geometry vocabulary
  (a deps-free leaf so the two never accidentally unify their trig oracles).
- `gui/` `geom/` — the app, and `geom/` its geometry worker: the kernel behind one byte-envelope
  (`handle(&[u8]) -> Vec<u8>`), a ~1.7 MB wasm that runs in a Web Worker (a kernel thread on native).
- `jit/` — a desktop-only Cranelift JIT for the numeric long tail, bit-identical to the interpreter
  (fuzz-proven). Mostly idle on real models — see the writeup for why.
- `gen/` — a grammar-directed `.scad` generator (valid-by-construction programs; the fuzzer's corpus).
- `scad-lib/` — my MIT SCAD modules (the slicer + connector lib). `libs/` — BOSL2 and friends as pinned
  submodules. `models/` — the designs, a SEPARATE repo (CC BY-NC-SA), pinned as a submodule.

## Building + testing

`cargo build --release` builds the `fab` CLI + `fab-gui`; a self-contained `.app` / DMG comes from
`cargo packager` (`Packager.toml`, static kernel + baked fonts → zero runtime dylib/asset deps). The
gate is `cargo nextest run --workspace` **+** `cargo test --workspace --doc` (doctests don't run under
nextest), plus `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
The kernel and language are fuzzed (`.github/workflows/fuzz.yml`), and a byte-golden suite holds
native≡wasm and serial≡parallel bit-for-bit.

## License

The tool is **GPL-2.0-or-later** (`LICENSE`) — EXACTLY OpenSCAD's license, on purpose. The correctness
here derives from the OpenSCAD community's accumulated semantics, tests and docs; taking that value while
licensing around their GPL would be legal and wrong. Matching it byte-for-byte means anything here flows
UPSTREAM with zero friction if they ever find value in it.

In practice you comply with GPLv3's terms when you distribute a build: the kernel is a Rust port of
Apache-2.0 Manifold, and Apache-2.0 is one-way compatible INTO GPLv3 — the `or-later` is what makes the
combination legal (the same mechanism, for the record, that makes OpenSCAD+Manifold legal). The GRANT
stays 2-or-later so upstream can take our code on their terms; the rules you actually comply with are
v3's. `scad-lib` stays MIT. The designs in `models/` are a separate repo under **CC BY-NC-SA 4.0** —
different repo, different license, on purpose, so the slicer stays upstreamable without entangling the
designs' terms.
