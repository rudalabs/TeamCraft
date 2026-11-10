//! STL, ASCII and binary.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use super::*;

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    if let Ok(n) = u32_le(data, 80).map(|n| n as usize) {
        if n.checked_mul(50).and_then(|b| b.checked_add(84)) == Some(data.len()) {
            if n == 0 {
                bail!("The STL file contains no triangles.");
            }
            check_triangles(n)?;
            let mut w = Welder::new(n);
            for t in 0..n {
                let o = 84 + t * 50 + 12;
                for v in 0..3 {
                    let at = o + v * 12;
                    let f = |k: usize| f32_le(data, at + k * 4);
                    w.push([f(0)?, f(1)?, f(2)?]);
                }
            }
            return Ok(w.mesh);
        }
    }
    let text = text(data);
    if !text.trim_start().starts_with("solid") {
        bail!("Not an STL file (neither binary nor ASCII).");
    }
    let mut w = Welder::new(data.len() / 200);
    let mut tokens = text.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        if token == "vertex" {
            let mut p = [0f32; 3];
            for c in &mut p {
                *c = float(tokens.next().context("incomplete vertex")?)? as f32;
            }
            w.push(p);
            check_triangles(w.mesh.indices.len() / 3)?;
        }
    }
    let len = w.mesh.indices.len();
    w.mesh.indices.truncate(len - len % 3);
    if w.mesh.indices.is_empty() {
        bail!("The STL file contains no triangles.");
    }
    Ok(w.mesh)
}

/// Welds identical vertices of a triangle soup (STL).
struct Welder {
    map: HashMap<[u32; 3], u32>,
    mesh: Mesh,
}

impl Welder {
    fn new(triangles: usize) -> Welder {
        Welder {
            map: HashMap::with_capacity(triangles / 2),
            mesh: Mesh {
                parts: 1,
                indices: Vec::with_capacity(triangles * 3),
                ..Mesh::default()
            },
        }
    }

    fn push(&mut self, p: [f32; 3]) {
        let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
        let next = self.mesh.vertex_count() as u32;
        let index = *self.map.entry(key).or_insert_with(|| {
            self.mesh.positions.extend(p);
            next
        });
        self.mesh.indices.push(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::test_support::cube_stl_ascii;

    #[test]
    fn stl_ascii_and_binary_are_welded() {
        let mesh = crate::mesh::read("stl", cube_stl_ascii().as_bytes()).unwrap();
        assert_eq!(mesh.triangle_count(), 4);
        assert_eq!(mesh.vertex_count(), 8, "shared corners are welded");

        let mut bin = vec![0u8; 80];
        bin.extend_from_slice(&2u32.to_le_bytes());
        for tri in [
            [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            [[1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
        ] {
            bin.extend_from_slice(&[0u8; 12]);
            for p in tri {
                for c in p {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            bin.extend_from_slice(&[0, 0]);
        }
        let mesh = crate::mesh::read("stl", &bin).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (2, 4));
        assert!(crate::mesh::read("stl", b"garbage").is_err());
        let mut empty = vec![0u8; 80];
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert!(crate::mesh::read("stl", &empty).is_err());
    }

    #[test]
    fn stl_edge_cases() {
        // binary with the size of zero triangles
        let mut empty = vec![0u8; 80];
        empty.extend_from_slice(&0u32.to_le_bytes());
        let err = read(&empty).unwrap_err().to_string();
        assert!(err.contains("no triangles"), "{err}");
        // ASCII with a vertex that has only two coordinates
        let err = read(b"solid x\nfacet\nouter loop\nvertex 0 0").unwrap_err();
        assert!(format!("{err:#}").contains("incomplete vertex"), "{err:#}");
        // a binary file whose 80-byte header starts with "solid" is still binary
        let mut bin = b"solid but binary".to_vec();
        bin.resize(80, b' ');
        bin.extend_from_slice(&1u32.to_le_bytes());
        bin.extend_from_slice(&[0u8; 12]);
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
        bin.extend_from_slice(&[0, 0]);
        assert_eq!(read(&bin).unwrap().triangle_count(), 1);
        // the welded cube from the shared fixture is one part
        let mesh = read(cube_stl_ascii().as_bytes()).unwrap();
        assert_eq!(mesh.parts, 1);
    }
}
