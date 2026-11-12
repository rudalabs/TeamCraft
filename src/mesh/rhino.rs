//! Rhino models (`.3dm`, versions 1 to 8).
//!
//! The format is the openNURBS archive (documented by McNeel's openNURBS
//! sources, which were read but not copied): a 32-byte header, then nested
//! chunks – a `u32` type code and a length (32 bit, 64 bit from version 50).
//! Short chunks (type bit `0x8000_0000`) carry their value in the length;
//! chunks with type bit `0x8000` end in a CRC-32. The object table holds one
//! record per object: the class UUID, its data and its attributes.
//!
//! What is drawn is what Rhino stored for display:
//!
//! * mesh objects;
//! * the render meshes of polysurfaces (one per B-rep face, saved after the
//!   exact geometry unless the file was saved small);
//! * the mesh cache of extrusions (Rhino 6 and later);
//! * SubD objects, which never store a mesh: their control net is refined
//!   by Catmull-Clark subdivision (creases and corners kept) and pushed to
//!   the limit surface;
//! * block instances: the objects of the block definition, transformed
//!   (nested blocks up to [`MAX_DEPTH`]);
//! * the quantised meshes of Rhino 1 files.
//!
//! Hidden objects, objects on hidden layers and the objects of block
//! definitions (drawn only through their instances) are left out.
//! Coordinates are converted from the model unit to millimetres.
//!
//! Everything is bounded: chunk lengths must fit their parent, counts are
//! checked against the bytes left before anything is allocated, the
//! triangles of the whole scene (blocks expanded) are counted against
//! [`MAX_TRIANGLES`] before they are copied.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use anyhow::{anyhow, bail, Result};

use super::{check_triangles, Mesh, MAX_TRIANGLES};

/// The start of the 32-byte header of every `.3dm` file.
pub const MAGIC: &[u8] = b"3D Geometry File Format ";
/// openNURBS skips up to 32 MiB of foreign data (OLE wrappers) before the header.
const MAX_HEADER_OFFSET: usize = 32 * 1024 * 1024;
/// Nesting of block instances.
pub const MAX_DEPTH: usize = 8;
/// Quads one SubD may be subdivided into.
const MAX_SUBD_FACES: usize = 200_000;
/// Subdivision steps of a SubD (each one quadruples the faces).
const MAX_SUBD_LEVELS: usize = 4;

const TCODE_SHORT: u32 = 0x8000_0000;
const TCODE_CRC: u32 = 0x8000;
const TCODE_ENDOFFILE: u32 = 0x0000_7FFF;
const TCODE_ENDOFTABLE: u32 = 0xFFFF_FFFF;
const TCODE_PROPERTIES_TABLE: u32 = 0x1000_0014;
const TCODE_SETTINGS_TABLE: u32 = 0x1000_0015;
const TCODE_LAYER_TABLE: u32 = 0x1000_0011;
const TCODE_OBJECT_TABLE: u32 = 0x1000_0013;
const TCODE_IDEF_TABLE: u32 = 0x1000_0021;
const TCODE_OPENNURBS_VERSION: u32 = 0xA000_0026;
const TCODE_COMPRESSED_PREVIEW: u32 = 0x2000_8025;
const TCODE_UNITS: u32 = 0x2000_8031;
const TCODE_LAYER_RECORD: u32 = 0x2000_8050;
const TCODE_IDEF_RECORD: u32 = 0x2000_8076;
const TCODE_OBJECT_RECORD: u32 = 0x2000_8070;
const TCODE_OBJECT_TYPE: u32 = 0x8200_0071;
const TCODE_OBJECT_ATTRIBUTES: u32 = 0x0200_8072;
const TCODE_OBJECT_END: u32 = 0x8200_007F;
const TCODE_CLASS: u32 = 0x0002_7FFA;
const TCODE_CLASS_UUID: u32 = 0x0002_FFFB;
const TCODE_CLASS_DATA: u32 = 0x0002_FFFC;
const TCODE_CLASS_END: u32 = 0x8002_7FFF;
const TCODE_ANONYMOUS: u32 = 0x4000_8000;
const TCODE_V1_MESH: u32 = 0x0010_0015;
const TCODE_V1_MESH_GEOMETRY: u32 = 0x0010_0017;

/// A class UUID as it is stored: `u32`, `u16`, `u16` little endian, 8 bytes.
const fn uuid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const MESH: [u8; 16] = uuid(
    0x4ED7_D4E4,
    0xE947,
    0x11d3,
    [0xBF, 0xE5, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0],
);
/// ON_Brep and the legacy classes openNURBS reads as one: old trimmed
/// surface, old B-rep and `TL_Brep` (every Rhino 4 polysurface).
const BREPS: [[u8; 16]; 4] = [
    uuid(
        0x60B5_DBC5,
        0xE660,
        0x11d3,
        [0xBF, 0xE4, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0],
    ),
    uuid(
        0x0705_FDEF,
        0x3E2A,
        0x11d4,
        [0x80, 0x0E, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0],
    ),
    uuid(
        0x2D4C_FEDB,
        0x3E2A,
        0x11d4,
        [0x80, 0x0E, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0],
    ),
    uuid(
        0xF06F_C243,
        0xA32A,
        0x4608,
        [0x9D, 0xD8, 0xA7, 0xD2, 0xC4, 0xCE, 0x2A, 0x36],
    ),
];
const EXTRUSION: [u8; 16] = uuid(
    0x36F5_3175,
    0x72B8,
    0x4d47,
    [0xBF, 0x1F, 0xB4, 0xE6, 0xFC, 0x24, 0xF4, 0xB9],
);
const SUBD: [u8; 16] = uuid(
    0xF09B_A4D9,
    0x455B,
    0x42C3,
    [0xBA, 0x3B, 0xE6, 0xCC, 0xAC, 0xEF, 0x85, 0x3B],
);
const INSTANCE_REF: [u8; 16] = uuid(
    0xF9CF_B638,
    0xB9D4,
    0x4340,
    [0x87, 0xE3, 0xC5, 0x6E, 0x78, 0x65, 0xD9, 0x6A],
);

/// Millimetres per model unit (ON::LengthUnitSystem); no unit counts as mm.
fn millimetres_per_unit(unit: u32) -> f64 {
    match unit {
        1 => 1e-3,
        3 => 10.0,
        4 => 1000.0,
        5 => 1e6,
        6 => 2.54e-5,
        7 => 0.0254,
        8 => 25.4,
        9 => 304.8,
        10 => 1_609_344.0,
        12 => 1e-7,
        13 => 1e-6,
        14 => 100.0,
        15 => 1e4,
        16 => 1e5,
        19 => 914.4,
        20 => 1_852_000.0,
        _ => 1.0,
    }
}

// ---------------------------------------------------------------------------
// archive cursor
// ---------------------------------------------------------------------------

fn short_file(at: usize) -> anyhow::Error {
    anyhow!("The Rhino file is damaged or ends early (byte {at}).")
}

/// A bounded read position inside one chunk.
#[derive(Clone, Copy)]
struct Cur<'a> {
    data: &'a [u8],
    at: usize,
    end: usize,
    /// 64-bit chunk lengths (version 50 and later).
    big: bool,
    /// Version 1: no 32-bit CRCs.
    v1: bool,
}

struct Chunk<'a> {
    tcode: u32,
    value: i64,
    body: Option<Cur<'a>>,
}

impl<'a> Cur<'a> {
    fn left(&self) -> usize {
        self.end.saturating_sub(self.at)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.end)
            .ok_or_else(|| short_file(self.at))?;
        let s = self
            .data
            .get(self.at..end)
            .ok_or_else(|| short_file(self.at))?;
        self.at = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let at = self.at;
        <[u8; N]>::try_from(self.bytes(N)?).map_err(|_| short_file(at))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    /// Skips `count` items of `size` bytes (counts from the file are checked).
    fn skip_items(&mut self, count: usize, size: usize) -> Result<()> {
        let n = count.checked_mul(size).ok_or_else(|| short_file(self.at))?;
        self.skip(n)
    }

