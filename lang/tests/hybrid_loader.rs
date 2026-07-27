//! SW.1 — the HYBRID loader contract: `use`/`include` consult the in-memory overlay (a project's
//! live buffers) BEFORE the filesystem, workspace libraries come off disk, and a disk-resolved
//! library's own includes never see the overlay. This is the seam that lets the native GUI render
//! unsaved edits with NO shadow mirror (phase SW) — the overlay IS the project truth, the fs is
//! everything else.
//!
//! Fixtures materialize per-test into `CARGO_TARGET_TMPDIR` (the `loader_corpus` pattern); values
//! assert through `echo` messages so no geometry backend is involved.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fab_lang::{Config, Message, resolve_geometry_hybrid_full};

/// A per-test fixture dir under the cargo tmp root — unique by name so parallel tests don't collide.
fn fixture_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hybrid_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fixture dir");
    dir
}

/// Build an overlay map from `(relative path, contents)` pairs.
fn overlay(entries: &[(&str, &str)]) -> BTreeMap<PathBuf, String> {
    entries
        .iter()
        .map(|(p, s)| (PathBuf::from(p), (*s).to_string()))
        .collect()
}

/// Run the hybrid entry with no JIT and a reader that refuses (no test here imports meshes),
/// returning the run's messages. Panics on failure — every case here is expected to render.
fn run(source: &str, base_dir: &Path, ovl: &BTreeMap<PathBuf, String>) -> Vec<Message> {
    let (_, messages) =
        resolve_geometry_hybrid_full(source, base_dir, ovl, &[], None, Config::default(), |raw| {
            panic!("unexpected import of '{raw}'")
        })
        .expect("hybrid run renders");
    messages
}

/// Did the run echo exactly this text?
fn echoed(messages: &[Message], want: &str) -> bool {
    messages
        .iter()
        .any(|m| matches!(m, Message::Echo(e) if e.contains(want)))
}

/// An overlay-only library binds: nothing on disk, the live buffer serves the include.
#[test]
fn the_overlay_serves_a_live_buffer() {
    let dir = fixture_dir("overlay_hit");
    let ovl = overlay(&[("lib.scad", "function s() = 3;\n")]);
    let messages = run("use <lib.scad>\necho(s());\n", &dir, &ovl);
    assert!(
        echoed(&messages, "3"),
        "overlay lib must bind: {messages:?}"
    );
}

/// A disk-only library binds through the fs fallback — both `base_dir`-relative and via a
/// `library_paths`-style ref living under `base_dir` (the BOSL2 shape once geomsvc passes roots).
#[test]
fn the_fs_fallback_reaches_disk_libraries() {
    let dir = fixture_dir("fs_fallback");
    std::fs::write(dir.join("disklib.scad"), "function d() = 7;\n").unwrap();
    let messages = run("use <disklib.scad>\necho(d());\n", &dir, &BTreeMap::new());
    assert!(echoed(&messages, "7"), "disk lib must bind: {messages:?}");
}

/// The same relative path in BOTH worlds: the overlay (the live, possibly-unsaved buffer) wins
/// over the stale disk copy — the whole point of the hybrid.
#[test]
fn the_overlay_shadows_the_disk() {
    let dir = fixture_dir("overlay_wins");
    std::fs::write(dir.join("lib.scad"), "function s() = 5;\n").unwrap();
    let ovl = overlay(&[("lib.scad", "function s() = 3;\n")]);
    let messages = run("use <lib.scad>\necho(s());\n", &dir, &ovl);
    assert!(
        echoed(&messages, "3") && !echoed(&messages, "5"),
        "the live buffer must beat the disk copy: {messages:?}"
    );
}

/// A missing library warns and renders on (M.6.1 parity with both sibling drivers) — never a
/// hard failure.
#[test]
fn a_missing_library_warns_and_renders_on() {
    let dir = fixture_dir("missing");
    let messages = run("use <nope.scad>\necho(1);\n", &dir, &BTreeMap::new());
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Warning(w) if w.contains("Can't open library"))),
        "missing lib must warn: {messages:?}"
    );
    assert!(echoed(&messages, "1"), "and the run renders on");
}

