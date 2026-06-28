//! `fab` — a workflow layer around OpenSCAD.
//!
//! OpenSCAD is a great geometry engine with no workflow story; `fab` supplies the
//! lifecycle it lacks (render, slice, output, publish) and never reimplements the
//! geometry. Foundation: a CLI skeleton, a real `doctor` preflight, and the OpenSCAD
//! wrap (see [`openscad`]).

mod manifest;
mod num;
mod openscad;
mod printers;
mod project;
mod slicing;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use openscad::Openscad;

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
    },
    /// Render a .scad to geometry (+ optional PNG thumbnail).
    /// File-level for now; project/DAG-aware in Phase 6.
    Render {
        /// Path to a .scad file.
        target: PathBuf,
        /// Also write an auto-framed PNG thumbnail next to the output.
        #[arg(long)]
        png: bool,
        /// Output path (default: <dir>/out/<stem>.stl).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Kill the render after this many seconds.
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
        Commands::Slice { target, spread, out, png } => slice_cmd(&target, spread, out, png),
        Commands::Render {
            target,
            png,
            out,
            timeout,
        } => render_cmd(&target, out, png, timeout),
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

fn slice_cmd(target: &Path, spread: f64, out: Option<PathBuf>, png: bool) -> Result<()> {
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
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());
    let outdir = dir.join("out");
    std::fs::create_dir_all(&outdir).with_context(|| format!("creating {}", outdir.display()))?;

    // 1. Freeze the source to a mesh — slicing the frozen STL is linear (no 2^N).
    let source_stl = outdir.join(format!("{stem}.stl"));
    println!("freeze {} -> {}", target.display(), source_stl.display());
    let f = oscad.render(target, &source_stl, timeout)?;
    print_report(&f);
    if !f.ok {
        bail!("source render failed");
    }

    // 2. Generate the slicer driver from the spec (imports the frozen mesh by name).
    let driver = slicing::driver_scad(spec, &format!("{stem}.stl"), spread)?;
    let driver_path = outdir.join(format!("{stem}-sliced.scad"));
    std::fs::write(&driver_path, driver)
        .with_context(|| format!("writing {}", driver_path.display()))?;

    // 3. Render the sliced result.
    let sliced = out.unwrap_or_else(|| outdir.join(format!("{stem}-sliced.stl")));
    println!("slice  {} -> {}", driver_path.display(), sliced.display());
    let r = oscad.render(&driver_path, &sliced, timeout)?;
    print_report(&r);
    if png {
        let thumb = sliced.with_extension("png");
        let t = oscad.thumbnail(&driver_path, &thumb, (512, 512), timeout)?;
        print_report(&t);
    }
    if !r.ok {
        bail!("slice render failed");
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
