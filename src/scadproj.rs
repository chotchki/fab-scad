//! Phase Z: the `.scadproj` container — a multi-file OpenSCAD PROJECT as ONE portable zip, so the fs-less
//! web app can open a project the way the desktop opens a folder (`include <hook.scad>` and friends
//! resolve, binary `import()`/`surface()` assets ride along). See `docs/web-projects-design.md`.
//!
//! A STORED zip (fab's OPC convention, and it dodges decompression bombs), self-identifying two ways: an
//! EPUB-style uncompressed `mimetype` FIRST entry (byte-sniffable without unzipping) plus a root
//! `fab-project.json` manifest naming the `.scad` to render. Reader + writer are BYTE-based — no fs — so
//! the same code runs on the wasm worker and the native `fab` CLI; the directory walk lives in the CLI.
//!
//! LOUDLY a zip (chotchki's call): a `.scadproj` is just a zip anyone can rename to `.zip` and unpack.

#![cfg(feature = "mesh-io")]

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, Write};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// The MIME a `.scadproj` self-identifies with — the `mimetype` entry body AND the site's stored
/// content-type. `application/` (not `model/`) keeps projects out of the mesh glob (matches SCAD's mime).
pub const PROJECT_MIME: &str = "application/x-openscad-project";

/// The uncompressed FIRST zip entry (the EPUB trick) — byte-sniff the type without unzipping.
pub const MIMETYPE_ENTRY: &str = "mimetype";

/// The root manifest: names the entry `.scad` (+ publish metadata).
pub const MANIFEST_ENTRY: &str = "fab-project.json";

/// The conventional extension. A distinct suffix is the disambiguator (never collides with a random zip).
pub const PROJECT_EXT: &str = "scadproj";

/// A zip-bomb backstop: a real project is a handful of small text files + a few assets.
const MAX_ENTRIES: usize = 10_000;
/// A zip-bomb backstop on total unpacked size (512 MiB — generous for meshes, far below a bomb).
const MAX_UNCOMPRESSED: u64 = 512 * 1024 * 1024;

/// The root `fab-project.json`. `entry` names the `.scad` that RENDERS — every file stays editable; the
/// entry only decides what the viewport builds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    /// Relative path of the `.scad` to render (e.g. `"shower_holder.scad"`).
    pub entry: String,
    /// Human title for publish; the app falls back to the entry stem when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Container schema version (currently 1).
    #[serde(default = "one")]
    pub version: u32,
}
fn one() -> u32 {
    1
}

/// An unpacked project: the manifest + every project file keyed by NORMALIZED relative path (`.scad`
/// neighbors AND assets). Excludes the container metadata (`mimetype`, the manifest itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub manifest: ProjectManifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Project {
    /// The entry `.scad`'s bytes.
    pub fn entry_bytes(&self) -> Result<&[u8]> {
        self.files
            .get(&self.manifest.entry)
            .map(Vec::as_slice)
            .ok_or_else(|| {
                anyhow!(
                    "manifest entry {:?} is not in the project",
                    self.manifest.entry
                )
            })
    }
}

/// Assemble a [`Project`] from a file map, forcing `entry` or deriving it (the single `.scad`). Rejects an
/// `entry` that isn't among the files.
pub fn project_from_files(
    files: BTreeMap<String, Vec<u8>>,
    entry: Option<String>,
    title: Option<String>,
) -> Result<Project> {
    let entry = match entry {
        Some(e) => {
            if !files.contains_key(&e) {
                bail!("entry {e:?} is not among the project files");
            }
            e
        }
        None => derive_entry(&files)?,
    };
    Ok(Project {
        manifest: ProjectManifest {
            entry,
            title,
            version: 1,
        },
        files,
    })
}

/// Pick the render entry when there's no manifest: the single `.scad`. Multiple `.scad` are AMBIGUOUS —
/// they need a `fab-project.json` to name the root (an include-graph heuristic could pick it, deferred).
fn derive_entry(files: &BTreeMap<String, Vec<u8>>) -> Result<String> {
    let scads: Vec<&String> = files.keys().filter(|k| k.ends_with(".scad")).collect();
    match scads.as_slice() {
        [] => bail!("no .scad file in the project"),
        [one] => Ok((*one).clone()),
        many => bail!(
            "{} .scad files and no {MANIFEST_ENTRY} — add a manifest naming the entry (one of {:?})",
            many.len(),
            many.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        ),
    }
}

/// Serialize a project to `.scadproj` bytes (the browser upload/download + native inline path).
pub fn write_scadproj(project: &Project) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    write_scadproj_to(&mut buf, project)?;
    Ok(buf.into_inner())
}

