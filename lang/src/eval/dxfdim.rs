//! `dxf_dim()` / `dxf_cross()` (AI.3) — legacy DXF dimension reading, `dxfdim.cc` + the
//! DIMENSION/LINE slice of `DxfData.cc` transliterated.
//!
//! A DXF is group-code/value line PAIRS; entities begin at code `0`. This parser collects ONLY
//! what the two builtins read: DIMENSION entities (their 7 coordinate pairs, type flags, angle,
//! name) and LINE entities (for `dxf_cross`'s intersection), with upstream's origin/scale
//! transform applied per group code (`11/12/16` scale-only, the rest origin-then-scale — the
//! reference's own quirk, kept). BLOCKS/INSERT expansion is deliberately out of scope: the
//! corpus dimension files are plain ENTITIES, and the geometry DXF path is a different backlog.

use super::trig;

/// One DIMENSION entity's fields, exactly the slice `dxf_dim` reads.
pub(super) struct Dim {
    /// Group code 70 — the dimension type flags; `& 7` selects the variant, `& 64` the ordinate axis.
    pub type_flags: i64,
    /// Coordinate pairs from group codes `1x`/`2x` (`x` = 0..6).
    pub coords: [[f64; 2]; 7],
    /// Group code 50 — the rotation angle of a linear dimension.
    pub angle: f64,
    /// Group code 1 — the dimension's name (what `dxf_dim(name=…)` matches).
    pub name: String,
}

/// A LINE entity's endpoints (`dxf_cross`).
pub(super) struct Line {
    pub p1: [f64; 2],
    pub p2: [f64; 2],
}

/// Parse `text` collecting the DIMENSION + LINE entities on `layername` (empty = every layer),
/// with `origin`/`scale` applied the reference way.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the index casts are range-guarded (10..=16 / 20..=26 minus their base → 0..=6)"
)]
pub(super) fn parse(
    text: &str,
    layername: &str,
    xorigin: f64,
    yorigin: f64,
    scale: f64,
) -> (Vec<Dim>, Vec<Line>) {
    let mut dims = Vec::new();
    let mut lines_out = Vec::new();

    let mut mode = String::new();
    let mut layer = String::new();
    let mut name = String::new();
    let mut dimtype: i64 = 0;
    let mut coords = [[0.0_f64; 2]; 7];
    let mut xverts: Vec<f64> = Vec::new();
    let mut yverts: Vec<f64> = Vec::new();
    let mut arc_start_angle = 0.0_f64;
    let mut in_entities = false;
    let mut iddata = String::new();

    let mut it = text.lines();
    while let (Some(id_line), Some(data)) = (it.next(), it.next()) {
        let Ok(id) = id_line.trim().parse::<i64>() else {
            break; // upstream warns "Illegal ID" and stops
        };
        let data = data.trim();
        let num = || data.parse::<f64>().unwrap_or(0.0);

        // DIMENSION coordinate pairs — the reference's split: codes 11/12/16 (and 21/22/26)
        // scale WITHOUT the origin shift, the rest shift then scale.
        if (10..=16).contains(&id) {
            let i = (id - 10) as usize;
            coords[i][0] = if id == 11 || id == 12 || id == 16 {
                num() * scale
            } else {
                (num() - xorigin) * scale
            };
        }
        if (20..=26).contains(&id) {
            let i = (id - 20) as usize;
            coords[i][1] = if id == 21 || id == 22 || id == 26 {
                num() * scale
            } else {
                (num() - yorigin) * scale
            };
        }

        match id {
            0 => {
                // finalize the entity that just ended
                if mode == "SECTION" {
                    in_entities = iddata == "ENTITIES";
                } else if mode == "LINE" {
                    if in_entities
                        && (layername.is_empty() || layername == layer)
                        && xverts.len() >= 2
                        && yverts.len() >= 2
                    {
                        lines_out.push(Line {
                            p1: [xverts[0], yverts[0]],
                            p2: [xverts[1], yverts[1]],
                        });
                    }
                } else if mode == "DIMENSION" && (layername.is_empty() || layername == layer) {
                    dims.push(Dim {
                        type_flags: dimtype,
                        coords,
                        angle: arc_start_angle,
                        name: name.clone(),
                    });
                }
                mode = data.to_string();
                layer.clear();
                name.clear();
                iddata.clear();
                dimtype = 0;
                coords = [[0.0; 2]; 7];
                xverts.clear();
                yverts.clear();
                arc_start_angle = 0.0;
            }
            1 => name = data.to_string(),
            2 => iddata = data.to_string(),
            8 => layer = data.to_string(),
            // LINE endpoints (the vert channel is SEPARATE from the DIMENSION coords above).
            10 | 11 => xverts.push((num() - xorigin) * scale),
            20 | 21 => yverts.push((num() - yorigin) * scale),
            50 => arc_start_angle = num(),
            70 => dimtype = data.parse::<i64>().unwrap_or(0),
            _ => {}
        }
    }
    (dims, lines_out)
}

