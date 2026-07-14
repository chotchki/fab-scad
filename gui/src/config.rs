//! U.3.14 Phase B — the manifest→GUI inverse bridge. Reads a `project.toml`'s per-part
//! `[[slicing.part]]` config into the live `Part` state on load, the EXACT inverse of the GUI→manifest
//! forward bridge (`fab::to_connectors` / `to_orient` / `cuts_to_spec`), so a save (Phase C) then
//! reload round-trips. A part with loaded cuts makes `kick_auto_plan` stand down, so config wins over
//! auto-derive; a part with no block (or a flat/legacy `[slicing]`) is left to auto-derive.

// Path + anyhow are used only by the native fs autosave (`save_slicing_config`); the load/hash bridge
// below is pure and wasm-safe.
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::{Context, Result};

use crate::*;
use fab_scad::manifest::{Connector, Cut as MCut, Manifest, PartKey, PartSlicing, PieceOrient};
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

/// Apply a manifest's per-part slicing config to freshly-built parts, BEFORE auto-derive runs (a part
/// with loaded cuts makes `kick_auto_plan` stand down, so config wins). Each block binds via
/// [`resolve_part`](fab_scad::backend::resolve_part) (name+nth, index fallback); an unresolvable block
/// WARNS + is skipped (best-effort, the reactive standard). A flat/empty `[slicing]` is a no-op —
/// auto-derive handles those parts.
pub(crate) fn apply_slicing_config(parts: &mut [Part], m: &Manifest) {
    if let Some(slicing) = &m.slicing {
        apply_blocks(parts, &slicing.parts);
    }
}

/// Apply parsed per-part slicing blocks to freshly-built parts — the shared core of the legacy
/// `project.toml` load and the [`fab:config`](read_config_block) block load. Each block binds via
/// `resolve_part` (name+nth, index fallback); an unresolvable block WARNS + is skipped (best-effort).
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

/// The marker prefix; the rest of the line is the config JSON. Versioned so a future schema can migrate.
const CONFIG_MARKER: &str = "// fab:config v1 ";

/// The `fab:config` block for the live parts — a single line `// fab:config v1 {json}` — or `None` when
/// nothing's worth persisting (every part auto-derives). Appended to a model's source at the bottom.
pub(crate) fn config_block(parts: &[Part]) -> Option<String> {
    let blocks = slicing_blocks(parts);
    if blocks.is_empty() {
        return None;
    }
    Some(format!("{CONFIG_MARKER}{}", serde_json::to_string(&blocks).ok()?))
}

/// Read a `fab:config` block out of source `text` — the per-part slicing config, or `None` if absent or
/// malformed (a stray/corrupt block is ignored, never fatal — those parts auto-derive).
pub(crate) fn read_config_block(text: &str) -> Option<Vec<PartSlicing>> {
    let payload = text
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(CONFIG_MARKER))?;
    serde_json::from_str(payload.trim()).ok()
}

/// `text` with any `fab:config` block line removed + trailing blank lines trimmed — the clean model
/// source (what the user edits + what the evaluator sees; the block is re-appended only on persist).
pub(crate) fn strip_config_block(text: &str) -> String {
    let trailing_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("// fab:config"))
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut s = lines.join("\n");
    if trailing_nl && !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Append (or replace) a model's `fab:config` block — strip any existing block, then add the fresh one
