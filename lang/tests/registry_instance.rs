//! AR.26.1 — THE GATE: two registries, one process, different answers.
//!
//! This file is the whole proof that the registry inversion is real rather than cosmetic. Every
//! case here is UNWRITEABLE against the design it replaced: dispatch used to consult three
//! process-lifetime `OnceLock`s keyed on nothing, so a process had exactly one answer to give about
//! what `_reg_probe` is. Handing in a library and getting a different verdict for the same program
//! is the capability, and a capability nothing exercises is a capability nobody can trust.
//!
//! THE INSTRUMENT, stated once because it looks like a bug: the probe natives DELIBERATELY DISAGREE
//! with the references they claim to implement. That disagreement is the only way a test can observe
//! WHICH TIER RAN — a native that agreed would make the two legs indistinguishable and every
//! assertion here would pass vacuously, which is the failure shape this phase keeps finding. It is
//! quarantined to this file and to `registry::tests`, and it never ships.
//!
//! The readout is a MESH DIMENSION rather than an echo, because the raw-AST entry returns geometry:
//! the probe function feeds a `cube`'s size and the probe module draws a differently-sized cube, so
//! "which tier ran" comes off `max(vert.x)` with no float tolerance and no transform or boolean —
//! `mesh_of` needs no kernel for a lone primitive.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::unnecessary_wraps,
    reason = "integration test: unwrap/expect ARE the assertions; the readout is an exact cube edge \
              a primitive wrote verbatim, so == is the right comparison; the probe natives wrap in \
              Ok to fit the fallible native ABI"
)]

use fab_lang::registry::{Entry, ModuleEntry, Registry, Rows};
use fab_lang::surface::{Children, ModuleCall};
use fab_lang::{Config, Scope, Value, parse};

// ── the probe library ────────────────────────────────────────────────────────────────────────────

/// Says `x + 1000` where its reference says `x + 1`.
fn probe_fn(_fx: &dyn fab_lang::surface::FnCtx, args: &[Value]) -> fab_lang::Result<Value> {
    match args.first() {
        Some(Value::Num(x)) => Ok(Value::Num(x + 1000.0)),
        _ => Ok(Value::Undef),
    }
}

static PROBE_FNS: &[Entry] = &[Entry {
    name: "_reg_probe",
    reference: "function _reg_probe(x) = x + 1;",
    consts: &[],
    consts_v: &[],
    deps: &[],
    builtins: &[],
    func: probe_fn,
}];

/// Draws `cube(2)` where its reference draws `cube(1)`.
fn probe_mod(fx: &dyn fab_lang::surface::ModuleCtx) -> fab_lang::Result<fab_lang::Geo> {
    fx.call(&ModuleCall {
        name: "cube",
        args: &[(Some("size"), Value::Num(2.0))],
        children: Children::None,
    })
}

static PROBE_MODS: &[ModuleEntry] = &[ModuleEntry {
    name: "_reg_probe_mod",
    reference: "module _reg_probe_mod() { cube(1); }",
    func: probe_mod,
    consts: &[],
}];

fn probe_registry() -> Registry {
    Registry::new().with(Rows {
        name: "probe",
        functions: PROBE_FNS,
        modules: PROBE_MODS,
        pins: &[],
    })
}

// ── readouts ─────────────────────────────────────────────────────────────────────────────────────

/// The largest x coordinate in `src`'s mesh — the cube edge the program actually built.
fn cube_edge(src: &str, registry: &Registry, config: Config) -> f64 {
    let program = parse(src).expect("probe source parses");
    let mesh = fab_lang::eval_program_with_registry(&program, &Scope::new(), registry, config)
        .expect("probe program evaluates");
    mesh.verts
        .iter()
        .map(|v| v.x)
        .fold(f64::NEG_INFINITY, f64::max)
}

/// `cube_edge` at the default config.
fn edge(src: &str, registry: &Registry) -> f64 {
    cube_edge(src, registry, Config::default())
}

/// A program whose cube size comes from the probe FUNCTION: interpreted `_reg_probe(0)` is 1, the
/// compiled row says 1000.
const FN_SRC: &str = "function _reg_probe(x) = x + 1;\ncube(_reg_probe(0));";
/// A program that instantiates the probe MODULE: interpreted it draws `cube(1)`, compiled `cube(2)`.
const MOD_SRC: &str = "module _reg_probe_mod() { cube(1); }\n_reg_probe_mod();";

// ── the cases ────────────────────────────────────────────────────────────────────────────────────

/// THE ONE THAT COULD NOT BE WRITTEN BEFORE. The same program, twice, in ONE process: against a
/// handed-in library it dispatches to the compiled row; against the shipped registry, which has
/// never heard of `_reg_probe`, it interprets. A process-lifetime table has one answer and would
/// have to give it both times.
#[test]
fn a_handed_in_row_wires_where_the_builtin_registry_does_not() {
    let probe = probe_registry();
    assert_eq!(
        edge(FN_SRC, &probe),
        1000.0,
        "the handed-in row must dispatch"
    );
    assert_eq!(
        edge(FN_SRC, Registry::builtin()),
        1.0,
        "the shipped registry knows nothing of this name — interpret"
    );
}

