//! Stand-alone JT files, version 10 (ISO 14306; the exchange format of the
//! automotive supply chain).
//!
//! ```text
//! file      80 bytes version text ("Version 10.5 JT …"), u8 byte order (0 = LE),
//!           i32 reserved, u64 offset of the TOC, GUID of the scene graph segment
//! TOC       i32 count, count × (GUID, u64 offset, u32 length, u32 attributes);
//!           the segment type is the high byte of the attributes
//! segment   GUID, i32 type, i32 length, data; the scene graph (type 1) and the
//!           other "compressed" types start with i32 flag, i32 length, u8
//!           algorithm: 3 = LZMA in the .xz container, 2 = zlib
//! ```
//!
//! The **scene graph** (LSG) holds the nodes, then the property atoms, then
//! the property table (node → key/value atoms). Partition, group, part,
//! LOD and switch nodes have children, an instance node shares a subtree,
//! a tri-strip shape node points through its late-loaded property
//! `JT_LLPROP_SHAPEIMPL` to the shape LOD segment that holds its triangles.
//! Transforms are 4 × 4 row-vector matrices (`p' = p · A · M`), stored as
//! the elements that differ from the identity. Range-LOD child 0 is the
//! finest level; nodes with the ignore flag or the property
//! `__PLM_PS_Hidden` are skipped; lengths are in `JT_PROP_MEASUREMENT_UNITS`.
//!
//! The **shape LOD** is the JT 9 topologically compressed tri-strip set
//! (see `jt`), with the third-generation integer codec of JT 10 (nibbler
//! bitlength, an escape flag per context entry, move-to-front, out-of-band
//! values only when the context has an escape), raw `f32` or quantised
//! coordinates and combined Deering normal codes. The composite hash of the
//! topology is checked, so a misread layout is an error, not a wrong mesh.
//! NX writes two blocks the specification does not list (after the vertex
//! bindings a length and a 16-byte id, before the first packet five bytes);
//! they are recognised by that length.
//!
//! Algorithms after the published *JT File Format Reference* 10.0 and 10.5
//! (Siemens); the JT data format is © Siemens. Everything is read
//! defensively: sizes are capped, every index is checked, a damaged file is
//! an error, never a panic or an endless loop.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{anyhow, bail, ensure, Result};

use super::jt::{
    arithmetic, build_lod, guid, lag1, Bits, DeeringTable, Entry, Reader, Symbols, MAX_DEPTH,
    MAX_VALUES, TRI_STRIP_SET_LOD,
};
use super::pn::{refine_level, Lod};
use super::{check_triangles, Mesh};

/// Largest decompressed scene graph segment.
const MAX_SEGMENT_BYTES: u64 = 256 * 1024 * 1024;
/// Most nodes and property atoms of a scene graph.
const MAX_ELEMENTS: usize = 2_000_000;
/// Deepest nesting of scene graph nodes.
const MAX_GRAPH_DEPTH: usize = 64;
/// Most shape occurrences (instances can multiply shared subtrees).
const MAX_OCCURRENCES: usize = 200_000;
/// Scene graph nodes visited at most (shared subtrees counted each time).
const MAX_VISITS: usize = 4_000_000;
/// Values in the move-to-front window.
const MTF_WINDOW: usize = 16;

const END_OF_ELEMENTS: [u8; 16] = [0xFF; 16];
const fn jt_guid(a: u32) -> [u8; 16] {
    guid(
        a,
        0x2ac8,
        0x11d1,
        [0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59, 0x97],
    )
}
const fn part_guid(a: u32) -> [u8; 16] {
    guid(
        a,
        0x38fb,
        0x11d1,
        [0xa5, 0x06, 0x00, 0x60, 0x97, 0xbd, 0xc6, 0xe1],
    )
}
const PARTITION_NODE: [u8; 16] = jt_guid(0x10dd_103e);
const GROUP_NODE: [u8; 16] = jt_guid(0x10dd_101b);
const INSTANCE_NODE: [u8; 16] = jt_guid(0x10dd_102a);
const LOD_NODE: [u8; 16] = jt_guid(0x10dd_102c);
const RANGE_LOD_NODE: [u8; 16] = jt_guid(0x10dd_104c);
const SWITCH_NODE: [u8; 16] = jt_guid(0x10dd_10f3);
const TRI_STRIP_SHAPE_NODE: [u8; 16] = jt_guid(0x10dd_1077);
const PART_NODE: [u8; 16] = part_guid(0xce35_7245);
const META_DATA_NODE: [u8; 16] = part_guid(0xce35_7244);
const TRANSFORM_ATTRIBUTE: [u8; 16] = jt_guid(0x10dd_1083);
const STRING_ATOM: [u8; 16] = jt_guid(0x10dd_106e);
const INTEGER_ATOM: [u8; 16] = jt_guid(0x10dd_102b);
const LATE_LOADED_ATOM: [u8; 16] = guid(
    0xe0b0_5be5,
    0xfbbd,
    0x11d1,
    [0xa3, 0xa7, 0x00, 0xaa, 0x00, 0xd1, 0x09, 0x54],
);

/// Points and triangles of one decoded (and refined) shape.
type Shape = (Vec<[f64; 3]>, Vec<[u32; 3]>);

fn damaged() -> anyhow::Error {
    anyhow!("The JT file is damaged.")
}

