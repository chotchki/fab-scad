//! Publish a project to hotchkiss.io (Phase 15): upload the cover + meshes as media, then
//! create-or-update a page under the `3d` gallery (`/pages/3d/<slug>`), authenticated with an `hio_` key.
//!
//! The site's contract (mapped from the hotchkiss-io source, re-verified W.3.28.6):
//!   - Auth: `Authorization: Bearer hio_…` — the key delegates its user's role (admin), so it can
//!     hit the admin-gated mutation routes headless. `Accept: application/json` gets JSON envelopes.
//!   - Media: `POST /media` (multipart: ANY part with a filename is ingested — multiple files per
//!     request become LOD variants of ONE item; optional `title` text field) → 201 + a manifest whose
//!     minted ref is the `ref` field. Embed/reference media in markdown + `page_cover_media_ref` by the
//!     bare ref or `/media/<ref>` (NOT `/media/file/<…>`, which takes a re-mintable url_key).
//!   - Page: `POST /pages/3d` (form `page_title`, server slugifies) then
//!     `PUT /pages/3d/{slug}` (form: `page_markdown`, `page_cover_media_ref`, `page_order`, …).
//!   - No upsert route, so we derive the slug locally (mirroring the server's slugify), GET to check,
//!     create if missing, then PUT the content.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

// The transport-independent contract (W.3.29.1): endpoints, field names, slug, markdown — shared with the
// wasm fetch client so they can't drift.
use crate::publish_contract as contract;

/// Max attempts for a transient (connection / timeout / 5xx) failure — chotchki's "overload retry".
const RETRIES: u32 = 5;

/// One media file to publish + the human title it gets.
pub struct Media<'a> {
    pub path: &'a Path,
    pub title: String,
}

/// Everything needed to publish one project page.
pub struct Project<'a> {
    pub title: &'a str,
    pub description_md: &'a str,
    /// The gallery cover image. `None` = don't set one (the site keeps any existing / renders its own
    /// from the mesh) — the headless `fab publish` path, which has no 3D view to capture (W.3.28).
    pub cover_png: Option<&'a Path>,
    /// Mesh variants (LOD) — COLORED 3MF (the kernel carries `color()` into base materials). Uploaded
    /// as part of the ONE model item (with [`Self::source`]); the viewer picks the smallest 3MF, the
    /// download the largest.
    pub mesh_variants: Vec<&'a Path>,
    /// The `.scad` source, folded into the SAME model item as a variant (not a standalone download), so
    /// the embed's "Open in the slicer" button appears. `None` skips it.
    pub source: Option<&'a Path>,
    /// Extra standalone downloads (e.g. the print-plates `.3mf`), each its own media item.
    pub downloads: Vec<Media<'a>>,
}

/// The `POST /media` 201 manifest — we only need the minted ref, which the server emits as `ref`
/// (`#[serde(rename = "ref")]` on the site side); the rest of the manifest (self, kind, variants…) is
/// ignored. NOT `media_ref` — that older key never existed on this server (the dogfood 404's sibling bug).
#[derive(Deserialize)]
struct UploadResp {
    #[serde(rename = "ref")]
    media_ref: String,
}

/// An authenticated client for one hotchkiss.io instance.
pub struct Client {
    base: String,
    key: String,
    http: reqwest::blocking::Client,
}