/// Write a project as a STORED zip to any sink: `mimetype` FIRST (uncompressed), then the manifest, then
/// the files in deterministic (`BTreeMap`) order so the same project yields the same bytes.
pub fn write_scadproj_to<W: Write + Seek>(out: W, project: &Project) -> Result<()> {
    let mut zip = zip::ZipWriter::new(out);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file(MIMETYPE_ENTRY, stored)
        .context("zip mimetype")?;
    zip.write_all(PROJECT_MIME.as_bytes())
        .context("write mimetype")?;

    let manifest = serde_json::to_vec_pretty(&project.manifest).context("serialize manifest")?;
    zip.start_file(MANIFEST_ENTRY, stored)
        .context("zip manifest")?;
    zip.write_all(&manifest).context("write manifest")?;

    for (path, bytes) in &project.files {
        // The reserved container names can never be project files (they'd shadow the metadata).
        if path == MIMETYPE_ENTRY || path == MANIFEST_ENTRY {
            continue;
        }
        zip.start_file(path, stored)
            .with_context(|| format!("zip {path}"))?;
        zip.write_all(bytes)
            .with_context(|| format!("write {path}"))?;
    }
    zip.finish().context("finalize scadproj")?;
    Ok(())
}

/// Read `.scadproj` bytes into a [`Project`]: verify the marker/manifest identity, sanitize every path
/// (zip-slip), cap the unpack (zip-bomb), and resolve the entry. BYTE-based — wasm-safe.
pub fn read_scadproj(bytes: &[u8]) -> Result<Project> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| anyhow!("not a readable zip: {e}"))?;
    if zip.len() > MAX_ENTRIES {
        bail!(
            "scadproj has too many entries ({} > {MAX_ENTRIES})",
            zip.len()
        );
    }

    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut mimetype: Option<String> = None;
    let mut manifest_raw: Option<Vec<u8>> = None;
    let mut total: u64 = 0;

    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| anyhow!("zip entry {i}: {e}"))?;
        let name = f.name().to_string();
        if name.ends_with('/') {
            continue; // a directory entry
        }
        total = total.saturating_add(f.size());
        if total > MAX_UNCOMPRESSED {
            bail!("scadproj unpacks to more than {MAX_UNCOMPRESSED} bytes — refusing (zip bomb?)");
        }
        let mut body = Vec::new();
        f.read_to_end(&mut body)
            .with_context(|| format!("read zip entry {name:?}"))?;

        match name.as_str() {
            MIMETYPE_ENTRY => {
                mimetype = Some(String::from_utf8_lossy(&body).trim().to_string());
            }
            MANIFEST_ENTRY => manifest_raw = Some(body),
            _ => {
                let clean = normalize_rel(&name)
                    .ok_or_else(|| anyhow!("unsafe path in scadproj: {name:?}"))?;
                files.insert(clean, body);
            }
        }
    }

    // Identity: the marker must say project, OR a manifest must be present (a bare zip is not a project).
    match &mimetype {
        Some(m) if m == PROJECT_MIME => {}
        Some(m) => bail!("not a .scadproj (mimetype is {m:?}, expected {PROJECT_MIME:?})"),
        None if manifest_raw.is_some() => {} // a manifest alone is enough to identify it
        None => bail!("not a .scadproj: no {MIMETYPE_ENTRY} marker and no {MANIFEST_ENTRY}"),
    }

    let manifest = match manifest_raw {
        Some(raw) => serde_json::from_slice(&raw).context("parse fab-project.json")?,
        None => ProjectManifest {
            entry: derive_entry(&files)?,
            title: None,
            version: 1,
        },
    };
    if !files.contains_key(&manifest.entry) {
        bail!(
            "manifest entry {:?} is not present in the project",
            manifest.entry
        );
    }
    Ok(Project { manifest, files })
}

