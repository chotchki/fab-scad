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

/// Is this `use`/`include` path a FONT file (AC.1)? Extension check, case-insensitive — the shapes
/// OpenSCAD's font registration accepts (`.ttf`/`.otf`/`.ttc`).
fn is_font_path(raw: &str) -> bool {
    Path::new(raw.trim()).extension().is_some_and(|ext| {
        ["ttf", "otf", "ttc"]
            .iter()
            .any(|f| ext.eq_ignore_ascii_case(f))
    })
}

/// Read a root `.scad` file to source text — the entry read behind [`evaluate_file`](crate::evaluate_file)
/// and kin. Split out here so the crate's every `std::fs` call lives in this one module.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if the path can't be read.
pub(crate) fn read_source(path: &Path) -> crate::Result<String> {
    let bytes =
        std::fs::read(path).map_err(|e| crate::Error::Load(format!("{}: {e}", path.display())))?;
    Ok(decode_source(bytes))
}

/// Decode source bytes: UTF-8, with a LATIN-1 fallback when the bytes aren't valid UTF-8 (AA.5,
/// chotchki's call — nbsp-latin1-test.scad). Upstream's lexer is byte-lenient (a raw `0xA0` NBSP is
/// whitespace to it); mapping each non-UTF-8 byte to its Latin-1 codepoint reproduces that for the
/// only 8-bit encoding .scad files exist in, while fab-lang's core stays `str`/UTF-8 — the fallback
/// lives ONLY at this fs seam. Lossy U+FFFD would still lex-error (looking handled while staying
/// red); Latin-1 actually closes the file.
pub(crate) fn decode_source(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| char::from(b)).collect(),
    }
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

