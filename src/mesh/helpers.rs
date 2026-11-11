//! Shared parsing helpers of the readers: bounded byte access, numbers in
//! text, polygon fans and the limits on triangles and points.

use anyhow::{anyhow, bail, Result};

use super::*;

/// The error of a read past the end: the file is shorter than it claims.
pub(crate) fn ends_early(at: usize) -> anyhow::Error {
    anyhow!("The file ends early (byte {at}).")
}

/// `data[from..to]`, or an error instead of a panic.
pub(crate) fn range(data: &[u8], from: usize, to: usize) -> Result<&[u8]> {
    data.get(from..to).ok_or_else(|| ends_early(from))
}

/// `N` bytes at `at`, or an error instead of a panic.
pub(crate) fn bytes_at<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]> {
    at.checked_add(N)
        .and_then(|end| data.get(at..end))
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .ok_or_else(|| ends_early(at))
}

pub(crate) fn u16_le(data: &[u8], at: usize) -> Result<u16> {
    bytes_at(data, at).map(u16::from_le_bytes)
}

pub(crate) fn u32_le(data: &[u8], at: usize) -> Result<u32> {
    bytes_at(data, at).map(u32::from_le_bytes)
}

pub(crate) fn i32_le(data: &[u8], at: usize) -> Result<i32> {
    bytes_at(data, at).map(i32::from_le_bytes)
}

pub(crate) fn f32_le(data: &[u8], at: usize) -> Result<f32> {
    bytes_at(data, at).map(f32::from_le_bytes)
}

pub(crate) fn f64_le(data: &[u8], at: usize) -> Result<f64> {
    bytes_at(data, at).map(f64::from_le_bytes)
}

pub(crate) fn u64_le(data: &[u8], at: usize) -> Result<u64> {
    bytes_at(data, at).map(u64::from_le_bytes)
}

/// The bytes in groups of four (an incomplete group at the end is dropped).
pub(crate) fn quads(data: &[u8]) -> impl Iterator<Item = [u8; 4]> + '_ {
    data.chunks_exact(4)
        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
}

pub(crate) fn text(data: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(data)
}

pub(crate) fn float(token: &str) -> Result<f64> {
    let v: f64 = token
        .parse()
        .map_err(|_| anyhow!("'{}' is not a number", truncate(token)))?;
    if !v.is_finite() {
        bail!("invalid coordinate '{}'", truncate(token));
    }
    Ok(v)
}

pub(crate) fn truncate(s: &str) -> String {
    s.chars().take(24).collect()
}

/// Polygon → triangle fan.
pub(crate) fn fan(mesh: &mut Mesh, polygon: &[u32]) {
    let Some((&first, rest)) = polygon.split_first() else {
        return;
    };
    for w in rest.windows(2) {
        if let [b, c] = *w {
            mesh.indices.extend([first, b, c]);
        }
    }
}

pub(crate) fn check_triangles(n: usize) -> Result<()> {
    if n > MAX_TRIANGLES {
        bail!("The model has {n} triangles – more than the preview shows ({MAX_TRIANGLES}).");
    }
    Ok(())
}

/// Every `stride`-th point of a point cloud with `count` points.
pub(crate) fn stride_for(count: usize) -> usize {
    count.div_ceil(MAX_POINTS).max(1)
}

