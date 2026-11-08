//! Triangle meshes and point clouds for the 3D preview.
//!
//! Pure-Rust readers for the common mesh, 3D-printing, visualisation and
//! point-cloud formats, plus STEP B-rep tessellation with the `truck` CAD
//! kernel (Apache-2.0). Every reader produces one [`Mesh`] in model
//! coordinates; [`Mesh::encode`] writes the compact binary form that
//! `shared/js/viewer.js` draws (`TCMESH01`, little endian).
//!
//! Simulation results (VTK, VTU/VTP, CalculiX FRD, see [`crate::simmesh`])
//! additionally carry per-vertex scalar [`Field`]s that the viewer colours
//! with a colour map; the section is optional and flagged, so older viewers
//! and cached meshes keep working.
//!
//! The readers are defensive: sizes are limited, indices are checked and a
//! malformed file is an error, never a panic or an endless loop. The server
//! still runs them in a separate process with a time limit (see `preview`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, bail, Context, Result};

mod jt;
mod p21;
mod pn;
pub mod render;
pub mod rhino;
mod step;
mod sw;
mod tessellated;

pub use step::MAX_STEP_BYTES;

/// Largest triangle count that is previewed.
pub const MAX_TRIANGLES: usize = 8_000_000;
/// Point clouds with more points are thinned out evenly.
pub const MAX_POINTS: usize = 3_000_000;

