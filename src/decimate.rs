//! QEM mesh decimation (W.5.3) — the web save-back's LOW-res mesh variant.
//!
//! Garland-Heckbert quadric edge-collapse: each vertex carries a 4x4 error quadric (the summed
//! squared-distance-to-incident-planes), each edge collapse relocates the surviving vertex to the
//! quadric-optimal point, and edges collapse cheapest-first until a triangle budget is met. This is
//! a DISPLAY decimation — the output feeds the site's three.js viewer, NOT a slicer — so it favors a
//! faithful silhouette over manifold guarantees (a collapse that would flip a face normal is
//! rejected, but non-manifold seams are tolerated).
//!
//! Deterministic: quadrics sum in face order, the collapse heap breaks cost ties by vertex index,
//! and the final compaction walks verts/tris in order — same mesh in, byte-identical mesh out (the
//! `deterministic_golden` test pins it). Mesh-only (raw `[f64;3]`/`[u32;3]`, no `Solid`), so it runs
//! on the wasm geom worker.
//!
//! COLOR-PRESERVING (the same-format-roundtrip rule needs a colored low-res 3MF): color rides as a
//! per-vertex RGBA carried through collapses, and a color BOUNDARY edge (endpoints of different
//! quantized color) is deprioritized behind every interior edge — so a region decimates internally
//! and its color seams stay put until nothing else is left. A boundary collapse (last resort) snaps
//! the survivor to the lower-index endpoint's color, never a blend, so the distinct-color table the
//! [`crate::threemf_out`] material pass dedups on never grows a spurious in-between shade.

/// A symmetric 4x4 error quadric, packed as its 10 unique entries in row order:
/// `[q11 q12 q13 q14  q22 q23 q24  q33 q34  q44]`. Error at a point `v=(x,y,z,1)` is `vᵀ Q v`.
#[derive(Clone, Copy, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    /// The fundamental quadric of a plane `ax+by+cz+d=0` (normal already unit): `K = p pᵀ`.
    fn plane(a: f64, b: f64, c: f64, d: f64) -> Self {
        Quadric([
            a * a,
            a * b,
            a * c,
            a * d,
            b * b,
            b * c,
            b * d,
            c * c,
            c * d,
            d * d,
        ])
    }

    fn add(&self, o: &Quadric) -> Quadric {
        let mut q = self.0;
        for (a, b) in q.iter_mut().zip(o.0) {
            *a += b;
        }
        Quadric(q)
    }

    /// `vᵀ Q v` for `v=(x,y,z,1)` — the sum of squared distances to the accumulated planes.
    fn error(&self, p: [f64; 3]) -> f64 {
        let [x, y, z] = p;
        let q = &self.0;
        q[0] * x * x
            + 2.0 * q[1] * x * y
            + 2.0 * q[2] * x * z
            + 2.0 * q[3] * x
            + q[4] * y * y
            + 2.0 * q[5] * y * z
            + 2.0 * q[6] * y
            + q[7] * z * z
            + 2.0 * q[8] * z
            + q[9]
    }

    /// The error-minimizing point, if the 3x3 leading block is non-singular (Cramer's rule). `None`
    /// when the incident planes are near-parallel (a flat/edge vertex) — the caller falls back to the
    /// endpoints or the midpoint.
    fn optimum(&self) -> Option<[f64; 3]> {
        let q = &self.0;
        // A = [[q11 q12 q13],[q12 q22 q23],[q13 q23 q33]],  A p = -[q14 q24 q34].
        let (a11, a12, a13) = (q[0], q[1], q[2]);
        let (a22, a23) = (q[4], q[5]);
        let a33 = q[7];
        let (b0, b1, b2) = (-q[3], -q[6], -q[8]);
        let det = a11 * (a22 * a33 - a23 * a23) - a12 * (a12 * a33 - a23 * a13)
            + a13 * (a12 * a23 - a22 * a13);
        if det.abs() < 1e-10 {
            return None;
        }
        let inv = 1.0 / det;
        // adjugate * b, componentwise (the symmetric-3x3 inverse).
        let x = inv
            * (b0 * (a22 * a33 - a23 * a23) - a12 * (b1 * a33 - a23 * b2)
                + a13 * (b1 * a23 - a22 * b2));
        let y = inv
            * (a11 * (b1 * a33 - a23 * b2) - b0 * (a12 * a33 - a23 * a13)
                + a13 * (a12 * b2 - b1 * a13));
        let z = inv
            * (a11 * (a22 * b2 - a23 * b1) - a12 * (a12 * b2 - a23 * b0)
                + b0 * (a12 * a23 - a22 * a13));
        Some([x, y, z])
    }
}

