//! Z.2: a `.scadproj` renders through the byte VFS — a project-local `include <hook.scad>` resolves from
//! the in-memory pack with NO filesystem, the exact mechanism the wasm worker uses. This ties the Z.1
//! container to the render path (`scadproj::project_render_sources` → `Source::Bytes` → kernel).

#![cfg(all(feature = "mesh-io", feature = "kernel"))]

use std::collections::BTreeMap;

use fab_scad::geomsg::Source;
use fab_scad::geomsvc::render_source_to_solid;
use fab_scad::scadproj;

#[test]
fn project_local_include_resolves_through_the_byte_vfs() {
    // A two-file project: the entry calls a module defined in a SIBLING file it includes. On the desktop
    // that resolves beside the file; here there is no file — only the in-memory project.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files.insert(
        "main.scad".into(),
        b"include <hook.scad>\nhook();\n".to_vec(),
    );
    files.insert(
        "hook.scad".into(),
        b"module hook() { cube([10, 10, 10]); }\n".to_vec(),
    );
    let project = scadproj::project_from_files(files, Some("main.scad".into()), None).unwrap();

    let (main, libs) = scadproj::project_render_sources(&project).unwrap();
    let solid = render_source_to_solid(&Source::Bytes { main, libs }, None)
        .expect("the project renders through the byte VFS");

    let (_verts, tris) = solid.to_indexed();
    assert!(
        !tris.is_empty(),
        "the cube from the included module produced geometry"
    );
}

#[test]
fn a_nested_relative_include_resolves() {
    // `include <sub/box.scad>` keyed by its project-relative path (not just basename).
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files.insert(
        "main.scad".into(),
        b"include <sub/box.scad>\nbox();\n".to_vec(),
    );
    files.insert(
        "sub/box.scad".into(),
        b"module box() { cube([5, 5, 5]); }\n".to_vec(),
    );
    let project = scadproj::project_from_files(files, Some("main.scad".into()), None).unwrap();
    let (main, libs) = scadproj::project_render_sources(&project).unwrap();
    let solid = render_source_to_solid(&Source::Bytes { main, libs }, None).unwrap();
    assert!(!solid.to_indexed().1.is_empty());
}
