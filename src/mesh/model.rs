//! The mesh itself: [`Mesh`], scalar [`Field`]s, the colour map and the `TCMESH01` wire format the viewer draws.

use anyhow::{bail, Context, Result};

use super::*;

/// Largest triangle count that is previewed.
pub const MAX_TRIANGLES: usize = 8_000_000;
/// Point clouds with more points are thinned out evenly.
pub const MAX_POINTS: usize = 3_000_000;

const MAGIC: &[u8; 8] = b"TCMESH01";
/// Colour of parts without own colour when they are merged with coloured ones.
const NEUTRAL: [u8; 3] = [176, 188, 200];

/// Triangles (or points when `indices` is empty) in model coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    /// x, y, z per vertex.
    pub positions: Vec<f32>,
    /// Three vertex indices per triangle; empty for a point cloud.
    pub indices: Vec<u32>,
    /// r, g, b per vertex – empty, or three bytes for every vertex.
    pub colors: Vec<u8>,
    /// Number of bodies / objects (for the status line of the viewer).
    pub parts: u32,
    /// Scalar result fields, one value per vertex (simulation results).
    pub fields: Vec<Field>,
    /// A partial preview: (faces shown, faces in the file) – STEP files
    /// whose B-rep the kernel could tessellate only in part.
    pub faces: Option<(u32, u32)>,
}

/// Fields a mesh carries at most (the viewer offers them in a selector).
pub const MAX_FIELDS: usize = 8;

/// A scalar result field on the vertices of a [`Mesh`]: a measured value,
/// the magnitude of a vector or the von Mises stress of a tensor. Values
/// that are not finite mark vertices without a value (drawn neutral).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Field {
    pub name: String,
    /// Unit if the file names one, otherwise empty.
    pub unit: String,
    pub values: Vec<f32>,
}

impl Field {
    /// Smallest and largest finite value (`(0, 0)` when there is none).
    pub fn range(&self) -> (f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &v in &self.values {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if lo > hi {
            (0.0, 0.0)
        } else {
            (lo, hi)
        }
    }
}

/// Stops of the colour map (perceptually uniform, after matplotlib's
/// "viridis", CC0). `shared/js/viewer.js` has the same table.
const COLOR_MAP: [[u8; 3]; 9] = [
    [68, 1, 84],
    [71, 45, 123],
    [59, 82, 139],
    [44, 114, 142],
    [33, 145, 140],
    [40, 174, 128],
    [94, 201, 98],
    [173, 220, 48],
    [253, 231, 37],
];

/// Colour of `t` in `0..=1` on the colour map (NaN = neutral grey).
pub fn color_map(t: f32) -> [u8; 3] {
    if !t.is_finite() {
        return NEUTRAL;
    }
    let x = t.clamp(0.0, 1.0) * (COLOR_MAP.len() - 1) as f32;
    let i = (x.floor() as usize).min(COLOR_MAP.len() - 2);
    let f = x - i as f32;
    let (Some(a), Some(b)) = (COLOR_MAP.get(i), COLOR_MAP.get(i + 1)) else {
        return NEUTRAL;
    };
    let mut rgb = [0u8; 3];
    for ((out, &a), &b) in rgb.iter_mut().zip(a).zip(b) {
        *out = (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    }
    rgb
}

impl Mesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 3
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn is_point_cloud(&self) -> bool {
        self.indices.is_empty()
    }

    pub(crate) fn vertex(&mut self, p: [f64; 3]) -> u32 {
        let index = self.vertex_count() as u32;
        self.positions
            .extend([p[0] as f32, p[1] as f32, p[2] as f32]);
        index
    }

    /// Appends `other` (indices are shifted; colours are filled in so that
    /// either every vertex or none has a colour).
    pub fn append(&mut self, mut other: Mesh) {
        let offset = self.vertex_count() as u32;
        // fields: those both meshes have (by name) survive
        if self.positions.is_empty() {
            self.fields = std::mem::take(&mut other.fields);
        } else {
            let mut theirs = std::mem::take(&mut other.fields);
            self.fields
                .retain_mut(|f| match theirs.iter_mut().find(|o| o.name == f.name) {
                    Some(o) => {
                        f.values.append(&mut o.values);
                        true
                    }
                    None => false,
                });
        }
        if self.colors.is_empty() != other.colors.is_empty() {
            if self.colors.is_empty() {
                self.colors = NEUTRAL.repeat(self.vertex_count());
            } else {
                other.colors = NEUTRAL.repeat(other.vertex_count());
            }
        }
        self.positions.append(&mut other.positions);
        self.indices
            .extend(other.indices.iter().map(|i| i + offset));
        self.colors.append(&mut other.colors);
        self.parts += other.parts.max(1);
    }

    /// Validates the mesh: finite coordinates, indices in range, limits.
    pub fn check(&self) -> Result<()> {
        if self.positions.is_empty() {
            bail!("The file contains no geometry.");
        }
        if self.positions.len() % 3 != 0 || self.indices.len() % 3 != 0 {
            bail!("Internal error: incomplete vertices or triangles.");
        }
        if !self.colors.is_empty() && self.colors.len() != self.positions.len() {
            bail!("Internal error: colours do not match the vertices.");
        }
        if self.fields.len() > MAX_FIELDS
            || self
                .fields
                .iter()
                .any(|f| f.values.len() != self.vertex_count())
        {
            bail!("Internal error: result fields do not match the vertices.");
        }
        if self.triangle_count() > MAX_TRIANGLES {
            bail!(
                "The model has {} triangles – more than the preview shows ({}).",
                self.triangle_count(),
                MAX_TRIANGLES
            );
        }
        if self.is_point_cloud() && self.vertex_count() > MAX_POINTS {
            bail!("Internal error: point cloud not thinned out.");
        }
        if self.positions.iter().any(|v| !v.is_finite()) {
            bail!("The file contains invalid coordinates (NaN or infinite).");
        }
        let n = self.vertex_count() as u32;
        if self.indices.iter().any(|&i| i >= n) {
            bail!("Internal error: triangle index out of range.");
        }
        Ok(())
    }

    /// Bounding box `[min x, min y, min z, max x, max y, max z]`.
    pub fn bounds(&self) -> [f32; 6] {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in self.positions.chunks_exact(3) {
            for ((lo, hi), &c) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *lo = lo.min(c);
                *hi = hi.max(c);
            }
        }
        let [x0, y0, z0] = lo;
        let [x1, y1, z1] = hi;
        [x0, y0, z0, x1, y1, z1]
    }