/// Quantize a 0..1 RGBA to 8-bit — the key color-boundary detection compares on (so float drift from
/// a boolean seam's linear-interpolated color doesn't read as a distinct region). Mirrors
/// [`crate::threemf_out`]'s quantization so the two agree on what "the same color" means.
fn qcolor(c: [f64; 4]) -> [u8; 4] {
    let q = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c[0]), q(c[1]), q(c[2]), q(c[3])]
}

/// A decimated mesh: verts, 0-based triangle indices, and (when the input was colored) the surviving
/// per-vertex RGBA, index-aligned to `verts`.
pub struct Decimated {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub colors: Option<Vec<[f64; 4]>>,
}

/// A collapse candidate on the heap. Ordered as a MIN-heap on `(boundary, cost, u, v)` — non-boundary
/// before boundary, then cheapest, then lowest index (the deterministic tiebreak) — realized by
/// reversing every comparison (`BinaryHeap` pops the max).
struct Cand {
    boundary: bool,
    cost: f64,
    u: u32,
    v: u32,
    tgt: [f64; 3],
    tcol: [f64; 4],
    // Endpoint version stamps at push time — a later collapse touching u or v bumps its stamp, so a
    // stale candidate (stamp mismatch) is skipped at pop rather than eagerly deleted.
    vu: u64,
    vv: u64,
}

impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == std::cmp::Ordering::Equal
    }
}
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cand {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // Reverse everything: the smallest (boundary,cost,u,v) key must compare as GREATEST so the
        // max-heap pops it first.
        o.boundary
            .cmp(&self.boundary)
            .then_with(|| o.cost.total_cmp(&self.cost))
            .then_with(|| o.u.cmp(&self.u))
            .then_with(|| o.v.cmp(&self.v))
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The (unnormalized) face normal of `[a,b,c]`.
fn face_normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f64; 3] {
    cross(sub(b, a), sub(c, a))
}

/// The working decimation state — one mesh, mutated in place through collapses, compacted at the end.
struct Mesh {
    pos: Vec<[f64; 3]>,
    col: Vec<[f64; 4]>, // empty when uncolored
    qcol: Vec<[u8; 4]>, // empty when uncolored — boundary detection key
    quad: Vec<Quadric>,
    valive: Vec<bool>,
    ver: Vec<u64>,
    vfaces: Vec<Vec<u32>>, // incident face indices (may hold dead/duplicate entries; filtered on read)
    tris: Vec<[u32; 3]>,
    falive: Vec<bool>,
    live_faces: usize,
    colored: bool,
}

impl Mesh {
    fn build(verts: &[[f64; 3]], tris: &[[u32; 3]], colors: Option<&[[f64; 4]]>) -> Mesh {
        let n = verts.len();
        let colored = colors.is_some_and(|c| c.len() == n);
        let col: Vec<[f64; 4]> = if colored {
            colors.unwrap().to_vec()
        } else {
            Vec::new()
        };
        let qcol: Vec<[u8; 4]> = col.iter().map(|&c| qcolor(c)).collect();

        // Per-vertex quadric = sum of incident face plane quadrics; adjacency built in the same pass.
        let mut quad = vec![Quadric::default(); n];
        let mut vfaces: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (fi, t) in tris.iter().enumerate() {
            let [ia, ib, ic] = *t;
            let (a, b, c) = (verts[ia as usize], verts[ib as usize], verts[ic as usize]);
            let nrm = face_normal(a, b, c);
            let len = dot(nrm, nrm).sqrt();
            if len > 0.0 {
                let (nx, ny, nz) = (nrm[0] / len, nrm[1] / len, nrm[2] / len);
                let d = -(nx * a[0] + ny * a[1] + nz * a[2]);
                let kp = Quadric::plane(nx, ny, nz, d);
                for &i in &[ia, ib, ic] {
                    quad[i as usize] = quad[i as usize].add(&kp);
                }
            }
            for &i in &[ia, ib, ic] {
                vfaces[i as usize].push(fi as u32);
            }
        }

        Mesh {
            pos: verts.to_vec(),
            col,
            qcol,
            quad,
            valive: vec![true; n],
            ver: vec![0; n],
            vfaces,
            tris: tris.to_vec(),
            falive: vec![true; tris.len()],
            live_faces: tris.len(),
            colored,
        }
    }

