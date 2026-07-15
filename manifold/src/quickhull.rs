//! Convex hull — a verbatim port of Manifold's `quickhull.cpp`/`.h` (itself derived from Antti
//! Kuukka's public-domain QuickHull). Builds the convex hull of a 3D point cloud as a triangle mesh:
//! seed a tetrahedron from extreme points, then iteratively extrude each face's farthest outside
//! point onto the horizon until no point sits outside any face.
//!
//! Two deliberate deviations from the C++, both invisible to the geometry oracle (which compares
//! volume / genus / point-in-mesh, all invariant under the changes below):
//!
//! - **Owned point data.** C++'s `originalVertexData` is a `VecView` that gets *rebound* to an
//!   internal `planarPointCloudTemp` on the degenerate/planar paths — a self-reference Rust won't
//!   allow. We keep one OWNED `Vec<Vec3>` and push the degenerate padding / planar apex onto it
//!   directly; `planarPointCloudTemp` was a copy of the same data anyway, so the collapsed
//!   single-buffer model reproduces the C++ aliasing (including the planar `back = front` reset).
//!
//! - **Serial reorder tail.** `build_mesh`'s final compaction (C++ `for_each` + `AtomicAdd` +
//!   `exclusive_scan`) is ported serial. It only RENUMBERS the finished hull — the hull SHAPE comes
//!   entirely from the serial `create_convex_halfedge_mesh` loop — so serial is bit-faithful to the
//!   C++ sequential policy, and any residual halfedge-ordering difference is invisible to the solid
//!   differential. Deterministic parallelism is M.4's problem, not the hull's.
//!
//! Cancellation (the C++ `ExecutionContext`/`IsCancelled` plumbing) is dropped: there is no ctx in
//! this kernel yet, and it's orthogonal to the geometry.

use std::collections::VecDeque;

use crate::linalg::Vec3;
use crate::mesh::{Halfedge, Mesh};
use crate::mesh_ids::{HalfedgeId, VertId};
use crate::status::Error;

/// Minimum plane distance (for a point cloud of scale 1) to count a point as "outside" a face.
fn default_eps() -> f64 {
    0.0000001
}

// --- Free geometric helpers (quickhull.cpp file-scope inline functions) ---

fn get_squared_distance_between_point_and_ray(p: Vec3, r: &Ray) -> f64 {
    let s = p - r.s;
    let t = s.dot(r.v);
    s.dot(s) - t * t * r.v_inv_length_squared
}

fn get_squared_distance(p1: Vec3, p2: Vec3) -> f64 {
    (p1 - p2).dot(p1 - p2)
}

/// Signed distance in units of the plane normal's length (divide by `|N|` for the real distance).
fn get_signed_distance_to_plane(v: Vec3, p: &Plane) -> f64 {
    p.n.dot(v) + p.d
}

/// `normalize((a-c) × (b-c))` — computed component-wise exactly as the C++ (matched op order).
fn get_triangle_normal(a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let x = a.x - c.x;
    let y = a.y - c.y;
    let z = a.z - c.z;
    let rhsx = b.x - c.x;
    let rhsy = b.y - c.y;
    let rhsz = b.z - c.z;
    let px = y * rhsz - z * rhsy;
    let py = z * rhsx - x * rhsz;
    let pz = x * rhsy - y * rhsx;
    Vec3::new(px, py, pz).normalize()
}

// --- Plane / Ray ---

#[derive(Clone, Copy, Default)]
struct Plane {
    n: Vec3,
    /// Signed distance to the plane from the origin (if `|N| == 1`).
    d: f64,
    /// `|N|²`.
    sqr_n_length: f64,
}

impl Plane {
    fn is_point_on_positive_side(&self, q: Vec3) -> bool {
        let d = self.n.dot(q) + self.d;
        d >= 0.0
    }

    /// Plane through point `p` with normal `n`.
    fn new(n: Vec3, p: Vec3) -> Plane {
        Plane {
            n,
            d: (-n).dot(p),
            sqr_n_length: n.dot(n),
        }
    }
}

struct Ray {
    s: Vec3,
    v: Vec3,
    v_inv_length_squared: f64,
}

impl Ray {
    fn new(s: Vec3, v: Vec3) -> Ray {
        Ray {
            s,
            v,
            v_inv_length_squared: 1.0 / v.dot(v),
        }
    }
}

// --- MeshBuilder: the working half-edge mesh QuickHull mutates in place ---

/// The value-form half-edge QuickHull works with internally (C++ `Halfedge`: `endVert` is stored,
/// not derived; `propVert` is unused here so it's dropped). Converted to the spine [`Halfedge`]
/// (which DERIVES end) only at `build_mesh`'s exit.
#[derive(Clone, Copy, Default)]
struct QHalfedge {
    start_vert: i32,
    end_vert: i32,
    paired_halfedge: i32,
}

struct Face {
    /// One of this face's half-edges, or `-1` when the face is disabled.
    he: i32,
    p: Plane,
    most_distant_point_dist: f64,
    most_distant_point: usize,
    visibility_checked_on_iteration: usize,
    is_visible_face_on_current_iteration: bool,
    in_face_stack: bool,
    /// One bit per half-edge of this face: set iff that edge is a horizon edge (3 bits used).
    horizon_edges_on_current_iteration: u8,
    points_on_positive_side: Option<Vec<usize>>,
}

