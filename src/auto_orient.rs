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

/// Triangle area.
fn tri_area(t: &[V3; 3]) -> f64 {
    0.5 * geom::norm(geom::cross(geom::sub(t[1], t[0]), geom::sub(t[2], t[0])))
}

/// `(unsupported-overhang area, bed-contact area)` of `tris` printed along `up`. A downward-facing
/// face (normal past the support angle below horizontal) counts as overhang if it sits above the
/// bed-contact layer, or as contact if it rests within `BED_EPS` of the lowest point — so the flat
/// bottom on the bed is NOT scored as the worst overhang (the trap the naive version falls into).
fn areas(tris: &[[V3; 3]], up: V3) -> (f64, f64) {
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
            continue; // not a downward-facing face
        }
        let area = tri_area(t);
        let centroid_h = (geom::dot(t[0], up) + geom::dot(t[1], up) + geom::dot(t[2], up)) / 3.0;
        if centroid_h - z_min > BED_EPS {
            overhang += area;
        } else {
            contact += area; // resting on the bed
        }
    }
    (overhang, contact)
}

/// Unsupported-overhang area minus a small bed-contact reward — lower is better. Retained as the
/// scalar overhang metric; `best_up` now scores overhang and contact separately (below).
pub fn overhang_score(tris: &[[V3; 3]], up: V3) -> f64 {
    let (overhang, contact) = areas(tris, up);
    overhang - BED_REWARD * contact
}

/// The least-support build-up for a piece mesh. PRIMARY: minimize unsupported overhang (avoid
/// supports — the never-trim intent). TIE-BREAK: among orientations whose overhang is within a hair
/// of the minimum, lay the LARGEST face on the bed (most contact) — the most stable, best-adhering,
/// lowest print, so a plain slab lies flat instead of standing on a cut face. Remaining ties fall to
/// the earliest candidate (+Z). Returns +Z for an empty mesh.
///
/// A joint that wanted its cut-face down but lost to a flatter face just downgrades to a bolt
/// downstream (the slicer's feasibility gate) — stability is preferred, feasibility follows.
pub fn best_up(tris: &[[V3; 3]], cut_normals: &[V3]) -> V3 {
    if tris.is_empty() {
        return [0.0, 0.0, 1.0];
    }
    // "Same overhang" tolerance: a hair of the total surface area — absorbs float noise and genuine
    // ties without letting a real overhang buy its way to a more stable face.
    let total: f64 = tris.iter().map(tri_area).sum();
    let tol = 1e-3 * total + 1e-9;
    let scored: Vec<(V3, f64, f64)> = candidates(cut_normals)
        .into_iter()
        .map(|u| {
            let (o, c) = areas(tris, u);
            (u, o, c)
        })
        .collect();
    let min_over = scored.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
    scored
        .iter()
        .enumerate()
        .filter(|(_, s)| s.1 <= min_over + tol)
        // Max contact; on a contact tie, the earliest candidate (smaller index) wins.
        .max_by(|a, b| {
            a.1 .2
                .partial_cmp(&b.1 .2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.cmp(&a.0))
        })
        .map(|(_, s)| s.0)
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
        assert_ne!(
            up,
            [0.0, 0.0, 1.0],
            "should avoid the orientation with the roof overhang"
        );
        assert!(overhang_score(&tris, up) <= overhang_score(&tris, [0.0, 0.0, 1.0]));
        assert!(
            overhang_score(&tris, up) < 0.5,
            "chosen up should have ~no overhang"
        );
    }

    #[test]
    fn cut_normals_are_candidates() {
        let cn = [0.123, 0.456, 0.789];
        assert!(candidates(&[cn]).contains(&geom::normalize(cn)));
    }

    // A closed box `w × d × h` at the origin, 12 tris, outward normals.
    fn box_mesh(w: f64, d: f64, h: f64) -> Vec<[V3; 3]> {
        let v = [
            [0.0, 0.0, 0.0],
            [w, 0.0, 0.0],
            [w, d, 0.0],
            [0.0, d, 0.0],
            [0.0, 0.0, h],
            [w, 0.0, h],
            [w, d, h],
            [0.0, d, h],
        ];
        let faces = [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [2, 3, 7, 6],
            [1, 2, 6, 5],
            [0, 4, 7, 3],
        ];
        let mut tris = Vec::new();
        for f in faces {
            tris.push([v[f[0]], v[f[1]], v[f[2]]]);
            tris.push([v[f[0]], v[f[2]], v[f[3]]]);
        }
        tris
    }

    #[test]
    fn tall_slab_lies_on_its_largest_face() {
        // Box tall in Z: the largest face is the 60×120 side (normal ±Y). best_up must lay THAT on
        // the bed (up = +Y), not stand it 120mm tall on the small 60×40 face (+Z). No overhang in any
        // box orientation, so this is purely the stability tie-break.
        let tris = box_mesh(60.0, 40.0, 120.0);
        let up = best_up(&tris, &[]);
        assert_eq!(
            up,
            [0.0, 1.0, 0.0],
            "should lay the largest face down, not stand tall"
        );
        assert_ne!(up, [0.0, 0.0, 1.0]);
        // The chosen orientation genuinely has the most bed contact of any overhang-free candidate.
        let best_contact = areas(&tris, up).1;
        for u in candidates(&[]) {
            let (o, c) = areas(&tris, u);
            if o <= 1e-6 {
                assert!(
                    c <= best_contact + 1e-6,
                    "a flatter face existed but wasn't chosen"
                );
            }
        }
    }
}
