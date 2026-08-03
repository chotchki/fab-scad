//! SZ.2 — pack `libs.json` from THE SAME library list the registry is built from.
//!
//!   cargo run --bin pack_libs -- gui/web/libs.json
//!
//! Replaces `packaging/web/pack_scad_libs.py`, which globbed a hand-written list of directories.
//! That made the browser's library SOURCE and the build's transpiled ROWS two independent
//! declarations, and they drifted the moment a third library appeared: MCAD joined the `kernel`
//! feature, so its natives compiled into the bundle while its `.scad` never entered `libs.json` —
//! `include <MCAD/units.scad>` worked natively and silently rendered nothing in the browser,
//! because a missing import costs a PART rather than an error.
//!
//! Now both halves read [`fab_scad::import::libraries`]. A library that is not declared there is
//! not packed AND not registered; one that is, is both. The drift is not fixed, it is
//! unrepresentable.
//!
//! Output is the same shape the app already fetches — a flat `{path: text}` JSON keyed by exactly
//! what an `include <...>` resolves to (`BOSL2/std.scad`, or a bare name for scad-lib) — so
//! `gui/src/lib_fetch.rs` needs no change. Assets (`.svg`) ride along for `import()`/`surface()`,
//! matched by basename in the worker; binary meshes would need a byte channel a text pack lacks and
//! are still out (unchanged from the python).
//!
//! Deterministic: entries are emitted in sorted order so a rebuild with no source change produces
//! byte-identical output, which is what lets the release manifest's sha256 mean anything.

#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a build-time CLI: stdout/stderr ARE its interface"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let out = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => bail!("usage: pack_libs <out.json>"),
    };
    // The repo root: this binary is built from it, so the manifest dir IS it.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut pack: BTreeMap<String, String> = BTreeMap::new();
    let mut report: Vec<(String, usize, u64)> = Vec::new();

    for lib in fab_scad::import::libraries() {
        let dir = root.join(lib.source_dir);
        if !dir.is_dir() {
            // A declared library with no source on disk is a REAL hole — the registry will carry
            // its rows while the browser cannot resolve a single include against it, which is the
            // exact asymmetry this binary exists to prevent. Loud, and fatal: unlike a missing
            // submodule at BUILD time (where declaring nothing is the honest answer), packing is
            // deliberate and a silent short pack ships a broken bundle.
            bail!(
                "library `{}` is declared but {} does not exist — either check out the submodule \
                 or remove it from `import::libraries()`",
                lib.name,
                dir.display()
            );
        }
        let (mut files, mut bytes) = (0_usize, 0_u64);
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let path = entry?.path();
            let is_source = path.extension().is_some_and(|e| e == "scad" || e == "svg");
            if !is_source {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            // Lossy on purpose, matching the python it replaces: a stray non-UTF8 byte in a library
            // comment should not fail the whole pack.
            let text = String::from_utf8_lossy(
                &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .into_owned();
            bytes += text.len() as u64;
            files += 1;
            pack.insert(format!("{}{name}", lib.prefix), text);
        }
        report.push((lib.name.to_string(), files, bytes));
    }

    // The web demo: a tiny self-contained module the sourceless web boot renders, proving the
    // fetch -> closure -> worker path without betting on a full BOSL2 eval. Not a declared library
    // (it has no rows and nothing includes it by name), so it is packed explicitly.
    let demo = root.join("packaging/web/web-demo");
    if demo.is_dir() {
        for entry in std::fs::read_dir(&demo)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "scad")
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                pack.insert(name.to_string(), std::fs::read_to_string(&path)?);
            }
        }
    }

    let json = serde_json::to_string(&pack).context("serialize the pack")?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&out, &json).with_context(|| format!("write {}", out.display()))?;

    for (name, files, bytes) in &report {
        eprintln!(
            "  {name:16} {files:3} files  {:.2} MB",
            *bytes as f64 / 1_048_576.0
        );
    }
    eprintln!(
        "packed {} entries -> {} ({:.2} MB)",
        pack.len(),
        out.display(),
        json.len() as f64 / 1_048_576.0
    );
    Ok(())
}