/// The 3D preview of a stand-alone JT file. JT 10: every visible tri-strip
/// shape at its place, in millimetres, one part per occurrence; JT 9 (as NX
/// writes it) goes to the reader of the JT inside NX parts.
pub(crate) fn read(data: &[u8]) -> Result<Mesh> {
    if data.starts_with(b"Version 9.") {
        return super::jt::read_file(data);
    }
    let file = JtFile::open(data)?;
    let graph = Graph::read(&file.segment(file.lsg)?.ok_or_else(damaged)?)?;
    let (occurrences, scale) = graph.occurrences()?;

    // decode every shape once; refine with the budget of all occurrences
    let mut lods: HashMap<[u8; 16], Option<Lod>> = HashMap::new();
    let mut total = 0usize;
    for o in &occurrences {
        let lod = match lods.entry(o.segment) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(match file.segment(o.segment)? {
                    Some(bytes) => read_shape(&bytes)?,
                    None => None,
                })
            }
        };
        if let Some(lod) = lod {
            total = total.saturating_add(lod.triangles.len());
        }
    }
    check_triangles(total)?;
    let level = refine_level(total);
    let mut refined: HashMap<[u8; 16], Shape> = HashMap::new();
    for (id, lod) in lods {
        let Some(lod) = lod else { continue };
        let shape = if level > 1 && lod.normals_valid() {
            super::pn::refine(
                &lod.points,
                &lod.triangles,
                &lod.normals,
                &lod.normal_triangles,
                level,
            )
        } else {
            (lod.points, lod.triangles)
        };
        refined.insert(id, shape);
    }

    let mut mesh = Mesh::default();
    for o in &occurrences {
        let Some((points, triangles)) = refined.get(&o.segment) else {
            continue;
        };
        check_triangles(mesh.triangle_count() + triangles.len())?;
        let mut part = Mesh {
            parts: 1,
            ..Mesh::default()
        };
        for p in points {
            let q = apply(&o.matrix, p).map(|c| c * scale);
            ensure!(q.iter().all(|c| c.is_finite()), damaged());
            part.vertex(q);
        }
        let mirrored = determinant(&o.matrix) < 0.0;
        for &[a, b, c] in triangles {
            part.indices
                .extend(if mirrored { [a, c, b] } else { [a, b, c] });
        }
        mesh.append(part);
    }
    if mesh.triangle_count() == 0 {
        bail!(
            "The JT file contains no tessellated shapes (JT files that keep their parts in \
             other files are not read yet)."
        );
    }
    Ok(mesh)
}

/// `p · M` for a row vector `p` (translation in the last row).
fn apply(m: &[[f64; 4]; 4], p: &[f64; 3]) -> [f64; 3] {
    let [x, y, z] = *p;
    let [r0, r1, r2, r3] = m;
    let mut q = [0.0; 3];
    for ((((out, a), b), c), d) in q.iter_mut().zip(r0).zip(r1).zip(r2).zip(r3) {
        *out = x * a + y * b + z * c + d;
    }
    q
}

/// Determinant of the 3 × 3 rotation-and-scale part (negative: mirrored).
fn determinant(m: &[[f64; 4]; 4]) -> f64 {
    let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = *m;
    a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
}

// ---------------------------------------------------------------------------
// container
// ---------------------------------------------------------------------------

struct TocEntry {
    id: [u8; 16],
    offset: usize,
    length: usize,
}

struct JtFile<'a> {
    data: &'a [u8],
    lsg: [u8; 16],
    toc: Vec<TocEntry>,
}

impl<'a> JtFile<'a> {
    fn open(data: &'a [u8]) -> Result<JtFile<'a>> {
        let head = super::range(data, 0, 80)?;
        ensure!(head.starts_with(b"Version "), "This is not a JT file.");
        let version = String::from_utf8_lossy(head);
        let number = version
            .trim_start_matches("Version ")
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string();
        let major: u32 = number
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        if major != 10 {
            bail!("The JT file has version {number} – the 3D preview reads JT 9 and 10.");
        }
        ensure!(
            super::bytes_at::<1>(data, 80)? == [0],
            "Big-endian JT data is not supported."
        );
        let toc_at = usize::try_from(super::u64_le(data, 85)?).map_err(|_| damaged())?;
        let lsg: [u8; 16] = super::bytes_at(data, 93)?;
        let mut r = Reader::at(data, toc_at);
        let count = r.count(32)?;
        let mut toc = Vec::with_capacity(count);
        for _ in 0..count {
            let id: [u8; 16] = r.array()?;
            let offset = usize::try_from(u64::from_le_bytes(r.array()?)).map_err(|_| damaged())?;
            let length = usize::try_from(r.u32()?).map_err(|_| damaged())?;
            r.u32()?; // attributes (segment type in the high byte)
            toc.push(TocEntry { id, offset, length });
        }
        Ok(JtFile { data, lsg, toc })
    }

    /// The data of a segment, decompressed; `None` when the TOC lacks it.
    fn segment(&self, id: [u8; 16]) -> Result<Option<Vec<u8>>> {
        let Some(e) = self.toc.iter().find(|e| e.id == id) else {
            return Ok(None);
        };
        let end = e.offset.checked_add(e.length).ok_or_else(damaged)?;
        let segment = super::range(self.data, e.offset, end)?;
        ensure!(segment.len() >= 24, damaged());
        let kind = super::i32_le(segment, 16)?;
        let body = segment.get(24..).unwrap_or_default();
        // shape LOD segments (6..=16) are never compressed as a whole
        if !(6..=16).contains(&kind) && body.len() >= 9 {
            let flag = super::i32_le(body, 0)?;
            if flag == 2 || flag == 3 {
                let length = usize::try_from(super::i32_le(body, 4)?).map_err(|_| damaged())?;
                let algorithm = body.get(8).copied().unwrap_or(0);
                let packed = super::range(body, 9, 8usize.saturating_add(length))?;
                return Ok(Some(match algorithm {
                    3 => xz(packed)?,
                    2 => zlib(packed)?,
                    _ => packed.to_vec(),
                }));
            }
        }
        Ok(Some(body.to_vec()))
    }
}

/// A writer that refuses more than [`MAX_SEGMENT_BYTES`].
struct Capped(Vec<u8>);

impl std::io::Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.len() as u64 + buf.len() as u64 > MAX_SEGMENT_BYTES {
            return Err(std::io::Error::other("segment too large"));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn xz(packed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Capped(Vec::new());
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(packed), &mut out)
        .map_err(|_| anyhow!("The JT file is damaged (LZMA data)."))?;
    Ok(out.0)
}

fn zlib(packed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(packed)
        .take(MAX_SEGMENT_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|_| anyhow!("The JT file is damaged (zlib data)."))?;
    ensure!((out.len() as u64) <= MAX_SEGMENT_BYTES, damaged());
    Ok(out)
}

// ---------------------------------------------------------------------------
// scene graph
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Children are traversed (partition, group, part, meta data).
    Group,
    /// Only the first child: the finest level of detail.
    Lod,
    /// Only the selected child.
    Switch(i32),
    Instance,
    TriStripShape,
    Other,
}

