//! `CrossSection` — the 2D polygon subsystem (Manifold's `CrossSection`, R5/M.5).
//!
//! Manifold's 2D IS Clipper2; per SPEC [OPEN #4] (chotchki-confirmed, M.5.0 spike) we adopt `i_overlay`
//! — pure-Rust, integer-coords ⇒ deterministic + wasm-clean — and validate by AREA-residual against
//! Clipper2-via-Manifold, NOT bit-identity. This is the ONE layer where the verbatim/byte-exact thesis
//! relaxes; the 3D core stays byte-exact. The exception that proves the rule is `offset` (M.5.4.1):
//! join-corner geometry is engine-DEFINED, not engine-independent, so the K.6 area gate forced a
//! verbatim port of Clipper2's offset walk (`offset.rs`) — i_overlay supplies only the finishing
//! boolean there, like everywhere else.
//!
//! A `CrossSection` is a set of polygon contours under the POSITIVE fill rule (Manifold's `from_polygons`
//! default): a CCW contour adds +1 winding (fills), a CW contour −1 (a hole). i_overlay handles the
//! f64↔integer-grid round-trip internally, so the determinism seam lives inside the dep, not here.
//!
//! NO-PANIC / TYPED-ERROR policy (M.5.4.5, chotchki): non-finite coordinates are undefined behavior
//! in the C++ (`static_cast` of NaN to int64 inside Clipper2) and would trip i_overlay's debug
//! assertions here — so they are REJECTED at every ingesting boundary instead. Each constructor or
//! operation where a coordinate can enter or go bad (`from_polygons*`, `square`, `circle`,
//! `from_rect`, the transform family, `warp`, `offset`, `hull_of_points`) returns
//! `Result<_, Error>` with [`Error::NonFiniteVertex`], upholding the type invariant that a
//! `CrossSection`'s contours are ALWAYS finite — which is why the closed operations over valid
//! inputs (booleans, hull, decompose, queries) stay infallible. Same decision shape as the 3D
//! side's M.3.2 eager-`Result` surface.

use crate::boolean::predicates::ccw;
use crate::linalg::{Mat2x3, Rect, Vec2};
use crate::mathf;
use crate::status::Error;
use i_overlay::core::fill_rule::FillRule as IoFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;

/// Corner handling for [`CrossSection::offset`] (Manifold/Clipper2 `JoinType`). All four are the
/// verbatim Clipper2 corner geometry via the `offset.rs` port. OpenSCAD mapping: `offset(r=…)` →
/// `Round`; `offset(delta=…)` → `Miter`; `offset(delta=…, chamfer=true)` → `Square` (NOT `Bevel` —
/// the 78.2548 canary pins this).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinType {
    /// Flat cap perpendicular to the corner bisector, tangent to the delta circle (Clipper2
    /// `jtSquare`).
    Square,
    /// Rounded corners (arc, resolution = `circular_segments`).
    Round,
    /// Sharp mitered corners, squared off beyond `miter_limit` (a distance ratio, Clipper2
    /// semantics — minimum/default 2).
    Miter,
    /// Corner cut by the straight segment between the two edge offsets (Clipper2 `jtBevel`).
    Bevel,
}

/// Filling rule for interpreting self-intersecting / overlapping input contours
/// (`Clipper2Lib::FillRule` via Manifold `CrossSection::FillRule`). Everything downstream of a
/// constructor is Positive-normalized regardless of which rule ingested it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// Inside iff a ray crosses an odd number of edges.
    EvenOdd,
    /// Inside iff the winding count is non-zero.
    NonZero,
    /// Inside iff the winding count is positive — the Manifold default.
    Positive,
    /// Inside iff the winding count is negative.
    Negative,
}

impl FillRule {
    fn to_io(self) -> IoFillRule {
        match self {
            FillRule::EvenOdd => IoFillRule::EvenOdd,
            FillRule::NonZero => IoFillRule::NonZero,
            FillRule::Positive => IoFillRule::Positive,
            FillRule::Negative => IoFillRule::Negative,
        }
    }
}

// One OpType for 2D and 3D, like the C++ (`manifold.h` OpType serves both surfaces).
pub use crate::boolean::OpType;

/// `Quality::GetCircularSegments(radius)` at Manifold's DEFAULT quality statics (segments unset,
/// min angle 10°, min edge length 1) — the circle/round-offset facet count when the caller passes
/// `circular_segments <= 2`. The C++ globals are settable; we ship the defaults only (fab-scad owns
/// `$fn` upstream, so the mutable-global surface has no consumer here).
pub fn get_circular_segments(radius: f64) -> i32 {
    let n_seg_a = 36; // 360.0 / DEFAULT_ANGLE(10°), as the C++ int truncation
    #[allow(clippy::cast_possible_truncation)]
    let n_seg_l = (2.0 * radius.abs() * core::f64::consts::PI / 1.0) as i32;
    let n_seg = n_seg_a.min(n_seg_l) + 3;
    let n_seg = n_seg - n_seg % 4;
    n_seg.max(4)
}

/// A 2D region as a set of polygon contours (Manifold `CrossSection`). Normalized under Positive fill:
/// CCW outers fill, CW contours subtract (holes) — the flat `Polygons` form, holes distinguished by
/// winding. Empty `contours` = no area.
///
/// INVARIANT: every contour coordinate is FINITE — enforced by the `Result`-returning ingestion
/// boundary (see the module doc), which is what lets the closed operations stay infallible. The
/// field is `pub(crate)` so external code can't plant a NaN past the checks; read via
/// [`Self::contours`].
///
/// TRANSFORM SEMANTICS deviation (documented): the C++ accumulates transforms lazily and applies the
/// composed matrix once on access; we apply eagerly per call. Same math, different rounding points —
/// chained-vs-composed agree to ~1 ULP per step, within the 2D layer's area-residual thesis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossSection {
    /// The polygon contours (outers CCW, holes CW). Finite by invariant.
    pub(crate) contours: Vec<Vec<Vec2>>,
}