    /// The other two vertices of face `fi` besides `v` (the face is a triangle, so exactly two).
    fn opposite(&self, fi: u32, v: u32) -> [u32; 2] {
        let t = self.tris[fi as usize];
        let mut out = [u32::MAX; 2];
        let mut k = 0;
        for &i in &t {
            if i != v {
                out[k] = i;
                k += 1;
            }
        }
        out
    }

    /// The live neighbor vertices of `v` (dedup + sorted → deterministic re-push order).
    fn neighbors(&self, v: u32) -> Vec<u32> {
        let mut ns: Vec<u32> = Vec::new();
        for &fi in &self.vfaces[v as usize] {
            if !self.falive[fi as usize] {
                continue;
            }
            for w in self.opposite(fi, v) {
                if w != u32::MAX && w != v && self.valive[w as usize] && !ns.contains(&w) {
                    ns.push(w);
                }
            }
        }
        ns.sort_unstable();
        ns
    }

    /// Cost + optimal target + color for collapsing edge `(u,v)` — `u<v`, and `u` survives. The
    /// survivor keeps the LOWER-index endpoint's color (no blend), so a boundary collapse snaps to one
    /// real region color.
    fn candidate(&self, u: u32, v: u32) -> Cand {
        let q = self.quad[u as usize].add(&self.quad[v as usize]);
        let pu = self.pos[u as usize];
        let pv = self.pos[v as usize];
        let mid = [
            (pu[0] + pv[0]) * 0.5,
            (pu[1] + pv[1]) * 0.5,
            (pu[2] + pv[2]) * 0.5,
        ];
        // Optimal point if solvable, else the cheapest of the two endpoints + the midpoint.
        let (tgt, cost) = match q.optimum() {
            Some(p) => (p, q.error(p)),
            None => [pu, pv, mid]
                .into_iter()
                .map(|p| (p, q.error(p)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("three candidates"),
        };
        let boundary = self.colored && self.qcol[u as usize] != self.qcol[v as usize];
        let tcol = if self.colored {
            self.col[u as usize]
        } else {
            [0.0; 4]
        };
        Cand {
            boundary,
            cost: cost.max(0.0),
            u,
            v,
            tgt,
            tcol,
            vu: self.ver[u as usize],
            vv: self.ver[v as usize],
        }
    }

    /// Would moving `v` (and, if `v==from`, its identity to `to` at `tgt`) flip any surviving incident
    /// face? Checks every live face of `v` that will OUTLIVE the collapse (i.e. doesn't contain the
    /// partner `to`): a sign change of the face normal, or a collapse to zero area, rejects.
    fn flips(&self, v: u32, to: u32, tgt: [f64; 3]) -> bool {
        for &fi in &self.vfaces[v as usize] {
            if !self.falive[fi as usize] {
                continue;
            }
            let t = self.tris[fi as usize];
            if t.contains(&to) {
                continue; // one of the ≤2 faces the collapse removes
            }
            let before = face_normal(
                self.pos[t[0] as usize],
                self.pos[t[1] as usize],
                self.pos[t[2] as usize],
            );
            let after_pos = |i: u32| if i == v { tgt } else { self.pos[i as usize] };
            let after = face_normal(after_pos(t[0]), after_pos(t[1]), after_pos(t[2]));
            let a2 = dot(after, after);
            if a2 <= 0.0 || dot(before, after) <= 0.0 {
                return true;
            }
        }
        false
    }

    /// Collapse edge `(u,v)` (u survives at `tgt` with `tcol`), returning the freed face count. The
    /// caller has already validated liveness, staleness, and flips.
    fn collapse(&mut self, u: u32, v: u32, tgt: [f64; 3], tcol: [f64; 4]) -> usize {
        // Faces containing BOTH u and v die; faces of v that survive get v rewired to u.
        let vfaces_v = std::mem::take(&mut self.vfaces[v as usize]);
        let mut freed = 0;
        for fi in vfaces_v {
            if !self.falive[fi as usize] {
                continue;
            }
            let t = &mut self.tris[fi as usize];
            if t.contains(&u) {
                self.falive[fi as usize] = false;
                freed += 1;
            } else {
                for slot in t.iter_mut() {
                    if *slot == v {
                        *slot = u;
                    }
                }
                self.vfaces[u as usize].push(fi);
            }
        }
        self.pos[u as usize] = tgt;
        self.quad[u as usize] = self.quad[u as usize].add(&self.quad[v as usize]);
        if self.colored {
            self.col[u as usize] = tcol;
            self.qcol[u as usize] = qcolor(tcol);
        }
        self.valive[v as usize] = false;
        self.ver[u as usize] += 1;
        self.live_faces -= freed;
        freed
    }

    /// Compact away dead verts/faces → a fresh 0-based indexed mesh.
    fn finish(self) -> Decimated {
        let mut remap = vec![u32::MAX; self.pos.len()];
        let mut verts = Vec::new();
        let mut colors = if self.colored { Some(Vec::new()) } else { None };
        for (i, &alive) in self.valive.iter().enumerate() {
            if alive {
                remap[i] = verts.len() as u32;
                verts.push(self.pos[i]);
                if let Some(c) = colors.as_mut() {
                    c.push(self.col[i]);
                }
            }
        }
        let mut tris = Vec::new();
        for (fi, t) in self.tris.iter().enumerate() {
            if !self.falive[fi] {
                continue;
            }
            let [a, b, c] = [
                remap[t[0] as usize],
                remap[t[1] as usize],
                remap[t[2] as usize],
            ];
            // A live face only ever references live verts, but a degenerate (repeated index) is
            // dropped defensively.
            if a != u32::MAX && b != u32::MAX && c != u32::MAX && a != b && b != c && a != c {
                tris.push([a, b, c]);
            }
        }
        Decimated {
            verts,
            tris,
            colors,
        }
    }
}

/// The publish/save-back PREVIEW-mesh triangle budget, picked from the full mesh's triangle count (W.3.41).
/// A FIXED count is the wrong lever: measured on real parts, 20K is 6% of a detailed holder (visibly
/// faceted, ~1.2mm mean surface error) but 21% of a simple one (fine) — quality tracks the budget/full
/// RATIO, both models converging to ~0.10% mean deviation (of bbox diagonal) around a quarter of full. So:
/// a quarter of the full mesh, FLOORED so simple parts still read clean and CAPPED so a dense part's embed
/// payload stays bounded (~8 MB 3MF at 100K tris vs 30 MB at full). `decimate_mesh`'s conditional-skip
/// leaves an already-sub-budget mesh untouched, so tiny models pass through this unaffected.
pub fn preview_budget(full_tris: usize) -> usize {
    (full_tris / 4).clamp(40_000, 100_000)
}

/// Decimate one indexed mesh to at most `target` triangles via QEM edge-collapse. Below the budget
/// already → returned unchanged (the conditional skip). `colors` (per-vertex RGBA 0..1, index-aligned
/// to `verts`; a length mismatch is treated as uncolored) rides through and is preserved region-wise.
pub fn decimate_mesh(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    colors: Option<&[[f64; 4]]>,
    target: usize,
) -> Decimated {
    // Conditional skip: already lean (or trivially small) → hand it back as-is.
    if tris.len() <= target.max(1) || verts.len() < 4 {
        return Decimated {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            colors: colors
                .filter(|c| c.len() == verts.len())
                .map(<[[f64; 4]]>::to_vec),
        };
    }

    let mut m = Mesh::build(verts, tris, colors);

    // Seed the heap with one candidate per unique edge.
    let mut heap: std::collections::BinaryHeap<Cand> = std::collections::BinaryHeap::new();
    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let e = (a.min(b), a.max(b));
            if e.0 != e.1 && seen.insert(e) {
                heap.push(m.candidate(e.0, e.1));
            }
        }
    }

