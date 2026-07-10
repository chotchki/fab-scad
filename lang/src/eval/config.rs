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
    /// `FAB_CSG_CACHE`.
    pub csg_cache: bool,
    /// Skip memoizing a module call whose (params + reaching `$`-context) key exceeds this shallow element
    /// count. `FAB_CSG_CACHE_KEYCAP` (default 2048).
    pub csg_cache_keycap: usize,
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
        }
    }
}

impl Config {
    /// Read the `FAB_*` env gates — the CLI/harness entry (`evaluate*` sugar + the models worker). The
    /// execution flags are strict `=1` (a NEW eval path stays off unless explicitly enabled), matching the
    /// per-module gates this replaces; the caps parse-or-default.
    #[must_use]
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            jit: env_on("FAB_JIT"),
            eval_cache: env_on("FAB_EVAL_CACHE"),
            eval_cache_argcap: env_usize("FAB_EVAL_CACHE_ARGCAP", d.eval_cache_argcap),
            csg_cache: env_on("FAB_CSG_CACHE"),
            csg_cache_keycap: env_usize("FAB_CSG_CACHE_KEYCAP", d.csg_cache_keycap),
        }
    }
}

/// A strict `FAB_*=1` gate (any other value / unset → off).
fn env_on(name: &str) -> bool {
    std::env::var_os(name).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// A `FAB_*` unsigned tuning cap, parse-or-default.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}
