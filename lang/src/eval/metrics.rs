//! `textmetrics()` + `fontmetrics()` (Phase AG) — upstream's experimental text-measurement
//! builtins, always-on here, returning OBJECT values (the AF type's first in-tree consumer).
//!
//! The numbers are `FreetypeRenderer.cc` transliterated, and the fixed-point chain is the whole
//! game: upstream sizes the face with `FT_Set_Char_Size(face, 0, 1e5, 100, 100)`, so an em is
//! `FT_MulDiv(1e5·100, 72)` = 138 889 26.6-units, the face scale is `FT_DivFix(138889, upem)`
//! (exactly `138889·65536/2048 = 4 444 448` for Liberation Sans), UNHINTED advances round through
//! `FT_MulFix` per glyph, and ink boxes GRID-FIT (floor mins / ceil maxes to whole 26.6 pixels).
//! Every published value is `(26.6 value)/1e5 · size`. Four independent golden values
//! (`12.5733`, `15.9709`, `13.6109`, `29.3443`) pin the chain — an f64 shortcut misses the
//! printed digits.
//!
//! Like `text()`, ONLY the bundled Liberation Sans face is honored (determinism doctrine #36).

use std::rc::Rc;

use rustybuzz::ttf_parser;

use super::object::ObjectMap;
use super::text;
use super::value::Value;

/// `FreetypeRenderer::scale` — the em size in 26.6 units is derived from it, and every metric is
/// divided by it before scaling to `size`.
const FT_SCALE: f64 = 1e5;

/// `FT_MulFix(a, b)` — 16.16 fixed multiply with round-half-up via the reference's exact
/// `(a·b + 0x8000) >> 16` (arithmetic shift, so negatives round the `FreeType` way).
fn ft_mulfix(a: i64, b: i64) -> i64 {
    (a * b + 0x8000) >> 16
}

/// The face scale (`FT_DivFix(scaled_em, upem)` in 16.16): 26.6 units per font unit, as fixed.
fn face_scale(upem: u32) -> i64 {
    // FT_MulDiv(1e5, 100, 72) rounds to nearest: 138 889.
    let scaled_em: i64 = 138_889;
    (scaled_em << 16) / i64::from(upem)
}

/// One shaped glyph's metrics contributions, everything already `/FT_SCALE` (em-ish doubles).
struct GlyphInk {
    x_advance: f64,
    y_advance: f64,
    x_offset: f64,
    y_offset: f64,
    /// Grid-fitted ink box, `None` for an inkless glyph (space).
    bbox: Option<[f64; 4]>, // x_min, y_min, x_max, y_max
}

/// The `ShapeResults` accumulation: ink extents, ascent/descent, offsets, advances — all in
/// em-ish units (× `size` happens at publication).
struct Shaped {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    ascent: f64,
    descent: f64,
    advance_x: f64,
    advance_y: f64,
    x_offset: f64,
    y_offset: f64,
}

/// The `textmetrics()` parameters after validation (bad-typed args fall back to defaults with a
/// warning upstream — the golden's zero-object case is `text=""` metrics).
pub(super) struct MetricsParams {
    pub text: String,
    pub size: f64,
    pub spacing: f64,
    pub direction: String,
    pub language: String,
    pub script: String,
    pub halign: String,
    pub valign: String,
}