impl Face {
    fn new(he: i32) -> Face {
        Face {
            he,
            p: Plane::default(),
            most_distant_point_dist: 0.0,
            most_distant_point: 0,
            visibility_checked_on_iteration: 0,
            is_visible_face_on_current_iteration: false,
            in_face_stack: false,
            horizon_edges_on_current_iteration: 0,
            points_on_positive_side: None,
        }
    }

    fn disable(&mut self) {
        self.he = -1;
    }

    fn is_disabled(&self) -> bool {
        self.he == -1
    }
}

impl Default for Face {
    fn default() -> Face {
        Face::new(-1)
    }
}

#[derive(Default)]
struct MeshBuilder {
    faces: Vec<Face>,
    halfedges: Vec<QHalfedge>,
    halfedge_to_face: Vec<i32>,
    halfedge_next: Vec<i32>,
    // Removed faces/half-edges aren't erased — they're marked disabled and their slots recycled.
    disabled_faces: Vec<usize>,
    disabled_halfedges: Vec<usize>,
}

impl MeshBuilder {
    fn add_face(&mut self) -> usize {
        if let Some(index) = self.disabled_faces.pop() {
            debug_assert!(self.faces[index].is_disabled());
            debug_assert!(self.faces[index].points_on_positive_side.is_none());
            self.faces[index].most_distant_point_dist = 0.0;
            return index;
        }
        self.faces.push(Face::default());
        self.faces.len() - 1
    }

    fn add_halfedge(&mut self) -> usize {
        if let Some(index) = self.disabled_halfedges.pop() {
            return index;
        }
        self.halfedges.push(QHalfedge::default());
        self.halfedge_to_face.push(0);
        self.halfedge_next.push(0);
        self.halfedges.len() - 1
    }

    /// Disable a face and hand back the points that were on its positive side (moved out).
    fn disable_face(&mut self, face_index: usize) -> Option<Vec<usize>> {
        self.faces[face_index].disable();
        self.disabled_faces.push(face_index);
        self.faces[face_index].points_on_positive_side.take()
    }

    fn disable_halfedge(&mut self, he_index: usize) {
        self.halfedges[he_index].paired_halfedge = -1;
        self.disabled_halfedges.push(he_index);
    }

    /// Build the initial tetrahedron ABCD. `dot(AB, normal(ABC))` should be negative.
    fn setup(&mut self, a: i32, b: i32, c: i32, d: i32) {
        self.faces.clear();
        self.halfedges.clear();
        self.halfedge_to_face.clear();
        self.halfedge_next.clear();
        self.disabled_faces.clear();
        self.disabled_halfedges.clear();

        // Each row: (endVert, pairedHalfedge, halfedgeToFace, halfedgeNext); startVert is 0 (unused
        // until build_mesh recomputes it). Verbatim from quickhull.cpp's setup().
        #[rustfmt::skip]
        let table: [(i32, i32, i32, i32); 12] = [
            (b,  6, 0, 1),  // AB
            (c,  9, 0, 2),  // BC
            (a,  3, 0, 0),  // CA
            (c,  2, 1, 4),  // AC
            (d, 11, 1, 5),  // CD
            (a,  7, 1, 3),  // DA
            (a,  0, 2, 7),  // BA
            (d,  5, 2, 8),  // AD
            (b, 10, 2, 6),  // DB
            (b,  1, 3, 10), // CB
            (d,  8, 3, 11), // BD
            (c,  4, 3, 9),  // DC
        ];
        for &(end, paired, to_face, next) in table.iter() {
            self.halfedges.push(QHalfedge {
                start_vert: 0,
                end_vert: end,
                paired_halfedge: paired,
            });
            self.halfedge_to_face.push(to_face);
            self.halfedge_next.push(next);
        }
        self.faces.push(Face::new(0));
        self.faces.push(Face::new(3));
        self.faces.push(Face::new(6));
        self.faces.push(Face::new(9));
    }

    fn get_vertex_indices_of_face(&self, f: &Face) -> [i32; 3] {
        let mut index = f.he as usize;
        let v0 = self.halfedges[index].end_vert;
        index = self.halfedge_next[index] as usize;
        let v1 = self.halfedges[index].end_vert;
        index = self.halfedge_next[index] as usize;
        let v2 = self.halfedges[index].end_vert;
        [v0, v1, v2]
    }

    fn get_vertex_indices_of_half_edge(&self, he: &QHalfedge) -> [i32; 2] {
        [
            self.halfedges[he.paired_halfedge as usize].end_vert,
            he.end_vert,
        ]
    }

    fn get_half_edge_indices_of_face(&self, f: &Face) -> [i32; 3] {
        let n0 = self.halfedge_next[f.he as usize];
        let n1 = self.halfedge_next[n0 as usize];
        [f.he, n0, n1]
    }
}

// --- The index-vector pool (recycles the per-face point vectors) ---

#[derive(Default)]
struct Pool {
    data: Vec<Vec<usize>>,
}

impl Pool {
    fn clear(&mut self) {
        self.data.clear();
    }

