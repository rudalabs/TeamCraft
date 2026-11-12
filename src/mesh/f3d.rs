//! The display scene inside Autodesk Fusion designs (`.f3d`).
//!
//! Fusion stores the triangles it draws in the archive (see `fusion`):
//! `OGS.BlobFolder/OGS/DefaultScene/world` is a binary serialisation of the
//! scene graph, `stream_mesh_NNN` holds the raw vertex and index buffers.
//! The format is not documented; this reader relies on what was verified
//! against the STL and STEP exports of a real design
//! (`docs/src/research/fusion-ogs-preview.md`):
//!
//! ```text
//! names            u32 length + UTF-16LE ("GroupNode", "Face", "Body", …)
//! OgsSimpleMesh    6 × f32 bounding box, u32 primitive (4 = triangle list),
//!                  3 bytes flags
//! VertexFormat     id string (16 hex digits); on its first use u32 count and
//!                  count × (u32 type 1 = float2 / 2 = float3, u32 semantic
//!                  0 position / 1 normal / …, u32 offset in floats, 3 × u32)
//! vertex buffer    u32 1, u32 stream, u32 offset, u32 length
//! u8               1 = index buffer follows
//! AIndexBuffer     id string, u32 stream, u8 0 (u32 indices), u32 offset, u32 length
//! ```
//!
//! A mesh counts when the last client type before it is `Face` (edges,
//! sketches, dimensions and work geometry have their own types); the meshes
//! up to a `Body` belong to one body. Coordinates are centimetres, every
//! vertex carries the exact normal of its face and the faces of a body share
//! their boundary points bit for bit: the faces are welded per body and
//! refined along the normals like NX parts (see `pn`).
//!
//! Everything is read defensively: entry sizes are capped, every offset and
//! index is checked, and a damaged scene is an error, never a panic.

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, bail, ensure, Result};

use super::{check_triangles, Mesh};
use crate::fusion::{Archive, SCENE_DIR, SCENE_WORLD};

/// Fusion lengths are centimetres.
const TO_MM: f64 = 10.0;
/// Largest scene graph and largest buffer stream that are read.
const MAX_ENTRY: u64 = 512 * 1024 * 1024;
/// Largest number of vertex format elements and floats per vertex.
const MAX_ELEMENTS: u32 = 16;
const MAX_STRIDE: u32 = 64;
/// Longest name string looked at.
const MAX_NAME: usize = 64;
/// Client types after which a mesh is not a body face.
const OTHER_CONTEXTS: &[&str] = &[
    "Edge",
    "Edges",
    "ProfileFace",
    "ProfileEdge",
    "SketchCurve",
    "SketchConstraint",
    "SketchDimension",
    "SketchPoint",
    "Sketch3D",
    "FTextNode",
    "FatPointMesh",
    "WorkPlane",
    "WorkAxis",
    "WorkPoint",
    "OriginPlane",
    "OriginAxis",
    "OriginPoint",
    "Component",
    "Instance",
];

fn damaged() -> anyhow::Error {
    anyhow!("The display scene of the Fusion design is damaged.")
}

