//! 3DS (legacy 3ds Max).

use anyhow::{bail, Result};

use super::*;

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    if data.len() < 6 || u16_le(data, 0).ok() != Some(0x4D4D) {
        bail!("Not a 3DS file.");
    }
    let mut out = Mesh::default();
    three_ds_chunks(data, 6, data.len(), &mut out, &mut None, 0)?;
    Ok(out)
}

fn three_ds_chunks(
    data: &[u8],
    mut at: usize,
    end: usize,
    out: &mut Mesh,
    object: &mut Option<Mesh>,
    depth: usize,
) -> Result<()> {
    if depth > 8 {
        bail!("3DS chunks nest too deeply");
    }
    while at + 6 <= end {
        let (Ok(id), Ok(len)) = (u16_le(data, at), u32_le(data, at + 2)) else {
            break; // truncated
        };
        let len = len as usize;
        if len < 6 || at + len > end {
            break; // truncated or padding
        }
        let body = at + 6;
        let chunk_end = at + len;
        // the u16 count at the start of a vertex or face list; a list chunk
        // too short for it is skipped like a truncated chunk
        let count = (body + 2 <= chunk_end)
            .then(|| u16_le(data, body).ok())
            .flatten()
            .map(usize::from);
        match id {
            // main → editor → object → triangle mesh: descend
            0x3D3D => three_ds_chunks(data, body, chunk_end, out, object, depth + 1)?,
            0x4000 => {
                // object block: zero-terminated name, then sub-chunks
                let name_end = range(data, body, chunk_end)?
                    .iter()
                    .position(|b| *b == 0)
                    .map_or(chunk_end, |p| body + p + 1);
                let mut mesh = Some(Mesh::default());
                three_ds_chunks(data, name_end, chunk_end, out, &mut mesh, depth + 1)?;
                if let Some(mut m) = mesh.filter(|m| !m.positions.is_empty()) {
                    m.parts = 1;
                    out.append(m);
                    check_triangles(out.triangle_count())?;
                }
            }
            0x4100 => three_ds_chunks(data, body, chunk_end, out, object, depth + 1)?,
            0x4110 => {
                let Some(mesh) = object.as_mut() else {
                    at = chunk_end;
                    continue;
                };
                let Some(n) = count else {
                    at = chunk_end;
                    continue;
                };
                if body + 2 + n * 12 > chunk_end {
                    bail!("3DS vertex list is truncated");
                }
                for v in 0..n {
                    let o = body + 2 + v * 12;
                    let f = |k: usize| f32_le(data, o + k * 4).map(f64::from);
                    mesh.vertex([f(0)?, f(1)?, f(2)?]);
                }
            }
            0x4120 => {
                let Some(mesh) = object.as_mut() else {
                    at = chunk_end;
                    continue;
                };
                let Some(n) = count else {
                    at = chunk_end;
                    continue;
                };
                if body + 2 + n * 8 > chunk_end {
                    bail!("3DS face list is truncated");
                }
                let vertices = mesh.vertex_count() as u32;
                for f in 0..n {
                    let o = body + 2 + f * 8;
                    let i = |k: usize| u16_le(data, o + k * 2).map(u32::from);
                    let t = [i(0)?, i(1)?, i(2)?];
                    if t.iter().any(|v| *v >= vertices) {
                        bail!("3DS face refers to a missing vertex");
                    }
                    mesh.indices.extend(t);
                }
            }
            _ => {}
        }
        at = chunk_end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::test_support::chunk_3ds as chunk;

    #[test]
    fn three_ds_mesh() {
        // main → editor → object "a" → mesh with 3 vertices and 1 face
        let mut vertices = 3u16.to_le_bytes().to_vec();
        for p in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for c in p {
                vertices.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mut faces = 1u16.to_le_bytes().to_vec();
        for i in [0u16, 1, 2, 0] {
            faces.extend_from_slice(&i.to_le_bytes());
        }
        let trimesh = chunk(
            0x4100,
            &[chunk(0x4110, &vertices), chunk(0x4120, &faces)].concat(),
        );
        let object = chunk(0x4000, &[b"a\0".to_vec(), trimesh].concat());
        let file = chunk(0x4D4D, &chunk(0x3D3D, &object));
        let mesh = crate::mesh::read("3ds", &file).unwrap();
        assert_eq!(
            (mesh.triangle_count(), mesh.vertex_count(), mesh.parts),
            (1, 3, 1)
        );
        assert!(read(b"\x4D\x4D").is_err(), "a chunk header cut short");
    }

    /// Regression: a vertex or face list chunk of the minimum length 6 has
    /// no room for its count. At the end of the file the count was read
    /// past the last byte – a panic. Now the chunk is skipped.
    #[test]
    fn three_ds_list_chunks_without_their_count() {
        let file_with = |lists: Vec<u8>| {
            let object = chunk(0x4000, &[b"a\0".to_vec(), chunk(0x4100, &lists)].concat());
            chunk(0x4D4D, &chunk(0x3D3D, &object))
        };
        for id in [0x4110u16, 0x4120] {
            // the empty list is the last chunk of the file
            let file = file_with(chunk(id, &[]));
            assert!(
                crate::mesh::read("3ds", &file).is_err(),
                "{id:#x}: no geometry"
            );
            assert!(read(&file).unwrap().positions.is_empty());
        }
        // an empty list before a complete one: skipped, the rest is read
        let mut vertices = 3u16.to_le_bytes().to_vec();
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            vertices.extend_from_slice(&c.to_le_bytes());
        }
        let mut faces = 1u16.to_le_bytes().to_vec();
        for i in [0u16, 1, 2, 0] {
            faces.extend_from_slice(&i.to_le_bytes());
        }
        let file = file_with(
            [
                chunk(0x4110, &[]),
                chunk(0x4110, &vertices),
                chunk(0x4120, &[]),
                chunk(0x4120, &faces),
            ]
            .concat(),
        );
        let mesh = crate::mesh::read("3ds", &file).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (1, 3));
    }
}
