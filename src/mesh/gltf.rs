//! glTF 2.0 and GLB with embedded buffers.

use anyhow::{anyhow, bail, Context, Result};

use super::*;

pub(super) fn read(data: &[u8]) -> Result<Mesh> {
    use base64::Engine;
    // gltf-json indexes the accessors named by a primitive without a range
    // check while validating (a POSITION index past the accessor list
    // panics), so a hostile file must not be able to unwind through here.
    let document = std::panic::catch_unwind(|| ::gltf::Gltf::from_slice(data))
        .map_err(|_| anyhow!("invalid glTF (inconsistent accessor references)"))?
        .context("invalid glTF")?;
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for buffer in document.buffers() {
        let bytes = match buffer.source() {
            ::gltf::buffer::Source::Bin => {
                document.blob.clone().context("GLB without binary chunk")?
            }
            ::gltf::buffer::Source::Uri(uri) => {
                let Some((_, payload)) = uri
                    .strip_prefix("data:")
                    .and_then(|rest| rest.split_once(";base64,"))
                else {
                    bail!("This glTF file refers to external buffers ({}); upload the .glb version to preview it.", truncate(uri));
                };
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .context("invalid base64 buffer")?
            }
        };
        buffers.push(bytes);
    }
    let mut out = Mesh::default();
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next());
    let roots: Vec<::gltf::Node> = match scene {
        Some(scene) => scene.nodes().collect(),
        None => document.nodes().collect(),
    };
    for node in roots {
        gltf_node(&node, &IDENTITY4, &buffers, &mut out, 0)?;
    }
    Ok(out)
}

const IDENTITY4: [[f32; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];

/// Column-major 4×4 product `a · b`.
fn mul4(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut m = [[0f32; 4]; 4];
    for (column, b_column) in m.iter_mut().zip(b) {
        for (r, value) in column.iter_mut().enumerate() {
            *value = a
                .iter()
                .zip(b_column)
                .map(|(a_column, b)| a_column.get(r).map_or(0.0, |a| a * b))
                .sum();
        }
    }
    m
}

fn gltf_node(
    node: &::gltf::Node,
    parent: &[[f32; 4]; 4],
    buffers: &[Vec<u8>],
    out: &mut Mesh,
    depth: usize,
) -> Result<()> {
    if depth > 64 {
        bail!("glTF nodes nest too deeply");
    }
    let world = mul4(parent, &node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            let points = match primitive.mode() {
                ::gltf::mesh::Mode::Triangles => false,
                ::gltf::mesh::Mode::Points => true,
                _ => continue,
            };
            let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let mut part = Mesh {
                parts: 1,
                ..Mesh::default()
            };
            for [x, y, z] in positions {
                // row r of the column-major matrix times (x, y, z, 1)
                let t = |r: usize| -> f32 {
                    world
                        .iter()
                        .zip([x, y, z, 1.0])
                        .map(|(column, s)| column.get(r).map_or(0.0, |v| v * s))
                        .sum()
                };
                part.positions.extend([t(0), t(1), t(2)]);
            }
            if !points {
                match reader.read_indices() {
                    Some(indices) => part.indices.extend(indices.into_u32()),
                    None => part.indices.extend(0..part.vertex_count() as u32),
                }
                let n = part.vertex_count() as u32;
                if part.indices.len() % 3 != 0 || part.indices.iter().any(|i| *i >= n) {
                    bail!("glTF primitive with invalid indices");
                }
            }
            if let Some(colors) = reader.read_colors(0) {
                part.colors = colors.into_rgb_u8().flatten().collect();
                if part.colors.len() != part.positions.len() {
                    part.colors.clear();
                }
            }
            out.append(part);
            check_triangles(out.triangle_count())?;
        }
    }
    for child in node.children() {
        gltf_node(&child, &world, buffers, out, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gltf_embedded_buffer() {
        use base64::Engine;
        let mut bin = Vec::new();
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bin)
        );
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
"nodes":[{{"mesh":0,"translation":[0,0,2]}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
"bufferViews":[{{"buffer":0,"byteLength":36}}],
"buffers":[{{"byteLength":36,"uri":"{uri}"}}]}}"#
        );
        let mesh = crate::mesh::read("gltf", json.as_bytes()).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.positions[2], 2.0, "node translation applied");
        let external = json.replace(&uri, "model.bin");
        let err = crate::mesh::read("gltf", external.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(err.contains("external buffers"), "{err}");
    }

    #[test]
    fn gltf_refuses_what_it_cannot_show() {
        assert!(read(b"{ not json").is_err());
        let no_meshes = br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[]}],"scene":0}"#;
        assert!(crate::mesh::read("gltf", no_meshes).is_err(), "no geometry");
    }
}
