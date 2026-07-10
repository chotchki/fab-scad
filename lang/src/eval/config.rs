//! One place for the execution knobs — the interp/JIT tier + the two memo caches — so an embedder (the GUI,
//! the wasm build) selects them PROGRAMMATICALLY instead of setting process-global env vars, and the harnesses
//! keep their env gates via [`Config::from_env`]. Replaces the ~dozen scattered per-module `OnceLock` env reads
//! (each cache/JIT used to sniff its own `FAB_*`); the dev PROBES (trace/profile/redundancy/fingerprint) stay
//! env-only — they're diagnostics, not execution modes.
//!
//! Every field is bit-identity-preserving by contract: interp, interp+JIT, and interp+caches must all produce
//! the SAME geometry (the A/B differential is exactly toggling these). So `Config` is purely a
//! SPEED/where-it-runs choice, never a correctness one — which is why the GUI can flip them freely per platform
//! (web = interp, desktop = +JIT +caches) without changing output.

/// The execution configuration threaded through [`Ctx`](super::Ctx). Construct with [`Config::from_env`] (the
/// harness/CLI path — reads the `FAB_*` gates) or a literal (the embedder path). All-off is [`Config::default`].
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Route numeric user-function calls through the Cranelift JIT (desktop only; needs a
    /// [`NumericJitFactory`](super::NumericJitFactory) — wasm passes none, so this is a no-op there). `FAB_JIT`.
    pub jit: bool,
    /// Memoize user-FUNCTION-call results (N.2c). `FAB_EVAL_CACHE`.
    pub eval_cache: bool,
    /// Skip memoizing a function call whose args exceed this shallow element count — a big key isn't worth
    /// hashing every lookup. `FAB_EVAL_CACHE_ARGCAP` (default 256).
    pub eval_cache_argcap: usize,
    /// Memoize a child-less user-MODULE call's `Geo` subtree — the content-addressed CSG cache (J.5.2a).
    /// Defaults OFF in [`Config::default`] (the baseline) but **ON in [`Config::from_env`]** (the app path) —
    /// it's bit-identity-validated + pure-win; `FAB_CSG_CACHE=0` disables.
    pub csg_cache: bool,
    /// Skip memoizing a module call whose (params + reaching `$`-context) key exceeds this shallow element
    /// count. `FAB_CSG_CACHE_KEYCAP` (default 2048).
    pub csg_cache_keycap: usize,
    /// The eval RESOURCE budget (Q.5): the max number of deterministic eval-steps a single evaluation may
    /// burn before it fails LOUD with an [`Error::Eval`](crate::Error::Eval), or `None` for UNLIMITED (the
    /// default — exact OpenSCAD parity, so no legitimate model ever trips it). `FAB_EVAL_BUDGET`.
    ///
    /// This is the ONE field that can change OUTPUT (the others are pure speed/where-it-runs knobs — see the
    /// module doc): a bound turns a would-be-huge success into an error. But it does so DETERMINISTICALLY —
    /// the step count is eval-steps, NOT wall-time, so the same `(program, budget)` fails at the same point
    /// on every machine (the reproducibility the differential harness + doctrine #36 require). At the `None`
    /// default it never fires, so the A/B differential contract (toggling caches/JIT never changes output)
    /// still holds on the baseline. The UNTRUSTED entry points — the fuzz targets now, the web playground
    /// later — set an explicit bound where the untrusted input actually enters; trusted CLI/desktop stays
    /// unbounded (Ctrl-C, like OpenSCAD).
    pub eval_budget: Option<u64>,
}

impl Default for Config {
    /// All execution accelerators OFF (pure interpreter), caps at their tuned defaults. The conservative,
    /// always-correct baseline — the oracle path and raw-AST tests run on this.
    fn default() -> Self {
        Self {
            jit: false,
            eval_cache: false,
            eval_cache_argcap: 256,
            csg_cache: false,
            csg_cache_keycap: 2048,
            eval_budget: None, // UNLIMITED — OpenSCAD parity; the untrusted paths opt into a bound
        }
    }
}

impl Config {
    /// Read the `FAB_*` env gates — the CLI/harness/GUI entry (`evaluate*` sugar + the models worker). Unlike
    /// [`Config::default`] (the all-off CONSERVATIVE baseline the oracle + differential tests run on), this is
    /// the APP-FACING config, so the SAFE-and-USEFUL accelerator defaults ON here:
    /// - `csg_cache` defaults **ON** — it's bit-identity-validated (the cache-on==off differential, N.2c) and a
    ///   pure win (a hit skips redundant CSG, a miss costs a key hash), so a forgetful desktop run gets it for
    ///   free. `FAB_CSG_CACHE=0` disables it (debugging the cache itself).
    /// - `jit` stays OPT-IN (`FAB_JIT=1`): bit-identical, but net-neutral-to-SLOWER on real geometry-dominated
    ///   models + per-model compile overhead, so defaulting it on would work AGAINST speed.
    /// - `eval_cache` stays OPT-IN (`FAB_EVAL_CACHE=1`): NOT yet proven safe to default on (N.2c.2) — a
    ///   side-effecting call can wrong-hit, so it's off until that lands.
    #[must_use]
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            jit: env_on("FAB_JIT"),
            eval_cache: env_on("FAB_EVAL_CACHE"),
            eval_cache_argcap: env_usize("FAB_EVAL_CACHE_ARGCAP", d.eval_cache_argcap),
            csg_cache: env_override("FAB_CSG_CACHE", true),
            csg_cache_keycap: env_usize("FAB_CSG_CACHE_KEYCAP", d.csg_cache_keycap),
            eval_budget: env_u64_opt("FAB_EVAL_BUDGET"),
        }
    }
}

/// A strict `FAB_*=1` gate (any other value / unset → off).
fn env_on(name: &str) -> bool {
    std::env::var_os(name).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// A `FAB_*` gate with an explicit `default` when UNSET — `=1` forces on, `=0` forces off, anything else (or
/// unset) takes `default`. For a knob that defaults ON in the app path but must stay disable-able (`=0`).
fn env_override(name: &str, default: bool) -> bool {
    match std::env::var_os(name).as_deref().and_then(|s| s.to_str()) {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    }
}

/// A `FAB_*` unsigned tuning cap, parse-or-default.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// An OPTIONAL `FAB_*` u64 limit: `Some(n)` iff the var is set to a parseable non-negative integer, else
/// `None` (unset OR unparseable → unlimited). The budget is opt-in, so a missing/garbage var means "no cap",
/// never a silent default cap.
fn env_u64_opt(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::{Config, env_override};

    /// The CONSERVATIVE baseline is untouched — `Config::default` stays all-off, because the oracle +
    /// differential harness run on it (flipping a default here would silently change what "the baseline" is).
    #[test]
    fn default_is_the_all_off_baseline() {
        let d = Config::default();
        assert!(!d.jit && !d.eval_cache && !d.csg_cache && d.eval_budget.is_none());
    }

    /// `env_override` takes the DEFAULT when the var is unset — the load-bearing branch that makes
    /// `from_env` default `csg_cache` ON in the app path without any env set.
    #[test]
    fn env_override_unset_takes_default() {
        assert!(env_override("FAB_A_KNOB_THAT_IS_NEVER_SET_XZ", true));
        assert!(!env_override("FAB_A_KNOB_THAT_IS_NEVER_SET_XZ", false));
    }
}
