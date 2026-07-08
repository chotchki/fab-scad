//! The customizer: OpenSCAD's special-comment annotations on top-level parameters.
//!
//! OpenSCAD's Customizer panel is driven ENTIRELY by comments — the language proper ignores them, but
//! a `/* [Group] */` header, a trailing `x = 5; // description [0:10]`, and the `[…]` widget hint
//! together describe an editable parameter. Our lexer PRESERVES comments ([`Lexed::all`]), so this
//! layer reconstructs that metadata WITHOUT re-lexing: it correlates each top-level assignment with
//! its active group + trailing annotation.
//!
//! SCOPE (H.4): top-level (root-scope) assignments only — that's what OpenSCAD's Customizer shows.
//! Each [`CustomParam`] keeps the value's byte span, so a UI can rewrite just the value in place
//! (the H.4.4 lossless-enough edit path). Widget parsing (H.4.3) is best-effort: a hint that doesn't
//! match a known shape yields `constraint: None` but the `description` still survives.

use crate::Span;
use crate::lexer::TokenKind;
use crate::parser::StmtKind;

/// The customizer view of a program: its editable top-level parameters, in source order.
#[derive(Debug, Clone, PartialEq)]
pub struct Customizer {
    /// Editable top-level parameters, in source order.
    pub params: Vec<CustomParam>,
}

/// One customizer parameter — a top-level assignment plus its comment annotations.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomParam {
    /// The assignment's variable name.
    pub name: String,
    /// The active group header (`/* [Name] */`) in effect at this assignment, if any.
    pub group: Option<String>,
    /// The trailing-comment description (the text before the `[…]` widget hint), if any.
    pub description: Option<String>,
    /// The parsed widget constraint from the `[…]` hint, if it matched a known shape.
    pub constraint: Option<Constraint>,
    /// Byte span of the VALUE expression — a UI edits this slice in place.
    pub value_span: Span,
}

/// A parsed widget hint (`// … [hint]`).
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    /// `[min:max]` or `[min:step:max]` — a slider.
    Range {
        /// Lower bound.
        min: f64,
        /// Step between stops (the 3-part form).
        step: Option<f64>,
        /// Upper bound.
        max: f64,
    },
    /// `[v, …]` or `[key:label, …]` — a dropdown.
    Dropdown(Vec<DropdownItem>),
    /// `[maxlen]` — a string's maximum length.
    MaxLength(u64),
}

/// One dropdown entry: a value with an optional display label (`key:label`).
#[derive(Debug, Clone, PartialEq)]
pub struct DropdownItem {
    /// The stored value.
    pub value: String,
    /// The display label (the `key:label` form), if given.
    pub label: Option<String>,
}

/// Build the [`Customizer`] annotation set for OpenSCAD `source`.
///
/// # Errors
/// Propagates a lex/parse [`Error`](crate::Error) for malformed source.
pub fn customize(source: &str) -> crate::Result<Customizer> {
    let lexed = crate::lex(source)?;
    let program = crate::parse(source)?;

    // Group headers in source order — `(byte_start, name)`, from `/* [Name] */` block comments.
    let mut groups: Vec<(usize, String)> = Vec::new();
    for tok in &lexed.all {
        if let TokenKind::BlockComment(raw) = tok.kind
            && let Some(name) = group_header(raw)
        {
            groups.push((tok.span.start, name));
        }
    }

    let mut params = Vec::new();
    for stmt in &program.stmts {
        if let StmtKind::Assignment { name, value } = &stmt.kind {
            // The active group is the LAST header positioned before this assignment.
            let group = groups
                .iter()
                .rev()
                .find(|(pos, _)| *pos < stmt.span.start)
                .map(|(_, n)| n.clone());
            let (description, constraint) = match trailing_comment(source, &lexed, stmt.span.end) {
                Some(text) => annotation(text),
                None => (None, None),
            };
            params.push(CustomParam {
                name: name.to_string(),
                group,
                description,
                constraint,
                value_span: value.span.clone(),
            });
        }
    }
    Ok(Customizer { params })
}