/// The review's blocker, pinned: NO phantom project-root search tier. A subdir file's
/// `include <colors.scad>` must resolve exactly as the fs driver would — requesting dir, then
/// library paths — never a like-named buffer at the project ROOT (which would make the GUI and
/// `fab render` disagree about the same saved project).
#[test]
fn a_subdir_ref_never_reaches_the_project_root() {
    let dir = fixture_dir("no_root_tier");
    let ovl = overlay(&[
        ("parts/bracket.scad", "include <colors.scad>\necho(c());\n"),
        ("colors.scad", "function c() = 666;\n"), // the root buffer that must NOT bind
    ]);
    let messages = run("include <parts/bracket.scad>\n", &dir, &ovl);
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Warning(w) if w.contains("Can't open library"))),
        "with nothing at parts/colors.scad and no lib, the ref must MISS like the fs driver: {messages:?}"
    );
    assert!(
        !echoed(&messages, "666"),
        "the project-root buffer must not shadow: {messages:?}"
    );
}

/// The subtree stays in the overlay world: a project file served from DISK (present on disk,
/// absent from the overlay — e.g. created after the project opened) re-enters the virtual world,
/// so ITS includes still see live buffers.
#[test]
fn a_disk_project_file_reenters_the_overlay_world() {
    let dir = fixture_dir("reentry");
    std::fs::write(dir.join("disk_only.scad"), "include <edited.scad>\n").unwrap();
    std::fs::write(dir.join("edited.scad"), "function v() = 5;\n").unwrap(); // stale save
    let ovl = overlay(&[("edited.scad", "function v() = 3;\n")]); // live buffer
    let messages = run("include <disk_only.scad>\necho(v());\n", &dir, &ovl);
    assert!(
        echoed(&messages, "3") && !echoed(&messages, "5"),
        "the disk-served project file's include must still hit the live buffer: {messages:?}"
    );
}

/// Overlay keys normalize at the driver: a `./`-prefixed key (a plausible SW.2 join artifact)
/// still matches instead of silently degrading every probe to the fs.
#[test]
fn a_dot_prefixed_overlay_key_still_matches() {
    let dir = fixture_dir("dot_key");
    let ovl = overlay(&[("./lib.scad", "function s() = 3;\n")]);
    let messages = run("use <lib.scad>\necho(s());\n", &dir, &ovl);
    assert!(
        echoed(&messages, "3"),
        "normalized key must bind: {messages:?}"
    );
}

/// A case-mismatched ref on a case-insensitive fs (APFS default) must still land on the LIVE
/// buffer: the fs fallback canonicalizes to the true-case path, which re-enters the overlay.
/// On a case-sensitive fs the ref misses entirely — warn-and-render-on, the fs driver's own
/// behavior — so the assertion is gated on what the fs actually does.
#[test]
fn a_case_mismatched_ref_still_prefers_the_live_buffer() {
    let dir = fixture_dir("case_fold");
    std::fs::write(dir.join("util.scad"), "function v() = 5;\n").unwrap();
    let ovl = overlay(&[("util.scad", "function v() = 3;\n")]);
    let case_insensitive = dir.join("UTIL.scad").canonicalize().is_ok();
    let messages = run("include <UTIL.scad>\necho(v());\n", &dir, &ovl);
    if case_insensitive {
        assert!(
            echoed(&messages, "3") && !echoed(&messages, "5"),
            "true-case canonicalization must re-enter the overlay: {messages:?}"
        );
    } else {
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Warning(w) if w.contains("Can't open library"))),
            "case-sensitive fs: the mismatch misses, warn-and-render-on: {messages:?}"
        );
    }
}

/// The precedence rule's sharp edge: a library resolved from DISK keeps disk semantics — its own
/// includes never consult the overlay, so a project buffer named like a library-internal file
/// cannot shadow it.
#[test]
fn a_disk_library_include_never_consults_the_overlay() {
    let dir = fixture_dir("no_shadow");
    // The disk library lives OUTSIDE the project dir (a workspace lib root), includes its own
    // `inner.scad`, and the overlay carries a poisoned `inner.scad` at the same relative name.
    let libroot = dir.join("libs");
    std::fs::create_dir_all(&libroot).unwrap();
    std::fs::write(libroot.join("outer.scad"), "include <inner.scad>\n").unwrap();
    std::fs::write(libroot.join("inner.scad"), "function v() = 42;\n").unwrap();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let ovl = overlay(&[("inner.scad", "function v() = 666;\n")]);
    let (_, messages) = resolve_geometry_hybrid_full(
        "include <outer.scad>\necho(v());\n",
        &project,
        &ovl,
        std::slice::from_ref(&libroot),
        None,
        Config::default(),
        |raw| panic!("unexpected import of '{raw}'"),
    )
    .expect("renders");
    assert!(
        echoed(&messages, "42") && !echoed(&messages, "666"),
        "the disk lib's own include must resolve on disk, not the overlay: {messages:?}"
    );
}
