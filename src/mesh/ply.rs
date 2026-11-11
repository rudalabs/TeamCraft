//! PLY, ASCII and binary (little and big endian).

use anyhow::{bail, Context, Result};

use super::*;

#[derive(Clone, Copy, PartialEq)]
enum PlyType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyType {
    fn parse(s: &str) -> Result<PlyType> {
        Ok(match s {
            "char" | "int8" => PlyType::I8,
            "uchar" | "uint8" => PlyType::U8,
            "short" | "int16" => PlyType::I16,
            "ushort" | "uint16" => PlyType::U16,
            "int" | "int32" => PlyType::I32,
            "uint" | "uint32" => PlyType::U32,
            "float" | "float32" => PlyType::F32,
            "double" | "float64" => PlyType::F64,
            other => bail!("unknown PLY type '{}'", truncate(other)),
        })
    }

    fn size(self) -> usize {
        match self {
            PlyType::I8 | PlyType::U8 => 1,
            PlyType::I16 | PlyType::U16 => 2,
            PlyType::I32 | PlyType::U32 | PlyType::F32 => 4,
            PlyType::F64 => 8,
        }
    }
}

struct PlyProperty {
    name: String,
    ty: PlyType,
    /// Count type of a list property.
    list: Option<PlyType>,
}

struct PlyElement {
    name: String,
    count: usize,
    properties: Vec<PlyProperty>,
}

enum PlyEncoding {
    Ascii,
    Little,
    Big,
}

/// Reads scalar values of a PLY body, binary or ASCII.
struct PlyReader<'a> {
    data: &'a [u8],
    at: usize,
    encoding: PlyEncoding,
    tokens: std::str::SplitAsciiWhitespace<'a>,
}

