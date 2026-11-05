//! Simulation results (FEA/CFD) for the 3D preview – pure Rust, no ParaView.
//!
//! Readers for the open result formats most solvers can write:
//!
//! * legacy VTK (`.vtk`): ASCII and BINARY (big endian), `DATASET
//!   UNSTRUCTURED_GRID` and `POLYDATA`, the classic `CELLS` layout and the
//!   `OFFSETS`/`CONNECTIVITY` layout of file version 5;
//! * VTK XML `UnstructuredGrid` (`.vtu`) and `PolyData` (`.vtp`): `ascii`,
//!   inline `binary` (base64) and appended data (`raw` or `base64`), optional
//!   zlib compression, `UInt32`/`UInt64` headers, every integer and float type;
//! * CalculiX results (`.frd`, ASCII): nodes (`2C`), elements (`3C`) and the
//!   nodal result blocks (`-4 DISP`, `-4 STRESS`, …).
//!
//! Every reader fills one [`Grid`] (points, cells, point and cell data);
//! [`Grid::into_mesh`] turns it into the preview [`Mesh`]: volume cells
//! (tetrahedra, hexahedra, wedges, pyramids – quadratic ones by their corner
//! nodes) contribute only their outer faces (faces used by exactly one cell),
//! surface cells are drawn as they are, and interior points are dropped.
//!
//! Result values become scalar [`Field`]s on the vertices: one-component
//! data as is, six- or nine-component tensors as von Mises equivalent,
//! other vectors as magnitude. Cell data is averaged to the points. At most
//! [`MAX_FIELDS`] fields are kept – the viewer offers them in a selector.
//!
//! Like the mesh readers, everything is defensive: counts are limited and
//! checked against the data before anything is allocated, decompression is
//! bounded by the sizes the arrays need, indices are checked – a hostile file
//! is an error, never a panic, an endless loop or an unbounded allocation.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;

use crate::mesh::{check_triangles, thin_out, Field, Mesh, MAX_FIELDS};

/// Points of one result file.
pub const MAX_NODES: usize = 20_000_000;
/// Cells (elements) of one result file.
pub const MAX_CELLS: usize = 20_000_000;
/// Node references of all cells together.
pub const MAX_CONNECTIVITY: usize = 160_000_000;
/// Surfaces with more triangles are thinned out evenly (gaps are visible,
/// the viewer stays fast).
pub const MAX_RESULT_TRIANGLES: usize = 2_000_000;
/// Field values carried at most (fields × vertices).
const FIELD_BUDGET: usize = 24_000_000;
/// Components of one data array.
const MAX_COMPONENTS: usize = 64;
/// Decoded bytes of one XML data array.
const MAX_ARRAY_BYTES: usize = 1 << 31;

// ---------------------------------------------------------------------------
// numbers
// ---------------------------------------------------------------------------

/// Binary number types of VTK files.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Num {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Num {
    fn size(self) -> usize {
        match self {
            Num::I8 | Num::U8 => 1,
            Num::I16 | Num::U16 => 2,
            Num::I32 | Num::U32 | Num::F32 => 4,
            Num::I64 | Num::U64 | Num::F64 => 8,
        }
    }

    /// Type names of legacy files (`float`, `vtktypeint64`, …).
    fn legacy(name: Option<&str>) -> Result<Num> {
        let name = name.context("the data type is missing")?;
        Ok(match name.to_ascii_lowercase().as_str() {
            "char" | "vtktypeint8" | "signed_char" => Num::I8,
            "unsigned_char" | "vtktypeuint8" => Num::U8,
            "short" | "vtktypeint16" => Num::I16,
            "unsigned_short" | "vtktypeuint16" => Num::U16,
            "int" | "vtktypeint32" => Num::I32,
            "unsigned_int" | "vtktypeuint32" => Num::U32,
            "long" | "vtktypeint64" | "vtkidtype" => Num::I64,
            "unsigned_long" | "vtktypeuint64" => Num::U64,
            "float" | "vtktypefloat32" => Num::F32,
            "double" | "vtktypefloat64" => Num::F64,
            other => bail!("unsupported VTK data type '{}'", short(other)),
        })
    }

    /// Type names of XML files (`Float32`, `Int64`, …).
    fn xml(name: Option<&str>) -> Result<Num> {
        let name = name.context("a DataArray has no type")?;
        Ok(match name {
            "Int8" | "Char" => Num::I8,
            "UInt8" => Num::U8,
            "Int16" => Num::I16,
            "UInt16" => Num::U16,
            "Int32" => Num::I32,
            "UInt32" => Num::U32,
            "Int64" => Num::I64,
            "UInt64" => Num::U64,
            "Float32" => Num::F32,
            "Float64" => Num::F64,
            other => bail!("unsupported VTK data type '{}'", short(other)),
        })
    }

    /// The value of `b` (exactly [`Num::size`] bytes).
    fn get(self, b: &[u8], big_endian: bool) -> f64 {
        macro_rules! conv {
            ($t:ty, $n:expr) => {{
                let mut a = [0u8; $n];
                a.copy_from_slice(&b[..$n]);
                (if big_endian {
                    <$t>::from_be_bytes(a)
                } else {
                    <$t>::from_le_bytes(a)
                }) as f64
            }};
        }
        match self {
            Num::I8 => b[0] as i8 as f64,
            Num::U8 => b[0] as f64,
            Num::I16 => conv!(i16, 2),
            Num::U16 => conv!(u16, 2),
            Num::I32 => conv!(i32, 4),
            Num::U32 => conv!(u32, 4),
            Num::I64 => conv!(i64, 8),
            Num::U64 => conv!(u64, 8),
            Num::F32 => conv!(f32, 4),
            Num::F64 => conv!(f64, 8),
        }
    }
}

fn short(s: &str) -> String {
    s.chars().take(32).collect()
}

/// `count` binary numbers from the start of `bytes`.
fn each_binary(
    bytes: &[u8],
    ty: Num,
    big_endian: bool,
    count: usize,
    f: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<()> {
    let need = count
        .checked_mul(ty.size())
        .context("data array too large")?;
    let data = bytes
        .get(..need)
        .context("the result file is truncated (data array shorter than declared)")?;
    for c in data.chunks_exact(ty.size()) {
        f(ty.get(c, big_endian))?;
    }
    Ok(())
}

/// `count` whitespace-separated numbers of `text`.
fn each_ascii(text: &[u8], count: usize, f: &mut dyn FnMut(f64) -> Result<()>) -> Result<()> {
    let mut tokens = text
        .split(|b| b.is_ascii_whitespace())
        .filter(|t| !t.is_empty());
    for _ in 0..count {
        let token = tokens
            .next()
            .context("the result file is truncated (fewer values than declared)")?;
        f(number(token)?)?;
    }
    Ok(())
}

fn number(token: &[u8]) -> Result<f64> {
    std::str::from_utf8(token)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| {
            anyhow!(
                "'{}' is not a number",
                short(&String::from_utf8_lossy(token))
            )
        })
}

/// A point index (non-negative integer that fits `u32`).
fn index(v: f64) -> Result<u32> {
    if v >= 0.0 && v <= u32::MAX as f64 && v.fract() == 0.0 {
        Ok(v as u32)
    } else {
        bail!("invalid point index or count in a cell")
    }
}

fn count(v: Option<&str>, what: &str) -> Result<usize> {
    v.and_then(|s| s.parse::<usize>().ok())
        .with_context(|| format!("invalid {what}"))
}

/// Reduces the components of a tuple to one scalar.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Reduce {
    Value,
    Magnitude,
    VonMises6,
    VonMises9,
}

impl Reduce {
    fn of(components: usize) -> Reduce {
        match components {
            0 | 1 => Reduce::Value,
            6 => Reduce::VonMises6,
            9 => Reduce::VonMises9,
            _ => Reduce::Magnitude,
        }
    }

