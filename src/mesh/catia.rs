//! The visualisation (CGR) data inside CATIA V5 parts.
//!
//! A `.CATPart` is a `V5_CFV2` container. Besides the exact B-rep it keeps
//! the tessellation CATIA draws on screen in the container `CATCGRCont`,
//! which is stored as a complete `V5_CFV2` document nested inside the file.
//! The mesh lives in its stream `SceneGraph/MainDataStream`, uncompressed:
//!
//! ```text
//! file       "V5_CFV2\0", u32 BE offset and u32 BE length of the directory
//! rep        marker FF FF 02 00 01 after a bounding sphere, then:
//! vertices   vi count, vi normal count, u32 reserved (0),
//!            one 2-bit code per vertex (LSB first), vi float count, f32[]
//!            code c replaces the last (c + 3) % 4 coordinates of the
//!            previous vertex (0: x y z, 3: y z, 2: z, 1: a duplicate)
//! normals    vi i16 count, i16[] (× 1/32767), one 4-bit code per normal:
//!            0..=5 two stored components, the largest one derived (axis
//!            c >> 1, negative if c & 1); 6..=11 an axis (c - 6) >> 1
//! faces      f32 chordal tolerance, 4 bytes, vi face count, per face:
//!            u8 1, u8 flags (0x40 | 1 list, 2 strips, 4 fans, 8 normal),
//!            vi counts, vi total, f32[3] normal, vi strip and fan lengths,
//!            indices (u8 below 256 vertices, otherwise big-endian u16/u32)
//! ```
//!
//! `vi` is a byte, or `FF` followed by a little-endian u32. Coordinates are
//! millimetres in part space. Strips alternate their winding; the result is
//! counter-clockwise seen from outside.
//!
//! The vertex normals are the exact surface normals of the B-rep, so the
//! mesh is refined along the true curvature like an NX part (see `pn`).
//! Planar faces carry one face normal instead.
//!
//! The layout was worked out from sample files (see
//! `docs/src/research/catia-cgr-preview.md`); no CATIA code or
//! documentation was used. Everything is read defensively: counts are capped
//! by the bytes that are left, every index is checked, and a record that
//! does not add up is skipped, never a panic.

use std::collections::HashMap;

use anyhow::{bail, Result};

use super::{check_triangles, Mesh};

/// Magic of a CATIA V5 container (the file and the nested CGR document).
const MAGIC: &[u8; 8] = b"V5_CFV2\0";
/// Start of a representation record in the scene graph stream.
const REP_MARKER: [u8; 5] = [0xFF, 0xFF, 0x02, 0x00, 0x01];
/// Face flags: always set / triangle list / strips / fans / face normal.
const FLAG_BASE: u8 = 0x40;
const FLAG_LIST: u8 = 0x01;
const FLAG_STRIPS: u8 = 0x02;
const FLAG_FANS: u8 = 0x04;
const FLAG_NORMAL: u8 = 0x08;
/// Triangles the refinement aims at (refined up to 3 × 3).
const REFINE_BUDGET: usize = 500_000;

type P = [f64; 3];

/// One surface representation: welded points, triangles and per corner
/// the index of its normal.
#[derive(Debug, Default)]
struct Rep {
    points: Vec<P>,
    triangles: Vec<[u32; 3]>,
    normals: Vec<P>,
    normal_triangles: Vec<[u32; 3]>,
}

