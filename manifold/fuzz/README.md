# fab-manifold fuzz lane (M.1.5 / gate K.5)

Structure-aware continuous fuzzing under ASan — nightly-only, kept OUT of the root workspace.
The proptest fast-gates in the crate are the CI layer; this lane is the long-soak robustness proof.

    cargo +nightly fuzz run csg_tree -- -max_total_time=86400   # the 24h K.5 run
    cargo +nightly fuzz run polygon  -- -max_total_time=86400

`csg_tree`: up to 100 CONTINUOUSLY-transformed unit cubes fold-unioned with
`KernelParams.intermediate_checks = true` (strictly-manifold after every op). Continuous transforms
on purpose — the GATE-B note: a grid generator floods the gate with exactly-coplanar cases.

`polygon`: valid-by-construction star polygons through the Delaunay-cost ear clip; a simple n-gon
must produce exactly n−2 in-range triangles.

Corpus + artifacts are gitignored; a found trophy repro lands in `artifacts/` — commit the MINIMIZED
repro as a regression test in the crate, never the corpus.
