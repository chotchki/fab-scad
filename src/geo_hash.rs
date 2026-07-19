//! Structural content hashing for [`GeoNode`]/[`Shape2D`] subtrees — shared by the BU.7
//! redundancy probe ([`crate::geo_redundancy`]) and the P.2 kernel-level Solid memo
//! ([`crate::backend`]). f64s hash by BITS (`+0 != -0`, NaN == its own bits), children memoize by
//! node address (valid for one tree's lifetime — O(tree) total). 64 bits BUCKET; the memo verifies
//! every hit with a deep `PartialEq` compare, so a collision can cost a re-render, never a wrong
//! mesh.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use fab_lang::{ExtrudeKind, GeoNode, Shape2D};

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

pub(crate) struct FnvHasher(pub(crate) u64);
impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        fnv(&mut self.0, bytes);
    }
}

pub(crate) fn hash_node(node: &GeoNode, memo: &mut BTreeMap<usize, u64>) -> u64 {
    let key = std::ptr::from_ref(node) as usize;
    if let Some(&h) = memo.get(&key) {
        return h;
    }
    let mut h = FnvHasher(0xcbf2_9ce4_8422_2325);
    hash_node_into(node, memo, &mut h);
    let out = h.finish();
    memo.insert(key, out);
    out
}

fn hash_node_into(node: &GeoNode, memo: &mut BTreeMap<usize, u64>, h: &mut FnvHasher) {
    std::mem::discriminant(node).hash(h);
    match node {
        GeoNode::Empty => {}
        GeoNode::Leaf(mesh) => {
            for v in &mesh.verts {
                for c in [v.x, v.y, v.z] {
                    h.write(&c.to_bits().to_le_bytes());
                }
            }
            for t in &mesh.tris {
                for i in t.0 {
                    h.write(&u64::from(i).to_le_bytes());
                }
            }
        }
        GeoNode::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node(child, memo).to_le_bytes());
        }
        GeoNode::Resize {
            newsize,
            auto,
            child,
        } => {
            for c in newsize {
                h.write(&c.to_bits().to_le_bytes());
            }
            for a in auto {
                h.write(&[u8::from(*a)]);
            }
            h.write(&hash_node(child, memo).to_le_bytes());
        }
        GeoNode::Union(kids)
        | GeoNode::Difference(kids)
        | GeoNode::Intersection(kids)
        | GeoNode::Hull(kids)
        | GeoNode::Minkowski(kids) => {
            for k in kids {
                h.write(&hash_node(k, memo).to_le_bytes());
            }
        }
        GeoNode::Extrude { kind, child } => {
            match kind {
                ExtrudeKind::Linear {
                    height,
                    twist,
                    scale,
                    slices,
                    facets,
                    center,
                } => {
                    h.write(&height.to_bits().to_le_bytes());
                    h.write(&twist.to_bits().to_le_bytes());
                    h.write(&scale[0].to_bits().to_le_bytes());
                    h.write(&scale[1].to_bits().to_le_bytes());
                    h.write(&u64::from(*slices).to_le_bytes());
                    h.write(&u64::from(*facets).to_le_bytes());
                    h.write(&[u8::from(*center)]);
                }
                ExtrudeKind::Rotate { angle, segments } => {
                    h.write(&angle.to_bits().to_le_bytes());
                    h.write(&u64::from(*segments).to_le_bytes());
                }
            }
            hash_shape_into(child, h);
        }
        GeoNode::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node(child, memo).to_le_bytes());
        }
    }
}

// ── 128-bit variant (X.1: the PERSISTENT cross-render Solid cache) ──────────────────────────────────
//
// The per-build P.2 memo verifies every 64-bit hit with a deep `PartialEq` (a collision costs a re-render,
// never a wrong mesh). A cross-BUILD cache can't keep that verifier — the borrowed `&GeoNode` is gone once
// the build ends, and an OWNED clone would duplicate every Leaf mesh (GeoNode is deep-owned, not Rc). So
// X.1 keys on a 128-bit FNV-1a and drops the deep-eq: at a bounded cache's scale the collision probability
// is ~1e-27 (far below a hardware bitflip), and `FAB_GEO_CACHE=verify` still re-renders + bitwise-compares
// every hit as the deterministic gate. Same tree walk as the 64-bit hash, widened.

const FNV128_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

pub(crate) struct Fnv128(pub(crate) u128);
impl Hasher for Fnv128 {
    /// Unused — the 128-bit value is read from `.0` directly; this satisfies the `Hasher` bound so
    /// `Discriminant::hash` can feed the variant tag, matching the 64-bit walk.
    fn finish(&self) -> u64 {
        self.0 as u64
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(FNV128_PRIME);
        }
    }
}

/// 128-bit structural hash of a [`GeoNode`] subtree — the X.1 persistent-cache key. Memoized by node
/// address (per tree lifetime) so hashing every node stays O(tree), not O(tree²).
pub(crate) fn hash_node_128(node: &GeoNode, memo: &mut BTreeMap<usize, u128>) -> u128 {
    let key = std::ptr::from_ref(node) as usize;
    if let Some(&h) = memo.get(&key) {
        return h;
    }
    let mut h = Fnv128(FNV128_OFFSET);
    hash_node_128_into(node, memo, &mut h);
    let out = h.0;
    memo.insert(key, out);
    out
}

