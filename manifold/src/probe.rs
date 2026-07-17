//! Wall-clock for the kernel's `tracing` stage-attribution probes (BU.2) — wasm-safe by construction.
//!
//! `std::time::Instant::now()` PANICS on wasm32-unknown-unknown (time is unsupported), and the geom
//! worker runs the boolean pipeline on exactly that target: the unconditional `Instant::now()` the
//! probes used killed every browser render (the web-v0.13.0 boot gate caught it). The probes only
//! FEED `tracing::debug!` — timing that nobody subscribes to on wasm anyway — so the wasm arm is a
//! zero-sized no-op reporting 0ms, and native keeps the real clock. Call sites stay one-liners.

/// A started probe clock: a real [`std::time::Instant`] on native, nothing on wasm.
pub(crate) struct ProbeClock {
    #[cfg(not(target_arch = "wasm32"))]
    start: std::time::Instant,
}

impl ProbeClock {
    /// Start the clock (a no-op on wasm).
    pub(crate) fn start() -> Self {
        ProbeClock {
            #[cfg(not(target_arch = "wasm32"))]
            start: std::time::Instant::now(),
        }
    }

    /// Elapsed whole milliseconds since [`start`](Self::start) — always 0 on wasm.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a tracing probe field; a >584-million-year boolean stage is not a real input"
    )]
    pub(crate) fn elapsed_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.start.elapsed().as_millis() as u64
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }
}
