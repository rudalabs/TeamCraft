//! Wavefront OBJ (faces, points, negative indices, vertex colours).

use anyhow::{anyhow, bail, Result};

use super::*;

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    let text = text(data);
    let mut mesh = Mesh::default();
    let mut colors: Vec<u8> = Vec::new();
    let mut groups = 0u32;
    let mut polygon = Vec::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("v") => {
                let values: Vec<f64> = it.map(float).collect::<Result<_>>()?;
                let [x, y, z, rest @ ..] = values.as_slice() else {
                    bail!("OBJ vertex with less than three coordinates");
                };
                mesh.vertex([*x, *y, *z]);
                if let [r, g, b, ..] = rest {
                    let rgb = [*r, *g, *b];
                    let scale = if rgb.iter().all(|c| *c <= 1.0) {
                        255.0
                    } else {
                        1.0
                    };
                    colors.extend(rgb.iter().map(|c| (c * scale).clamp(0.0, 255.0) as u8));
                }
            }
            Some("f") => {
                let n = mesh.vertex_count() as i64;
                polygon.clear();
                for corner in it {
                    let index: i64 = corner
                        .split('/')
                        .next()
                        .unwrap_or("")
                        .parse()
                        .map_err(|_| anyhow!("invalid OBJ face '{}'", truncate(line)))?;
                    let resolved = if index < 0 { n + index } else { index - 1 };
                    if resolved < 0 || resolved >= n {
                        bail!("OBJ face refers to a missing vertex");
                    }
                    polygon.push(resolved as u32);
                }
                fan(&mut mesh, &polygon);
                check_triangles(mesh.triangle_count())?;
            }
            Some("o") | Some("g") => groups += 1,
            _ => {}
        }
    }
    if colors.len() == mesh.positions.len() {
        mesh.colors = colors;
    }
    mesh.parts = groups.max(1);
    thin_out(&mut mesh);
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_faces_points_and_negative_indices() {
        let mesh = crate::mesh::read(
            "obj",
            b"o plate\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3/3/3 4/4/4\nf -4 -3 -2\n",
        )
        .unwrap();
        assert_eq!(mesh.triangle_count(), 3);
        assert!(
            crate::mesh::read("obj", b"v 0 0 0\nf 1 2 3\n").is_err(),
            "missing vertices"
        );
        let cloud = crate::mesh::read("obj", b"v 0 0 0 1 0 0\nv 1 2 3 0 1 0\n").unwrap();
        assert!(cloud.is_point_cloud());
        assert_eq!(cloud.colors, vec![255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn obj_groups_count_as_parts() {
        let mesh = read(b"v 0 0 0\nv 1 0 0\nv 0 1 0\no a\nf 1 2 3\ng b\nf 1 2 3\n").unwrap();
        assert_eq!((mesh.triangle_count(), mesh.parts), (2, 2));
        let single = read(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        assert_eq!(single.parts, 1, "no group is one part");
        assert!(read(b"v 0 0 x\n").is_err(), "not a number");
    }
}
