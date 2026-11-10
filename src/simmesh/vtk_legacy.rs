//! Legacy VTK (`.vtk`): ASCII and BINARY, `UNSTRUCTURED_GRID` and `POLYDATA`.

use anyhow::{anyhow, bail, Context, Result};

use crate::mesh::{Field, Mesh};

use super::*;

pub(super) struct Legacy<'a> {
    pub(super) data: &'a [u8],
    pub(super) at: usize,
    pub(super) binary: bool,
}

impl<'a> Legacy<'a> {
    /// The data not read yet.
    pub(super) fn rest(&self) -> &'a [u8] {
        self.data.get(self.at..).unwrap_or_default()
    }

    /// The next line as bytes (without the line end).
    pub(super) fn raw_line(&mut self) -> Option<&'a [u8]> {
        let rest = self.rest();
        if rest.is_empty() {
            return None;
        }
        let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        self.at += (end + 1).min(rest.len());
        Some(rest.get(..end).unwrap_or(rest))
    }

    /// Tokens of the next non-empty line; its line end is consumed, so binary
    /// data starts right after.
    pub(super) fn line(&mut self) -> Result<Option<Vec<&'a str>>> {
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
    pub(super) fn peek(&self, keyword: &str) -> bool {
        let rest = self.rest();
        let start = rest
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(rest.len());
        rest.get(start..)
            .and_then(|r| r.get(..keyword.len()))
            .is_some_and(|w| w.eq_ignore_ascii_case(keyword.as_bytes()))
    }

    pub(super) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.at)
    }

    /// `count` numbers of type `ty`.
    pub(super) fn values(
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
                let rest = self.rest();
                let start = rest
                    .iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .context("the VTK file is truncated (fewer values than declared)")?;
                let token = rest.get(start..).unwrap_or_default();
                let len = token
                    .iter()
                    .position(|b| b.is_ascii_whitespace())
                    .unwrap_or(token.len());
                self.at += start + len;
                f(number(token.get(..len).unwrap_or(token))?)?;
            }
            Ok(())
        }
    }

    /// Whether `count` values of `ty` can still follow (allocation limit):
    /// a binary value takes its size, an ASCII value at least one digit and
    /// one separator – except the last one, which may end the file.
    pub(super) fn fits(&self, count: usize, ty: Num) -> bool {
        if self.binary {
            count <= self.remaining() / ty.size()
        } else {
            count <= self.remaining() / 2 + 1
        }
    }

    pub(super) fn tuples(&mut self, tuples: usize, components: usize, ty: Num) -> Result<Vec<f32>> {
        if components == 0 || components > MAX_COMPONENTS {
            bail!("unsupported number of components ({components})");
        }
        let total = tuples.checked_mul(components).context("data too large")?;
        if !self.fits(total, ty) {
            bail!("the VTK file is truncated (data shorter than declared)");
        }
        let mut t = Tuples::new(tuples, components, self.remaining());
        self.values(total, ty, &mut |v| t.push(v))?;
        Ok(t.out)
    }

    pub(super) fn skip(&mut self, total: usize, ty: Num) -> Result<()> {
        if !self.fits(total, ty) {
            bail!("the VTK file is truncated (data shorter than declared)");
        }
        self.values(total, ty, &mut |_| Ok(()))
    }

    /// `CELLS`, `POLYGONS`, … with `a` and `b` from the section line.
    pub(super) fn cells(
        &mut self,
        grid: &mut Grid,
        a: usize,
        b: usize,
        poly_type: Option<u8>,
    ) -> Result<()> {
        grid.check_cells(a, b)?;
        let base = grid.conn.len();
        let first_cell = grid.cell_count();
        if self.peek("OFFSETS") {
            // file version 5: CELLS <offsets> <connectivity size>
            let t = self.line()?.context("truncated VTK file")?;
            let ty = Num::legacy(t.get(1).copied())?;
            if !self.fits(a, ty) {
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
            if !t
                .first()
                .is_some_and(|k| k.eq_ignore_ascii_case("CONNECTIVITY"))
            {
                bail!("CONNECTIVITY expected after OFFSETS");
            }
            let ty = Num::legacy(t.get(1).copied())?;
            if !self.fits(b, ty) {
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
                let [from, to] = *w else { continue };
                if to < from {
                    bail!("invalid cell offsets");
                }
                grid.ends.push(base + to);
            }
        } else {
            // classic layout: n, id × n, n, id × n, …
            let ty = if self.binary { Num::I32 } else { Num::F64 };
            if !self.fits(b, ty) {
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
    let first = |t: &[&str]| t.first().copied().unwrap_or_default().to_ascii_uppercase();
    r.binary = match first(&mode).as_str() {
        "ASCII" => false,
        "BINARY" => true,
        other => bail!("unknown VTK file type '{}'", short(other)),
    };
    let dataset = r.line()?.context("the VTK file is truncated")?;
    let kind = dataset.get(1).map(|s| s.to_ascii_uppercase());
    if first(&dataset) != "DATASET" {
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
        let key = first(&t);
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
                if !r.fits(total, ty) {
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
                if n > MAX_CELLS || !r.fits(n, ty) {
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
                    let name = a.first().copied().unwrap_or_default().to_string();
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
pub(super) fn skip_metadata(r: &mut Legacy<'_>) {
    while let Some(line) = r.raw_line() {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            break;
        }
    }
}