/// The fingerprint gate governs a FOREIGN row exactly as it governs a shipped one. The program's
/// body drifted from what the row was generated against, so the row must not wire — a handed-in
/// library buys a consumer no way around wire-only-if-proven.
#[test]
fn a_drifted_body_never_wires_a_handed_in_row() {
    const DRIFTED: &str = "function _reg_probe(x) = x + 2;\ncube(_reg_probe(0));";
    assert_eq!(
        edge(DRIFTED, &probe_registry()),
        2.0,
        "a body that does not fingerprint to the row's reference interprets"
    );
}

/// The MODULE tier reads the instance too — it resolves per CALL SITE rather than once at ctx
/// build, so it is a genuinely separate path through the same registry.
#[test]
fn a_handed_in_module_row_wires() {
    assert_eq!(
        edge(MOD_SRC, &probe_registry()),
        2.0,
        "the compiled module draws the bigger cube"
    );
    assert_eq!(
        edge(MOD_SRC, Registry::builtin()),
        1.0,
        "the shipped registry interprets the reference"
    );
}

/// A drifted MODULE definition does not wire either — the same contract on the tier that resolves
/// per call, where a per-call lookup is the easiest place to lose it.
#[test]
fn a_drifted_module_body_never_wires() {
    const DRIFTED: &str = "module _reg_probe_mod() { cube(3); }\n_reg_probe_mod();";
    assert_eq!(edge(DRIFTED, &probe_registry()), 3.0);
}

/// `Config::intrinsics` is the per-eval OFF switch and stays ORTHOGONAL to which libraries are
/// present: the same registry, natives disabled, must interpret. AR.2's differential depends on
/// exactly this — an oracle that had to drop the library to drop the natives would be comparing two
/// different programs rather than two tiers.
#[test]
fn the_run_gate_still_turns_a_handed_in_library_off() {
    let off = Config {
        intrinsics: false,
        ..Config::default()
    };
    let probe = probe_registry();
    assert_eq!(cube_edge(FN_SRC, &probe, off), 1.0, "function tier off");
    assert_eq!(cube_edge(MOD_SRC, &probe, off), 1.0, "module tier off");
}

/// An EMPTY registry is a legal instance and dispatches nothing. Worth pinning because it is the
/// silent-failure shape the `Default` on `&Registry` deliberately avoids: if an empty registry ever
/// became the default, every tier differential in the tree would start comparing the interpreter
/// against itself and still pass.
#[test]
fn an_empty_registry_dispatches_nothing() {
    let empty = Registry::new();
    assert_eq!(empty.function_count(), 0);
    assert_eq!(edge(FN_SRC, &empty), 1.0);
}

/// Two registries alive at once do not leak into each other — the property the shared `OnceLock`
/// could not have, asserted directly rather than inferred from the cases above.
#[test]
fn two_registries_coexist_without_cross_talk() {
    let probe = probe_registry();
    let empty = Registry::new();
    assert_eq!(edge(FN_SRC, &probe), 1000.0);
    assert_eq!(edge(FN_SRC, &empty), 1.0);
    assert_eq!(
        edge(FN_SRC, &probe),
        1000.0,
        "asking the empty one must not have disarmed the probe one"
    );
}

/// [`fab_lang::FnOracle::with_registry`] carries an instance too. The oracle interprets the
/// function it is ASKED for by design (it is the JIT differential's baseline), so the observable is
/// a NESTED call — `outer` interprets, and what `_reg_probe` resolves to inside it is the registry's
/// answer.
#[test]
fn the_fn_oracle_carries_an_instance() {
    const SRC: &str = "function _reg_probe(x) = x + 1;\nfunction outer(x) = _reg_probe(x);";
    let program = parse(SRC).expect("parses");
    let functions: Vec<(&str, &[fab_lang::Parameter], &fab_lang::Expr)> = program
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            fab_lang::StmtKind::FunctionDef { name, params, body } => {
                Some((name.as_str(), params.as_slice(), body))
            }
            _ => None,
        })
        .collect();
    let probe = probe_registry();
    let wired = fab_lang::FnOracle::with_registry(&functions, &[], &probe)
        .expect("oracle builds")
        .call("outer", &[Value::Num(1.0)])
        .expect("call succeeds");
    let plain = fab_lang::FnOracle::with_registry(&functions, &[], Registry::builtin())
        .expect("oracle builds")
        .call("outer", &[Value::Num(1.0)])
        .expect("call succeeds");
    assert_eq!(wired, Value::Num(1001.0));
    assert_eq!(plain, Value::Num(2.0));
}