/// The preview mesh of a Fusion design.
pub(crate) fn read_f3d(data: &[u8]) -> Result<Mesh> {
    let mut archive = Archive::open(Cursor::new(data))?
        .ok_or_else(|| anyhow!("This is not a Fusion design archive."))?;
    let world_name = archive
        .entry(&format!("{SCENE_DIR}{SCENE_WORLD}"))
        .ok_or_else(|| {
            anyhow!("This Fusion file has no display scene – the preview image is shown instead.")
        })?;
    let world = archive.read(&world_name, MAX_ENTRY)?.ok_or_else(damaged)?;
    let folder = world_name
        .strip_suffix(SCENE_WORLD)
        .unwrap_or(&world_name)
        .to_string();
    let bodies = faces(&world)?;
    ensure!(
        !bodies.is_empty(),
        "The display scene of the Fusion design contains no body."
    );
    let mut streams: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut lods = Vec::new();
    let mut total = 0usize;
    for body in &bodies {
        let mut lod = super::pn::Lod::default();
        let mut welded: HashMap<[u32; 3], u32> = HashMap::new();
        for face in body {
            for stream in [face.vertices.stream, face.indices.map_or(0, |i| i.stream)] {
                if let std::collections::hash_map::Entry::Vacant(slot) = streams.entry(stream) {
                    let name = format!("{folder}stream_mesh_{stream:03}");
                    let bytes = archive
                        .read(&name, MAX_ENTRY)?
                        .ok_or_else(|| anyhow!("The display scene misses {name}."))?;
                    slot.insert(bytes);
                }
            }
            let vertices = streams.get(&face.vertices.stream).ok_or_else(damaged)?;
            let base = lod.normals.len() as u32;
            let local = face.read_vertices(vertices, &mut lod, &mut welded)?;
            let indices = match face.indices {
                Some(b) => read_indices(streams.get(&b.stream).ok_or_else(damaged)?, b)?,
                None => (0..local.len() as u32).collect(),
            };
            ensure!(indices.len() % 3 == 0, damaged());
            check_triangles(total + lod.triangles.len() + indices.len() / 3)?;
            for t in indices.chunks_exact(3) {
                let corner = |k: usize| -> Result<u32> {
                    let i = *t.get(k).ok_or_else(damaged)? as usize;
                    local.get(i).copied().ok_or_else(damaged)
                };
                let normal = |k: usize| -> Result<u32> {
                    let i = *t.get(k).ok_or_else(damaged)?;
                    ensure!((i as usize) < local.len(), damaged());
                    Ok(base + i)
                };
                let tri = [corner(0)?, corner(1)?, corner(2)?];
                if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                    continue;
                }
                lod.triangles.push(tri);
                lod.normal_triangles
                    .push([normal(0)?, normal(1)?, normal(2)?]);
            }
        }
        total += lod.triangles.len();
        if !lod.triangles.is_empty() {
            lods.push(lod);
        }
    }
    ensure!(
        !lods.is_empty(),
        "The display scene of the Fusion design contains no triangles."
    );
    super::pn::to_mesh(lods, total, TO_MM)
}

/// Where a buffer lies in the `stream_mesh_NNN` entries.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Buffer {
    stream: u32,
    offset: u32,
    length: u32,
}

impl Buffer {
    fn bytes(self, data: &[u8]) -> Result<&[u8]> {
        let start = self.offset as usize;
        let end = start
            .checked_add(self.length as usize)
            .ok_or_else(damaged)?;
        data.get(start..end).ok_or_else(damaged)
    }
}

/// A vertex layout: floats per vertex and where position and normal are.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Format {
    stride: u32,
    position: u32,
    normal: Option<u32>,
}

/// One face mesh of a body.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    format: Format,
    vertices: Buffer,
    indices: Option<Buffer>,
}

impl Face {
    /// Reads the vertices into `lod`: positions welded by their exact bits,
    /// one normal per vertex. Returns the welded index of every vertex.
    fn read_vertices(
        &self,
        stream: &[u8],
        lod: &mut super::pn::Lod,
        welded: &mut HashMap<[u32; 3], u32>,
    ) -> Result<Vec<u32>> {
        let data = self.vertices.bytes(stream)?;
        let stride = self.format.stride as usize * 4;
        ensure!(stride > 0 && data.len() % stride == 0, damaged());
        let float = |v: &[u8], at: u32, k: usize| -> Result<f32> {
            let at = at as usize * 4 + k * 4;
            super::f32_le(v, at)
        };
        let mut local = Vec::with_capacity(data.len() / stride);
        for v in data.chunks_exact(stride) {
            let p = [
                float(v, self.format.position, 0)?,
                float(v, self.format.position, 1)?,
                float(v, self.format.position, 2)?,
            ];
            ensure!(p.iter().all(|x| x.is_finite()), damaged());
            let key = p.map(|x| (x + 0.0).to_bits());
            let next = lod.points.len() as u32;
            let index = *welded.entry(key).or_insert(next);
            if index == next {
                lod.points.push(p.map(f64::from));
            }
            local.push(index);
            let n = match self.format.normal {
                Some(at) => [float(v, at, 0)?, float(v, at, 1)?, float(v, at, 2)?],
                None => [f32::NAN; 3],
            };
            lod.normals.push(n.map(f64::from));
        }
        Ok(local)
    }
}

fn read_indices(stream: &[u8], buffer: Buffer) -> Result<Vec<u32>> {
    let data = buffer.bytes(stream)?;
    ensure!(data.len() % 4 == 0, damaged());
    Ok(data
        .chunks_exact(4)
        .map(|c| <[u8; 4]>::try_from(c).map_or(u32::MAX, u32::from_le_bytes))
        .collect())
}