    fn u8(&mut self) -> Result<u8> {
        self.array::<1>().map(|[b]| b)
    }
    fn u16(&mut self) -> Result<u16> {
        self.array().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }
    fn i32(&mut self) -> Result<i32> {
        self.array().map(i32::from_le_bytes)
    }
    fn f64(&mut self) -> Result<f64> {
        self.array().map(f64::from_le_bytes)
    }
    fn f64s<const N: usize>(&mut self) -> Result<[f64; N]> {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.f64()?;
        }
        Ok(out)
    }
    fn uuid(&mut self) -> Result<[u8; 16]> {
        self.array()
    }
    /// A count that must fit into the bytes left (`size` bytes per item).
    fn count(&mut self, size: usize) -> Result<usize> {
        let at = self.at;
        let n = self.u32()? as usize;
        if n.saturating_mul(size.max(1)) > self.left() {
            bail!("The Rhino file is damaged: a count at byte {at} exceeds the data.");
        }
        Ok(n)
    }
    /// `Read3dmChunkVersion`: major and minor version in one byte.
    fn version(&mut self) -> Result<(u8, u8)> {
        let v = self.u8()?;
        Ok((v >> 4, v & 0x0F))
    }
    /// The two `i32` that start a versioned chunk (`BeginRead3dmChunk`).
    fn version2(&mut self) -> Result<(i32, i32)> {
        Ok((self.i32()?, self.i32()?))
    }
    /// Skips an `ON_wString` (length with terminator, UTF-16).
    fn skip_string(&mut self) -> Result<()> {
        let n = self.count(2)?;
        self.skip_items(n, 2)
    }

    fn chunk(&mut self) -> Result<Chunk<'a>> {
        let tcode = self.u32()?;
        let short = tcode & TCODE_SHORT != 0;
        let value = if self.big {
            i64::from_le_bytes(self.array()?)
        } else if short
            && tcode != TCODE_OPENNURBS_VERSION
            && tcode != TCODE_OBJECT_TYPE
            && tcode != TCODE_ENDOFTABLE
        {
            i64::from(self.i32()?)
        } else {
            i64::from(self.u32()?)
        };
        if short {
            return Ok(Chunk {
                tcode,
                value,
                body: None,
            });
        }
        let len = usize::try_from(value)
            .ok()
            .filter(|&n| n <= self.left())
            .ok_or_else(|| {
                anyhow!(
                    "The Rhino file is damaged: a chunk at byte {} is too long.",
                    self.at
                )
            })?;
        let crc = if tcode & TCODE_CRC != 0 && !self.v1 {
            4
        } else {
            0
        };
        if len < crc {
            bail!(
                "The Rhino file is damaged: a chunk at byte {} is too short.",
                self.at
            );
        }
        let body = Cur {
            at: self.at,
            end: self.at + len - crc,
            ..*self
        };
        self.at += len;
        Ok(Chunk {
            tcode,
            value,
            body: Some(body),
        })
    }

    /// The next chunk, `None` at the end of the data or after an end marker.
    fn next(&mut self) -> Result<Option<Chunk<'a>>> {
        if self.left() == 0 {
            return Ok(None);
        }
        let c = self.chunk()?;
        Ok(Some(c))
    }

    /// The body of the next chunk, which must be a long chunk.
    fn body(&mut self) -> Result<Cur<'a>> {
        let at = self.at;
        self.chunk()?.body.ok_or_else(|| {
            anyhow!("The Rhino file is damaged: unexpected short chunk at byte {at}.")
        })
    }
}

/// A class-tagged object (`ReadObject`): its class UUID and its data.
fn read_object<'a>(cur: &mut Cur<'a>) -> Result<([u8; 16], Option<Cur<'a>>)> {
    let at = cur.at;
    let c = cur.chunk()?;
    let (TCODE_CLASS, Some(mut class)) = (c.tcode, c.body) else {
        bail!("The Rhino file is damaged: no object at byte {at}.");
    };
    let mut id = [0; 16];
    let mut data = None;
    while let Some(c) = class.next()? {
        match (c.tcode, c.body) {
            (TCODE_CLASS_UUID, Some(mut b)) => id = b.uuid()?,
            (TCODE_CLASS_DATA, Some(b)) => data = Some(b),
            (TCODE_CLASS_END, _) => break,
            _ => {}
        }
    }
    Ok((id, data))
}

// ---------------------------------------------------------------------------
// model
// ---------------------------------------------------------------------------

/// Triangles in model units.
#[derive(Default)]
struct Part {
    positions: Vec<f32>,
    indices: Vec<u32>,
}

impl Part {
    fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

enum Geometry {
    Parts(Vec<Part>),
    Instance {
        definition: [u8; 16],
        xform: [[f64; 4]; 4],
    },
}

struct Object {
    id: Option<[u8; 16]>,
    layer: i32,
    /// ON::object_mode: 1 hidden, 3 part of a block definition.
    mode: u8,
    visible: bool,
    geometry: Geometry,
}

struct Model {
    version: u32,
    opennurbs: u64,
    unit: u32,
    hidden_layers: Vec<bool>,
    definitions: HashMap<[u8; 16], Vec<[u8; 16]>>,
    objects: Vec<Object>,
    /// Triangles read so far (before blocks are expanded).
    triangles: usize,
}

/// The archive version and a cursor behind the 32-byte header.
fn open(data: &[u8]) -> Result<(u32, Cur<'_>)> {
    let start = data
        .get(..data.len().min(MAX_HEADER_OFFSET + MAGIC.len()))
        .and_then(|head| head.windows(MAGIC.len()).position(|w| w == MAGIC))
        .ok_or_else(|| anyhow!("This is not a Rhino model (.3dm header missing)."))?;
    let digits = data
        .get(start + MAGIC.len()..start + 32)
        .ok_or_else(|| short_file(start))?;
    let mut version: u32 = 0;
    for &d in digits.iter().skip_while(|&&d| d == b' ') {
        // openNURBS reads an 'X' in the last place as 2 (early beta files)
        let d = if d == b'X' { b'2' } else { d };
        if !d.is_ascii_digit() {
            bail!("This is not a Rhino model (invalid version).");
        }
        version = version
            .saturating_mul(10)
            .saturating_add(u32::from(d - b'0'));
    }
    if version == 0 {
        bail!("This is not a Rhino model (invalid version).");
    }
    let cur = Cur {
        data,
        at: start + 32,
        end: data.len(),
        big: version >= 50,
        v1: version == 1,
    };
    Ok((version, cur))
}

/// Reads a Rhino model into one preview mesh in millimetres.
pub fn read(data: &[u8]) -> Result<Mesh> {
    let (version, mut cur) = open(data)?;
    let mut model = Model {
        version,
        opennurbs: 0,
        unit: 2,
        hidden_layers: Vec::new(),
        definitions: HashMap::new(),
        objects: Vec::new(),
        triangles: 0,
    };
    while let Some(c) = cur.next()? {
        let Some(body) = c.body else {
            continue;
        };
        match c.tcode {
            TCODE_PROPERTIES_TABLE => model.properties(body)?,
            TCODE_SETTINGS_TABLE => model.settings(body)?,
            TCODE_LAYER_TABLE => model.layers(body)?,
            TCODE_IDEF_TABLE => model.definitions(body)?,
            TCODE_OBJECT_TABLE => model.objects(body)?,
            TCODE_V1_MESH if version == 1 => {
                // a damaged legacy mesh costs that mesh, not the model
                if let Ok(part) = v1_mesh(body) {
                    model.add_triangles(part.triangles())?;
                    model.objects.push(Object {
                        id: None,
                        layer: 0,
                        mode: 0,
                        visible: true,
                        geometry: Geometry::Parts(vec![part]),
                    });
                }
            }
            TCODE_ENDOFFILE => break,
            _ => {}
        }
    }
    model.scene()
}

impl Model {
    fn add_triangles(&mut self, n: usize) -> Result<()> {
        self.triangles = self.triangles.saturating_add(n);
        check_triangles(self.triangles)
    }

