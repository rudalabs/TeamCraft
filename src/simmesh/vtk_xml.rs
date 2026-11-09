//! VTK XML: `UnstructuredGrid` (`.vtu`) and `PolyData` (`.vtp`), ascii, inline binary and appended data, optional zlib.

use std::borrow::Cow;
use std::io::Read;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;

use crate::mesh::{Field, Mesh, MAX_FIELDS};

use super::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Section {
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
pub(super) enum Format {
    Ascii,
    Binary,
    Appended,
}

pub(super) struct XmlArray<'a> {
    pub(super) name: String,
    pub(super) ty: Num,
    pub(super) components: usize,
    pub(super) format: Format,
    pub(super) offset: usize,
    pub(super) text: Cow<'a, [u8]>,
}

#[derive(Default)]
pub(super) struct XmlPiece<'a> {
    pub(super) points: usize,
    pub(super) cells: usize,
    /// Verts, Lines, Strips, Polys
    pub(super) poly_counts: [usize; 4],
    pub(super) point_array: Option<XmlArray<'a>>,
    /// (section, array): connectivity, offsets, types
    pub(super) topology: Vec<(Section, XmlArray<'a>)>,
    pub(super) point_data: Vec<XmlArray<'a>>,
    pub(super) cell_data: Vec<XmlArray<'a>>,
    pub(super) active_scalars: Option<String>,
}

pub(super) struct XmlFile<'a> {
    pub(super) data: &'a [u8],
    pub(super) big_endian: bool,
    pub(super) header64: bool,
    pub(super) zlib: bool,
    /// Start of the appended data (after the `_`) and its encoding.
    pub(super) appended: Option<(usize, bool)>,
    /// End of the appended data.
    pub(super) appended_end: usize,
    /// Offsets of all appended arrays, sorted (end of one array = next offset).
    pub(super) offsets: Vec<usize>,
}

pub(super) fn xml_attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

