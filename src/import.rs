//! `import()`/`surface()` mesh readers (M.5) — the impure side of fab-lang's needs fixpoint. fab-lang
//! stays PURE and hands each `File` need (the literal `file=` path a running program reached) to a reader;
//! this is that reader, plus the thin drivers that wire it into `fab_lang::resolve_geometry_*`.
//!
//! Dispatch is by EXTENSION, matching OpenSCAD's own import demux: `.stl`/`.3mf` load through our existing
//! mesh readers ([`crate::stl`] / [`crate::threemf_in`]); `.svg`/`.dxf` (2D vector), `.off`, and
//! `surface()`'s `.dat`/`.png` heightmaps are LOUD-deferred (named, never silently empty) until their
//! readers land (surface = M.5.2). Path resolution is FILE-relative — OpenSCAD resolves an import against
//! the directory of the `.scad` that names it, NOT the library search path (that's a `use`/`include` thing)
//! — so a relative `raw` joins `base_dir`, an absolute one is used as-is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fab_lang::{Error, Geo, Imported, Mesh, Tri, Vec3};

/// Read an `import()`/`surface()` file → a dimension-tagged [`Imported`] — the reader fab-lang's fixpoint
/// hands each `File` need to. Dispatch is by EXTENSION (OpenSCAD's own import demux): `.stl`/`.3mf`/`.dat`
/// are 3D meshes, `.svg` is 2D vector art (Q.4). Every failure is an [`Error::Load`] so it travels the
/// fixpoint as a LOUD stop, never a silently-empty result.
///
/// # Errors
/// [`Error::Load`] for a deferred/unknown extension, an unreadable file, or a malformed mesh/vector.
pub fn read_import(base_dir: &Path, raw: &str) -> Result<Imported, Error> {
    let path = resolve(base_dir, raw);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "stl" => stl_mesh(&path).map(Imported::Mesh),
        "3mf" => threemf_mesh(&path).map(Imported::Mesh),
        "svg" | "dxf" => loud(raw, "2D vector import (svg/dxf) is deferred — no 2D import path yet"),
        "off" => loud(raw, "OFF import is deferred — the OFF reader isn't wired"),
        "dat" => crate::surface::dat_mesh(&path)
            .map(Imported::Mesh)
            .map_err(|e| Error::Load(format!("{}: {e:#}", path.display()))),
        "png" => loud(raw, "surface() PNG heightmap is deferred (backlog #159) — use a DAT file"),
        _ => loud(raw, "unknown import extension — expected stl, 3mf, svg, or dat"),
    }
}

/// Evaluate in-memory `source` to a geometry [`Geo`] tree, resolving `import`/`surface` files via
/// [`read_import`] against `base_dir`. The native driver behind an unsaved-buffer render.
///
/// # Errors
/// As [`fab_lang::resolve_geometry_with_base`], plus any [`read_import`] failure.
pub fn resolve_geometry_with_base(
    source: &str,
    base_dir: &Path,
    library_paths: &[PathBuf],
) -> Result<Geo, Error> {
    fab_lang::resolve_geometry_with_base(source, base_dir, library_paths, |raw| {
        read_import(base_dir, raw)
    })
}

/// Evaluate a `.scad` FILE to a geometry [`Geo`] tree, resolving its `use`/`include` graph AND its
/// `import`/`surface` files (via [`read_import`], relative to the file's own directory).
///
/// # Errors
/// As [`fab_lang::resolve_geometry_file`], plus any [`read_import`] failure.
pub fn resolve_geometry_file(path: &Path, library_paths: &[PathBuf]) -> Result<Geo, Error> {
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    fab_lang::resolve_geometry_file(path, library_paths, |raw| read_import(&base_dir, raw))
}

/// Join a relative `raw` onto `base_dir`; an absolute `raw` is used as-is (OpenSCAD `find_valid_path` for
/// imports, minus the library search — imports are file-relative only).
fn resolve(base_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

/// The LOUD refusal for a deferred/unknown import — names the file + why, so it's never a silent empty.
fn loud(raw: &str, why: &str) -> Result<Imported, Error> {
    Err(Error::Load(format!("import '{raw}': {why}")))
}

/// An STL (binary or ASCII) → an INDEXED [`Mesh`]: our [`crate::stl`] reader yields a flat triangle SOUP
/// (3 positions per triangle, welded nowhere), which [`index_soup`] dedups into unique verts + face
/// indices — the shape a `Leaf` needs.
fn stl_mesh(path: &Path) -> Result<Mesh, Error> {
    let soup = crate::stl::load_stl(path)
        .map_err(|e| Error::Load(format!("{}: {e:#}", path.display())))?;
    Ok(index_soup(&soup.positions))
}

/// A `.3mf` → a [`Mesh`]: [`crate::threemf_in::parse_3mf`] gives already-indexed build objects; `import()`
/// unions the whole file, so we CONCATENATE them, offsetting each object's face indices by the running
/// vertex count (color is dropped — an import is uncolored geometry).
fn threemf_mesh(path: &Path) -> Result<Mesh, Error> {
    let bytes = std::fs::read(path).map_err(|e| Error::Load(format!("{}: {e}", path.display())))?;
    let objects = crate::threemf_in::parse_3mf(&bytes)
        .map_err(|e| Error::Load(format!("{}: {e:#}", path.display())))?;
    let mut mesh = Mesh::new();
    for obj in objects {
        let base = u32::try_from(mesh.verts.len()).unwrap_or(u32::MAX);
        mesh.verts
            .extend(obj.verts.into_iter().map(Vec3::from_array));
        mesh.tris.extend(
            obj.tris
                .into_iter()
                .map(|[a, b, c]| Tri::new(a + base, b + base, c + base)),
        );
    }
    Ok(mesh)
}

/// Dedup a flat triangle SOUP (3 positions per triangle) into unique verts + index triples. Keyed by exact
/// f64 bits (the determinism doctrine's bit-identity via `to_bits`, so `NaN`/`±0` can't collide-or-miss the
/// way `==` would), giving the SAME indexing for the same soup every run. A trailing partial triangle (a
/// soup whose length isn't a multiple of 3) drops — defensive, our readers never emit one.
fn index_soup(positions: &[[f32; 3]]) -> Mesh {
    let mut verts: Vec<Vec3> = Vec::new();
    let mut tris = Vec::new();
    let mut index: BTreeMap<[u64; 3], u32> = BTreeMap::new();
    let mut face = [0u32; 3];
    for (i, p) in positions.iter().enumerate() {
        let v = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
        let key = [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()];
        let idx = if let Some(&existing) = index.get(&key) {
            existing
        } else {
            let n = u32::try_from(verts.len()).unwrap_or(u32::MAX);
            verts.push(Vec3::from_array(v));
            index.insert(key, n);
            n
        };
        face[i % 3] = idx;
        if i % 3 == 2 {
            tris.push(Tri::new(face[0], face[1], face[2]));
        }
    }
    Mesh { verts, tris }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: unwrap/expect ARE the assertions"
)]
mod tests {
    use std::path::PathBuf;

