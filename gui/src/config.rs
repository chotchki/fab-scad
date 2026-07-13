//! U.3.14 Phase B — the manifest→GUI inverse bridge. Reads a `project.toml`'s per-part
//! `[[slicing.part]]` config into the live `Part` state on load, the EXACT inverse of the GUI→manifest
//! forward bridge (`fab::to_connectors` / `to_orient` / `cuts_to_spec`), so a save (Phase C) then
//! reload round-trips. A part with loaded cuts makes `kick_auto_plan` stand down, so config wins over
//! auto-derive; a part with no block (or a flat/legacy `[slicing]`) is left to auto-derive.

use crate::*;
use fab_scad::manifest::{Connector, Cut as MCut, Manifest, PartSlicing, PieceOrient};

/// manifest axis string → GUI [`Axis`] (first char; defaults Z, matching the slicer's fallback).
fn parse_axis(s: &str) -> Axis {
    match s.chars().next() {
        Some('x' | 'X') => Axis::X,
        Some('y' | 'Y') => Axis::Y,
        _ => Axis::Z,
    }
}

/// manifest `Cut` → GUI [`CutDef`]. Every loaded cut is ENABLED — a disabled cut is never persisted,
/// so the stack index equals the enabled-cut index a connector's `.cut` references (the reversal of
/// `resolve_conns` is the identity here).
fn cut_to_def(c: &MCut) -> CutDef {
    CutDef {
        axis: parse_axis(&c.axis),
        at: c.at.f() as f32,
        enabled: true,
    }
}

/// manifest screw string → [`Screw`] (default M3, matching the auto-plan seed).
fn parse_screw(s: Option<&str>) -> Screw {
    match s {
        Some("M4") => Screw::M4,
        Some("M5") => Screw::M5,
        _ => Screw::M3,
    }
}

/// manifest `Connector` → GUI [`PlacedConn`], dropping any whose `cut` is out of range for this part's
/// cut stack (a stale hand-edit). `size` defaults to 6.0 (the auto-plan onion default).
fn conn_to_placed(c: &Connector, n_cuts: usize) -> Option<PlacedConn> {
    (c.cut < n_cuts).then(|| PlacedConn {
        cut: c.cut,
        pos: [c.pos[0].f() as f32, c.pos[1].f() as f32],
        size: c.size.unwrap_or(6.0) as f32,
        kind: if c.kind == "bolt" {
            fab::ConnKind::Bolt
        } else {
            fab::ConnKind::Onion
        },
        screw: parse_screw(c.screw.as_deref()),
    })
}

/// manifest per-piece orientations → GUI [`Orient`], keyed by `(slab, comp)` and recorded MANUAL so a
/// re-render keeps them.
fn orient_to_store(orient: &[PieceOrient]) -> Orient {
    let mut o = Orient::default();
    for po in orient {
        o.set_manual(
            (po.piece, po.comp),
            [
                po.up[0].f() as f32,
                po.up[1].f() as f32,
                po.up[2].f() as f32,
            ],
        );
    }
    o
}

/// Load one `[[slicing.part]]` block into a `Part`, overwriting its (derived/empty) cuts/conns/orient.
fn load_into_part(part: &mut Part, ps: &PartSlicing) {
    part.cuts.list = ps.cut.iter().map(cut_to_def).collect();
    part.cuts.active = 0;
    let n = part.cuts.list.len();
    part.conns.list = ps
        .connector
        .iter()
        .filter_map(|c| conn_to_placed(c, n))
        .collect();
    part.orient = orient_to_store(&ps.orient);
}