    fn reclaim(&mut self, v: Vec<usize>) {
        self.data.push(v);
    }

    fn get(&mut self) -> Vec<usize> {
        self.data.pop().unwrap_or_default()
    }
}

// --- QuickHull ---

#[derive(Clone, Copy)]
struct FaceData {
    face_index: i32,
    /// If this face turns out not to be visible, this half-edge is marked a horizon edge.
    entered_from_halfedge: i32,
}

/// A QuickHull computation over a point cloud. One instance per hull (thread-safe by isolation, as in
/// the C++). Construct with [`QuickHull::new`], then call [`QuickHull::build_mesh`].
pub struct QuickHull {
    m_epsilon: f64,
    epsilon_squared: f64,
    scale: f64,
    planar: bool,
    /// The point cloud. OWNED (see module doc): the degenerate-padding and planar-apex paths push
    /// onto this directly, replacing the C++ `VecView` rebind to `planarPointCloudTemp`.
    original_vertex_data: Vec<Vec3>,
    mesh: MeshBuilder,
    extreme_values: [usize; 6],
    failed_horizon_edges: usize,

    // Scratch reused across iterations.
    new_face_indices: Vec<usize>,
    new_halfedge_indices: Vec<usize>,
    visible_faces: Vec<usize>,
    horizon_edges_data: Vec<usize>,
    possibly_visible_faces: Vec<FaceData>,
    disabled_face_point_vectors: Vec<Vec<usize>>,
    face_list: VecDeque<i32>,
    index_vector_pool: Pool,
}

impl QuickHull {
    /// A QuickHull over the given point cloud (copied in).
    pub fn new(point_cloud: &[Vec3]) -> QuickHull {
        QuickHull {
            m_epsilon: 0.0,
            epsilon_squared: 0.0,
            scale: 0.0,
            planar: false,
            original_vertex_data: point_cloud.to_vec(),
            mesh: MeshBuilder::default(),
            extreme_values: [0; 6],
            failed_horizon_edges: 0,
            new_face_indices: Vec::new(),
            new_halfedge_indices: Vec::new(),
            visible_faces: Vec::new(),
            horizon_edges_data: Vec::new(),
            possibly_visible_faces: Vec::new(),
            disabled_face_point_vectors: Vec::new(),
            face_list: VecDeque::new(),
            index_vector_pool: Pool::default(),
        }
    }

    fn get_index_vector_from_pool(&mut self) -> Vec<usize> {
        let mut r = self.index_vector_pool.get();
        r.clear();
        r
    }

    fn reclaim_to_index_vector_pool(&mut self, ptr: Vec<usize>) {
        let old_size = ptr.len();
        if (old_size + 1) * 128 < ptr.capacity() {
            // The vector has grown far larger than it needs to be — drop it instead of pooling, so
            // the huge vectors from early iterations don't stay resident (C++ `ptr.reset(nullptr)`).
            return;
        }
        self.index_vector_pool.reclaim(ptr);
    }

    /// Associate point `point_index` with face `face_index` if it's on the plane's positive side.
    /// Returns true when it was added.
    fn add_point_to_face(&mut self, face_index: usize, point_index: usize) -> bool {
        let point = self.original_vertex_data[point_index];
        let d = get_signed_distance_to_plane(point, &self.mesh.faces[face_index].p);
        let sqr_n_length = self.mesh.faces[face_index].p.sqr_n_length;
        if d > 0.0 && d * d > self.epsilon_squared * sqr_n_length {
            if self.mesh.faces[face_index].points_on_positive_side.is_none() {
                let v = self.get_index_vector_from_pool();
                self.mesh.faces[face_index].points_on_positive_side = Some(v);
            }
            let f = &mut self.mesh.faces[face_index];
            f.points_on_positive_side
                .as_mut()
                .unwrap()
                .push(point_index);
            if d > f.most_distant_point_dist {
                f.most_distant_point_dist = d;
                f.most_distant_point = point_index;
            }
            return true;
        }
        false
    }

    fn get_extreme_values(&self) -> [usize; 6] {
        let mut out_indices = [0usize; 6];
        let p0 = self.original_vertex_data[0];
        let mut extreme_vals = [p0.x, p0.x, p0.y, p0.y, p0.z, p0.z];
        let v_count = self.original_vertex_data.len();
        for i in 1..v_count {
            let pos = self.original_vertex_data[i];
            if pos.x > extreme_vals[0] {
                extreme_vals[0] = pos.x;
                out_indices[0] = i;
            } else if pos.x < extreme_vals[1] {
                extreme_vals[1] = pos.x;
                out_indices[1] = i;
            }
            if pos.y > extreme_vals[2] {
                extreme_vals[2] = pos.y;
                out_indices[2] = i;
            } else if pos.y < extreme_vals[3] {
                extreme_vals[3] = pos.y;
                out_indices[3] = i;
            }
            if pos.z > extreme_vals[4] {
                extreme_vals[4] = pos.z;
                out_indices[4] = i;
            } else if pos.z < extreme_vals[5] {
                extreme_vals[5] = pos.z;
                out_indices[5] = i;
            }
        }
        out_indices
    }

