# goldens/ — the M.7 freeze (correctness memory for the C++ cut)

Everything the differential oracle proved gets RECORDED here while the C++ is still linked, so the
`manifold3d`/`oracle` deletion (M.7) removes a dependency, not evidence.

- `oracle_goldens.json` — per corpus case × op: the C++ reference metrics (volume/area/genus/bbox,
  values bit-recorded as f64 bits) + the FINGERPRINT of our own output (`golden::mesh`, FNV-1a over
  canonical bytes — the byte-exact snapshot at 8 bytes/case). The golden-mode lane asserts our
  current output against the frozen C++ metrics at the SAME tolerances the live differential used
  (volume 1e-9 rel, genus exact where it was gated, bbox 1e-9) and fingerprint-equality for byte
  stability. `area` is recorded but NOT asserted — it was never a live gate (cleanliness-sensitive;
  see the M.1.6 methodology note in PLAN.md).
- `models/*.obj` — the frozen nasty-corpus inputs. Provenance: test assets from
  https://github.com/elalish/manifold (Apache-2.0); vendored because they previously came from the
  C++ build directory, which dies with the dependency.
- `inputs/*.bin` — C++-GENERATED inputs (spheres/cylinder) frozen as little-endian MeshGL dumps
  (`FMGL` magic; see tests/m7_golden_mode.rs) — same reason.

Regenerate — the C++ was CUT at M.7.4, so the freeze test is GONE from the tree. To re-freeze
against a future reference, resurrect `freeze_oracle_goldens` (tests/m7_golden_mode.rs) and the
`oracle` feature from git history (pre-M.7.4), then:

    cargo test --release --features oracle --test m7_golden_mode freeze -- --ignored --nocapture

Regeneration is byte-idempotent when nothing changed. A fingerprint mismatch after a code change
means a DELIBERATE output change (regen + review the diff) or a regression (fix the code).
