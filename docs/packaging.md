# Native packaging (Phase 18 spike results)

**Verdict: cargo-packager works for this workspace, first try.** One `Packager.toml` at the
root bundles BOTH binaries (fab-gui as the .app, `fab` riding along in Contents/MacOS) plus the
Bevy assets, and emits a mountable DMG. The two-package workspace shape (root + gui member)
caused zero friction.

## Local build

One command for local dogfooding — builds the release bins, packages just the `.app` (skips the DMG),
and ad-hoc signs it so it launches on this machine:

```
bash packaging/macos/build-app.sh          # → target/packager/fab-scad.app, ad-hoc signed
```

Ad-hoc (`codesign -s -`) is enough to RUN it here; it is NOT Developer-ID / notarized, so `spctl` still
rejects it for Gatekeeper distribution — that paid path lives in CI (see "signing" below).

The raw steps the script wraps (e.g. to also emit the DMG):

```
cargo build --release --workspace --bins   # packager does NOT build — binaries must exist
cargo packager --release                   # reads Packager.toml → target/packager/ (app + dmg)
```

Artifacts: `target/packager/fab-scad.app` (~115 MB) and `fab-scad_0.1.0_aarch64.dmg` (~40 MB
compressed). Verified: the bundled `fab --help`/`--version` run; the bundled fab-gui drove the
FULL pipeline (scripted screenshot harness — load .scad, cut, reslice, shot) from the .app AND
from the mounted read-only DMG, with the icon font loading from `Contents/Resources/assets`.

The one code change this needed: `assets_dir()` in gui/src/main.rs bakes the dev crate path;
packaged bundles fall back to exe-relative `../Resources/assets` (mac bundle), then
`assets/` beside the exe (the Windows/Linux shape). Bevy resolves assets relative to the
EXECUTABLE, not Resources — the fallback bridges that (bevy#15618, closed-as-workaround).

## CI (`release-native.yml`, signing wired W.2.2)

A product `vX.Y.Z` tag builds macos-14 → signed app+dmg and windows-latest → NSIS, and attaches
both installers to the tag's GitHub Release; `release-web.yml` fires on the same tag and adds the
wasm bundle — ONE release, three artifacts (the site's `web-v*` channel is separate and
untouched). The tag must equal Packager.toml's version (checked in CI, as is the CFBundleVersion
pin). Manual dispatch still works → CI artifacts only, no release. `packaging/winget/` holds the
draft winget-pkgs manifests (placeholders for the release URL + SHA256; unsigned NSIS is
accepted — only MSIX requires Authenticode).

macOS signing is SECRETS-GATED: with all six secrets set the run produces a Developer-ID-signed,
notarized, STAPLED .app and DMG and `spctl`-asserts both in CI; with any missing it builds
unsigned (warnings, same artifacts). The pipeline is cargo-packager's own — it imports the .p12
from `APPLE_CERTIFICATE` into a temp keychain, codesigns with hardened runtime + timestamp,
submits to `notarytool`, staples the .app, signs the DMG — plus two workflow post-steps it
doesn't do itself: notarize+staple the DMG, and the `spctl` assessment. The identity is injected
by APPENDING `signing-identity` to Packager.toml at run time (that's why `[macos]` must stay the
LAST table there); it's never committed, so local builds and secretless dispatches keep working.

## The signing ceremony (chotchki, one-time, ~$99/yr)

