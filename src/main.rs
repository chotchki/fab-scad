//! `fab` — a workflow layer around OpenSCAD.
//!
//! OpenSCAD is a great geometry engine with no workflow story; `fab` supplies the
//! lifecycle it lacks (render, slice, output, publish) and never reimplements the
//! geometry. Foundation: a CLI skeleton, a real `doctor` preflight, and the OpenSCAD
//! wrap (see [`openscad`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use fab_scad::openscad::{self, Openscad};
use fab_scad::{credentials, manifest, printers, project, slicing};

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
        /// Export a multi-object 3mf, pieces bin-packed onto the printer bed, instead of a merged STL.
        #[arg(long = "3mf")]
        threemf: bool,
        /// Slice in-process via the Manifold kernel (Track C) instead of the OpenSCAD codegen path.
        /// OpenSCAD still renders the base mesh once; slicing + connectors run in-process.
        #[arg(long)]
        kernel: bool,
        /// Printer whose bed the --3mf plate targets (default: [slicing].printer, else printers.toml's default).
        #[arg(long)]
        printer: Option<String>,
        /// Gap between pieces bin-packed on the --3mf plate, mm.
        #[arg(long, default_value_t = 5.0)]
        gap: f64,
    },
    /// Make a printable Bambu multi-plate project from a model in ONE shot: render, auto-slice,
    /// auto-connect, orient, pack, export. The headless twin of the GUI's auto-open (Track C 14.3).
    Make {
        /// The model .scad to make printable.
        target: PathBuf,
        /// Printer name from printers.toml (default: the one flagged `default`).
        #[arg(long)]
        printer: Option<String>,
        /// Output .3mf (default: <model>-plates.3mf next to the model).
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Spacing between packed pieces on a plate, mm.
        #[arg(long, default_value_t = 5.0)]
        gap: f64,
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
        /// Geometry engine: `openscad` (the trusted oracle, default) or `scad-rs` (OUR pure-Rust evaluator +
        /// Manifold kernel — dogfooding). `--engine scad-rs` never touches the OpenSCAD binary.
        #[arg(long, value_enum, default_value_t = Engine::Openscad)]
        engine: Engine,
        /// With `--engine scad-rs`, ALSO render through OpenSCAD and report the boolean-residual/genus
        /// divergence — so every real dogfood render doubles as a differential datapoint against the oracle.
        #[arg(long)]
        check: bool,
        /// Kill each render after this many seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Publish a model to hotchkiss.io: render a cover + a low-`$fn` preview mesh + the full STL,
    /// upload them, and create/update the project page (Phase 15). Auth with an `hio_` API key.
    Publish {
        /// The model .scad to publish (its project.toml [project]/[publish] is consumed).
        target: PathBuf,
        /// hotchkiss.io base URL (default: $HIO_URL, else https://hotchkiss.io).
        #[arg(long)]
        url: Option<String>,
        /// API key `hio_…` (default: $HIO_API_KEY).
        #[arg(long)]
        api_key: Option<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Doctor => doctor(),
        Commands::Focus { project } => project::focus_cmd(&require_root()?, project),
        Commands::New { name } => project::new_cmd(&require_root()?, &name),
        Commands::Plan { size, printer } => plan_cmd(&size, printer),
        Commands::Coupon {
            kind,
            screw,
            d,
            slops,
            out,
        } => coupon_cmd(&kind, &screw, d, &slops, out),
        Commands::Slice {
            target,
            spread,
            out,
            png,
            threemf,
            kernel,
            printer,
            gap,
        } => slice_cmd(&target, spread, out, png, threemf, kernel, printer, gap),
        Commands::Make {
            target,
            printer,
            out,
            gap,
        } => make_cmd(&target, printer, out, gap),
        Commands::Render {
            target,
            all,
            force,
            png,
            out,
            engine,
            check,
            timeout,
        } => {
            if all {
                render_all_cmd(target, timeout, force)
            } else if let Some(target) = target {
                match engine {
                    Engine::Openscad => render_cmd(&target, out, png, timeout),
                    Engine::ScadRs => render_scadrs_cmd(&target, out, check, timeout),
                }
            } else {
                render_focus_cmd(png, timeout) // no target → the focused project's parts (6.9)
            }
        }
        Commands::Publish {
            target,
            url,
            api_key,
        } => publish_cmd(&target, url, api_key),
    }
}

