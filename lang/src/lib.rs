//! scad-rs — the OpenSCAD language, in Rust, over the Manifold kernel.
//!
//! Stock `.scad` in, the same mesh OpenSCAD produces out. Not a dialect, not a new language: a
//! reimplementation whose correctness is DEFINED against stock OpenSCAD running stock BOSL2 (the
//! differential oracle). `SPEC.md` carries the why; the short version:
//!
//! - **One module everywhere.** A `wasm32-unknown-unknown` front end kills the emscripten
//!   two-module split the official wasm forces on us.
//! - **The Safari recursion cliff dies by construction.** Evaluation runs on an EXPLICIT stack,
//!   so recursion is bounded by memory — not by whatever JS engine's frame budget we land under.
//! - **Caching, the customizer, and per-node progress are ours**, because evaluation happens in
//!   our process against an AST we own.
//!
//! # Pipeline
//!
//! ```text
//! source ──parse──▶ AST ──eval──▶ CSG node tree ──lower──▶ kernel::Solid (Manifold)
//! ```
//!
//! [`evaluate`] is the tracer-bullet spine tying those stages together. Each fills in as Phase G
//! lands (parse: G.3.2/3, eval: G.3.4, lower: G.3.5); until a stage exists the spine returns
//! [`Error::Unimplemented`] — LOUD, never silently wrong (SPEC deferral doctrine).
//!
//! # Observability
//!
//! Parse decisions and evaluation are each observable from their idiomatic tool, both free in
//! release. The parser leans on winnow's OWN tooling first — every named production wraps in
//! winnow's `trace()` (gated behind the `trace` feature → `winnow/debug`); we only reach for the
//! [`tracing`] crate on the parse side if winnow's isn't enough. The evaluator is the tracing
//! crate's home turf: TRACE-level spans on the eval path (`eval_program`, per-`builtin`, per-`module`)
//! that double as the benchmark corpus — a `Layer` aggregates their busy-time by name (see
//! `tests/tracing_bench.rs`). The explicit-stack machine means a user function's subtree isn't
//! scope-bounded, so a `call` event marks the path while the enclosing `eval_program` span times it.
//! In release those spans compile out when the linking binary sets `tracing`'s `release_max_level_off`
//! — compile-out-like-a-logger, the doctrine both sides share.
//!
//! License: GPL-2.0-or-later — OpenSCAD's exact license, on purpose (frictionless upstreaming; v3
//! rules apply in distributed builds via Apache-2.0 Manifold). See `README.md`.

mod customizer;
mod error;
mod eval;
mod geom;
mod lexer;
mod mesh;
mod parser;
mod webcolors;

pub use customizer::{Constraint, CustomParam, Customizer, DropdownItem, customize};
pub use error::{Error, Result};
pub use eval::jit_abi::{jit_math, jit_math_id};
pub use eval::rng::RandStream;
pub use eval::{
    Config, Contour, Evaluation, ExtrudeKind, FileTable, FnOracle, Geo, GeoNode, Imported,
    JitConst, JitDef, JitOutcome, Join2D, Message, NumericJit, NumericJitFactory, RANGE_MAX,
    RangeIter, Resolution, Scope, Shape2D, SourceNeed, Value, bench_intrinsic, eval_expr,
    eval_program, fragments, interpret_fn, range_iter, range_len,
};
pub use geom::{Affine, Affine2, Dims, Rgba, Tri, Vec2, Vec3};
pub use lexer::{Lexed, Token, TokenKind, decode_str, lex, num_value};
pub use mesh::Mesh;
pub use parser::{
    Arg, BinOp, Expr, ExprKind, Modifiers, ModuleInstantiation, Parameter, Program, Span, Stmt,
    StmtKind, UnOp, parse, print, print_expr,
};

