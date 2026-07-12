//! Auto-orient (#42): the smart-default print orientation for a piece — the build-up direction
//! needing the least support. A DETERMINISTIC discrete scan over a fixed candidate set scored from
//! the piece mesh, NOT an SO(3) optimization (chotchki's framing: a straightforward good guess).
//! Pure triangle math, so `fab slice` and the GUI compute byte-identical picks.

use fab_lang::Vec3;

/// Overhang threshold: a face is unsupported if it points downward more than this from horizontal.
/// Tunable like the slicer's gate; refine with a printed coupon.
pub const SUPPORT_ANGLE: f64 = 45.0;
/// Bed-contact layer thickness (mm): downward faces within this of the lowest point rest ON the
/// bed (not overhangs) and earn a small reward instead.
const BED_EPS: f64 = 0.6;
/// How much a flat bed footprint discounts the score (only breaks near-ties — it must not let a
/// big-overhang orientation win on contact alone).
const BED_REWARD: f64 = 0.1;
/// PREFER-FLAT budget: how much overhang a piece may carry and still be laid FLAT rather than
/// tilted. A candidate whose overhang is within this fraction of the piece's total surface area of
/// the least-overhang candidate counts as "flat-acceptable" and competes on bed contact — so a
/// stable flat face beats a precarious 45° tilt that only shaves a little support (the structured
/// cut-piece case — frame corners, walls — where the naive least-overhang pick stands them on a
/// corner). Only a genuinely overhang-heavy piece (no orientation under budget) still tilts.
/// Tunable like [`SUPPORT_ANGLE`]; `FAB_ORIENT_DEBUG=1` dumps per-candidate overhang/contact so the
/// budget can be set from real pieces.
pub const FLAT_BUDGET_FRAC: f64 = 0.15;

/// Candidate build-ups, in a FIXED order (so ties resolve deterministically; +Z wins first): the 6
/// axis-aligned ups, four 45° tilts of +Z toward each horizontal axis, then the piece's incident
/// cut-face normals (cut-face-on-bed usually prints best AND keeps onion joints feasible).
pub fn candidates(cut_normals: &[Vec3]) -> Vec<Vec3> {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let mut c = vec![
        Vec3::new(0.0, 0.0, 1.0), // +Z first: the tie-break winner
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(s, 0.0, s),
        Vec3::new(-s, 0.0, s),
        Vec3::new(0.0, s, s),
        Vec3::new(0.0, -s, s),
    ];
    c.extend(cut_normals.iter().map(|&n| n.normalize()));
    c
}

/// Triangle area.
fn tri_area(t: &[Vec3; 3]) -> f64 {
    0.5 * (t[1] - t[0]).cross(t[2] - t[0]).length()
}