/// Whether the data is a CATIA V5 container.
pub(crate) fn is_catia(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// The preview mesh of a CATIA V5 part.
pub(crate) fn read_part(data: &[u8]) -> Result<Mesh> {
    if !is_catia(data) {
        bail!("This is not a CATIA V5 file.");
    }
    let reps = reps(data);
    if reps.is_empty() {
        bail!(
            "The CATIA file contains no visualisation data (saved without it, or a product \
             or drawing)."
        );
    }
    let total: usize = reps.iter().map(|r| r.triangles.len()).sum();
    check_triangles(total)?;
    let level = refine_level(total);
    let mut mesh = Mesh::default();
    for rep in reps {
        let (points, triangles) = if level > 1 {
            super::pn::refine(
                &rep.points,
                &rep.triangles,
                &rep.normals,
                &rep.normal_triangles,
                level,
            )
        } else {
            (rep.points, rep.triangles)
        };
        check_triangles(mesh.triangle_count() + triangles.len())?;
        let mut part = Mesh {
            parts: 1,
            ..Mesh::default()
        };
        for p in &points {
            part.vertex(*p);
        }
        part.indices.extend(triangles.iter().flatten());
        mesh.append(part);
    }
    Ok(mesh)
}

/// Subdivision level so that the refined mesh stays near the budget.
fn refine_level(triangles: usize) -> usize {
    let mut level = 3;
    while level > 1 && triangles.saturating_mul(level * level) > REFINE_BUDGET {
        level -= 1;
    }
    level
}

/// Every surface representation of the nested CGR documents.
///
/// The marker is only searched inside nested `V5_CFV2` documents (the CGR
/// container), and every hit must decode completely and consistently –
/// other records that share the marker (plane symbols) fail that check.
fn reps(data: &[u8]) -> Vec<Rep> {
    let mut reps = Vec::new();
    for start in find_all(data, MAGIC).filter(|&s| s > 0) {
        let Some(doc) = data.get(start..nested_end(data, start)) else {
            continue;
        };
        for at in find_all(doc, &REP_MARKER) {
            if let Some(rep) = doc.get(at + REP_MARKER.len()..).and_then(read_rep) {
                reps.push(rep);
            }
        }
    }
    reps
}

/// End of a nested document: behind its directory, or the end of the file.
fn nested_end(data: &[u8], start: usize) -> usize {
    let be = |at: usize| {
        data.get(at..at.checked_add(4)?)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(|b| u32::from_be_bytes(b) as usize)
    };
    match (be(start + 8), be(start + 12)) {
        (Some(offset), Some(length)) => start
            .saturating_add(offset)
            .saturating_add(length)
            .clamp(start, data.len()),
        _ => data.len(),
    }
}

/// Offsets of every occurrence of `needle`.
fn find_all<'a>(hay: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    hay.windows(needle.len())
        .enumerate()
        .filter(move |(_, w)| *w == needle)
        .map(|(i, _)| i)
}

/// A cursor over the bytes of a record; every read checks the bounds.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn left(&self) -> usize {
        self.data.len() - self.at
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        <[u8; N]>::try_from(self.bytes(N)?).ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u32_le(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    fn f32(&mut self) -> Option<f64> {
        let v = f32::from_bits(self.u32_le()?);
        v.is_finite().then_some(f64::from(v))
    }

    fn i16(&mut self) -> Option<f64> {
        Some(f64::from(i16::from_le_bytes(self.array()?)) / 32767.0)
    }

    /// A count: one byte, or `FF` and a little-endian u32.
    fn vi(&mut self) -> Option<usize> {
        match self.u8()? {
            0xFF => Some(self.u32_le()? as usize),
            b => Some(b as usize),
        }
    }

    /// A count that cannot be larger than `per`-byte items in the rest.
    fn count(&mut self, per: usize) -> Option<usize> {
        let n = self.vi()?;
        (n.saturating_mul(per) <= self.left()).then_some(n)
    }

    /// A vertex index of the given width (big endian).
    fn index(&mut self, width: usize) -> Option<u32> {
        let b = self.bytes(width)?;
        Some(b.iter().fold(0u32, |v, &x| v << 8 | u32::from(x)))
    }
}

