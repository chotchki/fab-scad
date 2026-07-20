//! Build-time font subset pipeline (U.2.1 icons + W.1.4 UI fonts).
//!
//! Produces the committed font subsets under `assets/fonts/` from pinned upstream sources —
//! DETERMINISTICALLY: the same pins always yield byte-identical bytes (pinned source by sha256, fixed
//! axis instance, `SOURCE_DATE_EPOCH=0` on every fonttools call). Three fonts:
//!   - `MaterialSymbols-subset.ttf` — the PUA icon glyphs in `MANIFEST` (instanced at FILL=0/wght=400).
//!   - `Oswald-subset.ttf`          — the condensed UPPERCASE chrome face (variable, instanced wght=400).
//!   - `Quattrocento-subset.ttf`    — the serif accent voice (static Regular).
//!
//! Both UI fonts are Latin-subset (see `LATIN`); the icon font is subset to exactly the manifest.
//!
//! The committed subsets ARE the cache. A normal or CI build finds a matching stamp and does NOTHING —
//! no network, no fonttools, so the just-won green CI stays offline-clean. Only editing the glyph
//! MANIFEST, the `LATIN` range, or a source pin below triggers a regen (download → instance → subset)
//! of ALL three. Regen needs `curl`, `fonttools`, `pyftsubset`, `shasum` on PATH; the dev commits the
//! regenerated subsets + stamp. `.subset-stamp` is written by this script so it can't drift from the check.

use std::path::{Path, PathBuf};
use std::process::Command;

/// (const-name, codepoint) — the SINGLE source of truth for the ICON font. Drives BOTH the subset
/// `--unicodes` and the generated Rust glyph consts, so the font and the code that draws it can't drift.
const MANIFEST: &[(&str, u32)] = &[
    ("ADD", 0xe145),           // plus
    ("DELETE", 0xe872),        // trash
    ("EYE", 0xe8f4),           // visible (on)
    ("EYE_OFF", 0xe8f5),       // hidden (off)
    ("PLUG_CONNECT", 0xf35a),  // connector
    ("CHEVRON_RIGHT", 0xe5cc), // cut collapsed
    ("EXPAND_MORE", 0xe5cf),   // cut expanded
    ("SAVE", 0xe161),          // save (floppy)
    ("RESTART", 0xe5d5),       // refresh / reset-to-auto
    ("DOT", 0xe061), // fiber_manual_record — filled status dot (stale-tab badge + unsaved)
    ("CHECK", 0xe668), // check — affirmative tick ("flat ✓" → flat CHECK)
    ("SETTINGS", 0xe8b8), // gear — the header Settings entry (W.3.27; publish credentials)
];

/// Latin subset for the UI text fonts: Basic Latin + Latin-1 + the typographic marks the app renders
/// (en/em dash, curly quotes, ellipsis, bullet — the `gui/CLAUDE.md` known-safe set · … — × ° lives in
/// Latin-1). Chrome labels are words; readouts stay on Ubuntu-Light, so this range covers every Oswald site.
const LATIN: &str = "U+0020-00FF,U+2013-2014,U+2018-2019,U+201C-201D,U+2026,U+2022";

// --- Pinned sources (content-addressed = deterministic; a moved/updated upstream trips the sha256 check) ---

/// Material Symbols Outlined variable font (google/material-design-icons @ pinned commit).
const MS_URL: &str = "https://raw.githubusercontent.com/google/material-design-icons/819d78680a849ceef4c78f863d8753e3160b7c89/variablefont/MaterialSymbolsOutlined%5BFILL%2CGRAD%2Copsz%2Cwght%5D.ttf";
const MS_SHA256: &str = "e67c84976868d2016a4bb5e4daacd03d92e0c58c935ccd7091fe3b6230761552";
const MS_AXES: &[(&str, &str)] = &[
    ("FILL", "0"),
    ("wght", "400"),
    ("GRAD", "0"),
    ("opsz", "24"),
];

/// Oswald variable font — instanced to Regular (wght=400). Oswald ships variable-only upstream, so we
/// instance like the icon font. (google/fonts @ pinned commit; OFL-1.1.)
const OSWALD_URL: &str = "https://raw.githubusercontent.com/google/fonts/ec0464b978de222073645d6d3366f3fdf03376d8/ofl/oswald/Oswald%5Bwght%5D.ttf";
const OSWALD_SHA256: &str = "5b38c246e255a12f5712d640d56bcced0472466fc68983d2d0410ec0457c2817";
const OSWALD_AXES: &[(&str, &str)] = &[("wght", "400")];

/// Quattrocento Regular — a static face, no instancing needed. (google/fonts @ pinned commit; OFL-1.1.)
const QUATTRO_URL: &str = "https://raw.githubusercontent.com/google/fonts/ec0464b978de222073645d6d3366f3fdf03376d8/ofl/quattrocento/Quattrocento-Regular.ttf";
const QUATTRO_SHA256: &str = "57dc8daff9121be82e54cf1221658b7ba4f1801212817aead2d184a5660fbcb9";

const STAMP_REL: &str = "assets/fonts/.subset-stamp";

/// One font subset job: fetch `url` (verify `sha256`) → instance at `axes` (skip if empty = static) →
/// subset to `unicodes` → write `out_rel`.
struct FontJob {
    label: &'static str,
    url: &'static str,
    sha256: &'static str,
    axes: &'static [(&'static str, &'static str)],
    unicodes: String,
    out_rel: &'static str,
}

