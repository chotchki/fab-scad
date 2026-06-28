//! `fab` — a workflow layer around OpenSCAD.
//!
//! OpenSCAD is a great geometry engine with no workflow story; `fab` supplies the
//! lifecycle it lacks (render, slice, output, publish) and never reimplements the
//! geometry. Foundation: a CLI skeleton, a real `doctor` preflight, and the OpenSCAD
//! wrap (see [`openscad`]).

mod openscad;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Result};
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
    /// Set the active project so later commands need no name. (Phase 3.2 -> 3.3)
    Focus { project: Option<String> },
    /// Scaffold a new project from the template. (Phase 3.5)
    New { name: String },
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
        Commands::Focus { .. } => not_yet("focus", "3.3"),
        Commands::New { .. } => not_yet("new", "3.5"),
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
