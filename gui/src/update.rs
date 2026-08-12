//! TB: macOS self-update. On launch (windowed, packaged builds only) a background task fetches the
//! release manifest (`latest.json` on the newest GitHub release) via `cargo-packager-updater`; a
//! newer version raises a gold "UPDATE x.y.z" badge in the header. Install is PROMPTED, never
//! silent: the badge opens a dialog, the dialog downloads + minisign-verifies the `.app.tar.gz`
//! (the stapled bundle cargo-packager tarred at release time), swaps the bundle in place, and
//! relaunches into the same model. macOS only — the one platform with signed updater artifacts.
//!
//! Failure posture: the launch check NEVER nags — errors go to the console/log and the badge just
//! doesn't appear (a 404 during the release-upload window looks exactly like "no update"). Only a
//! MANUAL check (Settings -> "Check for updates") reports errors and "up to date" in your face.
//!
//! Known limits, inherited from the updater crate (docs/packaging.md "auto-update"): the swap is
//! not atomic (no rollback if the second rename fails — the DMG remains the recovery path), and a
//! translocated/DMG-run app cannot self-update (the dialog detects that and says so instead of
//! failing after a 40 MB download).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cargo_packager_updater::{Config, Update, UpdaterBuilder, semver::Version, url::Url};

use crate::console::{self, Kind};
use crate::theme;
use crate::*;

/// The static manifest on whatever release is currently `latest` — a plain web redirect (NOT the
/// GitHub API, so no rate limits). CI writes it via `packaging/macos/make-update-manifest.sh`.
const ENDPOINT: &str = "https://github.com/chotchki/fab-scad/releases/latest/download/latest.json";

/// Where to send a human when self-update can't run (translocated app, failed swap).
const RELEASES_URL: &str = "https://github.com/chotchki/fab-scad/releases/latest";

/// The minisign public key matching CI's `CARGO_PACKAGER_SIGN_PRIVATE_KEY` secret (base64 of the
/// `.pub` box, cargo-packager's storage format). Baked in: an update installs only if its bytes
/// verify against this. Rotating the private key ORPHANS every installed app — see the key
/// ceremony in docs/packaging.md before touching it.
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDM2NjkzQjk3MDEwNThGMjYKUldRbWp3VUJsenRwTmx3UlcrRnR6RkQvVEh2cTlCL0drcFhUUnFxRENia1B0Y2daaGlJM21aMloK";

/// Download progress the install task streams and the UI reads — plain atomics because the writer
/// is an off-thread closure and the reader is a per-frame system.
#[derive(Default)]
pub(crate) struct Progress {
    got: AtomicU64,
    total: AtomicU64,
}

/// Self-update state: at most one check and one install in flight, plus the found update that
/// drives the header badge. `found` stays until an install succeeds, so the badge survives a
/// dismissed dialog.
#[derive(Resource, Default)]
pub(crate) struct UpdateState {
    check: Option<Task<Result<Option<Update>, String>>>,
    install: Option<Task<Result<(), String>>>,
    found: Option<Update>,
    dialog_open: bool,
    /// User-initiated check (Settings button): report "up to date" and errors loudly. The launch
    /// check reports nothing but a badge.
    manual: bool,
    error: Option<String>,
    progress: Option<Arc<Progress>>,
}

impl UpdateState {
    /// The available version string, if the check found one — the header badge's text + gate.
    pub(crate) fn found_version(&self) -> Option<&str> {
        self.found.as_ref().map(|u| u.version.as_str())
    }
}

/// True for the opt-out spellings of `FAB_UPDATE_CHECK`. Unset or anything else means check.
fn opted_out_value(v: Option<&str>) -> bool {
    matches!(v, Some("0" | "off" | "false" | "no"))
}

fn opted_out() -> bool {
    let v = std::env::var("FAB_UPDATE_CHECK").ok();
    opted_out_value(v.as_deref())
}

