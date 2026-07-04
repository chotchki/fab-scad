//! Minimal STL reader → triangle soup (positions + normals, 3 verts/tri, flat) — shared by the
//! desktop GUI and fab-web so the parser exists ONCE (A.5). Handles BOTH formats: OpenSCAD
//! emits ASCII (`solid …`), everything else mostly binary; detected by the binary size formula.
//! Pure std — no bevy, no feature gate; `load_stl` (path) simply fails at runtime on wasm.

use std::path::Path;

use anyhow::{ensure, Context, Result};

/// Per-vertex positions and normals (3 vertices per triangle, flat).
pub struct StlMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// Load an STL from disk, detecting binary vs ASCII by the binary size formula (`84 + 50*count`).
pub fn load_stl(path: &Path) -> Result<StlMesh> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    load_stl_bytes(&bytes)
}

/// Parse STL bytes in memory (same binary-vs-ASCII detection as `load_stl`) — lets the Manifold
/// kernel path turn a `Solid`'s `to_stl_bytes()` straight into a mesh, no disk round-trip.
pub fn load_stl_bytes(bytes: &[u8]) -> Result<StlMesh> {
    ensure!(bytes.len() >= 84, "STL too short");
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if bytes.len() == 84 + 50 * count {
        parse_binary(bytes, count)
    } else {
        parse_ascii(&String::from_utf8_lossy(bytes))
    }
}

fn parse_binary(bytes: &[u8], count: usize) -> Result<StlMesh> {
    let f =
        |at: usize| f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut positions = Vec::with_capacity(count * 3);
    let mut normals = Vec::with_capacity(count * 3);
    let mut off = 84;
    for _ in 0..count {
        ensure!(off + 50 <= bytes.len(), "binary STL truncated mid-triangle");
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

fn parse_ascii(text: &str) -> Result<StlMesh> {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut normal = [0.0f32; 3];
    let mut tok = text.split_whitespace();
    let three = |tok: &mut std::str::SplitWhitespace| -> Result<[f32; 3]> {
        Ok([read(tok)?, read(tok)?, read(tok)?])
    };
    while let Some(t) = tok.next() {
        match t {
            "facet" => {
                if tok.next() == Some("normal") {
                    normal = three(&mut tok)?;
                }
            }
            "vertex" => {
                positions.push(three(&mut tok)?);
                normals.push(normal);
            }
            _ => {}
        }
    }
    ensure!(
        !positions.is_empty() && positions.len() % 3 == 0,
        "ASCII STL: no triangles, or vertex count not a multiple of 3"
    );
    Ok(StlMesh { positions, normals })
}

fn read(tok: &mut std::str::SplitWhitespace) -> Result<f32> {
    tok.next()
        .context("STL: unexpected end of file")?
        .parse::<f32>()
        .context("STL: bad float")
}

/// Indexed mesh → binary STL bytes (per-face normals from winding). The display-fallback
/// twin of the kernel's `to_stl_bytes` for meshes that DON'T weld — viewing needs no manifold.
pub fn binary_from_indexed(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + 50 * tris.len());
    out.extend_from_slice(&[0u8; 80]);
    out.extend_from_slice(&(tris.len() as u32).to_le_bytes());
    for t in tris {
        let p: Vec<[f32; 3]> = t
            .iter()
            .map(|&i| {
                let v = verts[i as usize];
                [v[0] as f32, v[1] as f32, v[2] as f32]
            })
            .collect();
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let w = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let mut n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l > 0.0 {
            for c in &mut n {
                *c /= l;
            }
        }
        for v in std::iter::once(&n).chain(p.iter()) {
            for c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out.extend_from_slice(&[0u8; 2]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_stl() {
        let mut b = vec![0u8; 80];
        b.extend_from_slice(&1u32.to_le_bytes());
        let mut push = |v: [f32; 3]| {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        };
        push([0.0, 0.0, 1.0]); // normal
        push([1.0, 0.0, 0.0]);
        push([0.0, 1.0, 0.0]);
        push([0.0, 0.0, 2.0]);
        b.extend_from_slice(&0u16.to_le_bytes());
        let path = std::env::temp_dir().join(format!("fab-gui-bin-{}.stl", std::process::id()));
        std::fs::write(&path, &b).unwrap();
        let m = load_stl(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.positions[2], [0.0, 0.0, 2.0]);
        assert_eq!(m.normals, vec![[0.0, 0.0, 1.0]; 3]);
    }

    #[test]
    fn parses_ascii_stl() {
        let text = "solid x\n\
            facet normal 0 0 1\n outer loop\n\
            vertex 1 0 0\n vertex 0 1 0\n vertex 0 0 2\n\
            endloop\n endfacet\nendsolid x\n";
        let m = parse_ascii(text).unwrap();
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.positions[0], [1.0, 0.0, 0.0]);
        assert_eq!(m.positions[2], [0.0, 0.0, 2.0]);
        assert_eq!(m.normals, vec![[0.0, 0.0, 1.0]; 3]);
    }
}