impl Default for MetricsParams {
    fn default() -> Self {
        MetricsParams {
            text: String::new(),
            size: 10.0,
            spacing: 1.0,
            direction: String::new(),
            language: "en".to_string(),
            script: String::new(),
            halign: "default".to_string(),
            valign: "default".to_string(),
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "26.6 grid indices divide values already bounded by the glyph run; the length IS the \
              ShapeResults transliteration — splitting it would decouple it from the reference"
)]
fn shape(p: &MetricsParams) -> Option<Shaped> {
    let ttf = ttf_parser::Face::parse(text::LIBERATION_SANS, 0).ok()?;
    let rb = rustybuzz::Face::from_slice(text::LIBERATION_SANS, 0)?;
    let fscale = face_scale(u32::from(ttf.units_per_em()));

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(&p.text);
    buffer.guess_segment_properties();
    if let Some(dir) = text::parse_direction(&p.direction) {
        buffer.set_direction(dir);
    }
    if !p.script.is_empty()
        && let Some(tag) = text::script_tag(&p.script)
    {
        buffer.set_script(tag);
    }
    if !p.language.is_empty()
        && let Ok(lang) = p.language.parse::<rustybuzz::Language>()
    {
        buffer.set_language(lang);
    }
    let horizontal = !matches!(
        text::parse_direction(&p.direction),
        Some(rustybuzz::Direction::TopToBottom | rustybuzz::Direction::BottomToTop)
    );
    let glyphs = rustybuzz::shape(&rb, &[], buffer);

    let mut inks = Vec::new();
    for (info, pos) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
        let gid = ttf_parser::GlyphId(u16::try_from(info.glyph_id).unwrap_or(0));
        // hb-ft's DEFAULT load flags include NO_HINTING: advances are the linear design values
        // through one rounding FT_MulFix each.
        let mulfix = |units: i32| ft_mulfix(i64::from(units), fscale);
        #[allow(clippy::cast_precision_loss, reason = "26.6 values are far under 2^53")]
        let em = |v: i64| v as f64 / FT_SCALE;
        let bbox = ttf.glyph_bounding_box(gid).and_then(|b| {
            // FT_GLYPH_BBOX_GRIDFIT: scale, then floor mins / ceil maxes to whole pixels (64ths).
            let x_min = ft_mulfix(i64::from(b.x_min), fscale).div_euclid(64) * 64;
            let y_min = ft_mulfix(i64::from(b.y_min), fscale).div_euclid(64) * 64;
            let x_max = (ft_mulfix(i64::from(b.x_max), fscale) + 63).div_euclid(64) * 64;
            let y_max = (ft_mulfix(i64::from(b.y_max), fscale) + 63).div_euclid(64) * 64;
            // a null box contributes no ink (upstream's xMax > xMin && yMax > yMin gate).
            (x_max > x_min && y_max > y_min).then(|| [em(x_min), em(y_min), em(x_max), em(y_max)])
        });
        inks.push(GlyphInk {
            x_advance: em(mulfix(pos.x_advance)),
            y_advance: em(mulfix(pos.y_advance)),
            x_offset: em(mulfix(pos.x_offset)),
            y_offset: em(mulfix(pos.y_offset)),
            bbox,
        });
    }

    let mut s = Shaped {
        left: f64::MAX,
        right: f64::MIN,
        top: f64::MIN,
        bottom: f64::MAX,
        ascent: f64::MIN,
        descent: f64::MAX,
        advance_x: 0.0,
        advance_y: 0.0,
        x_offset: 0.0,
        y_offset: 0.0,
    };
    for g in &inks {
        if let Some([x_min, y_min, x_max, y_max]) = g.bbox {
            s.ascent = s.ascent.max(y_max);
            s.descent = s.descent.min(y_min);
            s.left = s.left.min(s.advance_x + g.x_offset + x_min);
            s.right = s.right.max(s.advance_x + g.x_offset + x_max);
            s.top = s.top.max(s.advance_y + g.y_offset + y_max);
            s.bottom = s.bottom.min(s.advance_y + g.y_offset + y_min);
        }
        s.advance_x += g.x_advance * p.spacing;
        s.advance_y += g.y_advance * p.spacing;
    }

