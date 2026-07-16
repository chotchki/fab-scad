//! Byte-fingerprinting for the golden-mode gates — FNV-1a over canonical output bytes (M.6.3; the
//! vocabulary M.7's frozen `oracle_goldens` flip stands on). Every float enters via `to_bits`, so a
//! 1-ULP drift anywhere flips the fingerprint: matching goldens across serial / `par` / wasm IS the
//! bit-identity proof, not an approximation of it.

use crate::cross_section::CrossSection;
use crate::mesh::Mesh;
use crate::mesh_ids::TriId;

/// FNV-1a accumulator.
#[derive(Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    /// Fold bytes into the hash.
    pub fn eat(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    /// Fold one f64, bitwise.
    pub fn eat_f64(&mut self, v: f64) {
        self.eat(&v.to_bits().to_le_bytes());
    }
    /// The digest.
    pub fn digest(self) -> u64 {
        self.0
    }
}

/// Fingerprint a mesh's canonical output bytes: vertex positions, per-corner start-verts, then the
/// property stride + rows (the K.D shape — `sort_geometry` canonicalizes order, so the bytes are
/// stable given the same numeric results).
pub fn mesh(m: &Mesh) -> u64 {
    let mut h = Fnv::default();
    for p in &m.vert_pos {
        h.eat_f64(p.x);
        h.eat_f64(p.y);
        h.eat_f64(p.z);
    }
    for tri in 0..m.num_tri() {
        let t = TriId::from_usize(tri);
        for i in 0..3 {
            h.eat(&m.start(t.halfedge(i)).raw().to_le_bytes());
        }
    }
    h.eat(&(m.num_prop as u64).to_le_bytes());
    for &v in &m.properties {
        h.eat_f64(v);
    }
    h.digest()
}

/// Fingerprint a 2D region: per-contour length then vertex bits, in stored order (i_overlay's
/// integer-grid output is deterministic, so the order is stable).
pub fn cross_section(cs: &CrossSection) -> u64 {
    let mut h = Fnv::default();
    for c in cs.contours() {
        h.eat(&(c.len() as u64).to_le_bytes());
        for p in c {
            h.eat_f64(p.x);
            h.eat_f64(p.y);
        }
    }
    h.digest()
}

/// Fingerprint a value stream — for the raw `mathf` sweeps.
pub fn f64s(vals: impl IntoIterator<Item = f64>) -> u64 {
    let mut h = Fnv::default();
    for v in vals {
        h.eat_f64(v);
    }
    h.digest()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_bit_sensitive() {
        let a = f64s([1.0, 2.0, 3.0]);
        assert_eq!(a, f64s([1.0, 2.0, 3.0]), "deterministic");
        assert_ne!(
            a,
            f64s([1.0, 2.0, f64::from_bits(3.0_f64.to_bits() + 1)]),
            "1-ULP flips it"
        );
        assert_ne!(a, f64s([1.0, 3.0, 2.0]), "order-sensitive");
        // ±0 differ bitwise — exactly the sensitivity a byte gate wants.
        assert_ne!(f64s([0.0]), f64s([-0.0]));
    }
}
