//! A small software renderer for thumbnails: an isometric view of a
//! [`Mesh`] with a z-buffer, flat shading and dark outlines where the depth
//! jumps (silhouettes, steps), written as an RGBA PNG with a transparent
//! background, so it sits on light and dark skins alike.
//!
//! Pure Rust; the PNG is written with `flate2` (zlib stream and CRC-32).
//! Cost is bounded by the triangle count (checked by [`Mesh::check`]) and
//! the pixel count.

use std::io::Write;

use super::Mesh;

/// Supersampling factor (anti-aliasing).
const SS: usize = 2;

/// The mesh drawn as a `size` × `size` PNG, or `None` for point clouds and
/// meshes without area.
pub fn png(mesh: &Mesh, size: u32) -> Option<Vec<u8>> {
    if mesh.is_point_cloud() || size == 0 {
        return None;
    }
    let rgba = draw(mesh, size as usize)?;
    Some(encode_png(size, size, &rgba))
}

type V = [f64; 3];

fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: V) -> V {
    let l = dot(a, a).sqrt();
    if l > 0.0 {
        [a[0] / l, a[1] / l, a[2] / l]
    } else {
        a
    }
}

/// RGBA pixels of the view (row major), or `None` without a visible area.
fn draw(mesh: &Mesh, size: usize) -> Option<Vec<u8>> {
    let w = size * SS;
    let points: Vec<V> = mesh
        .positions
        .chunks_exact(3)
        .map(|p| match *p {
            [x, y, z] => [f64::from(x), f64::from(y), f64::from(z)],
            _ => [0.0; 3],
        })
        .collect();
    let b = mesh.bounds();
    let centre = [
        (f64::from(b[0]) + f64::from(b[3])) / 2.0,
        (f64::from(b[1]) + f64::from(b[4])) / 2.0,
        (f64::from(b[2]) + f64::from(b[5])) / 2.0,
    ];
    // isometric camera from +x, -y, +z with z up
    let view = unit([1.0, -1.0, 0.8]);
    let right = unit(cross([0.0, 0.0, 1.0], view));
    let up = cross(view, right);
    let light = unit([
        0.75 * view[0] + 0.45 * up[0] - 0.35 * right[0],
        0.75 * view[1] + 0.45 * up[1] - 0.35 * right[1],
        0.75 * view[2] + 0.45 * up[2] - 0.35 * right[2],
    ]);
    // screen coordinates (x, y in pixels, z towards the camera)
    let projected: Vec<V> = points
        .iter()
        .map(|&p| {
            let q = sub(p, centre);
            [dot(q, right), dot(q, up), dot(q, view)]
        })
        .collect();
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in &projected {
        for ((l, h), &v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
            *l = l.min(v);
            *h = h.max(v);
        }
    }
    let half = ((hi[0] - lo[0]).max(hi[1] - lo[1]) / 2.0) * 1.08;
    if !(half.is_finite() && half > 0.0) {
        return None;
    }
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let scale = w as f64 / (2.0 * half);
    let screen: Vec<V> = projected
        .iter()
        .map(|p| {
            [
                (p[0] - mid[0]) * scale + w as f64 / 2.0,
                w as f64 / 2.0 - (p[1] - mid[1]) * scale,
                p[2],
            ]
        })
        .collect();

    let mut depth = vec![f64::NEG_INFINITY; w * w];
    let mut colour = vec![[0u8; 3]; w * w];
    let has_colours = mesh.colors.len() == mesh.positions.len();
    for t in mesh.indices.chunks_exact(3) {
        let [ia, ib, ic] = match *t {
            [a, b, c] => [a as usize, b as usize, c as usize],
            _ => continue,
        };
        let (Some(&a), Some(&b), Some(&c)) = (screen.get(ia), screen.get(ib), screen.get(ic))
        else {
            continue;
        };
        let (Some(&pa), Some(&pb), Some(&pc)) = (points.get(ia), points.get(ib), points.get(ic))
        else {
            continue;
        };
        let normal = unit(cross(sub(pb, pa), sub(pc, pa)));
        let shade = if dot(normal, view) >= 0.0 {
            0.22 + 0.78 * dot(normal, light).clamp(0.0, 1.0)
        } else {
            0.12 // inside seen through an open mesh
        };
        let base = if has_colours {
            let c = |i: usize, k: usize| f64::from(*mesh.colors.get(i * 3 + k).unwrap_or(&0));
            [0, 1, 2].map(|k| (c(ia, k) + c(ib, k) + c(ic, k)) / 3.0)
        } else {
            [150.0, 170.0, 192.0]
        };
        let rgb = base.map(|v| (v * shade * 1.35).clamp(0.0, 255.0) as u8);
        fill(&mut depth, &mut colour, w, [a, b, c], rgb);
    }
    if depth.iter().all(|d| d.is_infinite()) {
        return None;
    }
    outline(&depth, &mut colour, w, hi[2] - lo[2]);
    Some(downsample(&depth, &colour, w, size))
}

