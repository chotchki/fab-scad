//! Standard 3MF whole-model writer (W.5.4/W.5.5) — the web save-back's full-res mesh variant.
//!
//! UNLIKE [`crate::bambu`] (a Bambu PROJECT `.3mf`: a plate grid + a slicer `project_settings.config`),
//! this emits a PLAIN standard 3MF — ONE `<object>` at the origin — that any conforming reader opens,
//! specifically the site's three.js `3MFLoader`. Color rides CORE 3MF `<basematerials>` (a `displaycolor`
//! plus a per-triangle `pid`/`p1` material ref), NOT a slicer paint extension (Bambu MMU) the viewer can't
//! read. So the per-vertex RGBA the kernel carries (`Solid::vertex_colors`, survives every boolean)
//! collapses to a small DISTINCT-COLOR material table.
//!
//! Writer-generic (an in-memory `Cursor` on wasm, a `File` on native) and mesh-only — no `Solid`, so it
//! runs on the geom worker thread. Deterministic: the material table is first-seen order.
//!
//! Color fidelity caveat: a triangle takes its FIRST vertex's color (uniform within a colored subtree,
//! so exact there); at a boolean seam between two colors the picked band is arbitrary. Fine for a
//! preview/print viewer; upgrade to the per-vertex `<m:colorgroup>` extension if seams ever matter.

use std::collections::HashMap;
use std::io::{Cursor, Seek, Write};

use anyhow::{Context, Result};

// Static OPC entries (3MF core; no png/gcode — this isn't a slicer project).
const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\
 <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\
 <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\
</Types>\n";
const RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\
 <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n\
</Relationships>\n";

/// Quantize a 0..1 RGBA channel set to 8-bit, the key the material table dedups on (and the value
/// `displaycolor` emits).
fn quantize(c: [f64; 4]) -> [u8; 4] {
    let q = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c[0]), q(c[1]), q(c[2]), q(c[3])]
}

/// The DISTINCT-color table (first-seen order) + a per-triangle index into it. A triangle's color is
/// its first vertex's (see the module caveat). `HashMap` is lookup-only — the emitted order is the
/// `Vec`'s push order (mesh-triangle order), so this stays deterministic despite the map.
fn materials(tris: &[[u32; 3]], colors: &[[f64; 4]]) -> (Vec<[u8; 4]>, Vec<usize>) {
    let mut table: Vec<[u8; 4]> = Vec::new();
    let mut seen: HashMap<[u8; 4], usize> = HashMap::new();
    let mut per_tri = Vec::with_capacity(tris.len());
    for t in tris {
        let q = quantize(colors[t[0] as usize]);
        let idx = *seen.entry(q).or_insert_with(|| {
            table.push(q);
            table.len() - 1
        });
        per_tri.push(idx);
    }
    (table, per_tri)
}

/// Build `3D/3dmodel.model`. `colors` (per-vertex RGBA 0..1, index-aligned to `verts`) → a
/// `<basematerials>` table + per-triangle refs; `None` (or a length mismatch, guarded by the caller)
/// → a plain uncolored mesh.
fn model_xml(verts: &[[f64; 3]], tris: &[[u32; 3]], colors: Option<&[[f64; 4]]>) -> String {
    let mats = colors.map(|c| materials(tris, c));
    let colored = mats.as_ref().is_some_and(|(t, _)| !t.is_empty());

    let mut s = String::with_capacity(64 * (verts.len() + tris.len()) + 512);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n");
    s.push_str(" <resources>\n");

    if let Some((table, _)) = mats.as_ref().filter(|_| colored) {
        s.push_str("  <basematerials id=\"1\">\n");
        for (i, c) in table.iter().enumerate() {
            s.push_str(&format!(
                "   <base name=\"c{i}\" displaycolor=\"#{:02X}{:02X}{:02X}{:02X}\"/>\n",
                c[0], c[1], c[2], c[3]
            ));
        }
        s.push_str("  </basematerials>\n");
    }

    // object id=2 (id=1 is the materials resource when colored); a default material index makes the
    // object valid even if a reader ignores per-triangle refs.
    if colored {
        s.push_str("  <object id=\"2\" type=\"model\" pid=\"1\" pindex=\"0\">\n   <mesh>\n    <vertices>\n");
    } else {
        s.push_str("  <object id=\"2\" type=\"model\">\n   <mesh>\n    <vertices>\n");
    }
    for v in verts {
        s.push_str(&format!(
            "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n",
            v[0], v[1], v[2]
        ));
    }
    s.push_str("    </vertices>\n    <triangles>\n");
    match mats.as_ref().filter(|_| colored) {
        Some((_, per_tri)) => {
            for (t, m) in tris.iter().zip(per_tri) {
                s.push_str(&format!(
                    "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\" pid=\"1\" p1=\"{m}\"/>\n",
                    t[0], t[1], t[2]
                ));
            }
        }
        None => {
            for t in tris {
                s.push_str(&format!(
                    "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n",
                    t[0], t[1], t[2]
                ));
            }
        }
    }
    s.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    s.push_str(" </resources>\n <build>\n  <item objectid=\"2\"/>\n </build>\n</model>\n");
    s
}

