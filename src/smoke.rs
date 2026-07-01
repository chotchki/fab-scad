//! The smoke oracle (6.7) + the tree sweep it feeds (6.8). No goldens, no manifests — the fast
//! "does every model still render to REAL geometry" check. A `.scad` passes iff OpenSCAD exits clean
//! AND the mesh has faces; that catches the two failures that matter in a refactor — a broken render,
//! and a render that silently collapses to nothing — without the cost of golden meshes (deferred to 8.4).

use crate::openscad::Openscad;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One file's smoke result.
pub struct Smoke {
    pub input: PathBuf,
    pub pass: bool,
    pub faces: u64,
    pub duration: Duration,
    /// Failure reason, or empty on pass.
    pub detail: String,
}

/// Smoke-render one `.scad`: render to a scratch STL, pass iff it rendered clean with faces > 0.
/// The scratch STL is deleted after counting — we only wanted the verdict, not the geometry.
pub fn smoke(oscad: &Openscad, input: &Path, tmp_dir: &Path, timeout: Duration) -> Smoke {
    use std::hash::{Hash, Hasher};
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher); // unique per input so parallel renders never collide on the scratch path
    let out = tmp_dir.join(format!("smoke-{stem}-{:x}.stl", hasher.finish()));

    let (pass, faces, detail, duration) = match oscad.render(input, &out, timeout) {
        Ok(r) if r.timed_out => (
            false,
            0,
            format!("timed out after {}s", timeout.as_secs()),
            r.duration,
        ),
        Ok(r) if !r.ok => (false, 0, "openscad error or empty output".into(), r.duration),
        Ok(r) => match stl_triangle_count(&out) {
            0 => (false, 0, "rendered but zero faces".into(), r.duration),
            n => (true, n, String::new(), r.duration),
        },
        Err(e) => (false, 0, format!("{e:#}"), Duration::ZERO),
    };
    let _ = std::fs::remove_file(&out);
    Smoke {
        input: input.to_path_buf(),
        pass,
        faces,
        duration,
        detail,
    }
}

/// Triangle count of an STL. Binary STLs carry it in a u32 at byte 80 — trusted only when the file
/// size matches the exact `84 + 50n` binary layout (guards against an ASCII file that happens to be
/// ≥84 bytes); otherwise fall back to counting ASCII `facet` records. Enough to tell real geometry
/// from an empty render — NOT a mesh validator.
pub fn stl_triangle_count(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as u64;
        if bytes.len() as u64 == 84 + 50 * n {
            return n;
        }
    }
    String::from_utf8_lossy(&bytes).matches("facet normal").count() as u64
}

/// Every renderable `.scad` under `root` (recursive, sorted). Skips VCS/build/output dirs and the
/// OPENSCADPATH LIBRARY dirs (`scad-lib`, `libs`) — those hold `include`d modules, not standalone
/// models, so they'd render zero faces and read as false failures.
pub fn scad_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(root, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.')
                || matches!(name, "out" | "target" | "node_modules" | "scad-lib" | "libs")
            {
                continue;
            }
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("scad") {
            out.push(p);
        }
    }
}

/// Incremental cache (6.2) for the sweep: each file's include-closure content-hash + face count from
/// its last PASS, so a re-sweep re-renders only what changed (or last failed). Keyed by the OpenSCAD
/// version — a toolchain bump invalidates the lot. Plain-text, one `hash<TAB>faces<TAB>path` per
/// line; a corrupt or version-mismatched file simply misses (re-render everything), never errors.
pub struct SweepCache {
    entries: HashMap<PathBuf, (u64, u64)>,
}

impl SweepCache {
    /// An empty cache — the effect of `--force` (nothing hits, everything re-renders).
    pub fn empty() -> Self {
        SweepCache {
            entries: HashMap::new(),
        }
    }

