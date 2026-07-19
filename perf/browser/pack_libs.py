#!/usr/bin/env python3
"""Pack the scad libraries the worker mounts at /libraries: BOSL2 (the libs/ submodule, at its
pinned tag) + scad-lib (this commit). One JSON of path -> text — no unzip machinery in the
worker, brotli on the wire does the compression."""
import glob
import json
import os
import sys

out = sys.argv[1]
libs = {}
for f in sorted(glob.glob("libs/BOSL2/*.scad")):
    libs["libraries/BOSL2/" + os.path.basename(f)] = open(f, encoding="utf-8", errors="replace").read()
for f in sorted(glob.glob("scad-lib/*.scad")):
    libs["libraries/" + os.path.basename(f)] = open(f, encoding="utf-8", errors="replace").read()
assert len(libs) > 50, f"suspiciously few lib files ({len(libs)}) — submodule not checked out?"
json.dump(libs, open(out, "w"))
print(f"packed {len(libs)} lib files -> {out} ({os.path.getsize(out)} bytes)")
