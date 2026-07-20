//! W.3.29.1: the hotchkiss.io publish CONTRACT — the pure, transport-independent knowledge both publish
//! paths share so they can't drift: the section, the endpoint URLs, the multipart/form field names, the
//! slug mirror, and the page markdown. UNGATED (no reqwest, no web-sys), so the native reqwest client
//! (`crate::publish`) AND the wasm fetch client (fab-gui, W.3.29.2) build against the SAME strings.
//!
//! What's NOT here: JSON PARSING (serde_json is dev-only) and the transport itself — each side parses the
//! upload response its own way (reqwest `.json()` native, `JSON.parse` wasm) keyed on [`MEDIA_REF_FIELD`].
//! See [[publish-contract-hotchkiss-io]] and the `hotchkiss-io` source (the authority when publish breaks).

/// The content-tree SECTION fab publishes under: the `3d` gallery, not `projects`. Pages nest as
/// `/pages/3d/<slug>` (the browsable detail URL); `/3d` itself is the gallery index that LISTS them.
pub const SECTION: &str = "3d";

/// hotchkiss.io's default base — the URL when nothing overrides it.
pub const DEFAULT_URL: &str = "https://hotchkiss.io";

// --- multipart field names for `POST /media` ---
/// Any part WITH a filename is ingested as a file variant; the name itself is ignored server-side, but we
/// send `file` for clarity. Filenames carry the KIND (`.3mf`/`.stl`→mesh, `.scad`→source).
pub const MEDIA_FILE_FIELD: &str = "file";
/// The media item's human title (a text part, no filename).
pub const MEDIA_TITLE_FIELD: &str = "title";
/// The JSON field of the 201 manifest that carries the minted ref (NOT `media_ref`).
pub const MEDIA_REF_FIELD: &str = "ref";

// --- page write form fields (`POST`/`PUT /pages/3d/...`) ---
pub const PAGE_TITLE_FIELD: &str = "page_title";
pub const PAGE_MARKDOWN_FIELD: &str = "page_markdown";
pub const PAGE_COVER_FIELD: &str = "page_cover_media_ref";
pub const PAGE_ORDER_FIELD: &str = "page_order";

/// `POST /media` — upload one media item (mesh + source variants in one multipart request).
pub fn media_url(base: &str) -> String {
    format!("{}/media", base.trim_end_matches('/'))
}

/// `POST /pages/3d` — create a page under the section.
pub fn create_page_url(base: &str) -> String {
    format!("{}/pages/{SECTION}", base.trim_end_matches('/'))
}

/// `GET`/`PUT /pages/3d/<slug>` — the page's own endpoint (existence check + update).
pub fn page_url(base: &str, slug: &str) -> String {
    format!("{}/pages/{SECTION}/{slug}", base.trim_end_matches('/'))
}

/// The browsable detail URL to report back — same shape as [`page_url`] (there's no bare `/3d/<slug>`).
pub fn public_url(base: &str, slug: &str) -> String {
    page_url(base, slug)
}

/// URL-safe slug from a title — a byte-for-byte mirror of the server's `web::util::slug::slugify`
/// (lowercase, non-alphanumeric runs collapse to a single `-`, edges trimmed), so client and server agree
/// on a page's address for idempotent create-or-update.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in input.trim().chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.push(c);
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }
    out
}

/// Compose the page body: the description, the embedded interactive model, then a downloads list. Media is
/// referenced by `/media/<ref>` (NOT `/media/file/<…>`, which takes a re-mintable url_key) — the ref-based
/// form the server rewrites to a per-viewer embed. `model_ref` is the one item carrying the mesh + `.scad`
/// variants (so the embed shows the viewer + "Open in the slicer"); `downloads` are `(title, ref)` pairs.
pub fn compose_markdown(
    description: &str,
    model_ref: &str,
    downloads: &[(String, String)],
) -> String {
    let mut md = String::new();
    if !description.trim().is_empty() {
        md.push_str(description.trim());
        md.push_str("\n\n");
    }
    md.push_str(&format!("![Interactive preview](/media/{model_ref})\n\n"));
    if !downloads.is_empty() {
        md.push_str("## Downloads\n\n");
        for (title, r) in downloads {
            md.push_str(&format!("- [{title}](/media/{r})\n"));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_mirrors_the_server() {
        // The exact cases from hotchkiss-io's slug.rs test — the mirror must not drift. (ASCII-alphanumeric
        // only; non-ASCII letters drop — matching the server, and why we don't test `café` here.)
        assert_eq!(
            slugify("How I Make AI Write Software I Trust"),
            "how-i-make-ai-write-software-i-trust"
        );
        assert_eq!(slugify("  Hello,   World!!!  "), "hello-world");
        assert_eq!(slugify("already-a-slug"), "already-a-slug");
        assert_eq!(slugify("--Edge--"), "edge");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("Underdesk Mount v2"), "underdesk-mount-v2");
    }

    #[test]
    fn markdown_embeds_preview_and_lists_downloads() {
        let md = compose_markdown(
            "A desk mount.",
            "cover123",
            &[
                ("Full STL".into(), "full456".into()),
                ("Plates 3mf".into(), "plates789".into()),
            ],
        );
        assert!(md.starts_with("A desk mount.\n\n"));
        assert!(md.contains("![Interactive preview](/media/cover123)"));
        assert!(md.contains("## Downloads"));
        assert!(md.contains("- [Full STL](/media/full456)"));
        assert!(md.contains("- [Plates 3mf](/media/plates789)"));
    }

    #[test]
    fn markdown_without_downloads_or_description() {
        let md = compose_markdown("", "v1", &[]);
        assert_eq!(md, "![Interactive preview](/media/v1)\n\n");
    }

    #[test]
    fn urls_carry_the_3d_section_and_trim_slashes() {
        assert_eq!(media_url("https://h.io/"), "https://h.io/media");
        assert_eq!(create_page_url("https://h.io"), "https://h.io/pages/3d");
        assert_eq!(page_url("https://h.io", "x"), "https://h.io/pages/3d/x");
        assert_eq!(public_url("https://h.io", "x"), "https://h.io/pages/3d/x");
    }
}