/// Every coordinate finite? The ingestion gate behind the type invariant.
fn ensure_finite(polygons: &[Vec<Vec2>]) -> Result<(), Error> {
    if polygons.iter().flatten().all(|p| p.is_finite()) {
        Ok(())
    } else {
        Err(Error::NonFiniteVertex)
    }
}

impl CrossSection {
    /// The empty cross-section (no area).
    pub fn new() -> Self {
        Self::default()
    }

    /// The polygon contours (outers CCW, holes CW) — read-only; construction goes through the
    /// validating boundary.
    pub fn contours(&self) -> &[Vec<Vec2>] {
        &self.contours
    }

    /// Build from raw polygon contours, normalizing under Positive fill (Manifold `from_polygons`): a
    /// `Subject`-rule self-overlay resolves self-intersections + canonicalizes the winding so booleans
    /// and area are well-defined. CCW is the outer-contour convention. Non-finite coordinates →
    /// [`Error::NonFiniteVertex`] (C++ UB, rejected here).
    pub fn from_polygons(polygons: &[Vec<Vec2>]) -> Result<Self, Error> {
        Self::from_polygons_with(polygons, FillRule::Positive)
    }

    /// [`Self::from_polygons`] under an explicit [`FillRule`] — how self-intersecting sub-regions
    /// are interpreted (the C++ constructor's `fillrule` parameter).
    pub fn from_polygons_with(polygons: &[Vec<Vec2>], fill_rule: FillRule) -> Result<Self, Error> {
        ensure_finite(polygons)?;
        if polygons.iter().all(|c| c.is_empty()) {
            return Ok(Self::new());
        }
        Ok(Self {
            contours: normalize(polygons, fill_rule.to_io()),
        })
    }