/// The `.app` bundle root an exe path sits in, if it sits in one. `None` for a bare cargo build —
/// there is nothing to swap, so no check runs.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    let bundle = contents.parent()?;
    (macos_dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// Gatekeeper app translocation runs the bundle from a read-only randomized mount — the swap can
/// only fail there. Detect it up front and say "install to /Applications" instead.
fn translocated(bundle: &Path) -> bool {
    bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
}

/// True when the swap is DOOMED before the download: translocation, or the bundle living on a
/// different device than the temp dir the updater stages in — its first move is a plain
/// `fs::rename` into `$TMPDIR`, which cannot cross devices, so a DMG-mounted (even unquarantined)
/// or external-volume install fails with EXDEV after the full download. Cheaper to say so up front.
fn cannot_self_update(bundle: &Path) -> bool {
    if translocated(bundle) {
        return true;
    }
    use std::os::unix::fs::MetadataExt;
    match (bundle.metadata(), std::env::temp_dir().metadata()) {
        (Ok(b), Ok(t)) => b.dev() != t.dev(),
        _ => false, // can't tell — let the install try and report honestly
    }
}

fn current_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?)
}

fn current_version() -> Version {
    // CARGO_PKG_VERSION is always valid semver — cargo enforces it at build time.
    Version::parse(env!("CARGO_PKG_VERSION")).expect("cargo version is semver")
}

/// Off-thread check. Blocking reqwest inside an AsyncComputeTaskPool task — the `publish_native`
/// upload precedent (the pool thread is not an async runtime, which blocking reqwest requires).
fn spawn_check() -> Task<Result<Option<Update>, String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        let config = Config {
            endpoints: vec![Url::parse(ENDPOINT).expect("endpoint const parses")],
            pubkey: PUBKEY.into(),
            windows: None,
        };
        UpdaterBuilder::new(current_version(), config)
            .build()
            .and_then(|u| u.check())
            .map_err(|e| e.to_string())
    })
}

/// Off-thread download + verify + swap. The updater verifies the minisign signature over the raw
/// tar.gz bytes in `download()` BEFORE `install()` touches the bundle.
fn spawn_install(up: Update, progress: Arc<Progress>) -> Task<Result<(), String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        let bytes = up
            .download_extended(
                |chunk, total| {
                    progress.got.fetch_add(chunk as u64, Ordering::Relaxed);
                    if let Some(t) = total {
                        progress.total.store(t, Ordering::Relaxed);
                    }
                },
                || {},
            )
            .map_err(|e| format!("download: {e}"))?;
        up.install(bytes).map_err(|e| format!("install: {e}"))
    })
}

/// Respawn the (now swapped) bundle binary on the same model, then exit. The exe PATH is unchanged
/// by the swap — `current_exe` points at the NEW build. Never returns.
fn relaunch_and_exit(scene: &SceneCfg) -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        if let Some(src) = scene.source.as_ref().or(scene.stl.as_ref()) {
            cmd.arg(src);
        }
        if let Err(e) = cmd.spawn() {
            error!("relaunch failed (start the app manually): {e}");
        }
    }
    std::process::exit(0);
}

/// Launch check — packaged builds only, `FAB_UPDATE_CHECK=off` to disable. Startup, windowed app.
pub(crate) fn update_check_startup(mut st: ResMut<UpdateState>) {
    if opted_out() || current_bundle().is_none() {
        return;
    }
    st.manual = false;
    st.check = Some(spawn_check());
}

