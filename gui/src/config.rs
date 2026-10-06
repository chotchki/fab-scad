//! The slicing-config persistence bridge (W.3.8). Per-part cuts/connectors/orient round-trip through a
//! trailing `fab:config` comment block in the `.scad` buffer itself — ONE mechanism both platforms share
//! (desktop bakes it on Save, web on download). `slicing_blocks` + `config_block` are the write side;
//! `read_config_block` + `apply_blocks` the read side (the EXACT inverse of the GUI→manifest forward
//! bridge `fab::to_connectors` / `to_orient` / `cuts_to_spec`). A part with loaded cuts makes
//! `kick_auto_plan` stand down, so config wins over auto-derive; a part with no block auto-derives.
//! (This retired the U.3.14 `project.toml`/toml_edit autosave — the block is fs-less, so it works on wasm.)

use crate::*;
use fab_scad::manifest::{Connector, Cut as MCut, PartKey, PartSlicing, PieceOrient};
use fab_scad::num::Num;

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

/// Apply parsed per-part slicing blocks to freshly-built parts, BEFORE auto-derive runs (a part with
/// loaded cuts makes `kick_auto_plan` stand down, so config wins). Each block binds via
/// [`resolve_part`](fab_scad::backend::resolve_part) (name+nth, index fallback); an unresolvable block
/// WARNS + is skipped (best-effort, the reactive standard). Empty blocks → auto-derive handles the parts.
pub(crate) fn apply_blocks(parts: &mut [Part], blocks: &[PartSlicing]) {
    if blocks.is_empty() {
        return;
    }
    let names: Vec<Option<String>> = parts.iter().map(|p| p.name.clone()).collect();
    for ps in blocks {
        match fab_scad::backend::resolve_part(&names, &ps.key) {
            Some(i) => load_into_part(&mut parts[i], ps),
            None => warn!("slicing config: no part matches {:?} — skipped", ps.key),
        }
    }
}

// ── the fab:config block (W.3.8): per-part slicing config as JSON in a .scad line-comment ─────────────
// ONE persistence mechanism, both targets: the browser has no filesystem, so config rides INERT in the
// source (baked into the downloaded .scad); desktop writes the SAME block, retiring the toml_edit
// autosave. A comment is ignored by scad-rs/OpenSCAD, so the model renders identically with or without it.

/// The per-model PRINTER a `fab:config` block declares (W.3.8+, the wasm dogfooding path): the model
/// records the bed it was sliced for, so it round-trips a printer size where there's no `printers.toml`
/// (the browser) — and, once set, wins over `printers.toml` on desktop too. `bed` is [x,y,z] (the plan
/// needs z for tall parts); the Bambu-export plate footprint just tracks the bed's x/y.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct PrinterCfg {
    pub(crate) bed: [f64; 3],
}

/// The whole `fab:config` v2 payload: the optional per-model printer + the per-part slicing blocks. v1 was
/// the bare `[PartSlicing]` array; [`read_config_block`] still reads it (printer `None`) so existing models
/// keep their cuts.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct FabConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) printer: Option<PrinterCfg>,
    pub(crate) parts: Vec<PartSlicing>,
}

/// The v2 marker; the rest of the line is the v2 JSON object. v1 (a bare array) is still READ for
/// back-compat, never written. Versioned so a future schema can migrate.
const CONFIG_MARKER: &str = "// fab:config v2 ";
/// The legacy v1 marker — read-only (existing desktop models' per-part config).
const CONFIG_MARKER_V1: &str = "// fab:config v1 ";

/// The `fab:config` block for the live parts + `printer` — a single line `// fab:config v2 {json}` — or
/// `None` when nothing's worth persisting (no printer set AND every part auto-derives). Appended at the
/// model source's bottom.
pub(crate) fn config_block(parts: &[Part], printer: Option<PrinterCfg>) -> Option<String> {
    let blocks = slicing_blocks(parts);
    if blocks.is_empty() && printer.is_none() {
        return None;
    }
    Some(format!(
        "{CONFIG_MARKER}{}",
        serde_json::to_string(&FabConfig {
            printer,
            parts: blocks,
        })
        .ok()?
    ))
}

