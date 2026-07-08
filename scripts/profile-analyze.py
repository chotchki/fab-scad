#!/usr/bin/env python3
# Aggregate a samply-saved Firefox-profiler capture into self/inclusive/allocation tables.
# Companion to scripts/profile-model.sh; see docs/models-profile.md for the reading.
#
# samply's --save-only writes an UN-symbolicated profile (nativeSymbols empty, frames are
# lib-relative addresses). We symbolicate the worker's own frames with `atos` against the
# release binary — its __TEXT base is 0x100000000, so atos wants BASE+addr — and label
# system-library frames by dylib. Then: self = leaf frame per sample; inclusive = the unique
# set of frames on each sample's stack; allocation = every sample whose leaf is malloc/free/
# memmove/RawVec-grow, charged UP to the first non-plumbing caller (the SITE that allocated).
#
# Usage:  python3 scripts/profile-analyze.py <profile.json.gz> <path/to/release/binary>

import gzip, json, subprocess, collections, re, sys

if len(sys.argv) != 3:
    sys.exit("usage: profile-analyze.py <profile.json.gz> <release-binary>")
PROFILE, BIN = sys.argv[1], sys.argv[2]
BASE = 0x100000000  # macho __TEXT vmaddr; atos resolves lib-relative addrs at BASE+addr

d = json.load(gzip.open(PROFILE))
# The eval runs on a named worker thread; fall back to the busiest thread if the name changes.
threads = d["threads"]
t = next((th for th in threads if th.get("name") == "model-eval"), None)
if t is None:
    t = max(threads, key=lambda th: th.get("samples", {}).get("length", 0))
S = t["stringArray"]; ft = t["frameTable"]; fu = t["funcTable"]
st = t["stackTable"]; rt = t["resourceTable"]

res_lib = {i: (S[rt["name"][i]] if isinstance(rt["name"][i], int) else str(rt["name"][i]))
           for i in range(rt["length"])}
WORKER = BIN.split("/")[-1]

def func_lib(fi):
    r = fu["resource"][fi]
    return res_lib.get(r, "?") if r is not None and r >= 0 else "?"

# Batch-symbolicate every worker frame address with atos.
worker_addrs = sorted({ft["address"][fr] for fr in range(ft["length"])
                       if func_lib(ft["func"][fr]) == WORKER
                       and ft["address"][fr] not in (None, -1)})
sym = {}
def atos(addrs):
    args = ["atos", "-o", BIN, "-arch", "arm64"] + [hex(BASE + a) for a in addrs]
    out = subprocess.run(args, capture_output=True, text=True).stdout.splitlines()
    for a, line in zip(addrs, out):
        sym[a] = line.strip()
for i in range(0, len(worker_addrs), 1500):
    atos(worker_addrs[i:i + 1500])

def clean(name):
    name = re.sub(r"\s*\(in [^)]*\)", "", name)
    name = re.sub(r"\s*\([^)]*:\d+\)", "", name)
    name = re.sub(r"::h[0-9a-f]{16}", "", name)
    name = re.sub(r"\s*\+\s*\d+$", "", name)
    return name.strip()

def fname(fr):
    lib = func_lib(ft["func"][fr])
    if lib == WORKER:
        return clean(sym.get(ft["address"][fr], hex(ft["address"][fr])))
    return f"[{lib}]"

def is_alloc(nm):
    return (nm.startswith("[libsystem_malloc") or nm.startswith("[libsystem_platform")
            or "rdl_alloc" in nm or "rdl_dealloc" in nm or "finish_grow" in nm
            or "do_reserve" in nm or "DYLD-STUB" in nm or "rust_no_alloc_shim" in nm
            or "rc_inner_layout" in nm)

SKIP = ["raw_vec", "RawVec", "alloc..vec", "alloc::vec", "__rust", "memcpy", "memmove",
        "spec_from_iter", "SpecFromIter", "reserve", "grow", "Allocator",
        "alloc..alloc", "alloc::alloc"]

sam = t["samples"]; stk = sam["stack"]; wt = sam.get("weight") or [1] * len(stk)
total = sum(w for w in wt if w)

def frames_of(si):
    cur = si
    while cur is not None:
        yield st["frame"][cur]; cur = st["prefix"][cur]

self_ct = collections.Counter(); incl_ct = collections.Counter()
alloc_ct = collections.Counter(); alloc_total = 0
for s_i, w in zip(stk, wt):
    if s_i is None or w is None:
        continue
    leaf = fname(st["frame"][s_i])
    self_ct[leaf] += w
    seen = set()
    for fr in frames_of(s_i):
        nm = fname(fr)
        if nm not in seen:
            seen.add(nm); incl_ct[nm] += w
    if is_alloc(leaf):
        alloc_total += w
        for fr in frames_of(s_i):
            nm = fname(fr)
            if is_alloc(nm) or nm.startswith("["):
                continue
            if any(k in nm for k in SKIP):
                continue
            alloc_ct[nm] += w
            break

def table(title, counter, n=30):
    print(f"\n===== {title} =====")
    for nm, c in counter.most_common(n):
        print(f"{100 * c / total:6.2f}%  {c:7d}  {nm[:82]}")

print(f"thread={t.get('name')!r}  samples={total}")
print(f"allocation/memory-traffic samples: {alloc_total}  ({100 * alloc_total / total:.1f}% of all)")
table("TOP SELF (leaf frame)", self_ct)
table("TOP INCLUSIVE (subtree)", incl_ct)
table("ALLOCATION charged to nearest semantic caller", alloc_ct)
