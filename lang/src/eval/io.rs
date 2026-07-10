//! The IO shell (M.4) — the ONE place fab-lang touches the filesystem. Everything else (parser,
//! evaluator, loader) is PURE: it consumes source text + mesh tables handed IN and hands NEEDS back
//! ([`resolve_source`](super::resolve_source) returns a [`Resolution`](super::Resolution)). This module is
//! the impure boundary that DRIVES that fixpoint — it fulfills each need (`find_valid_path` + read for a
//! `use`/`include` source, a caller-supplied reader for an `import`/`surface` mesh), augments the tables,
//! and re-runs until the run closes. A pure-wasm build drops this module wholesale and drives the same
//! fixpoint from an async host instead (awaiting between rounds where this sync loop reads inline).
//!
//! Its lines are DOCUMENTED off the pure-core 100% coverage gate: `std::fs` failure paths (a file deleted
//! between resolve and read, a permissions flip) are TOCTOU races the corpus can't reproduce
//! deterministically, and the whole point of the module is that it's the seam the coverage doctrine draws
//! its line at. (The cargo `io` feature + a CI `--no-default-features` lane that PROVES the core compiles
//! without this module wait for the first wasm consumer — fab-web — to keep an unused gate from rotting.)

use std::path::{Path, PathBuf};

use super::loader::{ProvidedSource, SourceMap};
use super::{
    FileTable, Geo, Imported, Message, NumericJitFactory, Resolution, SourceNeed, resolve_source,
};
use crate::parser::{Program, parse};

/// An empty program — the stand-in a tolerated missing/broken `use`/`include` contributes (no statements,
/// no defs), so the graph closes and the run renders on (M.6.1).
fn empty_program() -> Program {
    Program { stmts: Vec::new() }
}

/// Read a root `.scad` file to source text — the entry read behind [`evaluate_file`](crate::evaluate_file)
/// and kin. Split out here so the crate's every `std::fs` call lives in this one module.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if the path can't be read.
pub(crate) fn read_source(path: &Path) -> crate::Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| crate::Error::Load(format!("{}: {e}", path.display())))
}

/// Drive the needs fixpoint to completion — the outer loop behind every native entry point. Loops the pure
/// [`resolve_source`]: on [`Resolution::Incomplete`] it fulfills each need (a `use`/`include` source via
/// `find_valid_path` + read + parse-once; an `import`/`surface` mesh via `mesh_reader`), augments the
/// tables, and re-runs — until [`Resolution::Complete`]. Progress is guaranteed: every surfaced need is
/// absent from the tables (the resolver/eval only name what they're missing), so each round either grows a
/// table or fails LOUD, bounded by the total sources. `root_path` is the root's own path when it's a file
/// (canonicalized here for back-reference dedup + cycle-break).
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) for an unresolvable/unreadable `use`/`include` or a `mesh_reader`
/// failure; [`Error::Parse`](crate::Error::Parse) for a malformed source; any evaluation error.
pub(crate) fn drive<R>(
    source: &str,
    base_dir: &Path,
    root_path: Option<&Path>,
    library_paths: &[PathBuf],
    jit_factory: Option<&dyn NumericJitFactory>,
    config: super::Config,
    mut mesh_reader: R,
) -> crate::Result<(Geo, Vec<Message>)>
where
    R: FnMut(&str) -> crate::Result<Imported>,
{
    let root_id = root_path.and_then(|p| std::fs::canonicalize(p).ok());
    let mut scad = SourceMap::new();
    let mut files = FileTable::new();
    // Loader warnings (a missing/broken `use`/`include`, M.6.1) are emitted at LOAD time, BEFORE the run's
    // echoes — so we accumulate them across rounds and prepend them to the eval messages when the run closes.
    let mut warnings: Vec<Message> = Vec::new();
    loop {
        match resolve_source(
            source,
            base_dir,
            root_id.as_deref(),
            &scad,
            &files,
            jit_factory,
            config,
        )? {
            Resolution::Complete { geo, messages } => {
                warnings.extend(messages);
                return Ok((geo, warnings));
            }
            Resolution::Incomplete { needs } => {
                for need in needs {
                    fulfill(
                        need,
                        library_paths,
                        &mut mesh_reader,
                        &mut scad,
                        &mut files,
                        &mut warnings,
                    )?;
                }
            }
        }
    }
}