impl Client {
    /// Client for `base` (e.g. `https://hotchkiss.io`) authing with an `hio_` API `key`.
    pub fn new(base: &str, key: &str) -> Result<Self> {
        // `Accept: application/json` on every request: the page-write endpoints fork on the client kind
        // and return a 303-to-HTML for a browser but a JSON envelope for an API client (hotchkiss-io
        // responder.rs). We're the latter — ask for JSON so status codes are unambiguous.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .default_headers(headers)
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            http,
        })
    }

    /// Send a request built fresh each attempt (multipart bodies aren't reusable), retrying with
    /// exponential backoff on TRANSIENT failure — a connection error, timeout, or 5xx. A build error
    /// (bad file/args) or a non-5xx response returns immediately; the caller checks the status.
    fn send_retry(
        &self,
        what: &str,
        build: impl Fn() -> Result<reqwest::blocking::RequestBuilder>,
    ) -> Result<reqwest::blocking::Response> {
        let mut delay = Duration::from_millis(500);
        let mut last: Option<anyhow::Error> = None;
        for attempt in 1..=RETRIES {
            let req = build()?; // a build failure is fatal (missing file / bad arg) — don't retry
            match req.send() {
                Ok(resp) if resp.status().is_server_error() => {
                    last = Some(anyhow!("{what}: server returned {}", resp.status()));
                }
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    last = Some(anyhow::Error::new(e).context(format!("{what}: request failed")))
                }
            }
            if attempt < RETRIES {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("{what}: no attempt made")))
            .with_context(|| format!("{what} failed after {RETRIES} attempts"))
    }

    /// Upload one or more files as ONE media item — the site makes a variant per file in a single
    /// request, so multiple files become LOD variants of one item (viewer picks the light one).
    /// Returns the item's `media_ref`.
    fn upload_media_multi(&self, files: &[&Path], title: &str) -> Result<String> {
        // `POST /media` (hotchkiss-io media_router): any multipart part WITH a filename is ingested; all
        // files in one request become LOD variants of ONE item. Admin-gated — the `hio_` Bearer key
        // satisfies it. Returns 201 + a manifest whose ref field is `ref` (see [`UploadResp`]).
        let url = contract::media_url(&self.base);
        let resp = self.send_retry("media upload", || {
            let mut form = reqwest::blocking::multipart::Form::new()
                .text(contract::MEDIA_TITLE_FIELD, title.to_string());
            for f in files {
                form = form
                    .file(contract::MEDIA_FILE_FIELD, f)
                    .with_context(|| format!("reading {}", f.display()))?;
            }
            Ok(self.http.post(&url).bearer_auth(&self.key).multipart(form))
        })?;
        if !resp.status().is_success() {
            bail!("media upload → {}", resp.status());
        }
        Ok(resp
            .json::<UploadResp>()
            .context("parsing upload response")?
            .media_ref)
    }

    /// Upload a single file as its own media item; returns its `media_ref`.
    fn upload_media(&self, file: &Path, title: &str) -> Result<String> {
        self.upload_media_multi(&[file], title)
    }

    /// Does a project page already exist at this slug? (GET is public; 2xx = yes.)
    fn page_exists(&self, slug: &str) -> Result<bool> {
        let url = contract::page_url(&self.base, slug);
        let resp = self.send_retry("page check", || {
            Ok(self.http.get(&url).bearer_auth(&self.key))
        })?;
        Ok(resp.status().is_success())
    }

    /// Create an (empty) page from its title; the server slugifies + nests it under the `3d` section.
    /// With `Accept: application/json` the server returns the created-page envelope (not a 303).
    fn create_page(&self, title: &str) -> Result<()> {
        let url = contract::create_page_url(&self.base);
        let resp = self.send_retry("page create", || {
            Ok(self
                .http
                .post(&url)
                .bearer_auth(&self.key)
                .form(&[(contract::PAGE_TITLE_FIELD, title)]))
        })?;
        let s = resp.status();
        if !(s.is_success() || s.is_redirection()) {
            bail!("page create '{title}' → {s}");
        }
        Ok(())
    }

    /// Write the page's markdown body + cover.
    fn update_page(
        &self,
        slug: &str,
        title: &str,
        markdown: &str,
        cover_ref: Option<&str>,
    ) -> Result<()> {
        let url = contract::page_url(&self.base, slug);
        let resp = self.send_retry("page update", || {
            Ok(self.http.put(&url).bearer_auth(&self.key).form(&[
                (contract::PAGE_TITLE_FIELD, title),
                ("page_category", ""),
                (contract::PAGE_MARKDOWN_FIELD, markdown),
                // empty = leave the cover unset (or keep whatever the site already has).
                (contract::PAGE_COVER_FIELD, cover_ref.unwrap_or("")),
                (contract::PAGE_ORDER_FIELD, "0"),
                ("page_creation_date", ""),
            ]))
        })?;
        if !resp.status().is_success() {
            bail!("page update '{slug}' → {}", resp.status());
        }
        Ok(())
    }
}