const MAGIC: &[u8; 8] = b"TCMESH01";
/// Colour of parts without own colour when they are merged with coloured ones.
pub(crate) const NEUTRAL: [u8; 3] = [176, 188, 200];

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

    fn vertex(&mut self, p: [f64; 3]) -> u32 {
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

/// Mesh formats the readers understand (ids as in `cadformat`).
pub fn read(format: &str, data: &[u8]) -> Result<Mesh> {
    let mesh = match format {
        "stl" => stl(data)?,
        "obj" => obj(data)?,
        "ply" => ply(data)?,
        "off" => off(data)?,
        "3mf" => three_mf(data)?,
        "amf" => amf(data)?,
        "gltf" | "glb" => gltf(data)?,
        "3ds" => three_ds(data)?,
        "pts" | "xyz" => text_points(data)?,
        "las" => las(data)?,
        "step" => step::read(data)?,
        "nx-part" => jt::read_nx(data)?,
        "jt" => jt::read_file(data)?,
        "solidworks-part" => sw::read_part(data)?,
        "solidworks-assembly" => bail!(
            "A SOLIDWORKS assembly is shown with the previews of its parts (found by file name)."
        ),
        "rhino" => rhino::read(data)?,
        "vtk-legacy" => crate::simmesh::vtk_legacy(data)?,
        "vtk-unstructured" | "vtk-polydata" => crate::simmesh::vtk_xml(data)?,
        "calculix-frd" => crate::simmesh::frd(data)?,
        other => bail!("No 3D preview for the format '{other}'."),
    };
    mesh.check()?;
    Ok(mesh)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The error of a read past the end: the file is shorter than it claims.
fn ends_early(at: usize) -> anyhow::Error {
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

fn i32_le(data: &[u8], at: usize) -> Result<i32> {
    bytes_at(data, at).map(i32::from_le_bytes)
}

pub(crate) fn f32_le(data: &[u8], at: usize) -> Result<f32> {
    bytes_at(data, at).map(f32::from_le_bytes)
}

fn f64_le(data: &[u8], at: usize) -> Result<f64> {
    bytes_at(data, at).map(f64::from_le_bytes)
}

fn u64_le(data: &[u8], at: usize) -> Result<u64> {
    bytes_at(data, at).map(u64::from_le_bytes)
}

/// The bytes in groups of four (an incomplete group at the end is dropped).
pub(crate) fn quads(data: &[u8]) -> impl Iterator<Item = [u8; 4]> + '_ {
    data.chunks_exact(4)
        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
}

fn text(data: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(data)
}

fn float(token: &str) -> Result<f64> {
    let v: f64 = token
        .parse()
        .map_err(|_| anyhow!("'{}' is not a number", truncate(token)))?;
    if !v.is_finite() {
        bail!("invalid coordinate '{}'", truncate(token));
    }
    Ok(v)
}

fn truncate(s: &str) -> String {
    s.chars().take(24).collect()
}

/// Polygon → triangle fan.
fn fan(mesh: &mut Mesh, polygon: &[u32]) {
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
fn stride_for(count: usize) -> usize {
    count.div_ceil(MAX_POINTS).max(1)
}

/// Welds identical vertices of a triangle soup (STL).
struct Welder {
    map: HashMap<[u32; 3], u32>,
    mesh: Mesh,
}

impl Welder {
    fn new(triangles: usize) -> Welder {
        Welder {
            map: HashMap::with_capacity(triangles / 2),
            mesh: Mesh {
                parts: 1,
                indices: Vec::with_capacity(triangles * 3),
                ..Mesh::default()
            },
        }
    }

    fn push(&mut self, p: [f32; 3]) {
        let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
        let next = self.mesh.vertex_count() as u32;
        let index = *self.map.entry(key).or_insert_with(|| {
            self.mesh.positions.extend(p);
            next
        });
        self.mesh.indices.push(index);
    }
}

// ---------------------------------------------------------------------------
// STL
// ---------------------------------------------------------------------------

fn stl(data: &[u8]) -> Result<Mesh> {
    if let Ok(n) = u32_le(data, 80).map(|n| n as usize) {
        if n.checked_mul(50).and_then(|b| b.checked_add(84)) == Some(data.len()) {
            if n == 0 {
                bail!("The STL file contains no triangles.");
            }
            check_triangles(n)?;
            let mut w = Welder::new(n);
            for t in 0..n {
                let o = 84 + t * 50 + 12;
                for v in 0..3 {
                    let at = o + v * 12;
                    let f = |k: usize| f32_le(data, at + k * 4);
                    w.push([f(0)?, f(1)?, f(2)?]);
                }
            }
            return Ok(w.mesh);
        }
    }
    let text = text(data);
    if !text.trim_start().starts_with("solid") {
        bail!("Not an STL file (neither binary nor ASCII).");
    }
    let mut w = Welder::new(data.len() / 200);
    let mut tokens = text.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        if token == "vertex" {
            let mut p = [0f32; 3];
            for c in &mut p {
                *c = float(tokens.next().context("incomplete vertex")?)? as f32;
            }
            w.push(p);
            check_triangles(w.mesh.indices.len() / 3)?;
        }
    }
    let len = w.mesh.indices.len();
    w.mesh.indices.truncate(len - len % 3);
    if w.mesh.indices.is_empty() {
        bail!("The STL file contains no triangles.");
    }
    Ok(w.mesh)
}

// ---------------------------------------------------------------------------
// OBJ
// ---------------------------------------------------------------------------

fn obj(data: &[u8]) -> Result<Mesh> {
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

// ---------------------------------------------------------------------------
// PLY
// ---------------------------------------------------------------------------

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

fn ply(data: &[u8]) -> Result<Mesh> {
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

// ---------------------------------------------------------------------------
// OFF
// ---------------------------------------------------------------------------

fn off(data: &[u8]) -> Result<Mesh> {
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

// ---------------------------------------------------------------------------
// 3MF and AMF (XML, zipped)
// ---------------------------------------------------------------------------

type Affine = [f64; 12];
const IDENTITY: Affine = [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.];

/// 3MF transforms are row vectors: `p' = p · M` with the translation last.
fn parse_transform(s: Option<&str>) -> Result<Affine> {
    let Some(s) = s else { return Ok(IDENTITY) };
    let values: Vec<f64> = s
        .split_ascii_whitespace()
        .map(float)
        .collect::<Result<_>>()?;
    values
        .try_into()
        .map_err(|_| anyhow!("3MF transform with the wrong number of values"))
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    [
        p[0] * m[0] + p[1] * m[3] + p[2] * m[6] + m[9],
        p[0] * m[1] + p[1] * m[4] + p[2] * m[7] + m[10],
        p[0] * m[2] + p[1] * m[5] + p[2] * m[8] + m[11],
    ]
}

/// `a` first, then `b`.
// Fixed-size arrays: r < 4, c < 3 and k < 3 keep every index below 12.
#[allow(clippy::indexing_slicing)]
fn compose(a: &Affine, b: &Affine) -> Affine {
    let mut m = [0.0; 12];
    for r in 0..4 {
        for c in 0..3 {
            let mut v = if r == 3 { b[9 + c] } else { 0.0 };
            for k in 0..3 {
                v += a[r * 3 + k] * b[k * 3 + c];
            }
            m[r * 3 + c] = v;
        }
    }
    m
}

fn zip_entry(data: &[u8], wanted: impl Fn(&str) -> bool) -> Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).context("not a ZIP package")?;
    let names: Vec<String> = archive
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let Some(name) = names.into_iter().find(|n| wanted(n.as_str())) else {
        return Ok(None);
    };
    let mut entry = archive.by_name(&name)?;
    if entry.size() > 2 * 1024 * 1024 * 1024 {
        bail!("{name} is too large to preview");
    }
    let mut out = Vec::with_capacity(entry.size() as usize);
    entry
        .by_ref()
        .take(2 * 1024 * 1024 * 1024)
        .read_to_end(&mut out)?;
    Ok(Some(out))
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

#[derive(Default)]
struct ThreeMfObject {
    mesh: Mesh,
    components: Vec<(String, Affine)>,
}

fn three_mf(data: &[u8]) -> Result<Mesh> {
    // The start part is named in _rels/.rels; "3D/3dmodel.model" by convention.
    let rels = zip_entry(data, |n| n.eq_ignore_ascii_case("_rels/.rels"))?;
    let target = rels
        .as_deref()
        .and_then(|r| {
            let r = String::from_utf8_lossy(r).into_owned();
            let at = r.find("3dmodel")?;
            let start = r.get(..at)?.rfind("Target=\"")? + 8;
            let end = start + r.get(start..)?.find('"')?;
            Some(r.get(start..end)?.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| "3D/3dmodel.model".to_string());
    let xml = zip_entry(data, |n| n.eq_ignore_ascii_case(&target))?
        .or(zip_entry(data, |n| {
            n.to_ascii_lowercase().ends_with(".model")
        })?)
        .context("the 3MF package contains no 3D model")?;
    three_mf_model(&xml)
}

fn three_mf_model(xml: &[u8]) -> Result<Mesh> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut objects: HashMap<String, ThreeMfObject> = HashMap::new();
    let mut items: Vec<(String, Affine)> = Vec::new();
    let mut current: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .context("invalid 3MF XML")?;
        match &event {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "object" => {
                    let id = attr(e, "id").unwrap_or_default();
                    objects.entry(id.clone()).or_default();
                    current = Some(id);
                }
                "vertex" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let c = |n: &str| float(attr(e, n).as_deref().unwrap_or("0"));
                        obj.mesh.vertex([c("x")?, c("y")?, c("z")?]);
                    }
                }
                "triangle" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let n = obj.mesh.vertex_count() as u32;
                        let mut t = [0u32; 3];
                        for (slot, name) in t.iter_mut().zip(["v1", "v2", "v3"]) {
                            *slot = attr(e, name)
                                .and_then(|v| v.parse().ok())
                                .filter(|v| *v < n)
                                .context("3MF triangle refers to a missing vertex")?;
                        }
                        obj.mesh.indices.extend(t);
                        check_triangles(obj.mesh.triangle_count())?;
                    }
                }
                "component" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let target = attr(e, "objectid").unwrap_or_default();
                        obj.components
                            .push((target, parse_transform(attr(e, "transform").as_deref())?));
                    }
                }
                "item" => {
                    let target = attr(e, "objectid").unwrap_or_default();
                    items.push((target, parse_transform(attr(e, "transform").as_deref())?));
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "object" => current = None,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if items.is_empty() {
        // no build section: show every object that has geometry
        let mut ids: Vec<&String> = objects.keys().collect();
        ids.sort();
        items = ids.into_iter().map(|id| (id.clone(), IDENTITY)).collect();
    }
    let mut out = Mesh::default();
    for (id, transform) in &items {
        instantiate(&objects, id, transform, 0, &mut out)?;
    }
    Ok(out)
}

fn instantiate(
    objects: &HashMap<String, ThreeMfObject>,
    id: &str,
    transform: &Affine,
    depth: usize,
    out: &mut Mesh,
) -> Result<()> {
    if depth > 16 {
        bail!("3MF components nest too deeply (cycle?)");
    }
    let Some(obj) = objects.get(id) else {
        return Ok(()); // object in another model part (production extension)
    };
    if !obj.mesh.positions.is_empty() {
        let mut part = obj.mesh.clone();
        for p in part.positions.chunks_exact_mut(3) {
            if let [x, y, z] = p {
                let [qx, qy, qz] = apply(transform, [*x as f64, *y as f64, *z as f64]);
                (*x, *y, *z) = (qx as f32, qy as f32, qz as f32);
            }
        }
        part.parts = 1;
        out.append(part);
        check_triangles(out.triangle_count())?;
    }
    for (child, local) in &obj.components {
        instantiate(objects, child, &compose(local, transform), depth + 1, out)?;
    }
    Ok(())
}

fn amf(data: &[u8]) -> Result<Mesh> {
    use quick_xml::events::Event;
    let unzipped;
    let xml = if data.starts_with(b"PK") {
        unzipped = zip_entry(data, |_| true)?.context("empty AMF archive")?;
        &unzipped[..]
    } else {
        data
    };
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = Mesh::default();
    let mut object = Mesh::default();
    let mut coord = [0f64; 3];
    let mut tri = [0u32; 3];
    let mut field: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .context("invalid AMF XML")?
        {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "object" {
                    object = Mesh::default();
                }
                field = Some(name);
            }
            Event::Text(t) => {
                let value = t.xml_content(quick_xml::XmlVersion::Implicit1_0);
                let value = value.trim();
                match field.as_deref() {
                    Some("x") => coord[0] = float(value)?,
                    Some("y") => coord[1] = float(value)?,
                    Some("z") => coord[2] = float(value)?,
                    Some("v1") => tri[0] = value.parse().context("AMF triangle")?,
                    Some("v2") => tri[1] = value.parse().context("AMF triangle")?,
                    Some("v3") => tri[2] = value.parse().context("AMF triangle")?,
                    _ => {}
                }
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "coordinates" => {
                        object.vertex(coord);
                    }
                    "triangle" => {
                        let n = object.vertex_count() as u32;
                        if tri.iter().any(|i| *i >= n) {
                            bail!("AMF triangle refers to a missing vertex");
                        }
                        object.indices.extend(tri);
                        check_triangles(out.triangle_count() + object.triangle_count())?;
                    }
                    "object" => {
                        object.parts = 1;
                        out.append(std::mem::take(&mut object));
                    }
                    _ => {}
                }
                field = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// glTF / GLB
// ---------------------------------------------------------------------------

fn gltf(data: &[u8]) -> Result<Mesh> {
    use base64::Engine;
    // gltf-json indexes the accessors named by a primitive without a range
    // check while validating (a POSITION index past the accessor list
    // panics), so a hostile file must not be able to unwind through here.
    let document = std::panic::catch_unwind(|| gltf::Gltf::from_slice(data))
        .map_err(|_| anyhow!("invalid glTF (inconsistent accessor references)"))?
        .context("invalid glTF")?;
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for buffer in document.buffers() {
        let bytes = match buffer.source() {
            gltf::buffer::Source::Bin => {
                document.blob.clone().context("GLB without binary chunk")?
            }
            gltf::buffer::Source::Uri(uri) => {
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
    let roots: Vec<gltf::Node> = match scene {
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
    node: &gltf::Node,
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
                gltf::mesh::Mode::Triangles => false,
                gltf::mesh::Mode::Points => true,
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

// ---------------------------------------------------------------------------
// 3DS (legacy 3ds Max)
// ---------------------------------------------------------------------------

fn three_ds(data: &[u8]) -> Result<Mesh> {
    if data.len() < 6 || u16_le(data, 0).ok() != Some(0x4D4D) {
        bail!("Not a 3DS file.");
    }
    let mut out = Mesh::default();
    three_ds_chunks(data, 6, data.len(), &mut out, &mut None, 0)?;
    Ok(out)
}

fn three_ds_chunks(
    data: &[u8],
    mut at: usize,
    end: usize,
    out: &mut Mesh,
    object: &mut Option<Mesh>,
    depth: usize,
) -> Result<()> {
    if depth > 8 {
        bail!("3DS chunks nest too deeply");
    }
    while at + 6 <= end {
        let (Ok(id), Ok(len)) = (u16_le(data, at), u32_le(data, at + 2)) else {
            break; // truncated
        };
        let len = len as usize;
        if len < 6 || at + len > end {
            break; // truncated or padding
        }
        let body = at + 6;
        let chunk_end = at + len;
        // the u16 count at the start of a vertex or face list; a list chunk
        // too short for it is skipped like a truncated chunk
        let count = (body + 2 <= chunk_end)
            .then(|| u16_le(data, body).ok())
            .flatten()
            .map(usize::from);
        match id {
            // main → editor → object → triangle mesh: descend
            0x3D3D => three_ds_chunks(data, body, chunk_end, out, object, depth + 1)?,
            0x4000 => {
                // object block: zero-terminated name, then sub-chunks
                let name_end = range(data, body, chunk_end)?
                    .iter()
                    .position(|b| *b == 0)
                    .map_or(chunk_end, |p| body + p + 1);
                let mut mesh = Some(Mesh::default());
                three_ds_chunks(data, name_end, chunk_end, out, &mut mesh, depth + 1)?;
                if let Some(mut m) = mesh.filter(|m| !m.positions.is_empty()) {
                    m.parts = 1;
                    out.append(m);
                    check_triangles(out.triangle_count())?;
                }
            }
            0x4100 => three_ds_chunks(data, body, chunk_end, out, object, depth + 1)?,
            0x4110 => {
                let Some(mesh) = object.as_mut() else {
                    at = chunk_end;
                    continue;
                };
                let Some(n) = count else {
                    at = chunk_end;
                    continue;
                };
                if body + 2 + n * 12 > chunk_end {
                    bail!("3DS vertex list is truncated");
                }
                for v in 0..n {
                    let o = body + 2 + v * 12;
                    let f = |k: usize| f32_le(data, o + k * 4).map(f64::from);
                    mesh.vertex([f(0)?, f(1)?, f(2)?]);
                }
            }
            0x4120 => {
                let Some(mesh) = object.as_mut() else {
                    at = chunk_end;
                    continue;
                };
                let Some(n) = count else {
                    at = chunk_end;
                    continue;
                };
                if body + 2 + n * 8 > chunk_end {
                    bail!("3DS face list is truncated");
                }
                let vertices = mesh.vertex_count() as u32;
                for f in 0..n {
                    let o = body + 2 + f * 8;
                    let i = |k: usize| u16_le(data, o + k * 2).map(u32::from);
                    let t = [i(0)?, i(1)?, i(2)?];
                    if t.iter().any(|v| *v >= vertices) {
                        bail!("3DS face refers to a missing vertex");
                    }
                    mesh.indices.extend(t);
                }
            }
            _ => {}
        }
        at = chunk_end;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Point clouds: PTS / XYZ (text), LAS (binary)
// ---------------------------------------------------------------------------

/// `x y z [intensity] [r g b]` per line (spaces, commas or semicolons); lines
/// that are not numbers (headers, the point count of .pts files) are skipped.
fn text_points(data: &[u8]) -> Result<Mesh> {
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

fn las(data: &[u8]) -> Result<Mesh> {
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

    fn cube_stl_ascii() -> String {
        let mut s = String::from("solid cube\n");
        let quads = [
            [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]],
            [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
        ];
        for q in quads {
            for t in [[0, 1, 2], [0, 2, 3]] {
                s.push_str("facet normal 0 0 1\n outer loop\n");
                for k in t {
                    let p = q[k];
                    s.push_str(&format!("  vertex {} {} {}\n", p[0], p[1], p[2]));
                }
                s.push_str(" endloop\nendfacet\n");
            }
        }
        s.push_str("endsolid cube\n");
        s
    }

    #[test]
    fn stl_ascii_and_binary_are_welded() {
        let mesh = read("stl", cube_stl_ascii().as_bytes()).unwrap();
        assert_eq!(mesh.triangle_count(), 4);
        assert_eq!(mesh.vertex_count(), 8, "shared corners are welded");

        let mut bin = vec![0u8; 80];
        bin.extend_from_slice(&2u32.to_le_bytes());
        for tri in [
            [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            [[1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
        ] {
            bin.extend_from_slice(&[0u8; 12]);
            for p in tri {
                for c in p {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            bin.extend_from_slice(&[0, 0]);
        }
        let mesh = read("stl", &bin).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (2, 4));
        assert!(read("stl", b"garbage").is_err());
        let mut empty = vec![0u8; 80];
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert!(read("stl", &empty).is_err());
    }

    #[test]
    fn obj_faces_points_and_negative_indices() {
        let mesh = read(
            "obj",
            b"o plate\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3/3/3 4/4/4\nf -4 -3 -2\n",
        )
        .unwrap();
        assert_eq!(mesh.triangle_count(), 3);
        assert!(
            read("obj", b"v 0 0 0\nf 1 2 3\n").is_err(),
            "missing vertices"
        );
        let cloud = read("obj", b"v 0 0 0 1 0 0\nv 1 2 3 0 1 0\n").unwrap();
        assert!(cloud.is_point_cloud());
        assert_eq!(cloud.colors, vec![255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn ply_ascii_and_binary() {
        let ascii = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\n\
property uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n\
0 0 0 255 0 0\n1 0 0 0 255 0\n0 1 0 0 0 255\n3 0 1 2\n";
        let mesh = read("ply", ascii).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.colors.len(), 9);

        let mut bin = b"ply\nformat binary_little_endian 1.0\nelement vertex 4\nproperty double x\nproperty double y\nproperty double z\nend_header\n".to_vec();
        for i in 0..4 {
            for c in [i as f64, 0.5, -1.0] {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let cloud = read("ply", &bin).unwrap();
        assert!(cloud.is_point_cloud());
        assert_eq!(cloud.vertex_count(), 4);
        assert_eq!(cloud.positions[3], 1.0);
        bin.truncate(bin.len() - 8);
        assert!(read("ply", &bin).is_err(), "truncated body");
    }

    #[test]
    fn off_and_three_ds() {
        let mesh = read(
            "off",
            b"OFF\n# square\n4 1 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n",
        )
        .unwrap();
        assert_eq!(mesh.triangle_count(), 2);

        // 3DS: main → editor → object "a" → mesh with 3 vertices and 1 face
        let mut vertices = 3u16.to_le_bytes().to_vec();
        for p in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for c in p {
                vertices.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mut faces = 1u16.to_le_bytes().to_vec();
        for i in [0u16, 1, 2, 0] {
            faces.extend_from_slice(&i.to_le_bytes());
        }
        let chunk = |id: u16, body: &[u8]| {
            let mut c = id.to_le_bytes().to_vec();
            c.extend_from_slice(&((body.len() + 6) as u32).to_le_bytes());
            c.extend_from_slice(body);
            c
        };
        let trimesh = chunk(
            0x4100,
            &[chunk(0x4110, &vertices), chunk(0x4120, &faces)].concat(),
        );
        let object = chunk(0x4000, &[b"a\0".to_vec(), trimesh].concat());
        let file = chunk(0x4D4D, &chunk(0x3D3D, &object));
        let mesh = read("3ds", &file).unwrap();
        assert_eq!(
            (mesh.triangle_count(), mesh.vertex_count(), mesh.parts),
            (1, 3, 1)
        );
    }

    /// Regression: a vertex or face list chunk of the minimum length 6 has
    /// no room for its count. At the end of the file the count was read
    /// past the last byte – a panic. Now the chunk is skipped.
    #[test]
    fn three_ds_list_chunks_without_their_count() {
        let chunk = |id: u16, body: &[u8]| {
            let mut c = id.to_le_bytes().to_vec();
            c.extend_from_slice(&((body.len() + 6) as u32).to_le_bytes());
            c.extend_from_slice(body);
            c
        };
        let file_with = |lists: Vec<u8>| {
            let object = chunk(0x4000, &[b"a\0".to_vec(), chunk(0x4100, &lists)].concat());
            chunk(0x4D4D, &chunk(0x3D3D, &object))
        };
        for id in [0x4110u16, 0x4120] {
            // the empty list is the last chunk of the file
            let file = file_with(chunk(id, &[]));
            assert!(read("3ds", &file).is_err(), "{id:#x}: no geometry");
            assert!(three_ds(&file).unwrap().positions.is_empty());
        }
        // an empty list before a complete one: skipped, the rest is read
        let mut vertices = 3u16.to_le_bytes().to_vec();
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            vertices.extend_from_slice(&c.to_le_bytes());
        }
        let mut faces = 1u16.to_le_bytes().to_vec();
        for i in [0u16, 1, 2, 0] {
            faces.extend_from_slice(&i.to_le_bytes());
        }
        let file = file_with(
            [
                chunk(0x4110, &[]),
                chunk(0x4110, &vertices),
                chunk(0x4120, &[]),
                chunk(0x4120, &faces),
            ]
            .concat(),
        );
        let mesh = read("3ds", &file).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (1, 3));
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn three_mf_with_components_and_build_transforms() {
        let model = r#"<?xml version="1.0"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
  </mesh></object>
  <object id="2" type="model"><components>
   <component objectid="1"/><component objectid="1" transform="1 0 0 0 1 0 0 0 1 10 0 0"/>
  </components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 5"/></build>
</model>"#;
        let package = zip_of(&[
            (
                "_rels/.rels",
                r#"<Relationships><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#,
            ),
            ("3D/3dmodel.model", model),
        ]);
        let mesh = read("3mf", &package).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.parts), (2, 2));
        let b = mesh.bounds();
        assert_eq!((b[0], b[3], b[2], b[5]), (0.0, 11.0, 5.0, 5.0));
        assert!(read("3mf", b"PK not really").is_err());
    }

    #[test]
    fn amf_plain_and_zipped() {
        let amf = "<amf><object id=\"0\"><mesh><vertices>\
<vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>\
<vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>\
<vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex>\
</vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>";
        assert_eq!(read("amf", amf.as_bytes()).unwrap().triangle_count(), 1);
        let zipped = zip_of(&[("model.amf", amf)]);
        assert_eq!(read("amf", &zipped).unwrap().triangle_count(), 1);
        assert!(read("amf", b"<amf><object><mesh><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>").is_err());
    }

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
        let mesh = read("gltf", json.as_bytes()).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.positions[2], 2.0, "node translation applied");
        let external = json.replace(&uri, "model.bin");
        let err = read("gltf", external.as_bytes()).unwrap_err().to_string();
        assert!(err.contains("external buffers"), "{err}");
    }

    #[test]
    fn point_clouds() {
        let pts = read(
            "pts",
            b"3\n0 0 0 10 255 0 0\n1 1 1 10 0 255 0\n2,2,2,10,0,0,255\n",
        )
        .unwrap();
        assert!(pts.is_point_cloud());
        assert_eq!(pts.vertex_count(), 3);
        assert_eq!(&pts.colors[..3], &[255, 0, 0]);
        let xyz = read("xyz", b"# x y z\n1.5 2.5 3.5\n4 5 6\n").unwrap();
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
        let cloud = read("las", &las).unwrap();
        assert_eq!(cloud.vertex_count(), 2);
        assert!((cloud.positions[3] - 2.0).abs() < 1e-6);
        assert_eq!(cloud.colors[0], 255);
        las[104] = 0x82;
        assert!(read("las", &las).unwrap_err().to_string().contains("LAZ"));
    }

    #[test]
    fn step_box_is_tessellated() {
        let mesh = read("step", include_bytes!("../tests/fixtures/cube.step")).unwrap();
        assert!(
            mesh.triangle_count() >= 12,
            "{} triangles",
            mesh.triangle_count()
        );
        let b = mesh.bounds();
        assert!((b[3] - b[0] - 10.0).abs() < 1e-3, "{b:?}");
        assert!(read(
            "step",
            b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n"
        )
        .is_err());
        assert!(read("step", b"not a step file").is_err());
    }

    #[test]
    fn encode_round_trip_and_limits() {
        let mut mesh = read("stl", cube_stl_ascii().as_bytes()).unwrap();
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
        assert!(read("dwg", b"").is_err());
    }

    #[test]
    fn partial_previews_carry_their_face_count() {
        let mut mesh = read("stl", cube_stl_ascii().as_bytes()).unwrap();
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
        let mut mesh = read("stl", cube_stl_ascii().as_bytes()).unwrap();
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
}