    /// The largest absolute coordinate among the extreme points (the C++ pointer trick reads
    /// component `i/2` of extreme point `i`).
    fn get_scale(&self, extreme_values_input: [usize; 6]) -> f64 {
        let mut s = 0.0;
        for (i, &ev) in extreme_values_input.iter().enumerate() {
            let p = self.original_vertex_data[ev];
            let component = match i / 2 {
                0 => p.x,
                1 => p.y,
                _ => p.z,
            };
            let a = component.abs();
            if a > s {
                s = a;
            }
        }
        s
    }

    /// Rearrange `horizon_edges` so consecutive edges share a vertex, forming a loop. Returns false
    /// on failure (numerical instability), matching the C++ give-up path.
    fn reorder_horizon_edges(&self, horizon_edges: &mut [usize]) -> bool {
        let horizon_edge_count = horizon_edges.len();
        let mut i = 0;
        while i + 1 < horizon_edge_count {
            let end_vertex_check = self.mesh.halfedges[horizon_edges[i]].end_vert;
            let mut found_next = false;
            for j in (i + 1)..horizon_edge_count {
                let paired = self.mesh.halfedges[horizon_edges[j]].paired_halfedge;
                let begin_vertex = self.mesh.halfedges[paired as usize].end_vert;
                if begin_vertex == end_vertex_check {
                    horizon_edges.swap(i + 1, j);
                    found_next = true;
                    break;
                }
            }
            if !found_next {
                return false;
            }
            i += 1;
        }
        debug_assert_eq!(
            self.mesh.halfedges[horizon_edges[horizon_edges.len() - 1]].end_vert,
            self.mesh.halfedges
                [self.mesh.halfedges[horizon_edges[0]].paired_halfedge as usize]
                .end_vert
        );
        true
    }

    /// Build the base tetrahedron and assign every outside point to a face. `extreme_values` must be
    /// set. Handles the degenerate (≤1D, 2D-planar) cases by producing a degenerate tetrahedron.
    fn setup_initial_tetrahedron(&mut self) {
        let vertex_count = self.original_vertex_data.len();

        // At most 4 points: a (possibly degenerate) tetrahedron straight off.
        if vertex_count <= 4 {
            if vertex_count < 4 {
                while self.original_vertex_data.len() < 4 {
                    let last = *self.original_vertex_data.last().unwrap();
                    self.original_vertex_data.push(last);
                }
            }
            let mut v = [0usize, 1, 2, 3];
            let n = get_triangle_normal(
                self.original_vertex_data[v[0]],
                self.original_vertex_data[v[1]],
                self.original_vertex_data[v[2]],
            );
            let triangle_plane = Plane::new(n, self.original_vertex_data[v[0]]);
            if triangle_plane.is_point_on_positive_side(self.original_vertex_data[v[3]]) {
                v.swap(0, 1);
            }
            self.mesh
                .setup(v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32);
            return;
        }

        // Find the two most distant extreme points.
        let mut max_d = self.epsilon_squared;
        let mut selected_points = (0usize, 0usize);
        for i in 0..6 {
            for j in (i + 1)..6 {
                let d = get_squared_distance(
                    self.original_vertex_data[self.extreme_values[i]],
                    self.original_vertex_data[self.extreme_values[j]],
                );
                if d > max_d {
                    max_d = d;
                    selected_points = (self.extreme_values[i], self.extreme_values[j]);
                }
            }
        }
        if max_d == self.epsilon_squared {
            // The cloud looks like a single point.
            self.mesh.setup(0, 1, 2, 3);
            return;
        }
        debug_assert_ne!(selected_points.0, selected_points.1);

        // Find the point most distant from the line through the two chosen extremes.
        let r = Ray::new(
            self.original_vertex_data[selected_points.0],
            self.original_vertex_data[selected_points.1] - self.original_vertex_data[selected_points.0],
        );
        max_d = self.epsilon_squared;
        let mut max_i = usize::MAX;
        let v_count = self.original_vertex_data.len();
        for i in 0..v_count {
            let dist_to_ray =
                get_squared_distance_between_point_and_ray(self.original_vertex_data[i], &r);
            if dist_to_ray > max_d {
                max_d = dist_to_ray;
                max_i = i;
            }
        }
        if max_d == self.epsilon_squared {
            // The cloud lies on a 1D subspace: no volume. Pick four distinct indices for a
            // degenerate tetrahedron.
            let first_point = selected_points.0;
            let second_point = selected_points.1;
            let mut third_point = 0usize;
            while third_point == first_point || third_point == second_point {
                third_point += 1;
            }
            let mut fourth_point = third_point + 1;
            while fourth_point == first_point || fourth_point == second_point {
                fourth_point += 1;
            }
            self.mesh.setup(
                first_point as i32,
                second_point as i32,
                third_point as i32,
                fourth_point as i32,
            );
            return;
        }

        // These three form the base triangle of the tetrahedron.
        debug_assert!(selected_points.0 != max_i && selected_points.1 != max_i);
        let mut base_triangle = [selected_points.0, selected_points.1, max_i];
        let base_triangle_vertices = [
            self.original_vertex_data[base_triangle[0]],
            self.original_vertex_data[base_triangle[1]],
            self.original_vertex_data[base_triangle[2]],
        ];

        // The 4th vertex is the point farthest from the base-triangle plane.
        max_d = self.m_epsilon;
        max_i = 0;
        let n = get_triangle_normal(
            base_triangle_vertices[0],
            base_triangle_vertices[1],
            base_triangle_vertices[2],
        );
        let triangle_plane = Plane::new(n, base_triangle_vertices[0]);
        for i in 0..v_count {
            let d =
                get_signed_distance_to_plane(self.original_vertex_data[i], &triangle_plane).abs();
            if d > max_d {
                max_d = d;
                max_i = i;
            }
        }
        if max_d == self.m_epsilon {
            // Everything lies on a 2D subspace — add one apex so the hull has volume. `build_mesh`
            // resets the apex back onto vertex 0 afterward, collapsing the output flat.
            self.planar = true;
            let n1 = get_triangle_normal(
                base_triangle_vertices[1],
                base_triangle_vertices[2],
                base_triangle_vertices[0],
            );
            let extra_point = n1 + self.original_vertex_data[0];
            self.original_vertex_data.push(extra_point);
            max_i = self.original_vertex_data.len() - 1;
        }

        // Enforce CCW orientation.
        let tri_plane = Plane::new(n, base_triangle_vertices[0]);
        if tri_plane.is_point_on_positive_side(self.original_vertex_data[max_i]) {
            base_triangle.swap(0, 1);
        }

        // Build the tetrahedron and its per-face planes.
        self.mesh.setup(
            base_triangle[0] as i32,
            base_triangle[1] as i32,
            base_triangle[2] as i32,
            max_i as i32,
        );
        for fi in 0..self.mesh.faces.len() {
            let v = self.mesh.get_vertex_indices_of_face(&self.mesh.faces[fi]);
            let n1 = get_triangle_normal(
                self.original_vertex_data[v[0] as usize],
                self.original_vertex_data[v[1] as usize],
                self.original_vertex_data[v[2] as usize],
            );
            let plane = Plane::new(n1, self.original_vertex_data[v[0] as usize]);
            self.mesh.faces[fi].p = plane;
        }

        // Assign a face to each vertex outside the tetrahedron (`v_count` excludes any planar apex).
        for i in 0..v_count {
            for fi in 0..self.mesh.faces.len() {
                if self.add_point_to_face(fi, i) {
                    break;
                }
            }
        }
    }