/// `dxf_dim`'s measurement of `dim` — `dxfdim.cc`'s `type & 7` case split. `None` = unsupported
/// type (upstream warns).
pub(super) fn dim_value(d: &Dim) -> Option<f64> {
    match d.type_flags & 7 {
        0 => {
            // rotated / horizontal / vertical: |(p4-p3) projected on the angle|
            let x = d.coords[4][0] - d.coords[3][0];
            let y = d.coords[4][1] - d.coords[3][1];
            Some((x * trig::cos_degrees(d.angle) + y * trig::sin_degrees(d.angle)).abs())
        }
        1 => {
            // aligned
            let x = d.coords[4][0] - d.coords[3][0];
            let y = d.coords[4][1] - d.coords[3][1];
            Some(x.hypot(y))
        }
        2 => {
            // angular
            let a1 = trig::atan2_degrees(
                d.coords[0][0] - d.coords[5][0],
                d.coords[0][1] - d.coords[5][1],
            );
            let a2 = trig::atan2_degrees(
                d.coords[4][0] - d.coords[3][0],
                d.coords[4][1] - d.coords[3][1],
            );
            Some((a1 - a2).abs())
        }
        3 | 4 => {
            // diameter / radius
            let x = d.coords[5][0] - d.coords[0][0];
            let y = d.coords[5][1] - d.coords[0][1];
            Some(x.hypot(y))
        }
        6 => Some(if d.type_flags & 64 != 0 {
            d.coords[3][0] // ordinate X
        } else {
            d.coords[3][1] // ordinate Y
        }),
        _ => None, // 5 = angular-3-point — unsupported upstream too
    }
}

/// `dxf_cross`: the intersection of the first two LINE entities, or `None` (parallel / < 2 lines).
pub(super) fn cross(lines: &[Line]) -> Option<[f64; 2]> {
    let (a, b) = (lines.first()?, lines.get(1)?);
    let (x1, y1, x2, y2) = (a.p1[0], a.p1[1], a.p2[0], a.p2[1]);
    let (x3, y3, x4, y4) = (b.p1[0], b.p1[1], b.p2[0], b.p2[1]);
    let dem = (y4 - y3) * (x2 - x1) - (x4 - x3) * (y2 - y1);
    if dem == 0.0 {
        return None;
    }
    let ua = ((x4 - x3) * (y1 - y3) - (y4 - y3) * (x1 - x3)) / dem;
    Some([x1 + ua * (x2 - x1), y1 + ua * (y2 - y1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal two-entity DXF: one aligned DIMENSION (3-4-5 triangle → 5) and two crossing LINEs.
    const FIXTURE: &str = "0\nSECTION\n2\nENTITIES\n\
        0\nDIMENSION\n8\n0\n1\nd1\n70\n1\n13\n0\n23\n0\n14\n3\n24\n4\n\
        0\nLINE\n8\n0\n10\n0\n20\n0\n11\n2\n21\n2\n\
        0\nLINE\n8\n0\n10\n0\n20\n2\n11\n2\n21\n0\n\
        0\nENDSEC\n0\nEOF\n";

    #[test]
    fn parses_dimensions_and_lines() {
        let (dims, lines) = parse(FIXTURE, "", 0.0, 0.0, 1.0);
        assert_eq!(dims.len(), 1);
        assert_eq!(dims[0].name, "d1");
        assert_eq!(dim_value(&dims[0]), Some(5.0), "aligned 3-4-5");
        assert_eq!(lines.len(), 2);
        assert_eq!(cross(&lines), Some([1.0, 1.0]));
    }

    #[test]
    fn origin_and_scale_transform() {
        let (dims, _) = parse(FIXTURE, "", 0.0, 0.0, 2.0);
        assert_eq!(dim_value(&dims[0]), Some(10.0), "scale doubles the length");
        let (dims, _) = parse(FIXTURE, "nope", 0.0, 0.0, 1.0);
        assert!(dims.is_empty(), "layer filter");
    }
}
