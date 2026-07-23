//! K.3 → AJ — a grammar-directed OpenSCAD program generator covering the WHOLE language surface.
//!
//! A seed picks a deterministic walk through the grammar (via fab-lang's own MT19937 `RandStream`,
//! so a seed replays the exact program on every platform), emitting a VALID-by-construction
//! program: bounded depth + fuel, scope-tracked variables, and ONLY calls to known builtins /
//! already-defined functions and modules. Range magnitudes, `$fn`, recursion and tree depth are
//! all bounded, so a generated program never reproduces the unbounded-comprehension DoS class —
//! the whole corpus stays cheap to evaluate.
//!
//! COVERAGE IS GATED (AJ.1): `grammar_covers_the_language_surface` asserts every construct family
//! appears across the first seeds — a language feature that ships without a production here fails
//! CI with the family named. The v0 grammar covered 7 of 62 families; this is the AJ.2-5 fill.
//!
//! HERMETIC: the file-value builtins (`import()`, `dxf_dim`) are emitted against paths that never
//! exist — they exercise the needs channel + warn-and-undef only, no filesystem dependence. Only
//! SEEDED `rands` is emitted. Dimension-homogeneous geometry trees (2D and 3D never mix in one
//! boolean); extrudes bridge 2D→3D, `projection` 3D→2D.
//!
//! This is the "higher-level fuzzer" / ML-corpus half of the plan: where cargo-fuzz mutates BYTES
//! (dense but adversarial), this emits PROGRAMS (valid, diverse, labelable). The binary runs each
//! through the evaluator + the JIT bit-identity check to attach labels; the `gen_diff` fuzz target
//! (AJ.6) drives the same walk from fuzzer bytes with the Config A/B contract as its oracle.

use fab_lang::RandStream;

/// Bounds that keep every generated program small + cheap to evaluate.
const MAX_EXPR_DEPTH: u32 = 4;
const START_FUEL: u32 = 90;
const MAX_STMTS: i64 = 9;
const RANGE_BOUND: i64 = 32; // range endpoints stay in [-RANGE_BOUND, RANGE_BOUND] → tiny comprehensions

/// Builtins SAFE to call with any args (a type mismatch yields `undef`, never an error), with
/// arity. Calling only these (plus generated functions) keeps programs eval-clean.
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
    // list + string group (AJ.4)
    ("str", 2),
    ("chr", 1),
    ("ord", 1),
    ("concat", 2),
    ("reverse", 1),
    ("search", 2),
    ("lookup", 2),
    // type predicates
    ("is_num", 1),
    ("is_undef", 1),
    ("is_string", 1),
    ("is_list", 1),
    ("is_bool", 1),
    ("is_object", 1),
];

