//! Upstream-expectation filter for the sustain sweep (AE.1): a corpus failure only MEANS something
//! if OpenSCAD's own harness expects that file to succeed. Upstream encodes "this file must fail"
//! in two machine-readable places — `FAILING_FILES` in `tests/CMakeLists.txt` (the render must exit
//! non-zero) and the golden outputs in `tests/regression/echo/` (an `-expected.echo` whose text IS
//! the error) — plus `templates/`, whose files are `configure_file` inputs never run raw. Point
//! [`UpstreamExpectations::load`] at an openscad checkout and [`UpstreamExpectations::classify`]
//! splits the failure list into upstream-parity (they fail too — that's AGREEMENT) and genuine
//! divergence. Report-side only: the [`Bucket`]s and the worker wire format are untouched.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::corpus::Bucket;

/// Upstream's own must-fail verdicts, loaded once per sweep from an openscad checkout.
pub struct UpstreamExpectations {
    /// `FAILING_FILES` entries with the `${VAR}` prefix stripped — kept '/'-rooted so the suffix
    /// match can't bind to a same-named file in a different directory.
    failing: Vec<String>,
    /// `<root>/tests/regression/echo` — the golden console outputs.
    echo_dir: PathBuf,
}

impl UpstreamExpectations {
    /// Load the expectation sources from an openscad checkout root.
    ///
    /// # Errors
    /// A missing/unreadable `tests/CMakeLists.txt` fails LOUD: a sweep that silently dropped the
    /// expectation source would report upstream-expected failures as genuine — or worse, be trusted
    /// to have filtered when it didn't. (Individual goldens stay best-effort: most corpus files
    /// legitimately have none.)
    pub fn load(openscad_root: &Path) -> Result<Self> {
        let cmake = openscad_root.join("tests/CMakeLists.txt");
        let text = std::fs::read_to_string(&cmake)
            .with_context(|| format!("reading {}", cmake.display()))?;
        Ok(UpstreamExpectations {
            failing: parse_failing_files(&text),
            echo_dir: openscad_root.join("tests").join("regression").join("echo"),
        })
    }

    /// Why upstream expects `file` to fail, or `None` if this failure is genuinely ours. Call only
    /// on non-[`Bucket::Pass`] results — a pass needs no excuse (and "upstream fails where we pass"
    /// is the deliberate accept-more doctrine, not a defect to flag).
    #[must_use]
    pub fn classify(&self, file: &str, bucket: Bucket, detail: &str) -> Option<String> {
        if file.contains("/tests/data/scad/templates/") {
            return Some("CMake template — a `configure_file` input, never run raw".to_string());
        }
        if self.failing.iter().any(|t| file.ends_with(t.as_str())) {
            return Some(
                "in upstream `FAILING_FILES` — their harness requires a non-zero exit".to_string(),
            );
        }
        let stem = Path::new(file).file_stem()?.to_str()?;
        let golden =
            std::fs::read_to_string(self.echo_dir.join(format!("{stem}-expected.echo"))).ok()?;
        if let Some(err) = golden.lines().find(|l| l.starts_with("ERROR:")) {
            return Some(format!("upstream golden echo expects failure: `{err}`"));
        }
        // A missing-library Load where the golden documents the SAME can't-open: upstream warns and
        // limps on where our sweep refuses the vacuous pass — different bar, same wall. Verbatim
        // detail match on purpose: a can't-open for a library upstream DOES have (a submodule we
        // failed to wire) won't appear in their golden and stays a genuine failure.
        if bucket == Bucket::Load {
            let needle = detail.trim();
            if !needle.is_empty() && golden.contains(needle) {
                return Some(format!(
                    "upstream golden echo hits the same wall: `{needle}`"
                ));
            }
        }
        None
    }
}

