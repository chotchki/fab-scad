//! `fab-lib` — the OpenSCAD library transpiler. `.scad` source in, Rust source out.
//!
//! AR.14.3. This is the compiler half of the AR bet, moved out of fab-lang so the evaluator stops
//! shipping a code generator to every consumer that only wants to evaluate.
//!
//! # The dependency runs one way, and that is the whole architecture
//!
//! fab-lib depends on fab-lang: it reads what fab-lang parses, and it emits code against
//! `fab_lang::rt`. fab-lang does NOT depend on fab-lib — it cannot, because the generated crate
//! (`fab-lib-bosl2`, AR.14.4) will depend on fab-lang for the runtime, and a cycle there is
//! unbuildable.
//!
//! The apparent problem is fab-lang's regen gate, which needs the emitter to check that the
//! checked-in `generated.rs` is current. It dissolves once stated plainly: fab-lang does not need
//! the transpiler AT RUNTIME AT ALL, because the generated file is checked in. Only its TEST does.
//! So fab-lang takes fab-lib as a DEV-dependency, which Cargo permits precisely for this shape.
//!
//! # What it is a pure function of
//!
//! Library source in, Rust source out, with NO opinion on delivery. That is deliberate and it is
//! what keeps chotchki's end state open: `fab_transpile!(["BOSL2/std.scad"])` as a proc macro and a
//! checked-in generated crate are the same transpiler with a different consumer, so switching
//! between them is a decision rather than a rewrite.

pub mod emit;
pub mod library;