    fn label(self, name: &str) -> String {
        match self {
            Reduce::Value => name.to_string(),
            Reduce::Magnitude => format!("{name} (magnitude)"),
            Reduce::VonMises6 | Reduce::VonMises9 => format!("{name} (von Mises)"),
        }
    }

    fn apply(self, c: &[f64]) -> f32 {
        let vm = |xx: f64, yy: f64, zz: f64, xy: f64, yz: f64, xz: f64| {
            (0.5 * ((xx - yy).powi(2) + (yy - zz).powi(2) + (zz - xx).powi(2))
                + 3.0 * (xy * xy + yz * yz + xz * xz))
                .sqrt()
        };
        (match self {
            Reduce::Value => c.first().copied().unwrap_or(f64::NAN),
            Reduce::Magnitude => c.iter().map(|v| v * v).sum::<f64>().sqrt(),
            Reduce::VonMises6 if c.len() >= 6 => vm(c[0], c[1], c[2], c[3], c[4], c[5]),
            Reduce::VonMises9 if c.len() >= 9 => vm(
                c[0],
                c[4],
                c[8],
                0.5 * (c[1] + c[3]),
                0.5 * (c[5] + c[7]),
                0.5 * (c[2] + c[6]),
            ),
            _ => f64::NAN,
        }) as f32
    }
}

/// Collects `tuples × components` numbers into one scalar per tuple.
struct Tuples {
    components: usize,
    reduce: Reduce,
    buf: Vec<f64>,
    out: Vec<f32>,
}

impl Tuples {
    fn new(tuples: usize, components: usize, budget_bytes: usize) -> Tuples {
        Tuples {
            components: components.max(1),
            reduce: Reduce::of(components),
            buf: Vec::with_capacity(components.max(1)),
            out: Vec::with_capacity(tuples.min(budget_bytes / 2)),
        }
    }

