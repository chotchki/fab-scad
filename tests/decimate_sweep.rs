//! W.3.41.1 — an IGNORED measurement harness, not a gate test. Publish decimates the preview mesh variant
//! to a hard-coded 20K triangles (`publish_native.rs`/`publish_web.rs`/`jobs.rs`); on a detailed part like
//! the shower_holder that reads FACETED in the hotchkiss.io /3d embed. This sweeps the budget on real
//! models and reports, per budget: the achieved triangle count, the 3MF byte size (the embed payload cost),
//! and the one-sided surface deviation (how far the ORIGINAL surface sits from the decimated one, as a % of
//! the model's bbox diagonal — the faceting proxy). The knee in the deviation curve is the tuning target.
//!
//! Run: `cargo test -p fab-scad --release --test decimate_sweep -- --ignored --nocapture`

use fab_scad::decimate::decimate_mesh;
use fab_scad::geomsg::Source;
use fab_scad::geomsvc::render_source_to_solid;
use fab_scad::threemf_out::to_3mf_bytes;

/// The repo root — resolves `include <BOSL2/std.scad>` (root/libs) and `include <hook.scad>` (beside model).
fn root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

/// The budget ladder. `None` = the full mesh as-is (the deviation floor + the size ceiling).
const LADDER: &[Option<usize>] = &[
    Some(20_000), // today's default
    Some(30_000),
    Some(40_000),
    Some(60_000),
    Some(80_000),
    Some(120_000),
    Some(160_000),
    None,
];

/// Squared distance from point `p` to triangle `abc` (Ericson, Real-Time Collision Detection §5.1.5).
fn point_tri_dist2(p: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let sub = |u: [f64; 3], v: [f64; 3]| [u[0] - v[0], u[1] - v[1], u[2] - v[2]];
    let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return dot(ap, ap); // vertex region A
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return dot(bp, bp); // vertex region B
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3); // edge AB
        let q = [a[0] + v * ab[0], a[1] + v * ab[1], a[2] + v * ab[2]];
        let qp = sub(p, q);
        return dot(qp, qp);
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return dot(cp, cp); // vertex region C
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6); // edge AC
        let q = [a[0] + w * ac[0], a[1] + w * ac[1], a[2] + w * ac[2]];
        let qp = sub(p, q);
        return dot(qp, qp);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6)); // edge BC
        let q = [
            b[0] + w * (c[0] - b[0]),
            b[1] + w * (c[1] - b[1]),
            b[2] + w * (c[2] - b[2]),
        ];
        let qp = sub(p, q);
        return dot(qp, qp);
    }
    // interior — project onto the plane via barycentric
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let q = [
        a[0] + ab[0] * v + ac[0] * w,
        a[1] + ab[1] * v + ac[1] * w,
        a[2] + ab[2] * v + ac[2] * w,
    ];
    let qp = sub(p, q);
    dot(qp, qp)
}

/// One model, one full sweep — renders, decimates across the ladder, prints the table.
fn sweep(name: &str, rel_scad: &str) {
    let root = root();
    let path = format!("{root}/{rel_scad}");
    let solid = render_source_to_solid(&Source::Path(path), Some(&root))
        .unwrap_or_else(|e| panic!("render {name}: {e}"));

    let (verts, tris) = solid.to_indexed();
    let v: Vec<[f64; 3]> = verts.iter().map(|p| p.to_array()).collect();
    let t: Vec<[u32; 3]> = tris.iter().map(|tri| tri.indices()).collect();
    let c: Option<Vec<[f64; 4]>> = solid
        .vertex_colors()
        .map(|cs| cs.iter().map(|x| [x.r, x.g, x.b, x.a]).collect());

    // bbox diagonal — the deviation denominator (scale-free faceting %).
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in &v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();

    // Subsample the full-mesh vertices as deviation probes (stride to ~1500 — enough to catch lost detail,
    // cheap enough to brute-force against every low triangle).
    let stride = (v.len() / 1500).max(1);
    let probes: Vec<[f64; 3]> = v.iter().step_by(stride).copied().collect();

    let full_bytes = solid.to_3mf_bytes().len();
    println!(
        "\n=== {name}: full = {} tris, {} verts, {:.2} MB 3MF, bbox diag {:.1} mm | {} probes ===",
        t.len(),
        v.len(),
        full_bytes as f64 / 1.048_576e6,
        diag,
        probes.len(),
    );
    println!("  budget      tris     3MF MB   %full   dev.mean%  dev.max%");

    for &budget in LADDER {
        let d = match budget {
            Some(b) => decimate_mesh(&v, &t, c.as_deref(), b),
            None => fab_scad::decimate::Decimated {
                verts: v.clone(),
                tris: t.clone(),
                colors: c.clone(),
            },
        };
        let bytes = to_3mf_bytes(&d.verts, &d.tris, d.colors.as_deref()).len();

        // one-sided deviation: each probe (a point ON the original surface) → nearest low triangle.
        let (mut sum, mut max) = (0.0_f64, 0.0_f64);
        for &p in &probes {
            let mut best = f64::MAX;
            for tri in &d.tris {
                let a = d.verts[tri[0] as usize];
                let bb = d.verts[tri[1] as usize];
                let cc = d.verts[tri[2] as usize];
                let dist2 = point_tri_dist2(p, a, bb, cc);
                if dist2 < best {
                    best = dist2;
                    if best == 0.0 {
                        break;
                    }
                }
            }
            let dist = best.sqrt();
            sum += dist;
            max = max.max(dist);
        }
        let mean = sum / probes.len() as f64;
        let label = budget
            .map(|b| b.to_string())
            .unwrap_or_else(|| "full".into());
        println!(
            "  {:>7}  {:>8}   {:>6.2}   {:>4.0}%   {:>8.3}   {:>7.3}",
            label,
            d.tris.len(),
            bytes as f64 / 1.048_576e6,
            100.0 * d.tris.len() as f64 / t.len() as f64,
            100.0 * mean / diag,
            100.0 * max / diag,
        );
    }
}

#[test]
#[ignore = "measurement harness — run explicitly with --ignored --nocapture"]
fn budget_sweep() {
    // The problem case (detailed, lots of holes/rounds) + its lighter sibling, to see whether one fixed
    // budget serves both or an adaptive fraction fits better.
    sweep("shower_holder", "models/shower_holder/shower_holder.scad");
    sweep(
        "shower_holder_mini",
        "models/shower_holder/shower_holder_mini.scad",
    );
}
