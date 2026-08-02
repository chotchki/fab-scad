//! AR.31 — WHERE A PARAMETER DEFAULT EVALUATES, against the oracle.
//!
//! A default is not evaluated in the caller's scope and not purely in the callee's either: it splits.
//! A PLAIN name resolves lexically, in the callee's own island — so a `use`d library's default sees
//! that library's globals and not the root's. A `$`-name resolves DYNAMICALLY, on the caller's chain,
//! exactly as it would anywhere else. Two rules in one expression, and the interpreter implements
//! only the first: `push_call` schedules an unfilled slot's default as `Task::Eval(default, base)`
//! where `base` is the island global, which has no dynamic parent, so every `$`-read in a default
//! stops there.
//!
//! FOUND BY AR.30's adversarial review, and it was a conformance bug rather than a transpiler one —
//! the compiled tier read the caller and was RIGHT; the interpreter is what disagreed with upstream.
//! FIXED HERE: `Scope::call_frame(base, caller)` is exactly the split rule, and it is built only when
//! a default is actually unfilled, so an ordinary all-args call allocates no extra frame.
//!
//! MODULES DIVERGED IDENTICALLY, which the finding did not say because it arrived through a
//! function: the rule belongs to the language, not to the callable kind.
//!
//! WHY THESE ARE FILE-ROOTED. Half the matrix is about which ISLAND a name resolves in, and a single
//! source string is one island — `use` versus `include` cannot be asked of it at all. That is the
//! same argument AQ.1 made for `warnings_file`, one channel over, and it is why `Driver::echo_file`
//! exists.
//!
//! The oracle leg skips cleanly when the binary is absent, like every differential here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use fab_scad::differ::diff_echo_file;
use fab_scad::openscad::find_bin;

/// The library every `use`/`include` case shares. It binds its OWN `$fn` and `G` at top level, which
/// is the whole point: if a default resolved in the callee's island, these are the values it would
/// see, and they are deliberately different from anything the callers bind.
const LIB: &str = "\
$fn = 7;
G = 70;
function f_dollar(n = $fn) = n;
function f_global(n = G) = n;
function f_both(a = G, b = $fn) = [a, b];
function f_provided(n = $fn) = n;
module m_dollar(n = $fn) { echo(mod = n); }
";

/// Write `root` (plus the shared library) into a fresh directory and read BOTH engines' echo.
///
/// Returns the consoles rather than a verdict, because a bare `Err` cannot tell a real divergence
/// from a plumbing failure — a library that never resolved makes both legs empty on one engine and
/// the case "diverges" for a reason that has nothing to do with scoping. The caller checks the
/// consoles themselves.
///
/// A trailing `cube(1);` on every case, because the oracle needs geometry to export before it will
/// report a console at all — the same trick the string-rooted echo differ uses.
fn consoles(name: &str, root_src: &str) -> (Vec<String>, Vec<String>, Result<(), String>) {
    let dir = std::env::temp_dir().join(format!("fab-ar31-{name}"));
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).expect("scratch dir");
    std::fs::write(lib.join("mylib.scad"), LIB).expect("write lib");
    let root = dir.join("root.scad");
    std::fs::write(&root, format!("{root_src}\ncube(1);\n")).expect("write root");
    let libs = vec![lib];
    let drivers = fab_scad::differ::drivers();
    let fab = drivers[0].echo_file(&root, &libs);
    let oracle = drivers[1].echo_file(&root, &libs);
    let verdict = diff_echo_file(&root, &libs);
    let _ = std::fs::remove_dir_all(&dir);
    (fab, oracle, verdict)
}

/// Every case, with any KNOWN-DIVERGENT one marked. A `false` is a claim about upstream that this
/// test checks; a `true` is a recorded bug, guarded in BOTH directions — a regression fails, and so
/// does a FIX, which is the signal to clear the flag.
///
/// The list is empty now. It was not when this file was written: eight cases were marked, AR.31's
/// fix made all eight agree, and the test failed on the second assertion — which is the mechanism
/// working, not a nuisance. Leaving the field in place is deliberate: the next conformance gap found
/// here gets recorded rather than argued about or quietly skipped.
struct Case {
    name: &'static str,
    src: &'static str,
    /// Known-divergent, and WHY. Empty today.
    known_bad: bool,
}

