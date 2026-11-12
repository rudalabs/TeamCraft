//! SOLIDWORKS parts (2015 and newer): the display tessellation SOLIDWORKS
//! stores in the section `Contents/DisplayLists` (see
//! `docs/src/research/solidworks-display-preview.md`).
//!
//! The section is an MFC archive. Every B-rep face is an object whose
//! payload holds three typed arrays, each behind a header
//! `u32 element size, u32 type (8 integer, 100 float), u32 2, u32 count`:
//!
//! ```text
//! 04 08 02 n_strips   u32 strip lengths
//! 0c 64 02 n_verts    f32 xyz per vertex, metres
//! 0c 64 02 n_verts    f32 unit surface normal per vertex
//! ```
//!
//! The strips are plain triangle strips (`sum(lengths) == n_verts`, no
//! index buffer) in the usual order, counter-clockwise around the outward
//! normals. A face object starts with `u32 n_triangles, u32 n_strips` right
//! before the strip array (SOLIDWORKS 2016/2018) or 40 bytes before it
//! (2024: 32 more bytes). Strip sets without that header are second meshes
//! inside a face record (shaded cosmetic threads lying on the hole's
//! cylinder) and are left out.
//!
//! The faces are found by their array signatures, every count is checked
//! against the section before anything is allocated, and the vertices of
//! neighbouring faces – stored per face – are welded on a 1 µm grid so that
//! the PN refinement (`pn.rs`, as for NX parts) sees shared edges.

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, bail, Result};

use super::{Mesh, MAX_TRIANGLES};

/// The section with the display tessellation.
pub(crate) const DISPLAY_LISTS: &str = "Contents/DisplayLists";
/// Largest display list inflated (the biggest seen: about 10 MiB).
const MAX_SECTION: u64 = 256 * 1024 * 1024;
/// SOLIDWORKS lengths are metres.
const TO_MM: f64 = 1000.0;
/// Weld distance of the per-face vertices (metres).
const WELD: f64 = 1e-6;
/// Header of the strip-length array.
const STRIPS: [u8; 12] = header(4, 8);
/// Header of a vertex or normal array (without the count).
const VECTORS: [u8; 12] = header(12, 100);

const fn header(size: u32, kind: u32) -> [u8; 12] {
    let s = size.to_le_bytes();
    let k = kind.to_le_bytes();
    [s[0], s[1], s[2], s[3], k[0], k[1], k[2], k[3], 2, 0, 0, 0]
}

type P = [f64; 3];

/// One face: strip lengths, vertices (metres) and their normals.
#[derive(Debug, Default)]
struct Face {
    strips: Vec<usize>,
    points: Vec<P>,
    normals: Vec<P>,
}

/// The preview mesh of a SOLIDWORKS part (bytes of the whole file).
pub(crate) fn read_part(data: &[u8]) -> Result<Mesh> {
    let Some(mut file) = crate::swpart::SwContainer::open(Cursor::new(data))? else {
        bail!(
            "This SOLIDWORKS file was saved before SOLIDWORKS 2015 – the 3D preview \
             reads the newer files; the preview image is shown instead."
        );
    };
    let lists = file.read(DISPLAY_LISTS, MAX_SECTION)?.ok_or_else(|| {
        anyhow!(
            "This SOLIDWORKS file stores no display tessellation – the preview image \
             is shown instead."
        )
    })?;
    let faces = faces(&lists)?;
    if faces.is_empty() {
        bail!("The SOLIDWORKS part contains no triangles.");
    }
    build(&faces)
}

/// The face meshes of a display list, in file order.
fn faces(lists: &[u8]) -> Result<Vec<Face>> {
    let mut faces = Vec::new();
    let mut triangles = 0usize;
    let mut pos = 0;
    while let Some(at) = find(lists, &STRIPS, pos) {
        match face_at(lists, at) {
            Some((face, end)) => {
                if let Some(face) = face {
                    triangles += face.strips.iter().map(|l| l - 2).sum::<usize>();
                    if triangles > MAX_TRIANGLES {
                        bail!(
                            "The model has more than {MAX_TRIANGLES} triangles – more \
                             than the preview shows."
                        );
                    }
                    faces.push(face);
                }
                pos = end;
            }
            None => pos = at + 1,
        }
    }
    Ok(faces)
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn u32_at(data: &[u8], at: usize) -> Option<usize> {
    let b = data.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_le_bytes(b.try_into().ok()?)).ok()
}

