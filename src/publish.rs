//! Publish a project to hotchkiss.io (Phase 15): upload the cover + meshes as media, then
//! create-or-update a page under `/projects`, authenticated with an `hio_` API key.
//!
//! The site's contract (mapped from the hotchkiss-io source):
//!   - Auth: `Authorization: Bearer hio_…` — the key delegates its user's role (admin), so it can
//!     hit the admin mutation routes headless.
//!   - Media: `POST /admin/media/upload` (multipart: a `file` part + optional `title`) → JSON
//!     `{media_id, media_ref, markdown}`. STL is `kind=stl`, 3mf/other → `file`, PNG → image. No
//!     size limit. Content-addressed, so re-uploading the same bytes dedups.
//!   - Page: `POST /pages/projects` (form `page_title`, server slugifies) then
//!     `PUT /pages/projects/{slug}` (form: `page_markdown`, `page_cover_media_ref`, `page_order`, …).
//!   - No upsert route, so we derive the slug locally (mirroring the server's slugify), GET to check,
//!     create if missing, then PUT the content.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::openscad::Openscad;

/// Max attempts for a transient (connection / timeout / 5xx) failure — chotchki's "overload retry".
const RETRIES: u32 = 5;

/// URL-safe slug from a title — a byte-for-byte mirror of the server's `web::util::slug::slugify`
/// (lowercase, non-alphanumeric runs collapse to a single `-`, edges trimmed), so client and server
/// agree on a page's address for idempotent create-or-update.
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

/// One media file to publish + the human title it gets.
pub struct Media<'a> {
    pub path: &'a Path,
    pub title: String,
}

/// Everything needed to publish one project page.
pub struct Project<'a> {
    pub title: &'a str,
    pub description_md: &'a str,
    pub cover_png: &'a Path,
    /// Mesh variants (LOD) uploaded as ONE media item — the site makes a variant per file in a
    /// single request. Order low-res → full-res: the viewer renders the light one, the full one is
    /// the download. Both COLORED 3MF (OpenSCAD carries `color()` into base materials).
    pub mesh_variants: Vec<&'a Path>,
    /// Extra standalone downloads (e.g. the print-plates `.3mf`), each its own media item.
    pub downloads: Vec<Media<'a>>,
}