/// with a blank-line separator. This is what desktop Save writes to the `.scad` and web download bakes
/// into the bytes. No config to persist → just the clean model.
pub(crate) fn with_config_block(model: &str, parts: &[Part]) -> String {
    let clean = strip_config_block(model);
    match config_block(parts) {
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

// ── save (Phase C): GUI → project.toml ───────────────────────────────────────────────────────────

/// Assemble the per-part slicing blocks from the live parts — the WRITE side of the round-trip. Each
/// non-empty part becomes one `[[slicing.part]]`, keyed so [`apply_slicing_config`] binds it back to
/// the same part (name+nth survives a reorder, index is the fallback). An empty part (no enabled cuts,
/// no connectors, no manual orient) produces NO block — it auto-derives on reload.
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

/// A content hash of EXACTLY the persisted slicing config — what [`slicing_blocks`] would write,
/// quantised like the slice-hash so float jitter never churns it. The autosave baseline keys on this:
/// a bare open (live config == what's on disk) never writes; an edit moves it and triggers one save.
pub(crate) fn config_hash(parts: &[Part]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |x: f64| (x * 1000.0).round() as i64;
    for ps in slicing_blocks(parts) {
        ps.key.name.hash(&mut h);
        ps.key.nth.hash(&mut h);
        ps.key.index.hash(&mut h);
        for c in &ps.cut {
            c.axis.hash(&mut h);
            q(c.at.f()).hash(&mut h);
        }
        for c in &ps.connector {
            c.cut.hash(&mut h);
            c.kind.hash(&mut h);
            c.screw.hash(&mut h);
            q(c.pos[0].f()).hash(&mut h);
            q(c.pos[1].f()).hash(&mut h);
            c.size.map(q).hash(&mut h);
        }
        for o in &ps.orient {
            o.piece.hash(&mut h);
            o.comp.hash(&mut h);
            o.up.iter().for_each(|u| q(u.f()).hash(&mut h));
        }
    }
    h.finish()
}

/// Write the live per-part config into `source`'s nearest `project.toml`, PRESERVING the rest of the
/// file — toml_edit keeps [project]/[[part]]/comments byte-for-byte, only the `[slicing]` table is
/// rebuilt. Migrate-on-save: just `[[slicing.part]]` blocks are written, the flat `[slicing]`
/// cut/connector/orient are dropped (skip-if-empty on the struct), so the output never mixes the two
/// (the manifest's flat-XOR-per-part rule). No project.toml above `source` → no-op (a loose `.scad`
/// has nowhere to persist).
// Native-only: toml_edit + fs autosave to project.toml. On wasm there's no filesystem — config
// persistence rides a `fab:config` block in the .scad buffer instead (W.3.8).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_slicing_config(parts: &[Part], source: &Path) -> Result<()> {
    let Ok(path) = Manifest::find(source) else {
        return Ok(()); // no project.toml → nothing to persist
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;

    // Get-or-create the [slicing] table, PRESERVING an existing printer (Phase E owns it — model state,
    // not part state). Drop the flat cut/connector/orient (migrate-on-save → per-part only, the XOR).
    if !doc.contains_key("slicing") {
        doc["slicing"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let slicing = doc["slicing"]
        .as_table_mut()
        .context("`slicing` in project.toml is not a table")?;
    slicing.remove("cut");
    slicing.remove("connector");
    slicing.remove("orient");

    // One `[[slicing.part]]` header block per part (readable for hand-edits), nested cut/connector as
    // inline arrays. serde renders a Vec as an inline array, so build the array-of-tables ourselves.
    let mut arr = toml_edit::ArrayOfTables::new();
    for ps in slicing_blocks(parts) {
        let block = toml_edit::ser::to_document(&ps).context("serialising part")?;
        arr.push(block.as_table().clone());
    }
    slicing["part"] = toml_edit::Item::ArrayOfTables(arr);

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── save (Phase C) ───────────────────────────────────────────────────────────────────────────
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

    #[test]
    fn config_hash_reacts_to_a_cut_move() {
        let a = vec![part_with(Some("p"), &[(Axis::Z, 40.0, true)], &[])];
        let b = vec![part_with(Some("p"), &[(Axis::Z, 41.0, true)], &[])];
        assert_ne!(config_hash(&a), config_hash(&b));
        assert_eq!(config_hash(&a), config_hash(&a)); // stable across calls
    }

    #[test]
    fn save_writes_per_part_blocks_and_preserves_the_rest() {
        let dir = std::env::temp_dir().join("fab_gui_cfg_save_rt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let toml_path = dir.join("project.toml");
        std::fs::write(
            &toml_path,
            "[project]\nname = \"demo\" # keep me\n\n[[part]]\nsrc = \"m.scad\"\n",
        )
        .unwrap();
        let src = dir.join("m.scad");
        std::fs::write(&src, "cube(1);").unwrap();

        let parts = vec![part_with(
            Some("wall"),
            &[(Axis::Z, 40.0, true)],
            &[(0, fab::ConnKind::Bolt)],
        )];
        save_slicing_config(&parts, &src).unwrap();

        let written = std::fs::read_to_string(&toml_path).unwrap();
        assert!(
            written.contains("# keep me"),
            "preserves the project comment"
        );
        assert!(written.contains("[[slicing.part]]"));

        let s = Manifest::load(&toml_path).unwrap().slicing.unwrap();
        assert!(s.cut.is_empty()); // no flat cut — per-part only
        assert_eq!(s.parts.len(), 1);
        assert_eq!(s.parts[0].cut[0].axis, "z");
        assert_eq!(s.parts[0].connector[0].kind, "bolt");
        assert_eq!(s.parts[0].key.name.as_deref(), Some("wall"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_migrates_flat_slicing_to_per_part() {
        let dir = std::env::temp_dir().join("fab_gui_cfg_save_migrate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let toml_path = dir.join("project.toml");
        std::fs::write(
            &toml_path,
            "[project]\nname = \"demo\"\n\n[slicing]\ncut = [ { axis = \"x\", at = 0.0 } ]\n",
        )
        .unwrap();
        let src = dir.join("m.scad");
        std::fs::write(&src, "cube(1);").unwrap();

        let parts = vec![part_with(Some("wall"), &[(Axis::Y, 12.0, true)], &[])];
        save_slicing_config(&parts, &src).unwrap();

        let s = Manifest::load(&toml_path).unwrap().slicing.unwrap();
        assert!(s.cut.is_empty(), "flat cut stripped on migrate");
        assert_eq!(s.parts.len(), 1);
        assert_eq!(s.parts[0].cut[0].axis, "y");
        std::fs::remove_dir_all(&dir).ok();
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
        let saved = with_config_block(model, &parts);
        assert!(saved.starts_with(model), "model source untouched above the block");
        assert!(saved.contains("// fab:config v1 "));

        let blocks = read_config_block(&saved).expect("block reads back");
        let mut fresh = vec![part_named("wall"), part_named("frame")];
        apply_blocks(&mut fresh, &blocks);
        assert_eq!(fresh[0].cuts.list[0].at, 40.0);
        assert_eq!(fresh[0].conns.list[0].kind, fab::ConnKind::Bolt);
        assert_eq!(fresh[1].cuts.list[0].axis, Axis::X);

        assert_eq!(strip_config_block(&saved), model, "strip gives back the clean model");
    }

    #[test]
    fn no_config_writes_no_block_and_reads_none() {
        let parts = vec![part_named("wall")]; // auto-derive → nothing to persist
        assert!(config_block(&parts).is_none());
        let model = "cube(1);\n";
        assert_eq!(with_config_block(model, &parts), model);
        assert!(read_config_block(model).is_none());
    }

    #[test]
    fn resaving_replaces_the_block_never_stacks_it() {
        let a = vec![part_with(Some("p"), &[(Axis::Z, 40.0, true)], &[])];
        let b = vec![part_with(Some("p"), &[(Axis::Z, 41.0, true)], &[])];
        let once = with_config_block("cube(1);\n", &a);
        let twice = with_config_block(&once, &b); // re-save with moved cut
        assert_eq!(
            twice.matches("// fab:config").count(),
            1,
            "one block, not two"
        );
        assert_eq!(read_config_block(&twice).unwrap()[0].cut[0].at.f(), 41.0);
    }
}