/// Fulfill one [`SourceNeed`] into its table: a `Scad` reference is resolved (`find_valid_path`), read, and
/// parsed ONCE (cache the AST so the resolver clones it, not re-lexes a big library each pass); a `File`
/// reference is handed to `mesh_reader`. An already-present key is a duplicate reference in the same round —
/// skip it (the fulfill is idempotent). A source that can't be resolved/read/parsed, or a reader failure,
/// fails LOUD.
fn fulfill<R>(
    need: SourceNeed,
    library_paths: &[PathBuf],
    mesh_reader: &mut R,
    scad: &mut SourceMap,
    files: &mut FileTable,
    warnings: &mut Vec<Message>,
) -> crate::Result<()>
where
    R: FnMut(&str) -> crate::Result<Imported>,
{
    match need {
        SourceNeed::Scad { from_dir, raw } => {
            let key = (from_dir.clone(), raw.clone());
            if scad.contains_key(&key) {
                return Ok(()); // duplicate reference surfaced this round — already fulfilled
            }
            let Some(id) = resolve(&from_dir, &raw, library_paths) else {
                // TOLERANT (M.6.1): a missing library → warn + an EMPTY program (no statements, no defs), so
                // the graph closes + eval renders ON, matching OpenSCAD's warn-and-render (exit 0). The ROOT
                // is NOT tolerated — it's parsed in `resolve_source`, and a missing/broken root stays LOUD.
                // Exact warning TEXT is #94; this pins the RENDER behavior. The synthetic id (the unresolved
                // path) keeps this missing ref a distinct, self-consistent node in the graph.
                warnings.push(Message::Warning(format!("Can't open library '{raw}'.")));
                let id = from_dir.join(&raw);
                scad.insert(
                    key,
                    ProvidedSource {
                        id,
                        dir: from_dir,
                        program: empty_program(),
                    },
                );
                return Ok(());
            };
            let text = std::fs::read_to_string(&id).map_err(|e| {
                // TOCTOU: `resolve` already canonicalized this as a readable file, so a failure here is a
                // race (deleted / perms flipped). Off the pure core's coverage — this module is the seam.
                crate::Error::Load(format!("{}: {e}", id.display()))
            })?;
            let dir = id.parent().unwrap_or(Path::new(".")).to_path_buf();
            // A parse-broken USED/INCLUDED file is ALSO tolerated (warn + empty) — OpenSCAD renders on. The
            // root's parse (in `resolve_source`) is the only one that stays LOUD.
            let program = parse(&text).unwrap_or_else(|_| {
                warnings.push(Message::Warning(format!("Failed to parse '{raw}'.")));
                empty_program()
            });
            scad.insert(key, ProvidedSource { id, dir, program });
        }
        SourceNeed::File { raw } => {
            // No dedup guard needed: `Ctx::request_file` accumulates File needs in a `BTreeSet`, so each
            // `raw` surfaces at most once per round, and a fulfilled one never re-surfaces (the table has
            // it) — unlike `Scad`, where a diamond can name the same lib twice in one pass.
            let imported = mesh_reader(&raw)?;
            files.insert(raw, imported);
        }
    }
    Ok(())
}

/// The mesh reader the no-import convenience entries pass: any `import`/`surface` file is a LOUD error,
/// since those entries deliberately carry no reader (pure geometry only). A named error, never a
/// silently-empty mesh — real meshes flow through `resolve_geometry_*` + a reader (the M.5 backend).
///
/// # Errors
/// Always [`Error::Load`](crate::Error::Load): reaching this means an `import`/`surface` executed on an
/// entry point that has no way to read the file.
pub(super) fn no_import_reader(raw: &str) -> crate::Result<Imported> {
    Err(crate::Error::Load(format!(
        "import/surface references '{raw}', but this entry point supplies no mesh reader — evaluate \
         through resolve_geometry_* with a reader (the M.5 STL/3MF/heightmap backend)"
    )))
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