fn plan_cmd(size_str: &str, printer: Option<String>) -> Result<()> {
    use printers::Outcome;
    let root = require_root()?;
    let size = parse_size(size_str)?;
    let profiles = printers::load(&root.join("printers.toml"))?;
    let pr = printers::select(&profiles, printer.as_deref())?;
    let plan = printers::plan(size, pr.bed);

    let f = |x: f64| {
        if x.fract() == 0.0 {
            format!("{}", x as i64)
        } else {
            format!("{x:.1}")
        }
    };
    println!(
        "printer {}  bed {} × {} × {} mm",
        pr.name,
        f(pr.bed[0]),
        f(pr.bed[1]),
        f(pr.bed[2])
    );
    println!(
        "part    {} × {} × {} mm",
        f(size[0]),
        f(size[1]),
        f(size[2])
    );
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
        Outcome::NeedsCuts {
            oriented,
            cuts,
            pieces,
        } => {
            println!(
                "→ {pieces} pieces; orient [{} × {} × {}] mm on the bed:",
                f(oriented[0]),
                f(oriented[1]),
                f(oriented[2])
            );
            for c in cuts {
                let pos: Vec<String> = c.positions.iter().map(|p| f(*p)).collect();
                println!(
                    "   {} cut(s) on {} → slice(cuts=[{}], axis={})",
                    c.count,
                    c.axis,
                    pos.join(", "),
                    printers::slice_axis(c.axis)
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

fn coupon_cmd(
    kind: &str,
    screw: &str,
    d: f64,
    slops_str: &str,
    out: Option<PathBuf>,
) -> Result<()> {
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

fn publish_cmd(target: &Path, url: Option<String>, api_key: Option<String>) -> Result<()> {
    if !target.exists() {
        bail!("no such file: {}", target.display());
    }
    let m = manifest::Manifest::load(&find_manifest(target)?)?;
    let title = m
        .project
        .title
        .clone()
        .unwrap_or_else(|| m.project.name.clone());
    let description = m.publish.map(|p| p.description).unwrap_or_default();

    // --api-key/--url flags win; else resolve env-then-saved-file (W.3.27) — the same store the GUI
    // Settings screen writes, so a key saved there also unblocks the CLI.
    let resolved = credentials::resolve();
    let key = api_key.or(resolved.api_key).context(
        "no API key — pass --api-key, set HIO_API_KEY, or save one in fab-gui Settings (credentials.toml)",
    )?;
    let base = url.unwrap_or(resolved.url);

    let root = find_root();
    let oscad = Openscad::discover(root.as_deref())?;
    let out = target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("out")
        .join("publish");
    println!(
        "publishing {} to {base}… (rendering cover + full + preview meshes)",
        target.display()
    );
    let page_url = fab_scad::publish::publish_model(
        &oscad,
        target,
        &title,
        &description,
        &base,
        &key,
        &out,
        Duration::from_secs(120),
    )?;
    println!("published → {page_url}");
    Ok(())
}

#[cfg(feature = "kernel")]
fn make_cmd(target: &Path, printer: Option<String>, out: Option<PathBuf>, gap: f64) -> Result<()> {
    if !target.exists() {
        bail!("no such file: {}", target.display());
    }
    let root = require_root()?;
    let oscad = Openscad::discover(Some(&root))?;
    let profiles = printers::load(&root.join("printers.toml"))?;
    let pr = printers::select(&profiles, printer.as_deref())?;
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());
    let out_3mf = out.unwrap_or_else(|| target.with_file_name(format!("{stem}-plates.3mf")));
    let out_dir = root.join("out").join("make");
    let f = |x: f64| {
        if x.fract() == 0.0 {
            format!("{}", x as i64)
        } else {
            format!("{x:.1}")
        }
    };
    println!(
        "make {} on {} ({} × {} × {} mm bed)",
        target.display(),
        pr.name,
        f(pr.bed[0]),
        f(pr.bed[1]),
        f(pr.bed[2])
    );
    let sum = fab_scad::auto::make(
        &oscad,
        target,
        fab_lang::Dims::from_array(pr.bed),
        &out_3mf,
        &out_dir,
        Duration::from_secs(120),
        gap,
    )?;
    println!(
        "  -> {} piece(s) on {} plate(s) ({:.0}% full) -> {}",
        sum.pieces,
        sum.plates,
        sum.fill * 100.0,
        out_3mf.display()
    );
    Ok(())
}

#[cfg(not(feature = "kernel"))]
fn make_cmd(_: &Path, _: Option<String>, _: Option<PathBuf>, _: f64) -> Result<()> {
    bail!("fab make needs the `kernel` feature (built without it)")
}

#[allow(clippy::too_many_arguments)] // a CLI verb — every arg is a distinct user-facing flag
fn slice_cmd(
    target: &Path,
    spread: f64,
    out: Option<PathBuf>,
    png: bool,
    threemf: bool,
    kernel: bool,
    printer: Option<String>,
    gap: f64,
) -> Result<()> {
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

    // U.3.14 Phase E — the --3mf plate targets a printer bed: --printer > [slicing].printer > default.
    // `None` = STL output (no bed). Resolved once here; the kernel slice paths pack onto it.
    let plate = if threemf {
        let root_pb = root
            .clone()
            .context("--3mf needs a workspace root (printers.toml) above the target")?;
        let profiles = printers::load(&root_pb.join("printers.toml"))?;
        let pr = printers::select(&profiles, printer.as_deref().or(spec.printer.as_deref()))?;
        println!(
            "  printer {} ({:.0}×{:.0}mm bed)",
            pr.name, pr.bed[0], pr.bed[1]
        );
        Some(([pr.bed[0], pr.bed[1]], gap))
    } else {
        None
    };

    // U.3.14 Phase D — per-part slicing. A `[[slicing.part]]` spec addresses each `build_geo_parts`
    // part individually; it XORs with the flat whole-model `[slicing]` (a spec carrying BOTH is
    // ambiguous). It's the in-process kernel path — the split needs the evaluated tree, not one
    // OpenSCAD-rendered whole mesh — so route here BEFORE discovering OpenSCAD (per-part needs none).
    if !spec.parts.is_empty() {
        let has_flat =
            !spec.cut.is_empty() || !spec.connector.is_empty() || !spec.orient.is_empty();
        if has_flat {
            bail!(
                "slicing spec in {} mixes flat [slicing] cuts and [[slicing.part]] blocks — use one",
                manifest_path.display()
            );
        }
        #[cfg(all(feature = "kernel", feature = "native"))]
        {
            let outdir = target
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("out");
            println!(
                "slice {} (per-part -> {})",
                target.display(),
                if threemf { "3mf" } else { "stl" }
            );
            let produced =
                slicing::slice_model_parts(target, &scadrs_libs(), spec, spread, &outdir, plate)?;
            let final_out = match out {
                Some(o) => {
                    std::fs::copy(&produced, &o)
                        .with_context(|| format!("writing {}", o.display()))?;
                    o
                }
                None => produced,
            };
            println!("  -> {}", final_out.display());
            return Ok(());
        }
        #[cfg(not(all(feature = "kernel", feature = "native")))]
        bail!("per-part slicing ([[slicing.part]]) needs the `kernel` feature (built without it)");
    }

    let oscad = Openscad::discover(root.as_deref())?;
    let timeout = Duration::from_secs(120);
    let outdir = target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("out");
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // In-process kernel path (Track C, opt-in) — OpenSCAD renders the base mesh once, the rest runs
    // in-process. Same output shape (merged STL or a multi-object 3mf), no per-piece spawn.
    if kernel {
        #[cfg(feature = "kernel")]
        {
            println!(
                "slice {} -> {} (kernel)",
                target.display(),
                if threemf { "3mf" } else { "stl" }
            );
            let produced =
                slicing::slice_part_kernel(&oscad, target, spec, spread, &outdir, timeout, plate)?;
            let final_out = match out {
                Some(o) => {
                    std::fs::copy(&produced, &o)
                        .with_context(|| format!("writing {}", o.display()))?;
                    o
                }
                None => produced,
            };
            println!("  -> {}", final_out.display());
            return Ok(());
        }
        #[cfg(not(feature = "kernel"))]
        bail!("--kernel needs the `kernel` feature (built without it)");
    }

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

/// Which geometry engine `fab render` uses. OpenSCAD is the default + the trusted oracle; scad-rs is our own
/// evaluator, exposed here for DOGFOODING (Q.1) — run real parts through our pipeline to generate the real
/// usage samples the perf tier (N/O/P) should be cut from, not a fixed model set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, ValueEnum)]
enum Engine {
    /// The OpenSCAD binary (the oracle; the default).
    #[default]
    Openscad,
    /// scad-rs — our pure-Rust evaluator + Manifold kernel, no OpenSCAD.
    #[value(name = "scad-rs")]
    ScadRs,
}

/// The library search path scad-rs's loader resolves `<lib.scad>` against — the workspace `libs/` (BOSL2) +
/// `scad-lib`, mirroring the `OPENSCADPATH` fab injects for the oracle (so `<BOSL2/std.scad>` /
/// `<connectors.scad>` resolve identically). Same-dir includes resolve against the target's own parent inside
/// the loader, so they need no entry here.
fn scadrs_libs() -> Vec<PathBuf> {
    find_root().map_or_else(Vec::new, |root| {
        vec![root.join("libs"), root.join("scad-lib")]
    })
}

/// `fab render --engine scad-rs` (Q.1 dogfooding) — render a `.scad` through OUR pipeline (fab-lang eval →
/// Manifold kernel → STL), never the OpenSCAD binary. With `--check`, ALSO render through OpenSCAD and report
/// the boolean-residual/genus divergence, so a real print doubles as a differential sample. Set
/// `FAB_EVAL_CACHE=1` to exercise the eval-memo cache (N.2c) on real work.
fn render_scadrs_cmd(
    target: &Path,
    out: Option<PathBuf>,
    check: bool,
    _timeout_secs: u64,
) -> Result<()> {
    use fab_scad::backend::{ManifoldBackend, build_geo};
    if !target.exists() {
        bail!("no such file: {}", target.display());
    }
    let libs = scadrs_libs();
    let stl = out.unwrap_or_else(|| default_out(target, "stl"));

    let start = std::time::Instant::now();
    let tree = fab_scad::import::resolve_geometry_file(target, &libs, fab_lang::Config::from_env())
        .with_context(|| format!("scad-rs eval of {}", target.display()))?;
    let solid = build_geo(&tree, &ManifoldBackend)
        .filter(|s| !s.is_empty())
        .with_context(|| {
            format!(
                "scad-rs rendered EMPTY geometry (no faces) for {}",
                target.display()
            )
        })?;
    let ms = start.elapsed().as_millis();
    std::fs::write(&stl, solid.to_stl_bytes())
        .with_context(|| format!("writing {}", stl.display()))?;
    println!(
        "scad-rs  {} -> {}  (vol {:.3}, genus {}, {ms} ms)",
        target.display(),
        stl.display(),
        solid.volume(),
        solid.genus(),
    );

    if check {
        // Reuse the differential: renders BOTH engines to `Solid` and agrees-or-explains (boolean residual +
        // genus). A real render is now a correctness datapoint against the oracle.
        match fab_scad::differ::diff_files(target, &libs) {
            Ok(()) => println!("check    AGREES with OpenSCAD (within residual tolerance)"),
            Err(detail) => println!("check    DIVERGES vs OpenSCAD: {detail}"),
        }
    }
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

/// `fab render --all [PATH]` (6.8) — the correctness sweep: find every renderable `.scad` under
/// `path` (or the workspace root), smoke-render them in parallel, and print a pass/fail summary.
/// Exits non-zero if any model fails, so it drops straight into CI or a pre-refactor baseline.
fn render_all_cmd(path: Option<PathBuf>, timeout_secs: u64, force: bool) -> Result<()> {
    use fab_scad::{deps, smoke};
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = find_root();
    // Sweep the given path, else the workspace root, else the current dir.
    let sweep = path
        .or_else(|| root.clone())
        .unwrap_or_else(|| PathBuf::from("."));
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
    println!(
        "smoke-rendering {total} .scad under {} ...",
        sweep.display()
    );

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
                println!(
                    "  ok    {} ({} faces, {:.1}s)",
                    rel(&s.input),
                    s.faces,
                    s.duration.as_secs_f64()
                );
            }
        } else {
            println!("  FAIL  {} — {}", rel(&s.input), s.detail);
        }
    }
    // Persist the passing set so the next sweep skips the unchanged ones. Failures are omitted, so
    // they always re-run. Best-effort — a cache we can't write just means no speedup next time.
    let _ = smoke::SweepCache::save(&cache_path, &version, &passing);

    let failed = total - passed;
    let tail = if failed > 0 {
        format!(", {failed} FAILED")
    } else {
        String::new()
    };
    let cache_note = if cached > 0 {
        format!(" ({cached} cached)")
    } else {
        String::new()
    };
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
                println!(
                    "        -> {} ({:.1}s)",
                    out.display(),
                    r.duration.as_secs_f64()
                );
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
