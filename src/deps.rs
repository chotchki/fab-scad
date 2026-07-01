//! The scad include/use dependency DAG (track B). OpenSCAD has no build graph — a model that
//! `include`s a shared module rebuilds only when YOU remember to. This resolves the real graph so a
//! front-end can watch (or content-hash) the transitive closure and rebuild whatever an edit touches.
//!
//! Resolution mirrors OpenSCAD's own: `include <name>` / `use <name>` search the including file's
//! directory FIRST, then each OPENSCADPATH entry in order. `use` and `include` differ semantically
//! (use imports only modules/functions) but pull the same file, so the graph treats them alike.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The `<name>` targets of every `include`/`use` directive in `src`, in source order (unresolved —
/// still just the bracketed names). Comments and string literals are skipped so a commented-out or
/// quoted `include` never registers. Duplicates are kept; the closure dedups by resolved path.
pub fn parse_deps(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // line comment
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // block comment
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // string literal (skip; scad paths use <> not "", but a string could contain 'include')
        if c == '"' {
            i += 1;
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        // `include <...>` / `use <...>` as whole words
        if let Some(kw_len) = kw_at(&chars, i) {
            let mut j = i + kw_len;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == '<' {
                let start = j + 1;
                let mut k = start;
                while k < n && chars[k] != '>' {
                    k += 1;
                }
                if k < n {
                    let name: String = chars[start..k].iter().collect();
                    let name = name.trim();
                    if !name.is_empty() {
                        out.push(name.to_string());
                    }
                    i = k + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// If `include` or `use` starts a whole word at `i`, its length; else None. Word boundaries keep
/// `included` / `refuse` from matching, and the char after must not continue an identifier.
fn kw_at(chars: &[char], i: usize) -> Option<usize> {
    for kw in ["include", "use"] {
        let k: Vec<char> = kw.chars().collect();
        if i + k.len() > chars.len() {
            continue;
        }
        if !chars[i..i + k.len()].iter().eq(k.iter()) {
            continue;
        }
        if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
            continue;
        }
        let after = i + k.len();
        if after < chars.len() && (chars[after].is_alphanumeric() || chars[after] == '_') {
            continue;
        }
        return Some(k.len());
    }
    None
}

/// Resolve a bracketed `<name>` to a file, OpenSCAD-style: the including file's `from_dir` first,
/// then each `search` (OPENSCADPATH) dir in order. Canonicalised so the closure dedups symlinks and
/// `../` paths. None if it resolves nowhere (a missing/third-party dep we simply don't follow).
pub fn resolve(name: &str, from_dir: &Path, search: &[PathBuf]) -> Option<PathBuf> {
    std::iter::once(from_dir.to_path_buf())
        .chain(search.iter().cloned())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

/// Every file `entry` depends on, transitively, INCLUDING `entry` itself — resolved + canonicalised.
/// Cycle-safe (a file already seen is never re-read) and missing-dep-tolerant (an unresolved include
/// is skipped, not fatal). This is the set to watch / content-hash to know when a render is stale.
pub fn closure(entry: &Path, search: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut stack = Vec::new();
    if let Ok(e) = entry.canonicalize() {
        stack.push(e);
    }
    while let Some(path) = stack.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        for name in parse_deps(&src) {
            if let Some(dep) = resolve(&name, dir, search) {
                if !seen.contains(&dep) {
                    stack.push(dep);
                }
            }
        }
    }
    seen
}

/// A content hash of `entry`'s WHOLE include closure — it changes iff any file in the graph changes.
/// The key for incremental rebuild (6.2): same hash ⇒ same inputs ⇒ the last render is still valid.
/// Hashes each file's canonical path + bytes in the closure's sorted order, so it's stable per run.
pub fn content_hash(entry: &Path, search: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in closure(entry, search) {
        path.hash(&mut h);
        match std::fs::read(&path) {
            Ok(bytes) => bytes.hash(&mut h),
            Err(_) => 0u8.hash(&mut h), // unreadable — fold in a marker so it still differs
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_include_and_use_skipping_comments_and_strings() {
        let src = "\
            include <BOSL2/std.scad>\n\
            use <../lib/helpers.scad>;\n\
            // include <commented.scad>\n\
            /* use <blocked.scad> */\n\
            x = \"include <string.scad>\";\n\
            used_var = 1; // 'use' as a prefix must NOT match\n\
            module included() {}   // nor 'include' as a prefix\n\
            include<nospace.scad>\n";
        assert_eq!(
            parse_deps(src),
            vec![
                "BOSL2/std.scad".to_string(),
                "../lib/helpers.scad".to_string(),
                "nospace.scad".to_string(),
            ]
        );
    }

    #[test]
    fn parse_ignores_word_boundary_false_positives() {
        assert!(parse_deps("refuse <x.scad>").is_empty()); // 'use' inside 'refuse'
        assert!(parse_deps("reincluded <x.scad>").is_empty());
        assert_eq!(parse_deps("use\t<x.scad>"), vec!["x.scad"]); // tab between keyword and bracket
    }

    // A scratch dir unique to a test, populated then torn down.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("fab_deps_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Scratch(d)
        }
        fn write(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_prefers_local_dir_then_search_path() {
        let s = Scratch::new("resolve");
        let libdir = s.0.join("libs");
        s.write("libs/shared.scad", "// shared");
        s.write("proj/local.scad", "// local");
        let from = s.0.join("proj");
        // a name only on the search path resolves there
        assert_eq!(
            resolve("shared.scad", &from, &[libdir.clone()]),
            Some(libdir.join("shared.scad").canonicalize().unwrap())
        );
        // a name in the from_dir wins over the search path
        s.write("libs/local.scad", "// decoy on the search path");
        assert_eq!(
            resolve("local.scad", &from, &[libdir]),
            Some(from.join("local.scad").canonicalize().unwrap())
        );
        // unresolved -> None
        assert_eq!(resolve("nope.scad", &from, &[]), None);
    }

    #[test]
    fn closure_is_transitive_and_cycle_safe() {
        let s = Scratch::new("closure");
        // a -> b -> c, and c -> a (cycle). plus b -> a shared lib on the search path.
        let a = s.write("a.scad", "include <b.scad>\nuse <libs/shared.scad>;\n");
        s.write("b.scad", "include <c.scad>\n");
        s.write("c.scad", "include <a.scad>\n"); // back-edge
        s.write("libs/shared.scad", "// leaf\n");
        let search = vec![s.0.clone()]; // so <libs/shared.scad> resolves from the search path too
        let cl = closure(&a, &search);
        let canon = |r: &str| s.0.join(r).canonicalize().unwrap();
        assert!(cl.contains(&canon("a.scad")), "entry itself is in the closure");
        assert!(cl.contains(&canon("b.scad")));
        assert!(cl.contains(&canon("c.scad")));
        assert!(cl.contains(&canon("libs/shared.scad")));
        assert_eq!(cl.len(), 4, "cycle visited once, no dup: {cl:?}");
    }

    #[test]
    fn content_hash_tracks_the_whole_closure() {
        let s = Scratch::new("hash");
        let a = s.write("a.scad", "include <b.scad>\ncube(1);\n");
        s.write("b.scad", "module m() { cube(2); }\n");
        let h0 = content_hash(&a, &[]);
        assert_eq!(h0, content_hash(&a, &[]), "stable when nothing changes");
        // editing the INCLUDED file changes the hash (that's the whole point)
        s.write("b.scad", "module m() { cube(3); }\n");
        assert_ne!(h0, content_hash(&a, &[]), "an included edit invalidates");
    }

    #[test]
    fn closure_tolerates_a_missing_dependency() {
        let s = Scratch::new("missing");
        let a = s.write("a.scad", "include <gone.scad>\ninclude <real.scad>\n");
        s.write("real.scad", "// here\n");
        let cl = closure(&a, &[]);
        assert_eq!(cl.len(), 2); // a + real; the missing one is skipped, not fatal
    }
}
