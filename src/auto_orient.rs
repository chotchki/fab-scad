//! Auto-orient (#42): the smart-default print orientation for a piece — the build-up direction
//! needing the least support. A DETERMINISTIC discrete scan over a fixed candidate set scored from
//! the piece mesh, NOT an SO(3) optimization (chotchki's framing: a straightforward good guess).
//! Pure triangle math, so `fab slice` and the GUI compute byte-identical picks.

use crate::geom::{self, V3};

/// Overhang threshold: a face is unsupported if it points downward more than this from horizontal.
/// Tunable like the slicer's gate; refine with a printed coupon.
pub const SUPPORT_ANGLE: f64 = 45.0;
/// Bed-contact layer thickness (mm): downward faces within this of the lowest point rest ON the
/// bed (not overhangs) and earn a small reward instead.
const BED_EPS: f64 = 0.6;
/// How much a flat bed footprint discounts the score (only breaks near-ties — it must not let a
/// big-overhang orientation win on contact alone).
const BED_REWARD: f64 = 0.1;

/// Candidate build-ups, in a FIXED order (so ties resolve deterministically; +Z wins first): the 6
/// axis-aligned ups, four 45° tilts of +Z toward each horizontal axis, then the piece's incident
/// cut-face normals (cut-face-on-bed usually prints best AND keeps onion joints feasible).
pub fn candidates(cut_normals: &[V3]) -> Vec<V3> {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let mut c = vec![
        [0.0, 0.0, 1.0], // +Z first: the tie-break winner
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [s, 0.0, s],
        [-s, 0.0, s],
        [0.0, s, s],
        [0.0, -s, s],
    ];
    c.extend(cut_normals.iter().map(|&n| geom::normalize(n)));
    c
}

/// Unsupported-overhang area of `tris` printed along `up`, minus a small bed-contact reward —
/// lower is better. A face counts as overhang if its normal points downward past the support
/// angle AND it sits above the bed-contact layer (so the flat bottom resting on the bed is NOT
/// scored as the worst overhang — the trap the naive version falls into).
pub fn overhang_score(tris: &[[V3; 3]], up: V3) -> f64 {
    let up = geom::normalize(up);
    let cos_t = SUPPORT_ANGLE.to_radians().cos(); // 0.707 at 45°
    let z_min = tris
        .iter()
        .flat_map(|t| t.iter())
        .map(|&v| geom::dot(v, up))
        .fold(f64::INFINITY, f64::min);
    let (mut overhang, mut contact) = (0.0, 0.0);
    for t in tris {
        let normal = geom::normalize(geom::cross(geom::sub(t[1], t[0]), geom::sub(t[2], t[0])));
        if geom::dot(normal, up) >= -cos_t {
            continue; // not a downward overhang face
        }
        let area = 0.5 * geom::norm(geom::cross(geom::sub(t[1], t[0]), geom::sub(t[2], t[0])));
        let centroid_h = (geom::dot(t[0], up) + geom::dot(t[1], up) + geom::dot(t[2], up)) / 3.0;
        if centroid_h - z_min > BED_EPS {
            overhang += area;
        } else {
            contact += area; // resting on the bed
        }
    }
    overhang - BED_REWARD * contact
}

/// The least-support build-up for a piece mesh: argmin overhang_score over the candidates, ties
/// to the earliest candidate (+Z). Returns +Z for an empty mesh.
pub fn best_up(tris: &[[V3; 3]], cut_normals: &[V3]) -> V3 {
    candidates(cut_normals)
        .into_iter()
        .map(|u| (overhang_score(tris, u), u))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, u)| u)
        .unwrap_or([0.0, 0.0, 1.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    // A unit quad (two tris) in the plane z = `z`, wound so its normal is -Z (downward-facing).
    fn down_quad(z: f64) -> [[V3; 3]; 2] {
        [
            [[0.0, 0.0, z], [0.0, 1.0, z], [1.0, 0.0, z]],
            [[1.0, 0.0, z], [0.0, 1.0, z], [1.0, 1.0, z]],
        ]
    }

    #[test]
    fn flat_box_prefers_plus_z() {
        // A closed-ish box has no overhangs any way up; +Z wins the tie (earliest candidate).
        let mut tris: Vec<[V3; 3]> = Vec::new();
        // bottom (normal -Z) + top (normal +Z) of a 1x1x1 box — enough to exercise the scan.
        tris.extend(down_quad(0.0));
        tris.extend([
            [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]],
            [[1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]],
        ]);
        assert_eq!(best_up(&tris, &[]), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn an_overhang_steers_away_from_plus_z() {
        // A downward-facing roof up at z=10 over a base at z=0: printed +Z the roof is a big
        // unsupported overhang, so best_up must NOT pick +Z, and its chosen up has ~no overhang.
        let mut tris: Vec<[V3; 3]> = Vec::new();
        tris.extend(down_quad(0.0)); // base
        tris.extend(down_quad(10.0)); // roof (overhang when printed +Z)
        let up = best_up(&tris, &[]);
        assert_ne!(up, [0.0, 0.0, 1.0], "should avoid the orientation with the roof overhang");
        assert!(overhang_score(&tris, up) <= overhang_score(&tris, [0.0, 0.0, 1.0]));
        assert!(overhang_score(&tris, up) < 0.5, "chosen up should have ~no overhang");
    }

    #[test]
    fn cut_normals_are_candidates() {
        let cn = [0.123, 0.456, 0.789];
        assert!(candidates(&[cn]).contains(&geom::normalize(cn)));
    }
}