    while m.live_faces > target {
        let Some(c) = heap.pop() else {
            break; // heap drained before the budget — every remaining collapse is blocked (flips)
        };
        // Stale? (either endpoint dead, or a versioned change since push) → drop it.
        if !m.valive[c.u as usize]
            || !m.valive[c.v as usize]
            || m.ver[c.u as usize] != c.vu
            || m.ver[c.v as usize] != c.vv
        {
            continue;
        }
        // Foldover guard: reject a collapse that flips any surviving face around either endpoint.
        if m.flips(c.v, c.u, c.tgt) || m.flips(c.u, c.v, c.tgt) {
            continue;
        }
        m.collapse(c.u, c.v, c.tgt, c.tcol);
        // Re-cost every edge now incident to the survivor (its quadric + position moved).
        for n in m.neighbors(c.u) {
            let (a, b) = (c.u.min(n), c.u.max(n));
            heap.push(m.candidate(a, b));
        }
    }

    m.finish()
}

/// A mesh part for batch decimation: owned verts/tris/optional colors.
pub struct Part {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub colors: Option<Vec<[f64; 4]>>,
}

/// Decimate several disjoint parts to a COMBINED `total_target`, budgeting each part in proportion to
/// its triangle share (min 4 tris apiece so a tiny part isn't decimated to nothing). Parts are
/// independent, so this maps in parallel (rayon) on native; the wasm worker runs it sequentially (the
/// heavy parallelism there is the manifold render upstream — decimation is the light tail).
pub fn decimate_parts(parts: Vec<Part>, total_target: usize) -> Vec<Decimated> {
    let total_tris: usize = parts.iter().map(|p| p.tris.len()).sum();
    let budgets: Vec<usize> = parts
        .iter()
        .map(|p| {
            if total_tris == 0 {
                0
            } else {
                // Proportional share, floored at 4 so a small part keeps a usable silhouette.
                ((total_target as u128 * p.tris.len() as u128) / total_tris as u128).max(4) as usize
            }
        })
        .collect();

    let one =
        |(p, budget): (&Part, usize)| decimate_mesh(&p.verts, &p.tris, p.colors.as_deref(), budget);

    #[cfg(feature = "native")]
    {
        use rayon::prelude::*;
        parts
            .par_iter()
            .zip(budgets)
            .map(|(p, b)| one((p, b)))
            .collect()
    }
    #[cfg(not(feature = "native"))]
    {
        parts
            .iter()
            .zip(budgets)
            .map(|(p, b)| one((p, b)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A unit-sphere octahedron subdivided `k` times (midpoints projected to the sphere), welded via a
    /// shared-midpoint cache → a closed manifold with real interior edges and increasing curvature.
    /// `k=0` is the bare octahedron (8 tris); each level quadruples the count.
    fn octasphere(k: u32) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let mut verts: Vec<[f64; 3]> = vec![
            [1., 0., 0.],
            [-1., 0., 0.],
            [0., 1., 0.],
            [0., -1., 0.],
            [0., 0., 1.],
            [0., 0., -1.],
        ];
        let mut tris: Vec<[u32; 3]> = vec![
            [0, 2, 4],
            [2, 1, 4],
            [1, 3, 4],
            [3, 0, 4],
            [2, 0, 5],
            [1, 2, 5],
            [3, 1, 5],
            [0, 3, 5],
        ];
        for _ in 0..k {
            let mut mid: std::collections::HashMap<(u32, u32), u32> =
                std::collections::HashMap::new();
            let mut next = Vec::new();
            let mut midpoint = |a: u32, b: u32, verts: &mut Vec<[f64; 3]>| -> u32 {
                let key = (a.min(b), a.max(b));
                *mid.entry(key).or_insert_with(|| {
                    let (pa, pb) = (verts[a as usize], verts[b as usize]);
                    let mut m = [
                        (pa[0] + pb[0]) * 0.5,
                        (pa[1] + pb[1]) * 0.5,
                        (pa[2] + pb[2]) * 0.5,
                    ];
                    let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
                    m = [m[0] / len, m[1] / len, m[2] / len];
                    verts.push(m);
                    (verts.len() - 1) as u32
                })
            };
            for t in &tris {
                let ab = midpoint(t[0], t[1], &mut verts);
                let bc = midpoint(t[1], t[2], &mut verts);
                let ca = midpoint(t[2], t[0], &mut verts);
                next.push([t[0], ab, ca]);
                next.push([ab, t[1], bc]);
                next.push([ca, bc, t[2]]);
                next.push([ab, bc, ca]);
            }
            tris = next;
        }
        (verts, tris)
    }

    fn bbox(verts: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
        let mut mn = [f64::MAX; 3];
        let mut mx = [f64::MIN; 3];
        for v in verts {
            for i in 0..3 {
                mn[i] = mn[i].min(v[i]);
                mx[i] = mx[i].max(v[i]);
            }
        }
        (mn, mx)
    }

    fn assert_wellformed(d: &Decimated) {
        for v in &d.verts {
            assert!(
                v.iter().all(|c| c.is_finite()),
                "vertex has non-finite coord: {v:?}"
            );
        }
        let n = d.verts.len() as u32;
        for t in &d.tris {
            assert!(t.iter().all(|&i| i < n), "tri index out of range: {t:?}");
            assert!(
                t[0] != t[1] && t[1] != t[2] && t[0] != t[2],
                "degenerate tri: {t:?}"
            );
        }
    }

    #[test]
    fn preview_budget_is_a_clamped_quarter() {
        // Below the floor: a simple part gets the 40K floor, not full_tris/4.
        assert_eq!(preview_budget(95_000), 40_000); // shower_holder_mini: 23.7K → floor
        // In-band: a quarter of full (the shower_holder case — was faceted at a fixed 20K).
        assert_eq!(preview_budget(342_320), 85_580);
        // Above the ceiling: a dense part is capped so the embed payload stays bounded.
        assert_eq!(preview_budget(2_000_000), 100_000);
        // Degenerate: an empty mesh still yields the floor (harmless — decimate skips it anyway).
        assert_eq!(preview_budget(0), 40_000);
    }

    #[test]
    fn skips_when_already_under_budget() {
        let (v, t) = octasphere(1); // 32 tris
        let d = decimate_mesh(&v, &t, None, 1000);
        assert_eq!(d.tris.len(), t.len(), "under budget → untouched");
        assert_eq!(d.verts.len(), v.len());
    }

    #[test]
    fn reduces_to_budget_and_keeps_the_silhouette() {
        let (v, t) = octasphere(4); // 8*4^4 = 2048 tris
        assert_eq!(t.len(), 2048);
        let d = decimate_mesh(&v, &t, None, 300);
        assert_wellformed(&d);
        assert!(d.tris.len() <= 300, "hit the budget: {} tris", d.tris.len());
        assert!(
            d.tris.len() > 50,
            "didn't collapse to nothing: {} tris",
            d.tris.len()
        );
        // A unit sphere: the decimated silhouette stays within a hair of the unit box.
        let (mn, mx) = bbox(&d.verts);
        assert!(
            mn.iter().all(|&c| c > -1.2) && mx.iter().all(|&c| c < 1.2),
            "silhouette drifted: {mn:?}..{mx:?}"
        );
        assert!(
            mx.iter().all(|&c| c > 0.8) && mn.iter().all(|&c| c < -0.8),
            "silhouette collapsed inward: {mn:?}..{mx:?}"
        );
    }

    #[test]
    fn deterministic_golden() {
        let (v, t) = octasphere(4);
        let a = decimate_mesh(&v, &t, None, 400);
        let b = decimate_mesh(&v, &t, None, 400);
        assert_eq!(a.verts, b.verts, "same mesh → identical verts");
        assert_eq!(a.tris, b.tris, "same mesh → identical tris");
        // Pin the count so a silent change in the collapse order/heuristics is caught.
        assert_eq!(a.tris.len(), 400, "golden decimated tri count");
    }

    #[test]
    fn color_preserving_keeps_regions_crisp() {
        // Color the sphere by hemisphere: z>=0 red, else blue. Decimation must NEVER emit a blended
        // third color — the low-res 3MF's material table stays exactly {red, blue}.
        let (v, t) = octasphere(4);
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let colors: Vec<[f64; 4]> = v
            .iter()
            .map(|p| if p[2] >= 0.0 { red } else { blue })
            .collect();
        let d = decimate_mesh(&v, &t, Some(&colors), 300);
        assert_wellformed(&d);
        let out = d.colors.expect("colored in → colored out");
        assert_eq!(out.len(), d.verts.len(), "colors index-aligned to verts");
        let distinct: HashSet<[u8; 4]> = out.iter().map(|&c| qcolor(c)).collect();
        assert!(
            distinct
                .iter()
                .all(|c| *c == qcolor(red) || *c == qcolor(blue)),
            "a blended color leaked in: {distinct:?}"
        );
        assert_eq!(distinct.len(), 2, "both regions survive: {distinct:?}");
    }

    #[test]
    fn parts_split_budget_and_run() {
        // Two independent spheres → one combined budget, proportionally shared.
        let (v, t) = octasphere(3); // 512 tris each
        let parts = vec![
            Part {
                verts: v.clone(),
                tris: t.clone(),
                colors: None,
            },
            Part {
                verts: v.clone(),
                tris: t.clone(),
                colors: None,
            },
        ];
        let out = decimate_parts(parts, 200);
        assert_eq!(out.len(), 2);
        let total: usize = out.iter().map(|d| d.tris.len()).sum();
        assert!(total <= 200 + 8, "combined budget respected: {total}");
        for d in &out {
            assert_wellformed(d);
            assert!(d.tris.len() > 4, "each part keeps geometry");
        }
    }
}
