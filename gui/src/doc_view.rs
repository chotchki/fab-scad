//! TG.7: what the GUI SAYS about the open document — the Project tab's card, the desktop window title,
//! the header chip's hover and the Model tab's breadcrumb. Pure text from the document and its Save
//! route; `panel_ui` and `sync_window_title` only lay it out. Cfg-free and table-tested on native (the
//! `file_ops` doctrine: wasm has no test harness). Every string is ASCII plus gui/CLAUDE.md's known-safe
//! set, and a test audits that, because a raw glyph here renders as tofu.

use std::path::Path;

use crate::file_ops::tilde;
use crate::project::{ProjectDoc, ProjectHome};
use crate::save::{SavePlan, doc_name};

/// The Project tab's lead card (design Decision 6): who the document is, where it lives, and what Save
/// does to it. The header chip shows `name` with `full` on hover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocCard {
    /// `bracket.scadproj`, a loose document's file, `untitled`, or the web item's name — what the
    /// window title and the status line call it too ([`doc_name`]).
    pub(crate) name: String,
    /// Place and kind: `~/models · .scadproj`, `~/models/brace · loose folder`, `not saved yet`,
    /// `on hotchkiss.io`.
    pub(crate) place: String,
    /// One sentence on what Save does here, from the same route Save takes ([`save_route`]).
    ///
    /// [`save_route`]: crate::save::save_route
    pub(crate) save_rule: String,
    /// The whole location, un-abbreviated: the header chip's hover.
    pub(crate) full: String,
}

/// The card for `doc`. `plan` is the route Save takes (`save::save_route`, so the sentence can't
/// promise what the button doesn't do); `on_site` = a hotchkiss.io item with a save-back target;
/// `home` folds the user's home folder to `~`.
pub(crate) fn doc_card(
    doc: &ProjectDoc,
    plan: &SavePlan,
    on_site: bool,
    home: Option<&Path>,
) -> DocCard {
    let name = doc_name(doc);
    let dir_of = |p: &Path| p.parent().unwrap_or(p).to_path_buf();
    let (place, full) = match &doc.home {
        ProjectHome::ScadProj(p) => (
            format!("{} · .scadproj", tilde(&dir_of(p), home)),
            p.display().to_string(),
        ),
        // A one-file loose document is a file; past that it's the folder the files share.
        ProjectHome::ScadFile(p) => {
            let kind = if doc.is_multifile() {
                "loose folder"
            } else {
                ".scad file"
            };
            (
                format!("{} · {kind}", tilde(&dir_of(p), home)),
                p.display().to_string(),
            )
        }
        ProjectHome::Fresh => ("not saved yet".into(), "not saved yet".into()),
        ProjectHome::WebModel(_) if on_site => ("on hotchkiss.io".into(), "on hotchkiss.io".into()),
        ProjectHome::WebModel(_) => ("from the web".into(), "from the web".into()),
    };
    let save_rule = match plan {
        // What Save writes, never what the archive IS — the old hint here was zip trivia.
        SavePlan::Rezip(_) => format!("Save rewrites {name}: every file, asset and the cut plan"),
        // Decision 7: say which file ops already hit the folder and which never will.
        SavePlan::WriteLoose(_) if doc.is_multifile() => "Save writes edited files back into this \
            folder — Add and Rename change it at once, Delete only drops a file from this session"
            .into(),
        // The file Save writes (the entry), not the home's name: the two part after a rename into a
        // subfolder, which the home can't follow (`ProjectDoc::follow_loose_home`).
        SavePlan::WriteLoose(_) => format!(
            "Save writes your edits and the cut plan to {}",
            doc.entry_name()
        ),
        SavePlan::NeedsSaveAs => "Save asks where to save it".into(),
        // The site button lives on the Model tab, and the card is on the Project tab: say where.
        SavePlan::Download(file) if on_site => format!(
            "Save downloads {file} · Save to hotchkiss.io, on the Model tab, updates the item"
        ),
        SavePlan::Download(file) => format!("Save downloads {file}"),
        SavePlan::Site => "Save updates this item on hotchkiss.io".into(),
    };
    DocCard {
        name,
        place,
        save_rule,
        full,
    }
}

