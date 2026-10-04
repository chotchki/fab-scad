# Phase TB - macOS auto-update: releases that install themselves

## Why

Before TB, every macOS release meant a manual DMG download on every install, which is the chore `docs/packaging.md` says this phase deletes. TB makes the signed app update itself: a launch check polls a `latest.json` that CI writes on each `v*` tag, and a prompted install minisign-verifies the tarball, swaps the bundle and relaunches. The phase closes only when a real install has taken a real update, because the GitHub redirect chain and Gatekeeper on the swapped bundle can't be reached from the localhost e2e (TB.4's residual).

## What Changes

- Shipped: TB.1–TB.4 in `8a7a9c2f` (2026-08-12), first released in v1.3.3 (v1.3.2 predates it). The app side uses cargo-packager-updater 0.2.3 (`gui/Cargo.toml:63`, `Cargo.lock:2055`) and polls `releases/latest/download/latest.json` (`gui/src/update.rs:28`) against a baked minisign `PUBKEY` (`:37`, untouched since `8a7a9c2f`). On the release side, `release-native.yml` refuses to publish a tarball that fails minisign against that `PUBKEY` (`:227-251`), writes the manifest with `packaging/macos/make-update-manifest.sh`, which the e2e also runs, and never overwrites a published asset (`overwrite_files: false`, `:276`). `manifest_sign_download_verify_swap` (`gui/tests/update_e2e.rs:83`) runs in CI's macOS `build` job on every push. The first real release took three runs and two fixes: `947ee0a6` (a trailing newline in the key killed cargo-packager) and `f160a4a8` (macOS `base64` reads stdin only).
- TB.5 CLOSED 2026-10-04 on chotchki's report: an installed app updated itself to v1.4.0 through the in-app updater, so the GitHub redirect chain and Gatekeeper on the swapped bundle held on a real install. The secret was created 2026-08-12 13:48Z and last re-set at 14:54Z (`gh api .../actions/secrets`), and v1.3.3 (`f160a4a8`), v1.3.4 (`3bab5a12`), v1.3.5 and v1.4.0 all published `fab-scad.app.tar.gz` + `latest.json` (v1.4.0's manifest carries the `macos-aarch64` key). The box's "ship v1.3.3 then v1.3.4" pair never got its live test; v1.4.0 did instead. This proposal's first draft read v1.4.0's download counters as proof no installed app had checked. That was wrong: a counter can't attribute a fetch, and the tarball's second, unattributed download fits chotchki's update.
- TB.2's "one human step left" is half stale: the secret is set. Nothing in the repo records the password-manager backup, and TE.7 asked for that confirmation and is ticked, but nothing records that the confirmation happened.
- TB.6 (OPEN), boxed 2026-10-04 from this review: `release-web.yml` still fires on `web-v*` tags with `make_latest: true` (`:13`, `:223`), and `docs/web-embed.md:188` still documents `web-vX.Y.Z` as the bundle's tag. A web-only release would become Latest with no `latest.json`; every installed app's launch check swallows the 404 as "no update" (`docs/packaging.md:173-176`), and updates stop silently until the next `v*` tag. It has never fired (the last `web-v*` tag is web-v0.28.0, 2026-07-21, from before TB), and nothing needs a web release to be Latest: hotchkiss-io fetches by an explicit product tag (`FAB_GUI_TAG = "v1.3.2"`).
- TB reduces to TB.6 (a workflow + docs edit) and a record of the key-backup confirmation TE.7's tick doesn't carry. `docs/packaging.md`'s through-the-app paragraph moved to past tense with TB.5's close.

## Capabilities

### New Capabilities

None declared yet. This change came over from PLAN.md on 2026-10-04 as a task list, not a spec, so `.openspec.yaml` carries `skip_specs: true`. When work resumes on a box that changes behavior, name the capability here, write its delta under `specs/` and drop the flag.

### Modified Capabilities

None yet.

## Impact

- `gui/src/update.rs` (endpoint, `PUBKEY`, check/install/relaunch, dialog), `gui/src/settings.rs:174` ("Check for updates"), `gui/src/panel.rs:394-399` (header badge), `gui/src/lib.rs:367-377` (system wiring), `gui/Cargo.toml:62-69` (macOS-gated updater dep, `minisign` dev-dep).
- `.github/workflows/release-native.yml` (key enable `:186`, manifest + verify `:227`, upload `:265`), `packaging/macos/make-update-manifest.sh`, `scripts/bump-version.sh` (four version pins, because the updater compares fab-gui's `CARGO_PKG_VERSION`).
- Tests: six unit tests in `gui/src/update.rs` (bundle shape, translocation marker, same-device check, opt-out spellings, semver, pubkey shape) and `gui/tests/update_e2e.rs` (macOS-only, with a tampered-payload must-fail). TB.5 was manual by nature (no test can reach the redirect chain or Gatekeeper), closed by a real install.
- Source of truth: `docs/packaging.md` "Auto-update (TB)" (`:146-221`: mechanism, key ceremony, limits). README.md doesn't mention self-update.
- Release reach: TB.6 needs no tag (`release-web.yml` and docs, live on main). Every `v*` tag publishes a `latest.json` that every updater-era install (v1.3.3+) fetches on its next launch. One-way doors: the workflow never overwrites a published asset (`:271-276`, policy only, since GitHub immutable releases are off), so a bad manifest can be deleted to silence the offer, but an update an app already installed can't be recalled and the fix is a newer patch tag; the cargo-packager 0.11.8 pin (`:172`) and the `macos-aarch64` key are a contract with installed apps; and the signing key can't rotate without a bridge release (signed with the old key, embedding the new pubkey), while losing it orphans every install with no bridge possible (`docs/packaging.md:186-190`).
- External: repo secret `CARGO_PACKAGER_SIGN_PRIVATE_KEY` plus the Apple signing secrets (updater artifacts ship only when both are present, `:187`); tag ruleset 19646314 (gates on `build`/`kani`/`miri`/`asan`/`boot-gate`, not yet `windows`, see TA.4); the repo staying public (the cross-host redirect strips auth); upstream cargo-packager-updater (#350 manifest, #397 sandbox). hotchkiss.io's pin and the OpenSCAD/BOSL2 pins are untouched, because the updater is macOS-only.