fn hash_node_128_into(node: &GeoNode, memo: &mut BTreeMap<usize, u128>, h: &mut Fnv128) {
    std::mem::discriminant(node).hash(h);
    match node {
        GeoNode::Empty => {}
        GeoNode::Leaf(mesh) => {
            for v in &mesh.verts {
                for c in [v.x, v.y, v.z] {
                    h.write(&c.to_bits().to_le_bytes());
                }
            }
            for t in &mesh.tris {
                for i in t.0 {
                    h.write(&u64::from(i).to_le_bytes());
                }
            }
        }
        GeoNode::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node_128(child, memo).to_le_bytes());
        }
        GeoNode::Resize {
            newsize,
            auto,
            child,
        } => {
            for c in newsize {
                h.write(&c.to_bits().to_le_bytes());
            }
            for a in auto {
                h.write(&[u8::from(*a)]);
            }
            h.write(&hash_node_128(child, memo).to_le_bytes());
        }
        GeoNode::Union(kids)
        | GeoNode::Difference(kids)
        | GeoNode::Intersection(kids)
        | GeoNode::Hull(kids)
        | GeoNode::Minkowski(kids) => {
            for k in kids {
                h.write(&hash_node_128(k, memo).to_le_bytes());
            }
        }
        GeoNode::Extrude { kind, child } => {
            match kind {
                ExtrudeKind::Linear {
                    height,
                    twist,
                    scale,
                    slices,
                    facets,
                    center,
                } => {
                    h.write(&height.to_bits().to_le_bytes());
                    h.write(&twist.to_bits().to_le_bytes());
                    h.write(&scale[0].to_bits().to_le_bytes());
                    h.write(&scale[1].to_bits().to_le_bytes());
                    h.write(&u64::from(*slices).to_le_bytes());
                    h.write(&u64::from(*facets).to_le_bytes());
                    h.write(&[u8::from(*center)]);
                }
                ExtrudeKind::Rotate { angle, segments } => {
                    h.write(&angle.to_bits().to_le_bytes());
                    h.write(&u64::from(*segments).to_le_bytes());
                }
            }
            hash_shape_128_into(child, h);
        }
        GeoNode::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            h.write(&hash_node_128(child, memo).to_le_bytes());
        }
    }
}

fn hash_shape_128_into(shape: &Shape2D, h: &mut Fnv128) {
    std::mem::discriminant(shape).hash(h);
    match shape {
        Shape2D::Empty => {}
        Shape2D::Polygon(contours) => {
            for c in contours {
                for p in c {
                    h.write(&p.x.to_bits().to_le_bytes());
                    h.write(&p.y.to_bits().to_le_bytes());
                }
            }
        }
        Shape2D::Union(kids) | Shape2D::Difference(kids) | Shape2D::Intersection(kids) => {
            for k in kids {
                hash_shape_128_into(k, h);
            }
        }
        Shape2D::Offset {
            delta,
            join,
            segments,
            child,
        } => {
            h.write(&delta.to_bits().to_le_bytes());
            std::mem::discriminant(join).hash(h);
            h.write(&u64::from(*segments).to_le_bytes());
            hash_shape_128_into(child, h);
        }
        Shape2D::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_128_into(child, h);
        }
        Shape2D::Projection { cut, child } => {
            h.write(&[u8::from(*cut)]);
            let mut memo = BTreeMap::new();
            h.write(&hash_node_128(child, &mut memo).to_le_bytes());
        }
        Shape2D::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_128_into(child, h);
        }
    }
}

/// 2D subtrees hash inline (no memo — profiles are small relative to 3D meshes).
fn hash_shape_into(shape: &Shape2D, h: &mut FnvHasher) {
    std::mem::discriminant(shape).hash(h);
    match shape {
        Shape2D::Empty => {}
        Shape2D::Polygon(contours) => {
            for c in contours {
                for p in c {
                    h.write(&p.x.to_bits().to_le_bytes());
                    h.write(&p.y.to_bits().to_le_bytes());
                }
            }
        }
        Shape2D::Union(kids) | Shape2D::Difference(kids) | Shape2D::Intersection(kids) => {
            for k in kids {
                hash_shape_into(k, h);
            }
        }
        Shape2D::Offset {
            delta,
            join,
            segments,
            child,
        } => {
            h.write(&delta.to_bits().to_le_bytes());
            std::mem::discriminant(join).hash(h);
            h.write(&u64::from(*segments).to_le_bytes());
            hash_shape_into(child, h);
        }
        Shape2D::Transform { matrix, child } => {
            for c in matrix.0 {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_into(child, h);
        }
        Shape2D::Projection { cut, child } => {
            h.write(&[u8::from(*cut)]);
            // A 3D child under a 2D projection: hash with a local memo (rare node, small subtree).
            let mut memo = BTreeMap::new();
            h.write(&hash_node(child, &mut memo).to_le_bytes());
        }
        Shape2D::Color { color, child } => {
            for c in [color.r, color.g, color.b, color.a] {
                h.write(&c.to_bits().to_le_bytes());
            }
            hash_shape_into(child, h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hash_node_128;
    use fab_lang::GeoNode;
    use std::collections::BTreeMap;

    #[test]
    fn hash_128_is_deterministic_and_content_sensitive() {
        let a = GeoNode::Union(vec![GeoNode::Empty]);
        let b = GeoNode::Union(vec![GeoNode::Empty]); // same content, different address
        assert_eq!(
            hash_node_128(&a, &mut BTreeMap::new()),
            hash_node_128(&b, &mut BTreeMap::new()),
            "identical subtrees must hash equal"
        );
        // Same children, different OP → different key (the discriminant tag).
        let c = GeoNode::Difference(vec![GeoNode::Empty]);
        assert_ne!(
            hash_node_128(&a, &mut BTreeMap::new()),
            hash_node_128(&c, &mut BTreeMap::new()),
            "union vs difference must not collide"
        );
    }
}
