//! Autodesk Inventor parts: the display tessellation Inventor stores in the
//! segment `PmGraphicsSegment` (see `crate::invpart` and
//! `docs/src/research/inventor-display-preview.md`).
//!
//! One section of class `D79AD3F3` per B-rep face (the simpler `A79EACD2`
//! in a few files) holds, after a 26-byte header with the owning face's key
//! at offset 18:
//!
//! ```text
//! array of points   f32 xyz, centimetres, part coordinates
//! array of u32      triangle list, counter-clockwise around the outside
//! array of vectors  unit surface normal per vertex (may be empty)
//! array of 2D points, …  (uv, edge flags: not needed)
//! ```
//!
//! The faces come in body groups, each opened by a record of class
//! `B32BF6A6` whose flag is 1 for solid bodies and 0 for surface bodies and
//! construction copies. The preview takes the solid bodies (or every face
//! of a part without one). Neighbouring faces share their boundary
//! vertices bit for bit, so the vertices are welded on their exact values,
//! and the mesh is refined along the stored normals with PN triangles, as
//! for NX and SOLIDWORKS parts.

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, bail, Result};

use super::{Mesh, MAX_TRIANGLES};
use crate::invpart::{self, Segment, POINT3F, VECTOR3F};

/// The segment with the display tessellation of a part.
pub(crate) const PART_GRAPHICS: &str = "PmGraphicsSegment";
/// Largest graphics segment inflated (the biggest seen: about 8 MiB).
const MAX_SEGMENT: u64 = 256 * 1024 * 1024;
/// Inventor lengths are centimetres.
const TO_MM: f64 = 10.0;
/// Face mesh (one per B-rep face).
const FACE_MESH: u32 = 0xD79A_D3F3;
/// Simple triangle set: work planes, second triangulations, and the face
/// meshes of some files.
const TRIANGLES: u32 = 0xA79E_ACD2;
/// B-rep face nodes that own face meshes.
const FACE_NODES: [u32; 2] = [0x37DB_9D1E, 0x03E3_D90B];
/// Opens a body group: `u32 version, u8 solid`.
const BODY_KIND: u32 = 0xB32B_F6A6;
/// Offset of the owner key in a mesh header.
const OWNER_AT: usize = 0x12;
/// Size of a mesh header.
const HEADER: usize = 0x1A;

type P = [f64; 3];

/// One face mesh: positions (cm, exact `f32`), triangles, normals.
#[derive(Debug, Default)]
struct Face {
    points: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
    normals: Vec<P>,
    solid: bool,
}

/// The preview mesh of an Inventor part (bytes of the whole file).
pub(crate) fn read_part(data: &[u8]) -> Result<Mesh> {
    let mut ole = cfb::CompoundFile::open(Cursor::new(data))
        .map_err(|_| anyhow!("This is no Inventor file (no OLE compound file)."))?;
    let segment = invpart::segment(&mut ole, PART_GRAPHICS, MAX_SEGMENT)?.ok_or_else(|| {
        anyhow!("This Inventor file stores no display tessellation – the preview image is shown instead.")
    })?;
    let faces = faces(&segment)?;
    let solid: Vec<&Face> = faces.iter().filter(|f| f.solid).collect();
    let chosen = if solid.is_empty() {
        faces.iter().collect()
    } else {
        solid
    };
    if chosen.iter().all(|f| f.triangles.is_empty()) {
        bail!("The Inventor part has no body (only sketches or work features).");
    }
    build(&chosen)
}

/// A mesh section: (section index, class, in a solid body, payload).
type Candidate<'a> = (usize, u32, bool, &'a [u8]);

