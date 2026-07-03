//! STL bytes → triangle soup, binary-vs-ASCII detected by the binary size formula
//! (`84 + 50*count`). LIFTED from gui/src/stl.rs (the bytes half — no disk path here);
//! unify into a shared crate at A.5 — until then keep both in sync.

use anyhow::{ensure, Context, Result};

/// Per-vertex positions and normals (3 vertices per triangle, flat).
pub struct StlMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// Parse STL bytes in memory — the browser upload path (File → bytes → mesh, no disk).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_stl_bytes() {
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
        let m = load_stl_bytes(&b).unwrap();
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.positions[2], [0.0, 0.0, 2.0]);
        assert_eq!(m.normals, vec![[0.0, 0.0, 1.0]; 3]);
    }

    #[test]
    fn parses_ascii_stl_bytes() {
        let text = "solid x\n\
            facet normal 0 0 1\n outer loop\n\
            vertex 1 0 0\n vertex 0 1 0\n vertex 0 0 2\n\
            endloop\n endfacet\nendsolid x\n";
        let m = load_stl_bytes(text.as_bytes()).unwrap();
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.positions[0], [1.0, 0.0, 0.0]);
        assert_eq!(m.normals, vec![[0.0, 0.0, 1.0]; 3]);
    }
}
