# SZ.4.1 — wasm side modules, proven end to end

The question this answers: can a transpiled library live in its OWN wasm module, loaded only when a
model needs it, without paying to marshal `Value` across the boundary?

**Yes.** Run it:

```
cd main && RUSTFLAGS='-C link-arg=--export-table -C link-arg=--export=__heap_base -C link-arg=--export=__data_end -C link-arg=--growable-table' \
  cargo +nightly build --release --target wasm32-unknown-unknown
cd ../side && RUSTFLAGS='-C relocation-model=pic -C link-arg=--experimental-pic -C link-arg=-shared -C link-arg=--unresolved-symbols=import-dynamic' \
  cargo +nightly build --release --target wasm32-unknown-unknown -Z build-std=std,panic_abort
node ../run.mjs main/target/.../mainmod.wasm side/target/.../sidemod.wasm
# side_roundtrip(10) = 110, expected 110 -> PASS
```

## What it proves, and why each part matters

`side_roundtrip` makes the three crossings a real native makes, and all three work:

| crossing | what it proves |
|---|---|
| side allocates a `Vec` | its `#[global_allocator]` forwards to the host's — ONE heap, not two over the same bytes |
| side hands the host a raw pointer, host sums it | shared linear memory: a `&[Value]` from one module is valid in the other |
| side calls back into the host | the `FnCtx` direction — a native re-entering the evaluator |

The second row is the load-bearing one. It kills the objection that separate modules mean separate
memories and therefore per-call marshalling — which would have made a native SLOWER than
interpreting and inverted the whole point of the compiled tier. The side module imports
`env.memory`, so there is no marshalling at all: a native call is an indirect call through the
shared table.

## What makes it work

- **PIC + `-Z build-std`.** Stable's precompiled `libstd` is not built `-fPIC`, so a side module
  fails to link against std itself (`relocation R_WASM_MEMORY_ADDR_SLEB cannot be used against
  symbol ...; recompile with -fPIC`). Rebuilding std with PIC fixes it. Nightly + `build-std` is
  NOT a new requirement here — the geom worker already builds that way for threads.
- **The allocator shim.** Without it the side module links its OWN `__rust_alloc` and runs a second
  heap over the shared memory. `side/src/lib.rs`'s `HostHeap` forwards to imported `host_alloc`/
  `host_dealloc`; the import list changes from "none" to exactly those symbols, which is how you
  can tell it took.
- **The loader supplies** `env.memory`, `env.__indirect_function_table`, `__memory_base`,
  `__table_base`, `__stack_pointer`, and the `GOT.mem`/`GOT.func` globals. Main must be built with
  `--export-table` or the table import cannot be satisfied.
- **Memory must be grown before instantiation.** The side module's data segments are placed AT
  `__memory_base`, so that region has to exist first, or instantiation fails with `data segment 0
  is out of bounds`.

## What it does NOT prove — read this before building on it

- **The 70 GOT globals are stubbed to zero.** A real loader resolves each to its symbol's address;
  this probe gets away with it because the call under test touches none of them (they are std
  internals — unicode tables, the panic counter). Resolving them properly is the bulk of the
  remaining work and is what Emscripten's loader does. **Do not read this spike as "the loader is
  easy".**
- **No wasm-bindgen.** Instantiation is manual. wasm-bindgen does not support side modules, so the
  library modules cannot use it — fine, since generated natives have no JS surface.
- **Node, not a browser.** Same `WebAssembly` API, but untested in Safari, which is where the
  existing stack-depth trouble lives.
- **Desktop is untested.** A `.dylib` shares the process address space so pointers work for the same
  reason, but the allocator and layout caveats apply identically and none of it is verified here.

## Why there is no stable ABI in sight

chotchki's scope call: *"we are only supporting a single build... these are ones we built under the
exact same build chain"*, and *"I want to avoid bringing in stabby"*. Both sides come out of one
`cargo build` of one workspace, so `repr(Rust)` is consistent by construction and `Value`/`FnCtx`
cross unchanged. A stable-ABI crate solves for independently-compiled Rust, which a closed world
does not have.

The residual risk is not versioning, it is STALENESS — a side module that is a build behind, or a
CDN-cached one, fails as silent memory corruption. That needs a build fingerprint embedded in both
and checked at instantiation (SZ.4.3), which is a tripwire rather than a version-negotiation scheme.
