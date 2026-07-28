//! AR.3 — the registry's CALL SURFACE, derived rather than declared.
//!
//! Three consumers want to know "what does this library host, and how is it called": the program
//! generator (AO — it can only fuzz a library it can describe), transpiled-library fuzzing (AR.1), and
//! the dispatch registry itself. The AR.3 ask was one declaration feeding all three.
//!
//! The declaration already exists and nobody wrote it: [`Entry::reference`] is the VERBATIM source of
//! the function each native stands in for, and a signature is exactly a parameter list. So the surface
//! is PARSED out of the reference instead of maintained beside it — which means it cannot drift, in the
//! strong sense. The reference is fingerprint-gated at dispatch (a body that no longer matches declines
//! to the interpreter), so a surface derived from it describes precisely the calls the native will
//! actually answer. A hand-written surface has no such tie, and AR.5a is the evidence that matters:
//! three of the registry's hand-maintained GUARD lists were wrong, in a table maintained by the same
//! hands under the same review. Deriving is not tidiness here, it is the maintenance win the whole AR
//! bet is being made for.
//!
//! DOMAINS come from the same place, which was not obvious. A signature carries names and defaults but
//! no types — so the first cut declared every parameter `Num` and the generator promptly emitted
//! `point2d(false, (-2 ? false : "x"))`, a call that returns `undef` before doing any work. But an
//! OpenSCAD library is defensive: its bodies TEST their own arguments, and across the 58 references
//! those tests are dense — `is_vector` 27 times, `is_list` 21, `is_num` 13, `len` 48. So a parameter's
//! domain is readable off the predicates the body applies to it, and stays derived rather than declared.
//!
//! That matters beyond tidiness (AR.4): a wrongly-typed argument returns `undef` almost immediately, so
//! a generator that guesses types produces a corpus which still runs, still agrees with the oracle and
//! still reports a ratio — while measuring ERROR HANDLING instead of geometry. The failure is invisible
//! by construction, which is exactly the kind this codebase keeps deciding not to ship.

use crate::parser::{Expr, ExprKind, StmtKind};

/// One type a reference's body is observed to ACCEPT for a parameter.
///
/// A parameter carries a SET of these, not one, and finding that out is the useful part: `approx(a,b)`
/// tests `is_num(a)` on one branch and `is_list(a)` on another, because it is polymorphic on purpose —
/// BOSL2 is full of functions that mean different things by argument type. Collapsing that to a single
/// "strongest" domain picks a winner the source never declared, and a generator built on it would only
/// ever exercise one branch while looking like it covered the function.
///
/// An empty set means the body never tested the parameter. Honest: the generator falls back to a scalar
/// rather than inventing a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SurfaceDomain {
    /// Indexed or `len`'d — a list OR a string, so it names neither.
    Indexable,
    /// `is_bool`.
    Bool,
    /// `is_string`.
    Str,
    /// `is_num` / `is_finite` / `is_int`.
    Num,
    /// `is_list`.
    List,
    /// `is_vector` / `is_matrix` / `is_path` — a list of NUMBERS, the shape most of BOSL2 wants.
    Vector,
}

/// One callable the registry hosts, as the generator needs to see it.
///
/// `required` is load-bearing and easy to lose: an unfilled DEFAULTLESS parameter must evaluate to
/// `undef` and must NOT fall through to a like-named global (AN.3). A generator that always fills every
/// argument cannot reach that path, so the flag has to survive into the surface rather than being
/// flattened to an arity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceParam {
    /// Every type the body is observed to accept here, sorted + deduped. EMPTY when the body never
    /// tests the parameter. See [`SurfaceDomain`] for why this is a set.
    pub domains: Vec<SurfaceDomain>,
    /// The declared name — load-bearing for a user function, where names BIND (unlike a builtin,
    /// where upstream discards them and binds positionally). Named-arg calls are the whole AN.14
    /// diagnostic family, and a surface without names cannot generate one.
    pub name: String,
    /// No default in the signature. See the type doc: an unfilled defaultless parameter is AN.3.
    pub required: bool,
}