    /// A `size.x × size.y` axis-aligned square, first-quadrant cornered on the origin, or centered
    /// when `center` (Manifold `Square`). Any negative dimension — or all-zero — is empty (the
    /// C++-documented contract); a non-finite dimension is an error (C++ UB, rejected). Built
    /// directly (the C++ private non-unioning constructor path), so the vertex order is exact.
    pub fn square(size: Vec2, center: bool) -> Result<Self, Error> {
        if !size.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        if size.x < 0.0 || size.y < 0.0 || size.length() == 0.0 {
            return Ok(Self::new());
        }
        let contour = if center {
            let w = size.x / 2.0;
            let h = size.y / 2.0;
            vec![
                Vec2::new(w, h),
                Vec2::new(-w, h),
                Vec2::new(-w, -h),
                Vec2::new(w, -h),
            ]
        } else {
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(size.x, 0.0),
                Vec2::new(size.x, size.y),
                Vec2::new(0.0, size.y),
            ]
        };
        Ok(Self {
            contours: vec![contour],
        })
    }

    /// A circle of `radius` with `circular_segments` facets (≤ 2 → the [`get_circular_segments`]
    /// quality default), vertex `i` at `dPhi·i` degrees via the exact-quadrant `sind`/`cosd`
    /// (Manifold `Circle` verbatim — same trig, so vertices match the C++ bit-for-bit).
    /// Non-positive radius is empty (the C++ contract); non-finite radius is an error.
    pub fn circle(radius: f64, circular_segments: i32) -> Result<Self, Error> {
        if !radius.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        if radius <= 0.0 {
            return Ok(Self::new());
        }
        let n = if circular_segments > 2 {
            circular_segments
        } else {
            get_circular_segments(radius)
        };
        let d_phi = 360.0 / f64::from(n);
        let contour = (0..n)
            .map(|i| {
                Vec2::new(
                    radius * mathf::cosd(d_phi * f64::from(i)),
                    radius * mathf::sind(d_phi * f64::from(i)),
                )
            })
            .collect();
        Ok(Self {
            contours: vec![contour],
        })
    }

    /// The axis-aligned rectangle as a cross-section (the C++ `CrossSection(Rect)` constructor).
    /// A non-finite rect — including the inverted-infinity default — is an error.
    pub fn from_rect(rect: Rect) -> Result<Self, Error> {
        if !rect.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        Ok(Self {
            contours: vec![vec![
                rect.min,
                Vec2::new(rect.max.x, rect.min.y),
                rect.max,
                Vec2::new(rect.min.x, rect.max.y),
            ]],
        })
    }

    fn boolean(&self, other: &Self, rule: OverlayRule) -> Self {
        let a = to_io(&self.contours);
        let b = to_io(&other.contours);
        Self {
            contours: from_io(a.overlay(&b, rule, IoFillRule::Positive)),
        }
    }

    /// `self ∪ other` (Manifold `+` / `Boolean(Add)`).
    pub fn union(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Union)
    }

    /// `self − other` (Manifold `-` / `Boolean(Subtract)`).
    pub fn difference(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Difference)
    }

    /// `self ∩ other` (Manifold `^` / `Boolean(Intersect)`).
    pub fn intersection(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Intersect)
    }

    /// N-ary boolean (Manifold `BatchBoolean`): `Intersect` folds pairwise; `Add`/`Subtract` run
    /// ONE op with the head as subject and every tail section's contours pooled as the clip —
    /// exactly the C++ shape (and why `Subtract` means "head minus all the rest").
    pub fn batch_boolean(sections: &[CrossSection], op: OpType) -> CrossSection {
        match (sections.len(), op) {
            (0, _) => Self::new(),
            (1, _) => sections[0].clone(),
            (_, OpType::Intersect) => sections[1..]
                .iter()
                .fold(sections[0].clone(), |acc, s| acc.intersection(s)),
            (_, _) => {
                let clips: Vec<Vec<Vec2>> = sections[1..]
                    .iter()
                    .flat_map(|s| s.contours.iter().cloned())
                    .collect();
                let rule = match op {
                    OpType::Add => OverlayRule::Union,
                    OpType::Subtract => OverlayRule::Difference,
                    OpType::Intersect => unreachable!("folded above"),
                };
                CrossSection {
                    contours: from_io(to_io(&sections[0].contours).overlay(
                        &to_io(&clips),
                        rule,
                        IoFillRule::Positive,
                    )),
                }
            }
        }
    }

    /// Batch union (Manifold `Compose` — literally `BatchBoolean(Add)`).
    pub fn compose(sections: &[CrossSection]) -> CrossSection {
        Self::batch_boolean(sections, OpType::Add)
    }

    /// Split into topologically disconnected components, each one outline plus its holes (Manifold
    /// `Decompose`). i_overlay's grouped `Shapes` output IS that grouping (shape = outer + holes),
    /// so this is one Subject-rule self-overlay keeping the groups `from_io` normally flattens.
    /// ORDER deviation (documented): components arrive in i_overlay's deterministic sweep order,
    /// not the C++'s reversed-PolyTree order.
    pub fn decompose(&self) -> Vec<CrossSection> {
        if self.num_contour() < 2 {
            return vec![self.clone()];
        }
        let shapes = to_io(&self.contours).overlay(
            &empty_clip(),
            OverlayRule::Subject,
            IoFillRule::Positive,
        );
        shapes
            .into_iter()
            .map(|shape| CrossSection {
                contours: shape
                    .into_iter()
                    .map(|c| c.into_iter().map(|p| Vec2::new(p[0], p[1])).collect())
                    .collect(),
            })
            .collect()
    }

    /// Apply a 2D affine (Manifold `Transform`). Eager (see the type-level deviation note); mirrors
    /// (negative linear determinant) reverse each contour so the Positive-fill winding convention
    /// survives — the C++ `::transform` helper's `invert` indexing. No re-union (C++ doesn't
    /// either: an affine can't introduce self-intersections). A non-finite matrix — or an overflow
    /// to ∞ — surfaces as [`Error::NonFiniteVertex`], checked on the OUTPUT like `Mesh::transform`.
    pub fn transform(&self, m: Mat2x3) -> Result<Self, Error> {
        let invert = m.det_linear() < 0.0;
        let contours: Vec<Vec<Vec2>> = self
            .contours
            .iter()
            .map(|c| {
                let mut out: Vec<Vec2> = c.iter().map(|&p| m.transform_point(p)).collect();
                if invert {
                    out.reverse();
                }
                out
            })
            .collect();
        ensure_finite(&contours)?;
        Ok(Self { contours })
    }

    /// Translate every vertex by `v` (Manifold `Translate`).
    pub fn translate(&self, v: Vec2) -> Result<Self, Error> {
        self.transform(Mat2x3::translate(v))
    }

    /// Rotate about the origin by `degrees` (Manifold `Rotate`).
    pub fn rotate(&self, degrees: f64) -> Result<Self, Error> {
        self.transform(crate::linalg::rotate2_degrees(degrees))
    }

    /// Componentwise scale (Manifold `Scale`).
    pub fn scale(&self, v: Vec2) -> Result<Self, Error> {
        self.transform(Mat2x3::scale(v))
    }

    /// Mirror over the line through the origin whose NORMAL is `ax` (Manifold `Mirror`:
    /// `I − 2·n·nᵀ`). A zero axis returns empty, per the C++.
    pub fn mirror(&self, ax: Vec2) -> Result<Self, Error> {
        if !ax.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        if ax.length() == 0.0 {
            return Ok(Self::new());
        }
        let n = ax.normalize();
        let m = Mat2x3 {
            x: Vec2::new(1.0 - 2.0 * n.x * n.x, -2.0 * n.x * n.y),
            y: Vec2::new(-2.0 * n.x * n.y, 1.0 - 2.0 * n.y * n.y),
            w: Vec2::ZERO,
        };
        self.transform(m)
    }

    /// Move every vertex through `warp_func`, then re-normalize under Positive fill so any
    /// introduced self-intersections resolve (Manifold `Warp`/`WarpBatch`). A warp that produces a
    /// non-finite coordinate is an error, per the ingestion boundary.
    pub fn warp(&self, mut warp_func: impl FnMut(&mut Vec2)) -> Result<Self, Error> {
        let warped: Vec<Vec<Vec2>> = self
            .contours
            .iter()
            .map(|c| {
                c.iter()
                    .map(|&p| {
                        let mut q = p;
                        warp_func(&mut q);
                        q
                    })
                    .collect()
            })
            .collect();
        Self::from_polygons(&warped)
    }

    /// Grow (`delta > 0`) or shrink (`< 0`) the region by `delta`, with the given corner handling
    /// (Manifold `Offset` → Clipper2 `InflatePaths`, ported verbatim in `offset.rs` — all four join
    /// types produce Clipper2's exact corner geometry, negative deltas ride the concave-corner
    /// negative-region rule). `circular_segments` is the Round join's segments-per-360° (≤ 2 → the
    /// quality default); `miter_limit` is Clipper2's distance-ratio limit (min/default 2).
    /// Errors: non-finite `delta`/`miter_limit`, or a walk output that went non-finite (possible
    /// only for sub-1e-150-scale geometry — the raw-f64 `unit_normal` underflow documented in
    /// `offset.rs`; the check turns what would be a dep panic into [`Error::NonFiniteVertex`]).
    pub fn offset(
        &self,
        delta: f64,
        join_type: JoinType,
        miter_limit: f64,
        circular_segments: i32,
    ) -> Result<Self, Error> {
        if !delta.is_finite() || !miter_limit.is_finite() {
            return Err(Error::NonFiniteVertex);
        }
        if self.is_empty() {
            return Ok(Self::new());
        }
        // The Round segment count goes to the walk DIRECTLY — Manifold's arc-tolerance encoding of
        // n is a lossy acos∘cos round-trip; see `offset_polygons`' deviation note.
        let steps_per_360 = if join_type == JoinType::Round {
            let n = if circular_segments > 2 {
                circular_segments
            } else {
                get_circular_segments(delta)
            };
            f64::from(n)
        } else {
            0.0
        };
        let (raw, reversed) = crate::offset::offset_polygons(
            &self.contours,
            delta,
            join_type,
            miter_limit,
            steps_per_360,
        );
        // The invariant gate before the finishing union — a non-finite walk output (the subnormal
        // unit_normal underflow) becomes an Err here instead of an i_overlay debug panic.
        ensure_finite(&raw)?;
        // The finishing self-union removes the concave negative regions (Clipper2 ExecuteInternal's
        // closing Union). Reversed (CW-wound) input — reachable via `from_rect` with swapped
        // corners, since neither engine normalizes there — closes under Negative fill AND flips the
        // output back to CW (`c.ReverseSolution(paths_reversed)`: "the solution should retain the
        // orientation of the input"; i_overlay always emits CCW outers, so the reversal is ours).
        let rule = if reversed {
            IoFillRule::Negative
        } else {
            IoFillRule::Positive
        };
        let mut contours = normalize(&raw, rule);
        if reversed {
            for c in &mut contours {
                c.reverse();
            }
        }
        Ok(Self { contours })
    }

    /// Net signed area — outer contours positive, holes negative (Manifold `Area`).
    pub fn area(&self) -> f64 {
        self.contours.iter().map(|c| signed_area(c)).sum()
    }

    /// No contours?
    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }

    /// Number of contours (outers + holes).
    pub fn num_contour(&self) -> usize {
        self.contours.len()
    }

    /// Total vertex count across all contours.
    pub fn num_vert(&self) -> usize {
        self.contours.iter().map(|c| c.len()).sum()
    }

    /// Axis-aligned bounds (Manifold `Bounds`). EMPTY-INPUT QUIRK ported faithfully (M.5.4
    /// verification, confirmed against the compiled C++): Clipper2's `GetBounds` hands back the
    /// `(MAX, MAX)→(LOWEST, LOWEST)` sentinel for a vertex-less path set, and the C++ feeds it
    /// through `Rect(a, b)` — whose componentwise min/max SORTS the corners — so an empty
    /// cross-section yields the ALL-ENCOMPASSING rect (`is_empty() == false`, contains every
    /// point), NOT the inverted-infinity default `Rect`.
    pub fn bounds(&self) -> Rect {
        if self.num_vert() == 0 {
            return Rect::from_points(Vec2::splat(f64::MAX), Vec2::splat(f64::MIN));
        }
        let mut r = Rect::default();
        for &p in self.contours.iter().flatten() {
            r.union_point(p);
        }
        r
    }

    /// Convex hull of this cross-section's vertices (Manifold `Hull()`). Infallible: the input
    /// already holds the finiteness invariant.
    pub fn hull(&self) -> Self {
        let pts: Vec<Vec2> = self.contours.iter().flatten().copied().collect();
        Self::hull_of_valid_points(pts)
    }

    /// Convex hull enveloping a set of cross-sections (Manifold `Hull(vector<CrossSection>)`) —
    /// pools every vertex of every section. Infallible, like [`Self::hull`].
    pub fn hull_of(sections: &[CrossSection]) -> Self {
        let pts: Vec<Vec2> = sections
            .iter()
            .flat_map(|s| s.contours.iter().flatten().copied())
            .collect();
        Self::hull_of_valid_points(pts)
    }

    /// Convex hull of a RAW point set (Manifold `Hull(SimplePolygon)`) — the ingesting variant, so
    /// non-finite points are rejected.
    pub fn hull_of_points(pts: &[Vec2]) -> Result<Self, Error> {
        if !pts.iter().all(|p| p.is_finite()) {
            return Err(Error::NonFiniteVertex);
        }
        Ok(Self::hull_of_valid_points(pts.to_vec()))
    }

    /// The C++'s Andrew monotone-chain `HullImpl` — lexicographic sort, lower + upper chains,
    /// strict left turns only (collinear points are dropped, `CCW ≤ 0` backtracks). Under 3 points
    /// is empty. Callers guarantee finite points.
    fn hull_of_valid_points(pts: Vec<Vec2>) -> Self {
        if pts.len() < 3 {
            return Self::new();
        }
        let mut pts = pts;
        // V2Lesser (x, then y). total_cmp — inputs are finite geometry; total order keeps the sort
        // deterministic where the C++ comparator leaves ±0 ties to the sort implementation.
        pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));

        fn backtrack(stack: &mut Vec<Vec2>, pt: Vec2) {
            while stack.len() >= 2
                && ccw(stack[stack.len() - 2], stack[stack.len() - 1], pt, 0.0) <= 0
            {
                stack.pop();
            }
        }
        let mut lower: Vec<Vec2> = Vec::new();
        for &pt in &pts {
            backtrack(&mut lower, pt);
            lower.push(pt);
        }
        let mut upper: Vec<Vec2> = Vec::new();
        for &pt in pts.iter().rev() {
            backtrack(&mut upper, pt);
            upper.push(pt);
        }
        lower.pop();
        upper.pop();
        lower.append(&mut upper);
        if lower.is_empty() {
            return Self::new();
        }
        Self {
            contours: vec![lower],
        }
    }

    /// The contours as raw `[f64; 2]` polygons (Manifold `ToPolygons`) — the interchange the 2D↔3D
    /// bridges and the area-residual oracle consume.
    pub fn to_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        self.contours
            .iter()
            .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
            .collect()
    }
}