/// Drive the check/install tasks + the manual-check command. `Update` schedule.
pub(crate) fn update_action(
    mut ev: MessageReader<PanelCmd>,
    mut st: ResMut<UpdateState>,
    mut status: ResMut<Status>,
    scene: Res<SceneCfg>,
) {
    if ev.read().any(|c| *c == PanelCmd::CheckUpdates) {
        if current_bundle().is_none() {
            status.0 = "update check: dev build (not a .app bundle) — nothing to update".into();
        } else {
            // A click while the LAUNCH check is still in flight promotes that check to manual
            // rather than being swallowed — its result then reports loudly instead of silently
            // (review finding: the old `&& st.check.is_none()` guard consumed the message first).
            st.manual = true;
            status.0 = "checking for updates...".into();
            if st.check.is_none() {
                st.error = None;
                st.check = Some(spawn_check());
            }
        }
    }

    if let Some(task) = st.check.as_mut()
        && let Some(res) = block_on(future::poll_once(task))
    {
        st.check = None;
        match res {
            Ok(Some(up)) => {
                let msg = format!(
                    "update available: {} (you have {})",
                    up.version, up.current_version
                );
                console::push(Kind::Scad, msg.clone());
                status.0 = msg;
                st.found = Some(up);
                if st.manual {
                    st.dialog_open = true;
                }
            }
            Ok(None) => {
                info!("update check: up to date");
                if st.manual {
                    status.0 = format!("up to date ({})", current_version());
                }
            }
            Err(e) => {
                // The launch check stays quiet: a release mid-upload 404s here, and offline is not
                // an error worth a status line. Manual checks get the truth in the dialog.
                info!("update check failed: {e}");
                if st.manual {
                    st.error = Some(e);
                    st.dialog_open = true;
                }
            }
        }
    }

    if st.install.is_some()
        && let Some(p) = st.progress.as_ref()
    {
        let (got, total) = (
            p.got.load(Ordering::Relaxed),
            p.total.load(Ordering::Relaxed),
        );
        status.0 = if total > 0 && got < total {
            format!("downloading update... {}%", got * 100 / total)
        } else {
            "installing update...".into()
        };
    }
    if let Some(task) = st.install.as_mut()
        && let Some(res) = block_on(future::poll_once(task))
    {
        st.install = None;
        st.progress = None;
        match res {
            Ok(()) => {
                console::push(Kind::Scad, "update installed — restarting");
                relaunch_and_exit(&scene);
            }
            Err(e) => {
                console::push(Kind::Scad, format!("update failed: {e}"));
                error!("update failed: {e}");
                st.error = Some(e);
                st.dialog_open = true;
            }
        }
    }
}