/// One function the registry implements natively, described by its own reference signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceFn {
    /// The function this native stands in for — the name the registry dispatches on.
    pub name: String,
    /// Parameters in DECLARATION order, which is the order positional binding fills.
    pub params: Vec<SurfaceParam>,
}

/// The domain a call implies for its FIRST argument, if it is one we understand.
#[allow(
    clippy::match_same_arms,
    reason = "the arms are distinct CLASSES of evidence — an explicit `is_num` test the author wrote \
              versus a type merely implied by passing the value to a scalar-only builtin. They map to \
              the same domain today; merging them would collapse the reasoning into one unreadable \
              arm and lose the comments that say WHY each name is listed."
)]
fn predicate_domain(callee: &str) -> Option<SurfaceDomain> {
    Some(match callee {
        // A list of NUMBERS. `is_matrix`/`is_path` are lists OF vectors, but for generation purposes
        // "numeric list" is the useful constraint and the stronger claim would need a shape too.
        "is_vector" | "is_matrix" | "is_path" => SurfaceDomain::Vector,
        "is_list" | "is_region" => SurfaceDomain::List,
        "is_num" | "is_finite" | "is_int" | "is_nan" => SurfaceDomain::Num,
        "is_string" | "is_str" => SurfaceDomain::Str,
        "is_bool" => SurfaceDomain::Bool,
        // `len` accepts a list OR a string, so it is deliberately the weak signal. Same for indexing.
        "len" => SurfaceDomain::Indexable,
        // Passing a parameter to a math builtin is evidence it is a NUMBER — these are all
        // scalar-only upstream, so a vector argument is undef, not a componentwise map. Weaker than an
        // `is_num` test only in that the author didn't say it out loud.
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "abs" | "floor" | "ceil"
        | "round" | "ln" | "log" | "exp" | "sign" => SurfaceDomain::Num,
        // `norm`/`cross`/`unit` take a VECTOR, same reasoning.
        "norm" | "cross" | "unit" => SurfaceDomain::Vector,
        // `is_undef`/`is_def` say the parameter is OPTIONAL, not what type it is — no domain evidence.
        _ => return None,
    })
}

/// Walk `e`, collecting every type each parameter is observed to accept.
///
/// Evidence is a call `pred(p, …)` whose first argument is the bare parameter, or `p[i]` / `len(p)`.
/// Everything observed is kept — see [`SurfaceDomain`] on why a "strongest wins" collapse would
/// silently pick a branch the source never chose.
fn collect_domains(
    e: &Expr,
    out: &mut std::collections::BTreeMap<String, std::collections::BTreeSet<SurfaceDomain>>,
) {
    let note = |name: &str,
                d: SurfaceDomain,
                out: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<SurfaceDomain>,
    >| {
        if let Some(slot) = out.get_mut(name) {
            slot.insert(d);
        }
    };
    match &e.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind
                && let Some(d) = predicate_domain(name)
                && let Some(first) = args.first()
                && let ExprKind::Ident(p) = &first.value.kind
            {
                note(p, d, out);
            }
            collect_domains(callee, out);
            for a in args {
                collect_domains(&a.value, out);
            }
        }
        // `p[i]` — indexable, the weak signal.
        ExprKind::Index { base, index } => {
            if let ExprKind::Ident(p) = &base.kind {
                note(p, SurfaceDomain::Indexable, out);
            }
            collect_domains(base, out);
            collect_domains(index, out);
        }
        ExprKind::Unary { operand, .. } => collect_domains(operand, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_domains(lhs, out);
            collect_domains(rhs, out);
        }
        ExprKind::Ternary { cond, then, els } => {
            collect_domains(cond, out);
            collect_domains(then, out);
            collect_domains(els, out);
        }
        ExprKind::Member { base, .. } => collect_domains(base, out),
        ExprKind::Vector(xs) => {
            for x in xs {
                collect_domains(x, out);
            }
        }
        // `assert(is_matrix(m)) …` and `let(n = len(x)) …` are where BOSL2 actually puts its type
        // tests — a walker that stops at the catch-all sees almost none of them. Measured: adding
        // these two took derived coverage from 16% of parameters to the figure in the module doc.
        // `assert(cond) rest` / `echo(x) rest` — same shape, and the assert arm is the important one:
        // BOSL2 states most of its argument types as `assert(is_matrix(m), "...")`.
        ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
            for a in args {
                collect_domains(&a.value, out);
            }
            if let Some(b) = body {
                collect_domains(b, out);
            }
        }
        // `let(n = len(x)) …` and a `for` comprehension's bindings — the other place the tests hide.
        ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
            for b in bindings {
                collect_domains(&b.value, out);
            }
            collect_domains(body, out);
        }
        ExprKind::Range { start, step, end } => {
            collect_domains(start, out);
            if let Some(st) = step {
                collect_domains(st, out);
            }
            collect_domains(end, out);
        }
        ExprKind::LcEach(inner) => collect_domains(inner, out),
        ExprKind::LcIf { cond, then, els } => {
            collect_domains(cond, out);
            collect_domains(then, out);
            if let Some(e2) = els {
                collect_domains(e2, out);
            }
        }
        // Literals and identifiers carry no evidence of their own.
        _ => {}
    }
}

