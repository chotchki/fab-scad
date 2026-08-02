//! AR.26.3 — transpile BOSL2 at BUILD time, into `OUT_DIR`.
//!
//! The whole point of the phase in one file: the generated Rust never enters version control, so
//! there is nothing to keep current and no regen gate to run. A BOSL2 bump changes the submodule
//! pointer and the next build re-transpiles; the diff worth reviewing is the SUBMODULE's, and the
//! regression that actually matters — functions falling out of coverage — is caught by the ratchet,
//! which is a test rather than an artifact.
//!
//! DECLINES ARE NOT FAILURES. `function_band` drops what the emitter cannot compile and says so;
//! this script writes the survivors and reports the rest. A library the transpiler covers PART of is
//! a library that works — the remainder interprets, which is what all of it did before any of this
//! existed. The build fails only when the emitter cannot produce a file at all.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let root = manifest
        .parent()
        .expect("the crate has a parent directory")
        .join("libs/BOSL2");

    // A build script that reads files must SAY which, or cargo serves a stale transpile past an
    // upstream bump. The directory itself is watched too, so a NEW .scad triggers a rebuild — a
    // per-file list alone cannot see a file that did not exist when the list was made.
    println!("cargo:rerun-if-changed={}", root.display());
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "scad") {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }

    if !root.join("std.scad").exists() {
        // A submodule-less checkout still BUILDS — it just declares nothing. Loud, and visible in
        // the surface (`transpiled()` answers false), never a silent half-library: a missing asset
        // costs a PART rather than an error, and this is exactly where that would start.
        println!(
            "cargo:warning=libs/BOSL2 is not checked out — fab-bosl2 declares NOTHING. \
             Run `git submodule update --init` and rebuild."
        );
        std::fs::write(out.join("bosl2.rs"), EMPTY_LIBRARY).expect("OUT_DIR is writable");
        return;
    }

    let lib = fab_lib::library::Library::read(&root)
        .unwrap_or_else(|e| panic!("BOSL2 does not read: {e:?}"));
    for (file, why) in &lib.unparsed {
        // A file our grammar cannot take is a real coverage hole and it must not be silent — it
        // shrinks every number downstream while everything still passes.
        println!(
            "cargo:warning=fab-bosl2: {file} does not parse ({why}) — its functions are absent"
        );
    }

    let band = fab_lib::emit::function_band(&lib)
        .unwrap_or_else(|e| panic!("the function band does not compute: {e}"));
    let declared: Vec<&str> = lib.functions.values().map(|f| f.source.as_str()).collect();
    let functions = fab_lib::emit::generate_batch(&band.subjects, &declared)
        .unwrap_or_else(|e| panic!("the function band does not emit: {e}"));
    println!(
        "cargo:warning=fab-bosl2: {} of {} functions compiled ({} declined, {} fixpoint rounds)",
        band.subjects.len(),
        lib.functions.len(),
        band.declined.len(),
        band.rounds
    );

    // The MODULE band ships as its own set of files, exactly as it does in-tree: one per BOSL2
    // source plus a spine. Same reason as AR.23 — the split keeps each source's output separable —
    // and it costs nothing here.
    let module_files = fab_lib::emit::generate_standalone_modules(&root)
        .unwrap_or_else(|e| panic!("the module band does not emit: {e}"));
    let module_dir = out.join("modules");
    std::fs::create_dir_all(&module_dir).expect("OUT_DIR is writable");
    let mut spine = String::new();
    for (name, text) in &module_files {
        if name == "mod.rs" {
            spine.clone_from(text);
        } else {
            std::fs::write(module_dir.join(name), text)
                .unwrap_or_else(|e| panic!("writing {name}: {e}"));
        }
    }
    assert!(!spine.is_empty(), "the module band emitted no spine");

    // The spine declares its members as `mod ident;`, which resolves against the file that
    // CONTAINS the declaration — and this one arrives through `include!`, where that is not a
    // path anybody can predict. Point each at its absolute OUT_DIR file instead. Rewritten here
    // rather than in the emitter on purpose: the emitted text stays the text fab-lang compiles,
    // so the two consumers cannot drift, and a generated file that must be edited to move is a
    // file nobody can diff.
    let mut fixed = String::new();
    for line in spine.lines() {
        if let Some(ident) = line.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) {
            // The FILE is named for the ident VERBATIM — `r#move.rs` when the source stem is a
            // Rust keyword — so the path must not strip the escape. Stripping it worked only
            // because no BOSL2 source is named after a keyword today, and `scad_mod_ident`'s whole
            // promise is that an upstream RENAME cannot produce an unbuildable regen.
            let path = module_dir.join(format!("{ident}.rs"));
            let _ = writeln!(
                fixed,
                "#[path = {:?}] mod {ident};",
                path.display().to_string()
            );
        } else {
            fixed.push_str(line);
            fixed.push('\n');
        }
    }

    // ONE wrapper file, included at the crate root. Both generated halves open with INNER
    // attributes (`#![allow(…)]`), which `include!` inside a `mod { }` body rejects outright — so
    // the attributes are hoisted onto the wrapping `mod` as OUTER ones. Hoisting rather than
    // hand-mirroring them in lib.rs is what keeps the allow-list owned by the emitter: a lint the
    // generated idiom needs is added in one place and arrives here automatically.
    let mut wrapper = String::from(
        "// GENERATED by fab-bosl2's build.rs — not in version control, not editable.\n\n",
    );
    write_module(&mut wrapper, "functions", &functions);
    write_module(&mut wrapper, "modules", &fixed);
    wrapper.push_str("pub(crate) const TRANSPILED: bool = true;\n");
    std::fs::write(out.join("bosl2.rs"), wrapper).expect("OUT_DIR is writable");
}