/// SU.2 (sustainment): drive the STATIC needs fixpoint for the intrinsic parity matrix — the same
/// loader loop as [`drive`], but resolution stops after the hoist (nothing executes; `File` needs can't
/// surface) and returns the audit instead of geometry. STRICT where [`drive`] is tolerant: a missing or
/// unparseable library here means the audit would silently report everything `Missing`/`Changed` against
/// the WRONG tree (a mistyped root, a half checkout), so any loader warning is promoted to a hard
/// [`Error::Load`](crate::Error::Load).
pub(crate) fn drive_intrinsic_matrix(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> crate::Result<Vec<super::IntrinsicMatrixRow>> {
    let mut scad = SourceMap::new();
    let mut files = FileTable::new();
    let mut warnings: Vec<Message> = Vec::new();
    let mut reader = no_import_reader;
    loop {
        match super::resolve_intrinsic_matrix(source, base_dir, &scad)? {
            super::MatrixResolution::Complete(rows) => {
                let broken = warnings.iter().find_map(|m| match m {
                    Message::Warning(w) => Some(w.as_str()),
                    Message::Echo(_) => None,
                });
                if let Some(w) = broken {
                    return Err(crate::Error::Load(format!(
                        "intrinsic matrix needs a complete library tree: {w}"
                    )));
                }
                return Ok(rows);
            }
            super::MatrixResolution::Incomplete { needs } => {
                for need in needs {
                    fulfill(
                        need,
                        library_paths,
                        &mut reader,
                        &mut scad,
                        &mut files,
                        &mut warnings,
                    )?;
                }
            }
        }
    }
}

/// Drive the needs fixpoint from an IN-MEMORY source map — the fs-FREE twin of [`drive`] for the wasm
/// host (the browser has no filesystem; the geom worker renders from bytes, W.3.6 Stage 2). A
/// `use`/`include` resolves against `sources` — a virtual lib tree keyed by NORMALIZED relative path
/// ("BOSL2/std.scad", "BOSL2/vectors.scad", …) — instead of `library_paths` + disk. Missing/broken
/// libraries are TOLERATED exactly as native (warn + empty program), so a stray include still renders.
/// `import`/`surface` meshes still go through `mesh_reader`.
///
/// # Errors
/// [`Error::Parse`](crate::Error::Parse) for a malformed ROOT source, a `mesh_reader` failure, or any
/// evaluation error. (A malformed USED/INCLUDED lib is tolerated, like native.)
pub(crate) fn drive_from_map<R>(
    source: &str,
    sources: &std::collections::BTreeMap<PathBuf, String>,
    jit_factory: Option<&dyn NumericJitFactory>,
    config: super::Config,
    mut mesh_reader: R,
) -> crate::Result<(Geo, Vec<Message>)>
where
    R: FnMut(&str) -> crate::Result<Imported>,
{
    let base_dir = Path::new(""); // virtual root — the `main` string has no path of its own
    let mut scad = SourceMap::new();
    let mut files = FileTable::new();
    let mut warnings: Vec<Message> = Vec::new();
    loop {
        match resolve_source(source, base_dir, None, &scad, &files, jit_factory, config)? {
            Resolution::Complete { geo, messages } => {
                warnings.extend(messages);
                return Ok((geo, warnings));
            }
            Resolution::Incomplete { needs } => {
                for need in needs {
                    fulfill_from_map(
                        need,
                        sources,
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

/// [`fulfill`]'s fs-free twin: a `Scad` reference resolves against the virtual `sources` map
/// (`from_dir` first, then the lib root — both lexically normalized), a `File` reference goes to
/// `mesh_reader`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the twin of the fallible `fulfill` (whose fs read stays LOUD); kept `Result` for the parallel \
              signature the needs-fixpoint drives both through"
)]
fn fulfill_from_map<R>(
    need: SourceNeed,
    sources: &std::collections::BTreeMap<PathBuf, String>,
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
            // Fonts contribute the empty program silently — same doctrine as the fs loader (AC.1).
            if is_font_path(&raw) {
                let key = (from_dir.clone(), raw.clone());
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
            }
            let key = (from_dir.clone(), raw.clone());
            if scad.contains_key(&key) {
                return Ok(());
            }
            // Mirror `resolve`'s order without a filesystem: relative to the requesting file's dir
            // first, then the lib root — the first normalized path present in the map wins.
            let id = normalize_lexical(&from_dir.join(&raw))
                .filter(|p| sources.contains_key(p))
                .or_else(|| normalize_lexical(Path::new(&raw)).filter(|p| sources.contains_key(p)));
            let Some(id) = id else {
                // TOLERANT (M.6.1), same as native: a missing library → warn + an EMPTY program so the
                // graph closes and the run renders on.
                warnings.push(Message::Warning(format!("Can't open library '{raw}'.")));
                scad.insert(
                    key,
                    ProvidedSource {
                        id: from_dir.join(&raw),
                        dir: from_dir,
                        program: empty_program(),
                    },
                );
                return Ok(());
            };
            let text = &sources[&id];
            let dir = id.parent().unwrap_or(Path::new("")).to_path_buf();
            let program = parse(text).unwrap_or_else(|_| {
                warnings.push(Message::Warning(format!("Failed to parse '{raw}'.")));
                empty_program()
            });
            scad.insert(key, ProvidedSource { id, dir, program });
        }
        SourceNeed::File { raw } => {
            // TOLERANT (L.5.7), like `fulfill` + the `Scad` arm: a missing/broken import → warn + EMPTY mesh.
            match mesh_reader(&raw) {
                Ok(imported) => {
                    files.insert(raw, imported);
                }
                Err(why) => {
                    warnings.push(Message::Warning(format!(
                        "Can't open import file '{raw}': {why}"
                    )));
                    let empty = Imported::empty_for(&raw);
                    files.insert(raw, empty);
                }
            }
        }
    }
    Ok(())
}

/// Lexical (no-fs) path normalization: drop `.` components and resolve `..` against the accumulated
/// path — the map key a `use`/`include` reference maps to. `None` if `..` escapes the root (an
/// unresolvable reference). Absolute components can't appear in a lib-relative reference.
fn normalize_lexical(p: &Path) -> Option<PathBuf> {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop()?;
            }
            std::path::Component::Normal(s) => out.push(s.to_os_string()),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(out.iter().collect())
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
            // `use <font.ttf>` (AC.1): OpenSCAD silently REGISTERS the font for `text()` — it is
            // not scad source. We contribute the empty program silently (no warning — upstream has
            // none); the registration half is a documented no-op: `text()` draws the BUNDLED
            // Liberation face regardless (the determinism doctrine bans host font lookup), so a
            // `font=` naming the used face still renders — with our deterministic glyphs.
            if is_font_path(&raw) {
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
            // TOCTOU: `resolve` already canonicalized this as a readable file, so a failure here is a
            // race (deleted / perms flipped). Off the pure core's coverage — this module is the seam.
            // `read_source` also gives included libs the AA.5 Latin-1 fallback, same as the root.
            let text = read_source(&id)?;
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
            // TOLERANT (L.5.7), matching the `Scad` arm above + OpenSCAD: a missing/broken import file →
            // warn + an EMPTY mesh (dimension by extension), so the rest of the model still renders
            // ("Can't open import file '…'", warn-and-render-without-it) instead of a hard load error.
            match mesh_reader(&raw) {
                Ok(imported) => {
                    files.insert(raw, imported);
                }
                Err(why) => {
                    warnings.push(Message::Warning(format!(
                        "Can't open import file '{raw}': {why}"
                    )));
                    let empty = Imported::empty_for(&raw);
                    files.insert(raw, empty);
                }
            }
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