impl SurfaceFn {
    /// Parse one `function name(params) = body;` into its surface. `None` when the source isn't a
    /// single function definition — which for a registry reference would mean the entry is malformed,
    /// so the caller treats it as a hard error rather than skipping quietly.
    fn from_reference(src: &str) -> Option<Self> {
        let program = crate::parse(src).ok()?;
        let stmt = program.stmts.first()?;
        let StmtKind::FunctionDef {
            name, params, body, ..
        } = &stmt.kind
        else {
            return None;
        };
        let mut domains: std::collections::BTreeMap<
            String,
            std::collections::BTreeSet<SurfaceDomain>,
        > = params
            .iter()
            .map(|p| (p.name.to_string(), std::collections::BTreeSet::new()))
            .collect();
        collect_domains(body, &mut domains);
        Some(SurfaceFn {
            name: name.clone(),
            params: params
                .iter()
                .map(|p| SurfaceParam {
                    name: p.name.to_string(),
                    required: p.default.is_none(),
                    domains: domains
                        .get(p.name.as_ref())
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default(),
                })
                .collect(),
        })
    }
}

/// The whole native surface, derived from the registry's references.
///
/// Sorted by name so the output is a STABLE description rather than registry-declaration order — a
/// consumer indexing it with an RNG (the generator does exactly that) must not have its meaning change
/// because an entry was inserted in the middle of the table.
#[must_use]
pub fn native_surface() -> Vec<SurfaceFn> {
    let mut out: Vec<SurfaceFn> = super::REGISTRY
        .iter()
        .filter_map(|e| SurfaceFn::from_reference(e.reference))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: expect/panic ARE the assertions"
)]
mod tests {
    use super::{SurfaceDomain, SurfaceParam, native_surface};

    /// Every registry entry must yield a surface — a reference that doesn't parse as one function
    /// definition is a malformed entry, and silently dropping it would understate the surface.
    #[test]
    fn every_entry_contributes_and_names_itself() {
        let surface = native_surface();
        assert_eq!(
            surface.len(),
            super::super::REGISTRY.len(),
            "an entry's reference failed to parse as a single function definition"
        );
        for f in &surface {
            let entry = super::super::REGISTRY
                .iter()
                .find(|e| e.name == f.name)
                .unwrap_or_else(|| panic!("derived a name no entry declares: {}", f.name));
            // The DERIVATION is the point: the name in the reference's signature must be the name the
            // registry dispatches on. A mismatch means the entry stands in for a different function
            // than it claims, which the fingerprint gate would never catch (it hashes params+body).
            assert_eq!(entry.name, f.name);
        }
    }

