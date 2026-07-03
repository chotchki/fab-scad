//! 3MF reader → per-object indexed meshes + display colors (A.9). Core spec via the `threemf`
//! crate: build items → objects (mesh or components, 3x4 transforms applied down the tree),
//! color from the object-level basematerial `displaycolor`. Per-TRIANGLE property overrides are
//! ignored — object-level pid is how slicers emit multi-color ASSEMBLIES, which is the case
//! that matters here. Mirroring transforms (negative determinant) are not corrected; Manifold
//! will reject the flipped winding rather than print an inside-out part.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, bail, Context, Result};
use threemf::model::{Model, Object};

/// Basematerial group id → displaycolors, in `<m:base>` order. Parsed by US with quick-xml on
/// LOCAL element names: threemf's serde renames bind the literal `m:` prefix and can't read
/// even its own output back (prefixes are arbitrary per XML-NS anyway).
type Materials = HashMap<usize, Vec<String>>;

/// One printable object out of the 3mf: indexed mesh (build/item transform applied) + color.
pub struct Object3mf {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    /// sRGBA 0..=1 from the object's basematerial, when it has one.
    pub color: Option<[f32; 4]>,
}

/// Parse a `.3mf` (zip) into its built objects. Errors on no-mesh files and unresolvable ids;
/// tolerates missing materials (color = None).
pub fn parse_3mf(bytes: &[u8]) -> Result<Vec<Object3mf>> {
    let models = threemf::read(Cursor::new(bytes)).map_err(|e| anyhow!("reading 3mf: {e:?}"))?;
    let materials = read_materials(bytes)?;
    let mut out = Vec::new();
    for model in &models {
        for item in &model.build.item {
            collect(
                model,
                &materials,
                item.objectid,
                item.transform,
                &mut out,
                0,
            )?;
        }
    }
    if out.is_empty() {
        bail!("3mf has no build items with meshes");
    }
    Ok(out)
}

fn collect(
    model: &Model,
    materials: &Materials,
    oid: usize,
    xf: Option<[f64; 12]>,
    out: &mut Vec<Object3mf>,
    depth: usize,
) -> Result<()> {
    if depth > 8 {
        bail!("3mf component nesting too deep (cycle?)");
    }
    let obj = model
        .resources
        .object
        .iter()
        .find(|o| o.id == oid)
        .with_context(|| format!("3mf references missing object {oid}"))?;
    if let Some(mesh) = &obj.mesh {
        out.push(Object3mf {
            verts: mesh
                .vertices
                .vertex
                .iter()
                .map(|v| apply(xf, [v.x, v.y, v.z]))
                .collect(),
            tris: mesh
                .triangles
                .triangle
                .iter()
                .map(|t| [t.v1 as u32, t.v2 as u32, t.v3 as u32])
                .collect(),
            color: color_of(materials, obj),
        });
    }
    if let Some(comps) = &obj.components {
        for c in &comps.component {
            collect(
                model,
                materials,
                c.objectid,
                compose(xf, c.transform),
                out,
                depth + 1,
            )?;
        }
    }
    Ok(())
}

/// 3MF transform: 12 values `m00..m22 tx ty tz`, ROW-vector convention —
/// `x' = x*m00 + y*m10 + z*m20 + tx`.
fn apply(xf: Option<[f64; 12]>, p: [f64; 3]) -> [f64; 3] {
    let Some(m) = xf else { return p };
    std::array::from_fn(|i| p[0] * m[i] + p[1] * m[3 + i] + p[2] * m[6 + i] + m[9 + i])
}

/// `outer ∘ inner` in the same row-vector convention (inner applies first).
fn compose(outer: Option<[f64; 12]>, inner: Option<[f64; 12]>) -> Option<[f64; 12]> {
    match (outer, inner) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => {
            let mut m = [0.0; 12];
            for i in 0..3 {
                for j in 0..3 {
                    m[i * 3 + j] = (0..3).map(|k| b[i * 3 + k] * a[k * 3 + j]).sum();
                }
            }
            for j in 0..3 {
                m[9 + j] = (0..3).map(|k| b[9 + k] * a[k * 3 + j]).sum::<f64>() + a[9 + j];
            }
            Some(m)
        }
    }
}

