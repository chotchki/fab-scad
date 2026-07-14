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

    /// Fetch the pack (once) and compute `main`'s include closure — the bytes the Worker needs.
    pub(crate) async fn lib_closure(main: &str) -> Vec<(String, Vec<u8>)> {
        let pack = fetch_pack().await;
        super::closure(main, pack.as_ref())
    }
}
#[cfg(target_arch = "wasm32")]
pub(crate) use web::lib_closure;

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
    fn normalize_resolves_dot_and_dotdot() {
        assert_eq!(normalize("BOSL2/./std.scad"), "BOSL2/std.scad");
        assert_eq!(normalize("BOSL2/../foo.scad"), "foo.scad");
        assert_eq!(normalize("a/b/../c.scad"), "a/c.scad");
    }

    #[test]
    fn closure_walks_transitively_from_dir_and_lib_root() {
        // main → widget.scad (lib-root) → BOSL2/std.scad (lib-root) → vectors.scad (from_dir-relative).
        let mut pack = HashMap::new();
        pack.insert("widget.scad".into(), "include <BOSL2/std.scad>\nmodule widget(){}".into());
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
