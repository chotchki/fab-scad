//! AR.37 — transpile a pinned OpenSCAD library into `OUT_DIR`, for a build script to call.
//!
//! Lifted out of `fab-bosl2`'s build.rs verbatim when MCAD became the second library, because the
//! only BOSL2-specific things in those 228 lines were a path and a sentinel filename. Duplicating
//! them per library is how the copies drift; a library crate is now the ~15 lines that say WHICH
//! library, plus the ~70 that declare what came out.
//!
//! DECLINES ARE NOT FAILURES. [`function_band`](crate::emit::function_band) drops what the emitter
//! cannot compile and says so; this writes the survivors and reports the rest. A library the
//! transpiler covers PART of is a library that works — the remainder interprets, which is what all
//! of it did before any of this existed. The build fails only when the emitter cannot produce a
//! file at all.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One library to transpile.
pub struct Library<'a> {
    /// The library's own name, as it appears in cargo warnings. Not derived from the directory:
    /// the message is read by a human looking for which crate is talking.
    pub name: &'a str,
    /// The directory of `.scad` files — a pinned submodule, in every case so far.
    pub root: &'a Path,
    /// A file whose ABSENCE means the submodule was never checked out. Per-library because there
    /// is no universal one (BOSL2 has `std.scad`, MCAD does not).
    pub sentinel: &'a str,
    /// The wrapper filename written into `OUT_DIR`, which the crate root `include!`s.
    pub out_file: &'a str,
}

/// The stack the transpile runs on, matching `fab_scad::EVAL_STACK` — the same 64 MiB, for the same
/// reason, at the one layer that had never been given it. (Not that constant: fab-lib cannot depend
/// on fab-scad, the dependency runs the other way.)
///
/// A build script gets the process's MAIN thread, and that thread's stack is fixed at LINK time —
/// 1 MiB on `x86_64-pc-windows-msvc` against 8 MiB on macOS and Linux, and nothing inside the
/// program can raise it. The transpiler walks the AST by recursion, so the ceiling it is really
/// working against is 1 MiB on exactly one platform. MCAD hit it: STATUS_STACK_OVERFLOW
/// (0xc00000fd) out of `fab-mcad`'s build script, transpiling source macOS takes without complaint.
/// Everything else in this workspace that recurses deeply already runs on an explicit stack —
/// `EVAL_STACK` has fourteen call sites — and build scripts were simply the place nobody had
/// reached yet, because CI has never built on Windows.
const TRANSPILE_STACK: usize = 64 * 1024 * 1024;

/// Transpile `lib` into `OUT_DIR`, emitting cargo directives as it goes.
///
/// Runs on a spawned thread with [`TRANSPILE_STACK`] rather than the build script's own — see there
/// for why. Scoped, so `lib`'s borrows need no `'static`; a panic inside is resumed here so it
/// still fails the build with its own message rather than a bare join error.
///
/// # Panics
/// When `OUT_DIR` is unwritable, or the emitter cannot produce a file at all — both are build
/// failures with nothing useful to fall back to. A per-function decline is NOT one of these.
pub fn transpile(lib: &Library) {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("transpile".into())
            .stack_size(TRANSPILE_STACK)
            .spawn_scoped(scope, || transpile_inner(lib))
            .expect("spawn the transpile thread");
        if let Err(panic) = handle.join() {
            std::panic::resume_unwind(panic);
        }
    });
}

fn transpile_inner(lib: &Library) {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let root = lib.root;
    let name = lib.name;

    // A build script that reads files must SAY which, or cargo serves a stale transpile past an
    // upstream bump. The directory itself is watched too, so a NEW .scad triggers a rebuild — a
    // per-file list alone cannot see a file that did not exist when the list was made.
    println!("cargo:rerun-if-changed={}", root.display());
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "scad") {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }

    if !root.join(lib.sentinel).exists() {
        // A submodule-less checkout still BUILDS — it just declares nothing. Loud, and visible in
        // the surface (`transpiled()` answers false), never a silent half-library: a missing asset
        // costs a PART rather than an error, and this is exactly where that would start.
        println!(
            "cargo:warning={} is not checked out — {name} declares NOTHING. \
             Run `git submodule update --init` and rebuild.",
            root.display()
        );
        std::fs::write(out.join(lib.out_file), empty_library(name, root))
            .expect("OUT_DIR is writable");
        return;
    }

    let read = crate::library::Library::read(root)
        .unwrap_or_else(|e| panic!("{name} does not read: {e:?}"));
    for (file, why) in &read.unparsed {
        // A file our grammar cannot take is a real coverage hole and it must not be silent — it
        // shrinks every number downstream while everything still passes.
        println!("cargo:warning={name}: {file} does not parse ({why}) — its functions are absent");
    }

    let band = crate::emit::function_band(&read)
        .unwrap_or_else(|e| panic!("the {name} function band does not compute: {e}"));
    let declared: Vec<&str> = read.functions.values().map(|f| f.source.as_str()).collect();
    let functions = crate::emit::generate_batch(&band.subjects, &declared)
        .unwrap_or_else(|e| panic!("the {name} function band does not emit: {e}"));
    println!(
        "cargo:warning={name}: {} of {} functions compiled ({} declined, {} fixpoint rounds)",
        band.subjects.len(),
        read.functions.len(),
        band.declined.len(),
        band.rounds
    );

    // The MODULE band ships as its own set of files, one per library source plus a spine. Same
    // reason as AR.23 — the split keeps each source's output separable — and it costs nothing here.
    let module_files = crate::emit::generate_standalone_modules(root)
        .unwrap_or_else(|e| panic!("the {name} module band does not emit: {e}"));
    let module_dir = out.join("modules");
    std::fs::create_dir_all(&module_dir).expect("OUT_DIR is writable");
    let mut spine = String::new();
    for (file, text) in &module_files {
        if file == "mod.rs" {
            spine.clone_from(text);
        } else {
            std::fs::write(module_dir.join(file), text)
                .unwrap_or_else(|e| panic!("writing {file}: {e}"));
        }
    }
    assert!(!spine.is_empty(), "the {name} module band emitted no spine");

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
    let mut wrapper =
        format!("// GENERATED by {name}'s build — not in version control, not editable.\n\n");
    write_module(&mut wrapper, "functions", &functions);
    write_module(&mut wrapper, "modules", &fixed);
    wrapper.push_str("pub(crate) const TRANSPILED: bool = true;\n");
    std::fs::write(out.join(lib.out_file), wrapper).expect("OUT_DIR is writable");
}

/// What a checkout without the submodule gets: a library that declares nothing, and says so.
fn empty_library(name: &str, root: &Path) -> String {
    format!(
        "// GENERATED by {name}'s build — {} was not checked out.\n\
         mod functions {{\n\
         \x20   pub(super) static REGISTRY: &[fab_lang::rt::Entry] = &[];\n\
         \x20   pub(super) static SURFACE: &[fab_lang::rt::Decl] = &[];\n\
         }}\n\
         mod modules {{\n\
         \x20   pub(super) static REGISTRY: &[super::ModuleEntry] = &[];\n\
         }}\n\
         pub(crate) const TRANSPILED: bool = false;\n",
        root.display()
    )
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