/// The golden's `ECHO:` lines, when `file` has an ELIGIBLE golden — one that exists and records a
/// SUCCESS run. A golden containing an `ERROR:` line documents a failing run, and a passing run's
/// full echo stream can't be expected to match a failure's prefix, so those are skipped (their
/// semantics belong to [`UpstreamExpectations::classify`]). v1 of the AH lane compares `ECHO:`
/// lines only: `WARNING:`/`TRACE:` lines carry file paths and the word-for-word warning-text
/// parity that's still the deferred I.5 backlog half.
#[must_use]
pub fn golden_echo_lines(echo_dir: &Path, file: &str) -> Option<Vec<String>> {
    let stem = Path::new(file).file_stem()?.to_str()?;
    let golden = std::fs::read_to_string(echo_dir.join(format!("{stem}-expected.echo"))).ok()?;
    if golden.lines().any(|l| l.starts_with("ERROR:")) {
        return None;
    }
    // One entry per ECHO UNIT, not per physical line: echo never escapes (AH.2.2), so a string
    // containing newlines prints across several golden lines — continuation lines (no message
    // prefix) re-attach to the ECHO above them. Lines under a WARNING/TRACE stay dropped.
    let prefixed = |l: &str| {
        ["WARNING:", "ERROR:", "TRACE:", "DEPRECATED:", "FONT-"]
            .iter()
            .any(|p| l.starts_with(p))
    };
    let mut echoes: Vec<String> = Vec::new();
    let mut in_echo = false;
    for line in golden.lines() {
        let l = line.trim_end_matches('\r');
        if l.starts_with("ECHO:") {
            echoes.push(l.to_string());
            in_echo = true;
        } else if prefixed(l) {
            in_echo = false;
        } else if in_echo && let Some(last) = echoes.last_mut() {
            last.push('\n');
            last.push_str(l);
        }
    }
    Some(echoes)
}

/// First divergence between our rendered `ECHO:` lines and the golden's, as a one-line report
/// (the sweep's wire detail is single-line); `None` = every line matches. Sides are capped so a
/// monster echo (a full VNF) can't blow up the report table.
#[must_use]
pub fn first_echo_divergence(ours: &[String], golden: &[String]) -> Option<String> {
    fn cap(s: &str) -> String {
        // Wire detail is ONE line — fold real newlines back to visible escapes for the report.
        let s = s.replace('\n', "\\n");
        // char-boundary safe: the corpus echoes unicode (utf8-tests et al).
        if s.chars().count() > 120 {
            let head: String = s.chars().take(117).collect();
            format!("{head}…")
        } else {
            s
        }
    }
    let n = ours.len().max(golden.len());
    for i in 0..n {
        let a = ours.get(i).map(|s| s.trim_end());
        let b = golden.get(i).map(|s| s.trim_end());
        if a != b {
            return Some(format!(
                "echo line {} diverges — ours: {} · golden: {}",
                i + 1,
                a.map_or("<none>".to_string(), cap),
                b.map_or("<none>".to_string(), cap),
            ));
        }
    }
    None
}