    fn push(&mut self, v: f64) -> Result<()> {
        self.buf.push(v);
        if self.buf.len() == self.components {
            self.out.push(self.reduce.apply(&self.buf));
            self.buf.clear();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// grid → surface mesh
// ---------------------------------------------------------------------------

/// Unstructured points and cells as read from a result file. Cells use VTK
/// cell type ids.
#[derive(Default)]
struct Grid {
    /// x, y, z per point.
    points: Vec<f32>,
    /// Node indices of all cells, one after another.
    conn: Vec<u32>,
    /// End of every cell in `conn`.
    ends: Vec<usize>,
    /// VTK cell type of every cell.
    types: Vec<u8>,
    point_fields: Vec<Field>,
    cell_fields: Vec<Field>,
}

// VTK cell types
const POLY_VERTEX: u8 = 2;
const LINE: u8 = 3;
const POLY_LINE: u8 = 4;
const TRIANGLE: u8 = 5;
const TRIANGLE_STRIP: u8 = 6;
const POLYGON: u8 = 7;
const PIXEL: u8 = 8;
const QUAD: u8 = 9;
const TETRA: u8 = 10;
const VOXEL: u8 = 11;
const HEXAHEDRON: u8 = 12;
const WEDGE: u8 = 13;
const PYRAMID: u8 = 14;
const QUADRATIC_EDGE: u8 = 21;
const QUADRATIC_TRIANGLE: u8 = 22;
const QUADRATIC_QUAD: u8 = 23;
const QUADRATIC_TETRA: u8 = 24;
const QUADRATIC_HEXAHEDRON: u8 = 25;
const QUADRATIC_WEDGE: u8 = 26;
const QUADRATIC_PYRAMID: u8 = 27;
const BIQUADRATIC_QUAD: u8 = 28;
const TRIQUADRATIC_HEXAHEDRON: u8 = 29;
const BIQUADRATIC_QUADRATIC_WEDGE: u8 = 31;
const BIQUADRATIC_QUADRATIC_HEXAHEDRON: u8 = 32;
const BIQUADRATIC_TRIANGLE: u8 = 34;

const TET_FACES: &[&[usize]] = &[&[0, 1, 3], &[1, 2, 3], &[2, 0, 3], &[0, 2, 1]];
const HEX_FACES: &[&[usize]] = &[
    &[0, 4, 7, 3],
    &[1, 2, 6, 5],
    &[0, 1, 5, 4],
    &[3, 7, 6, 2],
    &[0, 3, 2, 1],
    &[4, 5, 6, 7],
];
const WEDGE_FACES: &[&[usize]] = &[
    &[0, 1, 2],
    &[3, 5, 4],
    &[0, 3, 4, 1],
    &[1, 4, 5, 2],
    &[2, 5, 3, 0],
];
const PYRAMID_FACES: &[&[usize]] = &[
    &[0, 3, 2, 1],
    &[0, 1, 4],
    &[1, 2, 4],
    &[2, 3, 4],
    &[3, 0, 4],
];

/// Outer faces of volume cells: a face seen twice is inside and removed.
#[derive(Default)]
struct Boundary {
    /// Sorted nodes → (first seen, nodes in cell order, node count).
    faces: HashMap<[u32; 4], (u64, [u32; 4], u8)>,
    seq: u64,
}

impl Boundary {
    fn toggle(&mut self, nodes: &[u32]) {
        let mut face = [u32::MAX; 4];
        let n = nodes.len().min(4);
        face[..n].copy_from_slice(&nodes[..n]);
        let mut key = face;
        key[..n].sort_unstable();
        if self.faces.remove(&key).is_none() {
            self.faces.insert(key, (self.seq, face, n as u8));
            self.seq += 1;
        }
    }

    fn cell(&mut self, nodes: &[u32], faces: &[&[usize]], order: Option<&[usize; 8]>) {
        let mut face = [0u32; 4];
        for f in faces {
            for (k, &i) in f.iter().enumerate() {
                face[k] = nodes[order.map_or(i, |o| o[i])];
            }
            self.toggle(&face[..f.len()]);
        }
    }
}

/// Keeps every k-th triangle so that at most `max` remain.
fn thin_triangles(tris: &mut Vec<u32>, max: usize) {
    let triangles = tris.len() / 3;
    if triangles > max && max > 0 {
        let stride = triangles.div_ceil(max);
        *tris = tris
            .chunks_exact(3)
            .step_by(stride)
            .flatten()
            .copied()
            .collect();
    }
}

/// Polygon → triangle fan.
fn fan(out: &mut Vec<u32>, polygon: &[u32]) {
    for k in 1..polygon.len().saturating_sub(1) {
        out.extend([polygon[0], polygon[k], polygon[k + 1]]);
    }
}

impl Grid {
    fn point_count(&self) -> usize {
        self.points.len() / 3
    }

    fn cell_count(&self) -> usize {
        self.ends.len()
    }

    fn check_cells(&self, more: usize, more_conn: usize) -> Result<()> {
        if self.cell_count().saturating_add(more) > MAX_CELLS {
            bail!("The result has more than {MAX_CELLS} cells – too many for the preview.");
        }
        if self.conn.len().saturating_add(more_conn) > MAX_CONNECTIVITY {
            bail!("The result has too many cell nodes for the preview.");
        }
        Ok(())
    }

    fn cell(&self, c: usize) -> &[u32] {
        let start = if c == 0 { 0 } else { self.ends[c - 1] };
        &self.conn[start..self.ends[c]]
    }

    /// Adds a field unless the field budget is used up.
    fn add_field(fields: &mut Vec<Field>, field: Field) {
        if fields.len() < MAX_FIELDS && !fields.iter().any(|f| f.name == field.name) {
            fields.push(field);
        }
    }

    fn into_mesh(mut self) -> Result<Mesh> {
        let n = self.point_count();
        if n == 0 {
            bail!("The result file contains no points.");
        }
        if self.types.len() != self.ends.len() {
            bail!("The cell types do not match the cells.");
        }
        if self.conn.iter().any(|&i| i as usize >= n) {
            bail!("A cell refers to a missing point.");
        }
        let cells = self.cell_count();

        // fields: point data, then cell data averaged to the points
        let mut fields: Vec<Field> = std::mem::take(&mut self.point_fields);
        fields.retain(|f| f.values.len() == n);
        for f in std::mem::take(&mut self.cell_fields) {
            if f.values.len() != cells || fields.len() >= MAX_FIELDS {
                continue;
            }
            let mut sum = vec![0f64; n];
            let mut hits = vec![0u32; n];
            for (c, &v) in f.values.iter().enumerate() {
                if v.is_finite() {
                    for &p in self.cell(c) {
                        sum[p as usize] += v as f64;
                        hits[p as usize] += 1;
                    }
                }
            }
            let values = sum
                .iter()
                .zip(&hits)
                .map(|(s, &h)| {
                    if h > 0 {
                        (s / h as f64) as f32
                    } else {
                        f32::NAN
                    }
                })
                .collect();
            Grid::add_field(
                &mut fields,
                Field {
                    name: format!("{} (cells)", f.name),
                    unit: f.unit,
                    values,
                },
            );
        }
        let keep = (FIELD_BUDGET / n).min(MAX_FIELDS);
        fields.truncate(keep);

        // surface triangles
        let mut tris: Vec<u32> = Vec::new();
        let mut boundary = Boundary::default();
        const VOXEL_ORDER: [usize; 8] = [0, 1, 3, 2, 4, 5, 7, 6];
        for c in 0..cells {
            let nodes = self.cell(c);
            let len = nodes.len();
            match self.types[c] {
                TRIANGLE | QUADRATIC_TRIANGLE | BIQUADRATIC_TRIANGLE if len >= 3 => {
                    tris.extend(&nodes[..3])
                }
                TRIANGLE_STRIP => {
                    for k in 0..len.saturating_sub(2) {
                        if k % 2 == 0 {
                            tris.extend([nodes[k], nodes[k + 1], nodes[k + 2]]);
                        } else {
                            tris.extend([nodes[k + 1], nodes[k], nodes[k + 2]]);
                        }
                    }
                }
                POLYGON => fan(&mut tris, nodes),
                PIXEL if len >= 4 => fan(&mut tris, &[nodes[0], nodes[1], nodes[3], nodes[2]]),
                QUAD | QUADRATIC_QUAD | BIQUADRATIC_QUAD if len >= 4 => fan(&mut tris, &nodes[..4]),
                TETRA | QUADRATIC_TETRA if len >= 4 => boundary.cell(nodes, TET_FACES, None),
                VOXEL if len >= 8 => boundary.cell(nodes, HEX_FACES, Some(&VOXEL_ORDER)),
                HEXAHEDRON
                | QUADRATIC_HEXAHEDRON
                | TRIQUADRATIC_HEXAHEDRON
                | BIQUADRATIC_QUADRATIC_HEXAHEDRON
                    if len >= 8 =>
                {
                    boundary.cell(nodes, HEX_FACES, None)
                }
                WEDGE | QUADRATIC_WEDGE | BIQUADRATIC_QUADRATIC_WEDGE if len >= 6 => {
                    boundary.cell(nodes, WEDGE_FACES, None)
                }
                PYRAMID | QUADRATIC_PYRAMID if len >= 5 => {
                    boundary.cell(nodes, PYRAMID_FACES, None)
                }
                // vertices, lines, beams, polyhedra and unknown cells are not drawn
                _ => {}
            }
            check_triangles(tris.len() / 3)?;
        }
        let mut outer: Vec<(u64, [u32; 4], u8)> = boundary.faces.into_values().collect();
        outer.sort_unstable_by_key(|f| f.0);
        for (_, face, k) in outer {
            fan(&mut tris, &face[..k as usize]);
            check_triangles(tris.len() / 3)?;
        }
        // drop degenerate triangles (collapsed hexahedra, repeated nodes)
        let mut kept = Vec::with_capacity(tris.len());
        for t in tris.chunks_exact(3) {
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                kept.extend_from_slice(t);
            }
        }
        let mut tris = kept;

        if tris.is_empty() {
            // no surface (only points, vertex or line cells): a point cloud
            let mut mesh = Mesh {
                positions: self.points,
                parts: 1,
                fields,
                ..Mesh::default()
            };
            thin_out(&mut mesh);
            mesh.bake_colors();
            return Ok(mesh);
        }

        thin_triangles(&mut tris, MAX_RESULT_TRIANGLES);

        // only the points of the surface
        let mut remap = vec![u32::MAX; n];
        let mut positions = Vec::new();
        let mut used: Vec<u32> = Vec::new();
        for i in tris.iter_mut() {
            let p = *i as usize;
            if remap[p] == u32::MAX {
                remap[p] = used.len() as u32;
                used.push(*i);
                positions.extend_from_slice(&self.points[p * 3..p * 3 + 3]);
            }
            *i = remap[p];
        }
        for f in &mut fields {
            f.values = used.iter().map(|&p| f.values[p as usize]).collect();
        }
        let mut mesh = Mesh {
            positions,
            indices: tris,
            parts: 1,
            fields,
            ..Mesh::default()
        };
        mesh.bake_colors();
        Ok(mesh)
    }
}

// ---------------------------------------------------------------------------
// legacy VTK
// ---------------------------------------------------------------------------

struct Legacy<'a> {
    data: &'a [u8],
    at: usize,
    binary: bool,
}

impl<'a> Legacy<'a> {
    /// The next line as bytes (without the line end).
    fn raw_line(&mut self) -> Option<&'a [u8]> {
        if self.at >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.at..];
        let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        self.at += (end + 1).min(rest.len());
        Some(&rest[..end])
    }

    /// Tokens of the next non-empty line; its line end is consumed, so binary
    /// data starts right after.
    fn line(&mut self) -> Result<Option<Vec<&'a str>>> {
        while let Some(raw) = self.raw_line() {
            let text = std::str::from_utf8(raw)
                .map_err(|_| anyhow!("unexpected binary data in a VTK section header"))?;
            let tokens: Vec<&str> = text.split_ascii_whitespace().collect();
            if !tokens.is_empty() {
                return Ok(Some(tokens));
            }
        }
        Ok(None)
    }

    /// Whether the next line starts with `keyword` (nothing is consumed).
    fn peek(&self, keyword: &str) -> bool {
        let rest = &self.data[self.at.min(self.data.len())..];
        let start = rest
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(rest.len());
        rest[start..]
            .get(..keyword.len())
            .is_some_and(|w| w.eq_ignore_ascii_case(keyword.as_bytes()))
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.at)
    }

    /// `count` numbers of type `ty`.
    fn values(
        &mut self,
        count: usize,
        ty: Num,
        f: &mut dyn FnMut(f64) -> Result<()>,
    ) -> Result<()> {
        if self.binary {
            let need = count.checked_mul(ty.size()).context("data too large")?;
            let bytes = self
                .data
                .get(self.at..)
                .and_then(|r| r.get(..need))
                .context("the VTK file is truncated (binary data shorter than declared)")?;
            self.at += need;
            each_binary(bytes, ty, true, count, f)
        } else {
            for _ in 0..count {
                let rest = &self.data[self.at.min(self.data.len())..];
                let start = rest
                    .iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .context("the VTK file is truncated (fewer values than declared)")?;
                let len = rest[start..]
                    .iter()
                    .position(|b| b.is_ascii_whitespace())
                    .unwrap_or(rest.len() - start);
                self.at += start + len;
                f(number(&rest[start..start + len])?)?;
            }
            Ok(())
        }
    }

    /// Bytes one value takes at least (allocation limit).
    fn min_bytes(&self, ty: Num) -> usize {
        if self.binary {
            ty.size()
        } else {
            2
        }
    }

    fn tuples(&mut self, tuples: usize, components: usize, ty: Num) -> Result<Vec<f32>> {
        if components == 0 || components > MAX_COMPONENTS {
            bail!("unsupported number of components ({components})");
        }
        let total = tuples.checked_mul(components).context("data too large")?;
        if total > self.remaining() / self.min_bytes(ty) {
            bail!("the VTK file is truncated (data shorter than declared)");
        }
        let mut t = Tuples::new(tuples, components, self.remaining());
        self.values(total, ty, &mut |v| t.push(v))?;
        Ok(t.out)
    }

    fn skip(&mut self, total: usize, ty: Num) -> Result<()> {
        if total > self.remaining() / self.min_bytes(ty) {
            bail!("the VTK file is truncated (data shorter than declared)");
        }
        self.values(total, ty, &mut |_| Ok(()))
    }

    /// `CELLS`, `POLYGONS`, … with `a` and `b` from the section line.
    fn cells(&mut self, grid: &mut Grid, a: usize, b: usize, poly_type: Option<u8>) -> Result<()> {
        grid.check_cells(a, b)?;
        let base = grid.conn.len();
        let first_cell = grid.cell_count();
        if self.peek("OFFSETS") {
            // file version 5: CELLS <offsets> <connectivity size>
            let t = self.line()?.context("truncated VTK file")?;
            let ty = Num::legacy(t.get(1).copied())?;
            if a > self.remaining() / self.min_bytes(ty) {
                bail!("the VTK file is truncated (offsets)");
            }
            let mut offsets: Vec<usize> = Vec::with_capacity(a);
            self.values(a, ty, &mut |v| {
                offsets.push(index(v)? as usize);
                Ok(())
            })?;
            let t = self
                .line()?
                .context("truncated VTK file (no CONNECTIVITY)")?;
            if !t[0].eq_ignore_ascii_case("CONNECTIVITY") {
                bail!("CONNECTIVITY expected after OFFSETS");
            }
            let ty = Num::legacy(t.get(1).copied())?;
            if b > self.remaining() / self.min_bytes(ty) {
                bail!("the VTK file is truncated (connectivity)");
            }
            grid.conn.reserve(b);
            self.values(b, ty, &mut |v| {
                grid.conn.push(index(v)?);
                Ok(())
            })?;
            if offsets.first().is_some_and(|&o| o != 0) || offsets.last().is_some_and(|&o| o != b) {
                bail!("invalid cell offsets");
            }
            for w in offsets.windows(2) {
                if w[1] < w[0] {
                    bail!("invalid cell offsets");
                }
                grid.ends.push(base + w[1]);
            }
        } else {
            // classic layout: n, id × n, n, id × n, …
            let ty = if self.binary { Num::I32 } else { Num::F64 };
            if b > self.remaining() / self.min_bytes(ty) {
                bail!("the VTK file is truncated (cells)");
            }
            grid.conn.reserve(b.saturating_sub(a));
            let mut left = 0usize;
            let mut seen = 0usize;
            let (conn, ends) = (&mut grid.conn, &mut grid.ends);
            self.values(b, ty, &mut |v| {
                let v = index(v)?;
                if left == 0 {
                    left = v as usize;
                    seen += 1;
                    if left == 0 {
                        ends.push(conn.len());
                    }
                } else {
                    conn.push(v);
                    left -= 1;
                    if left == 0 {
                        ends.push(conn.len());
                    }
                }
                Ok(())
            })?;
            if left != 0 || seen != a || grid.ends.len() != first_cell + a {
                bail!("the cell list does not match its declared size");
            }
        }
        if let Some(ty) = poly_type {
            let added = grid.cell_count() - first_cell;
            grid.types.extend(std::iter::repeat_n(ty, added));
        }
        Ok(())
    }
}