/// Normalize raw contours under the given i_overlay fill rule: a `Subject`-rule self-overlay (the
/// C++ `C2::Union(ps, fillrule)`) resolving self-intersections + canonicalizing winding.
fn normalize(polygons: &[Vec<Vec2>], rule: IoFillRule) -> Vec<Vec<Vec2>> {
    let subj = to_io(polygons);
    snap_to_inputs(
        from_io(subj.overlay(&empty_clip(), OverlayRule::Subject, rule)),
        polygons,
    )
}

/// Restore EXACT input coordinates after the i_overlay round-trip (M.7.3.1). The f64→int-grid→f64
/// normalization can shift non-dyadic contours by ~1e-9 — harmless by the 2D layer's area-residual
/// thesis, but TOPOLOGICALLY live: a revolve profile's `x = 0` axis verts came back at 7.5e-10, so
/// the revolved cutter grew a hair-thin axial tunnel and the subtract left a degenerate filament
/// component (the drill_guide genus divergence; C++ given the same damaged buffer agrees with us —
/// OpenSCAD differs only because Clipper2's DECIMAL grid kept the profile exact). Every output vert
/// within the grid-noise envelope of an input vert snaps back to the input's exact bits; genuinely
/// new verts are untouched. Ingest-only — boolean/offset outputs keep the relaxed thesis.
fn snap_to_inputs(mut out: Vec<Vec<Vec2>>, inputs: &[Vec<Vec2>]) -> Vec<Vec<Vec2>> {
    let mut scale = 0.0f64;
    for c in inputs {
        for p in c {
            scale = scale.max(p.x.abs()).max(p.y.abs());
        }
    }
    // Observed noise is ~1e-10 relative to the coordinate scale; 1e-8 relative gives 100× headroom
    // while staying far below any real 2D feature.
    let eps = scale * 1e-8;
    if eps == 0.0 {
        return out;
    }
    // Coarse hash grid over the input verts, cell = eps; a within-eps match is in the 3×3 block.
    use std::collections::HashMap;
    let cell = |v: f64| -> i64 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "finite coords / eps of their own scale — |result| is bounded by 1e8"
        )]
        {
            (v / eps).floor() as i64
        }
    };
    let mut grid: HashMap<(i64, i64), Vec<Vec2>> = HashMap::new();
    for c in inputs {
        for &p in c {
            grid.entry((cell(p.x), cell(p.y))).or_default().push(p);
        }
    }
    let eps2 = eps * eps;
    for c in &mut out {
        for p in c {
            let (cx, cy) = (cell(p.x), cell(p.y));
            let mut best: Option<(f64, Vec2)> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(cands) = grid.get(&(cx + dx, cy + dy)) {
                        for &q in cands {
                            let d = (q - *p).length2();
                            if d <= eps2 && best.is_none_or(|(bd, _)| d < bd) {
                                best = Some((d, q));
                            }
                        }
                    }
                }
            }
            if let Some((_, q)) = best {
                *p = q;
            }
        }
    }
    out
}

