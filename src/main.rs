//! `fab` — a workflow layer around OpenSCAD.
//!
//! OpenSCAD is a great geometry engine with no workflow story; `fab` supplies the
//! lifecycle it lacks (render, slice, output, publish) and never reimplements the
//! geometry. Foundation: a CLI skeleton, a real `doctor` preflight, and the OpenSCAD
//! wrap (see [`openscad`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use fab_scad::openscad::{self, Openscad};
use fab_scad::{manifest, printers, project, slicing};

#[derive(Parser)]
#[command(
    name = "fab",
    version,
    about = "Workflow layer around OpenSCAD: render, slice, output, publish"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Environment preflight: OpenSCAD, Manifold, submodules, NAS, OPENSCADPATH.
    Doctor,
    /// Set the active project (or show it with no arg) so later commands need no name.
    Focus { project: Option<String> },
    /// Scaffold a new project (minimal manifest + starter scad) and focus it.
    New { name: String },
    /// Plan how to fit a part on the printer bed: orient/rotate, or (last resort) cut.
    Plan {
        /// Part bounding box as WxHxD in mm, e.g. 400x200x150.
        #[arg(long)]
        size: String,
        /// Printer name from printers.toml (default: the one flagged `default`).
        #[arg(long)]
        printer: Option<String>,
    },
    /// Emit + render a printable tolerance-test coupon (a joint swept across slop values).
    Coupon {
        /// Feature to test: "pin" (dowel socket) or "insert" (heat-set pocket).
        #[arg(long = "type", default_value = "pin")]
        kind: String,
        /// Screw size for insert pockets (M3/M4/M5).
        #[arg(long, default_value = "M3")]
        screw: String,
        /// Dowel diameter for pin sockets, in mm.
        #[arg(long, default_value_t = 6.0)]
        d: f64,
        /// Comma-separated slop values in mm.
        #[arg(long, default_value = "0,0.05,0.1,0.15,0.2,0.25")]
        slops: String,
        /// Output .scad path (default: ./coupon-<type>.scad).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Slice a part per its project.toml [slicing] spec: freeze the source, apply cuts +
    /// connectors, render the pieces. The headless half of the GUI (5.2).
    Slice {
        /// The part .scad to slice (its project.toml [slicing] is consumed).
        target: PathBuf,
        /// Fan pieces out along each cut axis by this much, mm (0 = assembled in place).
        #[arg(long, default_value_t = 0.0)]
        spread: f64,
        /// Output STL (default: <dir>/out/<stem>-sliced.stl).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also write a PNG thumbnail.
        #[arg(long)]
        png: bool,
        /// Export a multi-object 3mf (pieces as SEPARATE objects on a plate) instead of a merged STL.
        #[arg(long = "3mf")]
        threemf: bool,
    },
    /// Render a .scad to geometry (+ optional PNG thumbnail), or smoke-render a whole tree with --all.
    Render {
        /// A .scad file to render; with --all, a directory to sweep (default: the workspace root).
        target: Option<PathBuf>,
        /// Smoke-render EVERY .scad under `target` in parallel — pass iff it renders to faces > 0 —
        /// and print a pass/fail summary. The correctness sweep (6.8); needs no manifests.
        #[arg(long)]
        all: bool,
        /// With --all, ignore the incremental cache and re-render every model.
        #[arg(long)]
        force: bool,
        /// Also write an auto-framed PNG thumbnail next to the output.
        #[arg(long)]
        png: bool,
        /// Output path (default: <dir>/out/<stem>.stl).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Kill each render after this many seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Build + publish a project to hotchkiss.io. (Phase 7)
    Publish { project: Option<String> },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Doctor => doctor(),
        Commands::Focus { project } => project::focus_cmd(&require_root()?, project),
        Commands::New { name } => project::new_cmd(&require_root()?, &name),
        Commands::Plan { size, printer } => plan_cmd(&size, printer),
        Commands::Coupon { kind, screw, d, slops, out } => coupon_cmd(&kind, &screw, d, &slops, out),
        Commands::Slice { target, spread, out, png, threemf } => slice_cmd(&target, spread, out, png, threemf),
        Commands::Render {
            target,
            all,
            force,
            png,
            out,
            timeout,
        } => {
            if all {
                render_all_cmd(target, timeout, force)
            } else if let Some(target) = target {
                render_cmd(&target, out, png, timeout)
            } else {
                render_focus_cmd(png, timeout) // no target → the focused project's parts (6.9)
            }
        }
        Commands::Publish { .. } => not_yet("publish", "7"),
    }
}

