# Geometry service — the one kernel seam (W.3)

The Manifold kernel stops being an in-process library call and becomes a **byte service behind one
seam**: the app sends a `Request`, gets a `Response`, and never touches a `Solid` or a file path. The
same seam runs two transports — a pool of **threads** on native, a pool of **Web Workers** on wasm — so
fab-gui is ONE codebase at full parity, desktop and web. This is fab-web's proven `geomsvc` model,
generalized to cover everything fab-gui asks of the kernel.

Why it exists (all three collapse to "the kernel can't be a local sync library"):
- `Solid` is `!Send` — today fab-gui works around that by serializing to **temp STL files** and
  reloading the `Solid` inside each background task. The files ARE a byte-transport, over disk.
- wasm is single-threaded — a synchronous kernel call would freeze the UI.
- the wasm kernel is `-fno-exceptions` — a `bad_alloc` is an unrecoverable **trap**, so the kernel must
  be isolated (a dead worker, not a dead app).

The service replaces disk with a channel, gives the kernel ONE home, and isolates the trap. Desktop
wins too: crash-isolation, no serialize→disk→deserialize between stages, still non-blocking.

## What crosses the seam — and what does NOT

ONLY the 5 ops that touch a `Solid`/kernel/FFI cross:

| Request        | maps fab.rs         | handle lifecycle              |
|----------------|---------------------|-------------------------------|
| `RenderWhole`  | `render_whole`      | **mints** 1 id                |
| `RenderParts`  | `render_parts`      | **mints** N ids               |
| `Reslice`      | `reslice_part_kernel` | **reads** a base id         |
| `CrossSection` | `cross_section`     | **reads** a base id           |
| `AutoPlan`     | `auto_plan`         | **reads** a base id           |
| `PrintLayout`  | `print_layout_kernel` | **reads** a base id         |
| `Free`         | (lifecycle)         | **drops** ids                 |

Everything else stays **in-process** in fab-gui — it's pure Rust math (no `Solid`), compiles to wasm
fine, and must NOT go async:
- `conn_feasibility` (`slicing::onion_feasibility` takes a `Slicing` spec, no base),
- `copack_summary` + the plate-packing / 3MF assembly (`export_plates`) — pure mesh math on bytes we
  already hold.

This is load-bearing: `estimate_copack` and the feasibility check are **synchronous per-frame Bevy
systems**. The transport is async; you cannot `.await` in a per-frame system (can't block the frame on
native, physically impossible on wasm). So pure math stays synchronous and local; only the heavy
`Solid`-touching ops go async-over-the-seam. (fab-web's `Analyze`/`Slice`/`Export`/`Section` upload arms
stay too, unchanged.)

## Handles + the store