/// The update dialog — raised by the header badge ([`PanelCmd::OpenUpdate`]) or a manual check's
/// result. Egui pass, modal, same idiom as [`crate::settings`].
pub(crate) fn update_dialog(
    mut contexts: EguiContexts,
    mut ev: MessageReader<PanelCmd>,
    mut st: ResMut<UpdateState>,
) {
    if ev.read().any(|c| *c == PanelCmd::OpenUpdate) {
        st.dialog_open = true;
    }
    if !st.dialog_open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let installing = st.install.is_some();
    let mut still_open = true;
    let mut kick_install = false;
    let modal = egui::Modal::new(egui::Id::new("update_modal")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(theme::chrome("Software update", 18.0).color(theme::NAVY));
        ui.separator();

        match (&st.found, &st.error) {
            (Some(up), _) => {
                ui.label(
                    egui::RichText::new(format!(
                        "fab-scad {} is available — you have {}",
                        up.version, up.current_version
                    ))
                    .strong()
                    .color(theme::NAVY),
                );
                if let Some(notes) = up.body.as_deref().filter(|n| !n.trim().is_empty()) {
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(120.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(notes).small());
                        });
                }
                ui.add_space(6.0);

                let doomed = current_bundle().is_some_and(|b| cannot_self_update(&b));
                if doomed {
                    // Downloading first would just fail at the swap — say why up front.
                    ui.label(
                        egui::RichText::new(
                            "The app is running from its DMG, a translocated mount, or a volume \
                             the updater can't stage across, so it can't replace itself. Copy \
                             fab-scad.app to /Applications and relaunch, or grab the new DMG:",
                        )
                        .color(theme::GOLD_DIM),
                    );
                    ui.hyperlink_to("releases page", RELEASES_URL);
                    if ui.button("Close").clicked() {
                        still_open = false;
                    }
                } else if installing {
                    ui.label("installing — the app restarts when it's done");
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Installs in place and restarts. Unsaved editor changes are lost.",
                        )
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let install = ui.add(
                            egui::Button::new(
                                theme::chrome("Install and relaunch", 14.0).color(theme::NAVY),
                            )
                            .fill(theme::GOLD),
                        );
                        if install.clicked() {
                            kick_install = true;
                        }
                        if ui.button("Later").clicked() {
                            still_open = false;
                        }
                    });
                }
            }
            (None, Some(_)) => {} // error-only dialog: the message below carries it
            (None, None) => {
                ui.label(format!("up to date ({})", current_version()));
                if ui.button("Close").clicked() {
                    still_open = false;
                }
            }
        }

        if let Some(e) = &st.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("update failed: {e}"))
                    .small()
                    .color(theme::GOLD_DIM),
            );
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("get it manually:").small());
                ui.hyperlink_to("releases page", RELEASES_URL);
            });
            // The error-only state (failed MANUAL check: found=None) has no other button — without
            // this, the modal closes only via Esc/backdrop, which nothing hints at.
            if st.found.is_none() && ui.button("Close").clicked() {
                still_open = false;
            }
        }
    });
    if modal.should_close() && !installing {
        still_open = false;
    }

    if kick_install && st.install.is_none() {
        let progress = Arc::new(Progress::default());
        st.progress = Some(progress.clone());
        st.error = None;
        if let Some(up) = st.found.clone() {
            st.install = Some(spawn_install(up, progress));
        }
    }
    st.dialog_open = still_open;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection_wants_the_full_bundle_shape() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/fab-scad.app/Contents/MacOS/fab-gui"
            )),
            Some(PathBuf::from("/Applications/fab-scad.app"))
        );
        // The CLI riding along in Contents/MacOS resolves to the same bundle.
        assert_eq!(
            bundle_of(Path::new("/Applications/fab-scad.app/Contents/MacOS/fab")),
            Some(PathBuf::from("/Applications/fab-scad.app"))
        );
        assert_eq!(bundle_of(Path::new("/repo/target/release/fab-gui")), None);
        assert_eq!(
            bundle_of(Path::new("/tmp/fab-scad.app/MacOS/fab-gui")),
            None,
            "missing Contents/ is not a bundle"
        );
    }

    #[test]
    fn translocation_detected_by_path_marker() {
        assert!(translocated(Path::new(
            "/private/var/folders/ab/T/AppTranslocation/1F2E/d/fab-scad.app"
        )));
        assert!(!translocated(Path::new("/Applications/fab-scad.app")));
    }

    #[test]
    fn same_device_as_tempdir_can_self_update() {
        // The temp dir trivially shares a device with itself: the cross-device doom check must not
        // fire there (a false positive would block updates on every stock install).
        assert!(!cannot_self_update(&std::env::temp_dir()));
        // A nonexistent bundle can't be judged — fall through and let the install report.
        assert!(!cannot_self_update(Path::new("/nonexistent/fab-scad.app")));
    }

    #[test]
    fn opt_out_spellings() {
        for v in ["0", "off", "false", "no"] {
            assert!(opted_out_value(Some(v)), "{v} should opt out");
        }
        assert!(!opted_out_value(None));
        assert!(!opted_out_value(Some("1")));
        assert!(!opted_out_value(Some("on")));
    }

    #[test]
    fn compiled_version_is_semver() {
        // Fails the build-time contract loudly if a Cargo.toml version ever stops parsing.
        let _ = current_version();
    }

    #[test]
    fn pubkey_is_a_minisign_box() {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(PUBKEY)
            .expect("pubkey is base64");
        let text = String::from_utf8(decoded).expect("pubkey box is utf8");
        assert!(
            text.starts_with("untrusted comment:"),
            "expected a minisign public-key box, got: {text:.40}"
        );
    }
}