/// Read a `fab:config` block out of source `text` — the printer + per-part slicing config, or `None` if
/// absent or malformed (a stray/corrupt block is ignored, never fatal — those parts auto-derive). Reads v2
/// (`{printer, parts}`) OR the legacy v1 bare array (→ printer `None`), whichever the last block is.
///
/// CONTRACT: `text` is ONE .scad's own source — the editing BUFFER (the main/open file), never resolved
/// `include`/`use` content. An include is a reference *line*; the lib's bytes (and any `fab:config` block
/// inside a lib that was itself edited in fab-gui) are NEVER spliced into the buffer, so they can't leak
/// in as this model's config. As belt-and-suspenders we read the LAST block (our own append position at
/// the file's bottom), so a file's own trailing metadata always wins over anything above it.
pub(crate) fn read_config_block(text: &str) -> Option<FabConfig> {
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("// fab:config"))?;
    let line = line.trim_start();
    if let Some(payload) = line.strip_prefix(CONFIG_MARKER) {
        serde_json::from_str(payload.trim()).ok()
    } else {
        // legacy v1: a bare [PartSlicing] array, no printer.
        let payload = line.strip_prefix(CONFIG_MARKER_V1)?;
        serde_json::from_str(payload.trim())
            .ok()
            .map(|parts| FabConfig {
                printer: None,
                parts,
            })
    }
}

/// `text` with any `fab:config` block line removed, plus the ONE blank separator line right above it —
/// the clean model source (what the user edits + what the evaluator sees; the block is re-appended
/// only on persist).
///
/// TG.3: exactly the inverse of what [`reattach_config_block`] / [`with_config_block`] append, and
/// block-less text comes back byte-identical, so hydrate -> flush is a fixpoint. It used to trim EVERY
/// trailing blank line: a file whose model ends in blank lines then re-flushed different from what it
/// was, and merely viewing another file marked a just-saved document unsaved.
pub(crate) fn strip_config_block(text: &str) -> String {
    let is_block = |l: &str| l.trim_start().starts_with("// fab:config");
    if !text.lines().any(is_block) {
        return text.to_string();
    }
    let trailing_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = Vec::new();
    for l in text.lines() {
        if !is_block(l) {
            lines.push(l);
        } else if lines.last().is_some_and(|p| p.trim().is_empty()) {
            lines.pop(); // the separator the append wrote, not the user's
        }
    }
    let mut s = lines.join("\n");
    if trailing_nl && !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Carry `stored`'s `fab:config` block across onto `edited`, which is the config-STRIPPED editor buffer.
///
/// The editor never shows the block, so flushing the buffer straight into the document would delete it —
/// and since SW.3 the document is the ONLY copy until Save (the old shadow file was an accidental
/// backstop). Losing it means a set-entry away and back re-derives an auto slicing plan, and the next
/// Save bakes THAT over the user's tuned cuts. `edited` wins whenever it carries a block of its own, so
/// this can't resurrect one the user deliberately deleted by hand.
pub(crate) fn reattach_config_block(stored: &str, edited: &str) -> String {
    if edited
        .lines()
        .any(|l| l.trim_start().starts_with("// fab:config"))
    {
        return edited.to_string();
    }
    let Some(block) = stored
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("// fab:config"))
    else {
        return edited.to_string();
    };
    let sep = if edited.is_empty() || edited.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{edited}{sep}\n{}\n", block.trim_start())
}

/// Append (or replace) a model's `fab:config` block — strip any existing block, then add the fresh one
/// with a blank-line separator. This is what desktop Save writes to the `.scad` and web download bakes
/// into the bytes. No config to persist → just the clean model.
pub(crate) fn with_config_block(
    model: &str,
    parts: &[Part],
    printer: Option<PrinterCfg>,
) -> String {
    let clean = strip_config_block(model);
    match config_block(parts, printer) {
        Some(block) => {
            let sep = if clean.is_empty() || clean.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{clean}{sep}\n{block}\n")
        }
        None => clean,
    }
}