    /// The binary form read by `shared/js/viewer.js`:
    ///
    /// ```text
    /// 0   "TCMESH01"
    /// 8   u32 vertices   12  u32 indices   16  u32 parts
    /// 20  u32 flags (1 = colours, 2 = fields, 4 = partial preview)
    /// 24  f32 × 6 bounding box
    /// 48  f32 × 3·vertices, u32 × indices, u8 × 3·vertices (colours, padded to 4 bytes)
    ///     only with flag 2, after that:
    ///     u32 field count, then per field
    ///     u32 n, n bytes name (UTF-8), u32 m, m bytes unit, padding to 4 bytes,
    ///     f32 min, f32 max, f32 × vertices (NaN = no value)
    /// with flag 4, the last 8 bytes of the file: u32 faces shown, u32 faces in the file
    /// ```
    ///
    /// Viewers that do not know flag 2 ignore the field section; meshes
    /// with fields also carry the first field as colours for them. The face
    /// count sits at the very end, so viewers that do not know flag 4 – and
    /// the field section in between – are not affected.
    pub fn encode(&self) -> Vec<u8> {
        let field_bytes: usize = self
            .fields
            .iter()
            .map(|f| 20 + f.name.len() + f.unit.len() + f.values.len() * 4)
            .sum();
        let mut out = Vec::with_capacity(
            56 + self.positions.len() * 4
                + self.indices.len() * 4
                + self.colors.len()
                + field_bytes
                + 4,
        );
        out.extend_from_slice(MAGIC);
        let flags = u32::from(!self.colors.is_empty())
            | (u32::from(!self.fields.is_empty()) << 1)
            | (u32::from(self.faces.is_some()) << 2);
        for v in [
            self.vertex_count() as u32,
            self.indices.len() as u32,
            self.parts.max(1),
            flags,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.bounds() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.positions {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.indices {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.colors);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        if !self.fields.is_empty() {
            out.extend_from_slice(&(self.fields.len() as u32).to_le_bytes());
            for f in &self.fields {
                for s in [&f.name, &f.unit] {
                    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
                while out.len() % 4 != 0 {
                    out.push(0);
                }
                let (lo, hi) = f.range();
                out.extend_from_slice(&lo.to_le_bytes());
                out.extend_from_slice(&hi.to_le_bytes());
                for v in &f.values {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        // last, so that viewers that do not know flag 4 are not affected
        if let Some((shown, total)) = self.faces {
            out.extend_from_slice(&shown.to_le_bytes());
            out.extend_from_slice(&total.to_le_bytes());
        }
        out
    }

    /// Sets the colours from the first field (for viewers that do not know
    /// fields); vertices without a value are neutral.
    pub fn bake_colors(&mut self) {
        let Some(field) = self.fields.first() else {
            return;
        };
        let (lo, hi) = field.range();
        let span = if hi > lo { hi - lo } else { 1.0 };
        self.colors = field
            .values
            .iter()
            .flat_map(|&v| {
                color_map(if v.is_finite() {
                    (v - lo) / span
                } else {
                    f32::NAN
                })
            })
            .collect();
    }

    /// Reads [`Mesh::encode`] output (tests, tools). Every size is checked,
    /// so a damaged cache file is an error, not a panic.
    pub fn decode(data: &[u8]) -> Result<Mesh> {
        if data.len() < 48 || data.get(..8) != Some(MAGIC.as_slice()) {
            bail!("not a TeamCraft mesh");
        }
        let u = |at: usize| -> Result<usize> {
            Ok(u32_le(data, at).context("truncated mesh")? as usize)
        };
        let (vertices, indices, parts, flags) = (u(8)?, u(12)?, u(16)?, u(20)?);
        let colors = if flags & 1 == 1 { vertices * 3 } else { 0 };
        let need = 48 + vertices * 12 + indices * 4 + colors;
        if data.len() < need {
            bail!("truncated mesh");
        }
        let positions = quads(range(data, 48, 48 + vertices * 12)?)
            .map(f32::from_le_bytes)
            .collect();
        let at = 48 + vertices * 12;
        let index_values = quads(range(data, at, at + indices * 4)?)
            .map(u32::from_le_bytes)
            .collect();
        let at = at + indices * 4;
        // end of the body: after the colours (padded) and the field section
        let mut end = (at + colors).div_ceil(4) * 4;
        let mut fields = Vec::new();
        if flags & 2 == 2 {
            let mut at = end;
            let count = u(at)?;
            at += 4;
            if count > MAX_FIELDS {
                bail!("too many fields");
            }
            for _ in 0..count {
                let mut text = || -> Result<String> {
                    let n = u(at)?;
                    let s = data.get(at + 4..at + 4 + n).context("truncated mesh")?;
                    at += 4 + n;
                    Ok(String::from_utf8_lossy(s).into_owned())
                };
                let (name, unit) = (text()?, text()?);
                at = at.div_ceil(4) * 4 + 8;
                let values_end = vertices
                    .checked_mul(4)
                    .and_then(|n| n.checked_add(at))
                    .context("truncated mesh")?;
                let values = quads(data.get(at..values_end).context("truncated mesh")?)
                    .map(f32::from_le_bytes)
                    .collect();
                at = values_end;
                fields.push(Field { name, unit, values });
            }
            end = at;
        }
        // flag 4: the face count in the last 8 bytes, after everything else
        let faces = if flags & 4 == 4 {
            if data.len() < end + 8 {
                bail!("truncated mesh");
            }
            let tail = data.len() - 8;
            Some((u(tail)? as u32, u(tail + 4)? as u32))
        } else {
            None
        };
        Ok(Mesh {
            positions,
            indices: index_values,
            colors: range(data, at, at + colors)?.to_vec(),
            parts: parts as u32,
            fields,
            faces,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::test_support::cube_stl_ascii;

    fn cube() -> Mesh {
        crate::mesh::read("stl", cube_stl_ascii().as_bytes()).unwrap()
    }

    #[test]
    fn encode_round_trip_and_limits() {
        let mut mesh = cube();
        mesh.colors = vec![9; mesh.positions.len()];
        let bytes = mesh.encode();
        assert_eq!(bytes.len() % 4, 0);
        assert_eq!(Mesh::decode(&bytes).unwrap(), mesh);
        assert!(Mesh::decode(&bytes[..40]).is_err());
        let mut a = Mesh::default();
        a.append(Mesh {
            positions: vec![0., 0., 0.],
            ..Mesh::default()
        });
        a.append(Mesh {
            positions: vec![1., 1., 1.],
            colors: vec![1, 2, 3],
            ..Mesh::default()
        });
        assert_eq!(a.colors, vec![176, 188, 200, 1, 2, 3]);
        assert!(Mesh {
            positions: vec![f32::NAN, 0., 0.],
            ..Mesh::default()
        }
        .check()
        .is_err());
        assert!(Mesh::default().check().is_err());
    }

    #[test]
    fn partial_previews_carry_their_face_count() {
        let mut mesh = cube();
        mesh.faces = Some((5, 6));
        let bytes = mesh.encode();
        assert_eq!(bytes.len() % 4, 0);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 4);
        assert_eq!(Mesh::decode(&bytes).unwrap(), mesh);
        // a complete preview has no face count
        mesh.faces = None;
        let complete = mesh.encode();
        assert_eq!(complete.len() + 8, bytes.len());
        assert_eq!(Mesh::decode(&complete).unwrap().faces, None);
        // a damaged face count is an error, not a panic
        assert!(Mesh::decode(&bytes[..bytes.len() - 4]).is_err());
    }

    fn flags(bytes: &[u8]) -> u32 {
        u32::from_le_bytes(bytes[20..24].try_into().unwrap())
    }

    #[test]
    fn fields_and_face_counts_combine() {
        let mut mesh = cube();
        let n = mesh.vertex_count();
        mesh.fields = vec![
            Field {
                name: "Displacement".into(),
                unit: "mm".into(),
                values: (0..n).map(|i| i as f32 * 0.5).collect(),
            },
            Field {
                name: "S".into(),
                unit: String::new(),
                values: (0..n)
                    .map(|i| if i == 0 { f32::NAN } else { i as f32 })
                    .collect(),
            },
        ];
        mesh.bake_colors();

        // plain mesh: no extension flags
        let plain = Mesh {
            fields: Vec::new(),
            colors: Vec::new(),
            ..mesh.clone()
        };
        let plain_bytes = plain.encode();
        assert_eq!(flags(&plain_bytes), 0);
        assert_eq!(Mesh::decode(&plain_bytes).unwrap(), plain);

        // fields only (flag 2, with the baked colours: flag 1)
        let fields_only = mesh.encode();
        assert_eq!(flags(&fields_only), 1 | 2);
        let decoded = Mesh::decode(&fields_only).unwrap();
        assert_eq!(decoded.faces, None);
        assert_eq!(decoded.fields.len(), 2);
        assert_eq!(decoded.fields[0], mesh.fields[0]);
        assert!(decoded.fields[1].values[0].is_nan());
        assert_eq!(decoded.fields[1].values[1..], mesh.fields[1].values[1..]);

        // face counts only (flag 4)
        let faces_only = Mesh {
            fields: Vec::new(),
            faces: Some((3, 7)),
            ..mesh.clone()
        };
        let faces_bytes = faces_only.encode();
        assert_eq!(flags(&faces_bytes), 1 | 4);
        assert_eq!(Mesh::decode(&faces_bytes).unwrap(), faces_only);

        // both: the field section first, the face count in the last 8 bytes
        mesh.faces = Some((5, 6));
        let both = mesh.encode();
        assert_eq!(both.len() % 4, 0);
        assert_eq!(flags(&both), 1 | 2 | 4);
        assert_eq!(both.len(), fields_only.len() + 8);
        // apart from the flags word, the body is the fields-only encoding
        assert_eq!(both[..20], fields_only[..20]);
        assert_eq!(both[24..fields_only.len()], fields_only[24..]);
        assert_eq!(both[both.len() - 8..both.len() - 4], 5u32.to_le_bytes());
        assert_eq!(both[both.len() - 4..], 6u32.to_le_bytes());
        let decoded = Mesh::decode(&both).unwrap();
        assert_eq!(decoded.faces, Some((5, 6)));
        assert_eq!(decoded.fields.len(), 2);
        assert_eq!(decoded.fields[0], mesh.fields[0]);
        assert_eq!(decoded.colors, mesh.colors);
        // the face count must not overlap the field section
        assert!(Mesh::decode(&both[..both.len() - 4]).is_err());
        let mut short = fields_only.clone();
        short[20] |= 4;
        short.extend_from_slice(&[0; 4]);
        assert!(Mesh::decode(&short).is_err());
    }

    #[test]
    fn the_colour_map_runs_from_the_first_to_the_last_stop() {
        assert_eq!(color_map(0.0), COLOR_MAP[0]);
        assert_eq!(color_map(1.0), COLOR_MAP[8]);
        assert_eq!(color_map(0.5), COLOR_MAP[4], "the middle stop");
        // values outside 0..=1 are clamped, non-finite ones are neutral grey
        assert_eq!(color_map(-3.0), COLOR_MAP[0]);
        assert_eq!(color_map(7.0), COLOR_MAP[8]);
        for t in [f32::NAN, f32::INFINITY] {
            assert_eq!(color_map(t), NEUTRAL, "{t}");
        }
        // half way between two stops is half way in every channel
        let half = color_map(1.0 / 16.0);
        for c in 0..3 {
            let mid = (COLOR_MAP[0][c] as f32 + COLOR_MAP[1][c] as f32) / 2.0;
            assert!((half[c] as f32 - mid).abs() <= 0.5, "{half:?}");
        }
    }

    #[test]
    fn append_shifts_indices_and_counts_parts() {
        let triangle = || Mesh {
            positions: vec![0., 0., 0., 1., 0., 0., 0., 1., 0.],
            indices: vec![0, 1, 2],
            parts: 1,
            ..Mesh::default()
        };
        let mut a = triangle();
        a.append(triangle());
        assert_eq!(a.indices, [0, 1, 2, 3, 4, 5]);
        assert_eq!((a.vertex_count(), a.parts), (6, 2));
        // a part count of 0 still counts as one part
        a.append(Mesh {
            parts: 0,
            ..triangle()
        });
        assert_eq!(a.parts, 3);
        // fields survive only when both sides have them
        let with_field = |name: &str| Mesh {
            fields: vec![Field {
                name: name.into(),
                unit: String::new(),
                values: vec![1.0; 3],
            }],
            ..triangle()
        };
        let mut b = with_field("T");
        b.append(with_field("T"));
        assert_eq!(b.fields[0].values.len(), 6);
        let mut c = with_field("T");
        c.append(with_field("S"));
        assert!(c.fields.is_empty());
        assert!(b.check().is_ok() && c.check().is_ok());
    }

    #[test]
    fn check_refuses_broken_meshes() {
        let msg = |m: Mesh| m.check().unwrap_err().to_string();
        assert!(msg(Mesh::default()).contains("no geometry"));
        assert!(msg(Mesh {
            positions: vec![0.0; 9],
            indices: vec![0, 1, 3],
            ..Mesh::default()
        })
        .contains("index out of range"));
        assert!(msg(Mesh {
            positions: vec![0.0; 9],
            colors: vec![1; 3],
            ..Mesh::default()
        })
        .contains("colours"));
        assert!(msg(Mesh {
            positions: vec![0.0; 9],
            fields: vec![Field {
                name: "T".into(),
                unit: String::new(),
                values: vec![1.0; 2],
            }],
            ..Mesh::default()
        })
        .contains("result fields"));
        assert!(msg(Mesh {
            positions: vec![0.0, f32::INFINITY, 0.0],
            ..Mesh::default()
        })
        .contains("invalid coordinates"));
    }

    #[test]
    fn bounds_of_a_mesh() {
        assert_eq!(cube().bounds(), [0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
        let one_point = Mesh {
            positions: vec![2.0, -3.0, 4.0],
            ..Mesh::default()
        };
        assert_eq!(one_point.bounds(), [2.0, -3.0, 4.0, 2.0, -3.0, 4.0]);
    }
}