/// Extract `FAILING_FILES` entries from upstream's `tests/CMakeLists.txt`: every
/// `set(FAILING_FILES`/`list(APPEND FAILING_FILES` block, one `${VAR}/path.scad` entry per line,
/// returned as the '/'-rooted tail after the variable. Line-based on purpose — the blocks are
/// plain path lists and a real CMake parser buys nothing here.
fn parse_failing_files(cmake: &str) -> Vec<String> {
    let mut tails = Vec::new();
    let mut in_block = false;
    for line in cmake.lines() {
        let t = line.trim();
        if !in_block {
            in_block =
                t.starts_with("set(FAILING_FILES") || t.starts_with("list(APPEND FAILING_FILES");
            continue;
        }
        if t.starts_with(')') {
            in_block = false;
            continue;
        }
        let entry = t.split('#').next().unwrap_or("").trim();
        if let Some(idx) = entry.find('}') {
            let tail = &entry[idx + 1..];
            if tail.ends_with(".scad") {
                tails.push(tail.to_string());
            }
        }
    }
    tails
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMAKE_FIXTURE: &str = r"
set(MISC_FILES
  ${TEST_SCAD_DIR}/misc/variable-overwrite.scad
)

list(APPEND FAILING_FILES
  ${TEST_SCAD_DIR}/issues/issue1890-comment.scad
  ${TEST_SCAD_DIR}/issues/issue1890-include.scad
  ${CCBD}/data/scad/issues/issue2342.scad
  # a comment line
)

add_failing_test(parsererrors SUFFIX stl FILES ${FAILING_FILES} ARGS --retval=1)
";

    #[test]
    fn failing_files_blocks_parse_to_rooted_tails() {
        let tails = parse_failing_files(CMAKE_FIXTURE);
        assert_eq!(
            tails,
            vec![
                "/issues/issue1890-comment.scad".to_string(),
                "/issues/issue1890-include.scad".to_string(),
                "/data/scad/issues/issue2342.scad".to_string(),
            ],
            "only FAILING_FILES entries, variable-stripped, other lists ignored"
        );
    }

    #[test]
    fn golden_echo_lines_strip_warnings_and_skip_error_goldens() {
        let dir = std::env::temp_dir().join(format!("echo-golden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mixed-expected.echo"),
            "WARNING: something in file x, line 1\nECHO: 1\nTRACE: called by y\nECHO: \"two\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("failing-expected.echo"),
            "ECHO: 1\nERROR: Assertion failed in file x, line 2\n",
        )
        .unwrap();
        assert_eq!(
            golden_echo_lines(&dir, "/c/tests/data/scad/misc/mixed.scad"),
            Some(vec!["ECHO: 1".to_string(), "ECHO: \"two\"".to_string()]),
            "only ECHO lines survive"
        );
        std::fs::write(
            dir.join("multiline-expected.echo"),
            "ECHO: \"a\nb\"\nWARNING: w in file x\nc-not-attached\nECHO: 2\n",
        )
        .unwrap();
        assert_eq!(
            golden_echo_lines(&dir, "/c/tests/data/scad/misc/multiline.scad"),
            Some(vec!["ECHO: \"a\nb\"".to_string(), "ECHO: 2".to_string()]),
            "continuation lines re-attach to their ECHO; lines under a WARNING drop"
        );
        assert_eq!(
            golden_echo_lines(&dir, "/c/tests/data/scad/misc/failing.scad"),
            None,
            "an ERROR golden documents a failing run — not this lane's to check"
        );
        assert_eq!(
            golden_echo_lines(&dir, "/c/tests/data/scad/misc/nogolden.scad"),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn echo_divergence_finds_the_first_mismatch_and_caps_monsters() {
        let ours = vec!["ECHO: 1".to_string(), "ECHO: undef".to_string()];
        let golden = vec!["ECHO: 1".to_string(), "ECHO: { a = 1; }".to_string()];
        let d = first_echo_divergence(&ours, &golden).expect("diverges");
        assert!(d.contains("echo line 2") && d.contains("undef") && d.contains("{ a = 1; }"));

        assert_eq!(first_echo_divergence(&ours, &ours.clone()), None);

        let short = vec!["ECHO: 1".to_string()];
        let d = first_echo_divergence(&short, &golden).expect("length mismatch diverges");
        assert!(d.contains("<none>"), "missing side reads <none>: {d}");

        let monster = vec![format!("ECHO: {}", "x".repeat(500))];
        let d = first_echo_divergence(&monster, &golden).expect("diverges");
        assert!(d.len() < 400, "capped, not 500+ chars: {}", d.len());
    }

    /// Build a throwaway openscad-root with a CMakeLists + goldens, exercising every classify rule.
    #[test]
    fn classify_covers_all_four_rules_and_stays_none_for_genuine() {
        let root = std::env::temp_dir().join(format!("sweep-expect-{}", std::process::id()));
        let echo = root.join("tests/regression/echo");
        std::fs::create_dir_all(&echo).unwrap();
        std::fs::write(root.join("tests/CMakeLists.txt"), CMAKE_FIXTURE).unwrap();
        std::fs::write(
            echo.join("recursion-test-function-expected.echo"),
            "ERROR: Recursion detected calling function 'crash' in file x, line 1\n",
        )
        .unwrap();
        std::fs::write(
            echo.join("linenumber-expected.echo"),
            "WARNING: Can't open library 'line 1'. in file linenumber.scad, line 1\n",
        )
        .unwrap();
        let exp = UpstreamExpectations::load(&root).unwrap();

        let golden_err = exp.classify(
            "/c/tests/data/scad/misc/recursion-test-function.scad",
            Bucket::Eval,
            "evaluation error: Recursion detected calling function 'crash'",
        );
        assert!(golden_err.is_some_and(|r| r.contains("golden echo expects failure")));

        let failing = exp.classify(
            "/c/tests/data/scad/issues/issue1890-comment.scad",
            Bucket::Parse,
            "parse error:",
        );
        assert!(failing.is_some_and(|r| r.contains("FAILING_FILES")));

        let template = exp.classify(
            "/c/tests/data/scad/templates/use-tests-template.scad",
            Bucket::Load,
            "Can't open library ''.",
        );
        assert!(template.is_some_and(|r| r.contains("template")));

        let load_parity = exp.classify(
            "/c/tests/data/scad/misc/linenumber.scad",
            Bucket::Load,
            "Can't open library 'line 1'.",
        );
        assert!(load_parity.is_some_and(|r| r.contains("same wall")));

        // A can't-open for a library upstream HAS (golden shows no such warning) stays genuine —
        // this is exactly the new-unwired-submodule case the verbatim match protects.
        let genuine_load = exp.classify(
            "/c/tests/data/scad/misc/linenumber.scad",
            Bucket::Load,
            "Can't open library 'MCAD/fonts.scad'.",
        );
        assert!(genuine_load.is_none());

        // No golden, no list membership → genuine.
        let genuine = exp.classify(
            "/c/tests/data/scad/misc/sub1/included.scad",
            Bucket::Load,
            "Can't open library 'not_exist.scad'.",
        );
        assert!(genuine.is_none());

        std::fs::remove_dir_all(&root).ok();
    }
}