/// Thins out a point cloud that has more than [`MAX_POINTS`] points.
pub(crate) fn thin_out(mesh: &mut Mesh) {
    if !mesh.is_point_cloud() || mesh.vertex_count() <= MAX_POINTS {
        return;
    }
    let stride = stride_for(mesh.vertex_count());
    let keep = |v: &[f32]| v.to_vec();
    mesh.positions = mesh
        .positions
        .chunks_exact(3)
        .step_by(stride)
        .flat_map(keep)
        .collect();
    if !mesh.colors.is_empty() {
        mesh.colors = mesh
            .colors
            .chunks_exact(3)
            .step_by(stride)
            .flat_map(|c| c.to_vec())
            .collect();
    }
    for f in &mut mesh.fields {
        f.values = f.values.iter().copied().step_by(stride).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_byte_access() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(range(&data, 1, 3).unwrap(), &[2, 3]);
        assert_eq!(range(&data, 9, 9).unwrap(), &[] as &[u8]);
        assert!(range(&data, 8, 10).is_err(), "past the end");
        assert!(range(&data, 3, 1).is_err(), "inverted");
        assert_eq!(bytes_at::<2>(&data, 7).unwrap(), [8, 9]);
        assert!(bytes_at::<2>(&data, 8).is_err());
        assert!(bytes_at::<2>(&data, usize::MAX).is_err(), "no overflow");
        let err = u32_le(&data, 7).unwrap_err().to_string();
        assert!(
            err.contains("ends early") && err.contains("byte 7"),
            "{err}"
        );
    }

    #[test]
    fn little_endian_numbers() {
        assert_eq!(u16_le(&[0x34, 0x12], 0).unwrap(), 0x1234);
        assert_eq!(u32_le(&[0x78, 0x56, 0x34, 0x12], 0).unwrap(), 0x1234_5678);
        assert_eq!(i32_le(&(-7i32).to_le_bytes(), 0).unwrap(), -7);
        assert_eq!(f32_le(&1.5f32.to_le_bytes(), 0).unwrap(), 1.5);
        assert_eq!(f64_le(&(-2.25f64).to_le_bytes(), 0).unwrap(), -2.25);
        assert_eq!(u64_le(&u64::MAX.to_le_bytes(), 0).unwrap(), u64::MAX);
        let mut at_offset = vec![0xFF; 3];
        at_offset.extend_from_slice(&9u16.to_le_bytes());
        assert_eq!(u16_le(&at_offset, 3).unwrap(), 9);
    }

    #[test]
    fn quads_drop_an_incomplete_group() {
        let groups: Vec<[u8; 4]> = quads(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).collect();
        assert_eq!(groups, [[1, 2, 3, 4], [5, 6, 7, 8]]);
        assert_eq!(quads(&[1, 2, 3]).count(), 0);
    }

    #[test]
    fn numbers_in_text() {
        assert_eq!(float("1.5").unwrap(), 1.5);
        assert_eq!(float("-2e3").unwrap(), -2000.0);
        assert!(float("1,5").is_err());
        for bad in ["nan", "inf", "-inf", "1e999"] {
            let err = float(bad).unwrap_err().to_string();
            assert!(err.contains("invalid coordinate"), "{bad}: {err}");
        }
        // the message quotes at most 24 characters of the token
        let long = "x".repeat(100);
        let err = float(&long).unwrap_err().to_string();
        assert!(
            err.contains(&"x".repeat(24)) && !err.contains(&"x".repeat(25)),
            "{err}"
        );
        assert_eq!(truncate("äöü"), "äöü");
        assert_eq!(truncate(&"ä".repeat(30)).chars().count(), 24);
        assert_eq!(text(b"ok \xFF"), "ok \u{FFFD}");
    }

    #[test]
    fn polygons_become_fans() {
        let mut mesh = Mesh::default();
        fan(&mut mesh, &[0, 1, 2, 3, 4]);
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3, 0, 3, 4]);
        let mut mesh = Mesh::default();
        fan(&mut mesh, &[7, 8, 9]);
        assert_eq!(mesh.indices, [7, 8, 9]);
        for degenerate in [&[][..], &[1], &[1, 2]] {
            let mut mesh = Mesh::default();
            fan(&mut mesh, degenerate);
            assert!(mesh.indices.is_empty(), "{degenerate:?}");
        }
    }

    #[test]
    fn triangle_and_point_limits() {
        assert!(check_triangles(MAX_TRIANGLES).is_ok());
        let err = check_triangles(MAX_TRIANGLES + 1).unwrap_err().to_string();
        assert!(err.contains("more than the preview shows"), "{err}");
        assert_eq!(stride_for(0), 1);
        assert_eq!(stride_for(MAX_POINTS), 1);
        assert_eq!(stride_for(MAX_POINTS + 1), 2);
        assert_eq!(stride_for(3 * MAX_POINTS), 3);
        assert_eq!(stride_for(3 * MAX_POINTS + 1), 4);
    }

    #[test]
    fn large_point_clouds_are_thinned_evenly() {
        // a mesh with triangles is never thinned
        let mut mesh = Mesh {
            positions: vec![0.0; 9],
            indices: vec![0, 1, 2],
            ..Mesh::default()
        };
        thin_out(&mut mesh);
        assert_eq!(mesh.vertex_count(), 3);
        // a cloud at the limit stays as it is
        let mut cloud = Mesh {
            positions: vec![0.0; 30],
            ..Mesh::default()
        };
        thin_out(&mut cloud);
        assert_eq!(cloud.vertex_count(), 10);
        // one point more: every second point, with its colour and field value
        let n = MAX_POINTS + 1;
        let mut cloud = Mesh {
            positions: (0..n).flat_map(|i| [i as f32, 0.0, 0.0]).collect(),
            colors: (0..n).flat_map(|i| [(i % 256) as u8, 0, 0]).collect(),
            fields: vec![Field {
                name: "T".into(),
                unit: String::new(),
                values: (0..n).map(|i| i as f32).collect(),
            }],
            ..Mesh::default()
        };
        thin_out(&mut cloud);
        assert_eq!(cloud.vertex_count(), n.div_ceil(2));
        assert_eq!(cloud.colors.len(), cloud.positions.len());
        assert_eq!(cloud.fields[0].values.len(), cloud.vertex_count());
        assert_eq!(&cloud.positions[3..6], &[2.0, 0.0, 0.0]);
        assert_eq!(&cloud.colors[3..6], &[2, 0, 0]);
        assert_eq!(cloud.fields[0].values[1], 2.0);
        assert!(cloud.check().is_ok());
    }
}
