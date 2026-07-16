//! M.7.2 — the golden FREEZE + the golden-mode lane (the correctness memory that outlives the C++).
//!
//! Two halves in one file so the schema lives once:
//! - `freeze_oracle_goldens` (CUT at M.7.4 with the C++; see the freeze HISTORY marker below):
//!   captured the C++ reference metrics per
//!   corpus case × op into `goldens/oracle_goldens.json`, alongside the FINGERPRINT of our own
//!   output; also freezes the C++-generated inputs (spheres/cylinder) as `goldens/inputs/*.bin`.
//!   Byte-idempotent when nothing changed.
//! - `golden_mode` (default features, native): replays the corpus WITHOUT the C++ — our current
//!   outputs vs the frozen C++ metrics at the live differential's own tolerances, plus fingerprint
//!   equality for byte stability. This lane is what survives the cut. (wasm bit-identity is the
//!   in-code M.6 corpus's job — this lane reads files, which the wasmtime runner doesn't preopen.)
#![cfg(not(target_arch = "wasm32"))]

use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::golden;
use fab_manifold::mesh::{Mesh, MeshGl};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

// ── schema ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct GoldenCase {
    /// "<left> <op> <right>" — stable id.
    name: String,
    /// C++ reference metrics, f64s bit-recorded (u64 = to_bits) so JSON can't round them.
    cpp_volume_bits: u64,
    cpp_area_bits: u64,
    cpp_genus: i32,
    cpp_bbox_bits: [u64; 6],
    /// Whether genus was a LIVE gate for this case (the self_intersect diff/intersect waiver).
    genus_checked: bool,
    /// FNV-1a fingerprint of OUR output (`golden::mesh`) — the byte-exact snapshot.
    ours_fingerprint: u64,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Goldens {
    schema: u32,
    cases: Vec<GoldenCase>,
}

// ── corpus definition (shared by freeze + replay) ───────────────────────────────────────────────

/// (left input, right input, op, op label, genus gated?)
struct CorpusCase {
    name: String,
    a: MeshGl,
    b: MeshGl,
    op: OpType,
    /// Was consumed by the (cut) oracle freeze; golden_mode reads the FROZEN flag instead.
    #[allow(dead_code)]
    genus_checked: bool,
}

fn op_label(op: OpType) -> &'static str {
    match op {
        OpType::Add => "∪",
        OpType::Subtract => "−",
        OpType::Intersect => "∩",
    }
}

/// The frozen corpus: every OBJ pair × all three ops (the M.2.4 gate set), plus the C++-generated
/// primitive booleans (K.0 lineage) whose inputs live in `goldens/inputs/*.bin`.
fn corpus() -> Vec<CorpusCase> {
    let dir = goldens_dir();
    let obj = |n: &str| load_obj(&dir.join("models").join(format!("{n}.obj")));
    let bin = |n: &str| read_mesh_bin(&dir.join("inputs").join(format!("{n}.bin")));

    let pairs = [
        ("Havocglass8_left", "Havocglass8_right", true),
        ("Cray_left", "Cray_right", true),
        (
            "Generic_Twin_7863.1.t0_left",
            "Generic_Twin_7863.1.t0_right",
            true,
        ),
        // ε-invalid self-intersecting inputs: genus was only ever gated on the UNION (M.2.4).
        ("self_intersectA", "self_intersectB", false),
        (
            "Generic_Twin_7081.1.t0_left",
            "Generic_Twin_7081.1.t0_right",
            true,
        ),
    ];
    let mut cases = Vec::new();
    for (l, r, genus_on_diff) in pairs {
        let (a, b) = (obj(l), obj(r));
        for op in [OpType::Add, OpType::Subtract, OpType::Intersect] {
            cases.push(CorpusCase {
                name: format!("{l} {} {r}", op_label(op)),
                a: a.clone(),
                b: b.clone(),
                op,
                genus_checked: if op == OpType::Add {
                    true
                } else {
                    genus_on_diff
                },
            });
        }
    }

    // Primitive booleans over the frozen C++-generated inputs.
    let sphere64 = bin("sphere64");
    let sphere128 = bin("sphere128");
    let cylinder64 = bin("cylinder64");
    let cube = Mesh::cube(fab_manifold::linalg::Vec3::new(10.0, 10.0, 10.0), true)
        .unwrap()
        .to_mesh_gl();
    for (name, a, b, op) in [
        (
            "sphere64 ∪ cube",
            sphere64.clone(),
            cube.clone(),
            OpType::Add,
        ),
        (
            "sphere128 − sphere64",
            sphere128,
            shifted(&sphere64, 3.0, 1.0, 2.0),
            OpType::Subtract,
        ),
        ("cylinder64 ∩ cube", cylinder64, cube, OpType::Intersect),
    ] {
        cases.push(CorpusCase {
            name: name.to_string(),
            a,
            b,
            op,
            genus_checked: true,
        });
    }
    cases
}

/// Run OUR pipeline on a case: full ingest (the C++-ctor-equal path) + the boolean.
fn run_ours(c: &CorpusCase) -> Mesh {
    let a = Mesh::from_mesh_gl(&c.a).unwrap();
    let b = Mesh::from_mesh_gl(&c.b).unwrap();
    boolean(&a, &b, c.op)
}

// ── tiny IO (obj + bin), no deps ────────────────────────────────────────────────────────────────

fn load_obj(path: &Path) -> MeshGl {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut verts: Vec<f64> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                for _ in 0..3 {
                    verts.push(it.next().expect("v x y z").parse().expect("f64"));
                }
            }
            Some("f") => {
                for _ in 0..3 {
                    let tok = it.next().expect("f a b c");
                    let idx: u32 = tok.split('/').next().unwrap().parse().expect("index");
                    tris.push(idx - 1);
                }
            }
            _ => {}
        }
    }
    MeshGl {
        num_prop: 3,
        vert_properties: verts,
        tri_verts: tris,
        ..Default::default()
    }
}