/// A known name (`u32 length` + UTF-16LE) at `at`, and the position after
/// it. Other names and anything else are `None`.
fn name_at(data: &[u8], at: usize) -> Option<(&'static str, usize)> {
    const KNOWN: &[&str] = &[
        "OgsSimpleMesh",
        "VertexFormat",
        "AIndexBuffer",
        "Face",
        "Body",
    ];
    let len = super::u32_le(data, at).ok()? as usize;
    if len == 0 || len > MAX_NAME {
        return None;
    }
    let end = at + 4 + 2 * len;
    let raw = data.get(at + 4..end)?;
    KNOWN
        .iter()
        .chain(OTHER_CONTEXTS)
        .find(|k| k.len() == len && k.bytes().zip(raw.chunks_exact(2)).all(|(a, c)| c == [a, 0]))
        .map(|k| (*k, end))
}

/// The face meshes of the scene, grouped by body.
fn faces(world: &[u8]) -> Result<Vec<Vec<Face>>> {
    let mut formats: HashMap<&[u8], Format> = HashMap::new();
    let mut bodies = Vec::new();
    let mut body = Vec::new();
    let mut in_face = false;
    let mut at = 0;
    while at + 6 <= world.len() {
        let Some((name, end)) = name_at(world, at) else {
            at += 1;
            continue;
        };
        match name {
            "Face" => in_face = true,
            "Body" => {
                in_face = false;
                if !body.is_empty() {
                    bodies.push(std::mem::take(&mut body));
                }
            }
            "OgsSimpleMesh" => {
                // every mesh is parsed, so that vertex formats defined by a
                // sketch or an edge are known when a face refers to them
                let mesh = simple_mesh(world, end, &mut formats);
                if in_face {
                    if let Some(face) = mesh.map_err(|_| damaged())? {
                        body.push(face);
                    }
                }
                at = end;
                continue;
            }
            "VertexFormat" | "AIndexBuffer" => {}
            _ => in_face = false,
        }
        at = end;
    }
    if !body.is_empty() {
        bodies.push(body);
    }
    Ok(bodies)
}

/// Reads the `OgsSimpleMesh` record after its name. `Some` for an indexed
/// or plain triangle list, `None` for lines and points.
fn simple_mesh<'a>(
    world: &'a [u8],
    at: usize,
    formats: &mut HashMap<&'a [u8], Format>,
) -> Result<Option<Face>> {
    let mut p = at + 24;
    let primitive = super::u32_le(world, p)?;
    p += 4 + 3;
    let (name, end) = name_at(world, p).ok_or_else(damaged)?;
    ensure!(name == "VertexFormat", damaged());
    let (id, end) = id_at(world, end)?;
    p = end;
    let format = match formats.get(id) {
        Some(f) => *f,
        None => {
            let count = super::u32_le(world, p)?;
            ensure!(count > 0 && count <= MAX_ELEMENTS, damaged());
            p += 4;
            let mut stride = 0;
            let mut position = None;
            let mut normal = None;
            for _ in 0..count {
                let kind = super::u32_le(world, p)?;
                let semantic = super::u32_le(world, p + 4)?;
                let offset = super::u32_le(world, p + 8)?;
                p += 24;
                let floats = match kind {
                    0 => 1,
                    1 => 2,
                    2 => 3,
                    3 => 4,
                    _ => bail!(damaged()),
                };
                stride = stride.max(offset.saturating_add(floats));
                match (semantic, floats) {
                    (0, 3) => position = Some(offset),
                    (1, 3) => normal = Some(offset),
                    _ => {}
                }
            }
            ensure!(stride <= MAX_STRIDE, damaged());
            let f = Format {
                stride,
                position: position.ok_or_else(damaged)?,
                normal,
            };
            formats.insert(id, f);
            f
        }
    };
    ensure!(super::u32_le(world, p)? == 1, damaged());
    let vertices = Buffer {
        stream: super::u32_le(world, p + 4)?,
        offset: super::u32_le(world, p + 8)?,
        length: super::u32_le(world, p + 12)?,
    };
    p += 16;
    let indexed = *world.get(p).ok_or_else(damaged)?;
    p += 1;
    let indices = match indexed {
        0 => None,
        1 => {
            let (name, end) = name_at(world, p).ok_or_else(damaged)?;
            ensure!(name == "AIndexBuffer", damaged());
            let (_, end) = id_at(world, end)?;
            let stream = super::u32_le(world, end)?;
            let kind = *world.get(end + 4).ok_or_else(damaged)?;
            ensure!(
                kind == 0,
                "Fusion display meshes with 16-bit indices are not supported yet."
            );
            Some(Buffer {
                stream,
                offset: super::u32_le(world, end + 5)?,
                length: super::u32_le(world, end + 9)?,
            })
        }
        _ => bail!(damaged()),
    };
    if primitive != 4 {
        return Ok(None);
    }
    Ok(Some(Face {
        format,
        vertices,
        indices,
    }))
}