// ── the persist side: live parts → serialisable blocks ─────────────────────────────────────────────

/// Assemble the per-part slicing blocks from the live parts — the WRITE side of the round-trip. Each
/// non-empty part becomes one block, keyed so [`apply_blocks`] binds it back to the same part
/// (name+nth survives a reorder, index is the fallback). An empty part (no enabled cuts, no
/// connectors, no manual orient) produces NO block — it auto-derives on reload.
pub(crate) fn slicing_blocks(parts: &[Part]) -> Vec<PartSlicing> {
    parts
        .iter()
        .enumerate()
        .filter_map(|(i, part)| {
            let nth = parts[..i].iter().filter(|p| p.name == part.name).count();
            part_to_slicing(part, i, nth)
        })
        .collect()
}

/// One part → one block, or `None` if it carries nothing worth persisting.
fn part_to_slicing(part: &Part, index: usize, nth: usize) -> Option<PartSlicing> {
    // Enabled cuts in stack order, plus the stack→enabled remap the connectors' `cut` field needs: a
    // disabled cut is never persisted, so its stack slot has no enabled index — a connector on it is
    // dropped, exactly as the slicer's `resolve_conns` would.
    let mut cut = Vec::new();
    let mut enabled_of: Vec<Option<usize>> = vec![None; part.cuts.list.len()];
    for (s, c) in part.cuts.list.iter().enumerate() {
        if c.enabled {
            enabled_of[s] = Some(cut.len());
            cut.push(MCut {
                axis: c.axis.scad().to_string(),
                at: Num::Float(f64::from(c.at)),
            });
        }
    }
    let connector: Vec<Connector> = part
        .conns
        .list
        .iter()
        .filter_map(|pc| {
            enabled_of
                .get(pc.cut)
                .copied()
                .flatten()
                .map(|ci| placed_to_connector(pc, ci))
        })
        .collect();
    // Only MANUAL orientations persist (the auto-pick is derived, never stored); sorted for a stable file.
    let mut manual: Vec<PieceKey> = part.orient.manual.iter().copied().collect();
    manual.sort();
    let orient: Vec<PieceOrient> = manual
        .into_iter()
        .filter_map(|k| {
            part.orient.map.get(&k).map(|up| PieceOrient {
                piece: k.0,
                comp: k.1,
                up: [
                    Num::Float(f64::from(up[0])),
                    Num::Float(f64::from(up[1])),
                    Num::Float(f64::from(up[2])),
                ],
            })
        })
        .collect();
    if cut.is_empty() && connector.is_empty() && orient.is_empty() {
        return None;
    }
    Some(PartSlicing {
        key: PartKey {
            name: part.name.clone(),
            nth,
            index,
        },
        cut,
        connector,
        orient,
    })
}

// ── the config fingerprint (TG.1): "would Save bake a different fab:config?" ──────────────────────
// `Parts` has no write chokepoint (cuts, connectors, orient, AutoPlace, Reset-to-auto, the bed all write
// it from different systems), so unsaved-config is a COMPARISON, not a flag: fingerprint exactly what
// Save persists and diff it against a baseline (`DocState`). Hashing the persisted projection means
// what Save drops can't read as a change: a disabled cut, a connector on one, an auto orientation.

/// What Save would bake as `fab:config`, fingerprinted (TG.1). Per part in `Parts` order, plus the
/// bed, which every save writes as the printer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigFp {
    /// [`part_fp`] per part; `None` = the part persists nothing (it auto-derives on reopen).
    pub(crate) parts: Vec<Option<u64>>,
    /// The bed as `f32` bits — exact, and the same `f32`s Save widens to the printer's `f64`s.
    pub(crate) bed: [u32; 3],
}

/// Part `i`'s persisted projection — [`part_to_slicing`]'s block serialized as Save serializes it —
/// hashed. `None` when the part writes no block, or when the block won't serialize (then
/// [`config_block`] writes nothing either, so `None` is still what Save does).
pub(crate) fn part_fp(parts: &[Part], i: usize) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let part = parts.get(i)?;
    let nth = parts[..i].iter().filter(|p| p.name == part.name).count();
    let json = serde_json::to_string(&part_to_slicing(part, i, nth)?).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    Some(h.finish())
}

