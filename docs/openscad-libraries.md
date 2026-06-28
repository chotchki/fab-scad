# Resolving OpenSCAD libraries (OPENSCADPATH)

Canonical include form across all designs:

    include <BOSL2/std.scad>          // library-path form — USE THIS
    // not: include <../BOSL2/std.scad>   // brittle relative path, being migrated out

OpenSCAD resolves `<...>` includes against `OPENSCADPATH`. `fab-scad` owns the pinned
toolchain, so point `OPENSCADPATH` at its lib dirs:

    OPENSCADPATH="$HOME/workspace/fab-scad/libs:$HOME/workspace/fab-scad/scad-lib"

- `libs/`     — pinned third-party deps (BOSL2 @ v2.0.746, more to come)
- `scad-lib/` — my shared modules (slicer, connectors, version-stamp, part-numbering)

## Interactive OpenSCAD (the .app)

Add the export to your shell so the GUI resolves includes too (OpenSCAD.app reads the env
at launch):

    # ~/.zshrc
    export OPENSCADPATH="$HOME/workspace/fab-scad/libs:$HOME/workspace/fab-scad/scad-lib"

## Headless / fab renders

`fab render` sets `OPENSCADPATH` itself — the pipeline needs no global env. The shell export
above is only for opening designs in the OpenSCAD GUI by hand.

## Bumping BOSL2 (deliberate, never silent)

Pinned as a submodule at a tag:

    cd libs/BOSL2 && git fetch --tags && git checkout <new-tag>
    cd ../..      && git add libs/BOSL2 && git commit -m "Bump BOSL2 to <new-tag>"

Verified: `include <BOSL2/std.scad>; cuboid([10,10,10]);` renders a clean cube with
`OPENSCADPATH` set as above (OpenSCAD 2026.06.12, BOSL2 v2.0.746).