    /// Load the cache at `path`, trusting it only if its version header matches `version`.
    pub fn load(path: &Path, version: &str) -> Self {
        let mut entries = HashMap::new();
        let header = format!("# fab-smoke v1 {version}");
        if let Ok(text) = std::fs::read_to_string(path) {
            let mut lines = text.lines();
            if lines.next() == Some(header.as_str()) {
                for l in lines {
                    let mut it = l.splitn(3, '\t');
                    if let (Some(h), Some(f), Some(p)) = (it.next(), it.next(), it.next()) {
                        if let (Ok(h), Ok(f)) = (h.parse::<u64>(), f.parse::<u64>()) {
                            entries.insert(PathBuf::from(p), (h, f));
                        }
                    }
                }
            }
        }
        SweepCache { entries }
    }

    /// The cached face count for `file`, IFF its closure hash still matches (else None → re-render).
    pub fn hit(&self, file: &Path, hash: u64) -> Option<u64> {
        self.entries
            .get(file)
            .filter(|(h, _)| *h == hash)
            .map(|(_, f)| *f)
    }

    /// Overwrite the cache: version header + one line per passing `(file, hash, faces)`.
    pub fn save(path: &Path, version: &str, passing: &[(PathBuf, u64, u64)]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = format!("# fab-smoke v1 {version}\n");
        for (p, h, f) in passing {
            out.push_str(&format!("{h}\t{f}\t{}\n", p.display()));
        }
        std::fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_cache_hits_only_on_a_matching_hash() {
        let dir = std::env::temp_dir().join(format!("smoke_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = dir.join(".fab/smoke-cache");
        let f = PathBuf::from("/models/a.scad");
        SweepCache::save(&cache, "v2024", &[(f.clone(), 42, 100)]).unwrap();

        let loaded = SweepCache::load(&cache, "v2024");
        assert_eq!(loaded.hit(&f, 42), Some(100)); // matching hash → cached faces
        assert_eq!(loaded.hit(&f, 99), None); // changed inputs → miss

        // a version bump invalidates the whole cache
        assert_eq!(SweepCache::load(&cache, "v2025").hit(&f, 42), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn counts_binary_stl_from_the_header() {
        // 80-byte header + u32 count(=2) + 2*50 bytes of triangle payload.
        let mut b = vec![0u8; 80];
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 100]);
        let d = std::env::temp_dir().join(format!("smoke_bin_{}.stl", std::process::id()));
        std::fs::write(&d, &b).unwrap();
        assert_eq!(stl_triangle_count(&d), 2);
        let _ = std::fs::remove_file(&d);
    }

    #[test]
    fn counts_ascii_stl_facets_when_size_mismatches() {
        let ascii = "solid x\n\
            facet normal 0 0 1\n outer loop\n vertex 0 0 0\n vertex 1 0 0\n vertex 0 1 0\n endloop\n endfacet\n\
            facet normal 0 0 1\n outer loop\n vertex 0 0 0\n vertex 1 0 0\n vertex 0 1 0\n endloop\n endfacet\n\
            endsolid x\n";
        let d = std::env::temp_dir().join(format!("smoke_ascii_{}.stl", std::process::id()));
        std::fs::write(&d, ascii).unwrap();
        assert_eq!(stl_triangle_count(&d), 2);
        let _ = std::fs::remove_file(&d);
    }

    #[test]
    fn missing_or_empty_file_is_zero_faces() {
        assert_eq!(stl_triangle_count(Path::new("/nonexistent/x.stl")), 0);
    }

    #[test]
    fn walker_skips_library_and_build_dirs() {
        let root = std::env::temp_dir().join(format!("smoke_walk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "cube(1);").unwrap();
        };
        mk("models/a.scad");
        mk("sub/b.scad");
        mk("scad-lib/lib.scad"); // library dir — skipped
        mk("out/gen.scad"); // build output — skipped
        mk(".hidden/c.scad"); // hidden — skipped
        let found: Vec<_> = scad_files(&root)
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(found, vec!["models/a.scad", "sub/b.scad"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