/// `(unsupported-overhang area, bed-contact area)` of `tris` printed along `up`. A downward-facing
/// face (normal past the support angle below horizontal) counts as overhang if it sits above the
/// bed-contact layer, or as contact if it rests within `BED_EPS` of the lowest point — so the flat
/// bottom on the bed is NOT scored as the worst overhang (the trap the naive version falls into).
fn areas(tris: &[[Vec3; 3]], up: Vec3) -> (f64, f64) {
    let up = up.normalize();
    let cos_t = SUPPORT_ANGLE.to_radians().cos(); // 0.707 at 45°
    let z_min = tris
        .iter()
        .flat_map(|t| t.iter())
        .map(|&v| v.dot(up))
        .fold(f64::INFINITY, f64::min);
    let (mut overhang, mut contact) = (0.0, 0.0);
    for t in tris {
        let normal = (t[1] - t[0]).cross(t[2] - t[0]).normalize();
        if normal.dot(up) >= -cos_t {
            continue; // not a downward-facing face
        }
        let area = tri_area(t);
        let centroid_h = (t[0].dot(up) + t[1].dot(up) + t[2].dot(up)) / 3.0;
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
pub fn overhang_score(tris: &[[Vec3; 3]], up: Vec3) -> f64 {
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
pub fn best_up(tris: &[[Vec3; 3]], cut_normals: &[Vec3]) -> Vec3 {
    if tris.is_empty() {
        return Vec3::new(0.0, 0.0, 1.0);
    }
    let total: f64 = tris.iter().map(tri_area).sum();
    let scored: Vec<(Vec3, f64, f64)> = candidates(cut_normals)
        .into_iter()
        .map(|u| {
            let (o, c) = areas(tris, u);
            (u, o, c)
        })
        .collect();
    let min_over = scored.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
    // PREFER-FLAT: admit every orientation whose overhang is within a support BUDGET of the best
    // (float-noise tolerance + [`FLAT_BUDGET_FRAC`] of the surface area), then lay the FLATTEST of
    // them down (max bed contact) — so a stable flat face wins over a 45° tilt that only shaves a
    // little overhang. Only when nothing is under budget does the piece genuinely tilt.
    let budget = min_over + FLAT_BUDGET_FRAC * total + 1e-9;
    let winner = scored
        .iter()
        .enumerate()
        .filter(|(_, s)| s.1 <= budget)
        // Max contact; on a contact tie, the earliest candidate (smaller index) wins.
        .max_by(|a, b| {
            a.1.2
                .partial_cmp(&b.1.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.cmp(&a.0))
        });
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("FAB_ORIENT_DEBUG").is_some() {
        let w = winner.map(|(i, _)| i).unwrap_or(usize::MAX);
        eprintln!(
            "[orient] tris={} area={total:.0} min_over={min_over:.1} budget={budget:.1} -> {:?}",
            tris.len(),
            winner.map(|(_, s)| s.0)
        );
        for (i, s) in scored.iter().enumerate() {
            eprintln!(
                "  {} {:?} overhang={:.1} contact={:.1}",
                if i == w { "*" } else { " " },
                s.0,
                s.1,
                s.2
            );
        }
    }
    winner.map(|(_, s)| s.0).unwrap_or(Vec3::new(0.0, 0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A unit quad (two tris) in the plane z = `z`, wound so its normal is -Z (downward-facing).
    fn down_quad(z: f64) -> [[Vec3; 3]; 2] {
        [
            [
                Vec3::new(0.0, 0.0, z),
                Vec3::new(0.0, 1.0, z),
                Vec3::new(1.0, 0.0, z),
            ],
            [
                Vec3::new(1.0, 0.0, z),
                Vec3::new(0.0, 1.0, z),
                Vec3::new(1.0, 1.0, z),
            ],
        ]
    }

    #[test]
    fn flat_box_prefers_plus_z() {
        // A closed-ish box has no overhangs any way up; +Z wins the tie (earliest candidate).
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        // bottom (normal -Z) + top (normal +Z) of a 1x1x1 box — enough to exercise the scan.
        tris.extend(down_quad(0.0));
        tris.extend([
            [
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(0.0, 1.0, 1.0),
            ],
            [
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(0.0, 1.0, 1.0),
            ],
        ]);
        assert_eq!(best_up(&tris, &[]), Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn an_overhang_steers_away_from_plus_z() {
        // A downward-facing roof up at z=10 over a base at z=0: printed +Z the roof is a big
        // unsupported overhang, so best_up must NOT pick +Z, and its chosen up has ~no overhang.
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        tris.extend(down_quad(0.0)); // base
        tris.extend(down_quad(10.0)); // roof (overhang when printed +Z)
        let up = best_up(&tris, &[]);
        assert_ne!(
            up,
            Vec3::new(0.0, 0.0, 1.0),
            "should avoid the orientation with the roof overhang"
        );
        assert!(overhang_score(&tris, up) <= overhang_score(&tris, Vec3::new(0.0, 0.0, 1.0)));
        assert!(
            overhang_score(&tris, up) < 0.5,
            "chosen up should have ~no overhang"
        );
    }

    // A quad in the plane z=`z` spanning [x0,x1]×[y0,y1], wound so its normal is -Z (downward).
    fn quad_down(x0: f64, y0: f64, x1: f64, y1: f64, z: f64) -> [[Vec3; 3]; 2] {
        let p = [
            Vec3::new(x0, y0, z),
            Vec3::new(x0, y1, z),
            Vec3::new(x1, y1, z),
            Vec3::new(x1, y0, z),
        ];
        [[p[0], p[1], p[2]], [p[0], p[2], p[3]]]
    }

    #[test]
    fn prefer_flat_keeps_a_small_ceiling_overhang_on_the_bed() {
        // T.3: a big flat face (lots of bed contact) with a SMALL downward ceiling overhang above it
        // — the structured cut-piece case (a slab with an interior overhang). Least-overhang ALONE
        // flees the flat face: any orientation where the ceiling isn't downward has ZERO overhang,
        // so the naive pick stands the piece off its big face onto an edge to save a sliver of
        // support. Prefer-flat instead accepts the small overhang (under budget) to keep the large,
        // stable face on the bed.
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        tris.extend(quad_down(0.0, 0.0, 10.0, 10.0, 0.0)); // 10×10 bed face (contact when up=+Z)
        tris.extend(quad_down(4.0, 4.0, 6.0, 6.0, 5.0)); // 2×2 ceiling above it (overhang when up=+Z)

        let up = best_up(&tris, &[]);
        assert_eq!(
            up,
            Vec3::new(0.0, 0.0, 1.0),
            "a small ceiling overhang must not lift the big face off the bed"
        );
        // Prove prefer-flat OVERRODE least-overhang: the chosen flat up genuinely carries overhang,
        // yet a zero-overhang orientation existed (-Z) — so pure min-overhang would NOT have picked it.
        assert!(
            areas(&tris, up).0 > 0.0,
            "the chosen flat orientation does carry the small overhang"
        );
        assert!(
            areas(&tris, Vec3::new(0.0, 0.0, -1.0)).0 < areas(&tris, up).0,
            "a lower-overhang orientation existed but prefer-flat kept the flat face down"
        );
    }

    #[test]
    fn cut_normals_are_candidates() {
        let cn = Vec3::new(0.123, 0.456, 0.789);
        assert!(candidates(&[cn]).contains(&cn.normalize()));
    }

    // A closed box `w × d × h` at the origin, 12 tris, outward normals.
    fn box_mesh(w: f64, d: f64, h: f64) -> Vec<[Vec3; 3]> {
        let v = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(w, 0.0, 0.0),
            Vec3::new(w, d, 0.0),
            Vec3::new(0.0, d, 0.0),
            Vec3::new(0.0, 0.0, h),
            Vec3::new(w, 0.0, h),
            Vec3::new(w, d, h),
            Vec3::new(0.0, d, h),
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
            Vec3::new(0.0, 1.0, 0.0),
            "should lay the largest face down, not stand tall"
        );
        assert_ne!(up, Vec3::new(0.0, 0.0, 1.0));
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
