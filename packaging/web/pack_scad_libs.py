#!/usr/bin/env python3
"""Pack the scad-rs library tree the geom Worker resolves use/include against (W.3.6 Stage 2): BOSL2
(libs/BOSL2, the pinned submodule) keyed 'BOSL2/<name>', scad-lib (this commit) keyed '<name>', and
the web demo lib keyed '<name>' — the exact paths an `include <...>` resolves to (the native
OPENSCADPATH is libs/ + scad-lib/). One JSON of path->text; the app fetches it once, computes a
model's include closure in-memory, and hands that closure to the worker as Source::Bytes.libs."""
import glob
import json
import os
import sys

out = sys.argv[1]
libs = {}
for f in sorted(glob.glob("libs/BOSL2/*.scad")):
    libs["BOSL2/" + os.path.basename(f)] = open(f, encoding="utf-8", errors="replace").read()
for f in sorted(glob.glob("scad-lib/*.scad")):
    libs[os.path.basename(f)] = open(f, encoding="utf-8", errors="replace").read()
# The web demo lib (a tiny self-contained module the sourceless web boot renders — proves the
# fetch->closure->worker path without betting on full BOSL2 eval).
for f in sorted(glob.glob("packaging/web/web-demo/*.scad")):
    libs[os.path.basename(f)] = open(f, encoding="utf-8", errors="replace").read()
assert len(libs) > 50, f"suspiciously few lib files ({len(libs)}) — is the BOSL2 submodule checked out?"
json.dump(libs, open(out, "w"))
print(f"packed {len(libs)} lib files -> {out} ({os.path.getsize(out)} bytes)")