pub(super) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Base64 that may consist of several separately encoded blocks (VTK encodes
/// the header of compressed data on its own, so `=` padding can appear in
/// the middle).
pub(super) fn base64_blocks(text: &[u8]) -> Result<Vec<u8>> {
    let clean: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut start = 0;
    while start < clean.len() {
        let mut end = start;
        while clean
            .get(end..end + 4)
            .is_some_and(|quad| !quad.contains(&b'='))
        {
            end += 4;
        }
        end = (end + 4).min(clean.len());
        base64::engine::general_purpose::STANDARD
            .decode_vec(clean.get(start..end).unwrap_or_default(), &mut out)
            .map_err(|e| anyhow!("invalid base64 data: {e}"))?;
        start = end;
    }
    Ok(out)
}

impl<'a> XmlFile<'a> {
    pub(super) fn header(&self, bytes: &[u8], i: usize) -> Result<usize> {
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
    pub(super) fn payload<'b>(&self, bytes: Cow<'b, [u8]>, need: usize) -> Result<Cow<'b, [u8]>> {
        let hs = if self.header64 { 8 } else { 4 };
        if !self.zlib {
            let n = self.header(&bytes, 0)?;
            let end = n.checked_add(hs).context("invalid data header")?;
            let truncated =
                "the result file is truncated (data block shorter than its header says)";
            if end > bytes.len() {
                bail!(truncated);
            }
            return Ok(match bytes {
                Cow::Borrowed(b) => Cow::Borrowed(b.get(hs..end).context(truncated)?),
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
        // the header may lie: reserve no more than zlib can expand the
        // bytes at hand to (at most 1032 : 1), the rest grows when it comes
        let mut out = Vec::with_capacity(want.min(bytes.len().saturating_mul(1032)));
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
    pub(super) fn values(
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
                // offsets are sorted: binary search (files may have many arrays)
                let end = self
                    .offsets
                    .get(self.offsets.partition_point(|&o| o <= a.offset))
                    .and_then(|&o| base.checked_add(o))
                    .unwrap_or(self.appended_end)
                    .min(self.appended_end);
                let region = self.data.get(start..end.max(start)).unwrap_or_default();
                let data = if raw {
                    self.payload(Cow::Borrowed(region), need)?
                } else {
                    self.payload(Cow::Owned(base64_blocks(region)?), need)?
                };
                each_binary(&data, a.ty, self.big_endian, count, f)
            }
        }
    }

    pub(super) fn tuples(&self, a: &XmlArray<'_>, tuples: usize) -> Result<Field> {
        let components = a.components;
        if components == 0 || components > MAX_COMPONENTS {
            bail!("unsupported number of components ({components})");
        }
        let total = tuples.checked_mul(components).context("data too large")?;
        let mut t = Tuples::new(tuples, components, MAX_NODES);
        self.values(a, total, &mut |v| t.push(v))?;
        Ok(Field {
            name: Reduce::of(components).label(&a.name),
            unit: String::new(),
            values: t.out,
        })
    }

    pub(super) fn indices(&self, a: &XmlArray<'_>, count: usize) -> Result<Vec<usize>> {
        let mut out = Vec::new();
        self.values(a, count, &mut |v| {
            out.push(index(v)? as usize);
            Ok(())
        })?;
        Ok(out)
    }

    /// Cells of one topology section (`Cells`, `Polys`, …).
    pub(super) fn topology(
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
/// missing in a piece are NaN). `first`: no values before this piece – the
/// fields of empty pieces before it (parallel writers emit those) are
/// replaced, so they do not hide the fields of the first real piece.
pub(super) fn merge_fields(into: &mut Vec<Field>, mut new: Vec<Field>, first: bool, count: usize) {
    if first {
        into.clear();
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
    let xml = appended_at.and_then(|at| data.get(..at)).unwrap_or(data);
    // the data from byte `i` on (empty past the end)
    let from = |i: usize| data.get(i..).unwrap_or_default();
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
        let tag_end = from(at)
            .iter()
            .position(|&b| b == b'>')
            .map(|p| at + p)
            .context("unterminated AppendedData tag")?;
        let tag = String::from_utf8_lossy(data.get(at..tag_end).unwrap_or_default());
        let raw = !tag.contains("base64");
        let underscore = from(tag_end)
            .iter()
            .position(|&b| b == b'_')
            .map(|p| tag_end + p + 1)
            .context("AppendedData without the '_' marker")?;
        file.appended = Some((underscore, raw));
        if let Some(end) = from(underscore)
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
                    "VTKFile" => {
                        kind = xml_attr(e, "type");
                        file.big_endian = xml_attr(e, "byte_order").as_deref() == Some("BigEndian");
                        file.header64 = xml_attr(e, "header_type").as_deref() == Some("UInt64");
                        match xml_attr(e, "compressor").as_deref() {
                            None | Some("") => {}
                            Some("vtkZLibDataCompressor") => file.zlib = true,
                            Some(other) => bail!(
                                "The 3D preview reads uncompressed and zlib-compressed VTK files, not {}.",
                                short(other)
                            ),
                        }
                    }
                    "Piece" => {
                        if pieces.len() >= 100_000 {
                            bail!("too many pieces");
                        }
                        let n = |name: &str| -> Result<usize> {
                            match xml_attr(e, name) {
                                None => Ok(0),
                                Some(v) => {
                                    v.trim().parse().map_err(|_| anyhow!("invalid piece size"))
                                }
                            }
                        };
                        pieces.push(XmlPiece {
                            points: n("NumberOfPoints")?,
                            cells: n("NumberOfCells")?,
                            poly_counts: [
                                n("NumberOfVerts")?,
                                n("NumberOfLines")?,
                                n("NumberOfStrips")?,
                                n("NumberOfPolys")?,
                            ],
                            ..XmlPiece::default()
                        });
                    }
                    name => {
                        let s = match name {
                            "Points" => Some(Section::Points),
                            "Cells" => Some(Section::Cells),
                            "Verts" => Some(Section::Verts),
                            "Lines" => Some(Section::Lines),
                            "Strips" => Some(Section::Strips),
                            "Polys" => Some(Section::Polys),
                            "PointData" => Some(Section::PointData),
                            "CellData" => Some(Section::CellData),
                            _ => None,
                        };
                        if let Some(s) = s {
                            if s == Section::PointData {
                                if let Some(p) = pieces.last_mut() {
                                    p.active_scalars = xml_attr(e, "Scalars");
                                }
                            }
                            if !empty {
                                section = Some(s);
                            }
                        } else if name == "DataArray" {
                            if let (Some(s), false) = (section, pieces.is_empty()) {
                                let format = match xml_attr(e, "format").as_deref() {
                                    Some("ascii") => Format::Ascii,
                                    Some("binary") => Format::Binary,
                                    Some("appended") => Format::Appended,
                                    other => bail!(
                                        "unknown DataArray format '{}'",
                                        short(other.unwrap_or(""))
                                    ),
                                };
                                let a = XmlArray {
                                    name: xml_attr(e, "Name").unwrap_or_default(),
                                    ty: Num::xml(xml_attr(e, "type").as_deref())?,
                                    components: match xml_attr(e, "NumberOfComponents") {
                                        None => 1,
                                        Some(v) => v
                                            .trim()
                                            .parse()
                                            .map_err(|_| anyhow!("invalid NumberOfComponents"))?,
                                    },
                                    format,
                                    offset: match xml_attr(e, "offset") {
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
                    let bytes = match t.into_inner() {
                        std::borrow::Cow::Borrowed(s) => std::borrow::Cow::Borrowed(s.as_bytes()),
                        std::borrow::Cow::Owned(s) => std::borrow::Cow::Owned(s.into_bytes()),
                    };
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
                    a.text.to_mut().extend_from_slice(t.into_inner().as_bytes());
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
                    "Points"
                        | "Cells"
                        | "Verts"
                        | "Lines"
                        | "Strips"
                        | "Polys"
                        | "PointData"
                        | "CellData"
                        | "Piece"
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
    for piece in &pieces {
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
            for ((s, ty), count) in [
                (Section::Verts, POLY_VERTEX),
                (Section::Lines, POLY_LINE),
                (Section::Strips, TRIANGLE_STRIP),
                (Section::Polys, POLYGON),
            ]
            .into_iter()
            .zip(piece.poly_counts)
            {
                file.topology(&mut grid, piece, s, count, base, Some(ty))?;
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
        merge_fields(&mut grid.point_fields, point_fields, base == 0, n);
        merge_fields(&mut grid.cell_fields, cell_fields, cells_before == 0, cells);
    }
    grid.into_mesh()
}

pub(super) fn store_array<'a>(pieces: &mut [XmlPiece<'a>], section: Section, a: XmlArray<'a>) {
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

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn base64_blocks_are_decoded_one_after_the_other() {
        // VTK writes the header and the data as separately padded blocks
        let text = format!("{}  \n{}\n", b64(b"ab"), b64(b"cdef"));
        assert!(b64(b"ab").ends_with('='), "the first block is padded");
        assert_eq!(base64_blocks(text.as_bytes()).unwrap(), b"abcdef");
        // one unpadded block, and nothing at all
        assert_eq!(base64_blocks(b64(b"abc").as_bytes()).unwrap(), b"abc");
        assert!(base64_blocks(b"").unwrap().is_empty());
        let err = base64_blocks(b"@@@@").unwrap_err().to_string();
        assert!(err.contains("invalid base64"), "{err}");
    }

    #[test]
    fn fields_of_several_pieces_are_joined() {
        let field = |name: &str, values: &[f32]| Field {
            name: name.into(),
            unit: String::new(),
            values: values.to_vec(),
        };
        let mut fields = Vec::new();
        // the first piece decides which fields there are
        merge_fields(
            &mut fields,
            vec![field("T", &[1.0]), field("P", &[2.0])],
            true,
            1,
        );
        // a later piece: values are appended; a field it lacks gets NaN for
        // its points; a field only it has is left out
        merge_fields(
            &mut fields,
            vec![field("T", &[3.0, 4.0]), field("Q", &[9.0])],
            false,
            2,
        );
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].values, [1.0, 3.0, 4.0]);
        assert_eq!(fields[1].values[0], 2.0);
        assert!(fields[1].values[1..].iter().all(|v| v.is_nan()));
        assert_eq!(fields[1].values.len(), 3);
        // `first` starts over
        merge_fields(&mut fields, vec![field("Z", &[0.0])], true, 1);
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn bytes_and_attributes() {
        assert_eq!(find(b"<VTKFile><AppendedData>", b"<AppendedData"), Some(9));
        assert_eq!(find(b"<VTKFile>", b"<AppendedData"), None);
        assert_eq!(find(b"ab", b"abc"), None, "needle longer than the text");

        let tag = quick_xml::events::BytesStart::from_content(
            r#"DataArray type="Float32" Name="Stress &amp; strain" vtk:format="ascii""#,
            9,
        );
        assert_eq!(xml_attr(&tag, "type").as_deref(), Some("Float32"));
        assert_eq!(xml_attr(&tag, "Name").as_deref(), Some("Stress & strain"));
        // the local name counts, a namespace prefix does not
        assert_eq!(xml_attr(&tag, "format").as_deref(), Some("ascii"));
        assert_eq!(xml_attr(&tag, "NumberOfComponents"), None);
    }
}