fn not_yet(cmd: &str, phase: &str) -> Result<()> {
    println!("`fab {cmd}` is not implemented yet (planned for Phase {phase}).");
    Ok(())
}

fn plan_cmd(size_str: &str, printer: Option<String>) -> Result<()> {
    use printers::Outcome;
    let root = require_root()?;
    let size = parse_size(size_str)?;
    let profiles = printers::load(&root.join("printers.toml"))?;
    let pr = printers::select(&profiles, printer.as_deref())?;
    let plan = printers::plan(size, pr.bed);

    let f = |x: f64| if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x:.1}") };
    println!("printer {}  bed {} × {} × {} mm", pr.name, f(pr.bed[0]), f(pr.bed[1]), f(pr.bed[2]));
    println!("part    {} × {} × {} mm", f(size[0]), f(size[1]), f(size[2]));
    match &plan.outcome {
        Outcome::FitsAsIs { up } => {
            println!("→ fits whole ({} up); no cuts", printers::axis_name(*up));
        }
        Outcome::FitsRotated { up, degrees } => {
            println!(
                "→ fits whole, rotate {degrees:.1}° in XY ({} up); no cuts",
                printers::axis_name(*up)
            );
        }
        Outcome::NeedsCuts { oriented, cuts, pieces } => {
            println!(
                "→ {pieces} pieces; orient [{} × {} × {}] mm on the bed:",
                f(oriented[0]), f(oriented[1]), f(oriented[2])
            );
            for c in cuts {
                let pos: Vec<String> = c.positions.iter().map(|p| f(*p)).collect();
                println!(
                    "   {} cut(s) on {} → slice(cuts=[{}], axis={})",
                    c.count, c.axis, pos.join(", "), printers::slice_axis(c.axis)
                );
            }
        }
    }
    Ok(())
}

fn parse_size(s: &str) -> Result<[f64; 3]> {
    let parts: Vec<f64> = s
        .split(['x', 'X', '*'])
        .map(|p| p.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("bad --size '{s}': {e} (want WxHxD, e.g. 400x200x150)"))?;
    match parts[..] {
        [x, y, z] => Ok([x, y, z]),
        _ => bail!("--size must be three numbers WxHxD, got '{s}'"),
    }
}

fn coupon_cmd(kind: &str, screw: &str, d: f64, slops_str: &str, out: Option<PathBuf>) -> Result<()> {
    if kind != "pin" && kind != "insert" {
        bail!("--type must be 'pin' or 'insert', got '{kind}'");
    }
    let root = require_root()?;
    let slops = parse_slops(slops_str)?;
    let list = slops
        .iter()
        .map(|s| format!("{s}"))
        .collect::<Vec<_>>()
        .join(", ");
    let driver = format!(
        "include <coupon.scad>\nslop_coupon(type = \"{kind}\", d = {d}, screw = \"{screw}\", slops = [{list}]);\n"
    );
    let scad = out.unwrap_or_else(|| PathBuf::from(format!("coupon-{kind}.scad")));
    std::fs::write(&scad, driver).with_context(|| format!("writing {}", scad.display()))?;
    println!("wrote {}", scad.display());

    let oscad = Openscad::discover(Some(&root))?;
    let timeout = Duration::from_secs(120);
    let stl = scad.with_extension("stl");
    println!("render {} -> {}", scad.display(), stl.display());
    let r = oscad.render(&scad, &stl, timeout)?;
    print_report(&r);
    let png = scad.with_extension("png");
    let t = oscad.thumbnail(&scad, &png, (640, 360), timeout)?;
    print_report(&t);

    if !r.ok {
        bail!("coupon render failed");
    }
    Ok(())
}

fn parse_slops(s: &str) -> Result<Vec<f64>> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("bad --slops '{s}': {e}"))?;
    if v.is_empty() {
        bail!("--slops needs at least one value");
    }
    Ok(v)
}