/// A `/* [Name] */` group header → `Name` (trimmed). `None` for an ordinary block comment.
fn group_header(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix("/*")?.strip_suffix("*/")?.trim();
    let name = inner.strip_prefix('[')?.strip_suffix(']')?;
    Some(name.trim().to_string())
}

/// The line comment immediately trailing byte offset `end` on the SAME line (no newline between), if
/// any — the assignment's annotation comment.
fn trailing_comment<'s>(source: &'s str, lexed: &crate::Lexed<'s>, end: usize) -> Option<&'s str> {
    lexed.all.iter().find_map(|tok| match tok.kind {
        TokenKind::LineComment(raw)
            if tok.span.start >= end && !source.get(end..tok.span.start)?.contains('\n') =>
        {
            Some(raw)
        }
        _ => None,
    })
}

/// Split a trailing comment `// description [hint]` into its description + parsed constraint. A hint
/// that doesn't parse still leaves the description intact.
fn annotation(raw: &str) -> (Option<String>, Option<Constraint>) {
    let text = raw.strip_prefix("//").unwrap_or(raw).trim();
    if let Some(open) = text.rfind('[')
        && let Some(inner) = text.strip_suffix(']')
    {
        let inner = inner.get(open + 1..).unwrap_or("");
        let desc = text.get(..open).unwrap_or("").trim();
        let description = (!desc.is_empty()).then(|| desc.to_string());
        return (description, constraint(inner));
    }
    let description = (!text.is_empty()).then(|| text.to_string());
    (description, None)
}

/// Parse the inside of a `[…]` widget hint. `None` if it matches no known shape.
fn constraint(inner: &str) -> Option<Constraint> {
    let inner = inner.trim();
    if inner.contains(',') {
        let items = inner
            .split(',')
            .map(|it| {
                let it = it.trim();
                match it.split_once(':') {
                    Some((v, l)) => DropdownItem {
                        value: v.trim().to_string(),
                        label: Some(l.trim().to_string()),
                    },
                    None => DropdownItem {
                        value: it.to_string(),
                        label: None,
                    },
                }
            })
            .collect();
        return Some(Constraint::Dropdown(items));
    }
    let parts: Vec<&str> = inner.split(':').collect();
    match parts.as_slice() {
        [min, max] => Some(Constraint::Range {
            min: min.trim().parse().ok()?,
            step: None,
            max: max.trim().parse().ok()?,
        }),
        [min, step, max] => Some(Constraint::Range {
            min: min.trim().parse().ok()?,
            step: Some(step.trim().parse().ok()?),
            max: max.trim().parse().ok()?,
        }),
        [single] => match single.trim().parse::<u64>() {
            Ok(n) => Some(Constraint::MaxLength(n)),
            Err(_) => (!single.trim().is_empty()).then(|| {
                Constraint::Dropdown(vec![DropdownItem {
                    value: single.trim().to_string(),
                    label: None,
                }])
            }),
        },
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "tests: unwrap IS the assertion; the constraint floats are exact literals"
)]
mod tests {
    use super::{Constraint, DropdownItem, customize};

