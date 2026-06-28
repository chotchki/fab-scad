//! Minimal binary-STL reader → triangle soup, so the GUI can show a `fab`-rendered mesh
//! without a third-party loader (bevy_stl lags Bevy by a version). OpenSCAD exports binary
//! STL: an 80-byte header, a u32 triangle count, then 50 bytes per triangle (a face normal
//! + three vertices + a 2-byte attribute).

use std::path::Path;

use anyhow::{ensure, Context, Result};

/// Per-vertex positions and normals (3 vertices per triangle, flat).
pub struct StlMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

pub fn load_binary_stl(path: &Path) -> Result<StlMesh> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(bytes.len() >= 84, "STL too short to hold a header + count");
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;

    let f = |at: usize| f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut positions = Vec::with_capacity(count * 3);
    let mut normals = Vec::with_capacity(count * 3);
    let mut off = 84;
    for _ in 0..count {
        ensure!(off + 50 <= bytes.len(), "STL truncated mid-triangle");
        let n = [f(off), f(off + 4), f(off + 8)];
        for v in 0..3 {
            let p = off + 12 + v * 12;
            positions.push([f(p), f(p + 4), f(p + 8)]);
            normals.push(n);
        }
        off += 50;
    }
    Ok(StlMesh { positions, normals })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_one_triangle_binary_stl() {
        let mut b = vec![0u8; 80]; // header
        b.extend_from_slice(&1u32.to_le_bytes()); // triangle count
        let mut push = |v: [f32; 3]| {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        };
        push([0.0, 0.0, 1.0]); // normal
        push([1.0, 0.0, 0.0]); // v0
        push([0.0, 1.0, 0.0]); // v1
        push([0.0, 0.0, 2.0]); // v2
        b.extend_from_slice(&0u16.to_le_bytes()); // attribute bytes

        let path = std::env::temp_dir().join(format!("fab-gui-stl-{}.stl", std::process::id()));
        std::fs::write(&path, &b).unwrap();
        let m = load_binary_stl(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.positions[0], [1.0, 0.0, 0.0]);
        assert_eq!(m.positions[2], [0.0, 0.0, 2.0]);
        assert_eq!(m.normals, vec![[0.0, 0.0, 1.0]; 3]); // face normal repeated per vertex
    }
}