/// The object's basematerial color: pid → materials group, pindex → entry, `#RRGGBB[AA]`.
fn color_of(materials: &Materials, obj: &Object) -> Option<[f32; 4]> {
    let group = materials.get(&obj.pid?)?;
    parse_color(group.get(obj.pindex.unwrap_or(0))?)
}

/// Pull the basematerials table straight out of every `*.model` zip entry, matching element
/// LOCAL names so any namespace prefix works.
fn read_materials(bytes: &[u8]) -> Result<Materials> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| anyhow!("3mf zip: {e:?}"))?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.ends_with(".model"))
        .collect();
    let mut materials = Materials::new();
    for name in names {
        let mut xml = String::new();
        zip.by_name(&name)
            .map_err(|e| anyhow!("3mf entry {name}: {e:?}"))?
            .read_to_string(&mut xml)?;
        let mut reader = quick_xml::Reader::from_str(&xml);
        let mut group: Option<usize> = None;
        loop {
            use quick_xml::events::Event;
            match reader.read_event().map_err(|e| anyhow!("3mf xml: {e:?}"))? {
                Event::Eof => break,
                Event::Start(e) | Event::Empty(e) => {
                    let local = e.local_name();
                    let attr = |key: &str| {
                        e.attributes().flatten().find_map(|a| {
                            (a.key.local_name().as_ref() == key.as_bytes())
                                .then(|| String::from_utf8_lossy(&a.value).into_owned())
                        })
                    };
                    if local.as_ref() == b"basematerials" {
                        group = attr("id").and_then(|v| v.parse().ok());
                        if let Some(id) = group {
                            materials.entry(id).or_default();
                        }
                    } else if local.as_ref() == b"base" {
                        if let (Some(id), Some(c)) = (group, attr("displaycolor")) {
                            materials.entry(id).or_default().push(c);
                        }
                    }
                }
                Event::End(e) if e.local_name().as_ref() == b"basematerials" => group = None,
                _ => {}
            }
        }
    }
    Ok(materials)
}

fn parse_color(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = if h.len() >= 8 { byte(6)? } else { 255 };
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_colors_from_a_prefixed_materials_table() {
        // Hand-built two-object 3mf with an <m:basematerials> table — the exact shape threemf's
        // own serde fails to read back (the reason read_materials exists).
        let model = r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02">
 <resources>
  <m:basematerials id="1"><m:base name="red" displaycolor="#D03030"/><m:base name="blue" displaycolor="#3060D0"/></m:basematerials>
  <object id="1" pid="1" pindex="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/></vertices><triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="1" v2="2" v3="3"/><triangle v1="0" v2="3" v3="2"/></triangles></mesh></object>
 </resources>
 <build><item objectid="1"/></build>
</model>"##;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("3D/3dmodel.model", o).unwrap();
            z.write_all(model.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let objs = parse_3mf(&buf.into_inner()).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].tris.len(), 4);
        // pindex 1 = blue
        let c = objs[0].color.unwrap();
        assert!(c[2] > c[0], "expected blue-dominant, got {c:?}");
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#FF8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(
            parse_color("#00000080").map(|c| (c[3] * 255.0) as u8),
            Some(128)
        );
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn roundtrips_a_kernel_3mf_with_transform_and_no_color() {
        // Write two cubes via the kernel's own 3mf writer, read them back.
        let a = crate::kernel::Solid::cube(10.0, 10.0, 10.0, false);
        let b = crate::kernel::Solid::cube(5.0, 5.0, 5.0, false).translate(20.0, 0.0, 0.0);
        let path = std::env::temp_dir().join(format!("threemf_in_{}.3mf", std::process::id()));
        crate::kernel::Solid::write_3mf(&path, &[a, b]).unwrap();
        let objs = parse_3mf(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].tris.len(), 12);
        // The second cube sits at x ∈ [20, 25].
        let xs: Vec<f64> = objs[1].verts.iter().map(|v| v[0]).collect();
        assert!(xs.iter().cloned().fold(f64::MAX, f64::min) >= 19.9);
        // And both weld into valid solids through from_indexed.
        for o in &objs {
            crate::kernel::Solid::from_indexed(&o.verts, &o.tris)
                .unwrap()
                .check()
                .unwrap();
        }
    }
}