#[derive(Deserialize)]
struct UploadResp {
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
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
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
        let url = format!("{}/admin/media/upload", self.base);
        let resp = self.send_retry("media upload", || {
            let mut form =
                reqwest::blocking::multipart::Form::new().text("title", title.to_string());
            for f in files {
                form = form
                    .file("file", f)
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
        let url = format!("{}/pages/projects/{}", self.base, slug);
        let resp = self.send_retry("page check", || {
            Ok(self.http.get(&url).bearer_auth(&self.key))
        })?;
        Ok(resp.status().is_success())
    }

    /// Create an (empty) project page from its title; the server slugifies + nests it under
    /// `/projects`. reqwest follows the create redirect to the edit page.
    fn create_page(&self, title: &str) -> Result<()> {
        let url = format!("{}/pages/projects", self.base);
        let resp = self.send_retry("page create", || {
            Ok(self
                .http
                .post(&url)
                .bearer_auth(&self.key)
                .form(&[("page_title", title)]))
        })?;
        let s = resp.status();
        if !(s.is_success() || s.is_redirection()) {
            bail!("page create '{title}' → {s}");
        }
        Ok(())
    }

    /// Write the page's markdown body + cover.
    fn update_page(&self, slug: &str, title: &str, markdown: &str, cover_ref: &str) -> Result<()> {
        let url = format!("{}/pages/projects/{}", self.base, slug);
        let resp = self.send_retry("page update", || {
            Ok(self.http.put(&url).bearer_auth(&self.key).form(&[
                ("page_title", title),
                ("page_category", ""),
                ("page_markdown", markdown),
                ("page_cover_media_ref", cover_ref),
                ("page_order", "0"),
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
    let slug = slugify(p.title);
    if slug.is_empty() {
        bail!("project title {:?} has no slug-able characters", p.title);
    }

    let cover = client.upload_media(p.cover_png, &format!("{} — cover", p.title))?;
    // The mesh: all LOD variants in ONE request → one item (viewer renders the light variant, the
    // full one downloads). Uploading them separately would make them unrelated items.
    let model = client.upload_media_multi(&p.mesh_variants, &format!("{} — model", p.title))?;
    let mut downloads = Vec::with_capacity(p.downloads.len());
    for d in &p.downloads {
        downloads.push((d.title.clone(), client.upload_media(d.path, &d.title)?));
    }

    let markdown = compose_markdown(p.description_md, &model, &downloads);
    if !client.page_exists(&slug)? {
        client.create_page(p.title)?;
    }
    client.update_page(&slug, p.title, &markdown, &cover)?;
    Ok(format!("{}/pages/projects/{}", client.base, slug))
}

/// Render the publish artifacts for `target` and publish them: a cover thumbnail, the full-res STL,
/// and a low-`$fn` PREVIEW mesh (forced via a `$preview = true` include wrapper so the source's
/// `$fn = $preview ? low : high` takes the light path — a mesh a browser viewer can handle). Gathers
/// downloads (the full STL + a `<stem>-plates.3mf` if `fab make` left one beside the model), then
/// creates/updates the project page. Shared by the CLI (`fab publish`) and the GUI Publish button.
#[allow(clippy::too_many_arguments)] // CLI/GUI shared entry — args mirror the publish form fields
pub fn publish_model(
    oscad: &Openscad,
    target: &Path,
    title: &str,
    description: &str,
    base: &str,
    key: &str,
    out_dir: &Path,
    timeout: Duration,
) -> Result<String> {
    std::fs::create_dir_all(out_dir)?;
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    let cover = out_dir.join(format!("{stem}.png"));
    if !oscad.thumbnail(target, &cover, (1200, 900), timeout)?.ok {
        bail!("cover render failed");
    }
    // Full-res download as 3MF too — carries color, and lighter than a raw STL.
    let full = out_dir.join(format!("{stem}.3mf"));
    if !oscad.render(target, &full, timeout)?.ok {
        bail!("mesh render failed");
    }
    // A COLORED 3MF, not a flat STL: OpenSCAD's 3MF export carries the model's `color()` as base
    // materials, so the site viewer shows the real colors. Still low-`$fn` (the `$preview` wrapper).
    let viewer = out_dir.join(format!("{stem}-preview.3mf"));
    let wrapper = out_dir.join(format!("{stem}-preview.scad"));
    let abs = target
        .canonicalize()
        .with_context(|| format!("resolving {}", target.display()))?;
    std::fs::write(
        &wrapper,
        format!("$preview = true;\ninclude <{}>;\n", abs.display()),
    )?;
    if !oscad.render(&wrapper, &viewer, timeout)?.ok {
        bail!("preview render failed");
    }

    // Standalone downloads (its own item each): the print-plates .3mf, if `fab make` left one.
    let mut downloads = Vec::new();
    let plates = target.with_file_name(format!("{stem}-plates.3mf"));
    if plates.exists() {
        downloads.push(Media {
            path: &plates,
            title: format!("{title} — print plates (.3mf)"),
        });
    }

    let client = Client::new(base, key)?;
    let project = Project {
        title,
        description_md: description,
        cover_png: &cover,
        // low-res (viewer) first, then full-res (download) — one item, LOD variants.
        mesh_variants: vec![&viewer, &full],
        downloads,
    };
    publish(&client, &project)
}

/// Compose the page body: the description, the embedded interactive preview mesh, then a downloads
/// list. Media is referenced by `/media/<ref>` — the server swaps each embed at render time.
fn compose_markdown(description: &str, viewer_ref: &str, downloads: &[(String, String)]) -> String {
    let mut md = String::new();
    if !description.trim().is_empty() {
        md.push_str(description.trim());
        md.push_str("\n\n");
    }
    md.push_str(&format!(
        "![Interactive preview](/media/file/{viewer_ref})\n\n"
    ));
    if !downloads.is_empty() {
        md.push_str("## Downloads\n\n");
        for (title, r) in downloads {
            md.push_str(&format!("- [{title}](/media/file/{r})\n"));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_mirrors_the_server() {
        // The exact cases from hotchkiss-io's slug.rs test — the mirror must not drift.
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
        assert!(md.contains("![Interactive preview](/media/file/cover123)"));
        assert!(md.contains("## Downloads"));
        assert!(md.contains("- [Full STL](/media/file/full456)"));
        assert!(md.contains("- [Plates 3mf](/media/file/plates789)"));
    }

    #[test]
    fn markdown_without_downloads_or_description() {
        let md = compose_markdown("", "v1", &[]);
        assert_eq!(md, "![Interactive preview](/media/file/v1)\n\n");
    }
}