fn slice_cmd(target: &Path, spread: f64, out: Option<PathBuf>, png: bool, threemf: bool) -> Result<()> {
    if !target.exists() {
        bail!("no such file: {}", target.display());
    }
    let root = find_root();
    let manifest_path = find_manifest(target)?;
    let m = manifest::Manifest::load(&manifest_path)?;
    let spec = m
        .slicing
        .as_ref()
        .with_context(|| format!("no [slicing] spec in {}", manifest_path.display()))?;

    let oscad = Openscad::discover(root.as_deref())?;
    let timeout = Duration::from_secs(120);
    let outdir = target.parent().unwrap_or_else(|| Path::new(".")).join("out");
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // 3mf path: pieces as separate objects on a plate (6.3) — no PNG/STL-copy branch below.
    if threemf {
        println!("slice {} -> 3mf", target.display());
        let plate = slicing::slice_part_3mf(&oscad, target, spec, spread, &outdir, timeout)?;
        let final_out = match out {
            Some(o) => {
                std::fs::copy(&plate, &o).with_context(|| format!("writing {}", o.display()))?;
                o
            }
            None => plate,
        };
        println!("  -> {}", final_out.display());
        return Ok(());
    }

    println!("slice {}", target.display());
    let sliced = slicing::slice_part(&oscad, target, spec, spread, &outdir, timeout)?;
    let final_out = match out {
        Some(o) => {
            std::fs::copy(&sliced, &o).with_context(|| format!("writing {}", o.display()))?;
            o
        }
        None => sliced,
    };
    println!("  -> {}", final_out.display());

    if png {
        let driver = outdir.join(format!("{stem}-sliced.scad"));
        let thumb = final_out.with_extension("png");
        let t = oscad.thumbnail(&driver, &thumb, (512, 512), timeout)?;
        print_report(&t);
    }
    Ok(())
}

/// Walk up from a target file to the nearest `project.toml`.
fn find_manifest(target: &Path) -> Result<PathBuf> {
    let abs = target
        .canonicalize()
        .with_context(|| format!("resolving {}", target.display()))?;
    let mut dir = abs.parent();
    while let Some(d) = dir {
        let m = d.join("project.toml");
        if m.exists() {
            return Ok(m);
        }
        dir = d.parent();
    }
    bail!("no project.toml found above {}", target.display());
}

fn render_cmd(target: &Path, out: Option<PathBuf>, png: bool, timeout_secs: u64) -> Result<()> {
    if !target.exists() {
        bail!("no such file: {}", target.display());
    }
    let root = find_root();
    let oscad = Openscad::discover(root.as_deref())?;
    let timeout = Duration::from_secs(timeout_secs);

    let stl = out.unwrap_or_else(|| default_out(target, "stl"));
    println!("render {} -> {}", target.display(), stl.display());
    let r = oscad.render(target, &stl, timeout)?;
    print_report(&r);

    if png {
        let thumb = stl.with_extension("png");
        println!("thumb  {} -> {}", target.display(), thumb.display());
        let t = oscad.thumbnail(target, &thumb, (512, 512), timeout)?;
        print_report(&t);
    }

    if !r.ok {
        bail!("render failed");
    }
    Ok(())
}

