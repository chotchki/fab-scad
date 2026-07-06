//! The IO shell (M.4) — the ONE place fab-lang touches the filesystem. Everything else (parser,
//! evaluator, loader) is PURE: it consumes source text + mesh tables handed IN and hands NEEDS back. This
//! module is the impure boundary that fulfills those needs — `find_valid_path` + read for `use`/`include`
//! sources, a caller-supplied reader for `import`/`surface` meshes — so a pure-wasm build can drop this
//! module wholesale and drive the fixpoint from an async host instead.
//!
//! Its lines are DOCUMENTED off the pure-core 100% coverage gate: `std::fs` failure paths (a file deleted
//! between resolve and read, a permissions flip) are TOCTOU races the corpus can't reproduce
//! deterministically, and the whole point of the module is that it's the seam the coverage doctrine draws
//! its line at. (The cargo `io` feature + a CI `--no-default-features` lane that PROVES the core compiles
//! without this module wait for the first wasm consumer — fab-web — to keep an unused gate from rotting.)

use std::path::{Path, PathBuf};

use super::loader::{self, GraphOutcome, Loaded, ProvidedSource, SourceMap};
use crate::parser::parse;

/// Read a root `.scad` file to source text — the entry read behind [`evaluate_file`](crate::evaluate_file)
/// and kin. Split out here so the crate's every `std::fs` call lives in this one module.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if the path can't be read.
pub(crate) fn read_source(path: &Path) -> crate::Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| crate::Error::Load(format!("{}: {e}", path.display())))
}

/// Load `source` (base directory `base_dir`) and everything it reaches via `use`/`include`, resolving
/// against `library_paths` after the including file's own directory — the STATIC parse-time fixpoint over
/// the include graph. Runs the pure [`loader::resolve_graph`]: the resolver names the references it can't
/// satisfy, this shell reads them (`find_valid_path` + read + parse-once), re-runs, until the graph closes.
/// `root_path` is the root's own file path when it has one (`evaluate_file`) so a dependency referencing the
/// root back dedups to the SAME node — parse-once + cycle-break instead of a re-parse; `None` for an
/// in-memory buffer (`evaluate_with_base`), which nothing on disk can name.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if a `use`/`include` target can't be resolved or read;
/// [`Error::Parse`](crate::Error::Parse) if the root or any loaded file fails to parse.
pub(super) fn load_graph(
    source: &str,
    base_dir: &Path,
    root_path: Option<&Path>,
    library_paths: &[PathBuf],
) -> crate::Result<Loaded> {
    let root_id = root_path.and_then(|p| std::fs::canonicalize(p).ok());
    let mut provided = SourceMap::new();
    loop {
        match loader::resolve_graph(source, base_dir, root_id.as_deref(), &provided)? {
            GraphOutcome::Complete(loaded) => return Ok(loaded),
            GraphOutcome::Incomplete(needs) => {
                for need in needs {
                    let key = (need.from_dir.clone(), need.raw.clone());
                    if provided.contains_key(&key) {
                        continue; // already satisfied this round (a duplicate need in the same pass)
                    }
                    let id = resolve(&need.from_dir, &need.raw, library_paths).ok_or_else(|| {
                        crate::Error::Load(format!(
                            "can't find '{}' from {}",
                            need.raw,
                            need.from_dir.display()
                        ))
                    })?;
                    let text = std::fs::read_to_string(&id).map_err(|e| {
                        // Defensive, never-panic: `resolve` already canonicalized this as a readable file,
                        // so a failure here is a TOCTOU race (deleted / perms changed between resolve and
                        // read). Off the pure core's coverage — this whole module is the IO seam.
                        crate::Error::Load(format!("{}: {e}", id.display()))
                    })?;
                    // Parse ONCE here, not on every fixpoint pass: cache the parsed AST so `resolve_graph`
                    // clones it (cheap) instead of re-lexing a big library (`std.scad`'s graph) per pass.
                    let program = parse(&text)?;
                    let dir = id.parent().unwrap_or(Path::new(".")).to_path_buf();
                    provided.insert(key, ProvidedSource { id, dir, program });
                }
            }
        }
    }
}

/// Resolve a `use`/`include` path reference to a canonical file, mirroring OpenSCAD's `find_valid_path_`
/// (`parsersettings.cc`): an absolute reference is checked directly; a relative one resolves against
/// `base_dir` first, then each library path in order — first existing non-directory wins. `None` if no
/// candidate is a readable file. Canonicalizing here makes the result the parse-once + cycle key.
fn resolve(base_dir: &Path, raw: &str, library_paths: &[PathBuf]) -> Option<PathBuf> {
    let local = Path::new(raw);
    if local.is_absolute() {
        return check_file(local);
    }
    if let Some(found) = check_file(&base_dir.join(local)) {
        return Some(found);
    }
    library_paths
        .iter()
        .find_map(|lib| check_file(&lib.join(local)))
}

/// A path is valid iff it canonicalizes (so it exists) to a regular file (OpenSCAD rejects directories).
/// The canonical form dedups symlinks/`..` for the parse-once + cycle keys.
fn check_file(p: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(p) {
        Ok(canon) if canon.is_file() => Some(canon),
        _ => None,
    }
}
