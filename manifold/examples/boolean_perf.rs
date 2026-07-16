//! Kernel-level boolean perf driver (Phase BU): OBJ pair in, ingest UNTIMED, the timed region is
//! the boolean alone (ends in `num_tri()` so nothing is lazily skipped). Two ways to read it:
//! `RUST_LOG=manifold::boolean=debug` prints the per-stage split the kernel already traces;
//! `samply record` on this binary gives function-level attribution.
//!
//!   cargo run --release [--features par] --example boolean_perf -- \
//!     goldens/models/Generic_Twin_7081.1.t0_left.obj \
//!     goldens/models/Generic_Twin_7081.1.t0_right.obj add 3

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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let (left, right, op_name, reps) = match args.as_slice() {
        [_, l, r, o] => (l, r, o, 3usize),
        [_, l, r, o, n] => (l, r, o, n.parse().expect("reps")),
        _ => {
            eprintln!("usage: boolean_perf <left.obj> <right.obj> <add|sub|int> [reps]");
            std::process::exit(2);
        }
    };
    let op = match op_name.as_str() {
        "add" => OpType::Add,
        "sub" => OpType::Subtract,
        "int" => OpType::Intersect,
        other => panic!("unknown op {other} (add|sub|int)"),
    };

    let a = Mesh::from_mesh_gl(&load_obj(Path::new(left))).unwrap();
    let b = Mesh::from_mesh_gl(&load_obj(Path::new(right))).unwrap();
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