/// The face meshes of a graphics segment, in file order.
fn faces(segment: &Segment) -> Result<Vec<Face>> {
    let mut meshes: HashMap<u32, Vec<Candidate>> = HashMap::new();
    let mut face_keys = std::collections::HashSet::new();
    let mut solid = true;
    for section in segment.sections() {
        let s = section?;
        match s.class {
            BODY_KIND => solid = s.payload.get(4).is_some_and(|&b| b != 0),
            c if FACE_NODES.contains(&c) => {
                if let Some(key) = invpart::u32_at(s.payload, 0) {
                    face_keys.insert(key);
                }
            }
            FACE_MESH | TRIANGLES => {
                if let Some(owner) = invpart::u32_at(s.payload, OWNER_AT) {
                    meshes
                        .entry(owner)
                        .or_default()
                        .push((s.index, s.class, solid, s.payload));
                }
            }
            _ => {}
        }
    }
    // per owner: its face mesh, else a simple triangle set owned by a face
    // node (other triangle sets are work planes and labels)
    let mut chosen: Vec<(usize, bool, &[u8])> = Vec::new();
    for (owner, list) in &meshes {
        let pick = list
            .iter()
            .find(|m| m.1 == FACE_MESH)
            .or_else(|| face_keys.contains(owner).then(|| list.first()).flatten());
        if let Some(&(index, _, solid, payload)) = pick {
            chosen.push((index, solid, payload));
        }
    }
    chosen.sort_by_key(|c| c.0);
    let mut out = Vec::with_capacity(chosen.len());
    let mut triangles = 0usize;
    for (_, solid, payload) in chosen {
        let face = face(segment, payload, solid).ok_or_else(|| {
            anyhow!("The Inventor part is damaged (a face mesh does not check out).")
        })?;
        triangles += face.triangles.len();
        if triangles > MAX_TRIANGLES {
            bail!(
                "The model has more than {MAX_TRIANGLES} triangles – more than the preview shows."
            );
        }
        out.push(face);
    }
    Ok(out)
}

/// The arrays of one face mesh (`None` when they do not check out).
fn face(segment: &Segment, payload: &[u8], solid: bool) -> Option<Face> {
    let mut c = invpart::Cursor::new(payload, HEADER);
    let positions = c.array(segment)?;
    let indices = c.array(segment)?;
    let normals = c.array(segment)?;
    if positions.count == 0 {
        return Some(Face::default());
    }
    if segment.element_class(positions.elem)? != POINT3F
        || !invpart::is_u32(indices.elem)
        || indices.count % 3 != 0
        || (normals.count != 0
            && (segment.element_class(normals.elem)? != VECTOR3F
                || normals.count != positions.count))
    {
        return None;
    }
    let f32s = |b: &[u8]| -> Option<Vec<[f32; 3]>> {
        b.chunks_exact(12)
            .map(|v| {
                let f = |k: usize| -> Option<f32> {
                    let x = f32::from_le_bytes(v.get(k..k + 4)?.try_into().ok()?);
                    x.is_finite().then_some(x)
                };
                Some([f(0)?, f(4)?, f(8)?])
            })
            .collect()
    };
    let points = f32s(positions.bytes)?;
    let normals: Vec<P> = f32s(normals.bytes)?
        .into_iter()
        .map(|n| unit(n.map(f64::from)))
        .collect();
    let n = u32::try_from(points.len()).ok()?;
    let triangles = indices
        .bytes
        .chunks_exact(12)
        .map(|t| {
            let i = |k: usize| invpart::u32_at(t, k).filter(|&v| v < n);
            Some([i(0)?, i(4)?, i(8)?])
        })
        .collect::<Option<Vec<[u32; 3]>>>()?;
    Some(Face {
        points,
        triangles,
        normals,
        solid,
    })
}

/// Welds the faces on their exact coordinates and refines them along the
/// normals.
fn build(faces: &[&Face]) -> Result<Mesh> {
    let mut points: Vec<P> = Vec::new();
    let mut welded: HashMap<[u32; 3], u32> = HashMap::new();
    let mut normals: Vec<P> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut normal_triangles: Vec<[u32; 3]> = Vec::new();
    for face in faces {
        let mut index = Vec::with_capacity(face.points.len());
        for p in &face.points {
            // -0.0 and 0.0 are the same vertex
            let key = p.map(|c| (c + 0.0).to_bits());
            let next = u32::try_from(points.len()).map_err(|_| anyhow!("too many vertices"))?;
            index.push(*welded.entry(key).or_insert_with(|| {
                points.push(p.map(f64::from));
                next
            }));
        }
        for t in &face.triangles {
            let w = t.map(|k| index.get(k as usize).copied().unwrap_or(u32::MAX));
            if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] || w.contains(&u32::MAX) {
                continue;
            }
            let n = if face.normals.is_empty() {
                // no stored normals: the flat normal of the triangle
                let g = flat(&points, w);
                let base = u32::try_from(normals.len()).map_err(|_| anyhow!("too many normals"))?;
                normals.extend([g; 3]);
                [base, base + 1, base + 2]
            } else {
                let base = normals.len();
                t.map(|k| u32::try_from(base + k as usize).unwrap_or(u32::MAX))
            };
            triangles.push(w);
            normal_triangles.push(n);
        }
        if !face.normals.is_empty() {
            normals.extend(&face.normals);
        }
    }
    let level = super::jt::refine_level(triangles.len());
    let (points, triangles) = if level > 1 {
        super::pn::refine(&points, &triangles, &normals, &normal_triangles, level)
    } else {
        (points, triangles)
    };
    if triangles.len() > MAX_TRIANGLES {
        bail!("The model has more than {MAX_TRIANGLES} triangles – more than the preview shows.");
    }
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    for p in &points {
        mesh.vertex([p[0] * TO_MM, p[1] * TO_MM, p[2] * TO_MM]);
    }
    mesh.indices.extend(triangles.iter().flatten());
    Ok(mesh)
}