/// Legacy `.vtk` files (UNSTRUCTURED_GRID and POLYDATA).
pub fn vtk_legacy(data: &[u8]) -> Result<Mesh> {
    let mut r = Legacy {
        data,
        at: 0,
        binary: false,
    };
    let first = r.raw_line().unwrap_or_default();
    if !String::from_utf8_lossy(first)
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("# vtk")
    {
        bail!("Not a legacy VTK file (no '# vtk DataFile' header).");
    }
    r.raw_line(); // title
    let mode = r.line()?.context("the VTK file is truncated")?;
    r.binary = match mode[0].to_ascii_uppercase().as_str() {
        "ASCII" => false,
        "BINARY" => true,
        other => bail!("unknown VTK file type '{}'", short(other)),
    };
    let dataset = r.line()?.context("the VTK file is truncated")?;
    let kind = dataset.get(1).map(|s| s.to_ascii_uppercase());
    if !dataset[0].eq_ignore_ascii_case("DATASET") {
        bail!("DATASET expected in the VTK file");
    }
    let poly = match kind.as_deref() {
        Some("UNSTRUCTURED_GRID") => false,
        Some("POLYDATA") => true,
        Some(other) => bail!(
            "The 3D preview reads UNSTRUCTURED_GRID and POLYDATA VTK files, not {}.",
            short(other)
        ),
        None => bail!("DATASET without a type in the VTK file"),
    };
    let mut grid = Grid::default();
    // POINT_DATA / CELL_DATA: (is point data, count)
    let mut attr: Option<(bool, usize)> = None;
    while let Some(t) = r.line()? {
        let key = t[0].to_ascii_uppercase();
        let target = |grid: &mut Grid, attr: Option<(bool, usize)>, field: Field| {
            if let Some((is_point, n)) = attr {
                if field.values.len() == n {
                    let list = if is_point {
                        &mut grid.point_fields
                    } else {
                        &mut grid.cell_fields
                    };
                    Grid::add_field(list, field);
                }
            }
        };
        match key.as_str() {
            "POINTS" => {
                let n = count(t.get(1).copied(), "point count")?;
                let ty = Num::legacy(t.get(2).copied())?;
                if n > MAX_NODES {
                    bail!(
                        "The result has more than {MAX_NODES} points – too many for the preview."
                    );
                }
                if !grid.points.is_empty() {
                    bail!("the VTK file has two POINTS sections");
                }
                let total = n * 3;
                if total > r.remaining() / r.min_bytes(ty) {
                    bail!("the VTK file is truncated (points)");
                }
                grid.points.reserve(total);
                let points = &mut grid.points;
                r.values(total, ty, &mut |v| {
                    points.push(v as f32);
                    Ok(())
                })?;
            }
            "CELLS" if !poly => {
                let a = count(t.get(1).copied(), "cell count")?;
                let b = count(t.get(2).copied(), "cell list size")?;
                r.cells(&mut grid, a, b, None)?;
            }
            "VERTICES" | "LINES" | "POLYGONS" | "TRIANGLE_STRIPS" if poly => {
                let a = count(t.get(1).copied(), "cell count")?;
                let b = count(t.get(2).copied(), "cell list size")?;
                let ty = match key.as_str() {
                    "VERTICES" => POLY_VERTEX,
                    "LINES" => POLY_LINE,
                    "POLYGONS" => POLYGON,
                    _ => TRIANGLE_STRIP,
                };
                r.cells(&mut grid, a, b, Some(ty))?;
            }
            "CELL_TYPES" if !poly => {
                let n = count(t.get(1).copied(), "cell type count")?;
                let ty = if r.binary { Num::I32 } else { Num::F64 };
                if n > MAX_CELLS || n > r.remaining() / r.min_bytes(ty) {
                    bail!("the VTK file is truncated (cell types)");
                }
                let types = &mut grid.types;
                types.reserve(n);
                r.values(n, ty, &mut |v| {
                    types.push(if (0.0..=255.0).contains(&v) {
                        v as u8
                    } else {
                        0
                    });
                    Ok(())
                })?;
            }
            "POINT_DATA" | "CELL_DATA" => {
                let n = count(t.get(1).copied(), "attribute count")?;
                attr = Some((key == "POINT_DATA", n));
            }
            "SCALARS" | "VECTORS" | "NORMALS" | "TENSORS" | "TENSORS6" => {
                let (_, n) = attr.context("data values before POINT_DATA / CELL_DATA")?;
                let name = t.get(1).copied().unwrap_or("values").to_string();
                let ty = Num::legacy(t.get(2).copied())?;
                let components = match key.as_str() {
                    "SCALARS" => t.get(3).and_then(|c| c.parse().ok()).unwrap_or(1),
                    "TENSORS" => 9,
                    "TENSORS6" => 6,
                    _ => 3,
                };
                if key == "SCALARS" && r.peek("LOOKUP_TABLE") {
                    r.line()?;
                }
                if key == "NORMALS" {
                    r.skip(n.checked_mul(3).context("data too large")?, ty)?;
                } else {
                    let values = r.tuples(n, components, ty)?;
                    let name = Reduce::of(components).label(&name);
                    target(
                        &mut grid,
                        attr,
                        Field {
                            name,
                            unit: String::new(),
                            values,
                        },
                    );
                }
            }
            "TEXTURE_COORDINATES" => {
                let (_, n) = attr.context("data values before POINT_DATA / CELL_DATA")?;
                let dim = count(t.get(2).copied(), "dimension")?.min(MAX_COMPONENTS);
                let ty = Num::legacy(t.get(3).copied())?;
                r.skip(n.checked_mul(dim).context("data too large")?, ty)?;
            }
            "COLOR_SCALARS" => {
                let (_, n) = attr.context("data values before POINT_DATA / CELL_DATA")?;
                let k = count(t.get(2).copied(), "colour components")?.min(MAX_COMPONENTS);
                let ty = if r.binary { Num::U8 } else { Num::F64 };
                r.skip(n.checked_mul(k).context("data too large")?, ty)?;
            }
            "LOOKUP_TABLE" => {
                let n = count(t.get(2).copied(), "lookup table size")?;
                let ty = if r.binary { Num::U8 } else { Num::F64 };
                r.skip(n.checked_mul(4).context("data too large")?, ty)?;
            }
            "FIELD" => {
                let arrays = count(t.get(2).copied(), "array count")?;
                for _ in 0..arrays {
                    // METADATA blocks may follow an array
                    while r.peek("METADATA") {
                        r.line()?;
                        skip_metadata(&mut r);
                    }
                    let a = r.line()?.context("the VTK file is truncated (FIELD)")?;
                    let name = a[0].to_string();
                    let components = count(a.get(1).copied(), "component count")?;
                    let tuples = count(a.get(2).copied(), "tuple count")?;
                    let ty = Num::legacy(a.get(3).copied())?;
                    let values = r.tuples(tuples, components, ty)?;
                    let name = Reduce::of(components).label(&name);
                    target(
                        &mut grid,
                        attr,
                        Field {
                            name,
                            unit: String::new(),
                            values,
                        },
                    );
                }
            }
            "METADATA" => skip_metadata(&mut r),
            "DATASET" => bail!("the VTK file has two DATASET sections"),
            other => bail!("unsupported VTK section '{}'", short(other)),
        }
    }
    grid.into_mesh()
}

