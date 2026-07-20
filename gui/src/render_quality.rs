//! W.3.25.2: the live view's Draft|Final quality — a process GLOBAL (like the console buffer) the render
//! kicks read, so the status-bar toggle flips it WITHOUT threading `quality` through the whole kick chain.
//! Session-scoped (defaults Draft each launch); export/save ignore it (always Final, W.3.25.1).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::Quality;

/// `false` = Draft (the fast default); `true` = Final (see the real smoothness in the live view).
static FINAL: AtomicBool = AtomicBool::new(false);

/// The live-view quality the render kicks inject.
pub(crate) fn current() -> Quality {
    if FINAL.load(Ordering::Relaxed) {
        Quality::Final
    } else {
        Quality::Draft
    }
}

/// Is the live view set to Final?
pub(crate) fn is_final() -> bool {
    FINAL.load(Ordering::Relaxed)
}

/// Set the live-view quality (the status-bar toggle).
pub(crate) fn set(final_quality: bool) {
    FINAL.store(final_quality, Ordering::Relaxed);
}
