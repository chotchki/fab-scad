---
name: bevy-change-detection-merge
description: Bevy gotcha — two chained systems can react to one resource change on different frames; merge derive+consume
metadata:
  type: reference
---

In fab-gui (Bevy 0.19), two systems chained `derive -> consume` on the same resource change reacted on DIFFERENT frames: the consumer read a frame-stale value (the print-orientation status reported the wrong onion-downgrade count). `.chain()` orders them within a frame but does NOT guarantee both detect the same `is_changed()` on the same frame when each also runs every frame and self-gates.

**How to apply:** when one system DERIVES data from a resource change and another must CONSUME that derived data for the SAME change, merge them into one system (so derive + consume share a run) rather than splitting + chaining. See `gui/src/main.rs::sync_orientation` (it does feasibility + layout together; the split version had `compute_feas` + `relayout_pieces`). Per-frame work that doesn't need same-frame freshness (e.g. `color_conn_markers` reading `Feas`) is fine left separate.

Related: [[builds-workflow-shells]].