/// `METADATA` ends with an empty line.
fn skip_metadata(r: &mut Legacy<'_>) {
    while let Some(line) = r.raw_line() {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// VTK XML (.vtu, .vtp)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Section {
    Points,
    Cells,
    Verts,
    Lines,
    Strips,
    Polys,
    PointData,
    CellData,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    Ascii,
    Binary,
    Appended,
}

struct XmlArray<'a> {
    name: String,
    ty: Num,
    components: usize,
    format: Format,
    offset: usize,
    text: Cow<'a, [u8]>,
}

#[derive(Default)]
struct XmlPiece<'a> {
    points: usize,
    cells: usize,
    /// Verts, Lines, Strips, Polys
    poly_counts: [usize; 4],
    point_array: Option<XmlArray<'a>>,
    /// (section, array): connectivity, offsets, types
    topology: Vec<(Section, XmlArray<'a>)>,
    point_data: Vec<XmlArray<'a>>,
    cell_data: Vec<XmlArray<'a>>,
    active_scalars: Option<String>,
}

struct XmlFile<'a> {
    data: &'a [u8],
    big_endian: bool,
    header64: bool,
    zlib: bool,
    /// Start of the appended data (after the `_`) and its encoding.
    appended: Option<(usize, bool)>,
    /// End of the appended data.
    appended_end: usize,
    /// Offsets of all appended arrays, sorted (end of one array = next offset).
    offsets: Vec<usize>,
}

