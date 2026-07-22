//! K.3 v0 — a grammar-directed OpenSCAD program generator. A seed picks a deterministic walk through the
//! language grammar (via fab-lang's own MT19937 `RandStream`, so a seed replays the exact program on every
//! platform), emitting a VALID-by-construction program: bounded depth + fuel, scope-tracked variables, and
//! ONLY calls to known builtins / already-defined functions (an unknown call is the one thing the evaluator
//! rejects LOUD, so the grammar never emits one). Range magnitudes are bounded, so a generated program never
//! reproduces the unbounded-comprehension DoS the fuzzer found — the whole corpus is cheap to evaluate.
//!
//! This is the "higher-level fuzzer" / ML-corpus half of the plan: where cargo-fuzz mutates BYTES (dense but
//! adversarial), this emits PROGRAMS (valid, diverse, labelable). The binary ([`crate`]'s `main`) runs each
//! through the evaluator + the JIT bit-identity check to attach labels — the supervised signal.

use fab_lang::RandStream;

/// Bounds that keep every generated program small + cheap to evaluate (no huge comprehensions, no runaway
/// recursion depth on our side — the evaluator's own MAX_DEPTH still guards generated self-recursion).
const MAX_EXPR_DEPTH: u32 = 4;
const START_FUEL: u32 = 60;
const MAX_STMTS: i64 = 8;
const RANGE_BOUND: i64 = 32; // range endpoints stay in [-RANGE_BOUND, RANGE_BOUND] → tiny comprehensions

/// A curated set of builtins that are SAFE to call with any args (a type mismatch yields `undef`, never an
/// error), with their arity. Calling only these (plus generated functions) keeps programs eval-clean.
const BUILTINS: &[(&str, usize)] = &[
    ("sin", 1),
    ("cos", 1),
    ("tan", 1),
    ("asin", 1),
    ("acos", 1),
    ("atan", 1),
    ("sqrt", 1),
    ("abs", 1),
    ("floor", 1),
    ("ceil", 1),
    ("round", 1),
    ("ln", 1),
    ("exp", 1),
    ("sign", 1),
    ("norm", 1),
    ("len", 1),
    ("pow", 2),
    ("atan2", 2),
    ("min", 2),
    ("max", 2),
    ("cross", 2),
];

/// The generator state: the RNG plus the lexical scope it's building up.
pub struct Gen {
    rng: RandStream,
    fuel: u32,
    depth: u32,
    vars: Vec<String>,           // in-scope variable names
    funcs: Vec<(String, usize)>, // defined functions (name, arity)
    next_id: u32,
}

/// Generate the program for `seed` — deterministic + reproducible (same seed → same bytes, every platform).
#[must_use]
pub fn generate(seed: u32) -> String {
    Gen::new(seed).program()
}

impl Gen {
    #[must_use]
    fn new(seed: u32) -> Self {
        Self {
            rng: RandStream::seeded(seed),
            fuel: START_FUEL,
            depth: 0,
            vars: Vec::new(),
            funcs: Vec::new(),
            next_id: 0,
        }
    }

    // --- draw helpers (all on the one MT19937 stream) ---