/// Apply a manifest's per-part slicing config to freshly-built parts, BEFORE auto-derive runs (a part
/// with loaded cuts makes `kick_auto_plan` stand down, so config wins). Each block binds via
/// [`resolve_part`](fab_scad::backend::resolve_part) (name+nth, index fallback); an unresolvable block
/// WARNS + is skipped (best-effort, the reactive standard). A flat/empty `[slicing]` is a no-op —
/// auto-derive handles those parts.
pub(crate) fn apply_slicing_config(parts: &mut [Part], m: &Manifest) {
    let Some(slicing) = &m.slicing else {
        return;
    };
    if slicing.parts.is_empty() {
        return;
    }
    let names: Vec<Option<String>> = parts.iter().map(|p| p.name.clone()).collect();
    for ps in &slicing.parts {
        match fab_scad::backend::resolve_part(&names, &ps.key) {
            Some(i) => load_into_part(&mut parts[i], ps),
            None => warn!("slicing config: no part matches {:?} — skipped", ps.key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_scad::manifest::PartKey;
    use fab_scad::num::Num;

    fn part_named(name: &str) -> Part {
        Part {
            name: Some(name.to_string()),
            ..default()
        }
    }

    #[test]
    fn loads_a_per_part_block_into_the_right_part() {
        let mut parts = vec![part_named("wall"), part_named("frame")];
        let m = Manifest {
            project: fab_scad::manifest::Project {
                name: "x".into(),
                title: None,
            },
            part: vec![],
            publish: None,
            slicing: Some(fab_scad::manifest::Slicing {
                printer: None,
                cut: vec![],
                connector: vec![],
                orient: vec![],
                parts: vec![PartSlicing {
                    key: PartKey {
                        name: Some("frame".into()),
                        nth: 0,
                        index: 1,
                    },
                    cut: vec![MCut {
                        axis: "z".into(),
                        at: Num::Float(40.0),
                    }],
                    connector: vec![Connector {
                        cut: 0,
                        kind: "bolt".into(),
                        screw: Some("M5".into()),
                        pos: [Num::Float(3.0), Num::Float(5.0)],
                        through: None,
                        size: None,
                    }],
                    orient: vec![PieceOrient {
                        piece: [0, 0, 1],
                        comp: 2,
                        up: [Num::Float(0.0), Num::Float(0.0), Num::Float(1.0)],
                    }],
                }],
            }),
        };
        apply_slicing_config(&mut parts, &m);

        // part 0 (wall) had no block → untouched.
        assert!(parts[0].cuts.list.is_empty());
        // part 1 (frame) loaded the block.
        let p = &parts[1];
        assert_eq!(p.cuts.list.len(), 1);
        assert_eq!(p.cuts.list[0].axis, Axis::Z);
        assert_eq!(p.cuts.list[0].at, 40.0);
        assert!(p.cuts.list[0].enabled);
        assert_eq!(p.conns.list.len(), 1);
        assert_eq!(p.conns.list[0].kind, fab::ConnKind::Bolt);
        assert_eq!(p.conns.list[0].screw, Screw::M5);
        assert_eq!(p.orient.map.get(&([0, 0, 1], 2)), Some(&[0.0, 0.0, 1.0]));
        assert!(p.orient.manual.contains(&([0, 0, 1], 2)));
    }

    #[test]
    fn out_of_range_connector_is_dropped() {
        let mut parts = vec![part_named("wall")];
        let m = Manifest {
            project: fab_scad::manifest::Project {
                name: "x".into(),
                title: None,
            },
            part: vec![],
            publish: None,
            slicing: Some(fab_scad::manifest::Slicing {
                printer: None,
                cut: vec![],
                connector: vec![],
                orient: vec![],
                parts: vec![PartSlicing {
                    key: PartKey {
                        name: None,
                        nth: 0,
                        index: 0,
                    },
                    cut: vec![], // no cuts → any connector.cut is out of range
                    connector: vec![Connector {
                        cut: 0,
                        kind: "onion".into(),
                        screw: None,
                        pos: [Num::Float(0.0), Num::Float(0.0)],
                        through: None,
                        size: Some(9.0),
                    }],
                    orient: vec![],
                }],
            }),
        };
        apply_slicing_config(&mut parts, &m);
        assert!(parts[0].conns.list.is_empty()); // cut 0 out of range (0 cuts) → dropped
    }
}