/// `FMGL` little-endian MeshGL dump: magic, u32 num_prop, u64 n_prop_f64s, f64s, u64 n_tri_u32s,
/// u32s. Byte-exact both ways. (Writer is freeze-side only.)
#[allow(dead_code)]
fn write_mesh_bin(path: &Path, m: &MeshGl) {
    let mut out: Vec<u8> = b"FMGL".to_vec();
    out.extend_from_slice(&(m.num_prop as u32).to_le_bytes());
    out.extend_from_slice(&(m.vert_properties.len() as u64).to_le_bytes());
    for v in &m.vert_properties {
        out.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    out.extend_from_slice(&(m.tri_verts.len() as u64).to_le_bytes());
    for t in &m.tri_verts {
        out.extend_from_slice(&t.to_le_bytes());
    }
    // Idempotent regen: only touch the file when the bytes changed.
    if std::fs::read(path).ok().as_deref() != Some(out.as_slice()) {
        std::fs::write(path, out).unwrap();
    }
}

fn read_mesh_bin(path: &Path) -> MeshGl {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&raw[..4], b"FMGL", "bad magic in {}", path.display());
    let mut at = 4usize;
    let u32_at = |raw: &[u8], at: &mut usize| {
        let v = u32::from_le_bytes(raw[*at..*at + 4].try_into().unwrap());
        *at += 4;
        v
    };
    let num_prop = u32_at(&raw, &mut at) as usize;
    let n = u64::from_le_bytes(raw[at..at + 8].try_into().unwrap()) as usize;
    at += 8;
    let mut vert_properties = Vec::with_capacity(n);
    for _ in 0..n {
        vert_properties.push(f64::from_bits(u64::from_le_bytes(
            raw[at..at + 8].try_into().unwrap(),
        )));
        at += 8;
    }
    let nt = u64::from_le_bytes(raw[at..at + 8].try_into().unwrap()) as usize;
    at += 8;
    let mut tri_verts = Vec::with_capacity(nt);
    for _ in 0..nt {
        tri_verts.push(u32_at(&raw, &mut at));
    }
    MeshGl {
        num_prop,
        vert_properties,
        tri_verts,
        ..Default::default()
    }
}

fn shifted(gl: &MeshGl, dx: f64, dy: f64, dz: f64) -> MeshGl {
    let mut out = gl.clone();
    for row in out.vert_properties.chunks_exact_mut(gl.num_prop) {
        row[0] += dx;
        row[1] += dy;
        row[2] += dz;
    }
    out
}

// ── the freeze (HISTORY) ───────────────────────────────────────────────────────────────────────
// `freeze_oracle_goldens` lived here until M.7.4 — an oracle-feature #[ignore]d capture that wrote
// goldens/inputs/*.bin (C++-generated meshes) + oracle_goldens.json (the C++ reference metrics,
// bit-recorded, per corpus case × op). The C++ is CUT; the frozen files are the correctness memory.
// To re-freeze against a future reference, resurrect it from git history (pre-M.7.4).

// ── golden mode (default features — what survives the cut) ──────────────────────────────────────

#[test]
fn golden_mode() {
    let path = goldens_dir().join("oracle_goldens.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!("goldens not frozen yet — run the freeze first (see goldens/README.md)");
    };
    let goldens: Goldens = serde_json::from_str(&text).unwrap();
    assert_eq!(goldens.schema, 1);

    let corpus = corpus();
    assert_eq!(
        corpus.len(),
        goldens.cases.len(),
        "corpus/golden size mismatch — refreeze"
    );

    for (c, g) in corpus.iter().zip(&goldens.cases) {
        assert_eq!(c.name, g.name, "corpus order changed — refreeze");
        // The big twin is a ~64.5M-candidate stress — minutes in the serial debug lane (same skip
        // discipline as the live big_twin test); the release lane asserts it.
        if cfg!(debug_assertions) && c.name.contains("Generic_Twin_7081") {
            eprintln!("(debug lane: skipping {})", c.name);
            continue;
        }
        let ours = run_ours(c);

        // The live differential's own gates, replayed against the frozen C++ facts.
        let cpp_vol = f64::from_bits(g.cpp_volume_bits);
        let rel = (ours.volume() - cpp_vol).abs() / cpp_vol.abs().max(1e-9);
        assert!(
            rel < 1e-9,
            "{}: volume {} vs frozen C++ {cpp_vol} (rel {rel:.3e})",
            g.name,
            ours.volume()
        );

        if g.genus_checked {
            assert_eq!(
                fab_manifold::check::genus(&ours),
                g.cpp_genus,
                "{}: genus vs frozen C++",
                g.name
            );
        }

        let bb = ours.b_box;
        let frozen: Vec<f64> = g.cpp_bbox_bits.iter().map(|&b| f64::from_bits(b)).collect();
        for (x, y) in [bb.min.x, bb.min.y, bb.min.z, bb.max.x, bb.max.y, bb.max.z]
            .iter()
            .zip(&frozen)
        {
            assert!(
                (x - y).abs() <= 1e-9 * y.abs().max(1.0),
                "{}: bbox {x} vs frozen C++ {y}",
                g.name
            );
        }

        // Byte stability: our output is bit-identical to the freeze-day output.
        assert_eq!(
            golden::mesh(&ours),
            g.ours_fingerprint,
            "{}: our output fingerprint drifted from the freeze",
            g.name
        );
    }
    eprintln!(
        "golden mode ✓ {} cases vs frozen C++ metrics + fingerprints",
        goldens.cases.len()
    );
}