/// An object id: `u32 length` + UTF-16LE (16 hex digits in the files seen).
fn id_at(data: &[u8], at: usize) -> Result<(&[u8], usize)> {
    let len = super::u32_le(data, at)? as usize;
    ensure!(len > 0 && len <= MAX_NAME, damaged());
    let end = at + 4 + 2 * len;
    Ok((data.get(at + 4..end).ok_or_else(damaged)?, end))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::fusion::sample;

    #[test]
    fn a_cube_becomes_a_closed_mesh_in_millimetres() {
        let archive = sample::cube_archive(2.0, b"png");
        let mesh = super::super::read("fusion-design", &archive).unwrap();
        assert_eq!(mesh.parts, 1);
        let lo = (0..3).map(|c| {
            mesh.positions
                .iter()
                .skip(c)
                .step_by(3)
                .fold(f32::MAX, |a, &b| a.min(b))
        });
        let hi = (0..3).map(|c| {
            mesh.positions
                .iter()
                .skip(c)
                .step_by(3)
                .fold(f32::MIN, |a, &b| a.max(b))
        });
        for (l, h) in lo.zip(hi) {
            assert!(l.abs() < 1e-4 && (h - 20.0).abs() < 1e-4, "{l} {h}");
        }
        // welded and refined: every edge is shared by exactly two triangles
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for t in mesh.indices.chunks(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        assert!(mesh.triangle_count() > 12);
        assert!(edges.values().all(|&n| n == 2));
    }

    #[test]
    fn sketches_and_edges_are_left_out() {
        let (world, _) = sample::scene(&sample::cube_faces(1.0));
        let bodies = faces(&world).unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].len(), 6);
        assert_eq!(bodies[0][0].format.stride, 8);
        assert_eq!(bodies[0][0].format.normal, Some(3));
    }

    #[test]
    fn names_are_matched_exactly() {
        let mut d = Vec::new();
        d.extend_from_slice(&4u32.to_le_bytes());
        d.extend("Face".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(name_at(&d, 0), Some(("Face", d.len())));
        let mut d = Vec::new();
        d.extend_from_slice(&5u32.to_le_bytes());
        d.extend("Faces".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(name_at(&d, 0).map(|n| n.0), None);
        assert_eq!(name_at(&[4, 0, 0, 0, b'F', 0], 0), None);
        assert_eq!(name_at(&[], 0), None);
    }

    #[test]
    fn damaged_scenes_are_errors_not_panics() {
        let (world, stream) = sample::scene(&sample::cube_faces(1.0));
        for cut in (0..world.len()).step_by(7) {
            let _ = faces(&world[..cut]);
            let mut bad = world.clone();
            bad[cut] ^= 0xff;
            let _ = faces(&bad);
        }
        // offsets past the stream end
        let bodies = faces(&world).unwrap();
        let mut lod = super::super::pn::Lod::default();
        let mut welded = HashMap::new();
        let face = &bodies[0][0];
        assert!(face
            .read_vertices(&stream[..10], &mut lod, &mut welded)
            .is_err());
        assert!(read_indices(&stream[..4], face.indices.unwrap()).is_err());
    }

    #[test]
    fn files_without_a_display_scene_say_so() {
        let png = b"png".to_vec();
        let archive = sample::zip(&[(
            "FusionAssetName[Active]/Previews/big.png",
            0,
            &png[..],
            &png[..],
        )]);
        let e = super::super::read("fusion-design", &archive).unwrap_err();
        assert!(e.to_string().contains("no display scene"), "{e}");
        assert!(super::super::read("fusion-design", b"PK\x03\x04").is_err());
    }
}
