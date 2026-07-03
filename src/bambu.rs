//! Bambu Studio / OrcaSlicer multi-plate project `.3mf` writer (Track C follow-on, Phase 12).
//!
//! A Bambu project `.3mf` is an OPC zip, and two facts (verified against the BambuStudio source —
//! see the research notes) make or break it:
//!
//!   1. **The recognition gate is one string.** `3D/3dmodel.model` MUST carry
//!      `<metadata name="Application">` whose value starts with `BambuStudio-`. Miss it and the
//!      importer opens the file as plain geometry and silently DROPS every plate + setting — you get
//!      one un-plated blob. That string is the whole difference between a project and an import.
//!   2. **Plate membership is POSITIONAL, not declared.** BambuStudio bins each piece to a plate by
//!      bounding-box intersection with that plate's world rectangle — it does NOT trust the
//!      `<model_instance>` list to place pieces. So each piece is physically placed at its plate's
//!      cell in Bambu's global grid (stride = bed * 1.2, rows descend into -Y, plate 0 at the world
//!      origin). We still write the `<model_instance>` records (the importer needs them for plate
//!      count + bookkeeping) and keep them consistent with the geometry.
//!
//! Minimal viable project = 4 zip entries: `[Content_Types].xml`, `_rels/.rels`, `3D/3dmodel.model`,
//! `Metadata/model_settings.config`. No gcode, no thumbnails — Bambu re-slices on open.

use std::io::{Seek, Write};
use std::path::Path;

use anyhow::{ensure, Context, Result};

use crate::geom;
use crate::pack::{self, Footprint};

/// An indexed triangle mesh: vertices and 0-based triangle indices into them.
pub struct Mesh {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
}

/// A piece placed on a plate. Its `mesh` is authored in the plate's LOCAL frame: oriented to its
/// print build-up (+Z up), seated on the bed (min z = 0), and shifted so its XY footprint's
/// min-corner is the local origin. `at` is where that min-corner sits within the plate's bed (mm
/// from the bed's front-left corner) — the packer's output. The writer adds the plate's grid origin.
pub struct Placed {
    pub mesh: Mesh,
    pub at: [f64; 2],
}

/// A pure-translation 3mf transform: 12 values, column-major 4×3, translation is the last three.
fn translate_xf(x: f64, y: f64, z: f64) -> String {
    format!("1 0 0 0 1 0 0 0 1 {x} {y} {z}")
}

/// BambuStudio's near-square plate-grid column count (mirrors `compute_colum_count`): `round(sqrt n)`,
/// bumped up one when `sqrt n` rounds down. 1→1, 2→2, 3→2, 5→3, 10→4.
fn column_count(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let v = (n as f64).sqrt();
    let r = v.round();
    if v > r {
        r as usize + 1
    } else {
        r as usize
    }
}

/// World-space min-corner of plate `p` in Bambu's global grid. Plates tile a near-square grid in ONE
/// shared coordinate space with a 20% gap (stride = bed * 1.2); rows descend into -Y; plate 0's
/// corner is the world origin.
fn plate_origin(p: usize, cols: usize, bed: [f64; 2]) -> [f64; 2] {
    let (col, row) = (p % cols, p / cols);
    [col as f64 * bed[0] * 1.2, -(row as f64) * bed[1] * 1.2]
}

/// Write `plates` as a Bambu multi-plate project `.3mf` at `path`. Each element is one plate; its
/// pieces are positioned within that plate's bed by their `at`. `bed` is the printer bed `[x, y]` in
/// mm (e.g. `[256.0, 256.0]` for an X1C) — it sets both the plate-grid stride AND the coordinate
/// frame Bambu bins pieces against, so it MUST match the printer the project opens on.
pub fn write_project(path: &Path, plates: &[Vec<Placed>], bed: [f64; 2]) -> Result<()> {
    let file =
        std::fs::File::create(path).with_context(|| format!("creating 3mf {}", path.display()))?;
    write_project_to(file, plates, bed)
}