/// Wrap `text` as `mod name { … }`, moving any leading INNER attributes out to OUTER ones on the
/// module itself.
fn write_module(out: &mut String, name: &str, text: &str) {
    let (attrs, body) = hoist_inner_attributes(text);
    for a in &attrs {
        // `#![x]` -> `#[x]`; the span is the module either way.
        let _ = writeln!(out, "#{}", &a[2..]);
    }
    let _ = writeln!(out, "mod {name} {{\n{body}\n}}\n");
}

/// Split a generated file into its leading inner attributes and the rest.
///
/// Brace/bracket counting rather than a line prefix: the emitter's `#![allow(…)]` spans a dozen
/// lines with a `reason = "…"` inside it. Stops at the first item, so nothing further down is
/// touched.
fn hoist_inner_attributes(text: &str) -> (Vec<String>, String) {
    let mut attrs = Vec::new();
    let mut rest = String::new();
    let mut lines = text.lines().peekable();
    let mut current: Option<(String, i32)> = None;
    while let Some(line) = lines.peek() {
        if let Some((buf, depth)) = current.as_mut() {
            buf.push('\n');
            buf.push_str(line);
            *depth += bracket_delta(line);
            if *depth <= 0 {
                attrs.push(std::mem::take(buf));
                current = None;
            }
            lines.next();
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            rest.push_str(line);
            rest.push('\n');
            lines.next();
            continue;
        }
        if trimmed.starts_with("#![") {
            let depth = bracket_delta(line);
            if depth <= 0 {
                attrs.push((*line).to_string());
            } else {
                current = Some(((*line).to_string(), depth));
            }
            lines.next();
            continue;
        }
        break; // the first real item — everything from here is body
    }
    for line in lines {
        rest.push_str(line);
        rest.push('\n');
    }
    (attrs, rest)
}

/// Net bracket depth a line contributes, ignoring anything inside a string literal.
fn bracket_delta(line: &str) -> i32 {
    let mut depth = 0;
    let mut in_str = false;
    let mut escaped = false;
    for c in line.chars() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// What a checkout without the BOSL2 submodule gets: a library that declares nothing, and says so.
const EMPTY_LIBRARY: &str = "\
// GENERATED by fab-bosl2's build.rs — libs/BOSL2 was not checked out.
mod functions {
    pub(super) static REGISTRY: &[fab_lang::rt::Entry] = &[];
    pub(super) static SURFACE: &[fab_lang::rt::Decl] = &[];
}
mod modules {
    pub(super) static REGISTRY: &[super::ModuleEntry] = &[];
}
pub(crate) const TRANSPILED: bool = false;
";
