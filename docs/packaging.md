# Native packaging (Phase 18 spike results)

**Verdict: cargo-packager works for this workspace, first try.** One `Packager.toml` at the
root bundles BOTH binaries (fab-gui as the .app, `fab` riding along in Contents/MacOS) plus the
Bevy assets, and emits a mountable DMG. The two-package workspace shape (root + gui member)
caused zero friction.

## Local build

```
cargo build --release --workspace --bins   # packager does NOT build — binaries must exist
cargo packager --release                   # reads Packager.toml → target/packager/
```

Artifacts: `target/packager/fab-scad.app` (~115 MB) and `fab-scad_0.1.0_aarch64.dmg` (~40 MB
compressed). Verified: the bundled `fab --help`/`--version` run; the bundled fab-gui drove the
FULL pipeline (scripted screenshot harness — load .scad, cut, reslice, shot) from the .app AND
from the mounted read-only DMG, with the icon font loading from `Contents/Resources/assets`.

The one code change this needed: `assets_dir()` in gui/src/main.rs bakes the dev crate path;
packaged bundles fall back to exe-relative `../Resources/assets` (mac bundle), then
`assets/` beside the exe (the Windows/Linux shape). Bevy resolves assets relative to the
EXECUTABLE, not Resources — the fallback bridges that (bevy#15618, closed-as-workaround).

## CI (drafted, not yet run)

`.github/workflows/release.yml` — workflow_dispatch or a `v*` tag; macos-14 → app+dmg,
windows-latest → NSIS, artifacts uploaded unsigned. `packaging/winget/` holds the draft
winget-pkgs manifests (placeholders for the release URL + SHA256; unsigned NSIS is accepted —
only MSIX requires Authenticode).

## The signing bill (decided facts, nothing bought)

- **macOS: unavoidable for real distribution.** Sequoia killed right-click-open; unsigned DMGs
  mean a Settings→Open-Anyway dance per user. Apple Developer Program $99/yr, then `rcodesign`
  (pure Rust, Linux-CI-friendly) signs + notarizes with an App Store Connect API key.
  cargo-packager has the macOS signing hooks. Current state: ad-hoc linker signature only;
  `spctl` rejects it — expected while unsigned.
- **Windows: optional to START.** winget pins SHA256, needs no Authenticode for NSIS — ship
  unsigned, eat SmartScreen warnings (winget installs mostly bypass the browser SmartScreen
  prompt anyway). When warnings matter: Azure Artifact Signing, $9.99/mo, US individuals OK,
  needs a PAID Azure subscription; EV lost its SmartScreen bypass in 2024 — never pay EV
  premium for reputation.
- **OpenSCAD is NOT bundled** (GPL distributor obligations + size; nightlies are signed/
  notarized upstream since ~2025). Detect-and-guide stays: `Openscad::discover` probes, the
  doctor explains. Pin a minimum snapshot when the GUI grows an install prompt.

## Re-verified 2026-07-14 (W.2, post-fab-gui)

Still works after the whole fab-gui rebuild (bevy diet, egui flip, the theme + `include_bytes!` fonts,
the static Manifold/TBB kernel). `otool -L target/release/fab-gui` lists **zero non-system dylibs** — the
kernel is statically linked — and the fonts are baked in, so the `.app` has NO runtime dylib or asset
dependency: fully self-contained. Verified: the bundled `fab-gui cube.scad --screenshot` rendered the
full themed UI + the model from inside the `.app` (Oswald/Quattrocento + the navy/gold theme + the kernel
all resolve from the bundle). **App icon added** — a navy/gold isometric box, source `packaging/macos/
icon.svg` → `make-icon.sh` (headless-Chrome render → `sips` → `iconutil`) → `fab-scad.icns`, wired via
`Packager.toml`'s `icons`.

## Known gaps before a real release

- **Windows .ico** — the `icons` entry is macOS-only (`.icns`); NSIS needs an `.ico`. Generate one from
  the same `icon.svg` before the Windows/winget resume (`make-icon.sh` is macOS-tool-only, so the .ico
  path needs ImageMagick or equivalent).
- CFBundleVersion is a build timestamp — pin in CI for reproducibility.
- ~3k absolute source paths embedded in the binary (standard Rust debug metadata) —
  `--remap-path-prefix` if we care.
- Decide winget shape BEFORE first submission (renames are painful): one NSIS installer with
  PATH registration for `fab`, or `fab` as a separate portable package.
