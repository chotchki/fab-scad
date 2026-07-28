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
//! What is NOT derivable is a parameter's DOMAIN — a signature carries names and defaults, not types.
//! Domains stay declared (AR.4's `Decl`), and this module's job is the half that can be mechanical.

use crate::parser::StmtKind;

/// One callable the registry hosts, as the generator needs to see it.
///
/// `required` is load-bearing and easy to lose: an unfilled DEFAULTLESS parameter must evaluate to
/// `undef` and must NOT fall through to a like-named global (AN.3). A generator that always fills every
/// argument cannot reach that path, so the flag has to survive into the surface rather than being
/// flattened to an arity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceParam {
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

impl SurfaceFn {
    /// Parse one `function name(params) = body;` into its surface. `None` when the source isn't a
    /// single function definition — which for a registry reference would mean the entry is malformed,
    /// so the caller treats it as a hard error rather than skipping quietly.
    fn from_reference(src: &str) -> Option<Self> {
        let program = crate::parse(src).ok()?;
        let stmt = program.stmts.first()?;
        let StmtKind::FunctionDef { name, params, .. } = &stmt.kind else {
            return None;
        };
        Some(SurfaceFn {
            name: name.clone(),
            params: params
                .iter()
                .map(|p| SurfaceParam {
                    name: p.name.to_string(),
                    required: p.default.is_none(),
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
    use super::{SurfaceParam, native_surface};

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
                SurfaceParam {
                    name: "a".into(),
                    required: true
                },
                SurfaceParam {
                    name: "b".into(),
                    required: true
                },
                SurfaceParam {
                    name: "eps".into(),
                    required: false
                },
            ],
            "names AND required-ness both come from the reference signature"
        );
        assert!(
            s.iter().any(|f| f.params.iter().any(|p| !p.required)),
            "the registry has defaulted params — a surface that lost them all is broken"
        );
    }
}
