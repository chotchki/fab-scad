//! `set -x` for scad — a debug-gated evaluation trace.
//!
//! When `FAB_TRACE` is present in the environment, the evaluator echoes each named binding + assert to
//! stderr as it runs — the flow of values BOSL2 lives on — so a divergence traces straight to the value
//! that went wrong (`+ minx = 24` next to `assert(minx <= size.x)` tells you the whole story). OFF by
//! default: a single cached env read, then a `bool` check per binding, so it's free in the normal path
//! and compiled to nearly nothing. It's a pure DEBUG affordance — stderr only, never touches the mesh or
//! the `echo` message buffer, so it can't perturb the deterministic output (that's [`super::message`]).
//!
//! Granularity is the value-producing constructs: assignments, `let` bindings, call params, and assert
//! outcomes. Module instantiation produces geometry, not a value, so it rides the `tracing` call-path
//! spans ([`super`] `trace!` events) instead — this trace is for the arithmetic/logic layer.

use std::sync::LazyLock;

use super::fmt::format_value;
use super::value::Value;

/// Cached once: is `FAB_TRACE` set? Reading the env on every binding would be absurd; this reads it the
/// first time the trace is consulted and holds the answer for the process.
static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("FAB_TRACE").is_some());

/// Test-only force flag: the `FAB_TRACE` env gate is process-cached, so a test can't flip it — this lets
/// the trace-emitting paths be exercised (and covered) directly. Compiled out of non-test builds.
#[cfg(test)]
static FORCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the `set -x` trace is on.
pub(super) fn on() -> bool {
    #[cfg(test)]
    if FORCE.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    *ENABLED
}

/// Test-only: force the trace on/off so the emit paths (and the evaluator's trace hooks) can be
/// exercised despite the process-cached env gate. Reset to `false` after, to not leak into other tests.
#[cfg(test)]
pub(super) fn set_enabled(v: bool) {
    FORCE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Trace a binding `name = value` — an assignment, a `let`, or a bound call parameter. `kind` is a
/// one-char sigil so the stream reads at a glance: `=` assignment, `l` let, `p` param.
pub(super) fn bind(kind: char, name: &str, value: &Value) {
    if on() {
        eprintln!("+ [{kind}] {name} = {}", format_value(value));
    }
}

/// Trace a USER function's RETURN value — `name => value`. Pushed as a peek-only continuation so it fires
/// right as the value lands on the stack, before the caller consumes it. Its args show up just above as
/// `[p]` param binds; this shows what came back, so a wrong return is obvious in context.
pub(super) fn ret(name: &str, value: &Value) {
    if on() {
        eprintln!("+ [call] {name} => {}", format_value(value));
    }
}

/// Trace a BUILTIN call in FULL — `name(a, b, c) => result`. Builtins have no param binds to show their
/// inputs (they're applied natively, not bound into a scope), so the args are printed inline — which is
/// exactly what pins down e.g. `min(undef) => undef` vs `min(5) => 5` in a divergence. Args print in
/// SOURCE order: a builtin reads every argument positionally, so a named arg shows as its bare value at
/// its position (`search(["bar"], […], 1, 1)`), matching how the builtin actually consumed it.
pub(super) fn builtin(name: &str, args: &[Value], result: &Value) {
    if on() {
        let parts: Vec<String> = args.iter().map(format_value).collect();
        eprintln!(
            "+ [call] {name}({}) => {}",
            parts.join(", "),
            format_value(result)
        );
    }
}

/// Trace an UNBOUND `$`-special reference. OpenSCAD stays SILENT on these (dynamically scoped, often
/// optionally-set), so they never reach the user-facing warning log — but for US a `$`-var that resolves
/// to nothing may be one we haven't implemented, so it goes here, to the dev trace, to be caught.
pub(super) fn unbound_special(name: &str) {
    if on() {
        eprintln!("+ [unbound $] {name}");
    }
}

/// Trace a MODULE instantiation — geometry, not a value, so there's nothing to show a result for; the
/// name marks entry so the value trace beneath it has a frame. `depth` indents by call nesting.
pub(super) fn module(depth: usize, name: &str) {
    if on() {
        eprintln!("+ [mod]{} {name}()", "  ".repeat(depth));
    }
}

/// Trace an assert outcome next to the pretty-printed condition (`ok`/`FAIL`), so the trace shows the
/// guard that passed right before the one that blew.
pub(super) fn assert(passed: bool, condition: &str) {
    if on() {
        let outcome = if passed { "ok" } else { "FAIL" };
        eprintln!("+ [assert {outcome}] {condition}");
    }
}
