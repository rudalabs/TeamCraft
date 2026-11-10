//! Point clouds: PTS and XYZ (text), LAS (binary, uncompressed).

use anyhow::{bail, Result};

use super::*;

/// `x y z [intensity] [r g b]` per line (spaces, commas or semicolons); lines
/// that are not numbers (headers, the point count of .pts files) are skipped.
pub(super) fn read_text(data: &[u8]) -> Result<Mesh> {
    let text = text(data);
    // estimate the number of points from the first lines to thin out evenly
    let sample: Vec<usize> = text.lines().take(1000).map(|l| l.len() + 1).collect();
    let average = (sample.iter().sum::<usize>() / sample.len().max(1)).max(8);
    let stride = stride_for(data.len() / average);
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    let mut colors = Vec::new();
    let mut row = 0usize;
    for line in text.lines() {
        let values: Vec<f64> = line
            .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
            .filter(|t| !t.is_empty())
            .map_while(|t| t.parse::<f64>().ok().filter(|v| v.is_finite()))
            .collect();
        let [x, y, z, rest @ ..] = values.as_slice() else {
            continue;
        };
        row += 1;
        if (row - 1) % stride != 0 {
            continue;
        }
        mesh.vertex([*x, *y, *z]);
        // the last three values are the colour (an intensity may come first)
        if let [.., r, g, b] = rest {
            let rgb = [*r, *g, *b];
            if rgb.iter().all(|c| (0.0..=255.0).contains(c)) {
                colors.extend(rgb.iter().map(|c| *c as u8));
            }
        }
        if mesh.vertex_count() > MAX_POINTS {
            break;
        }
    }
    if colors.len() == mesh.positions.len() {
        mesh.colors = colors;
    }
    Ok(mesh)
}

pub(super) fn read_las(data: &[u8]) -> Result<Mesh> {
    if data.len() < 227 || data.get(..4) != Some(b"LASF".as_slice()) {
        bail!("Not a LAS file.");
    }
    let u16_at = |at: usize| u16_le(data, at).map(usize::from);
    let u32_at = |at: usize| u32_le(data, at).map(|v| v as usize);
    let f64_at = |at: usize| f64_le(data, at);
    let byte_at = |at: usize| data.get(at).copied().ok_or_else(|| ends_early(at));
    let offset = u32_at(96)?;
    let format = byte_at(104)?;
    if format & 0x80 != 0 {
        bail!("Compressed LAZ point clouds are not previewed – export an uncompressed .las.");
    }
    let format = format & 0x3F;
    let record = u16_at(105)?;
    let mut count = u32_at(107)?;
    if count == 0 && data.len() >= 255 && byte_at(25)? >= 4 {
        count = u64_le(data, 247)? as usize;
    }
    if record < 12 || offset > data.len() {
        bail!("Invalid LAS header.");
    }
    count = count.min((data.len() - offset) / record);
    let [sx, sy, sz] = [f64_at(131)?, f64_at(139)?, f64_at(147)?];
    let [ox, oy, oz] = [f64_at(155)?, f64_at(163)?, f64_at(171)?];
    let rgb_at = match format {
        2 => Some(20),
        3 | 5 => Some(28),
        7 | 8 | 10 => Some(30),
        _ => None,
    }
    .filter(|at| at + 6 <= record);
    let stride = stride_for(count);
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    let mut raw_colors: Vec<u16> = Vec::new();
    for i in (0..count).step_by(stride) {
        let o = offset + i * record;
        let c = |k: usize| i32_le(data, o + k * 4).map(f64::from);
        mesh.vertex([c(0)? * sx + ox, c(1)? * sy + oy, c(2)? * sz + oz]);
        if let Some(at) = rgb_at {
            for k in 0..3 {
                raw_colors.push(u16_le(data, o + at + k * 2)?);
            }
        }
    }
    if !raw_colors.is_empty() {
        let shift = if raw_colors.iter().any(|c| *c > 255) {
            8
        } else {
            0
        };
        mesh.colors = raw_colors.iter().map(|c| (c >> shift) as u8).collect();
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_clouds() {
        let pts = crate::mesh::read(
            "pts",
            b"3\n0 0 0 10 255 0 0\n1 1 1 10 0 255 0\n2,2,2,10,0,0,255\n",
        )
        .unwrap();
        assert!(pts.is_point_cloud());
        assert_eq!(pts.vertex_count(), 3);
        assert_eq!(&pts.colors[..3], &[255, 0, 0]);
        let xyz = crate::mesh::read("xyz", b"# x y z\n1.5 2.5 3.5\n4 5 6\n").unwrap();
        assert_eq!(xyz.vertex_count(), 2);

        // LAS 1.2, point format 2 (with RGB), two points
        let mut las = vec![0u8; 227];
        las[..4].copy_from_slice(b"LASF");
        las[24] = 1;
        las[25] = 2;
        las[94..96].copy_from_slice(&227u16.to_le_bytes());
        las[96..100].copy_from_slice(&227u32.to_le_bytes());
        las[104] = 2;
        las[105..107].copy_from_slice(&26u16.to_le_bytes());
        las[107..111].copy_from_slice(&2u32.to_le_bytes());
        for at in [131, 139, 147] {
            las[at..at + 8].copy_from_slice(&0.01f64.to_le_bytes());
        }
        for (x, rgb) in [(100i32, 65535u16), (200, 0)] {
            let mut rec = vec![0u8; 26];
            rec[..4].copy_from_slice(&x.to_le_bytes());
            rec[20..22].copy_from_slice(&rgb.to_le_bytes());
            las.extend(rec);
        }
        let cloud = crate::mesh::read("las", &las).unwrap();
        assert_eq!(cloud.vertex_count(), 2);
        assert!((cloud.positions[3] - 2.0).abs() < 1e-6);
        assert_eq!(cloud.colors[0], 255);
        las[104] = 0x82;
        assert!(crate::mesh::read("las", &las)
            .unwrap_err()
            .to_string()
            .contains("LAZ"));
    }

    #[test]
    fn text_points_skip_comments_and_refuse_garbage() {
        let cloud = read_text(b"// header\n# comment\n\n1 2 3\n4 5 6\n").unwrap();
        assert_eq!(cloud.vertex_count(), 2);
        assert!(cloud.colors.is_empty(), "no colours given");
        // a line without three numbers is skipped, like the PTS point count;
        // nothing left is "no geometry" from the check in `read`
        assert!(read_text(b"1 2 x\n").unwrap().positions.is_empty());
        let err = crate::mesh::read("xyz", b"1 2 x\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no geometry"), "{err}");
        assert!(read_las(b"LASF").is_err(), "header cut short");
        assert!(read_las(b"not a las file at all").is_err());
    }
}