struct Node {
    kind: Kind,
    part: bool,
    flags: u32,
    attributes: Vec<i32>,
    children: Vec<i32>,
}

enum Atom {
    Text(String),
    Integer(i32),
    LateLoaded([u8; 16]),
}

#[derive(Default)]
struct Graph {
    root: Option<i32>,
    nodes: HashMap<i32, Node>,
    transforms: HashMap<i32, [[f64; 4]; 4]>,
    atoms: HashMap<i32, Atom>,
    properties: HashMap<i32, Vec<(i32, i32)>>,
}

/// A shape at its place in the assembly.
struct Occurrence {
    segment: [u8; 16],
    matrix: [[f64; 4]; 4],
}

const IDENTITY: [[f64; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn multiply(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut m = [[0.0; 4]; 4];
    for (row, a_row) in m.iter_mut().zip(a) {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a_row
                .iter()
                .zip(b)
                .map(|(x, b_row)| x * b_row.get(j).copied().unwrap_or(0.0))
                .sum();
        }
    }
    m
}

/// One logical element: object type, object id, the data after the id.
struct Element<'a> {
    kind: [u8; 16],
    id: i32,
    body: &'a [u8],
}

/// Elements up to the end marker; returns them and the offset after it.
fn elements(d: &[u8], mut at: usize) -> Result<(Vec<Element<'_>>, usize)> {
    let mut out = Vec::new();
    loop {
        let length = usize::try_from(super::i32_le(d, at)?).map_err(|_| damaged())?;
        let kind: [u8; 16] = super::bytes_at(d, at + 4)?;
        let end = at
            .checked_add(4)
            .and_then(|a| a.checked_add(length))
            .ok_or_else(damaged)?;
        ensure!(end <= d.len() && length >= 16, damaged());
        if kind == END_OF_ELEMENTS {
            return Ok((out, end));
        }
        ensure!(length >= 21 && out.len() < MAX_ELEMENTS, damaged());
        let id = super::i32_le(d, at + 21)?;
        let body = super::range(d, at + 25, end)?;
        out.push(Element { kind, id, body });
        at = end;
    }
}

impl Graph {
    fn read(d: &[u8]) -> Result<Graph> {
        let (nodes, at) = elements(d, 0)?;
        let (atoms, at) = elements(d, at)?;
        let mut g = Graph::default();
        for e in nodes {
            if g.root.is_none() {
                g.root = Some(e.id);
            }
            if e.kind == TRANSFORM_ATTRIBUTE {
                g.transforms.insert(e.id, transform(e.body)?);
            } else if let Some(node) = node(&e)? {
                g.nodes.insert(e.id, node);
            }
        }
        for e in atoms {
            // base property atom data: u8 version, u32 state flags
            let mut r = Reader::at(e.body, 5);
            let atom = if e.kind == STRING_ATOM {
                r.u8()?;
                let n = r.count(2)?;
                let units: Vec<u16> = r
                    .take(n * 2)?
                    .chunks_exact(2)
                    .filter_map(|c| <[u8; 2]>::try_from(c).ok())
                    .map(u16::from_le_bytes)
                    .collect();
                Atom::Text(String::from_utf16_lossy(&units))
            } else if e.kind == INTEGER_ATOM {
                r.u8()?;
                Atom::Integer(r.i32()?)
            } else if e.kind == LATE_LOADED_ATOM {
                r.u8()?;
                Atom::LateLoaded(r.array()?)
            } else {
                continue;
            };
            g.atoms.insert(e.id, atom);
        }
        // property table: i16 version, i32 count, per node its key/value pairs
        let mut r = Reader::at(d, at);
        r.i16()?;
        let count = r.count(8)?;
        for _ in 0..count {
            let node = r.i32()?;
            let mut pairs = Vec::new();
            loop {
                let key = r.i32()?;
                if key == 0 {
                    break;
                }
                pairs.push((key, r.i32()?));
            }
            g.properties.insert(node, pairs);
        }
        Ok(g)
    }