/// Emit `verts`/`tris` (+ optional per-vertex `colors`, RGBA 0..1 index-aligned to `verts`) as a
/// standard 3MF OPC zip into `out`. A color slice whose length doesn't match `verts` is treated as
/// uncolored (defensive — a mismatched table would index out of bounds).
pub fn write_3mf_to<W: Write + Seek>(
    out: W,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    colors: Option<&[[f64; 4]]>,
) -> Result<()> {
    let colors = colors.filter(|c| c.len() == verts.len());
    let model = model_xml(verts, tris, colors);
    let mut zip = zip::ZipWriter::new(out);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("3D/3dmodel.model", model.as_str()),
    ] {
        zip.start_file(name, opts)
            .with_context(|| format!("zip entry {name}"))?;
        zip.write_all(body.as_bytes())
            .with_context(|| format!("writing {name}"))?;
    }
    zip.finish().context("finalizing 3mf zip")?;
    Ok(())
}

/// In-memory bytes — the browser download/upload path (and native inline). Infallible: an in-memory
/// `Cursor` never fails a write, so the twin of [`crate::kernel`]'s `to_stl_bytes` shape holds.
pub fn to_3mf_bytes(verts: &[[f64; 3]], tris: &[[u32; 3]], colors: Option<&[[f64; 4]]>) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    write_3mf_to(&mut buf, verts, tris, colors).expect("in-memory 3mf write cannot fail");
    buf.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_entry(bytes: &[u8], name: &str) -> Option<String> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).ok()?;
        let mut f = zip.by_name(name).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    }

    /// A unit tetrahedron — 4 verts, 4 faces.
    fn tetra() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        (
            vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            vec![[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]],
        )
    }

    #[test]
    fn colorless_emits_the_three_opc_entries_and_the_mesh() {
        let (v, t) = tetra();
        let bytes = to_3mf_bytes(&v, &t, None);
        assert!(read_entry(&bytes, "[Content_Types].xml").is_some());
        assert!(read_entry(&bytes, "_rels/.rels").is_some());
        let model = read_entry(&bytes, "3D/3dmodel.model").expect("has the model part");
        assert_eq!(model.matches("<vertex ").count(), 4);
        assert_eq!(model.matches("<triangle ").count(), 4);
        assert!(!model.contains("basematerials"), "uncolored → no materials");
        assert!(
            !model.contains("pid="),
            "uncolored → no per-triangle material ref"
        );
    }

    #[test]
    fn two_uniform_color_faces_emit_two_basematerials_with_per_triangle_refs() {
        // Two DISJOINT triangles, each uniform — the realistic "two colored regions" case (a triangle
        // takes its v0's color, so each region's faces map cleanly to one material).
        let verts = vec![
            [0., 0., 0.],
            [1., 0., 0.],
            [0., 1., 0.], // red face
            [2., 0., 0.],
            [3., 0., 0.],
            [2., 1., 0.], // blue face
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        let red = [1., 0., 0., 1.];
        let blue = [0., 0., 1., 1.];
        let colors = vec![red, red, red, blue, blue, blue];
        let model = read_entry(
            &to_3mf_bytes(&verts, &tris, Some(&colors)),
            "3D/3dmodel.model",
        )
        .expect("model part");
        assert_eq!(model.matches("<base ").count(), 2, "two distinct colors");
        assert!(model.contains("displaycolor=\"#FF0000FF\""));
        assert!(model.contains("displaycolor=\"#0000FFFF\""));
        assert!(model.contains("p1=\"0\"") && model.contains("p1=\"1\""));
    }

    #[test]
    fn repeated_color_dedups_to_one_material() {
        let (v, t) = tetra();
        let all_green = vec![[0., 1., 0., 1.]; 4];
        let model =
            read_entry(&to_3mf_bytes(&v, &t, Some(&all_green)), "3D/3dmodel.model").unwrap();
        assert_eq!(model.matches("<base ").count(), 1, "one distinct color");
    }

    #[test]
    fn mismatched_color_length_falls_back_to_uncolored() {
        let (v, t) = tetra();
        let too_few = vec![[1., 0., 0., 1.]; 2]; // 2 colors, 4 verts
        let model = read_entry(&to_3mf_bytes(&v, &t, Some(&too_few)), "3D/3dmodel.model").unwrap();
        assert!(!model.contains("basematerials"), "guarded, not a panic");
    }

    #[test]
    fn output_is_byte_deterministic() {
        let (v, t) = tetra();
        let colors = vec![[0.2, 0.4, 0.6, 1.0]; 4];
        assert_eq!(
            to_3mf_bytes(&v, &t, Some(&colors)),
            to_3mf_bytes(&v, &t, Some(&colors)),
            "same mesh → identical bytes"
        );
    }
}