    /// A uniform index in `[0, n)` (0 when `n == 0`).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            // next_one is [min, max) → floor stays < n; clamp guards the (unreachable) max endpoint.
            (self.rng.next_one(0.0, n as f64) as usize).min(n - 1)
        }
    }

    /// True with probability `p`.
    fn chance(&mut self, p: f64) -> bool {
        self.rng.next_one(0.0, 1.0) < p
    }

    /// A uniform integer in `[lo, hi]` (inclusive).
    fn int_between(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        lo + self.below((hi - lo + 1) as usize) as i64
    }

    /// Pick one of `xs` (borrowing it out so the borrow of `self` ends before the caller draws again).
    fn pick_str(&mut self, xs: &[&'static str]) -> &'static str {
        xs[self.below(xs.len())]
    }

    /// A fresh, collision-free identifier with the given prefix (prefixes avoid keyword/builtin clashes).
    fn fresh(&mut self, prefix: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{prefix}{id}")
    }

    // --- program + statements ---

    /// A whole program: a handful of statements, at least one of them geometry (so `evaluate` yields a mesh
    /// often enough to make the render label meaningful).
    #[must_use]
    fn program(&mut self) -> String {
        let n = self.int_between(1, MAX_STMTS);
        let mut out = String::new();
        for _ in 0..n {
            if self.fuel == 0 {
                break;
            }
            out.push_str(&self.statement());
            out.push('\n');
        }
        // Guarantee at least one geometry statement — otherwise a value-only program renders an empty mesh
        // and every render label collapses to 0 tris.
        out.push_str(&self.geometry(0));
        out.push('\n');
        out
    }

    fn statement(&mut self) -> String {
        match self.below(4) {
            0 => self.assignment(),
            1 => self.function_def(),
            _ => self.geometry(0), // weight geometry higher (2/4) for render diversity
        }
    }

    /// `id = <expr>;` — binds a fresh variable into scope.
    fn assignment(&mut self) -> String {
        let e = self.expr();
        let name = self.fresh("v");
        self.vars.push(name.clone());
        format!("{name} = {e};")
    }

    /// `function id(p0, p1, ...) = <expr>;` — params are in scope ONLY for the body; the function joins the
    /// callable set afterward (so later statements can call it, exercising user-function dispatch + the JIT).
    fn function_def(&mut self) -> String {
        let arity = self.int_between(0, 3) as usize;
        let name = self.fresh("f");
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let mark = self.vars.len();
        self.vars.extend(params.iter().cloned());
        let body = self.expr();
        self.vars.truncate(mark); // params leave scope
        self.funcs.push((name.clone(), arity));
        format!("function {name}({}) = {body};", params.join(", "))
    }

    // --- geometry (bounded module tree) ---

    /// A geometry statement/child at nesting `d`: a leaf primitive, a transform of a child, a boolean of a
    /// couple children, or a bounded `for`/`if`. Depth-bounded so the module tree stays small.
    fn geometry(&mut self, d: u32) -> String {
        if d >= 3 || self.fuel == 0 || self.chance(0.35) {
            return self.primitive();
        }
        self.fuel = self.fuel.saturating_sub(1);
        match self.below(6) {
            0 => {
                let v = self.vec3_pos_small();
                format!("translate({v}) {}", self.geometry(d + 1))
            }
            1 => {
                let v = self.vec3_angle();
                format!("rotate({v}) {}", self.geometry(d + 1))
            }
            2 => {
                let v = self.vec3_scale();
                format!("scale({v}) {}", self.geometry(d + 1))
            }
            3 => {
                let op = self.pick_str(&["union", "difference", "intersection"]);
                let k = self.int_between(2, 3);
                let mut kids = String::new();
                for _ in 0..k {
                    kids.push_str("  ");
                    kids.push_str(&self.geometry(d + 1));
                    kids.push('\n');
                }
                format!("{op}() {{\n{kids}}}")
            }
            4 => {
                // for(i=[0:n]) child — bounded range, i in scope for the child
                let n = self.int_between(0, 4);
                let i = self.fresh("i");
                self.vars.push(i.clone());
                let body = self.geometry(d + 1);
                self.vars.pop();
                format!("for ({i} = [0:{n}]) {body}")
            }
            _ => {
                // if(cond) child [else child] — sometimes with an instantiation modifier prefix:
                // `if` takes `! # % *` like any module call (the AA.1 census gap), so the fuzzer
                // keeps that grammar corner exercised. `!` stays rare — root-capture rewrites the
                // whole render's output, which starves every other statement of coverage.
                let m = if self.chance(0.2) {
                    self.pick_str(&["*", "#", "%", "*#", "%*"])
                } else if self.chance(0.02) {
                    "!"
                } else {
                    ""
                };
                let c = self.expr();
                let then = self.geometry(d + 1);
                if self.chance(0.5) {
                    let els = self.geometry(d + 1);
                    format!("{m}if ({c}) {then} else {els}")
                } else {
                    format!("{m}if ({c}) {then}")
                }
            }
        }
    }

    /// A leaf primitive with POSITIVE bounded dimensions (so it renders real geometry, not empty/undef).
    fn primitive(&mut self) -> String {
        match self.below(3) {
            0 => format!("cube({});", self.vec3_pos_small()),
            1 => format!("sphere(r = {});", self.int_between(1, 20)),
            _ => format!(
                "cylinder(h = {}, r = {});",
                self.int_between(1, 20),
                self.int_between(1, 20)
            ),
        }
    }

    fn vec3_pos_small(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(1, 20),
            self.int_between(1, 20),
            self.int_between(1, 20)
        )
    }

    fn vec3_angle(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(-180, 180),
            self.int_between(-180, 180),
            self.int_between(-180, 180)
        )
    }

    fn vec3_scale(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(1, 5),
            self.int_between(1, 5),
            self.int_between(1, 5)
        )
    }

    // --- expressions (depth + fuel bounded) ---

    /// An expression. Bails to a leaf when out of depth/fuel or by chance, so the tree is finite + small.
    fn expr(&mut self) -> String {
        self.fuel = self.fuel.saturating_sub(1);
        if self.depth >= MAX_EXPR_DEPTH || self.fuel == 0 || self.chance(0.35) {
            return self.atom();
        }
        self.depth += 1;
        let e = match self.below(8) {
            0 => self.binary(),
            1 => self.unary(),
            2 => self.ternary(),
            3 => self.vector(),
            4 => self.range(),
            5 => self.comprehension(),
            6 => self.builtin_call(),
            _ => self.user_call(),
        };
        self.depth -= 1;
        e
    }

    fn binary(&mut self) -> String {
        let op = self.pick_str(&[
            "+", "-", "*", "/", "%", "<", "<=", ">", ">=", "==", "!=", "&&", "||",
        ]);
        format!("({} {} {})", self.expr(), op, self.expr())
    }

    fn unary(&mut self) -> String {
        let op = self.pick_str(&["-", "!"]);
        format!("{}({})", op, self.expr())
    }

    fn ternary(&mut self) -> String {
        format!("({} ? {} : {})", self.expr(), self.expr(), self.expr())
    }

    fn vector(&mut self) -> String {
        let k = self.int_between(2, 4);
        let items: Vec<String> = (0..k).map(|_| self.expr()).collect();
        format!("[{}]", items.join(", "))
    }

    /// `[lo:hi]` or `[lo:step:hi]` with SMALL, bounded endpoints — never a runaway range.
    fn range(&mut self) -> String {
        let lo = self.int_between(-RANGE_BOUND, RANGE_BOUND);
        let hi = self.int_between(lo, lo + RANGE_BOUND);
        if self.chance(0.5) {
            format!("[{lo}:{}:{hi}]", self.int_between(1, 4))
        } else {
            format!("[{lo}:{hi}]")
        }
    }

    /// `[for (i = <range>) <expr>]` — the loop var is in scope only for the body. Bounded range keeps it tiny.
    fn comprehension(&mut self) -> String {
        let r = self.range();
        let i = self.fresh("i");
        self.vars.push(i.clone());
        let body = self.expr();
        self.vars.pop();
        format!("[for ({i} = {r}) {body}]")
    }

    /// A call to a KNOWN builtin (arity-correct), so it never trips the unknown-call error.
    fn builtin_call(&mut self) -> String {
        let (name, arity) = *self.pick_builtin();
        let args: Vec<String> = (0..arity).map(|_| self.expr()).collect();
        format!("{name}({})", args.join(", "))
    }

    fn pick_builtin(&mut self) -> &'static (&'static str, usize) {
        &BUILTINS[self.below(BUILTINS.len())]
    }

    /// A call to a previously-defined function (arity-correct) if any exist, else fall back to an atom. This
    /// is what exercises user-function dispatch + the JIT on generated bodies.
    fn user_call(&mut self) -> String {
        if self.funcs.is_empty() {
            return self.atom();
        }
        let idx = self.below(self.funcs.len());
        let (name, arity) = self.funcs[idx].clone();
        let args: Vec<String> = (0..arity).map(|_| self.expr()).collect();
        format!("{name}({})", args.join(", "))
    }

    /// A leaf: an in-scope variable, or a literal (number / bool / short string).
    fn atom(&mut self) -> String {
        if !self.vars.is_empty() && self.chance(0.5) {
            let idx = self.below(self.vars.len());
            return self.vars[idx].clone();
        }
        match self.below(6) {
            0..=2 => self.int_between(-50, 50).to_string(),
            3 => {
                // a small decimal
                let n = self.int_between(-500, 500);
                format!("{}.{}", n / 10, (n.abs() % 10))
            }
            4 => if self.chance(0.5) { "true" } else { "false" }.to_string(),
            _ => format!("\"{}\"", self.pick_str(&["a", "x", "hello", ""])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::generate;

    /// Determinism: a seed maps to exactly one program, always (the reproducible-replay guarantee).
    #[test]
    fn seed_is_reproducible() {
        for seed in [0u32, 1, 42, 9999, u32::MAX] {
            assert_eq!(
                generate(seed),
                generate(seed),
                "seed {seed} must replay identically"
            );
        }
    }

    /// Different seeds generally differ (a sanity check that the walk actually branches on the RNG).
    #[test]
    fn seeds_differ() {
        let a = generate(1);
        let b = generate(2);
        assert_ne!(
            a, b,
            "distinct seeds should (almost always) give distinct programs"
        );
    }

    /// Every generated program PARSES — the "valid by construction" contract. If this ever fails, the grammar
    /// emitted something the parser rejects, which is a generator bug, not an evaluator finding.
    #[test]
    fn generated_programs_parse() {
        for seed in 0..2000u32 {
            let src = generate(seed);
            assert!(
                fab_lang::parse(&src).is_ok(),
                "seed {seed} produced an unparseable program:\n{src}"
            );
        }
    }
}