    fn property(&self, node: i32, name: &str) -> Option<&Atom> {
        self.properties.get(&node)?.iter().find_map(|(k, v)| {
            matches!(self.atoms.get(k), Some(Atom::Text(t)) if t == name)
                .then(|| self.atoms.get(v))
                .flatten()
        })
    }

    /// Every visible tri-strip shape with its world matrix, and the factor
    /// to millimetres (from the outermost part's units).
    fn occurrences(&self) -> Result<(Vec<Occurrence>, f64)> {
        let root = self.root.ok_or_else(damaged)?;
        let mut out = Vec::new();
        let mut scale = None;
        let mut visits = 0usize;
        self.walk(root, IDENTITY, 0, &mut visits, &mut scale, &mut out)?;
        Ok((out, scale.unwrap_or(1.0)))
    }

    fn walk(
        &self,
        id: i32,
        parent: [[f64; 4]; 4],
        depth: usize,
        visits: &mut usize,
        scale: &mut Option<f64>,
        out: &mut Vec<Occurrence>,
    ) -> Result<()> {
        ensure!(depth <= MAX_GRAPH_DEPTH, damaged());
        *visits += 1;
        ensure!(*visits <= MAX_VISITS, damaged());
        let Some(node) = self.nodes.get(&id) else {
            return Ok(());
        };
        let hidden = matches!(
            self.property(id, "__PLM_PS_Hidden"),
            Some(Atom::Integer(v)) if *v != 0
        );
        if node.flags & 1 != 0 || hidden {
            return Ok(());
        }
        let mut matrix = parent;
        for a in &node.attributes {
            if let Some(t) = self.transforms.get(a) {
                matrix = multiply(t, &matrix);
            }
        }
        if node.part && scale.is_none() {
            if let Some(Atom::Text(units)) = self.property(id, "JT_PROP_MEASUREMENT_UNITS") {
                *scale = Some(to_mm(units));
            }
        }
        let children: &[i32] = match node.kind {
            Kind::Group | Kind::Instance => &node.children,
            Kind::Lod => node.children.get(..1).unwrap_or_default(),
            Kind::Switch(selected) => usize::try_from(selected)
                .ok()
                .and_then(|i| node.children.get(i..=i))
                .unwrap_or_default(),
            Kind::TriStripShape => {
                if let Some(Atom::LateLoaded(segment)) = self.property(id, "JT_LLPROP_SHAPEIMPL") {
                    ensure!(
                        out.len() < MAX_OCCURRENCES,
                        "The JT file has more than {MAX_OCCURRENCES} shapes."
                    );
                    out.push(Occurrence {
                        segment: *segment,
                        matrix,
                    });
                }
                &[]
            }
            Kind::Other => &[],
        };
        for &c in children {
            self.walk(c, matrix, depth + 1, visits, scale, out)?;
        }
        Ok(())
    }
}

/// Millimetres per unit of `JT_PROP_MEASUREMENT_UNITS` (unknown: mm).
fn to_mm(units: &str) -> f64 {
    match units.trim().to_ascii_lowercase().as_str() {
        "micrometers" | "microns" => 0.001,
        "centimeters" => 10.0,
        "decimeters" => 100.0,
        "meters" => 1000.0,
        "kilometers" => 1_000_000.0,
        "inches" => 25.4,
        "feet" => 304.8,
        "yards" => 914.4,
        "miles" => 1_609_344.0,
        "mils" => 0.0254,
        _ => 1.0,
    }
}

/// A scene graph node (`None` for element types that are not nodes).
fn node(e: &Element<'_>) -> Result<Option<Node>> {
    let kind = match e.kind {
        k if k == PARTITION_NODE || k == GROUP_NODE || k == PART_NODE || k == META_DATA_NODE => {
            Kind::Group
        }
        k if k == LOD_NODE || k == RANGE_LOD_NODE => Kind::Lod,
        k if k == SWITCH_NODE => Kind::Switch(0),
        k if k == INSTANCE_NODE => Kind::Instance,
        k if k == TRI_STRIP_SHAPE_NODE => Kind::TriStripShape,
        _ => Kind::Other,
    };
    if kind == Kind::Other {
        return Ok(None);
    }
    // base node data: u8 version, u32 flags, i32 attribute count, ids
    let mut r = Reader::at(e.body, 0);
    r.u8()?;
    let flags = r.u32()?;
    let n = r.count(4)?;
    let attributes = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
    let mut node = Node {
        kind,
        part: e.kind == PART_NODE,
        flags,
        attributes,
        children: Vec::new(),
    };
    match kind {
        Kind::Instance => {
            r.u8()?;
            node.children.push(r.i32()?);
        }
        Kind::Group | Kind::Lod | Kind::Switch(_) => {
            // group node data: u8 version, i32 child count, ids
            r.u8()?;
            let n = r.count(4)?;
            node.children = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
            if let Kind::Switch(_) = kind {
                r.u8()?;
                node.kind = Kind::Switch(r.i32()?);
            }
        }
        Kind::TriStripShape | Kind::Other => {}
    }
    Ok(Some(node))
}