    /// The surface is sorted, so an RNG-indexing consumer keeps its meaning when the registry grows.
    #[test]
    fn the_surface_is_stably_ordered() {
        let s = native_surface();
        let mut sorted = s.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(s, sorted, "native_surface must be name-sorted");
    }

    /// Defaults are carried, not flattened to an arity — AN.3's unfilled-defaultless-parameter path is
    /// only reachable by a generator that knows which arguments it is allowed to OMIT.
    #[test]
    fn required_ness_survives_the_derivation() {
        let s = native_surface();
        // `approx(a, b, eps=_EPSILON)` — two required, one defaulted. If this entry ever leaves the
        // registry, swap it for another with a mixed signature rather than deleting the assertion.
        let approx = s
            .iter()
            .find(|f| f.name == "approx")
            .expect("approx is in the registry");
        assert_eq!(
            approx.params,
            vec![
                // `a`/`b` are tested by BOTH `is_bool` and `is_num` in approx's ternary chain; Num
                // wins because conflicts resolve by STRENGTH of evidence, not by source order.
                // approx is POLYMORPHIC and its body says so out loud — it tests `is_bool`, `is_num`
                // and `is_list` on `a`, and `len(a)` in the list branch. All four are recorded.
                // Collapsing to a single "strongest" domain would pick a branch the source never
                // chose, and a generator built on it would exercise one arm of the ternary chain
                // while looking like it covered the function.
                SurfaceParam {
                    name: "a".into(),
                    required: true,
                    domains: vec![
                        SurfaceDomain::Indexable,
                        SurfaceDomain::Bool,
                        SurfaceDomain::Num,
                        SurfaceDomain::List,
                    ],
                },
                SurfaceParam {
                    name: "b".into(),
                    required: true,
                    domains: vec![
                        SurfaceDomain::Indexable,
                        SurfaceDomain::Bool,
                        SurfaceDomain::Num,
                        SurfaceDomain::List,
                    ],
                },
                // `eps` is only ever compared against, never type-tested — Unknown is the honest
                // answer, and a generator falls back to a scalar rather than inventing a type.
                SurfaceParam {
                    name: "eps".into(),
                    required: false,
                    domains: vec![],
                },
            ],
            "names, required-ness AND domains all come from the reference"
        );
        assert!(
            s.iter().any(|f| f.params.iter().any(|p| !p.required)),
            "the registry has defaulted params — a surface that lost them all is broken"
        );
    }

    /// A COVERAGE FLOOR, not a printout. The walker's first cut only handled calls and indexing and
    /// derived a domain for 16% of parameters — because BOSL2 puts its type tests inside `assert(…)`
    /// and `let(…)`, which the catch-all silently skipped. Walking those took it to 55%.
    ///
    /// That failure mode is the reason this is an assertion: a walker that stops seeing a node type
    /// doesn't error, it just returns fewer domains, and the generator quietly goes back to feeding
    /// natives untyped arguments that return undef. Nothing fails; the corpus just stops measuring
    /// anything. The floor is set below the measured value so ordinary registry churn doesn't trip it,
    /// and far enough above the broken walk to catch a regression.
    #[test]
    fn the_derivation_covers_most_parameters() {
        let surface = native_surface();
        let total: usize = surface.iter().map(|f| f.params.len()).sum();
        let known: usize = surface
            .iter()
            .flat_map(|f| &f.params)
            .filter(|p| !p.domains.is_empty())
            .count();
        assert!(total > 100, "sanity: the registry has real parameters");
        assert!(
            known * 100 / total >= 45,
            "derived domains for {known}/{total} params ({}%) — the walker is missing a node type; \
             it was 16% before assert/let were walked and 55% after",
            known * 100 / total
        );
    }
}