/// The writer-generic twin of [`write_project`] — the browser build streams the project into a
/// `Cursor<Vec<u8>>` and hands the bytes to a download, no filesystem anywhere.
pub fn write_project_to<W: Write + Seek>(
    out: W,
    plates: &[Vec<Placed>],
    bed: [f64; 2],
) -> Result<()> {
    let cols = column_count(plates.len());

    // 3D/3dmodel.model — two-level objects (mesh + wrapper) and one build item per piece. Only the
    // `Application` (gate) + `3mfVersion` metadata are load-bearing.
    let mut model = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" xmlns:BambuStudio=\"http://schemas.bambulab.com/package/2021\">\n\
 <metadata name=\"Application\">BambuStudio-02.00.00.00</metadata>\n\
 <metadata name=\"BambuStudio:3mfVersion\">1</metadata>\n\
 <resources>\n",
    );
    let mut items = String::new(); // <build> items
    let mut config_objects = String::new(); // model_settings <object> entries
    let mut plate_blocks = String::new(); // model_settings <plate> entries

    let mut gi = 0usize; // global piece index, 0-based (object ids are 1-based, so 2*gi+1 / +2)
    for (pi, plate) in plates.iter().enumerate() {
        let [ox, oy] = plate_origin(pi, cols, bed);
        let mut instances = String::new();
        for placed in plate {
            let (mesh_id, wrap_id) = (2 * gi + 1, 2 * gi + 2);

            // Mesh object.
            model.push_str(&format!(
                "  <object id=\"{mesh_id}\" type=\"model\">\n   <mesh>\n    <vertices>\n"
            ));
            for v in &placed.mesh.verts {
                model.push_str(&format!(
                    "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n",
                    v[0], v[1], v[2]
                ));
            }
            model.push_str("    </vertices>\n    <triangles>\n");
            for t in &placed.mesh.tris {
                model.push_str(&format!(
                    "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n",
                    t[0], t[1], t[2]
                ));
            }
            model.push_str("    </triangles>\n   </mesh>\n  </object>\n");

            // Wrapper object (identity component); the build item carries the world placement.
            model.push_str(&format!(
                "  <object id=\"{wrap_id}\" type=\"model\">\n   <components>\n    <component objectid=\"{mesh_id}\" transform=\"{}\"/>\n   </components>\n  </object>\n",
                translate_xf(0.0, 0.0, 0.0)
            ));

            // Build item: world placement = plate origin + local footprint offset, seated on bed.
            let (wx, wy) = (ox + placed.at[0], oy + placed.at[1]);
            items.push_str(&format!(
                "  <item objectid=\"{wrap_id}\" transform=\"{}\" printable=\"1\"/>\n",
                translate_xf(wx, wy, 0.0)
            ));

            // model_settings: the object entry + this plate's binding instance.
            config_objects.push_str(&format!(
                "  <object id=\"{wrap_id}\">\n   <metadata key=\"name\" value=\"piece_{gi}\"/>\n   <metadata key=\"extruder\" value=\"1\"/>\n  </object>\n"
            ));
            instances.push_str(&format!(
                "    <model_instance>\n     <metadata key=\"object_id\" value=\"{wrap_id}\"/>\n     <metadata key=\"instance_id\" value=\"0\"/>\n     <metadata key=\"identify_id\" value=\"{}\"/>\n    </model_instance>\n",
                100 + gi
            ));
            gi += 1;
        }
        plate_blocks.push_str(&format!(
            "  <plate>\n   <metadata key=\"plater_id\" value=\"{}\"/>\n   <metadata key=\"plater_name\" value=\"\"/>\n   <metadata key=\"locked\" value=\"false\"/>\n{instances}  </plate>\n",
            pi + 1
        ));
    }
    model.push_str(" </resources>\n <build>\n");
    model.push_str(&items);
    model.push_str(" </build>\n</model>\n");

    let mut settings = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n");
    settings.push_str(&config_objects);
    settings.push_str(&plate_blocks);
    settings.push_str("</config>\n");

    // Static OPC entries (verbatim from the spec; the .rels needs only the model relationship).
    const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\
 <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\
 <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\
 <Default Extension=\"png\" ContentType=\"image/png\"/>\n\
 <Default Extension=\"gcode\" ContentType=\"text/x.gcode\"/>\n\
</Types>\n";
    const RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\
 <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n\
