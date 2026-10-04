# Design

## Context

`src/threemf_in.rs` reads a package twice. `threemf::read` serde-deserializes every `*.model` into an unnamed `Vec<Model>`, and `read_materials` re-reads the same entries with a quick-xml event loop on LOCAL element names, because threemf's serde renames bind the literal `m:` prefix and can't even read its own output back. `collect()` then walks each model's build items, looking objects up in that model only, and `compose()`/`apply()` already implement parent∘child in the 3MF row-vector convention. The three defects are in proposal.md (Why / What Changes). Callers see only `parse_3mf(&[u8]) -> Result<Vec<Object3mf>>`, which is the seam this change keeps.

## Goals / Non-Goals

**Goals:**
- One parse of the package, by our own reader, that resolves objects across model files.
- Identical output to today's reader on every package today's reader could read. Any difference there is a bug in the new reader, not a feature.

**Non-Goals:**
- Bambu-private data: `Metadata/model_settings.config` (its `assemble_item` transforms, per-part extruder, object names). We and the oracle both place by the core build transform.
- Other 3MF extensions (slice, beam lattice, boolean, displacement) and per-triangle properties. These are ignored as today.
- Unit scaling. The oracle ignores `unit` too (spec: "The model unit does not scale geometry").
- Writing Production-extension files; `Solid::write_3mf` is untouched.
- Matching the oracle's component placement bug-for-bug, or a live-oracle 3MF lane in CI (CI has no OpenSCAD binary; the divergence lives in the spec).

## Decisions

**1. Our own quick-xml reader for the read path; threemf stays for writing.**
Alternatives:
- (a) Enable quick-xml's `overlapped-lists` feature (Cargo unifies it into threemf's quick-xml) and side-parse `p:path`. It's one line for defect 1, but defect 2 then needs threemf's `Vec<Model>` mapped back to part names, and `read()` gives no names. We'd be index-aligning to its undocumented zip-iteration order, and the `m:` prefix fragility stays.
- (b) Fork or patch threemf to add the path field and the list handling. That's a fork to maintain, and a serde struct per element is the wrong tool for a format that interleaves elements freely.
- (c) lib3mf over FFI. It's C++, which breaks the pure-Rust deliverable and the wasm worker.

The event loop `read_materials` already runs proves the pattern on this exact input. Folding materials into the same pass makes it one parse instead of two.

**2. Package model, keyed by part name.** A `Package` maps normalized part name → `Part { objects: id → ObjectDef, build: Vec<Item>, materials: id → Vec<displaycolor> }`. An `ObjectDef` is an optional mesh, components `(path: Option<part>, objectid, transform)`, `pid` and `pindex`. Part names are normalized the OPC way: strip the leading `/` and compare ASCII-case-insensitively (OPC part names are case-insensitive). Percent-encoded names are not decoded, since no known producer emits them (see Risks). Object ids and material ids are per-part namespaces, so a color resolves in the part that declares the object.

**3. Root part from the package relationships.** Read `_rels/.rels`, take the relationship whose `Type` is the 3MF start-part type (`http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel`), and use its `Target`. Fall back to `3D/3dmodel.model`, which keeps today's hand-built test zips (no `.rels`) working. No root means an error.

**4. Parse parts lazily, memoized.** Parse the root first, then each part the first time a component names it. A `.model` nothing references is never parsed, so a broken unreferenced entry can't fail an import that never needed it. A Bambu package with N objects still parses each sub-model once.

**5. Resolution keeps `collect()`'s shape.** `collect(part, oid, xf, depth)` looks up `(part, oid)`. A component's `path` defaults to the containing part, and `compose(xf, component.transform)` keeps parent∘child. Only the root part's build items seed it. The depth guard (8) stays as the cycle stop, and its message names the cycle suspicion. Triangle indices are bounds-checked against the mesh's vertex count when its `</mesh>` closes, and an out-of-range index is an error naming it (today it flowed through to `Solid::from_indexed`, which only the GUI path calls). Every error names its part and object id.

**6. Streaming parse.** Each part is read through `quick_xml::Reader::from_reader(BufReader<ZipFile>)` with `read_event_into` over one reused buffer, not `read_to_string`. A 100 MB sub-model then costs its parsed mesh, not the mesh plus the whole XML text. Coordinates and transforms parse as f64; a malformed number or a transform without exactly 12 values is an error naming the element.

**7. `zip` declares `deflate` itself.** Today fab-scad's `zip` is `default-features = false` ("Stored only", for `.scadproj`), and DEFLATE reaches it only by feature unification through threemf's dependency. The reader needs it in its own right. `miniz_oxide` is already in both the native and the wasm graphs, so the build cost is nil.

**8. The divergence is recorded where it's enforced.** The spec carries the oracle's numbers. A comment at the compose site cites upstream `import_3mf_v2.cc:135` (the (0,0,1) default) and `:145` (`cm * m`). The A–D cube fixtures assert the spec values with the oracle's values in their comments, so the test that would fail if someone "fixed" toward the oracle explains why not to.

## Risks / Trade-offs

- [New code on every 3MF load path: desktop, web worker and CLI] → Keep the three existing tests unchanged. Add synthetic fixtures for each spec scenario. Before deleting the old read path, run old and new readers side by side on every `.3mf` under `models/` that the old one can read and require identical `Object3mf` output (vertices, triangles, colors). That's a one-shot differential recorded in the task, not a committed test, since `models/` is a separate repo.
- [OPC naming edge cases: percent-encoding, `./` segments] → Normalize case and the leading slash only. An unresolvable name fails loudly with the name in the message (spec), so a producer we don't handle shows up as a clear warning, not wrong geometry.
- [The oracle divergence surprises a future `fab render --check` or corpus sweep] → The spec explains it, with numbers. Revisit if upstream fixes its default: our behavior then becomes the agreement.
- [Bambu's `assemble_item` transform differs from the build transform] → Non-goal. The core build transform is what both the 3MF spec and the oracle use.
- [The depth guard of 8 rejects legitimate deep nesting] → No known producer nests past 2. The error names the depth, so raising it is a one-line change if one ever does.

## Migration Plan

Drop-in: `parse_3mf`'s signature and `Object3mf` don't change, so no caller moves. Rollback is a revert. Nothing persisted or written changes shape.
