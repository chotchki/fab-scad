//! Web lib-closure delivery (W.3.6 Stage 2): the browser has no fs, so before rendering an
//! include/use model the app fetches the packed scad library tree (`{base}libs.json`, path->text) ONCE,
//! computes the model's include CLOSURE in-memory, and hands just that closure to the geom Worker as
//! `Source::Bytes.libs`. The pure scan/normalize/BFS is native-testable; only the fetch is wasm.
//!
//! Perf note: this sends the closure per render (a BOSL2 model's closure is most of BOSL2 — ~MBs). The
//! follow-up is a WORKER-side lib cache (fetch libs.json in the worker, resolve without re-transfer).

use std::collections::{HashMap, HashSet, VecDeque};

/// Extract `include <PATH>` / `use <PATH>` references (the `<...>` library form; the quoted form isn't
/// a library ref). A word-boundary scan — good enough for the closure walk, tolerant of `include<x>`.
fn scan_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for kw in ["include", "use"] {
        let mut from = 0;
        while let Some(rel) = text[from..].find(kw) {
            let pos = from + rel;
            from = pos + kw.len();
            // The keyword must be a whole word (not `…include` / `use_foo`).
            let before_ok = text[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if !before_ok {
                continue;
            }
            let after = text[from..].trim_start();
            if let Some(inner) = after.strip_prefix('<')
                && let Some(end) = inner.find('>')
            {
                out.push(inner[..end].trim().to_string());
            }
        }
    }
    // W.3.24: import("path") / surface("path") reference ASSET files by a QUOTED path — pull those too so
    // the closure carries the asset bytes (the worker matches by basename). Over-inclusion is harmless:
    // a non-file string just misses the pack.
    for kw in ["import", "surface"] {
        let mut from = 0;
        while let Some(rel) = text[from..].find(kw) {
            let pos = from + rel;
            from = pos + kw.len();
            let before_ok = text[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if !before_ok {
                continue;
            }
            let after = &text[from..];
            // The first quoted string before the call's `)` is the file path (import("x") / file="x").
            let close = after.find(')').unwrap_or(after.len());
            if let Some(q1) = after[..close].find('"')
                && let Some(q2) = after[q1 + 1..close].find('"')
            {
                out.push(after[q1 + 1..q1 + 1 + q2].trim().to_string());
            }
        }
    }
    out
}

/// Lexical (no-fs) path normalization — the map key a reference resolves to: drop `.`, resolve `..`.
fn normalize(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out.join("/")
}

/// The include CLOSURE of `main` through the packed lib tree — a BFS mirroring the worker's resolver
/// (from_dir-first, then lib-root). Returns the subset of `pack` the model transitively needs, as
/// `(key, bytes)` for `Source::Bytes.libs`. Missing refs are skipped (the worker tolerates them).
pub(crate) fn closure(main: &str, pack: &HashMap<String, String>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // (from_dir, raw) — the resolution base + the literal reference.
    let mut queue: VecDeque<(String, String)> = scan_refs(main)
        .into_iter()
        .map(|r| (String::new(), r))
        .collect();
    while let Some((from_dir, raw)) = queue.pop_front() {
        let rel = if from_dir.is_empty() {
            raw.clone()
        } else {
            format!("{from_dir}/{raw}")
        };
        // from_dir-relative first, then lib-root — the first present in the pack wins.
        let key = {
            let a = normalize(&rel);
            if pack.contains_key(&a) {
                a
            } else {
                normalize(&raw)
            }
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(text) = pack.get(&key) {
            let dir = key
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            for sub in scan_refs(text) {
                queue.push_back((dir.clone(), sub));
            }
            out.push((key, text.clone().into_bytes()));
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    thread_local! {
        // The packed lib tree, fetched ONCE per page load (the "pack once" choice).
        static PACK: RefCell<Option<Rc<HashMap<String, String>>>> = const { RefCell::new(None) };
    }

    async fn fetch_pack() -> Rc<HashMap<String, String>> {
        if let Some(p) = PACK.with(|p| p.borrow().clone()) {
            return p;
        }
        let map = load().await.unwrap_or_default(); // missing libs.json → empty (no-include still renders)
        let rc = Rc::new(map);
        PACK.with(|p| *p.borrow_mut() = Some(rc.clone()));
        rc
    }

    async fn load() -> Option<HashMap<String, String>> {
        let win = web_sys::window()?;
        let url = format!("{}libs.json", crate::geom_wasm::bundle_base());
        let resp = JsFuture::from(win.fetch_with_str(&url)).await.ok()?;
        let resp: web_sys::Response = resp.dyn_into().ok()?;
        if !resp.ok() {
            return None;
        }
        let text = JsFuture::from(resp.text().ok()?).await.ok()?.as_string()?;
        serde_json::from_str(&text).ok()
    }

    /// The full worker `libs` for a PROJECT render (Z.3.4): `main`'s library closure PLUS, for each file
    /// in the project `pack`, that file itself AND its OWN library closure — so a project lib that
    /// `include`s BOSL2 pulls BOSL2 too. Binary assets ride verbatim (matched by basename in the worker).
    /// The fetched lib pack is BORROWED per file (no clone), and the worker's BTreeMap dedups + lets the
    /// project keys win on collision.
    pub(crate) async fn project_libs(
        main: &str,
        pack: Vec<(String, Vec<u8>)>,
    ) -> Vec<(String, Vec<u8>)> {
        let libs = fetch_pack().await;
        let mut out = super::closure(main, libs.as_ref());
        for (name, bytes) in pack {
            match String::from_utf8(bytes.clone()) {
                Ok(text) => {
                    out.extend(super::closure(&text, libs.as_ref()));
                    out.push((name, bytes));
                }
                Err(_) => out.push((name, bytes)), // binary asset — no include scan
            }
        }
        out
    }
}
#[cfg(target_arch = "wasm32")]
pub(crate) use web::project_libs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_include_and_use_refs() {
        let src = "include <BOSL2/std.scad>\nuse <helpers.scad>;\n// include <commented.scad> still scanned\ncube(1);";
        let refs = scan_refs(src);
        assert!(refs.contains(&"BOSL2/std.scad".to_string()));
        assert!(refs.contains(&"helpers.scad".to_string()));
    }

    #[test]
    fn scan_and_closure_pull_import_assets_by_basename() {
        // W.3.24: import("../FamilyLogo.svg") / surface("hm.dat") are found by scan_refs, and the closure
        // carries the packed asset — matched by basename since normalize drops the `..` to "FamilyLogo.svg".
        let refs =
            scan_refs("linear_extrude(1) import(\"../FamilyLogo.svg\");\nsurface(\"hm.dat\");");
        assert!(
            refs.contains(&"../FamilyLogo.svg".to_string()),
            "found the import path, got {refs:?}"
        );
        assert!(
            refs.contains(&"hm.dat".to_string()),
            "found the surface path"
        );
        let mut pack = HashMap::new();
        pack.insert("FamilyLogo.svg".to_string(), "<svg/>".to_string());
        let got = closure("import(\"../FamilyLogo.svg\");", &pack);
        assert!(
            got.iter().any(|(k, _)| k == "FamilyLogo.svg"),
            "closure should carry the imported svg, got {got:?}"
        );
    }

    #[test]
    fn normalize_resolves_dot_and_dotdot() {
        assert_eq!(normalize("BOSL2/./std.scad"), "BOSL2/std.scad");
        assert_eq!(normalize("BOSL2/../foo.scad"), "foo.scad");
        assert_eq!(normalize("a/b/../c.scad"), "a/c.scad");
    }

    #[test]
    fn closure_walks_transitively_from_dir_and_lib_root() {
        // main → widget.scad (lib-root) → BOSL2/std.scad (lib-root) → vectors.scad (from_dir-relative).
        let mut pack = HashMap::new();
        pack.insert(
            "widget.scad".into(),
            "include <BOSL2/std.scad>\nmodule widget(){}".into(),
        );
        pack.insert("BOSL2/std.scad".into(), "include <vectors.scad>".into());
        pack.insert("BOSL2/vectors.scad".into(), "// leaf".into());
        pack.insert("unused.scad".into(), "// not referenced".into());

        let got: HashSet<String> = closure("include <widget.scad>;\nwidget();", &pack)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            got,
            ["widget.scad", "BOSL2/std.scad", "BOSL2/vectors.scad"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<_>>(),
            "closure pulls exactly the transitive set, not the unused file"
        );
    }

    #[test]
    fn missing_ref_is_skipped_not_fatal() {
        let pack = HashMap::new();
        assert!(closure("include <nope.scad>;", &pack).is_empty());
    }
}