    fn create_convex_halfedge_mesh(&mut self) {
        self.visible_faces.clear();
        self.horizon_edges_data.clear();
        self.possibly_visible_faces.clear();

        self.setup_initial_tetrahedron();
        debug_assert_eq!(self.mesh.faces.len(), 4);

        // Seed the face stack with faces that have points assigned.
        self.face_list.clear();
        for i in 0..4usize {
            let has_points = self.mesh.faces[i]
                .points_on_positive_side
                .as_ref()
                .is_some_and(|p| !p.is_empty());
            if has_points {
                self.face_list.push_back(i as i32);
                self.mesh.faces[i].in_face_stack = true;
            }
        }

        let mut iter: usize = 0;
        while !self.face_list.is_empty() {
            iter += 1;
            if iter == usize::MAX {
                // The visibility BFS marks visited faces with the iteration counter; max means
                // "unvisited". Reset before we collide (won't happen on 64-bit, but the C++ guards).
                iter = 0;
            }

            let top_face_index = self.face_list.pop_front().unwrap();
            let tfi = top_face_index as usize;
            self.mesh.faces[tfi].in_face_stack = false;

            if self.mesh.faces[tfi].points_on_positive_side.is_none()
                || self.mesh.faces[tfi].is_disabled()
            {
                continue;
            }

            // Extrude to the face's most distant point.
            let active_point_index = self.mesh.faces[tfi].most_distant_point;
            let active_point = self.original_vertex_data[active_point_index];

            // Find every face with the active point on its positive side (the "visible" faces), and
            // the horizon edges bounding them.
            self.horizon_edges_data.clear();
            self.possibly_visible_faces.clear();
            self.visible_faces.clear();
            self.possibly_visible_faces.push(FaceData {
                face_index: top_face_index,
                entered_from_halfedge: -1,
            });
            while let Some(face_data) = self.possibly_visible_faces.pop() {
                let pvf_idx = face_data.face_index as usize;
                debug_assert!(!self.mesh.faces[pvf_idx].is_disabled());

                if self.mesh.faces[pvf_idx].visibility_checked_on_iteration == iter {
                    if self.mesh.faces[pvf_idx].is_visible_face_on_current_iteration {
                        continue;
                    }
                    // Checked this iteration and NOT visible — fall through to horizon handling.
                } else {
                    self.mesh.faces[pvf_idx].visibility_checked_on_iteration = iter;
                    let p_n = self.mesh.faces[pvf_idx].p.n;
                    let p_d = self.mesh.faces[pvf_idx].p.d;
                    let d = p_n.dot(active_point) + p_d;
                    if d > 0.0 {
                        self.mesh.faces[pvf_idx].is_visible_face_on_current_iteration = true;
                        self.mesh.faces[pvf_idx].horizon_edges_on_current_iteration = 0;
                        self.visible_faces.push(pvf_idx);
                        let he_indices =
                            self.mesh.get_half_edge_indices_of_face(&self.mesh.faces[pvf_idx]);
                        for &he_index in he_indices.iter() {
                            let paired = self.mesh.halfedges[he_index as usize].paired_halfedge;
                            if paired != face_data.entered_from_halfedge {
                                let to_face = self.mesh.halfedge_to_face[paired as usize];
                                self.possibly_visible_faces.push(FaceData {
                                    face_index: to_face,
                                    entered_from_halfedge: he_index,
                                });
                            }
                        }
                        continue;
                    }
                    debug_assert_ne!(face_data.face_index, top_face_index);
                    // Not visible — fall through to horizon handling.
                }

                // The half-edge we came from is part of the horizon edge loop.
                self.mesh.faces[pvf_idx].is_visible_face_on_current_iteration = false;
                self.horizon_edges_data
                    .push(face_data.entered_from_halfedge as usize);
                let hef = self.mesh.halfedge_to_face[face_data.entered_from_halfedge as usize]
                    as usize;
                let half_edges_mesh = self.mesh.get_half_edge_indices_of_face(&self.mesh.faces[hef]);
                let ind: u8 = if half_edges_mesh[0] == face_data.entered_from_halfedge {
                    0
                } else if half_edges_mesh[1] == face_data.entered_from_halfedge {
                    1
                } else {
                    2
                };
                self.mesh.faces[hef].horizon_edges_on_current_iteration |= 1 << ind;
            }
            let horizon_edge_count = self.horizon_edges_data.len();

            // Order the horizon edges into a loop. On failure, drop the active point and move on
            // (accept a minor degeneration), matching the C++ recovery path bug-for-bug.
            let mut horizon = std::mem::take(&mut self.horizon_edges_data);
            let ok = self.reorder_horizon_edges(&mut horizon);
            self.horizon_edges_data = horizon;
            if !ok {
                self.failed_horizon_edges += 1;
                {
                    let points = self.mesh.faces[tfi]
                        .points_on_positive_side
                        .as_mut()
                        .unwrap();
                    let mut change_flag = 0;
                    for index in 0..points.len() {
                        if points[index] == active_point_index {
                            change_flag = 1;
                        } else if change_flag == 1 {
                            change_flag = 2;
                            points[index - 1] = points[index];
                        }
                    }
                    if change_flag == 1 {
                        let nl = points.len() - 1;
                        points.truncate(nl);
                    }
                }
                if self.mesh.faces[tfi]
                    .points_on_positive_side
                    .as_ref()
                    .unwrap()
                    .is_empty()
                {
                    let v = self.mesh.faces[tfi].points_on_positive_side.take().unwrap();
                    self.reclaim_to_index_vector_pool(v);
                }
                continue;
            }

            // Except for the horizon edges, every half-edge of a visible face can be recycled. The
            // faces are disabled too, but we keep their point vectors to reassign to the new faces.
            self.new_face_indices.clear();
            self.new_halfedge_indices.clear();
            self.disabled_face_point_vectors.clear();
            let mut disable_counter = 0usize;
            for k in 0..self.visible_faces.len() {
                let face_index = self.visible_faces[k];
                let half_edges_mesh = self.mesh.get_half_edge_indices_of_face(&self.mesh.faces[face_index]);
                let horizon_bits = self.mesh.faces[face_index].horizon_edges_on_current_iteration;
                for (j, &hem) in half_edges_mesh.iter().enumerate() {
                    if (horizon_bits & (1 << j)) == 0 {
                        if disable_counter < horizon_edge_count * 2 {
                            self.new_halfedge_indices.push(hem as usize);
                            disable_counter += 1;
                        } else {
                            self.mesh.disable_halfedge(hem as usize);
                        }
                    }
                }
                if let Some(t) = self.mesh.disable_face(face_index) {
                    debug_assert!(!t.is_empty());
                    self.disabled_face_point_vectors.push(t);
                }
            }
            if disable_counter < horizon_edge_count * 2 {
                let new_half_edges_needed = horizon_edge_count * 2 - disable_counter;
                for _ in 0..new_half_edges_needed {
                    let h = self.mesh.add_halfedge();
                    self.new_halfedge_indices.push(h);
                }
            }

            // Create the new faces around the horizon edge loop, connecting each to the active point.
            for i in 0..horizon_edge_count {
                let ab = self.horizon_edges_data[i];
                let vidx = self.mesh.get_vertex_indices_of_half_edge(&self.mesh.halfedges[ab]);
                let a = vidx[0];
                let b = vidx[1];
                let c = active_point_index as i32;

                let new_face_index = self.mesh.add_face();
                self.new_face_indices.push(new_face_index);

                let ca = self.new_halfedge_indices[2 * i];
                let bc = self.new_halfedge_indices[2 * i + 1];

                self.mesh.halfedge_next[ab] = bc as i32;
                self.mesh.halfedge_next[bc] = ca as i32;
                self.mesh.halfedge_next[ca] = ab as i32;

                self.mesh.halfedge_to_face[bc] = new_face_index as i32;
                self.mesh.halfedge_to_face[ca] = new_face_index as i32;
                self.mesh.halfedge_to_face[ab] = new_face_index as i32;

                self.mesh.halfedges[ca].end_vert = a;
                self.mesh.halfedges[bc].end_vert = c;

                let plane_normal = get_triangle_normal(
                    self.original_vertex_data[a as usize],
                    self.original_vertex_data[b as usize],
                    active_point,
                );
                self.mesh.faces[new_face_index].p = Plane::new(plane_normal, active_point);
                self.mesh.faces[new_face_index].he = ab as i32;

                self.mesh.halfedges[ca].paired_halfedge = self.new_halfedge_indices
                    [if i > 0 { i * 2 - 1 } else { 2 * horizon_edge_count - 1 }]
                    as i32;
                self.mesh.halfedges[bc].paired_halfedge =
                    self.new_halfedge_indices[((i + 1) * 2) % (horizon_edge_count * 2)] as i32;
            }

            // Reassign the disabled faces' points to the new faces, then recycle the vectors.
            let disabled = std::mem::take(&mut self.disabled_face_point_vectors);
            for disabled_points in disabled {
                debug_assert!(!disabled_points.is_empty());
                for &point in disabled_points.iter() {
                    if point == active_point_index {
                        continue;
                    }
                    for j in 0..horizon_edge_count {
                        let target = self.new_face_indices[j];
                        if self.add_point_to_face(target, point) {
                            break;
                        }
                    }
                }
                self.reclaim_to_index_vector_pool(disabled_points);
            }

            // Push any new face that has points onto the stack.
            for k in 0..self.new_face_indices.len() {
                let nfi = self.new_face_indices[k];
                if self.mesh.faces[nfi].points_on_positive_side.is_some() {
                    debug_assert!(!self.mesh.faces[nfi]
                        .points_on_positive_side
                        .as_ref()
                        .unwrap()
                        .is_empty());
                    if !self.mesh.faces[nfi].in_face_stack {
                        self.face_list.push_back(nfi as i32);
                        self.mesh.faces[nfi].in_face_stack = true;
                    }
                }
            }
        }

        self.index_vector_pool.clear();
    }

