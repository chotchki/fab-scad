//! Name a web-loaded model (Z.3.9). A `?model=` deep-link points at an item, not a file: the site's
//! "Open in the editor" button links `?model=/media/<ref>?format=project`, whose last path segment is
//! an opaque 64-hex `media_ref`. Naming the document from that basename put the HASH on the panel
//! header, the status line, the Save download, and — via the publish/save-back stem — inside the
//! published `.scadproj` itself, where the next load read it back out (the Z.5 dogfood finding).
//!
//! The real name already rides the response: the byte route sets
//! `Content-Disposition: inline; filename="<Item title>.scadproj"` (hotchkiss-io Phase EE), and a
//! same-origin fetch can read it off the final response after the 307. So the fallback chain is
//! **header -> URL basename -> `model.scad`**: a cross-origin model host that doesn't expose the
//! header (needs `Access-Control-Expose-Headers`) degrades to exactly the old behavior instead of
//! breaking.
//!
//! Pure string logic, deliberately NOT wasm-gated — the whole chain unit-tests on the native target.

/// The suffix `publish_web` appends to the MODEL item's title (`format!("{title} — model")`, so the
/// three items a publish mints stay distinguishable in the gallery), in both the em-dash original and
/// the ASCII form the site's filename sanitizer leaves behind. We own BOTH ends of that convention, so
/// the header parse undoes it — otherwise every web-loaded model is called `Thing - model.scad` forever.
const PUBLISH_MODEL_SUFFIXES: [&str; 2] = [" — model", " - model"];

/// The document name for a `?model=` deep-link: the response's `Content-Disposition` filename when
/// there is a usable one, else the URL's basename (query + fragment stripped), else `model.scad`.
pub(crate) fn model_name(disposition: Option<&str>, url: &str) -> String {
    disposition
        .and_then(disposition_filename)
        .map(|n| strip_publish_suffix(&n))
        .or_else(|| url_basename(url))
        .unwrap_or_else(|| "model.scad".to_string())
}

/// Undo [`PUBLISH_MODEL_SUFFIXES`] on the NAME STEM (the extension is ours, not the title's). A title
/// that is nothing BUT the suffix is left alone — stripping it would leave no name at all.
fn strip_publish_suffix(name: &str) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (name, None),
    };
    let stripped = PUBLISH_MODEL_SUFFIXES
        .iter()
        .find_map(|s| stem.strip_suffix(s))
        .unwrap_or(stem)
        .trim_end();
    if stripped.is_empty() {
        return name.to_string();
    }
    match ext {
        Some(e) => format!("{stripped}.{e}"),
        None => stripped.to_string(),
    }
}

/// The `filename=` token from a `Content-Disposition` value, sanitized to a bare filename.
///
/// Params split on `;`, and the key must match `filename` EXACTLY — `filename*=` (RFC 5987) is a
/// different parameter and a naive `starts_with("filename")` would swallow its percent-encoded
/// `UTF-8''…` value. We don't parse `filename*` at all: the site sanitizes titles to ASCII before
/// quoting, so plain `filename=` always carries the name. Quotes are stripped; a value carrying path
/// separators is cut to its last segment and pure-dot names are rejected, so a hostile header can
/// never steer a download or a project entry out of its directory.
fn disposition_filename(value: &str) -> Option<String> {
    let raw = value.split(';').skip(1).find_map(|param| {
        let (k, v) = param.split_once('=')?;
        k.trim().eq_ignore_ascii_case("filename").then_some(v)
    })?;
    sanitize(raw.trim().trim_matches('"'))
}

/// The last path segment of a URL, with the query and fragment dropped — the pre-Z.3.9 derivation,
/// kept as the fallback for a model host that doesn't hand us a `Content-Disposition`.
fn url_basename(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    sanitize(path.rsplit('/').next().unwrap_or(path))
}