/// Tier-equality for doctrine #36 (`interp` == `intrinsics` == `JIT`, and cross-platform): two `f64`
/// results from different execution tiers AGREE iff they carry the same information — identical bits for
/// every finite value, both infinities, and signed zero, but NaN compares as a CLASS (any NaN ≡ any NaN),
/// its sign/payload UNSPECIFIED.
///
/// Why NaN is a class, not a bit pattern (Q.6): a NaN payload is UNOBSERVABLE — every NaN prints `nan`
/// (both signs, no `-nan`; matching OpenSCAD), no builtin exposes the bits, and comparisons are
/// payload-blind — so no program output can depend on it. It is also nondeterministic to produce: x86 and
/// ARM propagate NaN sign/payload by different hardware rules, AND Cranelift's optimizer legally rewrites
/// `(-x)*(-x)` → `x*x` (real-exact, but drops the sign the interpreter's `-x` sets). A stable NaN bit
/// pattern is therefore neither reachable across platforms nor meaningful; class equality is the strongest
/// form of bit-identity IEEE-754 permits for NaN. This is THE comparison every differential check — the
/// JIT `fast_eq_jit` proptest, the `corpus_diff` harness, the `jit_diff` fuzzer, the generator's label —
/// must route scalar leaves through, so none drifts back to a bit compare that flakes on `(-NaN)²`.
#[must_use]
pub fn tier_eq(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

use std::path::{Path, PathBuf};

/// Evaluate OpenSCAD source to a triangle [`Mesh`] — the end-to-end tracer-bullet spine.
///
/// Convenience over [`evaluate_with_base`]: no library paths, and `use`/`include` resolve relative to
/// the process CWD (the `.` base). For reproducible resolution — the determinism doctrine's concern —
/// use [`evaluate_file`] or [`evaluate_with_base`] with an explicit base + library paths.
///
/// # Errors
///
/// [`Error::Parse`] for malformed source, [`Error::Load`] for an unresolvable `use`/`include`, and
/// [`Error::Unimplemented`] for a well-formed program that uses a construct beyond the current subset.
pub fn evaluate(source: &str) -> Result<Mesh> {
    evaluate_full(source).map(|e| e.mesh)
}

/// Like [`evaluate`], but returns the full [`Evaluation`] — the mesh PLUS the ordered `echo`/warning
/// console messages (I.5). The `evaluate*` functions are mesh-only sugar over their `*_full` siblings.
///
/// # Errors
/// As [`evaluate`].
pub fn evaluate_full(source: &str) -> Result<Evaluation> {
    evaluate_with_base_full(source, Path::new("."), &[])
}

/// Evaluate a `.scad` FILE, resolving its `use`/`include` graph. Relative references resolve against
/// the file's OWN directory first, then `library_paths` in order (OpenSCAD's search order after the
/// including dir). The crate stays PURE — it never reads `OPENSCADPATH`; the caller (app/harness) reads
/// the environment + knows the BOSL2 dir and hands the resolved paths down. That keeps "same input →
/// bit-identical output" honest.
///
/// # Errors
///
/// [`Error::Load`] if the file or any `use`/`include` target can't be read/resolved, [`Error::Parse`]
/// for malformed source, and [`Error::Unimplemented`] for constructs beyond the current subset.
pub fn evaluate_file(path: &Path, library_paths: &[PathBuf]) -> Result<Mesh> {
    evaluate_file_full(path, library_paths).map(|e| e.mesh)
}

/// Like [`evaluate_file`], but returns the full [`Evaluation`] (mesh + `echo`/warning messages).
///
/// # Errors
/// As [`evaluate_file`].
pub fn evaluate_file_full(path: &Path, library_paths: &[PathBuf]) -> Result<Evaluation> {
    let source = eval::io::read_source(path)?;
    // The including-file dir. An empty parent (a bare `foo.scad`) resolves relative to CWD via the
    // loader's canonicalize, so no special-casing is needed beyond the parent-less root (`.`).
    let base_dir = path.parent().unwrap_or(Path::new("."));
    let (tree, messages) = eval::evaluate_source(
        &source,
        base_dir,
        Some(path),
        library_paths,
        Config::from_env(),
    )?;
    Ok(Evaluation {
        mesh: eval::mesh_of(tree)?,
        messages,
    })
}

/// Evaluate in-memory `source` as if it lived in `base_dir` — a GUI's unsaved buffer for the file it's
/// editing — resolving `use`/`include` against `base_dir`, then `library_paths`. Pass an ABSOLUTE
/// `base_dir` for reproducible resolution.
///
/// # Errors
///
/// As [`evaluate_file`], minus the root-file read.
pub fn evaluate_with_base(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> Result<Mesh> {
    evaluate_with_base_full(source, base_dir, library_paths).map(|e| e.mesh)
}

/// Like [`evaluate_with_base`], but returns the full [`Evaluation`] (mesh + `echo`/warning messages).
///
/// # Errors
/// As [`evaluate_with_base`].
pub fn evaluate_with_base_full(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> Result<Evaluation> {
    let (tree, messages) =
        eval::evaluate_source(source, base_dir, None, library_paths, Config::from_env())?;
    Ok(Evaluation {
        mesh: eval::mesh_of(tree)?,
        messages,
    })
}

/// Evaluate OpenSCAD `source` to a dimension-tagged geometry TREE ([`Geo`]) — the J.2/J.3 output. A
/// tree with transforms, booleans, or any 2D geometry can't be flattened by fab-lang alone (that needs
/// the Manifold kernel); a downstream backend (fab-scad's `GeometryBackend`) walks it, dispatching on the
/// [`Geo::D2`]/[`Geo::D3`] tag. `use`/`include` resolve against CWD.
///
/// # Errors
/// As [`evaluate`], minus the single-primitive restriction (a transform/boolean/2D tree is fine here).
pub fn evaluate_geometry(source: &str) -> Result<Geo> {
    evaluate_geometry_with_base(source, Path::new("."), &[])
}

/// Like [`evaluate_geometry`], but for a `.scad` FILE, resolving its `use`/`include` graph.
///
/// # Errors
/// As [`evaluate_file`], minus the single-primitive restriction.
pub fn evaluate_geometry_file(path: &Path, library_paths: &[PathBuf]) -> Result<Geo> {
    let source = eval::io::read_source(path)?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    Ok(eval::evaluate_source(
        &source,
        base_dir,
        Some(path),
        library_paths,
        Config::from_env(),
    )?
    .0)
}

/// Like [`evaluate_geometry`], but resolving `use`/`include` against `base_dir` (a GUI's unsaved buffer).
///
/// # Errors
/// As [`evaluate_with_base`], minus the single-primitive restriction.
pub fn evaluate_geometry_with_base(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> Result<Geo> {
    Ok(eval::evaluate_source(source, base_dir, None, library_paths, Config::from_env())?.0)
}

/// Like [`evaluate_geometry`], but returns the geometry tree PLUS the ordered `echo`/warning
/// [`Message`]s — the tree-side analogue of [`evaluate_full`]. Needed when a 2D or mixed program's
/// warnings matter (the 2D/3D "Mixing…" diagnostics), since [`evaluate_full`] can't reach them: it
/// flattens through the no-backend `mesh_of`, which a 2D result LOUD-rejects. `use`/`include` resolve
/// against CWD.
///
/// # Errors
/// As [`evaluate_geometry`].
pub fn evaluate_geometry_full(source: &str) -> Result<(Geo, Vec<Message>)> {
    eval::evaluate_source(source, Path::new("."), None, &[], Config::from_env())
}

/// Like [`evaluate_geometry_with_base`], but ALSO returns the ordered `echo`/warning [`Message`]s — the
/// base-dir analogue of [`evaluate_geometry_full`]. The BOSL2 corpus repro path wants both at once: a
/// `.scadtest` script `include`s `<../std.scad>` (so it needs `base_dir`) AND its `echo` output is the
/// clue when an `assert` diverges (so it needs the messages).
///
/// # Errors
/// As [`evaluate_geometry_with_base`].
pub fn evaluate_geometry_with_base_full(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> Result<(Geo, Vec<Message>)> {
    eval::evaluate_source(source, base_dir, None, library_paths, Config::from_env())
}

/// Evaluate `source` to a geometry [`Geo`] tree, resolving `import`/`surface` meshes through `mesh_reader`
/// (M.4) — the native driver over the whole needs fixpoint. `import`/`surface` paths are RUNTIME
/// expressions, discovered only by executing; each one the run reaches is handed to `mesh_reader` (the
/// literal `file=` path in → a dimension-tagged [`Imported`] out), which fab-scad backs with its STL/3MF/SVG readers
/// (M.5). fab-lang itself does ZERO IO — the `io` shell loops the pure inner step, reading `use`/`include`
/// sources + calling `mesh_reader` for meshes, until the run closes. (An ASYNC wasm host that can't run a
/// sync reader drives the same pure step directly, awaiting between rounds; that public seam lands with its
/// first consumer.)
///
/// # Errors
/// As [`evaluate_geometry`] (parse / `use`/`include` load / eval), plus any error `mesh_reader` returns for
/// a file it can't read.
pub fn resolve_geometry_with_base<R>(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
    jit_factory: Option<&dyn NumericJitFactory>,
    config: Config,
    mesh_reader: R,
) -> Result<Geo>
where
    R: FnMut(&str) -> Result<Imported>,
{
    Ok(eval::io::drive(
        source,
        base_dir,
        None,
        library_paths,
        jit_factory,
        config,
        mesh_reader,
    )?
    .0)
}

/// Like [`resolve_geometry_with_base`], but for a `.scad` FILE — resolving its `use`/`include` graph AND
/// its `import`/`surface` meshes (through `mesh_reader`). The root file is read here.
///
/// # Errors
/// As [`evaluate_geometry_file`], plus any error `mesh_reader` returns.
pub fn resolve_geometry_file<R>(
    path: &Path,
    library_paths: &[PathBuf],
    jit_factory: Option<&dyn NumericJitFactory>,
    config: Config,
    mesh_reader: R,
) -> Result<Geo>
where
    R: FnMut(&str) -> Result<Imported>,
{
    let source = eval::io::read_source(path)?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    Ok(eval::io::drive(
        &source,
        base_dir,
        Some(path),
        library_paths,
        jit_factory,
        config,
        mesh_reader,
    )?
    .0)
}

#[cfg(test)]
mod tests {
    use super::{Error, evaluate, tier_eq};

    #[test]
    fn tier_eq_is_bitwise_for_information_and_class_for_nan() {
        // Everything that carries information compares BITWISE — including the two things naive `==` gets
        // wrong: `-0.0` and `0.0` are DISTINCT (different bits), and `inf`/`-inf` are exact.
        assert!(tier_eq(1.5, 1.5));
        assert!(!tier_eq(0.0, -0.0)); // signed zero is information (stricter than IEEE `==`)
        assert!(tier_eq(f64::INFINITY, f64::INFINITY));
        assert!(!tier_eq(f64::INFINITY, f64::NEG_INFINITY));
        assert!(!tier_eq(1.0, 1.0 + f64::EPSILON));

        // NaN is a CLASS: any NaN ≡ any NaN, whatever the sign/payload — the exact `(-NaN)²` case (Q.6)
        // where the JIT yields `0x7ff8…` and the interpreter `0xfff8…`. Both are NaN → agree.
        let pos = f64::from_bits(0x7ff8_0000_0000_0000);
        let neg = f64::from_bits(0xfff8_0000_0000_0000);
        let payload = f64::from_bits(0x7ff8_0000_dead_beef);
        assert!(tier_eq(pos, neg));
        assert!(tier_eq(neg, payload));
        assert!(tier_eq(f64::NAN, neg));
        // ...but a NaN never equals a real number.
        assert!(!tier_eq(f64::NAN, 0.0));
        assert!(!tier_eq(f64::NAN, f64::INFINITY));
    }

    #[test]
    fn evaluate_produces_a_mesh() {
        // The tracer-bullet spine reaches geometry: source → a real triangle mesh.
        let mesh = evaluate("sphere(5, $fn = 8);").expect("sphere evaluates");
        assert!(mesh.tri_count() > 0 && mesh.vert_count() > 0);
    }

    #[test]
    fn evaluate_defers_transforms_loud() {
        // Beyond the G.3.5 subset (a transform) → LOUD, never silently wrong.
        let err = evaluate("translate([1,0,0]) cube(1);").unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
    }

    #[test]
    fn evaluate_surfaces_parse_errors() {
        let err = evaluate("\"unterminated").unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }
}