fn xml_attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Base64 that may consist of several separately encoded blocks (VTK encodes
/// the header of compressed data on its own, so `=` padding can appear in
/// the middle).
fn base64_blocks(text: &[u8]) -> Result<Vec<u8>> {
    let clean: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut start = 0;
    while start < clean.len() {
        let mut end = start;
        while end + 4 <= clean.len() && !clean[end..end + 4].contains(&b'=') {
            end += 4;
        }
        end = (end + 4).min(clean.len());
        base64::engine::general_purpose::STANDARD
            .decode_vec(&clean[start..end], &mut out)
            .map_err(|e| anyhow!("invalid base64 data: {e}"))?;
        start = end;
    }
    Ok(out)
}

impl<'a> XmlFile<'a> {
    fn header(&self, bytes: &[u8], i: usize) -> Result<usize> {
        let size = if self.header64 { 8 } else { 4 };
        let at = i.checked_mul(size).context("invalid data header")?;
        let b = bytes
            .get(at..at + size)
            .context("the data header is truncated")?;
        let v = Num::get(
            if self.header64 { Num::U64 } else { Num::U32 },
            b,
            self.big_endian,
        );
        usize::try_from(v as u64).map_err(|_| anyhow!("invalid data header"))
    }

    /// The data bytes behind a header (uncompressed or zlib blocks); only the
    /// first `need` bytes are decompressed.
    fn payload<'b>(&self, bytes: Cow<'b, [u8]>, need: usize) -> Result<Cow<'b, [u8]>> {
        let hs = if self.header64 { 8 } else { 4 };
        if !self.zlib {
            let n = self.header(&bytes, 0)?;
            let end = n.checked_add(hs).context("invalid data header")?;
            if end > bytes.len() {
                bail!("the result file is truncated (data block shorter than its header says)");
            }
            return Ok(match bytes {
                Cow::Borrowed(b) => Cow::Borrowed(&b[hs..end]),
                Cow::Owned(mut v) => {
                    v.truncate(end);
                    v.drain(..hs);
                    Cow::Owned(v)
                }
            });
        }
        let blocks = self.header(&bytes, 0)?;
        let block_size = self.header(&bytes, 1)?;
        let last = self.header(&bytes, 2)?;
        if blocks > bytes.len() / hs {
            bail!("invalid compressed data header");
        }
        let total = if blocks == 0 {
            0
        } else {
            block_size
                .checked_mul(blocks - 1)
                .and_then(|t| t.checked_add(if last == 0 { block_size } else { last }))
                .context("invalid compressed data header")?
        };
        if total > MAX_ARRAY_BYTES {
            bail!("a data array is too large for the preview");
        }
        let mut at = (3 + blocks) * hs;
        if at > bytes.len() {
            bail!("the compressed data header is truncated");
        }
        let want = need.min(total);
        let mut out = Vec::with_capacity(want);
        for i in 0..blocks {
            if out.len() >= want {
                break;
            }
            let size = self.header(&bytes, 3 + i)?;
            let end = at.checked_add(size).context("invalid compressed block")?;
            let block = bytes
                .get(at..end)
                .context("the result file is truncated (compressed block)")?;
            at = end;
            let full = if i + 1 == blocks && last != 0 {
                last
            } else {
                block_size
            };
            let take = full.min(want - out.len());
            let before = out.len();
            flate2::read::ZlibDecoder::new(block)
                .take(take as u64)
                .read_to_end(&mut out)
                .map_err(|e| anyhow!("corrupt compressed data: {e}"))?;
            if out.len() - before != take {
                bail!("corrupt compressed data (block shorter than declared)");
            }
        }
        Ok(Cow::Owned(out))
    }

    /// Calls `f` for the first `count` numbers of an array.
    fn values(
        &self,
        a: &XmlArray<'_>,
        count: usize,
        f: &mut dyn FnMut(f64) -> Result<()>,
    ) -> Result<()> {
        let need = count
            .checked_mul(a.ty.size())
            .context("data array too large")?;
        if need > MAX_ARRAY_BYTES {
            bail!("a data array is too large for the preview");
        }
        match a.format {
            Format::Ascii => {
                if count > a.text.len() / 2 + 1 {
                    bail!("the result file is truncated (fewer values than declared)");
                }
                each_ascii(&a.text, count, f)
            }
            Format::Binary => {
                let bytes = base64_blocks(&a.text)?;
                let data = self.payload(Cow::Owned(bytes), need)?;
                each_binary(&data, a.ty, self.big_endian, count, f)
            }
            Format::Appended => {
                let (base, raw) = self
                    .appended
                    .context("a DataArray refers to missing appended data")?;
                let start = base
                    .checked_add(a.offset)
                    .filter(|&s| s <= self.appended_end)
                    .context("a DataArray offset points outside the appended data")?;
                let end = self
                    .offsets
                    .iter()
                    .find(|&&o| o > a.offset)
                    .and_then(|&o| base.checked_add(o))
                    .unwrap_or(self.appended_end)
                    .min(self.appended_end);
                let region = &self.data[start..end.max(start)];
                let data = if raw {
                    self.payload(Cow::Borrowed(region), need)?
                } else {
                    self.payload(Cow::Owned(base64_blocks(region)?), need)?
                };
                each_binary(&data, a.ty, self.big_endian, count, f)
            }
        }
    }

    fn tuples(&self, a: &XmlArray<'_>, tuples: usize) -> Result<Field> {
        let components = a.components;
        if components == 0 || components > MAX_COMPONENTS {
            bail!("unsupported number of components ({components})");
        }
        let total = tuples.checked_mul(components).context("data too large")?;
        let mut t = Tuples::new(tuples, components, MAX_ARRAY_BYTES);
        t.out = Vec::with_capacity(tuples.min(MAX_NODES));
        self.values(a, total, &mut |v| t.push(v))?;
        Ok(Field {
            name: Reduce::of(components).label(&a.name),
            unit: String::new(),
            values: t.out,
        })
    }

    fn indices(&self, a: &XmlArray<'_>, count: usize) -> Result<Vec<usize>> {
        let mut out = Vec::new();
        self.values(a, count, &mut |v| {
            out.push(index(v)? as usize);
            Ok(())
        })?;
        Ok(out)
    }

    /// Cells of one topology section (`Cells`, `Polys`, …).
    fn topology(
        &self,
        grid: &mut Grid,
        piece: &XmlPiece<'_>,
        section: Section,
        cells: usize,
        base: u32,
        poly_type: Option<u8>,
    ) -> Result<()> {
        if cells == 0 {
            return Ok(());
        }
        let array = |name: &str| {
            piece
                .topology
                .iter()
                .find(|(s, a)| *s == section && a.name == name)
                .map(|(_, a)| a)
                .with_context(|| format!("the {section:?} section has no '{name}' array"))
        };
        grid.check_cells(cells, 0)?;
        let offsets = self.indices(array("offsets")?, cells)?;
        let total = offsets.last().copied().unwrap_or(0);
        grid.check_cells(cells, total)?;
        let start = grid.conn.len();
        let conn = &mut grid.conn;
        self.values(array("connectivity")?, total, &mut |v| {
            let i = index(v)?.checked_add(base).context("too many points")?;
            conn.push(i);
            Ok(())
        })?;
        let mut prev = 0;
        for &o in &offsets {
            if o < prev || o > total {
                bail!("invalid cell offsets");
            }
            prev = o;
            grid.ends.push(start + o);
        }
        match poly_type {
            Some(ty) => grid.types.extend(std::iter::repeat_n(ty, cells)),
            None => {
                let types = &mut grid.types;
                self.values(array("types")?, cells, &mut |v| {
                    types.push(if (0.0..=255.0).contains(&v) {
                        v as u8
                    } else {
                        0
                    });
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

/// Merges the fields of one piece into the grid's fields (by name; values
/// missing in a piece are NaN).
fn merge_fields(into: &mut Vec<Field>, mut new: Vec<Field>, first: bool, count: usize) {
    if first {
        for f in new {
            Grid::add_field(into, f);
        }
        return;
    }
    for f in into.iter_mut() {
        match new.iter_mut().find(|n| n.name == f.name) {
            Some(n) => f.values.append(&mut n.values),
            None => f.values.extend(std::iter::repeat_n(f32::NAN, count)),
        }
    }
}

/// VTK XML `UnstructuredGrid` (`.vtu`) and `PolyData` (`.vtp`) files.
pub fn vtk_xml(data: &[u8]) -> Result<Mesh> {
    use quick_xml::events::Event;
    // Raw appended data is binary and not XML: only the part before it is parsed.
    let appended_at = find(data, b"<AppendedData");
    let xml = &data[..appended_at.unwrap_or(data.len())];
    let mut file = XmlFile {
        data,
        big_endian: false,
        header64: false,
        zlib: false,
        appended: None,
        appended_end: data.len(),
        offsets: Vec::new(),
    };
    if let Some(at) = appended_at {
        let tag_end = data[at..]
            .iter()
            .position(|&b| b == b'>')
            .map(|p| at + p)
            .context("unterminated AppendedData tag")?;
        let tag = String::from_utf8_lossy(&data[at..tag_end]);
        let raw = !tag.contains("base64");
        let underscore = data[tag_end..]
            .iter()
            .position(|&b| b == b'_')
            .map(|p| tag_end + p + 1)
            .context("AppendedData without the '_' marker")?;
        file.appended = Some((underscore, raw));
        if let Some(end) = data[underscore..]
            .windows(b"</AppendedData".len())
            .rposition(|w| w == b"</AppendedData")
        {
            file.appended_end = underscore + end;
        }
    }

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut kind: Option<String> = None;
    let mut pieces: Vec<XmlPiece<'_>> = Vec::new();
    let mut section: Option<Section> = None;
    let mut array: Option<(Section, XmlArray<'_>)> = None;
    let mut nested = 0usize;
    loop {
        let event = reader.read_event().context("invalid VTK XML")?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                if array.is_some() {
                    if !empty {
                        nested += 1;
                    }
                    continue;
                }
                match e.local_name().as_ref() {
                    b"VTKFile" => {
                        kind = xml_attr(e, b"type");
                        file.big_endian =
                            xml_attr(e, b"byte_order").as_deref() == Some("BigEndian");
                        file.header64 = xml_attr(e, b"header_type").as_deref() == Some("UInt64");
                        match xml_attr(e, b"compressor").as_deref() {
                            None | Some("") => {}
                            Some("vtkZLibDataCompressor") => file.zlib = true,
                            Some(other) => bail!(
                                "The 3D preview reads uncompressed and zlib-compressed VTK files, not {}.",
                                short(other)
                            ),
                        }
                    }
                    b"Piece" => {
                        if pieces.len() >= 100_000 {
                            bail!("too many pieces");
                        }
                        let n = |name: &[u8]| -> Result<usize> {
                            match xml_attr(e, name) {
                                None => Ok(0),
                                Some(v) => {
                                    v.trim().parse().map_err(|_| anyhow!("invalid piece size"))
                                }
                            }
                        };
                        pieces.push(XmlPiece {
                            points: n(b"NumberOfPoints")?,
                            cells: n(b"NumberOfCells")?,
                            poly_counts: [
                                n(b"NumberOfVerts")?,
                                n(b"NumberOfLines")?,
                                n(b"NumberOfStrips")?,
                                n(b"NumberOfPolys")?,
                            ],
                            ..XmlPiece::default()
                        });
                    }
                    name => {
                        let s = match name {
                            b"Points" => Some(Section::Points),
                            b"Cells" => Some(Section::Cells),
                            b"Verts" => Some(Section::Verts),
                            b"Lines" => Some(Section::Lines),
                            b"Strips" => Some(Section::Strips),
                            b"Polys" => Some(Section::Polys),
                            b"PointData" => Some(Section::PointData),
                            b"CellData" => Some(Section::CellData),
                            _ => None,
                        };
                        if let Some(s) = s {
                            if s == Section::PointData {
                                if let Some(p) = pieces.last_mut() {
                                    p.active_scalars = xml_attr(e, b"Scalars");
                                }
                            }
                            if !empty {
                                section = Some(s);
                            }
                        } else if name == b"DataArray" {
                            if let (Some(s), false) = (section, pieces.is_empty()) {
                                let format = match xml_attr(e, b"format").as_deref() {
                                    Some("ascii") => Format::Ascii,
                                    Some("binary") => Format::Binary,
                                    Some("appended") => Format::Appended,
                                    other => bail!(
                                        "unknown DataArray format '{}'",
                                        short(other.unwrap_or(""))
                                    ),
                                };
                                let a = XmlArray {
                                    name: xml_attr(e, b"Name").unwrap_or_default(),
                                    ty: Num::xml(xml_attr(e, b"type").as_deref())?,
                                    components: match xml_attr(e, b"NumberOfComponents") {
                                        None => 1,
                                        Some(v) => v
                                            .trim()
                                            .parse()
                                            .map_err(|_| anyhow!("invalid NumberOfComponents"))?,
                                    },
                                    format,
                                    offset: match xml_attr(e, b"offset") {
                                        None => 0,
                                        Some(v) => v
                                            .trim()
                                            .parse()
                                            .map_err(|_| anyhow!("invalid DataArray offset"))?,
                                    },
                                    text: Cow::Borrowed(&[]),
                                };
                                if format == Format::Appended {
                                    file.offsets.push(a.offset);
                                }
                                if empty {
                                    store_array(&mut pieces, s, a);
                                } else {
                                    array = Some((s, a));
                                    nested = 0;
                                }
                            }
                        }
                    }
                }
            }
            Event::Text(t) => {
                if let (Some((_, a)), 0) = (array.as_mut(), nested) {
                    let bytes = t.into_inner();
                    if a.text.is_empty() {
                        a.text = bytes;
                    } else {
                        let text = a.text.to_mut();
                        text.push(b' ');
                        text.extend_from_slice(&bytes);
                    }
                }
            }
            Event::CData(t) => {
                if let (Some((_, a)), 0) = (array.as_mut(), nested) {
                    a.text.to_mut().extend_from_slice(&t.into_inner());
                }
            }
            Event::End(ref e) => {
                if array.is_some() {
                    if nested > 0 {
                        nested -= 1;
                    } else if let Some((s, a)) = array.take() {
                        store_array(&mut pieces, s, a);
                    }
                    continue;
                }
                let name = e.local_name();
                if matches!(
                    name.as_ref(),
                    b"Points"
                        | b"Cells"
                        | b"Verts"
                        | b"Lines"
                        | b"Strips"
                        | b"Polys"
                        | b"PointData"
                        | b"CellData"
                        | b"Piece"
                ) {
                    section = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let poly = match kind.as_deref() {
        Some("UnstructuredGrid") => false,
        Some("PolyData") => true,
        Some(other) => bail!(
            "The 3D preview reads VTK UnstructuredGrid (.vtu) and PolyData (.vtp) files, not {}.",
            short(other)
        ),
        None => bail!("Not a VTK XML file (no VTKFile element)."),
    };
    if pieces.is_empty() {
        bail!("The VTK file contains no Piece.");
    }
    file.offsets.sort_unstable();
    file.offsets.dedup();

    let mut grid = Grid::default();
    for (i, piece) in pieces.iter().enumerate() {
        let n = piece.points;
        if n.saturating_add(grid.point_count()) > MAX_NODES {
            bail!("The result has more than {MAX_NODES} points – too many for the preview.");
        }
        let base = grid.point_count() as u32;
        let cells_before = grid.cell_count();
        if n > 0 {
            let a = piece
                .point_array
                .as_ref()
                .context("a Piece has no Points")?;
            if a.components != 3 {
                bail!("Points must have three components");
            }
            let points = &mut grid.points;
            points.reserve(n.min(file.data.len() / 3) * 3);
            file.values(a, n * 3, &mut |v| {
                points.push(v as f32);
                Ok(())
            })?;
        }
        if poly {
            for (k, (s, ty)) in [
                (Section::Verts, POLY_VERTEX),
                (Section::Lines, POLY_LINE),
                (Section::Strips, TRIANGLE_STRIP),
                (Section::Polys, POLYGON),
            ]
            .into_iter()
            .enumerate()
            {
                file.topology(&mut grid, piece, s, piece.poly_counts[k], base, Some(ty))?;
            }
        } else {
            file.topology(&mut grid, piece, Section::Cells, piece.cells, base, None)?;
        }
        let cells = grid.cell_count() - cells_before;
        // point and cell data
        let mut point_fields = Vec::new();
        let mut ordered: Vec<&XmlArray<'_>> = piece.point_data.iter().collect();
        if let Some(active) = &piece.active_scalars {
            ordered.sort_by_key(|a| a.name != *active);
        }
        for a in ordered {
            if point_fields.len() < MAX_FIELDS {
                point_fields.push(file.tuples(a, n)?);
            }
        }
        let mut cell_fields = Vec::new();
        for a in &piece.cell_data {
            if cell_fields.len() < MAX_FIELDS {
                cell_fields.push(file.tuples(a, cells)?);
            }
        }
        merge_fields(&mut grid.point_fields, point_fields, i == 0, n);
        merge_fields(&mut grid.cell_fields, cell_fields, i == 0, cells);
    }
    grid.into_mesh()
}

fn store_array<'a>(pieces: &mut [XmlPiece<'a>], section: Section, a: XmlArray<'a>) {
    let Some(p) = pieces.last_mut() else {
        return;
    };
    match section {
        Section::Points => {
            if p.point_array.is_none() {
                p.point_array = Some(a);
            }
        }
        Section::PointData => p.point_data.push(a),
        Section::CellData => p.cell_data.push(a),
        s => p.topology.push((s, a)),
    }
}

// ---------------------------------------------------------------------------
// CalculiX .frd
// ---------------------------------------------------------------------------

/// Nodes of the FRD element types 1–12 and their VTK cell type.
fn frd_element(ty: usize) -> Option<(usize, u8)> {
    Some(match ty {
        1 => (8, HEXAHEDRON),
        2 => (6, WEDGE),
        3 => (4, TETRA),
        4 => (20, QUADRATIC_HEXAHEDRON),
        5 => (15, QUADRATIC_WEDGE),
        6 => (10, QUADRATIC_TETRA),
        7 => (3, TRIANGLE),
        8 => (6, QUADRATIC_TRIANGLE),
        9 => (4, QUAD),
        10 => (8, QUADRATIC_QUAD),
        11 => (2, LINE),
        12 => (3, QUADRATIC_EDGE),
        _ => return None,
    })
}

/// Fixed-width record of an FRD data line: `' -1'`, the id in `width`
/// columns, then numbers in 12 columns (E12.5). Falls back to whitespace
/// separated tokens for files written by other programs.
fn frd_record(line: &str, width: usize, values: usize, out: &mut Vec<f64>) -> Option<i64> {
    let start = out.len();
    let fixed = (|| {
        let id = line.get(3..3 + width)?.trim();
        let id = if id.is_empty() { 0 } else { id.parse().ok()? };
        for k in 0..values {
            let at = 3 + width + 12 * k;
            let s = line.get(at..(at + 12).min(line.len()))?.trim();
            out.push(s.parse().ok()?);
        }
        Some(id)
    })();
    if fixed.is_some() {
        return fixed;
    }
    out.truncate(start);
    let mut tokens = line.split_ascii_whitespace();
    let tag = tokens.next()?;
    let id = if tag == "-1" {
        tokens.next()?.parse().ok()?
    } else {
        0
    };
    for _ in 0..values {
        out.push(tokens.next()?.parse().ok()?);
    }
    Some(id)
}

/// Integers of an FRD element line, `width` columns each from column 3.
fn frd_ints(line: &str, width: usize, out: &mut Vec<i64>) -> Option<()> {
    let mut at = 3;
    let mut any = false;
    while at < line.len() {
        let s = line.get(at..(at + width).min(line.len()))?.trim();
        if !s.is_empty() {
            out.push(s.parse().ok()?);
            any = true;
        }
        at += width;
    }
    any.then_some(())
}

/// Priority of FRD result blocks in the field selector (stress first).
fn frd_priority(name: &str) -> u8 {
    if name.starts_with("STRESS") {
        0
    } else if name.starts_with("DISP") {
        1
    } else {
        2
    }
}

/// CalculiX result files (`.frd`, ASCII). Nodes, elements and every nodal
/// result block are read; a block that occurs several times (one per
/// increment) shows its last increment.
pub fn frd(data: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let mut grid = Grid::default();
    let mut ids: HashMap<i64, u32> = HashMap::new();
    let mut width = 10;
    // (block name, field) in order of appearance
    let mut fields: Vec<(String, Field)> = Vec::new();
    let mut seen_nodes = false;
    let mut buf: Vec<f64> = Vec::new();
    let mut ints: Vec<i64> = Vec::new();
    while let Some(line) = lines.next() {
        let head = line.trim_start();
        if head.starts_with("2C") || head.starts_with("3C") {
            let tokens: Vec<&str> = head.split_ascii_whitespace().collect();
            let format = if tokens.len() >= 3 {
                tokens.last().and_then(|t| t.parse::<u8>().ok())
            } else {
                None
            };
            match format {
                Some(2) => bail!("Binary FRD files have no preview – write the results as ASCII (*NODE FILE, OUTPUT=ASCII)."),
                Some(0) => width = 5,
                Some(_) => width = 10,
                None => {}
            }
            if head.starts_with("2C") {
                if seen_nodes {
                    bail!("the FRD file has two node blocks");
                }
                seen_nodes = true;
                for line in lines.by_ref() {
                    let tag = line.trim_start();
                    if tag.starts_with("-3") {
                        break;
                    }
                    if !tag.starts_with("-1") {
                        bail!("unexpected line in the FRD node block");
                    }
                    buf.clear();
                    let id =
                        frd_record(line, width, 3, &mut buf).context("invalid FRD node line")?;
                    if ids.len() >= MAX_NODES {
                        bail!("The result has more than {MAX_NODES} nodes – too many for the preview.");
                    }
                    let next = grid.point_count() as u32;
                    match ids.entry(id) {
                        std::collections::hash_map::Entry::Occupied(o) => {
                            let at = *o.get() as usize * 3;
                            for (p, &c) in grid.points[at..at + 3].iter_mut().zip(&buf) {
                                *p = c as f32;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(next);
                            grid.points.extend(buf.iter().map(|&c| c as f32));
                        }
                    }
                }
            } else {
                // elements: ' -1' id type group material, then ' -2' node lines
                let mut current: Option<(usize, u8)> = None;
                ints.clear();
                let finish = |grid: &mut Grid,
                              current: Option<(usize, u8)>,
                              ints: &mut Vec<i64>|
                 -> Result<()> {
                    if let Some((nodes, ty)) = current {
                        if ints.len() < nodes {
                            bail!("an FRD element has too few nodes");
                        }
                        grid.check_cells(1, nodes)?;
                        for id in &ints[..nodes] {
                            let p = ids
                                .get(id)
                                .context("an FRD element refers to a missing node")?;
                            grid.conn.push(*p);
                        }
                        grid.ends.push(grid.conn.len());
                        grid.types.push(ty);
                    }
                    ints.clear();
                    Ok(())
                };
                for line in lines.by_ref() {
                    let tag = line.trim_start();
                    if tag.starts_with("-3") {
                        break;
                    } else if tag.starts_with("-1") {
                        finish(&mut grid, current.take(), &mut ints)?;
                        // id (width) then type (I5): read the type column directly
                        let ty = line
                            .get(3 + width..3 + width + 5)
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .or_else(|| {
                                line.split_ascii_whitespace()
                                    .nth(2)
                                    .and_then(|s| s.parse().ok())
                            })
                            .context("invalid FRD element line")?;
                        // unknown types (springs, …) are read and not drawn
                        current = Some(frd_element(ty).unwrap_or((0, 0)));
                    } else if tag.starts_with("-2") {
                        if frd_ints(line, width, &mut ints).is_none() {
                            bail!("invalid FRD element node line");
                        }
                        if ints.len() > 64 {
                            bail!("an FRD element has too many nodes");
                        }
                    } else {
                        bail!("unexpected line in the FRD element block");
                    }
                }
                finish(&mut grid, current.take(), &mut ints)?;
            }
        } else if head.starts_with("-4") {
            // result block: -4 NAME ncomps irtype, -5 component lines, data
            let tokens: Vec<&str> = head.split_ascii_whitespace().collect();
            let name = tokens.get(1).copied().unwrap_or("RESULT").to_string();
            let comps = count(tokens.get(2).copied(), "FRD component count")?;
            if comps > MAX_COMPONENTS {
                bail!("an FRD result block has too many components");
            }
            let mut stored = 0usize;
            let mut tensor = true;
            for _ in 0..comps {
                let c = lines.next().context("the FRD file is truncated")?;
                let t: Vec<&str> = c.split_ascii_whitespace().collect();
                if t.first() != Some(&"-5") {
                    bail!("component line (-5) expected in the FRD result block");
                }
                // the sixth number (iexist) is 1 for components the post-processor computes
                let computed = t.get(6).is_some_and(|s| s.starts_with('1'));
                if !computed {
                    stored += 1;
                    tensor &= t.get(3) == Some(&"4");
                }
            }
            let n = grid.point_count();
            let mut values = vec![f32::NAN; n];
            let reduce = if stored == 6 && tensor {
                Reduce::VonMises6
            } else if stored <= 1 {
                Reduce::Value
            } else {
                Reduce::Magnitude
            };
            let mut node: Option<u32> = None;
            let flush = |node: Option<u32>, buf: &mut Vec<f64>, values: &mut Vec<f32>| {
                if let Some(p) = node {
                    if buf.len() >= stored && stored > 0 {
                        values[p as usize] = reduce.apply(&buf[..stored]);
                    }
                }
                buf.clear();
            };
            buf.clear();
            for line in lines.by_ref() {
                let tag = line.trim_start();
                if tag.starts_with("-3") {
                    break;
                } else if tag.starts_with("-1") {
                    flush(node.take(), &mut buf, &mut values);
                    let id = frd_record(line, width, stored.min(6), &mut buf)
                        .context("invalid FRD result line")?;
                    node = ids.get(&id).copied();
                } else if tag.starts_with("-2") {
                    let more = stored.saturating_sub(buf.len()).min(6);
                    frd_record(line, width, more, &mut buf).context("invalid FRD result line")?;
                } else {
                    bail!("unexpected line in the FRD result block");
                }
            }
            flush(node.take(), &mut buf, &mut values);
            if stored > 0 && n > 0 {
                let field = Field {
                    name: reduce.label(&name),
                    unit: String::new(),
                    values,
                };
                match fields.iter_mut().find(|(k, _)| *k == name) {
                    Some((_, f)) => *f = field,
                    None => fields.push((name, field)),
                }
            }
        } else if head.starts_with("9999") {
            break;
        }
    }
    if !seen_nodes {
        bail!("Not a CalculiX result file (no node block).");
    }
    fields.sort_by_key(|(name, _)| frd_priority(name));
    for (_, f) in fields {
        Grid::add_field(&mut grid.point_fields, f);
    }
    grid.into_mesh()
}

#[cfg(test)]
mod tests;