fn font_jobs() -> Vec<FontJob> {
    let icon_unicodes = MANIFEST
        .iter()
        .map(|(_, c)| format!("{c:x}"))
        .collect::<Vec<_>>()
        .join(",");
    vec![
        FontJob {
            label: "material-symbols",
            url: MS_URL,
            sha256: MS_SHA256,
            axes: MS_AXES,
            unicodes: icon_unicodes,
            out_rel: "assets/fonts/MaterialSymbols-subset.ttf",
        },
        FontJob {
            label: "oswald",
            url: OSWALD_URL,
            sha256: OSWALD_SHA256,
            axes: OSWALD_AXES,
            unicodes: LATIN.to_string(),
            out_rel: "assets/fonts/Oswald-subset.ttf",
        },
        FontJob {
            label: "quattrocento",
            url: QUATTRO_URL,
            sha256: QUATTRO_SHA256,
            axes: &[],
            unicodes: LATIN.to_string(),
            out_rel: "assets/fonts/Quattrocento-subset.ttf",
        },
    ]
}

fn main() {
    // The manifest + pins all live in this file, so a change here is the only regen trigger.
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let stamp = manifest_dir.join(STAMP_REL);
    let jobs = font_jobs();

    // Glyph consts are cheap + std-only — regenerate every build so they always match the manifest.
    generate_consts();

    let want = stamp_string(&jobs);
    let have = std::fs::read_to_string(&stamp).unwrap_or_default();
    let all_present = jobs.iter().all(|j| manifest_dir.join(j.out_rel).exists());
    if all_present && have == want {
        return; // cache hit — every committed subset is current. No network, no tools.
    }

    // REGEN — the manifest, Latin range, or a source pin changed. Requires curl/fonttools/pyftsubset/shasum.
    eprintln!("fab-gui build.rs: regenerating font subsets (manifest/range/source changed)");
    let cache = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("font-cache");
    std::fs::create_dir_all(&cache).expect("create font-cache");
    for job in &jobs {
        regen_font(&cache, job, &manifest_dir.join(job.out_rel));
    }
    std::fs::write(&stamp, want).expect("write .subset-stamp");
}

/// The stamp is a canonical plain-text descriptor of everything that determines the subset bytes across
/// all fonts. Plain string compare (no hashing) keeps the fast path std-only.
fn stamp_string(jobs: &[FontJob]) -> String {
    let mut s = String::from("v2\n");
    for j in jobs {
        let axes: Vec<String> = j.axes.iter().map(|(a, v)| format!("{a}={v}")).collect();
        s.push_str(&format!(
            "font={}\nurl={}\nsha256={}\naxes={}\nunicodes={}\n",
            j.label,
            j.url,
            j.sha256,
            axes.join(","),
            j.unicodes
        ));
    }
    s
}

/// Emit `$OUT_DIR/icon_glyphs.rs` — one `pub const <NAME>: &str = "\u{cp}";` per manifest entry.
fn generate_consts() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("icon_glyphs.rs");
    let mut src = String::from(
        "// @generated by build.rs from the Material Symbols manifest — do not edit.\n",
    );
    for (name, cp) in MANIFEST {
        src.push_str(&format!("pub const {name}: &str = \"\\u{{{cp:x}}}\";\n"));
    }
    std::fs::write(&out, src).expect("write icon_glyphs.rs");
}

/// Download (cached, sha256-verified) → instance at fixed axes (skipped for a static font) → subset to
/// the job's unicodes. Every fonttools call runs under `SOURCE_DATE_EPOCH=0` so `head.modified` is fixed
/// and the output is byte-stable.
fn regen_font(cache: &Path, job: &FontJob, out: &Path) {
    // 1. Download the source font, keyed by its pinned hash; verify before use.
    let full = cache.join(format!("{}-{}.ttf", job.label, &job.sha256[..16]));
    if !full.exists() || sha256(&full) != job.sha256 {
        let ok = Command::new("curl")
            .args(["-sL", "--fail", "--max-time", "180", "-o"])
            .arg(&full)
            .arg(job.url)
            .status()
            .unwrap_or_else(|e| panic!("run curl to fetch {} font: {e}", job.label))
            .success();
        assert!(ok, "curl failed to download the {} source font", job.label);
        let got = sha256(&full);
        assert_eq!(
            got, job.sha256,
            "{} font sha256 mismatch — upstream moved; update the URL + SHA256 pin",
            job.label
        );
    }

    // 2. Instance the variable font to a static instance at the fixed axes (static fonts skip this).
    let to_subset = if job.axes.is_empty() {
        full.clone()
    } else {
        let static_font = cache.join(format!("{}-static.ttf", job.label));
        let axis_args: Vec<String> = job.axes.iter().map(|(a, v)| format!("{a}={v}")).collect();
        let ok = Command::new("fonttools")
            .env("SOURCE_DATE_EPOCH", "0")
            .arg("varLib.instancer")
            .arg(&full)
            .args(&axis_args)
            .arg("-q")
            .arg("-o")
            .arg(&static_font)
            .status()
            .expect("run fonttools varLib.instancer (install fonttools to regenerate font subsets)")
            .success();
        assert!(ok, "fonttools varLib.instancer failed for {}", job.label);
        static_font
    };

    // 3. Subset to exactly the job's codepoints (cmap preserved for egui).
    let ok = Command::new("pyftsubset")
        .env("SOURCE_DATE_EPOCH", "0")
        .arg(&to_subset)
        .arg(format!("--unicodes={}", job.unicodes))
        .arg("--no-hinting")
        .arg("--desubroutinize")
        .arg(format!("--output-file={}", out.display()))
        .status()
        .expect("run pyftsubset (install fonttools to regenerate font subsets)")
        .success();
    assert!(ok, "pyftsubset failed for {}", job.label);
}

/// sha256 of a file via `shasum -a 256` (only reached on regen, never on the cache-hit fast path).
fn sha256(path: &Path) -> String {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("run shasum");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}