const CASES: &[Case] = &[
    // ── The LEXICAL half, which the interpreter already gets right ──────────────────────────────
    Case {
        name: "plain_global_resolves_in_the_callees_island",
        src: "use <mylib.scad>\nG = 24;\necho(f_global());",
        // The root's `G = 24` must NOT win: a `use`d function's default sees its own library's 70.
        known_bad: false,
    },
    Case {
        name: "plain_global_under_include_is_the_root_global",
        // `include` splices, so there IS only one island and 24 is the same global.
        src: "include <mylib.scad>\nG = 24;\necho(f_global());",
        known_bad: false,
    },
    Case {
        name: "a_provided_argument_beats_the_default_entirely",
        src: "use <mylib.scad>\nmodule w() { echo(f_provided(5)); }\nw($fn = 24);",
        known_bad: false,
    },
    // ── The DYNAMIC half, which it does not ─────────────────────────────────────────────────────
    Case {
        name: "dollar_in_a_used_functions_default_reads_the_caller",
        // 24 — the CALLER's chain, not the library's own `$fn = 7`.
        src: "use <mylib.scad>\nmodule w() { echo(f_dollar()); }\nw($fn = 24);",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_used_functions_default_reads_a_let",
        src: "use <mylib.scad>\necho(let($fn = 24) f_dollar());",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_used_functions_default_reads_the_root",
        src: "use <mylib.scad>\n$fn = 24;\necho(f_dollar());",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_local_functions_default_reads_the_caller",
        // No library at all: the root's own island seeds `$fn = 0`, and 24 must still win.
        src: "function g(n = $fn) = n;\nmodule w() { echo(g()); }\nw($fn = 24);",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_local_functions_default_reads_a_let",
        src: "function g(n = $fn) = n;\necho(let($fn = 24) g());",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_module_default_reads_the_caller",
        // MODULES TOO, which is easy to miss when the finding arrives through a function.
        src: "use <mylib.scad>\nmodule w() { m_dollar(); }\nw($fn = 24);",
        known_bad: false,
    },
    Case {
        name: "dollar_in_a_default_under_include_agrees_by_coincidence",
        // `include` puts the callee in the ROOT island, so the lexical read happens to find the
        // caller's binding. It agrees, and it agrees for the wrong reason — which is precisely why
        // it is here: an `include`-only matrix would have reported this bug as absent.
        src: "include <mylib.scad>\n$fn = 24;\necho(f_dollar());",
        known_bad: false,
    },
    Case {
        name: "one_default_reading_both_kinds_splits",
        // The two rules in ONE parameter list: `a = G` is lexical (70), `b = $fn` is dynamic (24).
        // A fix that made defaults evaluate in the CALLER's scope outright would break `a`, so this
        // is the case that pins the fix's shape rather than just its direction.
        src: "use <mylib.scad>\nG = 1;\nmodule w() { echo(f_both()); }\nw($fn = 24);",
        known_bad: false,
    },
    Case {
        name: "an_unbound_dollar_in_a_default_is_undef_on_both",
        src: "use <mylib.scad>\nfunction h(n = $nosuch) = n;\necho(h());",
        known_bad: false,
    },
    Case {
        name: "a_nested_default_reads_the_outermost_caller",
        // `outer`'s default calls `f_dollar`, whose own default reads `$fn`. Two hops from the set.
        src: "use <mylib.scad>\nfunction outer(v = f_dollar()) = v;\n\
              module w() { echo(outer()); }\nw($fn = 24);",
        known_bad: false,
    },
];

/// THE MATRIX. Every case runs; the ones marked `known_bad` are asserted to STILL diverge, so this
/// file is a ratchet in both directions — a regression on a good case fails, and a FIX on a bad one
/// fails too, which is the signal that the list should shrink.
#[test]
fn parameter_default_scope_matches_the_oracle() {
    if find_bin().is_none() {
        eprintln!("skipping: no OpenSCAD binary — the oracle leg is optional, not required");
        return;
    }
    let mut regressions = Vec::new();
    let mut fixed = Vec::new();
    let mut hollow = Vec::new();
    for case in CASES {
        let (fab, oracle, verdict) = consoles(case.name, case.src);
        // EVERY case must get a real answer out of BOTH engines. Without this an unresolved
        // library, a crashed oracle or a failed export reads as a divergence, and a `known_bad`
        // case would go on "passing" for a reason unrelated to scoping.
        if fab.is_empty() || oracle.is_empty() {
            hollow.push(format!(
                "{}: fab {fab:?}, oracle {oracle:?} — one engine said nothing",
                case.name
            ));
            continue;
        }
        match (verdict, case.known_bad) {
            (Ok(()), false) | (Err(_), true) => {}
            (Err(why), false) => regressions.push(format!("{}: {why}", case.name)),
            (Ok(()), true) => fixed.push(case.name),
        }
    }
    assert!(
        hollow.is_empty(),
        "{} case(s) produced no console on one engine — the comparison is hollow, not passing:\n\n{}",
        hollow.len(),
        hollow.join("\n")
    );
    assert!(
        regressions.is_empty(),
        "{} default-scope case(s) diverged from the oracle:\n\n{}",
        regressions.len(),
        regressions.join("\n\n")
    );
    assert!(
        fixed.is_empty(),
        "{} case(s) marked `known_bad` now AGREE with the oracle — AR.31 is (partly) fixed, so \
         clear their `known_bad` flag and let them guard the fix: {fixed:?}",
        fixed.len()
    );
}

/// NON-VACUITY. `agree_echo` writes files, spawns the oracle and compares — a silent failure
/// anywhere in that chain (a library that never resolved, a console that came back empty on BOTH
/// legs) would make every case above "agree" while testing nothing. So: one case whose expected
/// values are pinned outright, not merely compared.
#[test]
fn the_matrix_actually_reaches_both_engines() {
    if find_bin().is_none() {
        return;
    }
    let dir = std::env::temp_dir().join("fab-ar31-vacuity");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).expect("scratch dir");
    std::fs::write(lib.join("mylib.scad"), LIB).expect("write lib");
    let root = dir.join("root.scad");
    std::fs::write(
        &root,
        "use <mylib.scad>\nG = 24;\necho(f_global());\ncube(1);\n",
    )
    .expect("write root");

    let libs = vec![lib];
    let drivers = fab_scad::differ::drivers();
    assert_eq!(drivers.len(), 2, "the oracle leg must be present here");
    for d in &drivers {
        let echo = d.echo_file(&root, &libs);
        assert_eq!(
            echo,
            vec!["ECHO: 70".to_string()],
            "{} did not resolve the library's own G — the case would have compared two empty \
             consoles and passed",
            d.name()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