    /// Compute the hull and return `(halfedges, vertices)` — the spine half-edge vector (triangle
    /// order, `end` derived) and the compacted vertex positions.
    pub fn build_mesh(&mut self, epsilon: f64) -> (Vec<Halfedge>, Vec<Vec3>) {
        if self.original_vertex_data.is_empty() {
            return (Vec::new(), Vec::new());
        }

        // Scale-dependent epsilon.
        self.extreme_values = self.get_extreme_values();
        self.scale = self.get_scale(self.extreme_values);
        self.m_epsilon = epsilon * self.scale;
        self.epsilon_squared = self.m_epsilon * self.m_epsilon;

        self.planar = false;
        self.create_convex_halfedge_mesh();

        if self.planar {
            // The apex was added only to give the hull volume; reset it onto vertex 0 so the output
            // coordinates are correct (the `extraPointIndex` the C++ computes here is unused).
            let n = self.original_vertex_data.len();
            self.original_vertex_data[n - 1] = self.original_vertex_data[0];
        }

        // --- Reorder + compact (C++ parallel section, ported serial; see module doc). ---
        // `halfedge_to_face`/`face_map` from the C++ are dead in the return path, so they're dropped.
        let nhe = self.mesh.halfedges.len();
        let mut out: Vec<QHalfedge> = vec![QHalfedge::default(); nhe];
        let mut mapping: Vec<i32> = vec![0; nhe];
        let mut face_counts: Vec<i32> = vec![0; self.mesh.faces.len()];
        let mut j: usize = 0;
        for i in 0..nhe {
            if self.mesh.halfedges[i].paired_halfedge < 0 {
                continue;
            }
            let face = self.mesh.halfedge_to_face[i] as usize;
            if self.mesh.faces[face].is_disabled() {
                continue;
            }
            // First half-edge of each live face claims the triple; the rest of the face is skipped.
            let prev = face_counts[face];
            face_counts[face] += 1;
            if prev > 0 {
                continue;
            }
            let curr_index = j;
            j += 3;
            mapping[i] = curr_index as i32;
            out[curr_index] = self.mesh.halfedges[i];

            let k1 = self.mesh.halfedge_next[i] as usize;
            mapping[k1] = (curr_index + 1) as i32;
            out[curr_index + 1] = self.mesh.halfedges[k1];

            let k2 = self.mesh.halfedge_next[k1] as usize;
            mapping[k2] = (curr_index + 2) as i32;
            out[curr_index + 2] = self.mesh.halfedges[k2];

            out[curr_index].start_vert = out[curr_index + 2].end_vert;
            out[curr_index + 1].start_vert = out[curr_index].end_vert;
            out[curr_index + 2].start_vert = out[curr_index + 1].end_vert;
        }
        out.truncate(j);
        for he in out.iter_mut() {
            he.paired_halfedge = mapping[he.paired_halfedge as usize];
        }

        // Remove unused vertices: count references, exclusive-scan to a compaction map, remap.
        let orig_len = self.original_vertex_data.len();
        let mut counts: Vec<i32> = vec![0; orig_len + 1];
        let ntri = out.len() / 3;
        for t in 0..ntri {
            counts[out[3 * t].start_vert as usize] += 1;
            counts[out[3 * t + 1].start_vert as usize] += 1;
            counts[out[3 * t + 2].start_vert as usize] += 1;
        }
        let mut acc = 0i32;
        for c in counts.iter_mut() {
            let sat = if *c > 0 { 1 } else { 0 };
            *c = acc;
            acc += sat;
        }
        let out_vert_count = counts[counts.len() - 1] as usize;
        let mut vertices: Vec<Vec3> = vec![Vec3::ZERO; out_vert_count];
        for i in 0..orig_len {
            if counts[i + 1] - counts[i] > 0 {
                vertices[counts[i] as usize] = self.original_vertex_data[i];
            }
        }
        for he in out.iter_mut() {
            he.start_vert = counts[he.start_vert as usize];
            he.end_vert = counts[he.end_vert as usize];
        }

        // C++ `Halfedges(std::move(...))`: copy start/paired/prop, DROP end (spine derives it).
        // prop_vert == start_vert in the positions-only (num_prop == 3) model.
        let halfedge: Vec<Halfedge> = out
            .iter()
            .map(|q| Halfedge {
                start_vert: VertId::new(q.start_vert),
                paired_halfedge: HalfedgeId::new(q.paired_halfedge),
                prop_vert: VertId::new(q.start_vert),
            })
            .collect();
        (halfedge, vertices)
    }