/// Normalize a zip entry name to a SAFE relative path, or `None` if it escapes the root (zip-slip) or is
/// absolute: backslashes → forward slashes, drop `.` components, reject `..`, absolute, and drive-letter
/// paths.
pub fn normalize_rel(raw: &str) -> Option<String> {
    let raw = raw.replace('\\', "/");
    if raw.starts_with('/') {
        return None; // absolute
    }
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        return None; // windows drive letter (C:/…)
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in raw.split('/') {
        match comp {
            "" | "." => continue,
            ".." => return None, // escapes the root
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(files: &[(&str, &[u8])], entry: &str) -> Project {
        let map: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect();
        project_from_files(map, Some(entry.to_string()), Some("Demo".into())).unwrap()
    }

    #[test]
    fn round_trips_files_and_manifest() {
        let p = proj(
            &[
                ("shower_holder.scad", b"include <hook.scad>\ncube(1);\n"),
                ("hook.scad", b"module hook(){}\n"),
                ("assets/logo.svg", b"<svg/>"),
            ],
            "shower_holder.scad",
        );
        let bytes = write_scadproj(&p).unwrap();
        let back = read_scadproj(&bytes).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.manifest.entry, "shower_holder.scad");
        assert_eq!(back.manifest.title.as_deref(), Some("Demo"));
        assert_eq!(
            back.entry_bytes().unwrap(),
            b"include <hook.scad>\ncube(1);\n"
        );
    }

    #[test]
    fn mimetype_is_the_first_entry_and_stored() {
        let p = proj(&[("a.scad", b"cube(1);")], "a.scad");
        let bytes = write_scadproj(&p).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let first = zip.by_index(0).unwrap();
        assert_eq!(first.name(), MIMETYPE_ENTRY, "mimetype must be entry 0");
        assert_eq!(
            first.compression(),
            zip::CompressionMethod::Stored,
            "mimetype must be uncompressed for byte-sniffing"
        );
    }

    #[test]
    fn writer_output_is_deterministic() {
        let p = proj(
            &[("a.scad", b"cube(1);"), ("b.scad", b"sphere(1);")],
            "a.scad",
        );
        assert_eq!(write_scadproj(&p).unwrap(), write_scadproj(&p).unwrap());
    }

    #[test]
    fn derives_entry_from_a_lone_scad_without_a_manifest() {
        // A zip with the marker + one .scad but NO fab-project.json.
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(MIMETYPE_ENTRY, o).unwrap();
            zip.write_all(PROJECT_MIME.as_bytes()).unwrap();
            zip.start_file("only.scad", o).unwrap();
            zip.write_all(b"cube(1);").unwrap();
            zip.finish().unwrap();
        }
        let back = read_scadproj(&buf.into_inner()).unwrap();
        assert_eq!(back.manifest.entry, "only.scad");
    }

    #[test]
    fn ambiguous_entry_without_a_manifest_is_an_error() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(MIMETYPE_ENTRY, o).unwrap();
            zip.write_all(PROJECT_MIME.as_bytes()).unwrap();
            for n in ["a.scad", "b.scad"] {
                zip.start_file(n, o).unwrap();
                zip.write_all(b"cube(1);").unwrap();
            }
            zip.finish().unwrap();
        }
        let err = read_scadproj(&buf.into_inner()).unwrap_err().to_string();
        assert!(err.contains("add a manifest"), "got: {err}");
    }

    #[test]
    fn rejects_a_zip_slip_path() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(MIMETYPE_ENTRY, o).unwrap();
            zip.write_all(PROJECT_MIME.as_bytes()).unwrap();
            zip.start_file("../../etc/passwd", o).unwrap();
            zip.write_all(b"pwned").unwrap();
            zip.finish().unwrap();
        }
        let err = read_scadproj(&buf.into_inner()).unwrap_err().to_string();
        assert!(err.contains("unsafe path"), "got: {err}");
    }

    #[test]
    fn rejects_a_plain_zip_that_is_not_a_project() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("random.txt", o).unwrap();
            zip.write_all(b"hello").unwrap();
            zip.finish().unwrap();
        }
        let err = read_scadproj(&buf.into_inner()).unwrap_err().to_string();
        assert!(err.contains("not a .scadproj"), "got: {err}");
    }

    #[test]
    fn manifest_entry_must_exist() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(MIMETYPE_ENTRY, o).unwrap();
            zip.write_all(PROJECT_MIME.as_bytes()).unwrap();
            zip.start_file(MANIFEST_ENTRY, o).unwrap();
            zip.write_all(br#"{"entry":"ghost.scad"}"#).unwrap();
            zip.start_file("real.scad", o).unwrap();
            zip.write_all(b"cube(1);").unwrap();
            zip.finish().unwrap();
        }
        let err = read_scadproj(&buf.into_inner()).unwrap_err().to_string();
        assert!(err.contains("not present"), "got: {err}");
    }

    #[test]
    fn normalize_rel_sanitizes() {
        assert_eq!(normalize_rel("hook.scad").as_deref(), Some("hook.scad"));
        assert_eq!(
            normalize_rel("sub/hook.scad").as_deref(),
            Some("sub/hook.scad")
        );
        assert_eq!(normalize_rel("./a/./b.scad").as_deref(), Some("a/b.scad"));
        assert_eq!(normalize_rel("a\\b.scad").as_deref(), Some("a/b.scad"));
        assert_eq!(normalize_rel("../escape"), None);
        assert_eq!(normalize_rel("a/../../escape"), None);
        assert_eq!(normalize_rel("/abs"), None);
        assert_eq!(normalize_rel("C:/win"), None);
        assert_eq!(normalize_rel(""), None);
    }
}