    #[test]
    fn groups_descriptions_and_every_constraint_shape() {
        let src = "\
            /* [Dimensions] */\n\
            width = 10;   // the box width [0:100]\n\
            height = 5;   // [1:0.5:20]\n\
            /* [Style] */\n\
            shape = \"box\"; // pick one [box, sphere, cyl]\n\
            mode = 1;      // [0:Off, 1:On]\n\
            label = \"hi\"; // a name [12]\n\
            plain = 3;\n";
        let c = customize(src).unwrap();
        assert_eq!(c.params.len(), 6);

        let w = &c.params[0];
        assert_eq!(w.name, "width");
        assert_eq!(w.group.as_deref(), Some("Dimensions"));
        assert_eq!(w.description.as_deref(), Some("the box width"));
        assert_eq!(
            w.constraint,
            Some(Constraint::Range {
                min: 0.0,
                step: None,
                max: 100.0
            })
        );

        // description-less, stepped range.
        assert_eq!(c.params[1].description, None);
        assert_eq!(
            c.params[1].constraint,
            Some(Constraint::Range {
                min: 1.0,
                step: Some(0.5),
                max: 20.0
            })
        );

        // new group.
        assert_eq!(c.params[2].group.as_deref(), Some("Style"));
        assert_eq!(
            c.params[2].constraint,
            Some(Constraint::Dropdown(vec![
                DropdownItem {
                    value: "box".into(),
                    label: None
                },
                DropdownItem {
                    value: "sphere".into(),
                    label: None
                },
                DropdownItem {
                    value: "cyl".into(),
                    label: None
                },
            ]))
        );

        // key:label dropdown.
        assert_eq!(
            c.params[3].constraint,
            Some(Constraint::Dropdown(vec![
                DropdownItem {
                    value: "0".into(),
                    label: Some("Off".into())
                },
                DropdownItem {
                    value: "1".into(),
                    label: Some("On".into())
                },
            ]))
        );

        // string maxlength.
        assert_eq!(c.params[4].description.as_deref(), Some("a name"));
        assert_eq!(c.params[4].constraint, Some(Constraint::MaxLength(12)));

        // no annotation at all.
        assert_eq!(c.params[5].name, "plain");
        assert_eq!(c.params[5].group.as_deref(), Some("Style"));
        assert_eq!(c.params[5].description, None);
        assert_eq!(c.params[5].constraint, None);
    }

    #[test]
    fn a_comment_on_the_next_line_is_not_a_trailing_annotation() {
        // The comment is on its own line → not this assignment's trailing comment.
        let c = customize("x = 1;\n// not attached [0:9]\ny = 2;").unwrap();
        assert_eq!(c.params[0].name, "x");
        assert_eq!(c.params[0].description, None);
        assert_eq!(c.params[0].constraint, None);
    }

    #[test]
    fn value_span_points_at_the_value() {
        let src = "size = 42; // [0:100]";
        let c = customize(src).unwrap();
        assert_eq!(&src[c.params[0].value_span.clone()], "42");
    }

    #[test]
    fn malformed_hints_keep_the_description() {
        // Empty brackets / non-numeric range → constraint None, description survives.
        let c = customize(
            "a = 1; // note []\nb = 2; // [x:y]\nc = 3; // just text\nd = 4; // [foo]\ne = 5; // [1:2:3:4]",
        )
        .unwrap();
        assert_eq!(c.params[0].description.as_deref(), Some("note"));
        assert_eq!(c.params[0].constraint, None); // empty brackets
        assert_eq!(c.params[1].constraint, None); // non-numeric range
        assert_eq!(c.params[2].description.as_deref(), Some("just text"));
        assert_eq!(c.params[2].constraint, None); // no brackets
        assert_eq!(c.params[4].constraint, None); // 4+ colon parts → no known shape
        // single non-numeric → a one-item dropdown.
        assert_eq!(
            c.params[3].constraint,
            Some(Constraint::Dropdown(vec![DropdownItem {
                value: "foo".into(),
                label: None
            }]))
        );
    }

    #[test]
    fn editing_a_value_preserves_the_annotation() {
        // H.4.4 lossless-enough: a UI splices a new value into `value_span`; re-customizing the edited
        // source yields the SAME group/description/constraint, just a new value.
        let src = "/* [Size] */\nwidth = 10; // box width [0:100]";
        let c = customize(src).unwrap();
        let p = &c.params[0];

        let mut edited = src.to_string();
        edited.replace_range(p.value_span.clone(), "42");

        let c2 = customize(&edited).unwrap();
        assert_eq!(c2.params[0].group, p.group);
        assert_eq!(c2.params[0].description, p.description);
        assert_eq!(c2.params[0].constraint, p.constraint);
        assert_eq!(&edited[c2.params[0].value_span.clone()], "42");
    }

    #[test]
    fn only_top_level_assignments_and_group_headers_matter() {
        // A block comment that isn't a `[Name]` header is ignored; non-assignment statements skipped.
        let c = customize("/* plain block */\ncube(1);\nx = 5;").unwrap();
        assert_eq!(c.params.len(), 1);
        assert_eq!(c.params[0].name, "x");
        assert_eq!(c.params[0].group, None);
    }
}