A base `Solid` is a pure function of source; it's reused across MANY calls (a `Reslice` fires on every
cut-drag settle). So the service **holds it, addressed by an opaque handle** — re-shipping the mesh per
call, or re-rendering from source per drag, are both non-starters (the latter is strictly worse than
the disk path we're replacing).

```rust
// wire: plain data, Send + Serialize. The `!Send` Solid NEVER appears in a boundary type.
struct SolidId { shard: u16, idx: u32 }          // shard-tagged — see routing
struct SolidStore { map: HashMap<SolidId, Solid>, next: u32, cap: usize }  // one per shard
```

`geomsvc::handle` gains the store: `handle(&mut SolidStore, Request) -> Response`. Render **mints** ids
into the store and returns (display-STL bytes + the ids); read-ops look the `Solid` up **read-only** and
NEVER free it.

**Lifecycle — free only on these, never after a read:**
- render re-mint (a part re-rendered → `Free` its old ids),
- `ModelState::reset` (file switch → `Free` all),
- part-count change on reload, and **`refresh_part`** (external-save reload re-mints → `Free` the old —
  don't leak N per save),
- LRU cap (~64/shard) as the backstop against a missed `Free`.

**Self-heal:** an op on an evicted / freed / crash-lost id returns `Failed{"unknown handle"}`, which the
GUI treats identically to a cache miss → **re-render from source to re-mint**. Evicted-handle,
worker-death, and stale-id all collapse to ONE recovery path.

## Sharding (designed in now, one shard to start)

Solids are `!Send`, so a single store on a single thread **serializes all geometry** — a hot reslice
could stall behind a heavy print-layout. The fix is per-part sharding, but we bake it into the TYPES now
and launch one shard, so scaling is a knob (not a protocol rewrite):

```rust
struct GeomPool { shards: Vec<Shard> }   // Shard = a kernel thread (native) / Worker (wasm), each owns ITS store
```
- `SolidId` carries its `shard`; the pool **routes every read-op by `id.shard`**.
- Render **assigns** a shard on mint (policy: `RenderParts` spreads part `i` → `i % N`; a future knob).
  The minted ids carry that shard, so all downstream ops for that part route home.
- **N = 1 today** (`assign` → shard 0, everything on one thread). Bumping N + the per-part assignment is
  a config change; the protocol, ids, and routing don't move. Same abstraction on wasm = N Workers
  (independent contexts, message-passing — no SharedArrayBuffer needed; that's the orthogonal TBB
  question, W.3.9).

## Protocol (extend `src/geomsg.rs`)

Keep the 4 fab-web variants; ADD fab-gui's. New wire types (all `Serialize`/`Deserialize`, `Send`,
bincode-total — same discipline as `GeomObject`): `SolidId`, `WirePart { id, stl, min, max, name }`,
`WirePiece { piece:[usize;3], comp, stl, up }`, `WireOrient { piece, up }`. Reuse `WireConn`, `PlanOut`,
`Sectioned.loops`. **No `Solid`/kernel type is ever a field** — meshes travel as raw STL `Vec<u8>`,
sections as `Vec<Vec<[f64;2]>>`.

`Render*` carries a **source form that anticipates wasm** (no fs in the Worker) so it doesn't churn at
W.3.6/W.3.8:
```rust
enum Source { Path(String), Bytes { main: Vec<u8>, libs: Vec<(String, Vec<u8>)> } }
RenderWhole { source: Source, root: Option<String> }   // preview flag dropped — both hardcode $preview=true
RenderParts { source: Source, root: Option<String> }
```
Native uses `Path` for now; wasm sends `Bytes` + a preloaded import/lib bundle. Read-ops:
`Reslice { base: SolidId, cuts, connectors: Vec<WireConn>, orient: Vec<WireOrient>, spread }`,
`CrossSection { base, axis, at }`, `AutoPlan { base, min, max, bed }`, `PrintLayout { base, cuts, connectors }`,
`Free { ids: Vec<SolidId> }`. Responses: `Rendered{id,stl,min,max}`, `PartsRendered{parts}`,
`Resliced{stl}`, reuse `Sectioned{loops}`, `Planned{cuts,connectors,pieces}`, `LaidOut{pieces}`, `Freed`,
reuse `Failed{error}`. (Copack/export/feasibility responses don't exist — those are in-process.)

The OpenSCAD-subprocess `reslice` twin does NOT go on the seam — it's desktop-only and retired under
W.3's "scad-rs + Manifold everywhere, no OpenSCAD".

## Transport (enum, not `dyn`)

`fn call() -> impl Future` (RPITIT) is not `dyn`-compatible, so `Arc<dyn _>` is out. Enum-dispatch:

```rust
enum Geom { Native(GeomPool), Wasm(WasmPool) }   // held as a Bevy Resource, cloneable Send handle
impl Geom { async fn call(&self, req: Request) -> anyhow::Result<Response> }  // Err = TRANSPORT death; domain errors ride Ok(Failed)
```
- **Native shard** = one `std::thread` owning its `SolidStore`, `mpsc<(Request, oneshot::Sender<Response>)>`
  (both `Send`), loop `reply.send(catch_unwind(|| handle(&mut store, req)))`. `Solid` never appears in a
  channel type → `!Send` is compile-enforced (a stray `Solid` fails `spawn`'s `Send` bound).
- **wasm shard** = fab-web's `geom_worker` + `worker_rpc` (bincode over `postMessage`, transferable
  ArrayBuffer, id-matched replies).

The native `call` future is `Send` (rides `AsyncComputeTaskPool`); the wasm future is `!Send` but wasm is
single-threaded so Bevy's task pool is fine.

## Error / isolation model

1. **Domain** (bad STL, empty geo, unknown handle) → `anyhow::Result` → `Response::Failed`; never errors
   the transport. `Failed{"unknown handle"}` → GUI re-renders.
2. **Rust panic** in `handle` → `catch_unwind` in the loop → `Failed{payload}`, loop lives (native).
   NB on wasm `panic=abort` makes `catch_unwind` a no-op → it degrades to tier 3.
3. **Hard C++ trap** (Manifold `bad_alloc`/abort) — uncatchable in-process. Native THREAD = process-wide
   abort (NO isolation — the honest limit of the thread transport; the `SubprocessTransport` follow-up or
   the wasm Worker gets true isolation). Worker/subprocess = dies in a separate address space; the parent
   detects death (EOF / `worker.onerror`), **drains the pending map to `Err`**, and **respawns fresh**.
   - **Respawn fix (both):** the dead shard MUST be cleared — wasm: null the `WORKER` thread_local (the
     reused `worker_rpc` drains pending but never clears it → every later call hangs → app wedged, the
     self-heal never fires); native: wrap the WHOLE loop body and use `let _ = reply.send(..)` (a
     superseded call drops its receiver — routine — and must not `unwrap`-kill the loop). After respawn
     the store is empty → all handles miss → re-render self-heals.

## fab-gui changes (the async shape is PRESERVED)

Today's `kick_* → AsyncComputeTaskPool::spawn → poll_once` flow stays; only the task BODY + payload
change (`transport.call(Request::..).await`, map `Response` → `JobResult`).
- **fab.rs**: the kernel functions collapse to thin `Request`-builders / `Response`-unpackers; every
  `use fab_scad::kernel::Solid`, `Solid::from_stl_file`, and temp-STL write DELETES. `ConnKind`/`Conn`/
  `Orient3`/`PiecePrint` GUI types stay (they map to `WireConn`/`WireOrient`/`WirePiece`).
- **state.rs**: `Part.base_stl: PathBuf` → `Part.base: SolidId`; `whole`/`sliced` `Handle<Mesh>` built
  from Response STL bytes (`mesh_and_bounds(&[u8])`); a `Geom` Resource wraps the transport.
- **jobs.rs**: `JobResult` carries `WirePart`/bytes+ids; `poll_*`/`build_part`/`refresh_part` build
  meshes from bytes; issue `Free` on reset / part-count-change / `refresh_part`.
- **print.rs**: `PrintJob` → `PrintLayout`; `estimate_copack`/`export_plates_action`/`sync_feasibility`
  stay IN-PROCESS on the mesh bytes (pure). Export 3MF bytes: native writes at the file-dialog edge, web
  hands to a Blob (W.3.4). Preset travels as DATA, not a name (no printers.toml in the Worker).
- **lib.rs**: build the `Geom` Resource at startup — native spawns the thread pool; wasm builds the
  Worker pool.

## Known tradeoff

At N=1 all geometry serializes on one thread — measure the reactive loop in dogfooding (the hot reslice
is single-slot-debounced; heavy ops are user-triggered, so contention should be rare). The escape hatch
is already in the types: bump `GeomPool` shards + per-part assignment. And the thread transport is NOT
crash-isolated (a Manifold abort takes the app down) — the `SubprocessTransport` follow-up closes that on
native and mirrors the wasm Worker.

## W.3.3 implementation order (native first, verify on the fast loop)

1. fab_scad: `SolidId`/`WirePart`/`WirePiece`/`WireOrient` in `geomsg`; `SolidStore` + `handle(&mut store, Request)`
   in `geomsvc` (existing 4 arms ignore the store, stay stateless — regression-test the codec).
2. fab_scad: implement each new arm by MOVING the matching fab.rs body server-side (render mints; read-ops
   `store.get(id)` instead of `Solid::from_stl_file`); unit-test every arm through encode/decode, incl. a
   Render→reuse-id→Reslice→Free round-trip.
3. fab_scad (feature=native): `Shard` (thread + store + `catch_unwind`) and `GeomPool` (routing by
   `id.shard`, `assign` on mint); test spawn/mint/reuse/free + "op on freed id → Failed".
4. gui: `Geom` enum Resource (native = pool) in lib.rs; `Part.base: SolidId`; gut fab.rs of kernel/Solid/temp-STL.
5. gui: rewire jobs.rs / print.rs task bodies to `transport.call`; meshes from bytes; `Free` on the lifecycle points.
6. Error pass: `catch_unwind` + loop-survival + `let _ = reply.send`; confirm task bodies are `Send`.
7. Verify: `cargo nextest run --workspace` + `--doc` (588); dogfood — drag a cut, confirm `Reslice` runs
   off the held handle with NO temp STL written, a bad-geometry model surfaces as a status error not a
   crash, and a file-switch `Free`s the old handles.
