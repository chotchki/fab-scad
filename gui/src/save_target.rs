//! Derive the round-trip SAVE target from the `?model=` deep-link (W.5, hotchkiss-io `media-design.md`
//! §10). The site's "Open in the slicer" button links `?model=/media/<ref>?format=scad` — the stable
//! `media_ref` rides the model URL's PATH. Per the shipped contract the SAVE target is that item's
//! variant collection, derived by DROPPING THE QUERY and appending `/variants`:
//! `PUT /media/<ref>/variants` (a COMPLETE variant-set replace, DQ.1). One `?model=` drives both load
//! and save — no separate `?ref=` param, and no `data-media-base` guess: the site OWNS the URL, we
//! follow the one it handed us (HATEOAS; the OPTIONS manifest's `replace-all` control is the equivalent
//! discovery, without the extra round-trip). A `?model=` that ISN'T a single media item (an external
//! `.scad`, a `/media/file/<url_key>` byte URL, an already-a-collection URL) yields None → no Save
//! affordance, so the app never dangles a write against a URL that isn't an item.

/// The `PUT …/variants` save target for a `?model=` value, or None when it isn't a saveable media
/// item. Strips the query (`?format=scad`) + fragment, then requires the path to END in
/// `/media/<ref>` — a single `<ref>` segment that ISN'T the `file` byte-route — and appends
/// `/variants` to exactly that. A path prefix (`/x/media/<ref>`) or an absolute same-origin URL
/// survives untouched, so a prefixed deploy needs no config. Already-a-collection (`/media/<ref>/variants`)
/// and byte URLs (`/media/file/<key>`) fall through to None (never double-append, never write bytes).
pub(crate) fn derive(model_url: &str) -> Option<String> {
    // The item resource is the bare path — drop the query and any fragment.
    let path = model_url
        .split(['?', '#'])
        .next()
        .unwrap_or(model_url)
        .trim_end_matches('/');
    // The last two non-empty path segments must be `media` / `<ref>` (the item), never `file` /
    // `<url_key>` (bytes) or `<ref>` / `variants` (already the collection).
    let mut tail = path.rsplitn(3, '/');
    let last = tail.next().unwrap_or("");
    let prev = tail.next().unwrap_or("");
    (prev == "media" && !last.is_empty() && last != "file").then(|| format!("{path}/variants"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_variants_target_dropping_the_format_query() {
        // The exact shape the slicer button emits (media-design.md §10).
        assert_eq!(
            derive("/media/0198f0deadbeef?format=scad").as_deref(),
            Some("/media/0198f0deadbeef/variants"),
        );
        // No query is fine too.
        assert_eq!(
            derive("/media/abc123").as_deref(),
            Some("/media/abc123/variants"),
        );
    }

    #[test]
    fn survives_a_path_prefix_and_an_absolute_same_origin_url() {
        // A path-prefixed deploy carries the prefix in `?model=` — no `data-media-base` needed.
        assert_eq!(
            derive("/app/media/xyz?format=scad").as_deref(),
            Some("/app/media/xyz/variants"),
        );
        assert_eq!(
            derive("https://hotchkiss.io/media/xyz").as_deref(),
            Some("https://hotchkiss.io/media/xyz/variants"),
        );
    }

    #[test]
    fn refuses_non_item_model_urls() {
        // A byte URL (`/media/file/<url_key>`) is NOT the item — writing there is nonsense.
        assert_eq!(derive("/media/file/deadbeefdeadbeef"), None);
        // Already the collection — don't double-append.
        assert_eq!(derive("/media/abc/variants"), None);
        // An external `.scad` (the generic W.3.12 load) has no item to save back to.
        assert_eq!(derive("https://example.com/thing.scad"), None);
        assert_eq!(derive("model.scad"), None);
        // `/media/` with no ref segment.
        assert_eq!(derive("/media/"), None);
        assert_eq!(derive("/media"), None);
    }
}