fn vectors(data: &[u8], at: usize, n: usize) -> Option<Vec<P>> {
    let bytes = data.get(at..at.checked_add(n.checked_mul(12)?)?)?;
    let mut out = Vec::with_capacity(n);
    for v in bytes.chunks_exact(12) {
        let f = |k: usize| -> Option<f64> {
            let x = f32::from_le_bytes(v.get(k..k + 4)?.try_into().ok()?);
            x.is_finite().then_some(f64::from(x))
        };
        out.push([f(0)?, f(4)?, f(8)?]);
    }
    Some(out)
}

/// The strip set whose strip array starts at `at`: `None` if the arrays do
/// not check out, otherwise the face (`None` for a strip set without a face
/// header) and where its arrays end.
fn face_at(data: &[u8], at: usize) -> Option<(Option<Face>, usize)> {
    let n_strips = u32_at(data, at + 12)?;
    if n_strips == 0 {
        return None;
    }
    let verts_at = at.checked_add(16)?.checked_add(n_strips.checked_mul(4)?)?;
    if data.get(verts_at..verts_at + 12)? != VECTORS {
        return None;
    }
    let n_verts = u32_at(data, verts_at + 12)?;
    let normals_at = verts_at
        .checked_add(16)?
        .checked_add(n_verts.checked_mul(12)?)?;
    if n_verts < 3
        || data.get(normals_at..normals_at + 12)? != VECTORS
        || u32_at(data, normals_at + 12)? != n_verts
    {
        return None;
    }
    let end = normals_at.checked_add(16)?.checked_add(n_verts * 12)?;
    if end > data.len() {
        return None;
    }
    let mut strips = Vec::with_capacity(n_strips);
    let mut total = 0usize;
    for k in 0..n_strips {
        let len = u32_at(data, at + 16 + 4 * k)?;
        if len < 3 {
            return None;
        }
        total = total.checked_add(len)?;
        strips.push(len);
    }
    if total != n_verts {
        return None;
    }
    let points = vectors(data, verts_at + 16, n_verts)?;
    let normals = vectors(data, normals_at + 16, n_verts)?;
    let n_tri: usize = strips.iter().map(|l| l - 2).sum();
    let header_at = |back: usize| {
        at.checked_sub(back)
            .and_then(|h| Some((u32_at(data, h)?, u32_at(data, h + 4)?)))
            == Some((n_tri, n_strips))
    };
    let face = (header_at(40) || header_at(8)).then_some(Face {
        strips,
        points,
        normals,
    });
    Some((face, end))
}

/// Welds the faces, orders the triangles and refines them along the
/// normals.
fn build(faces: &[Face]) -> Result<Mesh> {
    let mut points: Vec<P> = Vec::new();
    let mut welded: HashMap<[i64; 3], u32> = HashMap::new();
    let mut normals: Vec<P> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut normal_triangles: Vec<[u32; 3]> = Vec::new();
    for face in faces {
        let base = normals.len();
        let mut index = Vec::with_capacity(face.points.len());
        for p in &face.points {
            let grid = p.map(|c| (c / WELD).round() as i64);
            let next = u32::try_from(points.len()).map_err(|_| anyhow!("too many vertices"))?;
            index.push(*welded.entry(grid).or_insert_with(|| {
                points.push(*p);
                next
            }));
        }
        normals.extend(face.normals.iter().map(|n| unit(*n)));
        let mut start = 0;
        for &len in &face.strips {
            for i in 0..len - 2 {
                let (a, b, c) = (start + i, start + i + 1, start + i + 2);
                let (a, b) = if i % 2 == 1 { (b, a) } else { (a, b) };
                let corners = [a, b, c];
                let t = corners.map(|k| index.get(k).copied().unwrap_or(u32::MAX));
                if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                    continue;
                }
                let n = corners.map(|k| u32::try_from(base + k).unwrap_or(u32::MAX));
                // the strip order is counter-clockwise around the normals;
                // a triangle clearly facing against them is turned
                if facing(&points, &normals, t, n) < -0.5 {
                    triangles.push([t[1], t[0], t[2]]);
                    normal_triangles.push([n[1], n[0], n[2]]);
                } else {
                    triangles.push(t);
                    normal_triangles.push(n);
                }
            }
            start += len;
        }
    }
    let level = super::pn::refine_level(triangles.len());
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