impl<'a> PlyReader<'a> {
    fn value(&mut self, ty: PlyType) -> Result<f64> {
        if let PlyEncoding::Ascii = self.encoding {
            return float(self.tokens.next().context("PLY file ends early")?);
        }
        let n = ty.size();
        let bytes = self
            .data
            .get(self.at..self.at + n)
            .context("PLY file ends early")?;
        self.at += n;
        let mut b = [0u8; 8];
        for (to, from) in b.iter_mut().zip(bytes) {
            *to = *from;
        }
        if let (PlyEncoding::Big, Some(value)) = (&self.encoding, b.get_mut(..n)) {
            value.reverse();
        }
        let [b0, b1, b2, b3, ..] = b;
        Ok(match ty {
            PlyType::I8 => b0 as i8 as f64,
            PlyType::U8 => b0 as f64,
            PlyType::I16 => i16::from_le_bytes([b0, b1]) as f64,
            PlyType::U16 => u16::from_le_bytes([b0, b1]) as f64,
            PlyType::I32 => i32::from_le_bytes([b0, b1, b2, b3]) as f64,
            PlyType::U32 => u32::from_le_bytes([b0, b1, b2, b3]) as f64,
            PlyType::F32 => f32::from_le_bytes([b0, b1, b2, b3]) as f64,
            PlyType::F64 => f64::from_le_bytes(b),
        })
    }
}

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    let end = data
        .windows(10)
        .position(|w| w == b"end_header")
        .context("PLY header without end_header")?;
    let header = std::str::from_utf8(data.get(..end).unwrap_or_default())
        .context("PLY header is not text")?;
    let mut body = end + 10;
    // the header ends with "end_header\n" (or "\r\n")
    while let Some(&c) = data.get(body).filter(|c| matches!(c, b'\r' | b'\n')) {
        body += 1;
        if c == b'\n' {
            break;
        }
    }
    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        bail!("Not a PLY file.");
    }
    let mut encoding = None;
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in lines {
        let w: Vec<&str> = line.split_ascii_whitespace().collect();
        match w.as_slice() {
            ["format", "ascii", ..] => encoding = Some(PlyEncoding::Ascii),
            ["format", "binary_little_endian", ..] => encoding = Some(PlyEncoding::Little),
            ["format", "binary_big_endian", ..] => encoding = Some(PlyEncoding::Big),
            ["element", name, count] => elements.push(PlyElement {
                name: name.to_string(),
                count: count.parse().context("PLY element count")?,
                properties: Vec::new(),
            }),
            ["property", "list", count_ty, ty, name] => elements
                .last_mut()
                .context("PLY property outside an element")?
                .properties
                .push(PlyProperty {
                    name: name.to_string(),
                    ty: PlyType::parse(ty)?,
                    list: Some(PlyType::parse(count_ty)?),
                }),
            ["property", ty, name] => elements
                .last_mut()
                .context("PLY property outside an element")?
                .properties
                .push(PlyProperty {
                    name: name.to_string(),
                    ty: PlyType::parse(ty)?,
                    list: None,
                }),
            _ => {}
        }
    }
    let encoding = encoding.context("PLY header without format")?;
    let body_text = match encoding {
        PlyEncoding::Ascii => {
            std::str::from_utf8(data.get(body..).unwrap_or_default()).unwrap_or("")
        }
        _ => "",
    };
    let mut reader = PlyReader {
        data,
        at: body,
        encoding,
        tokens: body_text.split_ascii_whitespace(),
    };
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    let mut colors = Vec::new();
    let vertex_count = elements
        .iter()
        .find(|e| e.name == "vertex")
        .map_or(0, |e| e.count);
    let has_faces = elements.iter().any(|e| e.name == "face" && e.count > 0);
    let stride = if has_faces {
        1
    } else {
        stride_for(vertex_count)
    };
    if vertex_count.saturating_mul(12) > data.len().saturating_mul(4) + 1024 {
        bail!("The PLY header announces more vertices than the file holds.");
    }
    let mut polygon = Vec::new();
    for element in &elements {
        let is_vertex = element.name == "vertex";
        let is_face = element.name == "face";
        let [px, py, pz, pr, pg, pb] = ["x", "y", "z", "red", "green", "blue"]
            .map(|n| element.properties.iter().position(|p| p.name == n));
        let color_ty = pr.and_then(|i| element.properties.get(i)).map(|p| p.ty);
        let color_scale = color_ty.map_or(1.0, |t| {
            if matches!(t, PlyType::F32 | PlyType::F64) {
                255.0
            } else {
                1.0
            }
        });
        let mut values = vec![0f64; element.properties.len()];
        for row in 0..element.count {
            polygon.clear();
            for (property, slot) in element.properties.iter().zip(values.iter_mut()) {
                match property.list {
                    None => *slot = reader.value(property.ty)?,
                    Some(count_ty) => {
                        let n = reader.value(count_ty)? as usize;
                        if n > 1024 {
                            bail!("PLY face with {n} corners");
                        }
                        let wanted = is_face
                            && matches!(property.name.as_str(), "vertex_indices" | "vertex_index");
                        for _ in 0..n {
                            let v = reader.value(property.ty)?;
                            if wanted {
                                if v < 0.0 || v as usize >= vertex_count {
                                    bail!("PLY face refers to a missing vertex");
                                }
                                polygon.push(v as u32);
                            }
                        }
                    }
                }
            }
            if is_vertex && row % stride == 0 {
                let get = |i: Option<usize>| i.and_then(|i| values.get(i).copied());
                let (Some(x), Some(y), Some(z)) = (get(px), get(py), get(pz)) else {
                    bail!("PLY vertices without x, y, z");
                };
                mesh.vertex([x, y, z]);
                if let (Some(r), Some(g), Some(b)) = (get(pr), get(pg), get(pb)) {
                    colors.extend([r, g, b].map(|c| (c * color_scale).clamp(0.0, 255.0) as u8));
                }
            }
            if is_face {
                fan(&mut mesh, &polygon);
                check_triangles(mesh.triangle_count())?;
            }
        }
    }
    if colors.len() == mesh.positions.len() {
        mesh.colors = colors;
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ply_ascii_and_binary() {
        let ascii = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\n\
property uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n\
0 0 0 255 0 0\n1 0 0 0 255 0\n0 1 0 0 0 255\n3 0 1 2\n";
        let mesh = crate::mesh::read("ply", ascii).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.colors.len(), 9);

        let mut bin = b"ply\nformat binary_little_endian 1.0\nelement vertex 4\nproperty double x\nproperty double y\nproperty double z\nend_header\n".to_vec();
        for i in 0..4 {
            for c in [i as f64, 0.5, -1.0] {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let cloud = crate::mesh::read("ply", &bin).unwrap();
        assert!(cloud.is_point_cloud());
        assert_eq!(cloud.vertex_count(), 4);
        assert_eq!(cloud.positions[3], 1.0);
        bin.truncate(bin.len() - 8);
        assert!(crate::mesh::read("ply", &bin).is_err(), "truncated body");
    }

    #[test]
    fn ply_big_endian_and_bad_headers() {
        let mut bin = b"ply\nformat binary_big_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            bin.extend_from_slice(&c.to_be_bytes());
        }
        bin.push(3);
        for i in [0i32, 1, 2] {
            bin.extend_from_slice(&i.to_be_bytes());
        }
        let mesh = read(&bin).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.positions[3], 1.0);
        assert!(read(b"not a ply file").is_err());
        assert!(
            read(b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\n").is_err(),
            "no end_header"
        );
    }
}