Status 2026-07-22: steps 1–2 were ALREADY DONE (existing enrollment, team G53N9PU948; the
Developer ID Application cert in the login keychain is valid to **2027-02-01** — builds after
that need a renewed cert and refreshed `APPLE_CERTIFICATE`/`_PASSWORD` secrets; already-shipped
notarized builds stay valid, timestamped signatures outlive the cert). The bundle identifier
`io.hotchkiss.fab-scad` is just reverse-DNS `fab-scad.hotchkiss.io` — and Developer ID
distribution needs NO App ID registered in the portal (that's App Store / provisioning turf).

1. **Enroll** in the Apple Developer Program (developer.apple.com, personal account, $99/yr).
   Approval can take 24–48h.
2. **Certificate**: portal → Certificates → create **Developer ID Application** (needs a CSR:
   Keychain Access → Certificate Assistant → Request a Certificate From a Certificate Authority,
   saved to disk). Download, double-click into the login keychain, then export the cert+key pair
   as a password-protected `.p12` (Keychain Access → export).
3. **Notarization key**: App Store Connect → Users and Access → Integrations → App Store Connect
   API → Team key, role **Developer**. Download the `.p8` (ONE shot — it can't be re-downloaded),
   note the Key ID and the Issuer ID shown on that page.
4. **GitHub secrets** (repo → Settings → Secrets → Actions), all six:
   - `APPLE_SIGNING_IDENTITY` — the cert's common name, `Developer ID Application: <name> (<team id>)`
     (copy it from Keychain Access, or `security find-identity -v -p codesigning`)
   - `APPLE_CERTIFICATE` — the .p12, base64: `base64 -i cert.p12 | pbcopy`
   - `APPLE_CERTIFICATE_PASSWORD` — the export password
   - `APPLE_API_KEY` — the Key ID (e.g. `2X9R4HXF34`)
   - `APPLE_API_ISSUER` — the Issuer ID (a UUID)
   - `APPLE_API_KEY_P8` — the raw contents of the `.p8` file
5. Dispatch `release-native`, download the DMG artifact, and check Gatekeeper for real:
   mount + copy the .app with quarantine intact, `spctl -a -t exec -vv` says
   `source=Notarized Developer ID`, first open shows no warning.

Once the cert is in the login keychain, local `cargo packager` builds can sign too (add the
identity via `-c` or a temp append) — but ad-hoc `build-app.sh` stays the dogfood default.

## The signing bill (decided 2026-07-22: macOS bought, Windows deferred)

- **macOS: unavoidable for real distribution, so it's happening.** Sequoia killed
  right-click-open; unsigned DMGs mean a Settings→Open-Anyway dance per user. Apple Developer
  Program $99/yr. Tooling REVISED from the original rcodesign plan: cargo-packager's built-in
  codesign→notarytool→staple path (verified in its 0.11.8 source) does the whole job, and since
  the mac binaries must be BUILT on macos-14 anyway, `notarytool` is free — rcodesign's
  Linux-CI-friendliness was solving a problem this pipeline doesn't have.
- **Windows: optional to START.** winget pins SHA256, needs no Authenticode for NSIS — ship
  unsigned, eat SmartScreen warnings (winget installs mostly bypass the browser SmartScreen
  prompt anyway). When warnings matter: Azure Artifact Signing, $9.99/mo, US individuals OK,
  needs a PAID Azure subscription; EV lost its SmartScreen bypass in 2024 — never pay EV
  premium for reputation.
- **OpenSCAD is NOT bundled** (GPL distributor obligations + size; nightlies are signed/
  notarized upstream since ~2025). Detect-and-guide stays: `Openscad::discover` probes, the
  doctor explains. Pin a minimum snapshot when the GUI grows an install prompt.
- **Hardened-runtime caveat for the dormant JIT**: notarization requires hardened runtime
  (cargo-packager passes `--options runtime`), which blocks writable+executable pages. FAB_JIT
  is OFF and stays a correctness asset, so nothing breaks today — but if the Cranelift JIT ever
  ships enabled, the signed app needs a `com.apple.security.cs.allow-jit` entitlements plist
  wired via `[macos] entitlements`.

## Re-verified 2026-07-14 (W.2, post-fab-gui)

Still works after the whole fab-gui rebuild (bevy diet, egui flip, the theme + `include_bytes!` fonts,
the static Manifold/TBB kernel). `otool -L target/release/fab-gui` lists **zero non-system dylibs** — the
kernel is statically linked — and the fonts are baked in, so the `.app` has NO runtime dylib or asset
dependency: fully self-contained. Verified: the bundled `fab-gui cube.scad --screenshot` rendered the
full themed UI + the model from inside the `.app` (Oswald/Quattrocento + the navy/gold theme + the kernel
all resolve from the bundle). **App icon added** — a navy/gold isometric box, source `packaging/macos/
icon.svg` → `make-icon.sh` renders a 1024 master (headless Chrome) then packs BOTH `fab-scad.icns`
(macOS, `sips`+`iconutil`) and `fab-scad.ico` (Windows, ImageMagick multi-size), wired in `Packager.toml`'s
`icons` (cargo-packager picks per platform).

## Known gaps before a real release

- ~~CFBundleVersion is a build timestamp~~ — PINNED (W.2.2.1): `packaging/macos/Info.plist`
  merges over the generated plist via `[macos] info-plist-path`; build-app.sh + CI fail on
  drift from Packager.toml's version. Bump both together on release.
- ~3k absolute source paths embedded in the binary (standard Rust debug metadata) —
  `--remap-path-prefix` if we care.
- Decide winget shape BEFORE first submission (renames are painful): one NSIS installer with
  PATH registration for `fab`, or `fab` as a separate portable package.
