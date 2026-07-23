//! `text()` → 2D glyph contours (J.4.3) — OpenSCAD's `text` primitive, the pure-Rust way.
//!
//! OpenSCAD draws text with harfbuzz (shaping) + fontconfig (font lookup) + `FreeType` (glyph outlines) →
//! 2D polygons. We match the SHAPING with [`rustybuzz`] (the pure-Rust harfbuzz port) and the OUTLINES with
//! `ttf-parser` (which rustybuzz re-exports), then flatten the Bézier segments into `$fn`-resolved contours
//! that lower to a [`Shape2D::Polygon`](super::geo2d::Shape2D) — the same even-odd-filled leaf `square`/
//! `circle`/`polygon` produce, so glyph HOLES ('o', 'A', 'e') resolve for free.
//!
//! DETERMINISM (doctrine #36): NO system font lookup — fontconfig is banned (it resolves to different
//! files per machine). The default font is Liberation Sans (OpenSCAD's own default, SIL OFL), BUNDLED and
//! pinned below, so `text()` is bit-identical on every platform AND draws the same glyphs OpenSCAD does —
//! which lets `text()` output be validated against the oracle by VOLUME-RESIDUAL (`differ.rs`), not needing
//! a bit-exact mesh. A non-default `font=` is a LOUD-ish fallback for now (we ship one face; honoring
//! arbitrary system fonts would reintroduce the non-determinism we just removed — a later, opt-in task).

use rustybuzz::ttf_parser;

use crate::geom::Vec2;

use super::geo2d::Contour;

/// Liberation Sans (OpenSCAD's default `text()` font; SIL OFL, see `fonts/LiberationSans-LICENSE.txt`),
/// bundled so glyph outlines are deterministic + oracle-matching without touching the host's fonts.
pub(super) static LIBERATION_SANS: &[u8] = include_bytes!("fonts/LiberationSans-Regular.ttf");

/// The parsed `text()` arguments (OpenSCAD `TextModule`/`FreetypeRenderer`). `font` is accepted but only
/// the bundled face is honored for now (see the module determinism note).
pub(super) struct TextParams {
    pub text: String,
    pub size: f64,
    pub halign: String,
    pub valign: String,
    pub spacing: f64,
    pub direction: String,
    pub language: String,
    pub script: String,
}

/// Shape `p.text` with the bundled font and return its glyph outlines as `$fn`-flattened 2D contours
/// (mm units, baseline at the origin before the h/v-align shift). Empty text / a parse failure → no
/// contours (a present-but-empty 2D leaf, like `circle(0)`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "curve_segs is a small bounded positive segment count derived from $fn (>= 3 here)"
)]
pub(super) fn text_contours(p: &TextParams, fn_: f64, fa: f64, fs: f64) -> Vec<Contour> {
    // OpenSCAD renders text through FreeType at 72 DPI while treating `size` as a 100-unit measure, so its
    // glyphs come out an extra 100/72 larger than the naive `size / units_per_em` (an `I` at size=100 is
    // 95.55 mm tall, not the 68.8 mm the em ratio gives — `size` is effectively a point size, not the em
    // height). Match the oracle: the ratio is a constant 1.3888… across sizes (verified), i.e. exactly
    // 100/72. A legacy OpenSCAD quirk, NOT a metric of the font (units_per_em is a true 2048 here).
    const OPENSCAD_DPI_SCALE: f64 = 100.0 / 72.0;
    if p.text.is_empty() {
        return Vec::new();
    }
    let Ok(ttf) = ttf_parser::Face::parse(LIBERATION_SANS, 0) else {
        return Vec::new();
    };
    let Some(rb) = rustybuzz::Face::from_slice(LIBERATION_SANS, 0) else {
        return Vec::new();
    };
    let upem = f64::from(ttf.units_per_em());
    let scale = p.size / upem * OPENSCAD_DPI_SCALE;
    // Segments PER Bézier: a glyph curve is a small arc, so `$fn/8` (min 2) tracks the circle-fragment feel;
    // no `$fn` → the fragment count for a `size`-radius arc, capped, so curviness follows `$fa`/`$fs` too.
    let curve_segs = if fn_ >= 3.0 {
        ((fn_ / 8.0).ceil() as u32).max(2)
    } else {
        super::fragments::fragments(p.size, fn_, fa, fs)
            .div_ceil(4)
            .clamp(3, 16)
    };

    // Shape: guess script/direction/language from the text, then honor an explicit `direction=`.
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(&p.text);
    buffer.guess_segment_properties();
    if let Some(dir) = parse_direction(&p.direction) {
        buffer.set_direction(dir);
    }
    if !p.script.is_empty()
        && let Some(tag) = script_tag(&p.script)
    {
        buffer.set_script(tag);
    }
    if !p.language.is_empty()
        && let Ok(lang) = p.language.parse::<rustybuzz::Language>()
    {
        buffer.set_language(lang);
    }
    let glyphs = rustybuzz::shape(&rb, &[], buffer);

    let mut contours: Vec<Contour> = Vec::new();
    // Pen position in FONT units; each glyph is outlined at its pen + shaping offset, then advanced.
    let mut pen_x = 0.0_f64;
    let mut pen_y = 0.0_f64;
    for (info, pos) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
        let gid = ttf_parser::GlyphId(u16::try_from(info.glyph_id).unwrap_or(0)); // 0 = .notdef
        let origin_x = (pen_x + f64::from(pos.x_offset)) * scale;
        let origin_y = (pen_y + f64::from(pos.y_offset)) * scale;
        let mut collector = OutlineCollector::new(origin_x, origin_y, scale, curve_segs);
        ttf.outline_glyph(gid, &mut collector); // None for a space (no outline) — fine, no contours added
        contours.append(&mut collector.finish());
        // `spacing` scales the advance (OpenSCAD's letter spacing multiplier); default 1.0.
        pen_x += f64::from(pos.x_advance) * p.spacing;
        pen_y += f64::from(pos.y_advance) * p.spacing;
    }

    align(&mut contours, p, &ttf, scale, pen_x * scale);
    contours
}