</Relationships>\n";

    let mut zip = zip::ZipWriter::new(out);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("3D/3dmodel.model", model.as_str()),
        ("Metadata/model_settings.config", settings.as_str()),
    ] {
        zip.start_file(name, opts)
            .with_context(|| format!("zip entry {name}"))?;
        zip.write_all(body.as_bytes())
            .with_context(|| format!("writing {name}"))?;
    }
    zip.finish().context("finalizing 3mf zip")?;
    Ok(())
}

// --- orchestration: orient → seat → pack → place → write ----------------------------------------

/// A piece to lay out for print: its mesh in MODEL space (as it sits in the assembled part) and the
/// build-up direction it prints along (unit, model space — `auto_orient::best_up`). `export_plates`
/// rotates it so the build-up is +Z, seats it on the bed, packs it, and writes the project.
pub struct PieceToPlace {
    pub mesh: Mesh,
    pub up: [f64; 3],
}

/// What an export produced — for a status read-out.
pub struct ExportSummary {
    pub plates: usize,
    pub pieces: usize,
    pub fill: f64,
}

/// Lay out `pieces` and write a Bambu multi-plate project to `path`: orient each to its build-up
/// (+Z), seat it on the bed (min z = 0), pack the footprints onto the fewest `bed` = `[x, y]` mm
/// plates (leaving `gap` mm between pieces), and emit the `.3mf`. Mesh-based end to end — no `Solid`,
/// so it runs happily on a worker thread. Errors if a piece can't fit the bed.
pub fn export_plates(
    path: &Path,
    pieces: Vec<PieceToPlace>,
    bed: [f64; 2],
    gap: f64,
) -> Result<ExportSummary> {
    let file =
        std::fs::File::create(path).with_context(|| format!("creating 3mf {}", path.display()))?;
    export_plates_to(file, pieces, bed, gap)
}

/// The writer-generic twin of [`export_plates`] — same layout/pack/emit, any `Write + Seek` sink.
pub fn export_plates_to<W: Write + Seek>(
    out: W,
    pieces: Vec<PieceToPlace>,
    bed: [f64; 2],
    gap: f64,
) -> Result<ExportSummary> {
    ensure!(!pieces.is_empty(), "no pieces to export");

    // Orient (build-up → +Z) + seat (footprint min-corner to the origin); collect footprints.
    let mut oriented: Vec<Mesh> = Vec::with_capacity(pieces.len());
    let mut foots: Vec<Footprint> = Vec::with_capacity(pieces.len());
    for p in &pieces {
        let r = rot_up_to_z(p.up);
        let mut verts: Vec<[f64; 3]> = p.mesh.verts.iter().map(|&v| matvec(r, v)).collect();
        let (min, max) = bbox(&verts).context("piece has no vertices")?;
        for v in &mut verts {
            for k in 0..3 {
                v[k] -= min[k];
            }
        }
        oriented.push(Mesh {
            verts,
            tris: p.mesh.tris.clone(),
        });
        foots.push(Footprint {
            w: max[0] - min[0],
            h: max[1] - min[1],
        });
    }

    let placements = pack::pack(&foots, bed, gap)?;
    let plate_n = pack::plate_count(&placements);
    let fill = pack::fill_ratio(&foots, &placements, bed);

    let mut plates: Vec<Vec<Placed>> = (0..plate_n).map(|_| Vec::new()).collect();
    for (i, mut mesh) in oriented.into_iter().enumerate() {
        let pl = placements[i];
        if pl.rotated {
            // 90° CCW about Z: (x, y) → (-y, x); then re-seat the min-corner to the origin so the
            // footprint matches the packer's landscape-normalized dims.
            for v in &mut mesh.verts {
                let (x, y) = (v[0], v[1]);
                v[0] = -y;
                v[1] = x;
            }
            let (min, _) = bbox(&mesh.verts).expect("non-empty after orient");
            for v in &mut mesh.verts {
                v[0] -= min[0];
                v[1] -= min[1];
            }
        }
        plates[pl.plate].push(Placed {
            mesh,
            at: [pl.x, pl.y],
        });
    }

    write_project_to(out, &plates, bed)?;
    Ok(ExportSummary {
        plates: plate_n,
        pieces: pieces.len(),
        fill,
    })
}