/// Cosine between the triangle's own normal and the mean of its corner
/// normals (0 when either is degenerate).
fn facing(points: &[P], normals: &[P], t: [u32; 3], n: [u32; 3]) -> f64 {
    let p = |i: u32| points.get(i as usize).copied().unwrap_or_default();
    let nn = |i: u32| normals.get(i as usize).copied().unwrap_or_default();
    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let g = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let (n0, n1, n2) = (nn(n[0]), nn(n[1]), nn(n[2]));
    let m = [
        n0[0] + n1[0] + n2[0],
        n0[1] + n1[1] + n2[1],
        n0[2] + n1[2] + n2[2],
    ];
    let dot = g[0] * m[0] + g[1] * m[1] + g[2] * m[2];
    let len = |x: P| (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt();
    let d = len(g) * len(m);
    if d > 0.0 {
        dot / d
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strip set as SOLIDWORKS writes it, with or without the 2016/2018
    /// face header.
    fn strip_set(strips: &[u32], points: &[[f32; 3]], normals: &[[f32; 3]], face: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if face {
            let n_tri: u32 = strips.iter().map(|l| l - 2).sum();
            out.extend(n_tri.to_le_bytes());
            out.extend((strips.len() as u32).to_le_bytes());
        }
        out.extend(STRIPS);
        out.extend((strips.len() as u32).to_le_bytes());
        for s in strips {
            out.extend(s.to_le_bytes());
        }
        for set in [points, normals] {
            out.extend(VECTORS);
            out.extend((set.len() as u32).to_le_bytes());
            for p in set {
                for c in p {
                    out.extend(c.to_le_bytes());
                }
            }
        }
        out
    }

    /// The unit square in z = 0 as one strip of two triangles, normal +z.
    fn square(face: bool) -> Vec<u8> {
        let pts = [
            [0.0, 0.0, 0.0],
            [0.001, 0.0, 0.0],
            [0.0, 0.001, 0.0],
            [0.001, 0.001, 0.0],
        ];
        strip_set(&[4], &pts, &[[0.0, 0.0, 1.0]; 4], face)
    }

    #[test]
    fn strips_become_counter_clockwise_triangles_in_millimetres() {
        let mut lists = b"\x01\x00\x00\x00junk".to_vec();
        lists.extend(square(true));
        let faces = faces(&lists).unwrap();
        assert_eq!(faces.len(), 1);
        let mesh = build(&faces).unwrap();
        mesh.check().unwrap();
        // a flat face is not refined into anything but a finer flat face
        let max = mesh.positions.iter().copied().fold(f32::MIN, f32::max);
        assert!((max - 1.0).abs() < 1e-6, "mm: {max}");
        assert!(mesh.positions.chunks(3).all(|p| p[2] == 0.0));
        // every triangle faces +z
        for t in mesh.indices.chunks(3) {
            let p = |i: u32| &mesh.positions[3 * i as usize..3 * i as usize + 3];
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let z = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            assert!(z > 0.0);
        }
    }

    #[test]
    fn strip_sets_without_a_face_header_are_left_out() {
        let mut lists = square(true);
        lists.extend(square(false));
        assert_eq!(faces(&lists).unwrap().len(), 1);
    }

    #[test]
    fn neighbouring_faces_are_welded() {
        // the square twice, the second copy `dz` metres higher
        let vertices = |dz: f32| {
            let mut lists = square(true);
            let pts = [
                [0.0, 0.0, dz],
                [0.001, 0.0, dz],
                [0.0, 0.001, dz],
                [0.001, 0.001, dz],
            ];
            lists.extend(strip_set(&[4], &pts, &[[0.0, 0.0, 1.0]; 4], true));
            let faces = faces(&lists).unwrap();
            assert_eq!(faces.len(), 2);
            build(&faces).unwrap().vertex_count()
        };
        assert_eq!(vertices(4e-7), vertices(0.0));
        assert!(vertices(5e-6) > vertices(0.0));
    }

    #[test]
    fn damaged_lists_never_panic() {
        let lists = square(true);
        for cut in 0..lists.len() {
            let _ = faces(&lists[..cut]);
        }
        let mut lists = square(true);
        // a strip count that points far beyond the data
        lists[8 + 12..8 + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(faces(&lists).unwrap().is_empty());
        let mut lists = square(true);
        // strip lengths that do not add up to the vertices
        lists[8 + 16..8 + 20].copy_from_slice(&5u32.to_le_bytes());
        assert!(faces(&lists).unwrap().is_empty());
        let mut lists = square(true);
        // a NaN coordinate
        let nan = f32::NAN.to_le_bytes();
        lists[8 + 20 + 16..8 + 20 + 20].copy_from_slice(&nan);
        assert!(faces(&lists).unwrap().is_empty());
    }

    #[test]
    fn files_that_are_no_solidworks_parts_are_refused() {
        assert!(read_part(b"ISO-10303-21;").is_err());
        assert!(read_part(&[]).is_err());
    }
}