/// The whole live config's fingerprint: every part's [`part_fp`] plus `bed`.
pub(crate) fn config_fp(parts: &[Part], bed: [f32; 3]) -> ConfigFp {
    ConfigFp {
        parts: (0..parts.len()).map(|i| part_fp(parts, i)).collect(),
        bed: bed.map(f32::to_bits),
    }
}

/// GUI placement → manifest connector (the inverse of [`conn_to_placed`]): an onion carries its
/// diameter, a bolt its screw. `cut` is the ENABLED-cut index the caller computed.
fn placed_to_connector(pc: &PlacedConn, cut: usize) -> Connector {
    let pos = [
        Num::Float(f64::from(pc.pos[0])),
        Num::Float(f64::from(pc.pos[1])),
    ];
    match pc.kind {
        fab::ConnKind::Onion => Connector {
            cut,
            kind: "onion".to_string(),
            screw: None,
            pos,
            through: None,
            size: Some(f64::from(pc.size)),
        },
        fab::ConnKind::Bolt => Connector {
            cut,
            kind: "bolt".to_string(),
            screw: Some(pc.screw.label().to_string()),
            pos,
            through: None,
            size: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `strip` then `reattach` is the flush round-trip (SW.3): the editor shows the clean model, the
    /// document keeps the block. It must survive an edit, and must not fight a block the user typed.
    #[test]
    fn reattach_restores_what_strip_hid_and_yields_to_a_typed_block() {
        let parts = [part_with(Some("p"), &[(Axis::Z, 40.0, true)], &[])];
        let stored = with_config_block("cube(1);\n", &parts, None);
        assert!(stored.contains("// fab:config v2 "));
        let shown = strip_config_block(&stored);
        assert!(!shown.contains("fab:config"), "the editor never sees it");

        // Round-trip: unchanged buffer in, byte-identical file out (so the flush can't dirty it).
        assert_eq!(reattach_config_block(&stored, &shown), stored);
        // An edit keeps the block, and it still parses.
        let edited = reattach_config_block(&stored, "cube(2);\n");
        assert!(edited.starts_with("cube(2);"));
        assert!(read_config_block(&edited).is_some());
        // A block in the incoming text wins — deleting it by hand STAYS deleted.
        let typed = "cube(3);\n// fab:config v2 {\"printer\":null,\"parts\":[]}\n";
        assert_eq!(reattach_config_block(&stored, typed), typed);
        // Nothing to carry across is a no-op.
        assert_eq!(
            reattach_config_block("cube(1);\n", "cube(2);\n"),
            "cube(2);\n"
        );
    }

    fn part_named(name: &str) -> Part {
        Part {
            name: Some(name.to_string()),
            ..default()
        }
    }

    #[test]
    fn loads_a_per_part_block_into_the_right_part() {
        let mut parts = vec![part_named("wall"), part_named("frame")];
        let blocks = vec![PartSlicing {
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
        }];
        apply_blocks(&mut parts, &blocks);

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
        let blocks = vec![PartSlicing {
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
        }];
        apply_blocks(&mut parts, &blocks);
        assert!(parts[0].conns.list.is_empty()); // cut 0 out of range (0 cuts) → dropped
    }

    // ── slicing_blocks (the persist side) ─────────────────────────────────────────────────────────
    fn part_with(
        name: Option<&str>,
        cuts: &[(Axis, f32, bool)],
        conns: &[(usize, fab::ConnKind)],
    ) -> Part {
        let mut p = Part {
            name: name.map(String::from),
            ..default()
        };
        for &(axis, at, enabled) in cuts {
            p.cuts.list.push(CutDef { axis, at, enabled });
        }
        for &(cut, kind) in conns {
            p.conns.list.push(PlacedConn {
                cut,
                pos: [1.0, 2.0],
                size: 6.0,
                kind,
                screw: Screw::M4,
            });
        }
        p
    }

    #[test]
    fn slicing_blocks_skips_empty_parts_and_counts_nth() {
        let parts = vec![
            part_with(
                Some("wall"),
                &[(Axis::Z, 40.0, true)],
                &[(0, fab::ConnKind::Onion)],
            ),
            part_with(Some("wall"), &[], &[]), // empty → no block, but still counts for nth
            part_with(Some("wall"), &[(Axis::X, 10.0, true)], &[]),
        ];
        let blocks = slicing_blocks(&parts);
        assert_eq!(blocks.len(), 2); // the empty wall produced no block
        assert_eq!((blocks[0].key.index, blocks[0].key.nth), (0, 0));
        assert_eq!((blocks[1].key.index, blocks[1].key.nth), (2, 2)); // 3rd wall → nth 2, matches resolve_part
        assert_eq!(blocks[0].cut[0].axis, "z");
        assert_eq!(blocks[0].connector[0].kind, "onion");
        assert_eq!(blocks[0].connector[0].size, Some(6.0));
    }

    #[test]
    fn disabled_cut_drops_its_connector_and_reindexes() {
        // stack cuts [enabled, DISABLED, enabled]; a connector on stack-cut 2 → enabled index 1.
        let mut p = part_with(
            Some("p"),
            &[
                (Axis::Z, 10.0, true),
                (Axis::Z, 20.0, false),
                (Axis::Z, 30.0, true),
            ],
            &[(2, fab::ConnKind::Bolt), (1, fab::ConnKind::Onion)],
        );
        p.conns.list[0].screw = Screw::M5;
        let blocks = slicing_blocks(&[p]);
        assert_eq!(blocks[0].cut.len(), 2); // only the two enabled cuts persist
        assert_eq!(blocks[0].connector.len(), 1); // the conn on the disabled cut is dropped
        assert_eq!(blocks[0].connector[0].cut, 1); // stack idx 2 → enabled idx 1
        assert_eq!(blocks[0].connector[0].screw.as_deref(), Some("M5"));
    }

    // ── the fab:config block (W.3.8) ───────────────────────────────────────────────────────────────
    #[test]
    fn fab_config_block_round_trips_through_the_scad_buffer() {
        // parts → block in the .scad → read back → apply → same config. The ONE persistence path, both
        // platforms (the browser bakes this block into the downloaded .scad; desktop writes the same).
        let parts = vec![
            part_with(
                Some("wall"),
                &[(Axis::Z, 40.0, true)],
                &[(0, fab::ConnKind::Bolt)],
            ),
            part_with(Some("frame"), &[(Axis::X, 10.0, true)], &[]),
        ];
        let model = "cube([60,40,30]);\n";
        let saved = with_config_block(model, &parts, None);
        assert!(
            saved.starts_with(model),
            "model source untouched above the block"
        );
        assert!(saved.contains("// fab:config v2 "));

        let cfg = read_config_block(&saved).expect("block reads back");
        let mut fresh = vec![part_named("wall"), part_named("frame")];
        apply_blocks(&mut fresh, &cfg.parts);
        assert_eq!(fresh[0].cuts.list[0].at, 40.0);
        assert_eq!(fresh[0].conns.list[0].kind, fab::ConnKind::Bolt);
        assert_eq!(fresh[1].cuts.list[0].axis, Axis::X);

        assert_eq!(
            strip_config_block(&saved),
            model,
            "strip gives back the clean model"
        );
    }

    /// TG.3: strip is the exact inverse of the append, so a buffer that ends in blank lines survives
    /// save -> hydrate -> flush byte-identical, and a block-less file is never touched (a view of it
    /// can't dirty it).
    #[test]
    fn strip_then_reattach_is_a_fixpoint_for_trailing_blank_lines() {
        let parts = [part_with(Some("p"), &[(Axis::Z, 40.0, true)], &[])];
        for buf in [
            "cube(1);",
            "cube(1);\n",
            "cube(1);\n\n\n",
            "cube(1);\n  \n",
            "",
        ] {
            let saved = with_config_block(buf, &parts, None);
            let shown = strip_config_block(&saved);
            assert_eq!(
                reattach_config_block(&saved, &shown),
                saved,
                "{buf:?}: the hydrated buffer re-flushes to the saved bytes"
            );
            assert_eq!(
                with_config_block(&shown, &parts, None),
                saved,
                "{buf:?}: re-saving changes nothing"
            );
        }
        for raw in ["cube(1);\n\n\n", "cube(1);\r\n", "cube(1);"] {
            assert_eq!(strip_config_block(raw), raw, "no block, no change");
        }
    }

    #[test]
    fn an_included_libs_config_block_does_not_leak_in() {
        // chotchki's flag: a model that INCLUDEs a lib must not pick up the lib's fab:config. The
        // include is a reference LINE — the lib's content (block and all) is never in the main buffer,
        // so a model with only an include line + no own block reads NONE (auto-derives).
        let model = "include <widget.scad>\nuse <helpers.scad>\nwidget();\n";
        assert!(read_config_block(model).is_none());

        // And if a lib's block ever DID appear above the model's OWN (a hypothetical splice), the file's
        // own trailing block still wins — we read the bottom-most, our append position.
        let lib_block = config_block(
            &[part_with(Some("lib"), &[(Axis::Z, 9.0, true)], &[])],
            None,
        )
        .unwrap();
        let own_block =
            config_block(&[part_with(Some("me"), &[(Axis::X, 3.0, true)], &[])], None).unwrap();
        let spliced = format!("{lib_block}\nwidget();\n{own_block}\n");
        assert_eq!(
            read_config_block(&spliced).unwrap().parts[0].cut[0].axis,
            "x"
        );
    }

    #[test]
    fn no_config_writes_no_block_and_reads_none() {
        let parts = vec![part_named("wall")]; // auto-derive → nothing to persist
        assert!(config_block(&parts, None).is_none());
        let model = "cube(1);\n";
        assert_eq!(with_config_block(model, &parts, None), model);
        assert!(read_config_block(model).is_none());
    }

    #[test]
    fn resaving_replaces_the_block_never_stacks_it() {
        let a = vec![part_with(Some("p"), &[(Axis::Z, 40.0, true)], &[])];
        let b = vec![part_with(Some("p"), &[(Axis::Z, 41.0, true)], &[])];
        let once = with_config_block("cube(1);\n", &a, None);
        let twice = with_config_block(&once, &b, None); // re-save with moved cut
        assert_eq!(
            twice.matches("// fab:config").count(),
            1,
            "one block, not two"
        );
        assert_eq!(
            read_config_block(&twice).unwrap().parts[0].cut[0].at.f(),
            41.0
        );
    }

    // ── the config fingerprint (TG.1) ─────────────────────────────────────────────────────────────
    /// What Save drops can't read as a change: a disabled cut (and a connector on it), and an AUTO
    /// orientation. A manual orientation, an enabled cut, and the bed all do.
    #[test]
    fn fingerprint_tracks_only_what_save_persists() {
        let bed = [256.0; 3];
        let base = vec![part_with(
            Some("p"),
            &[(Axis::Z, 40.0, true)],
            &[(0, fab::ConnKind::Onion)],
        )];
        let fp = config_fp(&base, bed);
        assert!(fp.parts[0].is_some());

        let mut p = vec![part_with(
            Some("p"),
            &[(Axis::Z, 40.0, true), (Axis::X, 9.0, false)],
            &[(0, fab::ConnKind::Onion), (1, fab::ConnKind::Bolt)],
        )];
        assert_eq!(
            config_fp(&p, bed),
            fp,
            "a disabled cut + its connector persist nothing"
        );
        p[0].orient.map.insert(([0, 0, 0], 0), [0.0, 0.0, 1.0]); // the auto-pick seeds `map` only
        assert_eq!(
            config_fp(&p, bed),
            fp,
            "an auto orientation persists nothing"
        );

        p[0].orient.set_manual(([0, 0, 0], 0), [0.0, 0.0, 1.0]);
        assert_ne!(config_fp(&p, bed), fp, "a manual orientation persists");
        p[0].orient.reset(([0, 0, 0], 0));
        assert_eq!(config_fp(&p, bed), fp);
        p[0].cuts.list[1].enabled = true;
        assert_ne!(config_fp(&p, bed), fp, "enabling the cut persists it");
        // A connector placed on an enabled cut persists, and so does moving one.
        let fresh = || {
            vec![part_with(
                Some("p"),
                &[(Axis::Z, 40.0, true)],
                &[(0, fab::ConnKind::Onion)],
            )]
        };
        let mut placed = fresh();
        let mut conn = placed[0].conns.list[0];
        conn.pos = [3.0, 4.0];
        placed[0].conns.list.push(conn);
        assert_ne!(
            config_fp(&placed, bed),
            fp,
            "placing a connector persists it"
        );
        let mut moved = fresh();
        moved[0].conns.list[0].pos[0] += 1.0;
        assert_ne!(config_fp(&moved, bed), fp, "moving a connector persists it");
        assert_ne!(
            config_fp(&base, [256.0, 256.0, 300.0]),
            fp,
            "the bed is in it"
        );

        // A part with nothing to persist is `None` — the "no saved plan" an auto-plan may adopt.
        assert_eq!(config_fp(&[part_named("q")], bed).parts, vec![None]);
    }

    /// The whole Save → reopen loop, fingerprinted: a plan + bed baked into fab:config, read back and
    /// applied to freshly built parts the way `poll_job` does, fingerprints identically. So a reopened
    /// saved plan reads clean, f32 → f64 → JSON → f32 included.
    #[test]
    fn a_saved_plan_reapplied_fingerprints_the_same() {
        let mut live = vec![
            part_with(
                Some("wall"),
                &[(Axis::Z, 40.3, true), (Axis::Y, 1.0 / 3.0, true)],
                &[(1, fab::ConnKind::Bolt)],
            ),
            part_with(Some("wall"), &[], &[]), // auto-derives: no block
            part_with(
                Some("frame"),
                &[(Axis::X, -12.7, true)],
                &[(0, fab::ConnKind::Onion)],
            ),
        ];
        live[0].conns.list[0].pos = [0.1, 7.77];
        live[2]
            .orient
            .set_manual(([1, 0, 0], 0), [0.0, 0.70710677, 0.70710677]);
        let bed = [325.3, 320.0, 299.9];
        let saved = with_config_block(
            "cube(1);\n",
            &live,
            Some(PrinterCfg {
                bed: bed.map(f64::from),
            }),
        );

        let cfg = read_config_block(&saved).expect("reads back");
        let mut fresh = vec![part_named("wall"), part_named("wall"), part_named("frame")];
        apply_blocks(&mut fresh, &cfg.parts);
        let p = cfg.printer.expect("printer saved").bed;
        let reopened_bed = [p[0] as f32, p[1] as f32, p[2] as f32];
        assert_eq!(config_fp(&fresh, reopened_bed), config_fp(&live, bed));
    }

    #[test]
    fn printer_round_trips_and_a_v1_block_still_reads() {
        // v2: the printer rides in the block alongside the parts, and survives a save without cuts.
        let printer = Some(PrinterCfg {
            bed: [325.0, 320.0, 320.0],
        });
        let saved = with_config_block("cube(1);\n", &[part_named("p")], printer);
        assert!(saved.contains("// fab:config v2 "));
        let cfg = read_config_block(&saved).expect("reads back");
        assert_eq!(cfg.printer, printer);
        assert!(cfg.parts.is_empty()); // no cuts, but the block still exists FOR the printer

        // a printer with no parts still persists (the whole point — the model declares its bed).
        assert!(config_block(&[part_named("p")], printer).is_some());

        // back-compat: a legacy v1 bare-array block reads as parts with printer None.
        let v1 = "cube(1);\n// fab:config v1 [{\"key\":{\"name\":\"p\",\"nth\":0,\"index\":0},\"cut\":[{\"axis\":\"z\",\"at\":5.0}],\"connector\":[],\"orient\":[]}]\n";
        let cfg = read_config_block(v1).expect("v1 reads");
        assert!(cfg.printer.is_none());
        assert_eq!(cfg.parts[0].cut[0].axis, "z");
    }
}
