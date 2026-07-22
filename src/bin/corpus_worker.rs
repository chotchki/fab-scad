//! The BOSL2 corpus isolation worker (K.1). Args: `<bosl2_dir> <start> <end> [lane]` — runs the
//! flattened range `[start, end)` of the lane's corpus (`tests` = the `.scadtest` suite, default;
//! `examples` = `examples/*.scad`, SU.3) in-process and streams `idx\tbucket\tms\tdetail` per case to
//! stdout. A stack overflow aborts ONLY this process; the parent sweep restarts a worker past the
//! crasher and buckets it as a crash. One chunk of the parallel, crash-resilient sweep. See
//! [`fab_scad::corpus`] for the parent side.

use std::path::Path;

use fab_scad::corpus::Lane;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let bosl2_dir = args.next().unwrap_or_else(|| ".".to_string());
    let start: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let end: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let lane = args
        .next()
        .and_then(|s| Lane::from_label(&s))
        .unwrap_or(Lane::Tests);
    fab_scad::corpus::worker_main(Path::new(&bosl2_dir), start, end, lane)
}
