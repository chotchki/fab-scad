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

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

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
            model.push_str(&format!("  <object id=\"{mesh_id}\" type=\"model\">\n   <mesh>\n    <vertices>\n"));
            for v in &placed.mesh.verts {
                model.push_str(&format!("     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n", v[0], v[1], v[2]));
            }
            model.push_str("    </vertices>\n    <triangles>\n");
            for t in &placed.mesh.tris {
                model.push_str(&format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0], t[1], t[2]));
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

    let file =
        std::fs::File::create(path).with_context(|| format!("creating 3mf {}", path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("3D/3dmodel.model", model.as_str()),
        ("Metadata/model_settings.config", settings.as_str()),
    ] {
        zip.start_file(name, opts).with_context(|| format!("zip entry {name}"))?;
        zip.write_all(body.as_bytes()).with_context(|| format!("writing {name}"))?;
    }
    zip.finish().context("finalizing 3mf zip")?;
    Ok(())
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
        for (n, cols) in [(1, 1), (2, 2), (3, 2), (4, 2), (5, 3), (6, 3), (9, 3), (10, 4), (16, 4)] {
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
            vec![Placed { mesh: unit_cube(), at: [10.0, 10.0] }],
            vec![Placed { mesh: unit_cube(), at: [20.0, 20.0] }],
        ];
        write_project(&tmp, &plates, [256.0, 256.0]).unwrap();

        // Re-open the zip and pull the two XML entries back out.
        let f = std::fs::File::open(&tmp).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        for want in ["[Content_Types].xml", "_rels/.rels", "3D/3dmodel.model", "Metadata/model_settings.config"] {
            assert!(names.iter().any(|n| n == want), "missing {want}; have {names:?}");
        }
        let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| {
            use std::io::Read;
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        let model = read(&mut zip, "3D/3dmodel.model");
        // FACT 1: the recognition gate.
        assert!(model.contains("name=\"Application\">BambuStudio-"), "Application gate missing");
        // Two pieces → two build items, two wrapper objects (id 2 and 4).
        assert_eq!(model.matches("<item ").count(), 2, "expected 2 build items");
        assert!(model.contains("objectid=\"2\""));
        assert!(model.contains("objectid=\"4\""));

        let settings = read(&mut zip, "Metadata/model_settings.config");
        assert_eq!(settings.matches("<plate>").count(), 2, "expected 2 plates");
        assert!(settings.contains("value=\"1\"") && settings.contains("value=\"2\""), "plater ids");
        assert_eq!(settings.matches("<model_instance>").count(), 2, "one instance per plate");

        let _ = std::fs::remove_file(&tmp);
    }
}