/// Geometric transform attribute: base attribute data (u8 version, u8
/// state, u32 inhibit, u32 final), u8 version, u16 mask of the stored
/// elements (bit 15 = element 0), the stored elements as f64.
fn transform(body: &[u8]) -> Result<[[f64; 4]; 4]> {
    let mask = super::u16_le(body, 11)?;
    let mut m = IDENTITY;
    let mut at = 13;
    for i in 0..16 {
        if mask & (1 << (15 - i)) != 0 {
            let v = super::f64_le(body, at)?;
            ensure!(v.is_finite(), damaged());
            if let Some(cell) = m.get_mut(i / 4).and_then(|row| row.get_mut(i % 4)) {
                *cell = v;
            }
            at += 8;
        }
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// shape LOD
// ---------------------------------------------------------------------------

/// The tri-strip set of a shape LOD segment (`None` for other shapes).
fn read_shape(segment: &[u8]) -> Result<Option<Lod>> {
    let length = usize::try_from(super::i32_le(segment, 0)?).map_err(|_| damaged())?;
    let kind: [u8; 16] = super::bytes_at(segment, 4)?;
    if kind != TRI_STRIP_SET_LOD {
        return Ok(None);
    }
    let end = 4usize.checked_add(length).ok_or_else(damaged)?;
    let element = super::range(segment, 0, end)?;
    // after the element header (length, type, u8 base type, i32 object id)
    let mut r = Reader::at(element, 25);
    r.u8()?; // base shape LOD version
    r.u8()?; // vertex shape LOD version
    r.take(8)?; // vertex bindings
                // NX: length of the rest and a 16-byte id, not in the specification
    let nx_block = super::u32_le(element, r.at)
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| r.at.checked_add(4)?.checked_add(n))
        .is_some_and(|e| e == end || e + 1 == end);
    if nx_block {
        r.take(20)?;
    }
    r.u8()?; // topo mesh version
    r.u32()?; // vertex records object id
    r.u8()?; // topologically compressed version
    if nx_block {
        r.take(5)?;
    }
    read_topology(&mut r).map(Some)
}

/// Topologically compressed rep data and vertex records.
fn read_topology(r: &mut Reader<'_>) -> Result<Lod> {
    let mut packet = || read_packet(r, 0);
    let mut s = Symbols::default();
    for d in &mut s.degrees {
        *d = packet()?;
    }
    s.valences = packet()?;
    s.groups = packet()?;
    let flags = lag1(packet()?);
    for m in &mut s.attr_masks {
        *m = packet()?;
    }
    s.attr_masks_32 = packet()?;
    let n = r.count(4)?;
    s.attr_masks_large = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
    s.split_faces = lag1(read_packet(r, 0)?);
    s.split_positions = read_packet(r, 0)?;
    let stored = r.u32()?;
    ensure!(
        composite_hash(&s, &flags) == stored,
        "The JT file is damaged (the shape's topology does not match its hash)."
    );

    let bindings = u64::from_le_bytes(r.array()?);
    r.take(4)?; // quantisation parameters
    let vertices = r.i32()?;
    if vertices == 0 {
        return Ok(Lod::default());
    }
    r.i32()?; // vertex attribute records
    ensure!(bindings & 7 == 2, "The JT shape has no 3D coordinates.");
    let points = read_coordinates(r)?;
    let normals = if bindings & 8 != 0 {
        read_normals(r)?
    } else {
        Vec::new()
    };
    build_lod(s, points, normals)
}

fn read_coordinates(r: &mut Reader<'_>) -> Result<Vec<[f64; 3]>> {
    let count = r.count(1)?;
    ensure!(r.u8()? == 3, "The JT shape has no 3D coordinates.");
    let mut quantizers = [(0f32, 0f32, 0u8); 3];
    for q in &mut quantizers {
        *q = (r.f32()?, r.f32()?, r.u8()?);
    }
    let mut axes: Vec<Vec<f64>> = Vec::with_capacity(3);
    for (lo, hi, bits) in quantizers {
        let codes = lag1(read_packet(r, 0)?);
        ensure!(codes.len() == count && bits <= 32, damaged());
        axes.push(if bits == 0 {
            codes
                .iter()
                .map(|&c| f64::from(f32::from_bits(c as u32)))
                .collect()
        } else {
            let levels = if bits == 32 {
                f64::from(u32::MAX)
            } else {
                f64::from((1u32 << bits) - 1)
            };
            let step = (f64::from(hi) - f64::from(lo)) / levels;
            codes
                .iter()
                .map(|&c| f64::from(lo) + f64::from(c as u32) * step)
                .collect()
        });
    }
    r.u32()?; // hash
    let [x, y, z] = <[Vec<f64>; 3]>::try_from(axes).map_err(|_| damaged())?;
    let points: Vec<[f64; 3]> = x
        .into_iter()
        .zip(y)
        .zip(z)
        .map(|((x, y), z)| [x, y, z])
        .collect();
    ensure!(points.iter().flatten().all(|c| c.is_finite()), damaged());
    Ok(points)
}

/// Normals: one packet of Deering codes (octant, sextant, θ, ψ from the
/// top bit down) or three packets of raw `f32` bits.
fn read_normals(r: &mut Reader<'_>) -> Result<Vec<[f64; 3]>> {
    let count = r.count(1)?;
    let components = r.u8()?;
    let bits = r.u8()?;
    let normals = if bits != 0 {
        ensure!(bits <= 13, damaged());
        let codes = read_packet(r, 0)?;
        ensure!(codes.len() == count, damaged());
        let table = DeeringTable::new();
        let n = u32::from(bits);
        let field = (1u32 << n) - 1;
        let shift = 13 - n;
        codes
            .iter()
            .map(|&c| {
                let c = c as u32;
                let psi = (c & field) as i32;
                let theta = ((c >> n) & field) as i32;
                let sextant = ((c >> (2 * n)) & 7) as i32;
                let octant = ((c >> (2 * n + 3)) & 7) as i32;
                table.normal(sextant, octant, theta, psi, shift)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        ensure!(components == 3, damaged());
        let mut axis = || -> Result<Vec<f64>> {
            let v = read_packet(r, 0)?;
            ensure!(v.len() == count, damaged());
            Ok(v.iter()
                .map(|&c| f64::from(f32::from_bits(c as u32)))
                .collect())
        };
        let (x, y, z) = (axis()?, axis()?, axis()?);
        x.into_iter()
            .zip(y)
            .zip(z)
            .map(|((x, y), z)| [x, y, z])
            .collect()
    };
    r.u32()?; // hash
    Ok(normals)
}

// ---------------------------------------------------------------------------
// Int32 compressed data packet, third generation
// ---------------------------------------------------------------------------

/// Reads and decodes one packet: i32 count, u8 codec, codec data.
fn read_packet(r: &mut Reader<'_>, depth: u32) -> Result<Vec<i32>> {
    ensure!(depth <= MAX_DEPTH + 2, damaged());
    let count = usize::try_from(r.i32()?).map_err(|_| damaged())?;
    ensure!(count <= MAX_VALUES, damaged());
    if count == 0 {
        return Ok(Vec::new());
    }
    let codec = r.u8()?;
    match codec {
        4 => {
            let chop = r.u8()?;
            if chop == 0 {
                return read_packet(r, depth + 1);
            }
            let bias = r.i32()?;
            let span = r.u8()?;
            ensure!(chop <= span && span <= 32, damaged());
            let high = read_packet(r, depth + 1)?;
            let low = read_packet(r, depth + 1)?;
            ensure!(high.len() == count && low.len() == count, damaged());
            let shift = u32::from(span - chop);
            Ok(high
                .iter()
                .zip(&low)
                .map(|(&h, &l)| {
                    let h = if shift >= 32 { 0 } else { (h as u32) << shift };
                    ((h | l as u32) as i32).wrapping_add(bias)
                })
                .collect())
        }
        5 => {
            let values = read_packet(r, depth + 1)?;
            let offsets = read_packet(r, depth + 1)?;
            ensure!(offsets.len() == count, damaged());
            move_to_front(&values, &offsets)
        }
        0 | 1 | 3 => {
            let bit_length = usize::try_from(r.i32()?).map_err(|_| damaged())?;
            let raw = r.take(bit_length.div_ceil(32).checked_mul(4).ok_or_else(damaged)?)?;
            match codec {
                0 => {
                    ensure!(raw.len() / 4 >= count, damaged());
                    Ok(super::quads(raw)
                        .take(count)
                        .map(i32::from_le_bytes)
                        .collect())
                }
                1 => bitlength(&mut Bits::words(raw), count),
                _ => {
                    let context = read_context(r)?;
                    let escapes = context.iter().any(|e| e.symbol == -2);
                    let oob = if escapes {
                        read_packet(r, depth + 1)?
                    } else {
                        Vec::new()
                    };
                    if bit_length == 0 {
                        // nothing worth coding: every value is out of band
                        ensure!(oob.len() >= count, damaged());
                        return Ok(oob.get(..count).unwrap_or_default().to_vec());
                    }
                    arithmetic(&mut Bits::words(raw), count, &context, &oob)
                }
            }
        }
        other => bail!("The JT data uses the unknown codec {other}."),
    }
}

/// Probability context: u16 entries, u6 count bits, u7 value bits, s32
/// minimum, per entry u1 escape, the count and the value − minimum; a bit
/// stream on the bytes, ending byte aligned.
fn read_context(r: &mut Reader<'_>) -> Result<Vec<Entry>> {
    let rest = r.data.get(r.at..).unwrap_or_default();
    let mut head = Bits::new(rest.get(..rest.len().min(8)).unwrap_or_default().to_vec());
    let entries = head.u(16)? as usize;
    let count_bits = head.u(6)?;
    let value_bits = head.u(7)?;
    ensure!(value_bits <= 32, damaged());
    let total_bits = 61 + entries * (1 + count_bits + value_bits) as usize;
    let bytes = total_bits.div_ceil(8);
    ensure!(bytes <= rest.len(), damaged());
    let mut bits = Bits::new(rest.get(..bytes).unwrap_or_default().to_vec());
    bits.u(29)?;
    let min_value = bits.s(32)?;
    let mut context = Vec::with_capacity(entries);
    for _ in 0..entries {
        let escape = bits.u(1)? == 1;
        let occurrences = u64::from(bits.u(count_bits)?);
        let value = (bits.u(value_bits)? as i32).wrapping_add(min_value);
        context.push(Entry {
            symbol: if escape { -2 } else { 0 },
            occurrences,
            value,
        });
    }
    r.at += bytes;
    Ok(context)
}

/// "Nibbler" integer: 4-bit groups, least significant first, each followed
/// by a continue bit; sign-extended.
fn nibbler(bits: &mut Bits) -> Result<i32> {
    let mut v: u32 = 0;
    for n in 0..8 {
        v |= bits.u(4)? << (4 * n);
        if bits.u(1)? == 0 {
            let width = 4 * (n + 1);
            return Ok(if width < 32 && v & (1 << (width - 1)) != 0 {
                (v | !((1u32 << width) - 1)) as i32
            } else {
                v as i32
            });
        }
    }
    Err(damaged())
}

/// Bitlength codec: fixed width between nibbler-coded limits, or runs of a
/// varying width around a nibbler-coded mean (width changes and run
/// lengths in 4-bit fields).
fn bitlength(bits: &mut Bits, count: usize) -> Result<Vec<i32>> {
    let mut out = Vec::with_capacity(count);
    if bits.u(1)? == 0 {
        let min = nibbler(bits)?;
        let max = nibbler(bits)?;
        let range = max.wrapping_sub(min) as u32;
        let width = 32 - range.leading_zeros();
        for _ in 0..count {
            out.push((bits.u(width)? as i32).wrapping_add(min));
        }
        return Ok(out);
    }
    const FIELD: u32 = 4;
    let max_decrement = -(1i32 << (FIELD - 1));
    let max_increment = (1i32 << (FIELD - 1)) - 1;
    let mean = nibbler(bits)?;
    let mut width: i32 = 0;
    while out.len() < count {
        loop {
            let change = bits.s(FIELD)?;
            width = width.checked_add(change).ok_or_else(damaged)?;
            if change != max_decrement && change != max_increment {
                break;
            }
        }
        ensure!((0..=32).contains(&width), damaged());
        let run = (bits.u(FIELD)? as usize).min(count - out.len());
        for _ in 0..run {
            out.push(bits.s(width as u32)?.wrapping_add(mean));
        }
    }
    Ok(out)
}

/// Move-to-front: an offset of −1 takes the next new value, any other the
/// value at that place of the window; the value moves to the front.
fn move_to_front(values: &[i32], offsets: &[i32]) -> Result<Vec<i32>> {
    let mut window: Vec<i32> = Vec::with_capacity(MTF_WINDOW + 1);
    let mut new = values.iter();
    let mut out = Vec::with_capacity(offsets.len());
    for &o in offsets {
        let v = match usize::try_from(o) {
            Ok(i) if i < window.len() => window.remove(i),
            _ => *new.next().ok_or_else(damaged)?,
        };
        window.insert(0, v);
        window.truncate(MTF_WINDOW);
        out.push(v);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// hash (Bob Jenkins' lookup2)
// ---------------------------------------------------------------------------

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*b).wrapping_sub(*c) ^ (*c >> 13);
    *b = b.wrapping_sub(*c).wrapping_sub(*a) ^ (*a << 8);
    *c = c.wrapping_sub(*a).wrapping_sub(*b) ^ (*b >> 13);
    *a = a.wrapping_sub(*b).wrapping_sub(*c) ^ (*c >> 12);
    *b = b.wrapping_sub(*c).wrapping_sub(*a) ^ (*a << 16);
    *c = c.wrapping_sub(*a).wrapping_sub(*b) ^ (*b >> 5);
    *a = a.wrapping_sub(*b).wrapping_sub(*c) ^ (*c >> 3);
    *b = b.wrapping_sub(*c).wrapping_sub(*a) ^ (*a << 10);
    *c = c.wrapping_sub(*a).wrapping_sub(*b) ^ (*b >> 15);
}

/// `hash2` of lookup2 over 32-bit words.
fn hash32(words: &[i32], seed: u32) -> u32 {
    let (mut a, mut b, mut c) = (0x9e37_79b9u32, 0x9e37_79b9u32, seed);
    let mut chunks = words.chunks_exact(3);
    for k in &mut chunks {
        if let &[x, y, z] = k {
            a = a.wrapping_add(x as u32);
            b = b.wrapping_add(y as u32);
            c = c.wrapping_add(z as u32);
        }
        mix(&mut a, &mut b, &mut c);
    }
    c = c.wrapping_add(words.len() as u32);
    let rest = chunks.remainder();
    if let Some(&w) = rest.get(1) {
        b = b.wrapping_add(w as u32);
    }
    if let Some(&w) = rest.first() {
        a = a.wrapping_add(w as u32);
    }
    mix(&mut a, &mut b, &mut c);
    c
}

/// `hash3` of lookup2 over 16-bit values.
fn hash16(values: &[i32], seed: u32) -> u32 {
    let k: Vec<u32> = values.iter().map(|&v| v as u32 & 0xFFFF).collect();
    let (mut a, mut b, mut c) = (0x9e37_79b9u32, 0x9e37_79b9u32, seed);
    let mut chunks = k.chunks_exact(6);
    for w in &mut chunks {
        if let &[a0, a1, b0, b1, c0, c1] = w {
            a = a.wrapping_add(a0 + (a1 << 16));
            b = b.wrapping_add(b0 + (b1 << 16));
            c = c.wrapping_add(c0 + (c1 << 16));
        }
        mix(&mut a, &mut b, &mut c);
    }
    c = c.wrapping_add(k.len() as u32);
    let rest = chunks.remainder();
    let at = |i: usize| rest.get(i).copied().unwrap_or(0);
    c = c.wrapping_add(at(4) << 16);
    b = b.wrapping_add(at(3) << 16).wrapping_add(at(2));
    a = a.wrapping_add(at(1) << 16).wrapping_add(at(0));
    mix(&mut a, &mut b, &mut c);
    c
}

/// The composite hash of the topology arrays, in file order.
fn composite_hash(s: &Symbols, flags: &[i32]) -> u32 {
    let mut h = 0;
    for d in &s.degrees {
        h = hash32(d, h);
    }
    h = hash32(&s.valences, h);
    h = hash32(&s.groups, h);
    h = hash16(flags, h);
    for m in &s.attr_masks {
        h = hash32(m, h);
    }
    h = hash32(&s.attr_masks_32, h);
    h = hash32(&s.attr_masks_large, h);
    h = hash32(&s.split_faces, h);
    hash32(&s.split_positions, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs (value, bits) pairs MSB first.
    fn pack(fields: &[(u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc: u64 = 0;
        let mut n = 0;
        for &(v, bits) in fields {
            acc = (acc << bits) | u64::from(v & ((1u64 << bits) - 1) as u32);
            n += bits;
            while n >= 8 {
                out.push((acc >> (n - 8)) as u8);
                n -= 8;
                acc &= (1 << n) - 1;
            }
        }
        if n > 0 {
            out.push((acc << (8 - n)) as u8);
        }
        out
    }

    #[test]
    fn nibbler_is_sign_extended() {
        // 5 = 0101, stop
        let mut b = Bits::new(pack(&[(5, 4), (0, 1)]));
        assert_eq!(nibbler(&mut b).unwrap(), 5);
        // -1 in one nibble
        let mut b = Bits::new(pack(&[(0xF, 4), (0, 1)]));
        assert_eq!(nibbler(&mut b).unwrap(), -1);
        // 0x23 = nibbles 3, 2
        let mut b = Bits::new(pack(&[(3, 4), (1, 1), (2, 4), (0, 1)]));
        assert_eq!(nibbler(&mut b).unwrap(), 0x23);
        // never ends: damaged
        let mut b = Bits::new(vec![0xFF; 8]);
        assert!(nibbler(&mut b).is_err());
    }

    #[test]
    fn fixed_and_variable_bitlength() {
        // fixed: min 1, max 3 → 2-bit fields
        let mut b = Bits::new(pack(&[
            (0, 1),
            (1, 4),
            (0, 1),
            (3, 4),
            (0, 1),
            (1, 2),
            (2, 2),
            (0, 2),
        ]));
        assert_eq!(bitlength(&mut b, 3).unwrap(), vec![2, 3, 1]);
        // variable: mean 0, width +3, run 2 of (−4, 3), width −3, run 1 of 0
        let mut b = Bits::new(pack(&[
            (1, 1),
            (0, 4),
            (0, 1),
            (3, 4),
            (2, 4),
            (0b100, 3),
            (0b011, 3),
            (0xD, 4),
            (1, 4),
        ]));
        assert_eq!(bitlength(&mut b, 3).unwrap(), vec![-4, 3, 0]);
    }

    #[test]
    fn move_to_front_reuses_the_window() {
        assert_eq!(
            move_to_front(&[7, 9], &[-1, 0, -1, 1, 0]).unwrap(),
            vec![7, 7, 9, 7, 7]
        );
        assert!(move_to_front(&[7], &[-1, -1]).is_err(), "values run out");
    }

    #[test]
    fn lookup2_hashes() {
        // hash32 and hash16 agree on the same small values
        assert_ne!(hash32(&[1, 2, 3, 4], 0), hash32(&[1, 2, 3, 5], 0));
        assert_ne!(hash32(&[], 0), hash32(&[], 1));
        assert_eq!(hash16(&[0; 7], 0), hash16(&[0x10000; 7], 0), "16 bits only");
    }

    #[test]
    fn units_to_millimetres() {
        assert_eq!(to_mm("Millimeters"), 1.0);
        assert_eq!(to_mm("Meters"), 1000.0);
        assert_eq!(to_mm("Inches"), 25.4);
        assert_eq!(to_mm("whatever"), 1.0);
    }

    #[test]
    fn sparse_transform_and_mirror() {
        let mut body = vec![0u8; 11];
        // mask: element 3 (row 0, col 3) and element 12 (translation x)
        body.extend(((1u16 << 15) | (1 << 3)).to_le_bytes());
        body.extend((-1.0f64).to_le_bytes());
        body.extend(5.0f64.to_le_bytes());
        let m = transform(&body).unwrap();
        assert_eq!(m[0][0], -1.0);
        assert_eq!(m[3][0], 5.0);
        assert_eq!(m[1][1], 1.0);
        assert!(determinant(&m) < 0.0);
    }

    #[test]
    fn other_versions_and_garbage_are_errors() {
        assert!(read(b"not a JT file").is_err());
        let mut eight = b"Version 8.1 JT".to_vec();
        eight.resize(200, b' ');
        let err = read(&eight).unwrap_err().to_string();
        assert!(err.contains("reads JT 9 and 10"), "{err}");
        // JT 9 goes to the NX reader, which rejects this one as damaged
        let mut nine = b"Version 9.5 JT".to_vec();
        nine.resize(200, 0);
        assert!(read(&nine).is_err());
        let mut ten = b"Version 10.5 JT".to_vec();
        ten.resize(200, 0);
        assert!(read(&ten).is_err());
    }
}