    /// How many horizon-edge reorderings failed (numerical instability). Test/diagnostic hook.
    pub fn failed_horizon_edges(&self) -> usize {
        self.failed_horizon_edges
    }
}

impl Mesh {
    /// The convex hull of a 3D point cloud (Manifold's `Manifold::Hull(points)` / `Impl::Hull`).
    /// Positions-only (`num_prop == 3`). An empty cloud yields the empty mesh.
    pub fn hull_of_points(points: &[Vec3]) -> Result<Mesh, Error> {
        if points.is_empty() {
            return Ok(Mesh {
                num_prop: 3,
                ..Default::default()
            });
        }
        let mut qh = QuickHull::new(points);
        let (halfedge, vert_pos) = qh.build_mesh(default_eps());
        let mut mesh = Mesh {
            vert_pos,
            halfedge,
            num_prop: 3,
            ..Default::default()
        };
        // C++ Hull order: CalculateBBox, SetEpsilon, InitializeOriginal, SortGeometry,
        // SetNormalsAndCoplanar. All are no-op-safe on the empty mesh a fully-degenerate cloud could
        // produce.
        mesh.calculate_bbox();
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.sort_geometry();
        mesh.set_normals_and_coplanar();
        Ok(mesh)
    }

    /// The convex hull of this mesh's own vertices (Manifold's `Manifold::Hull()`).
    pub fn hull(&self) -> Result<Mesh, Error> {
        Mesh::hull_of_points(&self.vert_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_corners() -> Vec<Vec3> {
        let mut v = Vec::new();
        for &x in &[0.0, 1.0] {
            for &y in &[0.0, 1.0] {
                for &z in &[0.0, 1.0] {
                    v.push(Vec3::new(x, y, z));
                }
            }
        }
        v
    }

    #[test]
    fn hull_of_a_cube_is_the_cube() {
        let hull = Mesh::hull_of_points(&cube_corners()).unwrap();
        // A cube's hull: all 8 corners kept, 12 triangles (V - E + F = 8 - 18 + 12 = 2, genus 0).
        assert_eq!(hull.num_vert(), 8, "cube hull should keep all 8 corners");
        assert_eq!(hull.num_tri(), 12, "cube hull should be 12 triangles");
        assert!(hull.is_manifold(), "cube hull must be a closed manifold");
    }

    #[test]
    fn interior_points_are_ignored() {
        let mut pts = cube_corners();
        // Points strictly inside the cube must not appear on the hull.
        pts.push(Vec3::new(0.5, 0.5, 0.5));
        pts.push(Vec3::new(0.25, 0.75, 0.5));
        let hull = Mesh::hull_of_points(&pts).unwrap();
        assert_eq!(hull.num_vert(), 8, "interior points must be dropped");
        assert_eq!(hull.num_tri(), 12);
        assert!(hull.is_manifold());
    }

    #[test]
    fn hull_of_a_tetrahedron() {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let hull = Mesh::hull_of_points(&pts).unwrap();
        assert_eq!(hull.num_vert(), 4);
        assert_eq!(hull.num_tri(), 4);
        assert!(hull.is_manifold());
    }

    #[test]
    fn empty_cloud_is_empty_hull() {
        let hull = Mesh::hull_of_points(&[]).unwrap();
        assert!(hull.is_empty());
        assert_eq!(hull.num_tri(), 0);
    }

    #[test]
    fn hull_method_matches_point_cloud() {
        // Mesh::hull() delegates to hull_of_points over vert_pos.
        let mut m = Mesh {
            vert_pos: cube_corners(),
            num_prop: 3,
            ..Default::default()
        };
        m.calculate_bbox();
        let hull = m.hull().unwrap();
        assert_eq!(hull.num_vert(), 8);
        assert!(hull.is_manifold());
    }
}