/// Shift every contour to honor `halign` (left/center/right, over the total advance `width`) and `valign`
/// (top/center/baseline/bottom, over the font's ascender/descender). OpenSCAD's alignment semantics.
fn align(contours: &mut [Contour], p: &TextParams, ttf: &ttf_parser::Face, scale: f64, width: f64) {
    let dx = match p.halign.as_str() {
        "center" => -width / 2.0,
        "right" => -width,
        _ => 0.0, // "left" / default
    };
    let ascender = f64::from(ttf.ascender()) * scale;
    let descender = f64::from(ttf.descender()) * scale; // negative (below baseline)
    let dy = match p.valign.as_str() {
        "top" => -ascender,
        "center" => -(ascender + descender) / 2.0,
        "bottom" => -descender,
        _ => 0.0, // "baseline" / default
    };
    if dx != 0.0 || dy != 0.0 {
        for contour in contours.iter_mut() {
            for point in contour.iter_mut() {
                *point = Vec2::new(point.x + dx, point.y + dy);
            }
        }
    }
}

/// A [`ttf_parser::OutlineBuilder`] that flattens the glyph's Bézier segments into line-segment contours,
/// applying the glyph's `(origin, scale)` as it goes (font units → mm). One contour per closed loop.
struct OutlineCollector {
    origin_x: f64,
    origin_y: f64,
    scale: f64,
    curve_segs: u32,
    contours: Vec<Contour>,
    current: Contour,
    /// The pen, in FONT units (Bézier control math happens in font space, then each point is transformed).
    last: (f64, f64),
}

impl OutlineCollector {
    fn new(origin_x: f64, origin_y: f64, scale: f64, curve_segs: u32) -> Self {
        Self {
            origin_x,
            origin_y,
            scale,
            curve_segs,
            contours: Vec::new(),
            current: Vec::new(),
            last: (0.0, 0.0),
        }
    }

    /// Font-space point → the placed, scaled `Vec2`.
    fn place(&self, x: f64, y: f64) -> Vec2 {
        Vec2::new(
            self.origin_x + x * self.scale,
            self.origin_y + y * self.scale,
        )
    }

    fn finish(mut self) -> Vec<Contour> {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.contours
    }
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
        let (x, y) = (f64::from(x), f64::from(y));
        self.current.push(self.place(x, y));
        self.last = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = (f64::from(x), f64::from(y));
        self.current.push(self.place(x, y));
        self.last = (x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, c, p1) = (
            self.last,
            (f64::from(x1), f64::from(y1)),
            (f64::from(x), f64::from(y)),
        );
        for i in 1..=self.curve_segs {
            let t = f64::from(i) / f64::from(self.curve_segs);
            let mt = 1.0 - t;
            // quadratic: (1-t)²p0 + 2(1-t)t·c + t²p1
            let px = mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * p1.0;
            let py = mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * p1.1;
            self.current.push(self.place(px, py));
        }
        self.last = p1;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, c1, c2, p1) = (
            self.last,
            (f64::from(x1), f64::from(y1)),
            (f64::from(x2), f64::from(y2)),
            (f64::from(x), f64::from(y)),
        );
        for i in 1..=self.curve_segs {
            let t = f64::from(i) / f64::from(self.curve_segs);
            let mt = 1.0 - t;
            // cubic: (1-t)³p0 + 3(1-t)²t·c1 + 3(1-t)t²·c2 + t³p1
            let px = mt * mt * mt * p0.0
                + 3.0 * mt * mt * t * c1.0
                + 3.0 * mt * t * t * c2.0
                + t * t * t * p1.0;
            let py = mt * mt * mt * p0.1
                + 3.0 * mt * mt * t * c1.1
                + 3.0 * mt * t * t * c2.1
                + t * t * t * p1.1;
            self.current.push(self.place(px, py));
        }
        self.last = p1;
    }

    fn close(&mut self) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
    }
}

/// OpenSCAD `direction` → rustybuzz [`Direction`](rustybuzz::Direction). Unknown → `None` (keep the guess).
pub(super) fn parse_direction(dir: &str) -> Option<rustybuzz::Direction> {
    match dir {
        "ltr" => Some(rustybuzz::Direction::LeftToRight),
        "rtl" => Some(rustybuzz::Direction::RightToLeft),
        "ttb" => Some(rustybuzz::Direction::TopToBottom),
        "btt" => Some(rustybuzz::Direction::BottomToTop),
        _ => None,
    }
}

/// OpenSCAD `script` (an ISO-15924 tag like "latn"/"arab") → a rustybuzz [`Script`](rustybuzz::Script).
pub(super) fn script_tag(script: &str) -> Option<rustybuzz::Script> {
    let bytes = script.as_bytes();
    if bytes.len() == 4 {
        rustybuzz::Script::from_iso15924_tag(ttf_parser::Tag::from_bytes(&[
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))
    } else {
        None
    }
}