    if s.right >= s.left {
        if horizontal {
            // calc_offsets_horiz
            s.x_offset = match p.halign.as_str() {
                "right" => -s.advance_x,
                "center" => -s.advance_x / 2.0,
                _ => 0.0, // left/default (+ the unknown-value warning path)
            };
            s.y_offset = match p.valign.as_str() {
                "top" => -s.ascent,
                "center" => -(s.ascent - s.descent) / 2.0 - s.descent,
                "bottom" => -s.descent,
                _ => 0.0, // baseline/default
            };
        } else {
            // calc_offsets_vert
            s.x_offset = match p.halign.as_str() {
                "right" => -s.right,
                "left" => -s.left,
                _ => 0.0, // center/default
            };
            s.y_offset = match p.valign.as_str() {
                "center" => -s.advance_y / 2.0,
                "bottom" => -s.advance_y,
                _ => 0.0, // top/default (+ baseline warning path)
            };
        }
    } else {
        // whitespace-only: zero box at the origin; advances stay valid.
        s.left = 0.0;
        s.right = 0.0;
        s.top = 0.0;
        s.bottom = 0.0;
        s.ascent = 0.0;
        s.descent = 0.0;
        s.x_offset = 0.0;
        s.y_offset = 0.0;
    }
    Some(s)
}

fn pair(a: f64, b: f64) -> Value {
    Value::num_list(vec![a, b])
}

/// `textmetrics(...)` → `{ position; size; ascent; descent; offset; advance; }`.
#[must_use]
pub(super) fn textmetrics(p: &MetricsParams) -> Value {
    let Some(s) = shape(p) else {
        return Value::Undef;
    };
    let k = p.size;
    let mut o = ObjectMap::new();
    o.set(
        Rc::from("position"),
        pair((s.x_offset + s.left) * k, (s.y_offset + s.bottom) * k),
    );
    o.set(
        Rc::from("size"),
        pair((s.right - s.left) * k, (s.top - s.bottom) * k),
    );
    o.set(Rc::from("ascent"), Value::Num(s.ascent * k));
    o.set(Rc::from("descent"), Value::Num(s.descent * k));
    o.set(Rc::from("offset"), pair(s.x_offset * k, s.y_offset * k));
    o.set(Rc::from("advance"), pair(s.advance_x * k, s.advance_y * k));
    Value::Object(Rc::new(o))
}

/// `fontmetrics(...)` → `{ nominal = {...}; max = {...}; interline; font = {...}; }` — face-level
/// metrics (`FontMetrics` upstream: plain `FT_MulFix`, NO grid fit). The `max` box is the face's
/// global bounding box; `interline` is `FreeType`'s `face->height` = ascender − descender + line gap.
#[must_use]
pub(super) fn fontmetrics(size: f64) -> Value {
    let Ok(ttf) = ttf_parser::Face::parse(text::LIBERATION_SANS, 0) else {
        return Value::Undef;
    };
    let fscale = face_scale(u32::from(ttf.units_per_em()));
    #[allow(clippy::cast_precision_loss, reason = "26.6 values are far under 2^53")]
    let em = |units: i64| ft_mulfix(units, fscale) as f64 / FT_SCALE * size;
    let bbox = ttf.global_bounding_box();
    let height = i64::from(ttf.ascender()) - i64::from(ttf.descender()) + i64::from(ttf.line_gap());

    let mut nominal = ObjectMap::new();
    nominal.set(
        Rc::from("ascent"),
        Value::Num(em(i64::from(ttf.ascender()))),
    );
    nominal.set(
        Rc::from("descent"),
        Value::Num(em(i64::from(ttf.descender()))),
    );
    let mut max = ObjectMap::new();
    max.set(Rc::from("ascent"), Value::Num(em(i64::from(bbox.y_max))));
    max.set(Rc::from("descent"), Value::Num(em(i64::from(bbox.y_min))));
    let mut font = ObjectMap::new();
    // The bundled face's name-table values, pinned (one face, doctrine #36).
    font.set(Rc::from("family"), Value::string("Liberation Sans"));
    font.set(Rc::from("style"), Value::string("Regular"));

    let mut o = ObjectMap::new();
    o.set(Rc::from("nominal"), Value::Object(Rc::new(nominal)));
    o.set(Rc::from("max"), Value::Object(Rc::new(max)));
    o.set(Rc::from("interline"), Value::Num(em(height)));
    o.set(Rc::from("font"), Value::Object(Rc::new(font)));
    Value::Object(Rc::new(o))
}