    use fab_lang::{Geo, GeoNode};

    use fab_lang::Imported;

    use super::{read_import, resolve_geometry_with_base};

    /// Unwrap a 3D [`Imported`] payload to its mesh — every reader test here imports a mesh format.
    fn mesh_of(imported: Imported) -> fab_lang::Mesh {
        match imported {
            Imported::Mesh(mesh) => mesh,
            Imported::Contours(_) => panic!("expected a 3D mesh payload, got 2D contours"),
        }
    }

    /// The process temp dir (unit tests get no `CARGO_TARGET_TMPDIR` — that's integration-only).
    fn tmp() -> PathBuf {
        std::env::temp_dir()
    }

    /// A process-unique fixture name so parallel test binaries don't collide on the same file.
    fn unique(name: &str) -> String {
        format!("fab_import_{}_{name}", std::process::id())
    }

    /// A unit cube as indexed verts + fan-ish tris — 8 corners, 12 triangles.
    fn cube_indexed() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
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
        (verts, tris)
    }

    #[test]
    fn stl_import_dedups_the_soup_back_to_a_cube() {
        // Write a cube as a binary STL (a 36-vertex triangle soup), import it, and confirm read_import welds
        // the soup back to 8 unique corners + 12 faces.
        let (verts, tris) = cube_indexed();
        let bytes = crate::stl::binary_from_indexed(&verts, &tris);
        let name = unique("cube.stl");
        std::fs::write(tmp().join(&name), bytes).unwrap();

        let mesh = mesh_of(read_import(&tmp(), &name).expect("stl imports"));
        assert_eq!(mesh.vert_count(), 8, "8 unique corners after dedup");
        assert_eq!(mesh.tri_count(), 12, "12 faces");

        // ...and end to end: import(file) is a single 3D leaf carrying that mesh.
        let src = format!("import(\"{name}\");");
        match resolve_geometry_with_base(&src, &tmp(), &[]).expect("resolves") {
            Geo::D3(GeoNode::Leaf(ref leaf)) => assert_eq!(*leaf, mesh),
            other => panic!("expected a 3D leaf, got {other:?}"),
        }
    }

    #[test]
    fn threemf_import_concatenates_build_objects() {
        // Two kernel cubes written to a 3mf, imported back: their meshes concatenate (24 tris), face
        // indices offset so the second cube's faces point at its own verts.
        let a = crate::kernel::Solid::cube(10.0, 10.0, 10.0, false);
        let b = crate::kernel::Solid::cube(5.0, 5.0, 5.0, false)
            .translate(fab_lang::Vec3::new(20.0, 0.0, 0.0));
        let name = unique("two_cubes.3mf");
        let path = tmp().join(&name);
        crate::kernel::Solid::write_3mf(&path, &[a, b]).unwrap();

        let mesh = mesh_of(read_import(&tmp(), &name).expect("3mf imports"));
        assert_eq!(mesh.tri_count(), 24, "12 tris per cube, concatenated");
        // The second cube lives at x ∈ [20, 25] — proof the index offset kept its faces on its own verts.
        let max_x = mesh
            .verts
            .iter()
            .map(|v| v.x)
            .fold(f64::MIN, f64::max);
        assert!(max_x >= 24.9, "second cube's verts survived the offset, max_x = {max_x}");
    }

    #[test]
    fn deferred_and_unknown_imports_are_loud() {
        // Never silently empty: a deferred format, an unknown extension, and a missing file each name the
        // problem through Error::Load.
        // Deferred/unknown extensions refuse by EXTENSION before any read — a fixed name is fine.
        for (raw, needle) in [
            ("drawing.svg", "svg"),
            ("shape.dxf", "dxf"),
            ("solid.off", "OFF"),
            ("height.png", "PNG"),
            ("mystery.xyz", "unknown"),
        ] {
            let err = read_import(&tmp(), raw).unwrap_err();
            assert!(
                format!("{err}").contains(needle),
                "{raw}: expected an error mentioning {needle:?}, got {err}"
            );
        }
        // A KNOWN extension whose file is absent still fails LOUD (the read errors) — a process-unique name
        // that was never written is guaranteed missing.
        let missing = unique("absent.stl");
        let err = read_import(&tmp(), &missing).unwrap_err();
        assert!(format!("{err}").contains(&missing), "got {err}");
    }
}