/// Axis-aligned bounding box of a vertex set (`None` if empty).
fn bbox(verts: &[[f64; 3]]) -> Option<([f64; 3], [f64; 3])> {
    if verts.is_empty() {
        return None;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for v in verts {
        for k in 0..3 {
            min[k] = min[k].min(v[k]);
            max[k] = max[k].max(v[k]);
        }
    }
    Some((min, max))
}

/// 3×3 (row-major) times a column vector.
fn matvec(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// 3×3 (row-major) product.
fn matmul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// Rotation (row-major 3×3) taking unit `up` to +Z, via Rodrigues — the minimal rotation, so the
/// piece's spin about the build-up axis is arbitrary (fine, it prints the same either way). Handles
/// the already-aligned and antipodal (`up ≈ -Z`) cases.
fn rot_up_to_z(up: [f64; 3]) -> [[f64; 3]; 3] {
    const I: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let u = geom::normalize(up);
    let c = u[2]; // u · +Z
    if c > 1.0 - 1e-9 {
        return I;
    }
    if c < -1.0 + 1e-9 {
        return [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]; // 180° about X
    }
    let v = geom::cross(u, [0.0, 0.0, 1.0]); // rotation axis × sin
    let vx = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
    let k = 1.0 / (1.0 + c);
    let vx2 = matmul(vx, vx);
    std::array::from_fn(|i| std::array::from_fn(|j| I[i][j] + vx[i][j] + vx2[i][j] * k))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube (8 verts, 12 tris) authored with min-corner at the origin, min-z = 0.
    fn unit_cube() -> Mesh {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        Mesh { verts, tris }
    }

    #[test]
    fn column_count_matches_bambu_grid() {
        for (n, cols) in [
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 2),
            (5, 3),
            (6, 3),
            (9, 3),
            (10, 4),
            (16, 4),
        ] {
            assert_eq!(column_count(n), cols, "n={n}");
        }
    }

    #[test]
    fn plates_descend_into_negative_y() {
        // Row 1 (plate index = cols) sits a full stride into -Y; column advances +X.
        let cols = column_count(4); // 2
        assert_eq!(plate_origin(0, cols, [256.0, 256.0]), [0.0, 0.0]);
        assert_eq!(plate_origin(1, cols, [256.0, 256.0]), [256.0 * 1.2, 0.0]);
        assert_eq!(plate_origin(2, cols, [256.0, 256.0]), [0.0, -256.0 * 1.2]);
    }

    #[test]
    fn writes_a_valid_two_plate_project() {
        let tmp = std::env::temp_dir().join(format!("bambu_{}.3mf", std::process::id()));
        let plates = vec![
            vec![Placed {
                mesh: unit_cube(),
                at: [10.0, 10.0],
            }],
            vec![Placed {
                mesh: unit_cube(),
                at: [20.0, 20.0],
            }],
        ];
        write_project(&tmp, &plates, [256.0, 256.0]).unwrap();

        // Re-open the zip and pull the two XML entries back out.
        let f = std::fs::File::open(&tmp).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        for want in [
            "[Content_Types].xml",
            "_rels/.rels",
            "3D/3dmodel.model",
            "Metadata/model_settings.config",
        ] {
            assert!(
                names.iter().any(|n| n == want),
                "missing {want}; have {names:?}"
            );
        }
        let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| {
            use std::io::Read;
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        let model = read(&mut zip, "3D/3dmodel.model");
        // FACT 1: the recognition gate.
        assert!(
            model.contains("name=\"Application\">BambuStudio-"),
            "Application gate missing"
        );
        // Two pieces → two build items, two wrapper objects (id 2 and 4).
        assert_eq!(model.matches("<item ").count(), 2, "expected 2 build items");
        assert!(model.contains("objectid=\"2\""));
        assert!(model.contains("objectid=\"4\""));

        let settings = read(&mut zip, "Metadata/model_settings.config");
        assert_eq!(settings.matches("<plate>").count(), 2, "expected 2 plates");
        assert!(
            settings.contains("value=\"1\"") && settings.contains("value=\"2\""),
            "plater ids"
        );
        assert_eq!(
            settings.matches("<model_instance>").count(),
            2,
            "one instance per plate"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn rot_up_to_z_maps_the_build_up_to_plus_z() {
        for up in [
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0],
        ] {
            let z = matvec(rot_up_to_z(up), geom::normalize(up));
            assert!(
                (z[0]).abs() < 1e-9 && (z[1]).abs() < 1e-9 && (z[2] - 1.0).abs() < 1e-9,
                "up {up:?} → {z:?}"
            );
        }
    }

    #[test]
    fn export_plates_orients_and_writes() {
        let tmp = std::env::temp_dir().join(format!("bambu_export_{}.3mf", std::process::id()));
        // Two cubes, different build-ups — both small, so they share one plate.
        let pieces = vec![
            PieceToPlace {
                mesh: unit_cube(),
                up: [0.0, 0.0, 1.0],
            },
            PieceToPlace {
                mesh: unit_cube(),
                up: [1.0, 0.0, 0.0],
            },
        ];
        let sum = export_plates(&tmp, pieces, [256.0, 256.0], 3.0).unwrap();
        assert_eq!(sum.pieces, 2);
        assert_eq!(sum.plates, 1, "two 1mm cubes fit one 256 bed");
        assert!(sum.fill > 0.0);

        let f = std::fs::File::open(&tmp).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        use std::io::Read;
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("name=\"Application\">BambuStudio-"));
        assert_eq!(model.matches("<item ").count(), 2);
        let mut settings = String::new();
        zip.by_name("Metadata/model_settings.config")
            .unwrap()
            .read_to_string(&mut settings)
            .unwrap();
        assert_eq!(settings.matches("<plate>").count(), 1, "one shared plate");

        let _ = std::fs::remove_file(&tmp);
    }

    /// A 200×200 slab authored with min-corner at the origin (fills most of a 256 bed → one per plate).
    fn slab() -> Mesh {
        let s = 200.0;
        Mesh {
            verts: vec![
                [0.0, 0.0, 0.0],
                [s, 0.0, 0.0],
                [s, s, 0.0],
                [0.0, s, 0.0],
                [0.0, 0.0, 5.0],
                [s, 0.0, 5.0],
                [s, s, 5.0],
                [0.0, s, 5.0],
            ],
            tris: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [1, 2, 6],
                [1, 6, 5],
                [2, 3, 7],
                [2, 7, 6],
                [3, 0, 4],
                [3, 4, 7],
            ],
        }
    }

    #[test]
    fn pieces_land_inside_their_plate_cells() {
        // FACT 2: Bambu bins each piece to a plate by POSITION. Three bed-filling slabs → three
        // plates, one each; every build-item transform must sit inside its plate's world bed cell.
        let bed = [256.0, 256.0];
        let mut pieces = Vec::new();
        for _ in 0..3 {
            pieces.push(PieceToPlace {
                mesh: slab(),
                up: [0.0, 0.0, 1.0],
            });
        }
        let tmp = std::env::temp_dir().join(format!("bambu_cells_{}.3mf", std::process::id()));
        let sum = export_plates(&tmp, pieces, bed, 5.0).unwrap();
        assert_eq!(sum.plates, 3, "three bed-filling slabs need three plates");

        let f = std::fs::File::open(&tmp).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        use std::io::Read;
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();

        let cols = column_count(3);
        let mut plates_hit = std::collections::HashSet::new();
        for line in model.lines().filter(|l| l.contains("<item ")) {
            let xf = line
                .split("transform=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            let n: Vec<f64> = xf.split_whitespace().map(|s| s.parse().unwrap()).collect();
            let (tx, ty) = (n[9], n[10]); // translation = last three of the 12
            let p = (0..3)
                .find(|&p| {
                    let [ox, oy] = plate_origin(p, cols, bed);
                    tx >= ox - 1e-6
                        && tx <= ox + bed[0] + 1e-6
                        && ty >= oy - 1e-6
                        && ty <= oy + bed[1] + 1e-6
                })
                .unwrap_or_else(|| panic!("item at ({tx},{ty}) fell in no plate cell"));
            plates_hit.insert(p);
        }
        assert_eq!(
            plates_hit.len(),
            3,
            "each of the three plates holds exactly one piece"
        );

        let _ = std::fs::remove_file(&tmp);
    }
}