fn unit(n: P) -> P {
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l > 1e-12 {
        n.map(|c| c / l)
    } else {
        [0.0; 3]
    }
}

/// The unit normal of a triangle (zero when degenerate).
fn flat(points: &[P], t: [u32; 3]) -> P {
    let p = |i: u32| points.get(i as usize).copied().unwrap_or_default();
    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    unit([
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invpart::tests::{array, file};

    const CLASSES: [u32; 7] = [
        POINT3F,
        VECTOR3F,
        FACE_MESH,
        TRIANGLES,
        BODY_KIND,
        0x37DB_9D1E,
        0x591E_9565,
    ];

    fn floats(v: &[[f32; 3]]) -> Vec<u8> {
        v.iter().flatten().flat_map(|c| c.to_le_bytes()).collect()
    }

    /// A face mesh section of class index `class` owned by `owner`.
    fn mesh(
        class: u8,
        owner: u32,
        pts: &[[f32; 3]],
        tris: &[[u32; 3]],
        normal: Option<[f32; 3]>,
    ) -> (u8, Vec<u8>) {
        let mut p = vec![0u8; HEADER];
        p[OWNER_AT..OWNER_AT + 4].copy_from_slice(&owner.to_le_bytes());
        p.extend(array(0x100, pts.len(), &floats(pts)));
        let idx: Vec<u8> = tris
            .iter()
            .flatten()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        p.extend(array(invpart::ELEM_U32, 3 * tris.len(), &idx));
        match normal {
            Some(n) => p.extend(array(0x101, pts.len(), &floats(&vec![n; pts.len()]))),
            None => p.extend(array(0, 0, &[])),
        }
        p.extend(array(0, 0, &[]));
        (class, p)
    }

    fn body(solid: bool) -> (u8, Vec<u8>) {
        (4, vec![1, 0, 0, 0, u8::from(solid)])
    }

    fn node(key: u32) -> (u8, Vec<u8>) {
        (5, key.to_le_bytes().to_vec())
    }

    /// A unit cube (cm) as six faces of two triangles each.
    fn cube() -> Vec<(u8, Vec<u8>)> {
        let c = |x: f32, y: f32, z: f32| [x, y, z];
        let quads: [([[f32; 3]; 4], [f32; 3]); 6] = [
            (
                [c(0., 0., 0.), c(0., 1., 0.), c(1., 1., 0.), c(1., 0., 0.)],
                [0., 0., -1.],
            ),
            (
                [c(0., 0., 1.), c(1., 0., 1.), c(1., 1., 1.), c(0., 1., 1.)],
                [0., 0., 1.],
            ),
            (
                [c(0., 0., 0.), c(1., 0., 0.), c(1., 0., 1.), c(0., 0., 1.)],
                [0., -1., 0.],
            ),
            (
                [c(0., 1., 0.), c(0., 1., 1.), c(1., 1., 1.), c(1., 1., 0.)],
                [0., 1., 0.],
            ),
            (
                [c(0., 0., 0.), c(0., 0., 1.), c(0., 1., 1.), c(0., 1., 0.)],
                [-1., 0., 0.],
            ),
            (
                [c(1., 0., 0.), c(1., 1., 0.), c(1., 1., 1.), c(1., 0., 1.)],
                [1., 0., 0.],
            ),
        ];
        let mut out = vec![body(true)];
        for (k, (q, n)) in quads.iter().enumerate() {
            let key = 0x8000_0100 + k as u32;
            out.push(node(key));
            out.push(mesh(2, key, q, &[[0, 1, 2], [0, 2, 3]], Some(*n)));
        }
        out
    }

    fn part(sections: Vec<(u8, Vec<u8>)>) -> Vec<u8> {
        file(&[(PART_GRAPHICS, &CLASSES, sections)], &[])
    }

    fn edges_used_twice(mesh: &Mesh) -> bool {
        let mut count: HashMap<(u32, u32), i32> = HashMap::new();
        for t in mesh.indices.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *count.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        count.values().all(|&c| c == 2)
    }

    #[test]
    fn a_cube_becomes_a_closed_mesh_in_millimetres() {
        let mesh = read_part(&part(cube())).unwrap();
        mesh.check().unwrap();
        // the faces store their corners separately: closed only when welded
        assert!(edges_used_twice(&mesh), "shared corners are welded");
        assert!(mesh.indices.len() >= 36, "refined, never coarser");
        let max = mesh.positions.iter().copied().fold(f32::MIN, f32::max);
        let min = mesh.positions.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            (max - 10.0).abs() < 1e-6 && min.abs() < 1e-6,
            "cm to mm: {min}..{max}"
        );
    }

    #[test]
    fn surface_bodies_and_work_planes_are_left_out_when_there_is_a_solid() {
        let mut sections = vec![
            // a work plane before the bodies: a triangle set owned by no face
            mesh(
                3,
                0x8000_0001,
                &[[5., 5., 5.], [6., 5., 5.], [5., 6., 5.]],
                &[[0, 1, 2]],
                None,
            ),
        ];
        sections.extend(cube());
        // a surface body far away
        sections.push(body(false));
        sections.push(node(0x8000_0200));
        sections.push(mesh(
            2,
            0x8000_0200,
            &[[9., 9., 9.], [10., 9., 9.], [9., 10., 9.]],
            &[[0, 1, 2]],
            Some([0., 0., 1.]),
        ));
        let mesh = read_part(&part(sections)).unwrap();
        let max = mesh.positions.iter().copied().fold(f32::MIN, f32::max);
        assert!((max - 10.0).abs() < 1e-6, "only the cube: {max}");
    }

    #[test]
    fn a_surface_only_part_shows_its_surfaces() {
        let sections = vec![
            body(false),
            node(0x8000_0300),
            // a simple triangle set owned by a face node, without normals
            mesh(
                3,
                0x8000_0300,
                &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                &[[0, 1, 2]],
                None,
            ),
        ];
        let mesh = read_part(&part(sections)).unwrap();
        mesh.check().unwrap();
        assert!(!mesh.indices.is_empty());
        // a flat triangle stays flat in z = 0, 10 mm wide
        assert!(mesh.positions.chunks(3).all(|p| p[2] == 0.0));
        let max = mesh.positions.iter().copied().fold(f32::MIN, f32::max);
        assert!((max - 10.0).abs() < 1e-6, "{max}");
    }

    #[test]
    fn parts_without_a_body_or_a_graphics_segment_are_refused() {
        assert!(read_part(&part(vec![body(true)])).is_err());
        assert!(read_part(&file(&[("PmAppSegment", &CLASSES, vec![])], &[])).is_err());
        assert!(read_part(b"ISO-10303-21;").is_err());
        assert!(read_part(&[]).is_err());
    }

    #[test]
    fn damaged_face_meshes_never_panic() {
        // indices beyond the vertices
        let bad = vec![
            body(true),
            mesh(2, 1, &[[0., 0., 0.]; 3], &[[0, 1, 7]], None),
        ];
        assert!(read_part(&part(bad)).is_err());
        // a NaN coordinate
        let nan = vec![
            body(true),
            mesh(
                2,
                1,
                &[[f32::NAN, 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                &[[0, 1, 2]],
                None,
            ),
        ];
        assert!(read_part(&part(nan)).is_err());
        // every cut of a face payload
        let (_, payload) = mesh(
            2,
            1,
            &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            &[[0, 1, 2]],
            Some([0., 0., 1.]),
        );
        for cut in 0..payload.len() {
            let _ = read_part(&part(vec![body(true), (2, payload[..cut].to_vec())]));
        }
    }
}
