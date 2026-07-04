//! The "fab wraps OpenSCAD" seam. Everything that shells out to OpenSCAD goes through
//! here: it injects OPENSCADPATH (so projects resolve `libs/` + `scad-lib` with no global
//! env), forces the Manifold backend, enforces a timeout in-process (macOS has no
//! `timeout(1)`), and captures warnings/errors.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Locate the OpenSCAD binary: `$OPENSCAD`, the macOS .app, then `$PATH`.
pub fn find_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENSCAD") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let app = PathBuf::from("/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD");
    if app.exists() {
        return Some(app);
    }
    if Command::new("openscad").arg("--version").output().is_ok() {
        return Some(PathBuf::from("openscad"));
    }
    None
}

/// First line of `openscad --version` (it prints to stderr).
pub fn version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let text = if out.stderr.is_empty() {
        String::from_utf8_lossy(&out.stdout)
    } else {
        String::from_utf8_lossy(&out.stderr)
    };
    text.lines().next().map(|l| l.trim().to_string())
}

/// Whether this build advertises the Manifold backend.
pub fn has_manifold(bin: &Path) -> bool {
    let Ok(out) = Command::new(bin).arg("--help").output() else {
        return false;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.to_lowercase().contains("manifold")
}

/// A configured OpenSCAD invoker: the binary plus the OPENSCADPATH fab injects.
pub struct Openscad {
    bin: PathBuf,
    openscadpath: OsString,
}

#[derive(Debug)]
pub struct Report {
    pub output: PathBuf,
    pub duration: Duration,
    pub timed_out: bool,
    pub ok: bool,
    pub warnings: Vec<String>,
    /// `ECHO:` lines from the run, in order — the oracle's console output (G.3.6 differential harness).
    pub echo: Vec<String>,
}

impl Openscad {
    /// Build OPENSCADPATH from the fab-scad root (`libs/` + `scad-lib`). Falls back to the
    /// ambient env when there's no root (e.g. rendering a loose file).
    pub fn discover(root: Option<&Path>) -> Result<Self> {
        let bin = find_bin().context("OpenSCAD not found — set $OPENSCAD or install it")?;
        let openscadpath = match root {
            Some(r) => OsString::from(format!(
                "{}:{}",
                r.join("libs").display(),
                r.join("scad-lib").display()
            )),
            None => std::env::var_os("OPENSCADPATH").unwrap_or_default(),
        };
        Ok(Self { bin, openscadpath })
    }

    /// This toolchain's version string, for cache-keying an incremental sweep (6.2) — a bump
    /// invalidates cached verdicts. None if it can't be read.
    pub fn tool_version(&self) -> Option<String> {
        version(&self.bin)
    }

    /// Render geometry (format inferred from `output`'s extension) via Manifold.
    pub fn render(&self, input: &Path, output: &Path, timeout: Duration) -> Result<Report> {
        ensure_parent(output)?;
        let args = [
            OsString::from("--backend"),
            OsString::from("Manifold"),
            OsString::from("-o"),
            output.as_os_str().to_owned(),
            input.as_os_str().to_owned(),
        ];
        let mut r = self.run(&args, output, timeout)?;
        r.ok = r.ok && file_nonempty(output);
        Ok(r)
    }

    /// Render to a MULTI-OBJECT 3mf with lazy-union on, so top-level objects stay separate (6.3) —
    /// a multipart plate the slicer emits one piece per top-level statement. Otherwise like `render`;
    /// point `output` at a `.3mf`.
    pub fn render_multipart(
        &self,
        input: &Path,
        output: &Path,
        timeout: Duration,
    ) -> Result<Report> {
        ensure_parent(output)?;
        let args = [
            OsString::from("--backend"),
            OsString::from("Manifold"),
            OsString::from("--enable=lazy-union"),
            OsString::from("-o"),
            output.as_os_str().to_owned(),
            input.as_os_str().to_owned(),
        ];
        let mut r = self.run(&args, output, timeout)?;
        r.ok = r.ok && file_nonempty(output);
        Ok(r)
    }

    /// Render an auto-framed PNG thumbnail.
    pub fn thumbnail(
        &self,
        input: &Path,
        output: &Path,
        size: (u32, u32),
        timeout: Duration,
    ) -> Result<Report> {
        ensure_parent(output)?;
        let args = [
            OsString::from("--autocenter"),
            OsString::from("--viewall"),
            OsString::from("--imgsize"),
            OsString::from(format!("{},{}", size.0, size.1)),
            OsString::from("--colorscheme"),
            OsString::from("Cornfield"),
            OsString::from("-o"),
            output.as_os_str().to_owned(),
            input.as_os_str().to_owned(),
        ];
        let mut r = self.run(&args, output, timeout)?;
        r.ok = r.ok && file_nonempty(output);
        Ok(r)
    }

    fn run(&self, args: &[OsString], output: &Path, timeout: Duration) -> Result<Report> {
        let logname = output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "out".into());
        let logpath =
            std::env::temp_dir().join(format!("fab-oscad-{}-{logname}.log", std::process::id()));
        let log = fs::File::create(&logpath).context("failed to create OpenSCAD log file")?;

        let mut child = Command::new(&self.bin)
            .args(args)
            .env("OPENSCADPATH", &self.openscadpath)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .context("failed to spawn OpenSCAD")?;

        let start = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let duration = start.elapsed();

        let stderr = fs::read_to_string(&logpath).unwrap_or_default();
        let _ = fs::remove_file(&logpath);
        // Match OpenSCAD's "WARNING:" / "ERROR:" lines. Avoid bare "ERROR" so the normal
        // "Status: NoError" summary line isn't mistaken for a problem.
        let warnings = stderr
            .lines()
            .filter(|l| l.contains("WARNING") || l.contains("ERROR:"))
            .map(str::to_string)
            .collect();
        // Echo goes to stderr as `ECHO: …` lines (verified G.3.6) — the oracle's console output.
        let echo = stderr
            .lines()
            .filter(|l| l.trim_start().starts_with("ECHO:"))
            .map(str::to_string)
            .collect();

        let ok = !timed_out && status.map(|s| s.success()).unwrap_or(false);
        Ok(Report {
            output: output.to_path_buf(),
            duration,
            timed_out,
            ok,
            warnings,
            echo,
        })
    }
}

fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn file_nonempty(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}
