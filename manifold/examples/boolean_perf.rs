//! Kernel-level boolean perf driver (Phase BU): OBJ pair in, ingest UNTIMED, the timed region is
//! the boolean alone (ends in `num_tri()` so nothing is lazily skipped). Two ways to read it:
//! `RUST_LOG=manifold::boolean=debug` prints the per-stage split the kernel already traces;
//! `samply record` on this binary gives function-level attribution.
//!
//!   cargo run --release [--features par] --example boolean_perf -- \
//!     goldens/models/Generic_Twin_7081.1.t0_left.obj \
//!     goldens/models/Generic_Twin_7081.1.t0_right.obj add 3
//!
//! Inputs are `.obj` or the goldens' FMGL `.bin`; the optional trailing `dx dy dz` shifts B before
//! ingest (the harness's sphere-vs-shifted-sphere cases from one frozen input):
//!
//!   ... goldens/inputs/sphere128.bin goldens/inputs/sphere128.bin add 20 7 3 2

use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::mesh::{Mesh, MeshGl};
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

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

/// The goldens' `FMGL` little-endian MeshGL dump (see tests/m7_golden_mode.rs): magic, u32
/// num_prop, u64 f64-count, f64s, u64 u32-count, u32s.
fn read_mesh_bin(path: &Path) -> MeshGl {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&raw[..4], b"FMGL", "bad magic in {}", path.display());
    let mut at = 4usize;
    let read_u32 = |raw: &[u8], at: &mut usize| {
        let v = u32::from_le_bytes(raw[*at..*at + 4].try_into().unwrap());
        *at += 4;
        v
    };
    let read_u64 = |raw: &[u8], at: &mut usize| {
        let v = u64::from_le_bytes(raw[*at..*at + 8].try_into().unwrap());
        *at += 8;
        v as usize
    };
    let num_prop = read_u32(&raw, &mut at) as usize;
    let n_f64 = read_u64(&raw, &mut at);
    let vert_properties: Vec<f64> = (0..n_f64)
        .map(|_| {
            let v = f64::from_bits(u64::from_le_bytes(raw[at..at + 8].try_into().unwrap()));
            at += 8;
            v
        })
        .collect();
    let n_u32 = read_u64(&raw, &mut at);
    let tri_verts: Vec<u32> = (0..n_u32).map(|_| read_u32(&raw, &mut at)).collect();
    MeshGl {
        num_prop,
        vert_properties,
        tri_verts,
        ..Default::default()
    }
}

fn load(path: &Path) -> MeshGl {
    if path.extension().is_some_and(|e| e == "bin") {
        read_mesh_bin(path)
    } else {
        load_obj(path)
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let (left, right, op_name, reps, shift) = match args.as_slice() {
        [_, l, r, o] => (l, r, o, 3usize, None),
        [_, l, r, o, n] => (l, r, o, n.parse().expect("reps"), None),
        [_, l, r, o, n, dx, dy, dz] => (
            l,
            r,
            o,
            n.parse().expect("reps"),
            Some([
                dx.parse::<f64>().expect("dx"),
                dy.parse::<f64>().expect("dy"),
                dz.parse::<f64>().expect("dz"),
            ]),
        ),
        _ => {
            eprintln!("usage: boolean_perf <left.obj|.bin> <right.obj|.bin> <add|sub|int> [reps [dx dy dz]]");
            std::process::exit(2);
        }
    };
    let op = match op_name.as_str() {
        "add" => OpType::Add,
        "sub" => OpType::Subtract,
        "int" => OpType::Intersect,
        other => panic!("unknown op {other} (add|sub|int)"),
    };

    let a = Mesh::from_mesh_gl(&load(Path::new(left))).unwrap();
    let mut b_gl = load(Path::new(right));
    if let Some([dx, dy, dz]) = shift {
        for row in b_gl.vert_properties.chunks_exact_mut(b_gl.num_prop) {
            row[0] += dx;
            row[1] += dy;
            row[2] += dz;
        }
    }
    let b = Mesh::from_mesh_gl(&b_gl).unwrap();
    eprintln!("inputs: {} tri / {} tri", a.num_tri(), b.num_tri());

    let mut times: Vec<f64> = Vec::new();
    for i in 0..reps {
        let t = Instant::now();
        let out = boolean(&a, &b, op);
        black_box(out.num_tri());
        let ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("rep {i}: {ms:.2} ms ({} tri out)", out.num_tri());
        times.push(ms);
    }
    times.sort_by(f64::total_cmp);
    println!("median: {:.2} ms", times[times.len() / 2]);
}