    fn properties(&mut self, mut table: Cur<'_>) -> Result<()> {
        while let Some(c) = table.next()? {
            match c.tcode {
                TCODE_OPENNURBS_VERSION => self.opennurbs = u64::try_from(c.value).unwrap_or(0),
                TCODE_ENDOFTABLE => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn settings(&mut self, mut table: Cur<'_>) -> Result<()> {
        while let Some(c) = table.next()? {
            match (c.tcode, c.body) {
                (TCODE_UNITS, Some(mut b)) => {
                    let version = b.i32()?;
                    if (100..200).contains(&version) {
                        self.unit = b.u32()?;
                    }
                }
                (TCODE_ENDOFTABLE, _) => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn layers(&mut self, mut table: Cur<'_>) -> Result<()> {
        while let Some(c) = table.next()? {
            match (c.tcode, c.body) {
                (TCODE_LAYER_RECORD, Some(mut record)) => {
                    // the layer index is its place in the table
                    let hidden = read_object(&mut record)
                        .ok()
                        .and_then(|(_, data)| data)
                        .and_then(|mut d| {
                            let (major, _) = d.version().ok()?;
                            (major == 1).then(|| d.i32().ok()).flatten()
                        })
                        .is_some_and(|mode| mode == 1);
                    self.hidden_layers.push(hidden);
                }
                (TCODE_ENDOFTABLE, _) => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn definitions(&mut self, mut table: Cur<'_>) -> Result<()> {
        while let Some(c) = table.next()? {
            match (c.tcode, c.body) {
                (TCODE_IDEF_RECORD, Some(mut record)) => {
                    let Ok((_, Some(data))) = read_object(&mut record) else {
                        continue;
                    };
                    if let Ok((id, objects)) = definition(data, self.version) {
                        self.definitions.insert(id, objects);
                    }
                }
                (TCODE_ENDOFTABLE, _) => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn objects(&mut self, mut table: Cur<'_>) -> Result<()> {
        while let Some(c) = table.next()? {
            match (c.tcode, c.body) {
                (TCODE_OBJECT_RECORD, Some(record)) => {
                    // a damaged object costs that object, not the model
                    match self.object(record) {
                        Ok(Some(o)) => self.objects.push(o),
                        Ok(None) => {}
                        Err(e) if self.triangles > MAX_TRIANGLES => return Err(e),
                        Err(_) => {}
                    }
                }
                (TCODE_ENDOFTABLE, _) => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn object(&mut self, mut record: Cur<'_>) -> Result<Option<Object>> {
        let mut geometry = None;
        let mut attributes = None;
        while record.left() > 0 {
            let at = record.at;
            let c = record.chunk()?;
            match (c.tcode, c.body) {
                (TCODE_OBJECT_TYPE, _) => {}
                (TCODE_CLASS, Some(_)) => {
                    record.at = at;
                    let (class, data) = read_object(&mut record)?;
                    if let Some(data) = data {
                        geometry = self.geometry(class, data)?;
                    }
                }
                (TCODE_OBJECT_ATTRIBUTES, Some(b)) => {
                    attributes = Some(read_attributes(b, self.version, self.opennurbs)?);
                }
                (TCODE_OBJECT_END, _) => break,
                _ => {}
            }
        }
        let Some(geometry) = geometry else {
            return Ok(None);
        };
        let a = attributes.unwrap_or_default();
        Ok(Some(Object {
            id: a.id,
            layer: a.layer,
            mode: a.mode,
            visible: a.visible,
            geometry,
        }))
    }

    fn geometry(&mut self, class: [u8; 16], mut data: Cur<'_>) -> Result<Option<Geometry>> {
        let parts = if class == MESH {
            vec![mesh(data)?]
        } else if BREPS.contains(&class) {
            brep_meshes(data)?
        } else if class == EXTRUSION {
            extrusion_mesh(data)?.into_iter().collect()
        } else if class == SUBD {
            subd::mesh(data, self.version)?.into_iter().collect()
        } else if class == INSTANCE_REF {
            let (major, _) = data.version()?;
            if major != 1 {
                return Ok(None);
            }
            let definition = data.uuid()?;
            let m: [f64; 16] = data.f64s()?;
            let row = |r: usize| -> [f64; 4] {
                let mut out = [0.0; 4];
                for (k, v) in out.iter_mut().enumerate() {
                    *v = m.get(r * 4 + k).copied().unwrap_or(0.0);
                }
                out
            };
            return Ok(Some(Geometry::Instance {
                definition,
                xform: [row(0), row(1), row(2), row(3)],
            }));
        } else {
            return Ok(None);
        };
        let n = parts.iter().map(Part::triangles).sum();
        self.add_triangles(n)?;
        Ok((!parts.is_empty()).then_some(Geometry::Parts(parts)))
    }

    fn hidden(&self, o: &Object) -> bool {
        o.mode == 1
            || !o.visible
            || usize::try_from(o.layer)
                .ok()
                .and_then(|l| self.hidden_layers.get(l))
                .copied()
                .unwrap_or(false)
    }

    /// Visible objects, blocks expanded, in millimetres.
    fn scene(&self) -> Result<Mesh> {
        let by_id: HashMap<[u8; 16], &Object> = self
            .objects
            .iter()
            .filter_map(|o| o.id.map(|id| (id, o)))
            .collect();
        let in_definition: HashSet<[u8; 16]> =
            self.definitions.values().flatten().copied().collect();
        let top: Vec<&Object> = self
            .objects
            .iter()
            .filter(|o| o.mode != 3 && !o.id.is_some_and(|id| in_definition.contains(&id)))
            .filter(|o| !self.hidden(o))
            .collect();
        // count first: blocks can multiply the triangles
        let mut total = 0usize;
        for o in &top {
            total = total.saturating_add(self.count(o, &by_id, 0));
            check_triangles(total)?;
        }
        if total == 0 {
            bail!(
                "The Rhino model stores no meshes to show (polysurfaces saved without render \
                 meshes, e.g. with \"Save Small\", curves or points only)."
            );
        }
        let scale = millimetres_per_unit(self.unit);
        let mut out = Mesh::default();
        let identity = scaling(scale);
        for o in top {
            self.emit(o, &by_id, &identity, 0, &mut out);
        }
        Ok(out)
    }

    fn count(&self, o: &Object, by_id: &HashMap<[u8; 16], &Object>, depth: usize) -> usize {
        match &o.geometry {
            Geometry::Parts(parts) => parts.iter().map(Part::triangles).sum(),
            Geometry::Instance { definition, .. } => {
                if depth >= MAX_DEPTH {
                    return 0;
                }
                self.members(definition, by_id)
                    .map(|c| self.count(c, by_id, depth + 1))
                    .fold(0usize, usize::saturating_add)
            }
        }
    }

    fn members<'s>(
        &'s self,
        definition: &[u8; 16],
        by_id: &'s HashMap<[u8; 16], &'s Object>,
    ) -> impl Iterator<Item = &'s Object> + 's {
        self.definitions
            .get(definition)
            .into_iter()
            .flatten()
            .filter_map(|id| by_id.get(id).copied())
            .filter(|o| !self.hidden(o))
    }

    fn emit(
        &self,
        o: &Object,
        by_id: &HashMap<[u8; 16], &Object>,
        xform: &[[f64; 4]; 4],
        depth: usize,
        out: &mut Mesh,
    ) {
        match &o.geometry {
            Geometry::Parts(parts) => {
                let mut mesh = Mesh {
                    parts: 1,
                    ..Mesh::default()
                };
                for part in parts {
                    let base = mesh.vertex_count() as u32;
                    for p in part.positions.chunks_exact(3) {
                        if let [x, y, z] = *p {
                            mesh.positions.extend(apply(xform, [x, y, z]));
                        }
                    }
                    mesh.indices.extend(part.indices.iter().map(|i| i + base));
                }
                out.append(weld(mesh));
            }
            Geometry::Instance {
                definition,
                xform: local,
            } => {
                if depth >= MAX_DEPTH {
                    return;
                }
                let child = multiply(xform, local);
                for c in self.members(definition, by_id) {
                    self.emit(c, by_id, &child, depth + 1, out);
                }
            }
        }
    }
}

fn scaling(s: f64) -> [[f64; 4]; 4] {
    [
        [s, 0.0, 0.0, 0.0],
        [0.0, s, 0.0, 0.0],
        [0.0, 0.0, s, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// `a · b` (column-vector transforms: `b` is applied first).
fn multiply(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..4)
                .map(|k| {
                    a.get(i).and_then(|r| r.get(k)).copied().unwrap_or(0.0)
                        * b.get(k).and_then(|r| r.get(j)).copied().unwrap_or(0.0)
                })
                .sum();
        }
    }
    out
}

/// `M · (x, y, z, 1)` with the projective division of openNURBS.
fn apply(m: &[[f64; 4]; 4], [x, y, z]: [f32; 3]) -> [f32; 3] {
    let p = [f64::from(x), f64::from(y), f64::from(z), 1.0];
    let row = |r: &[f64; 4]| r.iter().zip(p).map(|(a, b)| a * b).sum::<f64>();
    let [r0, r1, r2, r3] = m;
    let w = row(r3);
    let w = if w == 0.0 || !w.is_finite() { 1.0 } else { w };
    [
        (row(r0) / w) as f32,
        (row(r1) / w) as f32,
        (row(r2) / w) as f32,
    ]
}

/// Merges vertices that coincide within 1 µm (the faces of a polysurface
/// are meshed separately; their edges share positions).
fn weld(mesh: Mesh) -> Mesh {
    let mut map: HashMap<[i64; 3], u32> = HashMap::with_capacity(mesh.vertex_count());
    let mut remap = Vec::with_capacity(mesh.vertex_count());
    let mut out = Mesh {
        parts: mesh.parts,
        ..Mesh::default()
    };
    for p in mesh.positions.chunks_exact(3) {
        let key = [0, 1, 2].map(|k| {
            p.get(k)
                .map_or(0, |&c| (f64::from(c) * 1000.0).round() as i64)
        });
        let next = out.vertex_count() as u32;
        let index = *map.entry(key).or_insert_with(|| {
            out.positions.extend_from_slice(p);
            next
        });
        remap.push(index);
    }
    for t in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| {
            t.get(k)
                .and_then(|&i| remap.get(i as usize))
                .copied()
                .unwrap_or(0)
        });
        if a != b && b != c && a != c {
            out.indices.extend([a, b, c]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// attributes, block definitions
// ---------------------------------------------------------------------------

struct Attributes {
    id: Option<[u8; 16]>,
    layer: i32,
    mode: u8,
    visible: bool,
}

impl Default for Attributes {
    fn default() -> Self {
        Attributes {
            id: None,
            layer: 0,
            mode: 0,
            visible: true,
        }
    }
}

/// `ON_3dmObjectAttributes`: id, layer, mode and visibility.
fn read_attributes(mut c: Cur<'_>, version: u32, opennurbs: u64) -> Result<Attributes> {
    let (_, minor) = c.version()?;
    let mut a = Attributes {
        id: Some(c.uuid()?),
        layer: c.i32()?,
        ..Attributes::default()
    };
    if version >= 5 && opennurbs >= 200_712_190 {
        // a list of (item id, value) pairs up to item 0; later items are not needed
        loop {
            match c.u8()? {
                0 => break,
                1 | 2 => c.skip_string()?,
                3 | 4 | 6 | 7 | 10 | 22 => c.skip(4)?,
                5 => {
                    c.body()?;
                }
                8 => c.skip(8)?,
                9 | 13..=17 | 19 => c.skip(1)?,
                11 => a.visible = c.u8()? != 0,
                12 => a.mode = c.u8()? & 0x0F,
                18 => {
                    let n = c.count(4)?;
                    c.skip_items(n, 4)?;
                }
                20 => c.skip(16)?,
                21 => {
                    let n = c.count(32)?;
                    c.skip_items(n, 32)?;
                }
                _ => break,
            }
        }
        return Ok(a);
    }
    // material, colour, obsolete line style (2 + 2 + 8 + 8), wire density
    c.skip(4 + 4 + 20 + 4)?;
    a.mode = c.u8()? & 0x0F;
    a.visible = a.mode != 1;
    c.skip(3)?;
    c.skip_string()?;
    c.skip_string()?;
    if minor >= 1 {
        let n = c.count(4)?;
        c.skip_items(n, 4)?;
        if minor >= 2 {
            a.visible = c.u8()? != 0;
        }
    }
    Ok(a)
}

/// `ON_InstanceDefinition`: its id and the ids of its objects.
fn definition(mut c: Cur<'_>, version: u32) -> Result<([u8; 16], Vec<[u8; 16]>)> {
    let ids = |c: &mut Cur<'_>| -> Result<Vec<[u8; 16]>> {
        let n = c.count(16)?;
        (0..n).map(|_| c.uuid()).collect()
    };
    if version >= 60 {
        let mut probe = c;
        let first = probe.chunk()?;
        if let (TCODE_ANONYMOUS, Some(mut a)) = (first.tcode, first.body) {
            a.version2()?;
            let mut attributes = a.body()?; // model component attributes
            attributes.version2()?;
            let bits = attributes.u32()?;
            if bits & 1 == 0 {
                bail!("block definition without id");
            }
            let id = attributes.uuid()?;
            a.skip(4)?; // definition type
            a.body()?; // unit system
            a.skip_string()?;
            a.skip_string()?;
            a.skip_string()?;
            a.skip(48)?; // bounding box
            let objects = if a.u8()? != 0 {
                ids(&mut a)?
            } else {
                Vec::new()
            };
            return Ok((id, objects));
        }
    }
    c.version()?;
    let id = c.uuid()?;
    Ok((id, ids(&mut c)?))
}

// ---------------------------------------------------------------------------
// meshes
// ---------------------------------------------------------------------------

/// `ReadCompressedBuffer` of exactly `size` bytes: CRC-32, method (0 raw,
/// 1 zlib in an anonymous chunk), data.
fn compressed(c: &mut Cur<'_>, size: usize) -> Result<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let crc = c.u32()?;
    let out = match c.u8()? {
        0 => c.bytes(size)?.to_vec(),
        1 => {
            let chunk = c.chunk()?;
            let (TCODE_ANONYMOUS, Some(mut z)) = (chunk.tcode, chunk.body) else {
                bail!("The Rhino file is damaged: compressed data expected.");
            };
            let input = z.bytes(z.left())?;
            // a zlib stream inflates to at most ~1032 times its size
            if size / 1032 > input.len() {
                bail!("The Rhino file is damaged: compressed data too short.");
            }
            let mut out = Vec::with_capacity(size);
            flate2::read::ZlibDecoder::new(input)
                .take(size as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| anyhow!("The Rhino file is damaged: {e}."))?;
            out
        }
        m => bail!("The Rhino file is damaged: unknown compression {m}."),
    };
    if out.len() != size {
        bail!("The Rhino file is damaged: compressed data has the wrong size.");
    }
    let mut check = flate2::Crc::new();
    check.update(&out);
    if check.sum() != crc {
        bail!("The Rhino file is damaged: checksum error in compressed data.");
    }
    Ok(out)
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    super::quads(bytes).map(f32::from_le_bytes).collect()
}

/// Quads and triangles (`d == c`) → triangles; faces with an index outside
/// the vertices or with non-finite vertices are dropped.
fn part(positions: Vec<f32>, faces: impl Iterator<Item = [u32; 4]>) -> Part {
    let n = (positions.len() / 3) as u32;
    let finite = |i: u32| {
        positions
            .get(i as usize * 3..i as usize * 3 + 3)
            .is_some_and(|p| p.iter().all(|v| v.is_finite()))
    };
    let mut indices = Vec::new();
    for [a, b, c, d] in faces {
        if a >= n || b >= n || c >= n || d >= n || ![a, b, c, d].into_iter().all(finite) {
            continue;
        }
        indices.extend([a, b, c]);
        if c != d {
            indices.extend([a, c, d]);
        }
    }
    Part { positions, indices }
}

/// `ON_Mesh::Read` (class data).
fn mesh(mut c: Cur<'_>) -> Result<Part> {
    let (major, _) = c.version()?;
    if major != 1 && major != 3 {
        bail!("unsupported mesh version {major}");
    }
    let vcount = c.u32()? as usize;
    let fcount = c.u32()? as usize;
    if vcount > 4 * MAX_TRIANGLES || fcount > MAX_TRIANGLES {
        bail!("The Rhino file is damaged: too many mesh vertices or faces.");
    }
    // texture and surface domains, scale, boxes (6 + 6 + 4 floats), closed flag
    c.skip(8 * 8 + 2 * 8 + 16 * 4 + 4)?;
    if c.u8()? != 0 {
        c.body()?; // mesh parameters
    }
    for _ in 0..4 {
        if c.u8()? != 0 {
            c.body()?; // curvature statistics
        }
    }
    let index_size = c.i32()?;
    let index_size = match index_size {
        1 | 2 | 4 => index_size as usize,
        _ if fcount == 0 => 4,
        other => bail!("unsupported face index size {other}"),
    };
    if fcount.saturating_mul(4 * index_size) > c.left() {
        bail!("The Rhino file is damaged: too many mesh faces.");
    }
    let face_bytes = c.bytes(fcount * 4 * index_size)?;
    let index = |i: usize| -> u32 {
        let at = i * index_size;
        match index_size {
            1 => face_bytes.get(at).map_or(u32::MAX, |&b| u32::from(b)),
            2 => face_bytes
                .get(at..at + 2)
                .and_then(|s| <[u8; 2]>::try_from(s).ok())
                .map_or(u32::MAX, |b| u32::from(u16::from_le_bytes(b))),
            _ => face_bytes
                .get(at..at + 4)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .map_or(u32::MAX, u32::from_le_bytes),
        }
    };
    let positions = if major == 1 {
        let n = c.count(12)?;
        floats(c.bytes(n * 12)?)
    } else if vcount > 0 {
        let size = c.u32()? as usize;
        if size != 0 && size != vcount.saturating_mul(12) {
            bail!("The Rhino file is damaged: vertex buffer has the wrong size.");
        }
        floats(&compressed(&mut c, size)?)
    } else {
        Vec::new()
    };
    let faces = (0..fcount).map(|f| [0, 1, 2, 3].map(|k| index(f * 4 + k)));
    Ok(part(positions, faces))
}

/// The render meshes of an `ON_Brep` (version 3.1 and later), one per face.
fn brep_meshes(mut c: Cur<'_>) -> Result<Vec<Part>> {
    let (major, minor) = c.version()?;
    if major != 3 || minor < 1 {
        return Ok(Vec::new());
    }
    // 2d curves, 3d curves, surfaces, vertices, edges, trims, loops, faces
    for _ in 0..8 {
        c.body()?;
    }
    c.skip(48)?; // bounding box
    let chunk = c.chunk()?;
    let (TCODE_ANONYMOUS, Some(mut meshes)) = (chunk.tcode, chunk.body) else {
        bail!("The Rhino file is damaged: B-rep render meshes expected.");
    };
    let mut out = Vec::new();
    while meshes.left() > 0 {
        if meshes.u8()? == 0 {
            continue;
        }
        let (class, data) = read_object(&mut meshes)?;
        if let (true, Some(data)) = (class == MESH, data) {
            out.push(mesh(data)?);
        }
    }
    Ok(out)
}

/// The render mesh of an `ON_Extrusion` (mesh cache, chunk version 1.3+).
fn extrusion_mesh(mut c: Cur<'_>) -> Result<Option<Part>> {
    let mut e = c.body()?;
    let (major, minor) = e.version2()?;
    if major != 1 || minor < 3 {
        return Ok(None);
    }
    read_object(&mut e)?; // profile
                          // path, t, up, have N0/N1, N0, N1, path domain, transposed, profile count, caps
    e.skip(6 * 8 + 2 * 8 + 3 * 8 + 2 + 6 * 8 + 2 * 8 + 1 + 4 + 2)?;
    let mut cache = e.body()?;
    cache.version2()?;
    while cache.u8()? == 1 {
        let mut item = cache.body()?;
        item.version2()?;
        item.uuid()?;
        let (class, data) = read_object(&mut item)?;
        if let (true, Some(data)) = (class == MESH, data) {
            // the render mesh comes first
            return Ok(Some(mesh(data)?));
        }
    }
    Ok(None)
}

/// A Rhino 1 mesh: points quantised to 16 bits inside the bounding box.
fn v1_mesh(mut c: Cur<'_>) -> Result<Part> {
    let chunk = c.chunk()?;
    let (TCODE_V1_MESH_GEOMETRY, Some(mut g)) = (chunk.tcode, chunk.body) else {
        bail!("not a Rhino 1 mesh");
    };
    let points = g.count(6)?;
    let faces = g.count(8)?;
    g.skip(8)?; // normals and texture flags
    let min: [f64; 3] = g.f64s()?;
    let max: [f64; 3] = g.f64s()?;
    let mut positions = Vec::with_capacity(points * 3);
    for _ in 0..points {
        for (lo, hi) in min.iter().zip(max) {
            let q = f64::from(g.u16()?);
            positions.push((lo + (hi - lo) * q / 65535.0) as f32);
        }
    }
    let wide = points >= 65535;
    let mut list = Vec::with_capacity(faces);
    for _ in 0..faces {
        let mut f = [0u32; 4];
        for v in &mut f {
            *v = if wide { g.u32()? } else { u32::from(g.u16()?) };
        }
        list.push(f);
    }
    Ok(part(positions, list.into_iter()))
}

// ---------------------------------------------------------------------------
// preview image
// ---------------------------------------------------------------------------

/// The preview bitmap Rhino stores in the properties table, as a DIB
/// (BITMAPINFOHEADER + pixels), for files without a 3D preview.
pub fn preview_dib(data: &[u8]) -> Option<Vec<u8>> {
    let (_, mut cur) = open(data).ok()?;
    while let Some(c) = cur.next().ok()? {
        if c.tcode == TCODE_PROPERTIES_TABLE {
            let mut table = c.body?;
            while let Some(c) = table.next().ok()? {
                if let (TCODE_COMPRESSED_PREVIEW, Some(mut b)) = (c.tcode, c.body) {
                    return dib(&mut b).ok();
                }
            }
            return None;
        }
    }
    None
}

fn dib(b: &mut Cur<'_>) -> Result<Vec<u8>> {
    let mut header = Vec::with_capacity(40);
    let _size = b.u32()?;
    header.extend(40u32.to_le_bytes());
    header.extend(b.bytes(8)?); // width, height
    header.extend(b.bytes(4)?); // planes, bit count
    header.extend(b.bytes(24)?); // compression … important colours
    let pixels = b.u32()? as usize;
    if pixels > 64 * 1024 * 1024 {
        bail!("preview too large");
    }
    let mut out = header;
    out.extend(compressed(b, pixels)?);
    Ok(out)
}

// ---------------------------------------------------------------------------
// SubD
// ---------------------------------------------------------------------------

mod subd {
    //! `ON_SubD`: the level-0 control net and its Catmull-Clark refinement.

    use std::collections::HashMap;

    use anyhow::{bail, Result};

    use super::{Cur, Part, MAX_SUBD_FACES, MAX_SUBD_LEVELS};

    /// End of the optional additions of an element (Rhino 7+).
    const END: u8 = 255;
    const ANONYMOUS: u8 = 254;
    const CREASE: u8 = 2;
    const CORNER: u8 = 3;

    /// The size byte of an optional field: 0 absent, `valid` present, 255 end.
    fn addition(c: &mut Cur<'_>, valid: u8) -> Result<u8> {
        let sz = c.u8()?;
        if sz != 0 && sz != valid && sz != END {
            bail!("The Rhino file is damaged: bad SubD data.");
        }
        Ok(sz)
    }

    /// Skips the rest of the additions up to the end mark.
    fn finish(c: &mut Cur<'_>) -> Result<()> {
        loop {
            match c.u8()? {
                END => return Ok(()),
                ANONYMOUS => {
                    c.body()?;
                }
                n => c.skip(usize::from(n))?,
            }
        }
    }

    /// The common part of vertices, edges and faces.
    fn base(c: &mut Cur<'_>, v7: bool) -> Result<u32> {
        let archive_id = c.u32()?;
        c.skip(4 + 2)?; // id, level
        if !v7 {
            if c.u8()? != 0 {
                c.skip(24)?;
            }
            if c.u8()? != 0 {
                c.skip(24)?;
            }
            return Ok(archive_id);
        }
        for (valid, size) in [(24, 24), (4, 4), (5, 5)] {
            match addition(c, valid)? {
                END => return Ok(archive_id),
                0 => {}
                _ => c.skip(size)?,
            }
        }
        finish(c)?;
        Ok(archive_id)
    }

    /// A list of element references (`u16` count, then id + flags).
    fn list(c: &mut Cur<'_>) -> Result<Vec<(u32, u8)>> {
        let n = usize::from(c.u16()?);
        if n.saturating_mul(5) > c.left() {
            bail!("The Rhino file is damaged: bad SubD list.");
        }
        (0..n).map(|_| Ok((c.u32()?, c.u8()?))).collect()
    }

    fn element_end(c: &mut Cur<'_>, v7: bool) -> Result<()> {
        if v7 {
            finish(c)
        } else if c.u8()? != 0 {
            bail!("The Rhino file is damaged: bad SubD element.")
        } else {
            Ok(())
        }
    }

    struct Net {
        points: Vec<[f64; 3]>,
        corner: Vec<bool>,
        creases: Vec<(u32, u32)>,
        faces: Vec<Vec<u32>>,
    }

    fn read(mut c: Cur<'_>, version: u32) -> Result<Option<Net>> {
        let v7 = version >= 70;
        if c.u8()? != 1 {
            return Ok(None);
        }
        let mut s = c.body()?;
        if s.version2()?.0 != 1 {
            return Ok(None);
        }
        let levels = s.u32()?;
        s.skip(12 + 48)?;
        if levels == 0 {
            return Ok(None);
        }
        let mut l = s.body()?;
        l.version2()?;
        l.skip(2 + 3 + 48)?;
        let [p0, p1, p2, p3] = [l.u32()?, l.u32()?, l.u32()?, l.u32()?];
        if !(p0 <= p1 && p1 <= p2 && p2 <= p3) || (p3 - p0) as usize > l.left() / 16 {
            bail!("The Rhino file is damaged: bad SubD counts.");
        }
        let mut vertex = HashMap::new();
        let mut net = Net {
            points: Vec::new(),
            corner: Vec::new(),
            creases: Vec::new(),
            faces: Vec::new(),
        };
        for _ in p0..p1 {
            let id = base(&mut l, v7)?;
            let tag = l.u8()?;
            let p: [f64; 3] = l.f64s()?;
            l.skip(4)?; // edge and face counts
            if l.u8()? != 0 {
                // saved limit points: 12 doubles and a face reference each
                let n = l.count(101)?;
                l.skip_items(n, 101)?;
            }
            list(&mut l)?;
            list(&mut l)?;
            element_end(&mut l, v7)?;
            if !p.iter().all(|v| v.is_finite()) {
                bail!("The Rhino file is damaged: SubD point not finite.");
            }
            vertex.insert(id, net.points.len() as u32);
            net.points.push(p);
            net.corner.push(tag == CORNER);
        }
        let mut edges = HashMap::new();
        for _ in p1..p2 {
            let id = base(&mut l, v7)?;
            let tag = l.u8()?;
            l.skip(2 + 16 + 8)?; // face count, sector coefficients, sharpness
            let ends = list(&mut l)?;
            list(&mut l)?;
            if v7 && version >= 80 {
                match l.u8()? {
                    END => {}
                    8 => {
                        l.skip(8)?;
                        finish(&mut l)?;
                    }
                    _ => bail!("The Rhino file is damaged: bad SubD edge."),
                }
            } else {
                element_end(&mut l, v7)?;
            }
            let end = |k: usize| ends.get(k).and_then(|(v, _)| vertex.get(v)).copied();
            if let (Some(a), Some(b)) = (end(0), end(1)) {
                edges.insert(id, (a, b));
                if tag == CREASE {
                    net.creases.push((a, b));
                }
            }
        }
        for _ in p2..p3 {
            base(&mut l, v7)?;
            l.skip(4 + 4 + 2)?; // level-zero face id, parent, edge count
            let loop_edges = list(&mut l)?;
            if v7 {
                // packing (34), material channel, colour, pack id (4 each), texture points
                let mut ended = false;
                for (i, valid) in [34u8, 4, 4, 4, 4].into_iter().enumerate() {
                    match addition(&mut l, valid)? {
                        END => {
                            ended = true;
                            break;
                        }
                        0 => {}
                        _ if i < 4 => l.skip(usize::from(valid))?,
                        _ => {
                            let tens = l.u32()? as usize;
                            for _ in 0..tens {
                                if l.u8()? != 240 {
                                    bail!("The Rhino file is damaged: bad SubD texture.");
                                }
                                l.skip(240)?;
                            }
                            let rest = loop_edges.len() % 10;
                            if rest > 0 {
                                if usize::from(l.u8()?) != rest * 24 {
                                    bail!("The Rhino file is damaged: bad SubD texture.");
                                }
                                l.skip(rest * 24)?;
                            }
                        }
                    }
                }
                if !ended {
                    finish(&mut l)?;
                }
            } else {
                element_end(&mut l, v7)?;
            }
            let corners: Option<Vec<u32>> = loop_edges
                .iter()
                .map(|(e, flags)| {
                    edges
                        .get(e)
                        .map(|&(a, b)| if flags & 1 == 0 { a } else { b })
                })
                .collect();
            if let Some(f) = corners.filter(|f| f.len() >= 3) {
                net.faces.push(f);
            }
        }
        Ok(Some(net))
    }

    fn key(a: u32, b: u32) -> (u32, u32) {
        (a.min(b), a.max(b))
    }

    fn add(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
        [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
    }

    fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
        [a[0] * s, a[1] * s, a[2] * s]
    }

    fn point(points: &[[f64; 3]], i: u32) -> [f64; 3] {
        points.get(i as usize).copied().unwrap_or([0.0; 3])
    }

    /// Edges of the net with their faces; boundary and crease edges are sharp.
    struct Topology {
        edge_faces: HashMap<(u32, u32), Vec<usize>>,
        vertex_edges: Vec<Vec<(u32, u32)>>,
        vertex_faces: Vec<Vec<usize>>,
    }

    fn topology(n: usize, faces: &[Vec<u32>]) -> Topology {
        let mut t = Topology {
            edge_faces: HashMap::new(),
            vertex_edges: vec![Vec::new(); n],
            vertex_faces: vec![Vec::new(); n],
        };
        for (fi, f) in faces.iter().enumerate() {
            for (i, &a) in f.iter().enumerate() {
                let b = f.get((i + 1) % f.len()).copied().unwrap_or(a);
                t.edge_faces.entry(key(a, b)).or_default().push(fi);
                if let Some(vf) = t.vertex_faces.get_mut(a as usize) {
                    vf.push(fi);
                }
            }
        }
        for &(a, b) in t.edge_faces.keys() {
            for v in [a, b] {
                if let Some(ve) = t.vertex_edges.get_mut(v as usize) {
                    ve.push((a, b));
                }
            }
        }
        t
    }

    fn other(e: (u32, u32), v: u32) -> u32 {
        if e.0 == v {
            e.1
        } else {
            e.0
        }
    }

    struct Level {
        points: Vec<[f64; 3]>,
        corner: Vec<bool>,
        creases: std::collections::HashSet<(u32, u32)>,
        faces: Vec<Vec<u32>>,
    }

    /// One Catmull-Clark step (creases: Hoppe et al. rules).
    fn subdivide(l: &Level) -> Level {
        let n = l.points.len();
        let t = topology(n, &l.faces);
        let sharp = |e: &(u32, u32)| {
            t.edge_faces.get(e).is_none_or(|f| f.len() != 2) || l.creases.contains(e)
        };
        let face_points: Vec<[f64; 3]> = l
            .faces
            .iter()
            .map(|f| {
                let s = f
                    .iter()
                    .fold([0.0; 3], |acc, &v| add(acc, point(&l.points, v), 1.0));
                scale(s, 1.0 / f.len() as f64)
            })
            .collect();
        let face_point = |fi: usize| face_points.get(fi).copied().unwrap_or([0.0; 3]);
        let mut edge_index = HashMap::with_capacity(t.edge_faces.len());
        let mut edge_points = Vec::with_capacity(t.edge_faces.len());
        for (e, faces) in &t.edge_faces {
            let mid = scale(add(point(&l.points, e.0), point(&l.points, e.1), 1.0), 0.5);
            let p = match (sharp(e), faces.as_slice()) {
                (false, [f, g]) => {
                    scale(add(add(mid, face_point(*f), 0.5), face_point(*g), 0.5), 0.5)
                }
                _ => mid,
            };
            edge_index.insert(*e, (n + edge_points.len()) as u32);
            edge_points.push(p);
        }
        let mut points = Vec::with_capacity(n + edge_points.len() + face_points.len());
        for v in 0..n {
            let p = point(&l.points, v as u32);
            let edges = t.vertex_edges.get(v).map(Vec::as_slice).unwrap_or(&[]);
            let sharp_edges: Vec<&(u32, u32)> = edges.iter().filter(|e| sharp(e)).collect();
            let corner = l.corner.get(v).copied().unwrap_or(false);
            let q = if corner || sharp_edges.len() > 2 || edges.is_empty() {
                p
            } else if let [a, b] = sharp_edges.as_slice() {
                let s = add(
                    point(&l.points, other(**a, v as u32)),
                    point(&l.points, other(**b, v as u32)),
                    1.0,
                );
                add(scale(p, 0.75), s, 0.125)
            } else {
                let k = edges.len() as f64;
                let faces = t.vertex_faces.get(v).map(Vec::as_slice).unwrap_or(&[]);
                let f = scale(
                    faces
                        .iter()
                        .fold([0.0; 3], |acc, &fi| add(acc, face_point(fi), 1.0)),
                    1.0 / faces.len().max(1) as f64,
                );
                let r = scale(
                    edges.iter().fold([0.0; 3], |acc, e| {
                        add(
                            acc,
                            add(point(&l.points, e.0), point(&l.points, e.1), 1.0),
                            0.5,
                        )
                    }),
                    1.0 / k,
                );
                scale(add(add(f, r, 2.0), p, k - 3.0), 1.0 / k)
            };
            points.push(q);
        }
        points.extend(edge_points);
        let first_face = points.len() as u32;
        points.extend(face_points.iter().copied());
        let mut faces = Vec::new();
        let mut creases = std::collections::HashSet::new();
        for (fi, f) in l.faces.iter().enumerate() {
            let m = f.len();
            for (i, &b) in f.iter().enumerate() {
                let a = f.get((i + m - 1) % m).copied().unwrap_or(b);
                let c = f.get((i + 1) % m).copied().unwrap_or(b);
                let (Some(&next), Some(&prev)) =
                    (edge_index.get(&key(b, c)), edge_index.get(&key(a, b)))
                else {
                    continue;
                };
                faces.push(vec![b, next, first_face + fi as u32, prev]);
            }
        }
        for (e, &mid) in &edge_index {
            if sharp(e) {
                creases.insert(key(e.0, mid));
                creases.insert(key(mid, e.1));
            }
        }
        let mut corner = l.corner.clone();
        corner.resize(points.len(), false);
        Level {
            points,
            corner,
            creases,
            faces,
        }
    }

    /// Moves the vertices of a quad net onto the limit surface.
    fn limit(l: &Level) -> Vec<[f64; 3]> {
        let t = topology(l.points.len(), &l.faces);
        let sharp = |e: &(u32, u32)| {
            t.edge_faces.get(e).is_none_or(|f| f.len() != 2) || l.creases.contains(e)
        };
        (0..l.points.len())
            .map(|v| {
                let p = point(&l.points, v as u32);
                let edges = t.vertex_edges.get(v).map(Vec::as_slice).unwrap_or(&[]);
                let sharp_edges: Vec<&(u32, u32)> = edges.iter().filter(|e| sharp(e)).collect();
                if l.corner.get(v).copied().unwrap_or(false)
                    || sharp_edges.len() > 2
                    || edges.is_empty()
                {
                    return p;
                }
                if let [a, b] = sharp_edges.as_slice() {
                    let s = add(
                        point(&l.points, other(**a, v as u32)),
                        point(&l.points, other(**b, v as u32)),
                        1.0,
                    );
                    return scale(add(scale(p, 4.0), s, 1.0), 1.0 / 6.0);
                }
                let n = edges.len() as f64;
                let e = edges.iter().fold([0.0; 3], |acc, e| {
                    add(acc, point(&l.points, other(*e, v as u32)), 1.0)
                });
                let faces = t.vertex_faces.get(v).map(Vec::as_slice).unwrap_or(&[]);
                let f = faces.iter().fold([0.0; 3], |acc, &fi| {
                    let face = l.faces.get(fi).map(Vec::as_slice).unwrap_or(&[]);
                    let opposite = face
                        .iter()
                        .position(|&x| x as usize == v)
                        .and_then(|i| face.get((i + 2) % face.len().max(1)))
                        .copied()
                        .unwrap_or(v as u32);
                    add(acc, point(&l.points, opposite), 1.0)
                });
                scale(
                    add(add(scale(p, n * n), e, 4.0), f, 1.0),
                    1.0 / (n * (n + 5.0)),
                )
            })
            .collect()
    }

    /// The SubD as a refined quad mesh (two triangles per quad).
    pub(super) fn mesh(data: Cur<'_>, version: u32) -> Result<Option<Part>> {
        let Some(net) = read(data, version)? else {
            return Ok(None);
        };
        if net.faces.is_empty() {
            return Ok(None);
        }
        let mut level = Level {
            points: net.points,
            corner: net.corner,
            creases: net.creases.iter().map(|&(a, b)| key(a, b)).collect(),
            faces: net.faces,
        };
        let mut corners: usize = level.faces.iter().map(Vec::len).sum();
        let mut steps = 0;
        while steps == 0 || (steps < MAX_SUBD_LEVELS && corners.saturating_mul(4) <= MAX_SUBD_FACES)
        {
            if corners > MAX_SUBD_FACES {
                bail!("The SubD has too many faces for the preview.");
            }
            level = subdivide(&level);
            corners = level.faces.len() * 4;
            steps += 1;
        }
        let points = limit(&level);
        let mut positions = Vec::with_capacity(points.len() * 3);
        for p in &points {
            positions.extend(p.map(|v| v as f32));
        }
        let mut indices = Vec::with_capacity(level.faces.len() * 6);
        for f in &level.faces {
            if let [a, b, c, d] = f.as_slice() {
                indices.extend([*a, *b, *c, *a, *c, *d]);
            }
        }
        Ok(Some(Part { positions, indices }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A 10 mm cube with smooth edges, `steps` times subdivided, on the limit surface.
        fn smooth_cube(steps: usize, creased: bool) -> Vec<[f64; 3]> {
            let mut points = Vec::new();
            for i in 0..8u32 {
                points.push([
                    f64::from(i & 1) * 10.0,
                    f64::from((i >> 1) & 1) * 10.0,
                    f64::from((i >> 2) & 1) * 10.0,
                ]);
            }
            let faces = vec![
                vec![0, 2, 3, 1],
                vec![4, 5, 7, 6],
                vec![0, 1, 5, 4],
                vec![2, 6, 7, 3],
                vec![0, 4, 6, 2],
                vec![1, 3, 7, 5],
            ];
            let mut creases = std::collections::HashSet::new();
            if creased {
                // the four edges around the top face
                for (a, b) in [(4, 5), (5, 7), (7, 6), (6, 4)] {
                    creases.insert(key(a, b));
                }
            }
            let mut level = Level {
                points,
                corner: vec![false; 8],
                creases,
                faces,
            };
            for _ in 0..steps {
                level = subdivide(&level);
            }
            limit(&level)
        }

        fn z_range(points: &[[f64; 3]]) -> (f64, f64) {
            points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
                (lo.min(p[2]), hi.max(p[2]))
            })
        }

        #[test]
        fn the_limit_surface_does_not_depend_on_the_steps() {
            let (lo1, hi1) = z_range(&smooth_cube(1, false));
            let (lo3, hi3) = z_range(&smooth_cube(3, false));
            // centre of a face of the Catmull-Clark limit of a cube: 5 ± 340/81
            assert!((hi1 - (5.0 + 340.0 / 81.0)).abs() < 1e-9, "{hi1}");
            assert!((lo1 - (5.0 - 340.0 / 81.0)).abs() < 1e-9, "{lo1}");
            assert!((hi3 - hi1).abs() < 1e-9 && (lo3 - lo1).abs() < 1e-9);
        }

        #[test]
        fn creases_keep_their_edges() {
            let (_, hi) = z_range(&smooth_cube(3, true));
            // the creased square stays in the plane z = 10 and pulls the top flat
            assert!((hi - 10.0).abs() < 1e-9, "{hi}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn uuids_are_stored_little_endian() {
        assert_eq!(
            MESH,
            [
                0xE4, 0xD4, 0xD7, 0x4E, 0x47, 0xE9, 0xD3, 0x11, 0xBF, 0xE5, 0x00, 0x10, 0x83, 0x01,
                0x22, 0xF0
            ]
        );
    }

    #[test]
    fn units_become_millimetres() {
        assert_eq!(millimetres_per_unit(2), 1.0);
        assert_eq!(millimetres_per_unit(4), 1000.0);
        assert_eq!(millimetres_per_unit(8), 25.4);
        assert_eq!(millimetres_per_unit(0), 1.0);
    }

    #[test]
    fn transforms_compose_as_column_vectors() {
        let mut shift = scaling(1.0);
        shift[0][3] = 10.0;
        let twice = multiply(&scaling(2.0), &shift);
        // the shift is applied first, then everything is scaled
        assert_eq!(apply(&twice, [1.0, 0.0, 0.0]), [22.0, 0.0, 0.0]);
    }

    #[test]
    fn not_a_rhino_file() {
        assert!(read(b"solid cube\nendsolid").is_err());
        assert!(read(b"3D Geometry File Format     abc").is_err());
    }

    #[test]
    fn a_header_alone_is_an_empty_model() {
        let err = read(b"3D Geometry File Format       50").unwrap_err();
        assert!(err.to_string().contains("no meshes"), "{err}");
    }

    // --- a small version-50 archive written by the test -------------------

    fn chunk(tcode: u32, body: &[u8]) -> Vec<u8> {
        let mut out = tcode.to_le_bytes().to_vec();
        let crc = if tcode & TCODE_CRC != 0 { 4 } else { 0 };
        out.extend(((body.len() + crc) as i64).to_le_bytes());
        out.extend(body);
        out.extend(vec![0; crc]); // the reader does not check chunk CRCs
        out
    }

    fn short(tcode: u32, value: i64) -> Vec<u8> {
        let mut out = tcode.to_le_bytes().to_vec();
        out.extend(value.to_le_bytes());
        out
    }

    fn object(class: [u8; 16], data: &[u8]) -> Vec<u8> {
        let mut body = chunk(TCODE_CLASS_UUID, &class);
        body.extend(chunk(TCODE_CLASS_DATA, data));
        body.extend(short(TCODE_CLASS_END, 0));
        chunk(TCODE_CLASS, &body)
    }

    fn table(tcode: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut body = records.concat();
        body.extend(short(TCODE_ENDOFTABLE, 0));
        chunk(tcode, &body)
    }

    /// An uncompressed (version 1.0) mesh: one triangle.
    fn triangle() -> Vec<u8> {
        let mut d = vec![0x10];
        d.extend(3i32.to_le_bytes());
        d.extend(1i32.to_le_bytes());
        d.extend(vec![0; 8 * 8 + 2 * 8 + 16 * 4 + 4]);
        d.extend([0, 0, 0, 0, 0]);
        d.extend(4i32.to_le_bytes());
        for i in [0u32, 1, 2, 2] {
            d.extend(i.to_le_bytes());
        }
        d.extend(3i32.to_le_bytes());
        for v in [0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            d.extend(v.to_le_bytes());
        }
        d
    }

    fn instance(definition: [u8; 16], [x, y, z]: [f64; 3]) -> Vec<u8> {
        let mut d = vec![0x10];
        d.extend(definition);
        let m = [
            1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z, 0.0, 0.0, 0.0, 1.0,
        ];
        for v in m {
            d.extend(f64::to_le_bytes(v));
        }
        d.extend(vec![0; 48]); // bounding box
        d
    }

    fn record(class: [u8; 16], data: &[u8], id: u8, mode: u8) -> Vec<u8> {
        let mut attributes = vec![0x20];
        attributes.extend([id; 16]);
        attributes.extend(0i32.to_le_bytes());
        attributes.extend([12, mode, 0]);
        let mut body = short(TCODE_OBJECT_TYPE, 0x20);
        body.extend(object(class, data));
        body.extend(chunk(TCODE_OBJECT_ATTRIBUTES, &attributes));
        body.extend(short(TCODE_OBJECT_END, 0));
        chunk(TCODE_OBJECT_RECORD, &body)
    }

    fn definition(id: u8, members: &[u8]) -> Vec<u8> {
        let mut d = vec![0x10];
        d.extend([id; 16]);
        d.extend((members.len() as i32).to_le_bytes());
        for &m in members {
            d.extend([m; 16]);
        }
        chunk(TCODE_IDEF_RECORD, &object([0; 16], &d))
    }

    /// An 8 × 8 preview bitmap (24 bit), stored raw.
    fn preview() -> Vec<u8> {
        let mut d = Vec::new();
        for v in [40i32, 8, 8] {
            d.extend(v.to_le_bytes());
        }
        d.extend(1i16.to_le_bytes());
        d.extend(24i16.to_le_bytes());
        d.extend([0u8; 24]);
        let pixels = [200u8; 8 * 24];
        d.extend((pixels.len() as u32).to_le_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&pixels);
        d.extend(crc.sum().to_le_bytes());
        d.push(0);
        d.extend(pixels);
        chunk(TCODE_COMPRESSED_PREVIEW, &d)
    }

    /// Metres; definition 1 = a triangle, definition 2 = definition 1
    /// raised by 5 m; drawn: definition 2 at x + 10 m and definition 1 at
    /// the origin. A hidden triangle and the definition's own triangle are
    /// not drawn.
    pub(crate) fn model() -> Vec<u8> {
        let mut file = b"3D Geometry File Format       50".to_vec();
        file.extend(table(
            TCODE_PROPERTIES_TABLE,
            &[short(TCODE_OPENNURBS_VERSION, 201_209_145), preview()],
        ));
        let mut units = 102i32.to_le_bytes().to_vec();
        units.extend(4u32.to_le_bytes());
        file.extend(table(TCODE_SETTINGS_TABLE, &[chunk(TCODE_UNITS, &units)]));
        file.extend(table(
            TCODE_IDEF_TABLE,
            &[definition(0xD1, &[0xA1]), definition(0xD2, &[0xA2])],
        ));
        file.extend(table(
            TCODE_OBJECT_TABLE,
            &[
                record(MESH, &triangle(), 0xA1, 3),
                record(
                    INSTANCE_REF,
                    &instance([0xD1; 16], [0.0, 0.0, 5.0]),
                    0xA2,
                    3,
                ),
                record(
                    INSTANCE_REF,
                    &instance([0xD2; 16], [10.0, 0.0, 0.0]),
                    0xB1,
                    0,
                ),
                record(INSTANCE_REF, &instance([0xD1; 16], [0.0; 3]), 0xB2, 0),
                record(MESH, &triangle(), 0xB3, 1),
            ],
        ));
        file.extend(chunk(TCODE_ENDOFFILE, &[0; 8]));
        file
    }

    #[test]
    fn blocks_are_placed_hidden_objects_left_out() {
        let mesh = read(&model()).unwrap();
        mesh.check().unwrap();
        assert_eq!(mesh.triangle_count(), 2);
        assert_eq!(mesh.parts, 2);
        assert_eq!(mesh.bounds(), [0.0, 0.0, 0.0, 11_000.0, 1000.0, 5000.0]);
    }

    #[test]
    fn the_stored_preview_bitmap_is_found() {
        let dib = preview_dib(&model()).unwrap();
        assert_eq!(dib.len(), 40 + 8 * 24);
        assert_eq!(dib.get(..4), Some(&40u32.to_le_bytes()[..]));
        assert!(preview_dib(b"3D Geometry File Format       50").is_none());
    }

    #[test]
    fn damaged_models_are_errors_not_panics() {
        let data = model();
        for cut in 0..data.len() {
            let _ = read(data.get(..cut).unwrap_or_default());
        }
        for at in 32..data.len() {
            for v in [0x00, 0xFF, 0x7F] {
                let mut copy = data.clone();
                if let Some(b) = copy.get_mut(at) {
                    *b = v;
                }
                let _ = read(&copy);
                let _ = preview_dib(&copy);
            }
        }
    }

    #[test]
    fn welding_joins_coincident_corners() {
        let mesh = Mesh {
            positions: vec![
                0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, // first triangle
                1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, // second, sharing an edge
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            parts: 1,
            ..Mesh::default()
        };
        let w = weld(mesh);
        assert_eq!(w.vertex_count(), 4);
        assert_eq!(w.triangle_count(), 2);
    }
}