/// Decodes one representation behind the marker, or `None` if the bytes
/// are not a complete, consistent surface mesh.
fn read_rep(data: &[u8]) -> Option<Rep> {
    let mut r = Reader { data, at: 0 };
    let nv = r.vi()?;
    let nn = r.vi()?;
    if nv == 0 || nn > nv || nv.div_ceil(4) > r.left() || r.u32_le()? != 0 {
        return None;
    }
    let vertices = read_vertices(&mut r, nv)?;
    let vertex_normals = if nn > 0 {
        read_normals(&mut r, nn)?
    } else {
        Vec::new()
    };
    let _tolerance = r.f32()?;
    r.bytes(4)?;
    let faces = r.count(3)?;
    let width = match nv {
        0..=255 => 1,
        256..=65_535 => 2,
        _ => 4,
    };

    let mut rep = Rep::default();
    // welded points: corners that only differ by their normal share one
    let mut welded: HashMap<[u64; 3], u32> = HashMap::new();
    let point_of: Vec<u32> = vertices
        .iter()
        .map(|p| {
            let key = p.map(f64::to_bits);
            *welded.entry(key).or_insert_with(|| {
                rep.points.push(*p);
                (rep.points.len() - 1) as u32
            })
        })
        .collect();
    rep.normals = vertex_normals;

    for _ in 0..faces {
        if r.u8()? != 1 {
            return None;
        }
        let flags = r.u8()?;
        if flags & FLAG_BASE == 0 || flags & !0x4F != 0 || flags & 0x07 == 0 {
            return None;
        }
        let lists = if flags & FLAG_LIST != 0 { r.vi()? } else { 0 };
        let strips = if flags & FLAG_STRIPS != 0 {
            r.count(1)?
        } else {
            0
        };
        let fans = if flags & FLAG_FANS != 0 {
            r.count(1)?
        } else {
            0
        };
        let total = r.count(width)?;
        let face_normal = if flags & FLAG_NORMAL != 0 {
            let n = [r.f32()?, r.f32()?, r.f32()?];
            rep.normals.push(n);
            Some((rep.normals.len() - 1) as u32)
        } else {
            None
        };
        let strip_lengths = (0..strips).map(|_| r.vi()).collect::<Option<Vec<_>>>()?;
        let fan_lengths = (0..fans).map(|_| r.vi()).collect::<Option<Vec<_>>>()?;
        let expected = lists
            .checked_mul(3)?
            .checked_add(
                strip_lengths
                    .iter()
                    .try_fold(0usize, |a, &b| a.checked_add(b))?,
            )?
            .checked_add(
                fan_lengths
                    .iter()
                    .try_fold(0usize, |a, &b| a.checked_add(b))?,
            )?;
        if expected != total {
            return None;
        }
        let indices = (0..total)
            .map(|_| r.index(width).filter(|&i| (i as usize) < nv))
            .collect::<Option<Vec<u32>>>()?;

        let mut corners: Vec<[u32; 3]> = Vec::new();
        let (list, mut rest) = indices.split_at_checked(lists.checked_mul(3)?)?;
        corners.extend(
            list.chunks_exact(3)
                .filter_map(|t| <[u32; 3]>::try_from(t).ok()),
        );
        for &len in &strip_lengths {
            let (s, tail) = rest.split_at_checked(len)?;
            for (j, w) in s.windows(3).enumerate() {
                if let &[a, b, c] = w {
                    corners.push(if j % 2 == 0 { [a, b, c] } else { [b, a, c] });
                }
            }
            rest = tail;
        }
        for &len in &fan_lengths {
            let (s, tail) = rest.split_at_checked(len)?;
            if let Some((&first, others)) = s.split_first() {
                for w in others.windows(2) {
                    if let &[b, c] = w {
                        corners.push([first, b, c]);
                    }
                }
            }
            rest = tail;
        }
        for c in corners {
            let t = c.map(|i| point_of.get(i as usize).copied());
            let [Some(a), Some(b), Some(d)] = t else {
                return None;
            };
            let t = [a, b, d];
            if a == b || b == d || a == d {
                continue;
            }
            let n = match face_normal {
                Some(n) => [n; 3],
                None => {
                    // a corner without a normal gets the triangle's own
                    let mut flat = None;
                    let mut n = [0u32; 3];
                    for (slot, &corner) in n.iter_mut().zip(c.iter()) {
                        *slot = if (corner as usize) < nn {
                            corner
                        } else {
                            *flat.get_or_insert_with(|| {
                                rep.normals.push(flat_normal(&rep.points, t));
                                (rep.normals.len() - 1) as u32
                            })
                        };
                    }
                    n
                }
            };
            rep.triangles.push(t);
            rep.normal_triangles.push(n);
        }
    }
    (!rep.triangles.is_empty()).then_some(rep)
}

/// The vertex block: prefix-shared coordinates.
fn read_vertices(r: &mut Reader<'_>, nv: usize) -> Option<Vec<P>> {
    let codes = r.bytes(nv.div_ceil(4))?;
    // coordinates each vertex replaces: the last (code + 3) % 4
    let replaced: Vec<usize> = codes
        .iter()
        .flat_map(|&b| (0..4).map(move |k| (usize::from((b >> (2 * k)) & 3) + 3) % 4))
        .take(nv)
        .collect();
    let floats = r.count(4)?;
    if floats != replaced.iter().sum::<usize>() {
        return None;
    }
    let mut current = [0.0f64; 3];
    let mut out = Vec::with_capacity(nv);
    for &replace in &replaced {
        for value in current.iter_mut().skip(3 - replace) {
            *value = r.f32()?;
        }
        out.push(current);
    }
    Some(out)
}

/// The normal block: two stored components and a code per normal.
fn read_normals(r: &mut Reader<'_>, nn: usize) -> Option<Vec<P>> {
    let values = r.count(2)?;
    let q = (0..values).map(|_| r.i16()).collect::<Option<Vec<_>>>()?;
    let codes = r.bytes(nn.div_ceil(2))?;
    let mut used = 0;
    let mut out = Vec::with_capacity(nn);
    let nibbles = codes.iter().flat_map(|&b| [b & 0x0F, b >> 4]).take(nn);
    for c in nibbles {
        let s = if c & 1 == 1 { -1.0 } else { 1.0 };
        let n = match c {
            0..=5 => {
                let (a, b) = (*q.get(used)?, *q.get(used + 1)?);
                used += 2;
                // the largest component, on axis c >> 1, is derived
                let d = s * (1.0 - a * a - b * b).max(0.0).sqrt();
                match c >> 1 {
                    0 => [d, a, b],
                    1 => [a, d, b],
                    _ => [a, b, d],
                }
            }
            6 | 7 => [s, 0.0, 0.0],
            8 | 9 => [0.0, s, 0.0],
            10 | 11 => [0.0, 0.0, s],
            _ => return None,
        };
        out.push(n);
    }
    (used == values).then_some(out)
}