/// The generator state: the RNG plus the lexical scope it's building up.
pub struct Gen {
    rng: RandStream,
    fuel: u32,
    depth: u32,
    vars: Vec<String>,                 // in-scope variable names
    funcs: Vec<(String, Vec<String>)>, // defined functions (name, param names)
    mods: Vec<(String, usize)>,        // defined modules (name, arity)
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
            mods: Vec::new(),
            next_id: 0,
        }
    }

    // --- draw helpers (all on the one MT19937 stream) ---

    /// A uniform index in `[0, n)` (0 when `n == 0`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "n is a tiny arm count; the draw is in [0, n)"
    )]
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
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "spans here are tiny (bounded by the grammar's own constants)"
    )]
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

    /// A whole program: an optional `$`-prologue, a handful of statements, at least one of them
    /// geometry (so `evaluate` yields real output often enough for meaningful labels).
    #[must_use]
    fn program(&mut self) -> String {
        let mut out = String::new();
        // $-assignments (dynamic-scope fallbacks) — SMALL $fn so any circle stays cheap.
        if self.chance(0.4) {
            out.push_str(&format!("$fn = {};\n", self.int_between(0, 12)));
        }
        if self.chance(0.15) {
            out.push_str("$fa = 12;\n$fs = 2;\n");
        }
        let n = self.int_between(1, MAX_STMTS);
        for _ in 0..n {
            if self.fuel == 0 {
                break;
            }
            out.push_str(&self.statement());
            out.push('\n');
        }
        // Guarantee at least one geometry statement — otherwise a value-only program renders empty
        // and every render label collapses to 0 tris.
        out.push_str(&self.geometry3(0));
        out.push('\n');
        out
    }

    fn statement(&mut self) -> String {
        match self.below(10) {
            0 => self.assignment(),
            1 => self.function_def(),
            2 => self.module_def(),
            3 if !self.mods.is_empty() => self.module_call(),
            4 => {
                // let/echo/assert STATEMENT wrapping geometry (assert conds are always-true so the
                // rest of the program keeps evaluating — halting coverage is the byte-fuzzer's job).
                let g = self.geometry3(1);
                match self.below(3) {
                    0 => {
                        let e = self.expr();
                        let v = self.fresh("l");
                        format!("let ({v} = {e}) {g}")
                    }
                    1 => format!("echo(\"s\", {}) {g}", self.expr()),
                    _ => format!("assert(1 == 1) {g}"),
                }
            }
            5 => {
                let n = self.int_between(1, 3);
                let i = self.fresh("i");
                self.vars.push(i.clone());
                let body = self.geometry3(1);
                self.vars.pop();
                format!("intersection_for ({i} = [0:{n}]) {body}")
            }
            6 => {
                // a 2D top-level tree (whole-program 2D renders fine; never mixed into 3D booleans)
                self.geometry2(0)
            }
            _ => self.geometry3(0), // weight 3D geometry highest for render diversity
        }
    }

    /// `id = <expr>;` — binds a fresh variable into scope. Sometimes the RHS is a function LITERAL,
    /// which also registers the name as callable (the named-closure / letrec path).
    fn assignment(&mut self) -> String {
        if self.chance(0.2) {
            // f = function(p) <expr>;
            let name = self.fresh("g");
            let params: Vec<String> = (0..self.int_between(1, 2))
                .map(|_| self.fresh("p"))
                .collect();
            let mark = self.vars.len();
            self.vars.extend(params.iter().cloned());
            let body = self.expr();
            self.vars.truncate(mark);
            self.funcs.push((name.clone(), params.clone()));
            return format!("{name} = function({}) {body};", params.join(", "));
        }
        let e = self.expr();
        let name = self.fresh("v");
        self.vars.push(name.clone());
        format!("{name} = {e};")
    }

    /// `function id(p0, p1, ...) = <expr>;` — params are in scope ONLY for the body; the function
    /// joins the callable set afterward (so later statements can call it, exercising dispatch + the JIT).
    #[allow(clippy::cast_sign_loss, reason = "arity is drawn from [0, 3]")]
    fn function_def(&mut self) -> String {
        let arity = self.int_between(0, 3) as usize;
        let name = self.fresh("f");
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let mark = self.vars.len();
        self.vars.extend(params.iter().cloned());
        let body = self.expr();
        self.vars.truncate(mark); // params leave scope
        self.funcs.push((name.clone(), params.clone()));
        format!("function {name}({}) = {body};", params.join(", "))
    }

    /// `module id(p...) { ...; children(); }` — registers the module; bodies read their params,
    /// place geometry, and exercise the children machinery (`children()`, `children(i)`,
    /// `$children`) so call-site child blocks matter.
    #[allow(clippy::cast_sign_loss, reason = "arity is drawn from [0, 2]")]
    fn module_def(&mut self) -> String {
        let arity = self.int_between(0, 2) as usize;
        let name = self.fresh("m");
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let mark = self.vars.len();
        self.vars.extend(params.iter().cloned());
        let inner = self.geometry3(2);
        let children = match self.below(3) {
            0 => "  children();\n",
            1 => "  if ($children > 0) children(0);\n",
            _ => "  for (ci = [0:$children-1]) children(ci);\n",
        };
        self.vars.truncate(mark);
        self.mods.push((name.clone(), arity));
        format!(
            "module {name}({}) {{\n  {inner}\n{children}}}",
            params.join(", ")
        )
    }

    /// A call to a previously-defined module, usually with a child block (feeding `children()`),
    /// sometimes with a `$`-arg (the dynamic-scope channel).
    fn module_call(&mut self) -> String {
        let idx = self.below(self.mods.len());
        let (name, arity) = self.mods[idx].clone();
        let mut args: Vec<String> = (0..arity).map(|_| self.expr()).collect();
        if self.chance(0.25) {
            args.push(format!("$fn={}", self.int_between(3, 10)));
        }
        let kids = match self.below(3) {
            0 => ";".to_string(),
            1 => format!(" {}", self.geometry3(2)),
            _ => {
                let a = self.geometry3(2);
                let b = self.geometry3(2);
                format!(" {{ {a} {b} }}")
            }
        };
        format!("{name}({}){kids}", args.join(", "))
    }

    // --- geometry: 3D and 2D channels, dimension-homogeneous by construction ---

    /// A 3D geometry statement/child at nesting `d`: a leaf primitive, a wrapper, a boolean of a
    /// couple of children, an extrusion of a 2D tree, or a bounded `for`/`if`.
    fn geometry3(&mut self, d: u32) -> String {
        if d >= 3 || self.fuel == 0 || self.chance(0.3) {
            return self.primitive3();
        }
        self.fuel = self.fuel.saturating_sub(1);
        match self.below(10) {
            0 => {
                let v = self.vec3_pos_small();
                format!("translate({v}) {}", self.geometry3(d + 1))
            }
            1 => {
                let v = self.vec3_angle();
                format!("rotate({v}) {}", self.geometry3(d + 1))
            }
            2 => {
                let v = self.vec3_scale();
                format!("scale({v}) {}", self.geometry3(d + 1))
            }
            3 => {
                let op = self.pick_str(&["union", "difference", "intersection"]);
                let k = self.int_between(2, 3);
                let mut kids = String::new();
                for _ in 0..k {
                    kids.push_str("  ");
                    kids.push_str(&self.geometry3(d + 1));
                    kids.push('\n');
                }
                format!("{op}() {{\n{kids}}}")
            }
            4 => {
                // for(i=[0:n]) child — bounded range, i in scope for the child
                let n = self.int_between(0, 4);
                let i = self.fresh("i");
                self.vars.push(i.clone());
                let body = self.geometry3(d + 1);
                self.vars.pop();
                format!("for ({i} = [0:{n}]) {body}")
            }
            5 => {
                // extrusions bridge 2D → 3D
                let flat = self.geometry2(d + 1);
                if self.chance(0.5) {
                    format!("linear_extrude(height = {}) {flat}", self.int_between(1, 8))
                } else {
                    // rotate_extrude needs an all-positive-x profile: shift the 2D tree right.
                    format!(
                        "rotate_extrude(angle = {}) translate([{}, 0]) {flat}",
                        self.int_between(30, 360),
                        self.int_between(6, 15)
                    )
                }
            }
            6 => {
                // wrappers: color / resize / mirror / multmatrix / hull / minkowski (tiny kids —
                // minkowski cost is multiplicative).
                match self.below(6) {
                    0 => format!(
                        "color(\"{}\") {}",
                        self.pick_str(&["red", "lime", "steelblue"]),
                        self.geometry3(d + 1)
                    ),
                    1 => format!(
                        "resize({}) {}",
                        self.vec3_pos_small(),
                        self.geometry3(d + 1)
                    ),
                    2 => format!(
                        "mirror([{}, {}, 1]) {}",
                        i64::from(self.chance(0.5)),
                        i64::from(self.chance(0.5)),
                        self.geometry3(d + 1)
                    ),
                    3 => {
                        let tx = self.int_between(-5, 5);
                        format!(
                            "multmatrix([[1, 0, 0, {tx}], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]) {}",
                            self.geometry3(d + 1)
                        )
                    }
                    4 => format!("hull() {{ {} {} }}", self.primitive3(), self.primitive3()),
                    _ => "minkowski() { cube(2); sphere(r = 1, $fn = 6); }".to_string(),
                }
            }
            _ => {
                // if(cond) child [else child] — sometimes with an instantiation modifier prefix:
                // `if` takes `! # % *` like any module call (the AA.1 census gap). `!` stays rare —
                // root-capture rewrites the whole render's output.
                let m = if self.chance(0.2) {
                    self.pick_str(&["*", "#", "%", "*#", "%*"])
                } else if self.chance(0.02) {
                    "!"
                } else {
                    ""
                };
                let c = self.expr();
                let then = self.geometry3(d + 1);
                if self.chance(0.5) {
                    let els = self.geometry3(d + 1);
                    format!("{m}if ({c}) {then} else {els}")
                } else {
                    format!("{m}if ({c}) {then}")
                }
            }
        }
    }

    /// A 2D geometry tree (used top-level or under an extrusion — NEVER mixed into a 3D boolean).
    fn geometry2(&mut self, d: u32) -> String {
        if d >= 3 || self.fuel == 0 || self.chance(0.4) {
            return self.primitive2();
        }
        self.fuel = self.fuel.saturating_sub(1);
        match self.below(5) {
            0 => format!(
                "offset(r = {}) {}",
                self.int_between(1, 3),
                self.geometry2(d + 1)
            ),
            1 => format!(
                "translate([{}, {}]) {}",
                self.int_between(-8, 8),
                self.int_between(-8, 8),
                self.geometry2(d + 1)
            ),
            2 => {
                let op = self.pick_str(&["union", "difference", "intersection"]);
                format!(
                    "{op}() {{ {} {} }}",
                    self.geometry2(d + 1),
                    self.geometry2(d + 1)
                )
            }
            3 => format!("projection() {}", self.primitive3()), // 3D → 2D bridge
            _ => self.primitive2(),
        }
    }

    /// A 3D leaf primitive with POSITIVE bounded dimensions.
    fn primitive3(&mut self) -> String {
        match self.below(3) {
            0 => format!("cube({});", self.vec3_pos_small()),
            1 => format!("sphere(r = {});", self.int_between(1, 20)),
            _ => format!(
                "cylinder(h = {}, r = {});",
                self.int_between(1, 20),
                self.int_between(1, 10)
            ),
        }
    }

    /// A 2D leaf primitive — square / circle / polygon / text, all tiny.
    fn primitive2(&mut self) -> String {
        match self.below(4) {
            0 => format!(
                "square([{}, {}]);",
                self.int_between(1, 12),
                self.int_between(1, 12)
            ),
            1 => format!(
                "circle(r = {}, $fn = {});",
                self.int_between(1, 10),
                self.int_between(3, 10)
            ),
            2 => {
                let w = self.int_between(2, 10);
                let h = self.int_between(2, 10);
                format!("polygon(points = [[0, 0], [{w}, 0], [0, {h}]]);")
            }
            _ => format!(
                "text(\"{}\", size = {});",
                self.pick_str(&["hi", "A9", "fab"]),
                self.int_between(2, 6)
            ),
        }
    }

    fn vec3_pos_small(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(1, 12),
            self.int_between(1, 12),
            self.int_between(1, 12)
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
            self.int_between(1, 3),
            self.int_between(1, 3),
            self.int_between(1, 3)
        )
    }

    // --- expressions ---

    fn expr(&mut self) -> String {
        self.fuel = self.fuel.saturating_sub(1);
        if self.depth >= MAX_EXPR_DEPTH || self.fuel == 0 || self.chance(0.3) {
            return self.atom();
        }
        self.depth += 1;
        let e = match self.below(15) {
            0 => self.binary(),
            1 => self.unary(),
            2 => self.ternary(),
            3 => self.vector(),
            4 => self.range(),
            5 => self.comprehension(),
            6 => self.builtin_call(),
            7 => self.chain_expr(),
            8 => self.index_or_swizzle(),
            9 => self.object_expr(),
            10 => self.fn_literal_call(),
            11 => self.metrics_or_rands(),
            12 if self.chance(0.25) => self.file_fn(), // rare: warn+undef channel only
            _ => self.user_call(),
        };
        self.depth -= 1;
        e
    }

    fn binary(&mut self) -> String {
        let op = self.pick_str(&[
            "+", "-", "*", "/", "%", "<", "<=", ">", ">=", "==", "!=", "&&", "||", "&", "|", "<<",
            ">>", "^",
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

    /// A vector literal, sometimes with an `each` splice mixed in (AJ.3).
    fn vector(&mut self) -> String {
        let k = self.int_between(2, 4);
        let mut items: Vec<String> = (0..k).map(|_| self.expr()).collect();
        if self.chance(0.25) {
            let n = self.int_between(0, 3);
            items.push(format!("each [0:{n}]"));
        }
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

    /// Comprehensions in all their forms: plain, if-filtered, C-style, and let-carrying.
    fn comprehension(&mut self) -> String {
        let i = self.fresh("i");
        match self.below(4) {
            0 => {
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                format!("[for ({i} = {r}) {body}]")
            }
            1 => {
                // filtered: [for (i = r) if (cond) body]
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                let m = self.int_between(2, 3);
                format!("[for ({i} = {r}) if ({i} % {m} == 0) {body}]")
            }
            2 => {
                // C-style: bounded by construction (i strictly increases to a small cap)
                let n = self.int_between(1, 6);
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                format!("[for ({i} = 0; {i} < {n}; {i} = {i} + 1) {body}]")
            }
            _ => {
                // let-carrying: [for (i = r) let (t = body) t]
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                let t = self.fresh("t");
                format!("[for ({i} = {r}) let ({t} = {body}) {t}]")
            }
        }
    }

    /// `let`/`assert`/`echo` EXPRESSION chains (AJ.3) — assert conds always-true, and an
    /// occasional DUPLICATE let name (the AH.2.3 first-wins rule). PARENTHESIZED: our parser only
    /// accepts a chain at expression-head position (bare `a * let(…) b` is a filed gap vs
    /// upstream's bison grammar — AJ finding #1), and valid-by-construction is this crate's
    /// contract.
    fn chain_expr(&mut self) -> String {
        match self.below(4) {
            0 => {
                let v = self.fresh("a");
                let e1 = self.expr();
                self.vars.push(v.clone());
                let body = self.expr();
                self.vars.pop();
                format!("(let({v} = {e1}) {body})")
            }
            1 => {
                // duplicate binding in ONE let: first wins (upstream-pinned)
                let v = self.fresh("a");
                let e1 = self.expr();
                let e2 = self.expr();
                self.vars.push(v.clone());
                let body = self.expr();
                self.vars.pop();
                format!("(let({v} = {e1}, {v} = {e2}) {body})")
            }
            2 => format!("(assert(1 == 1) {})", self.expr()),
            _ => format!("(echo(\"e\", {}) {})", self.expr(), self.expr()),
        }
    }

    /// Postfix access: indexing a vector, or swizzling one (single + multi-letter, both sets).
    fn index_or_swizzle(&mut self) -> String {
        if self.chance(0.5) {
            let v = self.vector();
            let i = self.int_between(0, 3);
            format!("({v})[{i}]")
        } else {
            let sw = self.pick_str(&[".x", ".y", ".z", ".wy", ".rgba", ".xyz"]);
            format!(
                "([{}, {}, {}, {}]){sw}",
                self.expr(),
                self.expr(),
                self.expr(),
                self.expr()
            )
        }
    }

    /// The object family (AJ.4): constructor forms (named members, copy + edit lists with removes),
    /// member access, methods (`this`), `has_key`.
    fn object_expr(&mut self) -> String {
        match self.below(4) {
            0 => format!("object(a = {}, b = {}).a", self.expr(), self.expr()),
            1 => {
                // copy + edit list: remove one member, set another, append a named one
                format!(
                    "object(object(a = 1, b = {}), [[\"a\"], [\"b\", {}]], c = {})",
                    self.expr(),
                    self.expr(),
                    self.expr()
                )
            }
            2 => {
                // method: receiver bound at extraction, `this` injected at call
                format!("object(a = {}, f = function(this) this.a).f()", self.expr())
            }
            _ => format!("has_key(object(k = 1), \"{}\")", self.pick_str(&["k", "z"])),
        }
    }

    /// A function LITERAL applied immediately (closure over the argument).
    fn fn_literal_call(&mut self) -> String {
        let p = self.fresh("p");
        self.vars.push(p.clone());
        let body = self.expr();
        self.vars.pop();
        format!("(function({p}) {body})({})", self.expr())
    }

    /// textmetrics/fontmetrics (deterministic — bundled font) and SEEDED rands.
    fn metrics_or_rands(&mut self) -> String {
        match self.below(4) {
            0 => format!(
                "textmetrics(\"{}\", size = {}).advance",
                self.pick_str(&["hi", "fab"]),
                self.int_between(2, 12)
            ),
            1 => "fontmetrics().interline".to_string(),
            _ => format!(
                "rands(0, 1, {}, {})",
                self.int_between(1, 3),
                self.int_between(0, 999)
            ),
        }
    }

    /// The file-value builtins against paths that NEVER exist — hermetic: exercises the needs
    /// channel + warn-and-undef, no filesystem dependence, rare by weight.
    fn file_fn(&mut self) -> String {
        if self.chance(0.5) {
            "import(\"__fab_gen_no_such__.json\")".to_string()
        } else {
            "dxf_dim(file = \"__fab_gen_no_such__.dxf\", name = \"d\")".to_string()
        }
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

    /// A call to a previously-defined function (arity-correct) if any exist, else fall back to an
    /// atom. Sometimes NAMED-arg form (`f(p0=…, …)` — the positional-after-named binding rules).
    fn user_call(&mut self) -> String {
        if self.funcs.is_empty() {
            return self.atom();
        }
        let idx = self.below(self.funcs.len());
        let (name, params) = self.funcs[idx].clone();
        if params.is_empty() {
            return format!("{name}()");
        }
        if self.chance(0.3) {
            // named first param + positionals for the rest — the AH.2.4 lowest-unfilled rule
            let named = format!("{}={}", params[0], self.expr());
            let rest: Vec<String> = (1..params.len()).map(|_| self.expr()).collect();
            let mut args = vec![named];
            args.extend(rest);
            return format!("{name}({})", args.join(", "));
        }
        let args: Vec<String> = (0..params.len()).map(|_| self.expr()).collect();
        format!("{name}({})", args.join(", "))
    }

    /// A leaf: an in-scope variable, or a literal (number / bool / string / undef / IEEE specials).
    fn atom(&mut self) -> String {
        if !self.vars.is_empty() && self.chance(0.5) {
            let idx = self.below(self.vars.len());
            return self.vars[idx].clone();
        }
        match self.below(9) {
            0..=2 => self.int_between(-50, 50).to_string(),
            3 => {
                // a small decimal
                let n = self.int_between(-500, 500);
                format!("{}.{}", n / 10, (n.abs() % 10))
            }
            4 => if self.chance(0.5) { "true" } else { "false" }.to_string(),
            5 => "undef".to_string(),
            6 => if self.chance(0.5) {
                "(1 / 0)"
            } else {
                "(0 / 0)"
            }
            .to_string(),
            7 => format!(
                "\"{}\"",
                self.pick_str(&["a", "x", "hello", "", "\u{2192}", "\\u0041"])
            ),
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

    /// AJ.1 — the COVERAGE GATE: every language-construct family must appear somewhere in the
    /// first N seeds' output. This is what keeps the grammar honest: a language feature that
    /// ships without a generator production fails HERE, in CI, with the missing family named.
    /// Markers are cheap substrings, deliberately chosen to be unambiguous in emitted text.
    #[test]
    fn grammar_covers_the_language_surface() {
        const SEEDS: u32 = 4000;
        let mut corpus = String::new();
        for seed in 0..SEEDS {
            corpus.push_str(&generate(seed));
            corpus.push('\n');
        }
        let families: &[(&str, &str)] = &[
            // statements + geometry tree
            ("user module def", "module "),
            ("children()", "children("),
            ("$children read", "$children"),
            ("intersection_for", "intersection_for"),
            ("modifier %", "%"),
            ("modifier #", "#"),
            ("if statement", "if ("),
            ("$-assignment", "$fn = "),
            ("$-arg at call", "$fn="),
            // 2D + extrudes + wrappers
            ("square", "square("),
            ("circle", "circle("),
            ("polygon", "polygon("),
            ("text", "text("),
            ("linear_extrude", "linear_extrude("),
            ("rotate_extrude", "rotate_extrude("),
            ("offset", "offset("),
            ("projection", "projection("),
            ("color", "color("),
            ("resize", "resize("),
            ("mirror", "mirror("),
            ("multmatrix", "multmatrix("),
            ("hull", "hull("),
            ("minkowski", "minkowski("),
            // expression forms
            ("let chain", "let("),
            ("assert chain", "assert("),
            ("echo chain", "echo("),
            ("each splice", "each "),
            ("comprehension", "[for ("),
            ("C-style for", " = 0; "),
            ("comprehension if", ") if ("),
            ("indexing", ")["),
            ("swizzle .x", ".x"),
            ("multi-swizzle", ".wy"),
            ("function literal", "function("),
            ("named user arg", "0="),
            ("bit and", " & "),
            ("bit or", " | "),
            ("shift left", " << "),
            ("shift right", " >> "),
            ("power", " ^ "),
            ("undef literal", "undef"),
            ("inf arithmetic", "(1 / 0)"),
            ("nan arithmetic", "(0 / 0)"),
            ("unicode string", "\u{2192}"),
            // objects + methods + metrics + string/table builtins
            ("object constructor", "object("),
            ("object edit list", "[[\""),
            ("method this", "this"),
            ("has_key", "has_key("),
            ("is_object", "is_object("),
            ("textmetrics", "textmetrics("),
            ("fontmetrics", "fontmetrics("),
            ("str", "str("),
            ("chr", "chr("),
            ("ord", "ord("),
            ("search", "search("),
            ("lookup", "lookup("),
            ("concat", "concat("),
            ("reverse", "reverse("),
            ("seeded rands", "rands("),
            ("type predicate", "is_num("),
            ("expression import", "import("),
            ("dxf_dim", "dxf_dim("),
        ];
        let missing: Vec<&str> = families
            .iter()
            .filter(|(_, marker)| !corpus.contains(marker))
            .map(|(family, _)| *family)
            .collect();
        assert!(
            missing.is_empty(),
            "grammar never emitted {} of {} families across {SEEDS} seeds: {missing:?}",
            missing.len(),
            families.len()
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