/// The desktop window title: `<document> — fab-scad`, `<document> (unsaved) — fab-scad` while
/// unsaved, `untitled — fab-scad` with no file.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the web host page owns <title>
pub(crate) fn window_title(doc: &ProjectDoc, unsaved: bool) -> String {
    let mark = if unsaved { " (unsaved)" } else { "" };
    format!("{}{mark} — fab-scad", doc_name(doc))
}

/// The Model tab's breadcrumb: the file in the editor, the document it's in, and — when that file
/// isn't the entry — which file renders: `hook.scad · in bracket.scadproj · main.scad renders`. The
/// "in" part drops where it would only repeat the file (a one-file loose `.scad` IS the document).
pub(crate) fn breadcrumb(doc: &ProjectDoc) -> String {
    let file = doc
        .files
        .get(doc.active)
        .map(|f| f.name.as_str())
        .unwrap_or_else(|| doc.entry_name());
    let container = match &doc.home {
        ProjectHome::ScadProj(_) => Some(doc_name(doc)),
        ProjectHome::ScadFile(p) if doc.is_multifile() => p
            .parent()
            .and_then(|d| d.file_name())
            .map(|d| format!("the {} folder", d.to_string_lossy())),
        ProjectHome::ScadFile(_) => None,
        ProjectHome::Fresh if doc.is_multifile() => Some(doc_name(doc)),
        ProjectHome::Fresh => None,
        ProjectHome::WebModel(n) => (n != file).then(|| n.clone()),
    };
    let mut parts = vec![file.to_string()];
    if let Some(c) = container {
        parts.push(format!("in {c}"));
    }
    if doc.active != doc.entry {
        parts.push(format!("{} renders", doc.entry_name()));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::plan;
    use crate::state::Platform;
    use std::path::PathBuf;

    const HOME: &str = "/Users/me";

    fn home() -> Option<&'static Path> {
        Some(Path::new(HOME))
    }

    /// `files` as a document homed at `home`, `entry` rendering, `active` in the editor.
    fn doc(home: ProjectHome, files: &[&str], entry: usize, active: usize) -> ProjectDoc {
        let mut d = ProjectDoc::single(files[0], "", home);
        for f in &files[1..] {
            d.add_file(f, String::new());
        }
        d.entry = entry;
        d.active = active;
        d
    }

    fn proj() -> ProjectHome {
        ProjectHome::ScadProj(PathBuf::from(format!("{HOME}/models/bracket.scadproj")))
    }

    fn loose() -> ProjectHome {
        ProjectHome::ScadFile(PathBuf::from(format!("{HOME}/models/brace/main.scad")))
    }

    /// The card a desktop shows (Save's desktop route), or the web's with `on_site`.
    fn card(d: &ProjectDoc, platform: Platform, on_site: bool) -> DocCard {
        doc_card(
            d,
            &crate::save::save_route(d, platform, on_site),
            on_site,
            home(),
        )
    }

    /// Only ASCII and gui/CLAUDE.md's known-safe set — anything else is tofu in egui's font stack.
    fn glyph_safe(s: &str) -> bool {
        s.chars()
            .all(|c| c.is_ascii() || matches!(c, '·' | '…' | '—' | '×' | '°'))
    }

    /// TG.7: the card per home — name, place · kind, and the Save rule from the route Save takes.
    #[test]
    fn card_per_home() {
        let d = Platform::Desktop;
        let c = card(&doc(proj(), &["main.scad", "hook.scad"], 0, 0), d, false);
        assert_eq!(c.name, "bracket.scadproj");
        assert_eq!(c.place, "~/models · .scadproj");
        assert_eq!(
            c.save_rule,
            "Save rewrites bracket.scadproj: every file, asset and the cut plan"
        );
        assert_eq!(c.full, format!("{HOME}/models/bracket.scadproj"));

        let c = card(&doc(loose(), &["main.scad", "hook.scad"], 0, 0), d, false);
        assert_eq!(c.name, "main.scad");
        assert_eq!(c.place, "~/models/brace · loose folder");
        assert!(c.save_rule.contains("into this folder"), "{}", c.save_rule);

        let c = card(&doc(loose(), &["main.scad"], 0, 0), d, false);
        assert_eq!(c.place, "~/models/brace · .scad file");
        assert_eq!(
            c.save_rule,
            "Save writes your edits and the cut plan to main.scad"
        );

        let c = card(&doc(ProjectHome::Fresh, &["untitled.scad"], 0, 0), d, false);
        assert_eq!(
            (c.name.as_str(), c.place.as_str(), c.save_rule.as_str()),
            ("untitled", "not saved yet", "Save asks where to save it")
        );

        // The web: Save downloads; a hotchkiss.io item says where the site update lives.
        let w = Platform::Web;
        let item = ProjectHome::WebModel("Brace".into());
        let c = card(
            &doc(item.clone(), &["main.scad", "hook.scad"], 0, 0),
            w,
            true,
        );
        assert_eq!(
            (c.name.as_str(), c.place.as_str()),
            ("Brace", "on hotchkiss.io")
        );
        assert_eq!(
            c.save_rule,
            "Save downloads Brace.scadproj · Save to hotchkiss.io, on the Model tab, updates the item"
        );
        let c = card(&doc(item, &["main.scad"], 0, 0), w, false);
        assert_eq!(c.place, "from the web");
        assert_eq!(c.save_rule, "Save downloads Brace.scad");
        let c = card(&doc(ProjectHome::Fresh, &["demo.scad"], 0, 0), w, false);
        assert_eq!(c.save_rule, "Save downloads demo.scad");

        // A path outside home stays whole.
        let c = card(
            &doc(
                ProjectHome::ScadProj("/Volumes/nas/a.scadproj".into()),
                &["m.scad"],
                0,
                0,
            ),
            d,
            false,
        );
        assert_eq!(c.place, "/Volumes/nas · .scadproj");
    }

    /// TG.7 (the Z.3.7 hint's two bugs): a fresh multi-file document claimed it "saves in place" — it
    /// has no place — and the `.scadproj` card was zip trivia instead of what Save does.
    #[test]
    fn card_never_promises_a_place_it_lacks_nor_talks_zip() {
        for platform in [Platform::Desktop, Platform::Web] {
            let fresh = doc(ProjectHome::Fresh, &["a.scad", "b.scad", "c.scad"], 0, 1);
            let c = card(&fresh, platform, false);
            assert!(!c.save_rule.contains("in place"), "{c:?}");
            assert!(!c.place.contains("in place"), "{c:?}");
        }
        let c = card(
            &doc(proj(), &["main.scad", "hook.scad"], 0, 0),
            Platform::Desktop,
            false,
        );
        let all = format!("{} {} {} {}", c.name, c.place, c.save_rule, c.full);
        assert!(!all.to_lowercase().contains("zip"), "{all}");
        assert!(c.save_rule.starts_with("Save rewrites"), "{c:?}");
    }

    /// TG.7: the desktop window title, per the spec's wording.
    #[test]
    fn window_title_table() {
        let p = doc(proj(), &["main.scad"], 0, 0);
        assert_eq!(window_title(&p, false), "bracket.scadproj — fab-scad");
        assert_eq!(
            window_title(&p, true),
            "bracket.scadproj (unsaved) — fab-scad"
        );
        let f = doc(ProjectHome::Fresh, &["untitled.scad"], 0, 0);
        assert_eq!(window_title(&f, false), "untitled — fab-scad");
        assert_eq!(window_title(&f, true), "untitled (unsaved) — fab-scad");
        assert_eq!(
            window_title(&doc(loose(), &["main.scad", "hook.scad"], 0, 1), false),
            "main.scad — fab-scad"
        );
    }

    /// TG.7 (review): a loose document is named by its home file, so renaming that file — or dropping
    /// it from the session — must move the name on every surface, and the Save sentence must name the
    /// file Save writes. The home used to stay put: title, chip and card kept a file that had moved.
    #[test]
    fn loose_name_follows_the_home_file() {
        let desk = Platform::Desktop;
        let mut d = doc(loose(), &["main.scad"], 0, 0);
        d.base_dir = Some(PathBuf::from(format!("{HOME}/models/brace")));
        let mut ed = crate::state::EditorBuf {
            path: d.editor_path(0),
            owner: Some(d.id()),
            ..Default::default()
        };
        crate::file_ops::rename(&mut d, &mut ed, 0, "bracket.scad").expect("renamed");
        assert_eq!(window_title(&d, false), "bracket.scad — fab-scad");
        let c = card(&d, desk, false);
        assert_eq!(c.name, "bracket.scad");
        assert_eq!(c.full, format!("{HOME}/models/brace/bracket.scad"));
        assert_eq!(
            c.save_rule,
            "Save writes your edits and the cut plan to bracket.scad"
        );
        assert_eq!(c.place, "~/models/brace · .scad file");

        // Renaming some OTHER file leaves the name alone.
        d.add_file("hook.scad", String::new());
        crate::file_ops::rename(&mut d, &mut ed, 1, "clip.scad").expect("renamed");
        assert_eq!(card(&d, desk, false).name, "bracket.scad");

        // Drop the home file from the session: the document is now called by what's left, the entry.
        crate::file_ops::delete(&mut d, &mut ed, 0).expect("deleted");
        assert_eq!(window_title(&d, false), "clip.scad — fab-scad");
        let c = card(&d, desk, false);
        assert_eq!(
            c.save_rule,
            "Save writes your edits and the cut plan to clip.scad"
        );
        assert_eq!(breadcrumb(&d), "clip.scad");

        // A rename into a subfolder keeps the home in the folder Save writes to (name goes stale,
        // the write dir doesn't move) — and the Save sentence still names the real file.
        crate::file_ops::rename(&mut d, &mut ed, 0, "parts/clip.scad").expect("renamed");
        assert_eq!(
            crate::save::plan(&d, desk, false),
            SavePlan::WriteLoose(PathBuf::from(format!("{HOME}/models/brace")))
        );
        assert_eq!(
            card(&d, desk, false).save_rule,
            "Save writes your edits and the cut plan to parts/clip.scad"
        );
    }

    /// TG.7: the breadcrumb names the viewed file, its document, and the entry when it isn't the file.
    #[test]
    fn breadcrumb_table() {
        let cases: [(ProjectDoc, &str); 7] = [
            (
                doc(proj(), &["main.scad", "hook.scad"], 0, 1),
                "hook.scad · in bracket.scadproj · main.scad renders",
            ),
            (
                doc(proj(), &["main.scad", "hook.scad"], 0, 0),
                "main.scad · in bracket.scadproj",
            ),
            (
                doc(loose(), &["main.scad", "hook.scad"], 0, 1),
                "hook.scad · in the brace folder · main.scad renders",
            ),
            // A one-file loose `.scad` IS the document — no "in main.scad".
            (doc(loose(), &["main.scad"], 0, 0), "main.scad"),
            (
                doc(ProjectHome::Fresh, &["untitled.scad"], 0, 0),
                "untitled.scad",
            ),
            (
                doc(ProjectHome::Fresh, &["untitled.scad", "b.scad"], 1, 0),
                "untitled.scad · in untitled · b.scad renders",
            ),
            (
                doc(ProjectHome::WebModel("Brace".into()), &["brace.scad"], 0, 0),
                "brace.scad · in Brace",
            ),
        ];
        for (d, want) in cases {
            assert_eq!(breadcrumb(&d), want);
        }
    }

    /// TG.7 / gui/CLAUDE.md: every string these build is ASCII or known-safe — across every home,
    /// platform, site flag, dirty state and active/entry pairing.
    #[test]
    fn every_string_passes_the_glyph_audit() {
        let homes = [
            proj(),
            loose(),
            ProjectHome::Fresh,
            ProjectHome::WebModel("Brace".into()),
        ];
        for h in homes {
            for files in [&["main.scad"][..], &["main.scad", "hook.scad"][..]] {
                for active in 0..files.len() {
                    let d = doc(h.clone(), files, 0, active);
                    for platform in [Platform::Desktop, Platform::Web] {
                        for on_site in [false, true] {
                            // Both the route Save takes and the raw plan (Site included).
                            for p in [
                                crate::save::save_route(&d, platform, on_site),
                                plan(&d, platform, on_site),
                            ] {
                                let c = doc_card(&d, &p, on_site, home());
                                for s in [&c.name, &c.place, &c.save_rule, &c.full] {
                                    assert!(glyph_safe(s), "tofu risk: {s:?}");
                                }
                            }
                        }
                    }
                    for unsaved in [false, true] {
                        let t = window_title(&d, unsaved);
                        assert!(glyph_safe(&t), "tofu risk: {t:?}");
                    }
                    let b = breadcrumb(&d);
                    assert!(glyph_safe(&b), "tofu risk: {b:?}");
                }
            }
        }
    }
}