/// Rasterises one triangle into the z-buffer.
fn fill(depth: &mut [f64], colour: &mut [[u8; 3]], w: usize, [a, b, c]: [V; 3], rgb: [u8; 3]) {
    let area = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if area.abs() < 1e-12 || !area.is_finite() {
        return;
    }
    let max = (w - 1) as f64;
    let x0 = a[0].min(b[0]).min(c[0]).floor().clamp(0.0, max) as usize;
    let x1 = a[0].max(b[0]).max(c[0]).ceil().clamp(0.0, max) as usize;
    let y0 = a[1].min(b[1]).min(c[1]).floor().clamp(0.0, max) as usize;
    let y1 = a[1].max(b[1]).max(c[1]).ceil().clamp(0.0, max) as usize;
    for y in y0..=y1 {
        let py = y as f64 + 0.5;
        for x in x0..=x1 {
            let px = x as f64 + 0.5;
            let w0 = ((b[1] - c[1]) * (px - c[0]) + (c[0] - b[0]) * (py - c[1])) / area;
            let w1 = ((c[1] - a[1]) * (px - c[0]) + (a[0] - c[0]) * (py - c[1])) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                continue;
            }
            let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
            let i = y * w + x;
            if let (Some(d), Some(col)) = (depth.get_mut(i), colour.get_mut(i)) {
                if z > *d {
                    *d = z;
                    *col = rgb;
                }
            }
        }
    }
}

/// Darkens pixels where the depth jumps (silhouettes and steps).
fn outline(depth: &[f64], colour: &mut [[u8; 3]], w: usize, range: f64) {
    let jump = range * 0.02;
    let far = |d: f64| if d.is_finite() { d } else { -1e30 };
    let mut edge = vec![false; w * w];
    for y in 0..w {
        for x in 0..w {
            let i = y * w + x;
            let d = far(*depth.get(i).unwrap_or(&f64::NEG_INFINITY));
            let right = (x + 1 < w).then(|| far(*depth.get(i + 1).unwrap_or(&f64::NEG_INFINITY)));
            let below = (y + 1 < w).then(|| far(*depth.get(i + w).unwrap_or(&f64::NEG_INFINITY)));
            if [right, below]
                .into_iter()
                .flatten()
                .any(|n| (n - d).abs() > jump)
            {
                if let Some(e) = edge.get_mut(i) {
                    *e = true;
                }
            }
        }
    }
    for (c, e) in colour.iter_mut().zip(&edge) {
        if *e {
            *c = c.map(|v| (f64::from(v) * 0.35) as u8);
        }
    }
}

/// Averages `SS` × `SS` blocks into RGBA; empty pixels are transparent.
fn downsample(depth: &[f64], colour: &[[u8; 3]], w: usize, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let (mut sum, mut covered) = ([0u32; 3], 0u32);
            for dy in 0..SS {
                for dx in 0..SS {
                    let i = (y * SS + dy) * w + x * SS + dx;
                    if depth.get(i).is_some_and(|d| d.is_finite()) {
                        if let Some(c) = colour.get(i) {
                            for (s, &v) in sum.iter_mut().zip(c) {
                                *s += u32::from(v);
                            }
                            covered += 1;
                        }
                    }
                }
            }
            if covered == 0 {
                out.extend([0, 0, 0, 0]);
            } else {
                out.extend(sum.map(|s| (s / covered) as u8));
                out.push((covered * 255 / (SS * SS) as u32) as u8);
            }
        }
    }
    out
}

/// An 8-bit RGBA PNG (no filtering, zlib-compressed).
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks(width as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // writing into a Vec cannot fail
    let compressed: Vec<u8> = z
        .write_all(&raw)
        .and_then(|_| z.finish())
        .unwrap_or_default();
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8 bit, RGBA, deflate, no filter set, no interlace
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &compressed);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    png.extend(kind);
    png.extend(data);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    png.extend(crc.sum().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mesh {
        let p = [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 1., 1.],
            [0., 1., 1.],
        ];
        let faces = [
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        Mesh {
            positions: p.iter().flatten().map(|&v: &f64| v as f32).collect(),
            indices: faces.iter().flatten().copied().collect(),
            parts: 1,
            ..Mesh::default()
        }
    }

    #[test]
    fn a_cube_becomes_a_png_with_a_transparent_background() {
        let png = png(&cube(), 64).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        // IHDR: 64 × 64, RGBA
        assert_eq!(png.get(16..24), Some(&[0, 0, 0, 64, 0, 0, 0, 64][..]));
        assert_eq!(png.get(25), Some(&6));
        let rgba = draw(&cube(), 64).unwrap();
        let alpha: Vec<u8> = rgba.chunks(4).filter_map(|p| p.get(3).copied()).collect();
        assert_eq!(alpha.first(), Some(&0), "corner is background");
        assert_eq!(alpha.get(32 * 64 + 32), Some(&255), "centre is the cube");
        // three visible sides with three different shades
        let mut shades: Vec<&[u8]> = rgba
            .chunks(4)
            .filter(|p| p.get(3) == Some(&255))
            .filter_map(|p| p.get(..3))
            .collect();
        shades.sort();
        shades.dedup();
        assert!(shades.len() >= 3, "{shades:?}");
    }

    #[test]
    fn png_chunks_carry_valid_checksums() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
        let mut at = 8;
        let mut kinds = Vec::new();
        let be =
            |from: usize| u32::from_be_bytes(png.get(from..from + 4).unwrap().try_into().unwrap());
        while at < png.len() {
            let len = be(at) as usize;
            let kind = png.get(at + 4..at + 8).unwrap();
            let data = png.get(at + 8..at + 8 + len).unwrap();
            let crc = be(at + 8 + len);
            let mut c = flate2::Crc::new();
            c.update(kind);
            c.update(data);
            assert_eq!(c.sum(), crc);
            kinds.push(String::from_utf8_lossy(kind).to_string());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    }

    #[test]
    fn point_clouds_and_flat_nothing_give_no_image() {
        let mut points = cube();
        points.indices.clear();
        assert!(png(&points, 32).is_none());
        let empty = Mesh {
            positions: vec![0.0; 9],
            indices: vec![0, 1, 2],
            ..Mesh::default()
        };
        assert!(png(&empty, 32).is_none());
    }
}