/// An empty clip contour set — the second operand for a `Subject`-rule normalization.
fn empty_clip() -> Vec<Vec<[f64; 2]>> {
    Vec::new()
}

fn to_io(contours: &[Vec<Vec2>]) -> Vec<Vec<[f64; 2]>> {
    contours
        .iter()
        .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

/// Flatten i_overlay's grouped `Shapes` (shape → contours) into the flat `Polygons` form.
fn from_io(shapes: Vec<Vec<Vec<[f64; 2]>>>) -> Vec<Vec<Vec2>> {
    shapes
        .into_iter()
        .flatten()
        .map(|c| c.into_iter().map(|p| Vec2::new(p[0], p[1])).collect())
        .collect()
}

/// Signed shoelace area of one contour (CCW ⇒ positive).
fn signed_area(c: &[Vec2]) -> f64 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = c[i];
        let q = c[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    0.5 * a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_at(x: f64, y: f64, s: f64) -> Vec<Vec2> {
        vec![
            Vec2::new(x, y),
            Vec2::new(x + s, y),
            Vec2::new(x + s, y + s),
            Vec2::new(x, y + s),
        ]
    }

    #[test]
    fn booleans_and_area_are_analytic() {
        let a = CrossSection::from_polygons(&[square_at(0.0, 0.0, 2.0)]).unwrap(); // area 4
        let b = CrossSection::from_polygons(&[square_at(1.0, 1.0, 2.0)]).unwrap(); // overlap 1
        assert!((a.area() - 4.0).abs() < 1e-9, "square area {}", a.area());

        assert!((a.union(&b).area() - 7.0).abs() < 1e-9);
        assert!((a.intersection(&b).area() - 1.0).abs() < 1e-9);
        assert!((a.difference(&b).area() - 3.0).abs() < 1e-9);

        // A hole: big square minus a fully-interior small one → outer + hole contour, area 96.
        let big = CrossSection::from_polygons(&[square_at(0.0, 0.0, 10.0)]).unwrap();
        let small = CrossSection::from_polygons(&[square_at(4.0, 4.0, 2.0)]).unwrap();
        let holed = big.difference(&small);
        assert!(
            (holed.area() - 96.0).abs() < 1e-9,
            "holed area {}",
            holed.area()
        );
        assert_eq!(holed.num_contour(), 2, "outer + 1 hole");
    }

    #[test]
    fn empty_and_bounds() {
        let e = CrossSection::new();
        assert!(e.is_empty() && e.area() == 0.0);
        // The ported C++ quirk: empty bounds = the ALL-ENCOMPASSING rect (Clipper2's inverted
        // sentinel sorted by the Rect ctor), so it contains everything and is NOT "empty".
        let b = e.bounds();
        assert!(!b.is_empty() && b.is_finite() && b.contains_point(Vec2::ZERO));
        // Disjoint squares that don't touch → empty intersection.
        let a = CrossSection::from_polygons(&[square_at(0.0, 0.0, 1.0)]).unwrap();
        let b = CrossSection::from_polygons(&[square_at(5.0, 5.0, 1.0)]).unwrap();
        assert!(a.intersection(&b).is_empty());

        let r = a.bounds();
        assert_eq!((r.min, r.max), (Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)));
    }

    #[test]
    fn offset_round_matches_analytic() {
        let (s, r) = (4.0, 1.0);
        let a = CrossSection::from_polygons(&[square_at(0.0, 0.0, s)]).unwrap();
        let grown = a.offset(r, JoinType::Round, 2.0, 256).unwrap();
        // Rounded rectangle: s² + 4·s·r (edge strips) + π·r² (corner quarter-circles).
        let expected = s * s + 4.0 * s * r + core::f64::consts::PI * r * r;
        assert!(
            (grown.area() - expected).abs() / expected < 2e-3,
            "round-offset area {} vs analytic {expected}",
            grown.area()
        );
        // A negative offset shrinks: a 4-square inset by 1 → a 2-square, area 4.
        let inset = a.offset(-1.0, JoinType::Miter, 2.0, 16).unwrap();
        assert!(
            (inset.area() - 4.0).abs() < 1e-6,
            "inset area {}",
            inset.area()
        );
    }

    #[test]
    fn offset_square_join_hits_the_openscad_canary() {
        // OpenSCAD: `offset(delta = 2, chamfer = true) square(5);` → 78.2548 (jtSquare — the
        // fab-scad backend.rs canary). The area that pinned chamfer → Square, not Bevel.
        let sq = CrossSection::square(Vec2::new(5.0, 5.0), false).unwrap();
        let grown = sq.offset(2.0, JoinType::Square, 2.0, 0).unwrap();
        assert!(
            (grown.area() - 78.2548).abs() < 1e-3,
            "square-join canary: area {}",
            grown.area()
        );
        // Sibling canaries: miter keeps the full corner (81), bevel cuts the corner triangles (73).
        let miter = sq.offset(2.0, JoinType::Miter, 2.0, 0).unwrap();
        assert!(
            (miter.area() - 81.0).abs() < 1e-9,
            "miter area {}",
            miter.area()
        );
        let bevel = sq.offset(2.0, JoinType::Bevel, 2.0, 0).unwrap();
        assert!(
            (bevel.area() - 73.0).abs() < 1e-9,
            "bevel area {}",
            bevel.area()
        );
    }

    #[test]
    fn is_deterministic() {
        let a = CrossSection::from_polygons(&[square_at(0.0, 0.0, 3.0)]).unwrap();
        let b = CrossSection::from_polygons(&[square_at(1.3, 0.7, 3.0)]).unwrap();
        assert_eq!(
            a.union(&b),
            a.union(&b),
            "CrossSection union not deterministic"
        );
    }

    #[test]
    fn constructors_are_exact() {
        let sq = CrossSection::square(Vec2::new(4.0, 6.0), false).unwrap();
        assert_eq!(sq.num_vert(), 4);
        assert!((sq.area() - 24.0).abs() < 1e-12);
        let sqc = CrossSection::square(Vec2::new(4.0, 6.0), true).unwrap();
        let b = sqc.bounds();
        assert_eq!((b.min, b.max), (Vec2::new(-2.0, -3.0), Vec2::new(2.0, 3.0)));
        assert!(
            CrossSection::square(Vec2::new(-1.0, 2.0), false)
                .unwrap()
                .is_empty()
        );
        assert!(
            CrossSection::square(Vec2::new(0.0, 0.0), false)
                .unwrap()
                .is_empty()
        );

        let circ = CrossSection::circle(2.0, 64).unwrap();
        assert_eq!(circ.num_vert(), 64);
        // Inscribed regular n-gon area: ½·n·r²·sin(2π/n).
        let expected = 0.5 * 64.0 * 4.0 * mathf::sind(360.0 / 64.0);
        assert!(
            (circ.area() - expected).abs() < 1e-9,
            "circle area {}",
            circ.area()
        );
        assert!(CrossSection::circle(-1.0, 8).unwrap().is_empty());
        // Quality default at r=2: nSegL = ⌊4π⌋ = 12, min(36,12)+3 = 15 → 12 after %4.
        assert_eq!(get_circular_segments(2.0), 12);
        assert_eq!(CrossSection::circle(2.0, 0).unwrap().num_vert(), 12);
    }

    #[test]
    fn transforms_move_scale_mirror_and_preserve_winding() {
        let sq = CrossSection::square(Vec2::new(2.0, 2.0), false).unwrap(); // area 4
        let moved = sq.translate(Vec2::new(10.0, -3.0)).unwrap();
        assert!((moved.area() - 4.0).abs() < 1e-12);
        assert_eq!(moved.bounds().min, Vec2::new(10.0, -3.0));

        let spun = sq.rotate(45.0).unwrap();
        assert!((spun.area() - 4.0).abs() < 1e-9, "rotation preserves area");

        let scaled = sq.scale(Vec2::new(2.0, 3.0)).unwrap();
        assert!((scaled.area() - 24.0).abs() < 1e-9);

        // A mirror flips winding; the contour reversal keeps the region positive.
        let mirrored = sq.mirror(Vec2::new(1.0, 0.0)).unwrap();
        assert!(
            (mirrored.area() - 4.0).abs() < 1e-9,
            "mirror area {}",
            mirrored.area()
        );
        assert_eq!(
            mirrored.bounds().max.x,
            0.0,
            "mirrored across x-normal line"
        );
        assert!(sq.mirror(Vec2::ZERO).unwrap().is_empty());

        // Eager-vs-composed agreement (the documented lazy-transform deviation, bounded).
        let chained = sq
            .rotate(30.0)
            .unwrap()
            .scale(Vec2::new(2.0, 3.0))
            .unwrap()
            .translate(Vec2::new(4.0, 5.0))
            .unwrap();
        let composed = sq
            .transform(
                Mat2x3::translate(Vec2::new(4.0, 5.0)).compose(
                    Mat2x3::scale(Vec2::new(2.0, 3.0)).compose(crate::linalg::rotate2_degrees(30.0)),
                ),
            )
            .unwrap();
        for (c1, c2) in chained.contours.iter().zip(&composed.contours) {
            for (p1, p2) in c1.iter().zip(c2) {
                assert!(
                    (*p1 - *p2).length() < 1e-12,
                    "chained {p1:?} vs composed {p2:?}"
                );
            }
        }
    }

    #[test]
    fn warp_resolves_self_intersections() {
        let sq = CrossSection::square(Vec2::new(10.0, 10.0), false).unwrap();
        let a = sq
            .scale(Vec2::new(2.0, 3.0))
            .unwrap()
            .translate(Vec2::new(4.0, 5.0))
            .unwrap();
        let b = sq
            .warp(|v| {
                v.x = v.x * 2.0 + 4.0;
                v.y = v.y * 3.0 + 5.0;
            })
            .unwrap();
        assert!(
            (a.area() - b.area()).abs() < 1e-9,
            "warp == affine for an affine warp"
        );
    }

    #[test]
    fn batch_boolean_and_compose_match_folds() {
        let a = CrossSection::from_polygons(&[square_at(0.0, 0.0, 2.0)]).unwrap();
        let b = CrossSection::from_polygons(&[square_at(1.0, 0.0, 2.0)]).unwrap();
        let c = CrossSection::from_polygons(&[square_at(2.0, 0.0, 2.0)]).unwrap();
        let batch = CrossSection::batch_boolean(&[a.clone(), b.clone(), c.clone()], OpType::Add);
        let fold = a.union(&b).union(&c);
        assert!((batch.area() - fold.area()).abs() < 1e-9);
        assert!(
            (CrossSection::compose(&[a.clone(), b.clone()]).area() - a.union(&b).area()).abs()
                < 1e-9
        );

        let sub = CrossSection::batch_boolean(&[a.clone(), b.clone(), c.clone()], OpType::Subtract);
        assert!((sub.area() - a.difference(&b).difference(&c).area()).abs() < 1e-9);

        let int3 =
            CrossSection::batch_boolean(&[a.clone(), b.clone(), c.clone()], OpType::Intersect);
        assert!(int3.is_empty(), "a ∩ b ∩ c has no common region");
        assert!(CrossSection::batch_boolean(&[], OpType::Add).is_empty());
        assert_eq!(
            CrossSection::batch_boolean(core::slice::from_ref(&a), OpType::Subtract),
            a
        );
    }

    #[test]
    fn decompose_groups_outers_with_their_holes() {
        let ring = |x: f64, y: f64| {
            CrossSection::from_polygons(&[square_at(x, y, 4.0)])
                .unwrap()
                .difference(
                    &CrossSection::from_polygons(&[square_at(x + 1.0, y + 1.0, 2.0)]).unwrap(),
                )
        };
        let two = ring(0.0, 0.0).union(&ring(10.0, 0.0));
        assert_eq!(two.num_contour(), 4);
        let parts = two.decompose();
        assert_eq!(parts.len(), 2);
        for p in &parts {
            assert_eq!(p.num_contour(), 2, "each part = outer + its hole");
            assert!((p.area() - 12.0).abs() < 1e-9);
        }
        // Single component passes through whole.
        assert_eq!(ring(0.0, 0.0).decompose().len(), 1);
    }

    #[test]
    fn hull_is_the_monotone_chain() {
        // Hull of an annulus == its outer square (collinear midpoints dropped).
        let ring = CrossSection::from_polygons(&[square_at(0.0, 0.0, 4.0)])
            .unwrap()
            .difference(&CrossSection::from_polygons(&[square_at(1.0, 1.0, 2.0)]).unwrap());
        let hull = ring.hull();
        assert_eq!(hull.num_contour(), 1);
        assert!((hull.area() - 16.0).abs() < 1e-9);

        // An interior point never survives; under 3 points is empty.
        let tri = CrossSection::hull_of_points(&[
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(0.0, 4.0),
            Vec2::new(1.0, 1.0),
        ])
        .unwrap();
        assert_eq!(tri.num_vert(), 3);
        assert!((tri.area() - 8.0).abs() < 1e-12);
        assert!(
            CrossSection::hull_of_points(&[Vec2::ZERO, Vec2::new(1.0, 0.0)])
                .unwrap()
                .is_empty()
        );

        // hull_of pools sections.
        let a = CrossSection::square(Vec2::new(1.0, 1.0), false).unwrap();
        let b = a.translate(Vec2::new(3.0, 0.0)).unwrap();
        let pooled = CrossSection::hull_of(&[a, b]);
        assert!(
            (pooled.area() - 4.0).abs() < 1e-9,
            "1×4 hull strip, area {}",
            pooled.area()
        );
    }

    #[test]
    fn fill_rules_interpret_self_intersections() {
        // The cross_section_test.cpp FillRule polygon: Positive 0.683 / Negative 0.193 /
        // EvenOdd == NonZero 0.875.
        let poly = vec![
            Vec2::new(-7.0, 13.0),
            Vec2::new(-7.0, 12.0),
            Vec2::new(-5.0, 9.0),
            Vec2::new(-5.0, 8.1),
            Vec2::new(-4.8, 8.0),
        ];
        let area = |rule| {
            CrossSection::from_polygons_with(core::slice::from_ref(&poly), rule)
                .unwrap()
                .area()
        };
        assert!((area(FillRule::Positive) - 0.683).abs() < 0.001);
        assert!((area(FillRule::Negative) - 0.193).abs() < 0.001);
        assert!((area(FillRule::EvenOdd) - 0.875).abs() < 0.001);
        assert!((area(FillRule::NonZero) - 0.875).abs() < 0.001);
    }

    // ── M.5.4.5: the no-panic / typed-error boundary ──────────────────────────────────────────

    #[test]
    fn non_finite_inputs_error_never_panic() {
        // Every ingesting boundary rejects NaN/±inf with NonFiniteVertex — the C++ hits UB here,
        // and pre-M.5.4.5 these panicked inside i_overlay's debug asserts.
        let nan = f64::NAN;
        assert_eq!(
            CrossSection::square(Vec2::new(nan, 1.0), false).unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(
            CrossSection::circle(nan, 8).unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(
            CrossSection::circle(f64::INFINITY, 8).unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(
            CrossSection::from_polygons(&[vec![Vec2::new(0.0, 0.0), Vec2::new(nan, 1.0)]])
                .unwrap_err(),
            Error::NonFiniteVertex
        );
        // The inverted-infinity default Rect is non-finite → rejected.
        assert_eq!(
            CrossSection::from_rect(Rect::default()).unwrap_err(),
            Error::NonFiniteVertex
        );

        let sq = CrossSection::square(Vec2::new(2.0, 2.0), false).unwrap();
        assert_eq!(sq.warp(|p| p.x = nan).unwrap_err(), Error::NonFiniteVertex);
        assert_eq!(
            sq.translate(Vec2::new(nan, 0.0)).unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(sq.rotate(nan).unwrap_err(), Error::NonFiniteVertex);
        assert_eq!(
            sq.mirror(Vec2::new(nan, 1.0)).unwrap_err(),
            Error::NonFiniteVertex
        );
        // Overflow to ∞ through a huge scale is caught on the OUTPUT, like Mesh::transform.
        assert_eq!(
            sq.scale(Vec2::new(1e300, 1e300))
                .unwrap()
                .scale(Vec2::new(1e10, 1e10))
                .unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(
            sq.offset(nan, JoinType::Round, 2.0, 16).unwrap_err(),
            Error::NonFiniteVertex
        );
        assert_eq!(
            CrossSection::hull_of_points(&[Vec2::new(nan, 0.0), Vec2::ZERO, Vec2::new(1.0, 1.0)])
                .unwrap_err(),
            Error::NonFiniteVertex
        );
    }

    #[test]
    fn subnormal_scale_offset_errors_instead_of_panicking() {
        // The unit_normal underflow (M.5.4 verification finding): edges shorter than ~2⁻⁵³⁷ give
        // dx²+dy² == 0 → non-finite normals. Pre-M.5.4.5 this PANICKED in i_overlay's debug assert
        // (release: silently empty); now the walk's non-finite output is caught at the invariant
        // gate. (The C++ can't reach this — its int grid collapses the path to one point.)
        let tiny = CrossSection::circle(1e-300, 4).unwrap();
        assert_eq!(
            tiny.offset(1.0, JoinType::Square, 2.0, 0).unwrap_err(),
            Error::NonFiniteVertex
        );
    }

    #[test]
    fn degenerate_zero_area_contour_offset_grows_like_cpp() {
        // The all-degenerate-group rule (clipper.offset.cpp:461, M.5.4 verification finding): a
        // zero-area (collinear) contour has no orientation to read, so Clipper2 walks with |δ|.
        // A singular scale produces exactly that contour (transform does no re-union, like C++).
        let collapsed = CrossSection::from_polygons(&[vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 1.0),
            Vec2::new(1.0, 2.0),
        ]])
        .unwrap()
        .scale(Vec2::new(1.0, 0.0))
        .unwrap();
        let grown_neg = collapsed.offset(-1.0, JoinType::Miter, 2.0, 0).unwrap();
        let grown_pos = collapsed.offset(1.0, JoinType::Miter, 2.0, 0).unwrap();
        // Both walk +1 (the C++差 measured: sausage (len+2δ)·2δ = (2+2)·2 = 8).
        assert!(
            (grown_neg.area() - 8.0).abs() < 1e-9,
            "negative-δ sausage area {}",
            grown_neg.area()
        );
        assert!(
            (grown_pos.area() - grown_neg.area()).abs() < 1e-12,
            "±δ agree"
        );
    }

    #[test]
    fn reversed_input_offset_retains_cw_orientation() {
        // ExecuteInternal's ReverseSolution (M.5.4 verification finding): "the solution should
        // retain the orientation of the input". A single-axis-swapped Rect gives a CW contour
        // (area −25); its offset must come back CW (area −49), matching the C++ measured value.
        let cw = CrossSection::from_rect(Rect {
            min: Vec2::new(0.0, 5.0),
            max: Vec2::new(5.0, 0.0),
        })
        .unwrap();
        assert!(
            (cw.area() + 25.0).abs() < 1e-12,
            "CW square area {}",
            cw.area()
        );
        let grown = cw.offset(1.0, JoinType::Miter, 2.0, 0).unwrap();
        assert!(
            (grown.area() + 49.0).abs() < 1e-9,
            "offset retains CW: area {}",
            grown.area()
        );
    }
}
