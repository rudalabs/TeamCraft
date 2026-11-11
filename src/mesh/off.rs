//! OFF (Object File Format).

use anyhow::{bail, Context, Result};

use super::*;

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    let text = text(data);
    let mut lines = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty());
    let first = lines.next().context("empty OFF file")?;
    let counts_line = match first {
        "OFF" | "COFF" | "NOFF" | "CNOFF" => lines.next().context("OFF file without counts")?,
        other if other.starts_with("OFF") => other.trim_start_matches("OFF"),
        _ => bail!("Not an OFF file."),
    };
    let counts: Vec<usize> = counts_line
        .split_ascii_whitespace()
        .map(|t| t.parse().context("OFF counts"))
        .collect::<Result<_>>()?;
    let (nv, nf) = (*counts.first().unwrap_or(&0), *counts.get(1).unwrap_or(&0));
    check_triangles(nf)?;
    if nv > data.len() {
        bail!("The OFF header announces more vertices than the file holds.");
    }
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    for _ in 0..nv {
        let line = lines.next().context("OFF file ends early")?;
        let v: Vec<f64> = line
            .split_ascii_whitespace()
            .take(3)
            .map(float)
            .collect::<Result<_>>()?;
        let [x, y, z] = *v.as_slice() else {
            bail!("OFF vertex with less than three coordinates");
        };
        mesh.vertex([x, y, z]);
    }
    let mut polygon = Vec::new();
    for _ in 0..nf {
        let line = lines.next().context("OFF file ends early")?;
        let mut it = line.split_ascii_whitespace();
        let n: usize = it.next().unwrap_or("0").parse().context("OFF face")?;
        polygon.clear();
        for _ in 0..n {
            let i: usize = it.next().context("OFF face")?.parse().context("OFF face")?;
            if i >= nv {
                bail!("OFF face refers to a missing vertex");
            }
            polygon.push(i as u32);
        }
        fan(&mut mesh, &polygon);
    }
    thin_out(&mut mesh);
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_polygons_become_triangles() {
        let mesh = crate::mesh::read(
            "off",
            b"OFF\n# square\n4 1 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n",
        )
        .unwrap();
        assert_eq!(mesh.triangle_count(), 2);
        let cloud = read(b"OFF\n2 0 0\n0 0 0\n1 1 1\n").unwrap();
        assert!(cloud.is_point_cloud());
        assert!(read(b"PLY\n").is_err(), "no OFF header");
        assert!(
            read(b"OFF\n2 1 0\n0 0 0\n1 1 1\n3 0 1 7\n").is_err(),
            "index out of range"
        );
    }
}
