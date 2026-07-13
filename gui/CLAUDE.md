# fab-gui conventions

## UI glyphs: no raw non-ASCII, or you get tofu

The egui font stack is egui's defaults (Ubuntu-Light / Hack + emoji fallbacks) plus the Material
Symbols SUBSET (`assets/fonts/MaterialSymbols-subset.ttf`), installed lowest-priority as a fallback
(`install_fonts`). That subset carries ONLY the PUA icon codepoints in the `build.rs` manifest — not
general Unicode. So a raw symbol/dingbat glyph in a rendered string (`●` U+25CF, `→` U+2192, `✓`
U+2713, most of the U+25xx geometric-shapes block) resolves in NO loaded font and renders as **tofu**
(□). This bit us on the stale-tab dot + the Publish button (both shipped tofu).

Rule for any string egui renders (labels, buttons, RichText, status): never hardcode a non-ASCII
symbol. Two sanctioned paths:

1. **A Material Symbols glyph via `icons::*`** (panel.rs `mod icons`, generated from the manifest).
   To add one: append `("NAME", 0xCODEPOINT)` to `MANIFEST` in `gui/build.rs` — codepoint from the
   pinned Material Symbols `.codepoints` file (the same repo/commit as `SOURCE_URL`) — then rebuild.
   The build regenerates the subset (download → instance at FILL=0 → subset) AND the `icons::NAME`
   const, and commit the regenerated `assets/fonts/MaterialSymbols-subset.ttf` + `.subset-stamp`.
   The subset is cut at FILL=0, so pick an inherently-FILLED glyph (`fiber_manual_record`, `lens`)
   over an outline one (`circle` reads as a hollow ring at FILL=0). `icons` is panel.rs-private —
   strings built in other modules (jobs.rs / print.rs status) can't reach it; use path 2 there.
2. **ASCII where there's a clean form** — `->` not `→`. (The codebase already uses `-> bolt`.)

Known-SAFE non-ASCII, covered by egui's defaults — fine to keep using: `·` (U+00B7), `…` (U+2026),
`—` (U+2014), `×` (U+00D7), `°` (U+00B0).

Audit before shipping a glyph:
`grep -rnoP '"[^"]*"' gui/src/*.rs | grep -P '[^\x00-\x7F]'` — anything past the known-safe set (and
comments) is a tofu risk. Verify rendered glyphs on a real frame, not just a compile (see the
`gui-real-window-verify` memory: offscreen `--screenshot`/`--script` renders egui reliably; the
windowed `--shot` PNG goes black from a CLI-spawned window).