/// `fab render --all [PATH]` (6.8) — the correctness sweep: find every renderable `.scad` under
/// `path` (or the workspace root), smoke-render them in parallel, and print a pass/fail summary.
/// Exits non-zero if any model fails, so it drops straight into CI or a pre-refactor baseline.
fn render_all_cmd(path: Option<PathBuf>, timeout_secs: u64, force: bool) -> Result<()> {
    use fab_scad::{deps, smoke};
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = find_root();
    // Sweep the given path, else the workspace root, else the current dir.
    let sweep = path.or_else(|| root.clone()).unwrap_or_else(|| PathBuf::from("."));
    let files = smoke::scad_files(&sweep);
    if files.is_empty() {
        println!("no renderable .scad under {}", sweep.display());
        return Ok(());
    }
    let oscad = Openscad::discover(root.as_deref())?;
    let tmp = std::env::temp_dir();
    let timeout = Duration::from_secs(timeout_secs);
    let total = files.len();

    // Incremental (6.2): key each file's cache entry on the content-hash of its include closure,
    // resolved against the workspace OPENSCADPATH. Same hash + a prior pass ⇒ skip the render.
    let search: Vec<PathBuf> = root
        .as_ref()
        .map(|r| vec![r.join("libs"), r.join("scad-lib")])
        .unwrap_or_default();
    let version = oscad.tool_version().unwrap_or_default();
    let cache_dir = root.clone().unwrap_or_else(|| sweep.clone());
    let cache_path = cache_dir.join(".fab/smoke-cache");
    let cache = if force {
        smoke::SweepCache::empty()
    } else {
        smoke::SweepCache::load(&cache_path, &version)
    };
    println!("smoke-rendering {total} .scad under {} ...", sweep.display());

    // Parallel across the rayon pool; a running counter to stderr so a long sweep isn't silent.
    let done = AtomicUsize::new(0);
    let mut results: Vec<(smoke::Smoke, u64)> = files
        .par_iter()
        .map(|f| {
            let hash = deps::content_hash(f, &search);
            let s = match cache.hit(f, hash) {
                Some(faces) => smoke::Smoke {
                    input: f.clone(),
                    pass: true,
                    faces,
                    duration: Duration::ZERO,
                    detail: "cached".into(),
                },
                None => smoke::smoke(&oscad, f, &tmp, timeout),
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprint!("\r  {n}/{total} checked");
            (s, hash)
        })
        .collect();
    eprintln!();
    results.sort_by(|a, b| a.0.input.cmp(&b.0.input));

    let rel = |p: &Path| p.strip_prefix(&sweep).unwrap_or(p).display().to_string();
    let (mut passed, mut cached) = (0, 0);
    let mut passing = Vec::new();
    for (s, hash) in &results {
        if s.pass {
            passed += 1;
            passing.push((s.input.clone(), *hash, s.faces));
            if s.detail == "cached" {
                cached += 1;
                println!("  ok    {} ({} faces, cached)", rel(&s.input), s.faces);
            } else {
                println!("  ok    {} ({} faces, {:.1}s)", rel(&s.input), s.faces, s.duration.as_secs_f64());
            }
        } else {
            println!("  FAIL  {} — {}", rel(&s.input), s.detail);
        }
    }
    // Persist the passing set so the next sweep skips the unchanged ones. Failures are omitted, so
    // they always re-run. Best-effort — a cache we can't write just means no speedup next time.
    let _ = smoke::SweepCache::save(&cache_path, &version, &passing);

    let failed = total - passed;
    let tail = if failed > 0 { format!(", {failed} FAILED") } else { String::new() };
    let cache_note = if cached > 0 { format!(" ({cached} cached)") } else { String::new() };
    println!("\n{passed}/{total} passed{tail}{cache_note}");
    if failed > 0 {
        bail!("{failed} model(s) failed to render");
    }
    Ok(())
}

/// `fab render` with no target (6.9): render every `[[part]]` of the FOCUSED project. Paths resolve
/// against the project dir (`src = "src/foo.scad"`), outputs land in `renders/` unless `out` overrides.
/// The zero-argument entry point — `fab focus <name>` once, then just `fab render`.
fn render_focus_cmd(png: bool, timeout_secs: u64) -> Result<()> {
    let root = require_root()?;
    let name = project::read_focus(&root)
        .context("no focused project — run `fab focus <name>`, or pass a .scad target / --all")?;
    let pdir = project::project_dir(&root, &name);
    let manifest = manifest::Manifest::load(&pdir.join("project.toml"))?;
    if manifest.part.is_empty() {
        bail!("project '{name}' has no [[part]] entries to render");
    }
    let oscad = Openscad::discover(Some(&root))?;
    let timeout = Duration::from_secs(timeout_secs);
    let total = manifest.part.len();
    println!("render project '{name}' — {total} part(s)");

    let mut failed = 0;
    for (i, part) in manifest.part.iter().enumerate() {
        let src = pdir.join(&part.src);
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "part".into());
        let label = part.name.clone().unwrap_or_else(|| stem.clone());
        let out = part
            .out
            .as_ref()
            .map(|o| pdir.join(o))
            .unwrap_or_else(|| pdir.join("renders").join(format!("{stem}.stl")));
        println!("  [{}/{total}] {label}", i + 1);
        if !src.exists() {
            println!("        FAIL — no such src: {}", src.display());
            failed += 1;
            continue;
        }
        match oscad.render(&src, &out, timeout) {
            Ok(r) if r.ok => {
                println!("        -> {} ({:.1}s)", out.display(), r.duration.as_secs_f64());
                if png {
                    let thumb = out.with_extension("png");
                    let _ = oscad.thumbnail(&src, &thumb, (512, 512), timeout);
                }
            }
            Ok(_) => {
                println!("        FAIL — openscad error or empty output");
                failed += 1;
            }
            Err(e) => {
                println!("        FAIL — {e:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed}/{total} part(s) failed to render");
    }
    Ok(())
}

fn default_out(target: &Path, ext: &str) -> PathBuf {
    let stem = target
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "out".into());
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("out")
        .join(stem)
        .with_extension(ext)
}

fn print_report(r: &openscad::Report) {
    let status = if r.timed_out {
        "TIMEOUT".to_string()
    } else if r.ok {
        format!("ok ({:.1?})", r.duration)
    } else {
        "FAILED".to_string()
    };
    println!("  [{status}] {}", r.output.display());
    for w in &r.warnings {
        println!("    {w}");
    }
}

#[derive(Clone, Copy)]
enum Level {
    Ok,
    Warn,
    Fail,
}

type Check = (Level, String, String);

fn doctor() -> Result<()> {
    let mut checks: Vec<Check> = Vec::new();

    let root = find_root();
    match &root {
        Some(r) => checks.push((Level::Ok, "fab-scad root".into(), r.display().to_string())),
        None => checks.push((
            Level::Warn,
            "fab-scad root".into(),
            "not found — run inside the fab-scad tree".into(),
        )),
    }

    match openscad::find_bin() {
        Some(p) => {
            let ver = openscad::version(&p).unwrap_or_else(|| "unknown version".into());
            checks.push((
                Level::Ok,
                "OpenSCAD".into(),
                format!("{ver} ({})", p.display()),
            ));
            let (lvl, detail) = if openscad::has_manifold(&p) {
                (Level::Ok, "available".into())
            } else {
                (
                    Level::Warn,
                    "not confirmed (need a Manifold-capable build)".into(),
                )
            };
            checks.push((lvl, "Manifold backend".into(), detail));
        }
        None => checks.push((
            Level::Fail,
            "OpenSCAD".into(),
            "not found — set $OPENSCAD or install OpenSCAD".into(),
        )),
    }

    if let Some(r) = &root {
        for (name, rel) in [
            ("BOSL2", "libs/BOSL2"),
            ("machineblocks", "libs/machineblocks"),
        ] {
            let p = r.join(rel);
            if dir_has_contents(&p) {
                let tag = git_describe(&p).unwrap_or_else(|| "present".into());
                checks.push((Level::Ok, format!("submodule {name}"), tag));
            } else {
                checks.push((
                    Level::Fail,
                    format!("submodule {name}"),
                    "missing — run `git submodule update --init`".into(),
                ));
            }
        }

        let scad_lib = r.join("scad-lib");
        if scad_lib.join("version_stamp.scad").exists() {
            checks.push((Level::Ok, "scad-lib".into(), "present".into()));
        } else {
            checks.push((Level::Warn, "scad-lib".into(), "missing".into()));
        }

        let libs = r.join("libs");
        let want = format!("{}:{}", libs.display(), scad_lib.display());
        let have = std::env::var("OPENSCADPATH").unwrap_or_default();
        if have.split(':').any(|d| Path::new(d) == libs) {
            checks.push((Level::Ok, "OPENSCADPATH".into(), "set".into()));
        } else {
            checks.push((
                Level::Warn,
                "OPENSCADPATH".into(),
                format!("not set for interactive OpenSCAD; want: {want}"),
            ));
        }
    }

    let nas = Path::new("/Volumes/NAS");
    if nas.exists() {
        checks.push((
            Level::Ok,
            "NAS cold archive".into(),
            "/Volumes/NAS mounted".into(),
        ));
    } else {
        checks.push((
            Level::Warn,
            "NAS cold archive".into(),
            "/Volumes/NAS not mounted".into(),
        ));
    }

    for (lvl, name, detail) in &checks {
        let mark = match lvl {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("[{mark}] {name:<20} {detail}");
    }

    if checks.iter().any(|(l, _, _)| matches!(l, Level::Fail)) {
        bail!("doctor found blocking problems");
    }
    Ok(())
}

/// The fab-scad root, or a clear error — the workflow commands all need it.
fn require_root() -> Result<PathBuf> {
    find_root().context(
        "not inside a fab-scad tree — no `printers.toml` + `scad-lib/` found above the current dir",
    )
}

/// Walk up from the cwd to the fab-scad root (the dir holding `printers.toml` + `scad-lib`).
fn find_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("printers.toml").exists() && dir.join("scad-lib").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn git_describe(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["describe", "--tags", "--always"])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn dir_has_contents(p: &Path) -> bool {
    p.read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