/// A bare, non-empty filename or nothing: cut to the last path segment (either separator), then
/// reject the empty / all-dots cases (`""`, `.`, `..`) that aren't names at all.
fn sanitize(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    (!base.trim_matches('.').is_empty()).then(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped header shape (hotchkiss-io `download_filename`) wins over the URL — the bug this
    /// module exists to fix, in both the project and the plain-scad flavor.
    #[test]
    fn the_disposition_filename_beats_the_hash_basename() {
        let url = "/media/019f81dd2c3b72839333a1b5ec961d64?format=project";
        assert_eq!(
            model_name(Some("inline; filename=\"Shower Holder.scadproj\""), url),
            "Shower Holder.scadproj"
        );
        assert_eq!(
            model_name(Some("inline; filename=\"Shower Holder.scad\""), url),
            "Shower Holder.scad"
        );
        // Active content is force-downloaded — same filename token, different disposition type.
        assert_eq!(
            model_name(Some("attachment; filename=\"Thing.scad\""), url),
            "Thing.scad"
        );
    }

    /// No header (cross-origin without `Access-Control-Expose-Headers`, or a plain static host) is
    /// exactly the pre-Z.3.9 behavior — degraded, never broken.
    #[test]
    fn falls_back_to_the_url_basename_then_to_model_scad() {
        assert_eq!(
            model_name(None, "https://example.com/parts/hook.scad"),
            "hook.scad"
        );
        // The query is not part of the name (the deep-link carries `?format=`).
        assert_eq!(
            model_name(None, "/media/019f81dd?format=project"),
            "019f81dd"
        );
        assert_eq!(model_name(None, "/models/x.scad#frag"), "x.scad");
        // Nothing name-able anywhere → the generic default.
        assert_eq!(model_name(None, "https://example.com/"), "model.scad");
        assert_eq!(model_name(None, ""), "model.scad");
        assert_eq!(model_name(Some("inline"), ""), "model.scad");
    }

    /// `filename*=` is a DIFFERENT parameter (RFC 5987, percent-encoded) — matching the key loosely
    /// would name the document `UTF-8''Shower%20Holder.scadproj`. We ignore it and take `filename=`.
    #[test]
    fn ignores_rfc5987_filename_star_and_unrelated_params() {
        assert_eq!(
            model_name(
                Some(
                    "inline; filename*=UTF-8''Shower%20Holder.scadproj; filename=\"Shower.scadproj\""
                ),
                "/media/deadbeef"
            ),
            "Shower.scadproj"
        );
        // filename* ALONE is unparsed → fall through to the URL, not to a percent-encoded mess.
        assert_eq!(
            model_name(
                Some("inline; filename*=UTF-8''Shower%20Holder.scadproj"),
                "/media/deadbeef"
            ),
            "deadbeef"
        );
        // The disposition TYPE is never a filename, even though it precedes the first `;`.
        assert_eq!(
            model_name(Some("attachment; size=12"), "/media/deadbeef"),
            "deadbeef"
        );
    }

    /// A header is attacker-influenceable in the general case (any `?model=` host) and its value
    /// lands in a download filename + a `.scadproj` entry name — so a path never survives it.
    #[test]
    fn strips_paths_and_refuses_dot_names() {
        assert_eq!(
            model_name(Some("inline; filename=\"../../etc/passwd\""), "/media/ref"),
            "passwd"
        );
        assert_eq!(
            model_name(Some("inline; filename=\"a\\\\b\\\\c.scad\""), "/media/ref"),
            "c.scad"
        );
        // Dot-only names aren't names — fall through to the URL basename.
        assert_eq!(
            model_name(Some("inline; filename=\"..\""), "/media/ref"),
            "ref"
        );
        assert_eq!(
            model_name(Some("inline; filename=\"\""), "/media/ref"),
            "ref"
        );
        assert_eq!(
            model_name(Some("inline; filename=\"   \""), "/media/ref"),
            "ref"
        );
    }

    /// A published model's item title is `"{title} — model"` (publish_web.rs), which the site's
    /// filename sanitizer flattens to `- model` — undo it so the round-trip is lossless.
    #[test]
    fn undoes_the_publish_model_title_suffix() {
        let url = "/media/deadbeef?format=project";
        assert_eq!(
            model_name(
                Some("inline; filename=\"Shower Holder - model.scadproj\""),
                url
            ),
            "Shower Holder.scadproj"
        );
        // The em-dash form too, in case a host serves the unsanitized title.
        assert_eq!(
            model_name(Some("inline; filename=\"Shower Holder — model.scad\""), url),
            "Shower Holder.scad"
        );
        // Only a TRAILING stem suffix — a model named "model" or "- model kit" keeps its name.
        assert_eq!(
            model_name(Some("inline; filename=\"model.scad\""), url),
            "model.scad"
        );
        assert_eq!(
            model_name(Some("inline; filename=\"Rocket - model kit.scad\""), url),
            "Rocket - model kit.scad"
        );
        // Stripping must never leave an empty name.
        assert_eq!(
            model_name(Some("inline; filename=\"- model.scad\""), url),
            "- model.scad"
        );
        // The plate/cover items carry different suffixes — untouched (they're never `?model=` targets).
        assert_eq!(
            model_name(Some("inline; filename=\"Shower Holder - cover.png\""), url),
            "Shower Holder - cover.png"
        );
    }

    /// Unquoted values are legal (RFC 6266 token form) and whitespace around `=` is common.
    #[test]
    fn accepts_unquoted_and_loosely_spaced_values() {
        assert_eq!(
            model_name(Some("inline; filename=hook.scad"), "/media/ref"),
            "hook.scad"
        );
        assert_eq!(
            model_name(Some("inline;  FileName = \"Hook.scadproj\" "), "/media/ref"),
            "Hook.scadproj"
        );
    }
}