/// Publish `p` to `client`: upload the cover + viewer mesh + downloads, compose the page markdown,
/// create the page if it's new, then write its body. Returns the published page URL. Idempotent —
/// re-publishing the same title updates the existing page.
pub fn publish(client: &Client, p: &Project) -> Result<String> {
    let slug = contract::slugify(p.title);
    if slug.is_empty() {
        bail!("project title {:?} has no slug-able characters", p.title);
    }

    let cover = match p.cover_png {
        Some(png) => Some(client.upload_media(png, &format!("{} — cover", p.title))?),
        None => None,
    };
    // The model item: the mesh LOD variants AND the `.scad` source, uploaded in ONE request → ONE media
    // item. The site's embed renders the three.js viewer + download from the mesh AND — because a `.scad`
    // variant is present — appends an "Open in the slicer" button that deep-links the source into the
    // fab-gui web editor (`/3d/editor?model=/media/<ref>?format=scad`). A SEPARATE `.scad` item would be
    // invisible to that embed, so the source must ride the same item. Kinds come from the filename
    // extensions (`.3mf`/`.stl` → mesh, `.scad` → source), and identical bytes dedup by content hash.
    let mut model_files: Vec<&Path> = p.mesh_variants.clone();
    if let Some(src) = p.source {
        model_files.push(src);
    }
    let model = client.upload_media_multi(&model_files, &format!("{} — model", p.title))?;
    let mut downloads = Vec::with_capacity(p.downloads.len());
    for d in &p.downloads {
        downloads.push((d.title.clone(), client.upload_media(d.path, &d.title)?));
    }

    let markdown = contract::compose_markdown(p.description_md, &model, &downloads);
    if !client.page_exists(&slug)? {
        client.create_page(p.title)?;
    }
    client.update_page(&slug, p.title, &markdown, cover.as_deref())?;
    Ok(contract::public_url(&client.base, &slug))
}

/// Publish PRE-RENDERED artifacts — the kernel-first entry (W.3.28): the caller renders the cover +
/// mesh variants (via fab's own renderer, not OpenSCAD) to files, then hands the paths here. Builds the
/// [`Client`] + [`Project`] and uploads. `cover` is optional (headless `fab publish` has no 3D view).
#[allow(clippy::too_many_arguments)]
pub fn upload_model(
    base: &str,
    key: &str,
    title: &str,
    description: &str,
    cover: Option<&Path>,
    mesh_variants: &[&Path],
    source: Option<&Path>,
    downloads: Vec<Media<'_>>,
) -> Result<String> {
    let client = Client::new(base, key)?;
    let project = Project {
        title,
        description_md: description,
        cover_png: cover,
        mesh_variants: mesh_variants.to_vec(),
        source,
        downloads,
    };
    publish(&client, &project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_response_reads_the_ref_field() {
        // hotchkiss-io's `POST /media` 201 manifest names the minted ref `ref` (not `media_ref`) and
        // carries extra fields we ignore. This pins the deserialization — the dogfood 404's sibling bug.
        let resp: UploadResp = serde_json::from_str(
            r#"{"ref":"0191abcd","self":"/media/0191abcd","kind":"model","title":"x","variants":[]}"#,
        )
        .unwrap();
        assert_eq!(resp.media_ref, "0191abcd");
    }
}