/// The unit normal of a triangle (zero for a degenerate one).
fn flat_normal(points: &[P], t: [u32; 3]) -> P {
    let [a, b, c] = t.map(|i| points.get(i as usize).copied().unwrap_or_default());
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l > 0.0 {
        n.map(|x| x / l)
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nested document around one rep record.
    fn file_with(rep: &[u8]) -> Vec<u8> {
        let mut inner = MAGIC.to_vec();
        inner.extend([0u8; 8]);
        inner.extend([0x30, 0x04, 0x04, 0xFD]);
        inner.extend(REP_MARKER);
        inner.extend(rep);
        let len = inner.len() as u32;
        inner[8..12].copy_from_slice(&len.to_be_bytes());
        let mut file = MAGIC.to_vec();
        file.extend([0u8; 8]);
        file.extend(&inner);
        file
    }

    fn f(v: f32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// The box of the worked example: 8 vertices without normals, one
    /// planar face as a strip, one as a fan.
    fn box_rep() -> Vec<u8> {
        let mut rep = vec![8, 0, 0, 0, 0, 0, 0xB8, 0xB8, 14];
        for v in [
            -500.0, -10.0, -10.0, 10.0, 10.0, -10.0, 10.0, 500.0, -10.0, -10.0, 10.0, 10.0, -10.0,
            10.0,
        ] {
            rep.extend(f(v));
        }
        rep.extend(f(0.2));
        rep.extend([0, 4, 2, 10]);
        rep.push(2); // faces
                     // face x = -500: strip 2 0 3 1 with normal (-1, 0, 0)
        rep.extend([1, 0x4A, 1, 4]);
        for v in [-1.0, 0.0, 0.0] {
            rep.extend(f(v));
        }
        rep.extend([4, 2, 0, 3, 1]);
        // face x = 500: fan 4 5 7 6
        rep.extend([1, 0x44, 1, 4, 4, 4, 5, 7, 6]);
        rep
    }

    #[test]
    fn prefix_shared_vertices_and_strips_decode() {
        let rep = read_rep(&box_rep()).expect("rep");
        assert_eq!(rep.points.len(), 8);
        assert_eq!(rep.points[0], [-500.0, -10.0, -10.0]);
        assert_eq!(rep.points[1], [-500.0, -10.0, 10.0]);
        assert_eq!(rep.points[3], [-500.0, 10.0, 10.0]);
        assert_eq!(rep.points[7], [500.0, 10.0, 10.0]);
        assert_eq!(
            rep.triangles,
            vec![[2, 0, 3], [3, 0, 1], [4, 5, 7], [4, 7, 6]]
        );
        // the strip face uses its face normal on every corner
        assert_eq!(
            rep.normals[rep.normal_triangles[0][0] as usize],
            [-1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn a_part_file_gives_a_mesh() {
        let mesh = read_part(&file_with(&box_rep())).unwrap();
        assert_eq!(mesh.parts, 1);
        assert!(mesh.triangle_count() >= 4);
        let xs: Vec<f32> = mesh.positions.chunks(3).map(|p| p[0]).collect();
        assert!(xs.iter().all(|&x| (-500.0..=500.0).contains(&x)));
    }

    #[test]
    fn normals_are_decoded_from_two_components_and_axis_codes() {
        let mut data = vec![4]; // 4 int16 values
        for v in [0i16, 0, 16384, 0] {
            data.extend(v.to_le_bytes());
        }
        data.extend([0x54, 0x06]); // codes 4, 5 | 6, 0 → z+, z-, x+, x+
        let mut r = Reader { data: &data, at: 0 };
        let n = read_normals(&mut r, 3).unwrap();
        assert_eq!(n[0], [0.0, 0.0, 1.0]);
        assert!((n[1][0] - 0.5).abs() < 1e-4 && n[1][2] < -0.8);
        assert_eq!(n[2], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn inconsistent_records_are_skipped() {
        let good = box_rep();
        // wrong float count
        let mut bad = good.clone();
        bad[8] = 13;
        assert!(read_rep(&bad).is_none());
        // an index out of range
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] = 9;
        assert!(read_rep(&bad).is_none());
        // a file without nested CGR document
        assert!(read_part(MAGIC).is_err());
        assert!(read_part(b"ISO-10303-21;").is_err());
    }

    #[test]
    fn truncated_records_never_panic() {
        let file = file_with(&box_rep());
        for len in 0..file.len() {
            let _ = read_part(&file[..len]);
        }
        let rep = box_rep();
        for len in 0..rep.len() {
            assert!(read_rep(&rep[..len]).is_none() || len == rep.len());
        }
    }
}
