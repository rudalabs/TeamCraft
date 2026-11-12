//! CATIA V5 products (`.CATProduct`): which documents are placed where (see
//! `docs/src/research/catia-product-structure.md`).
//!
//! * Every CATIA document lists its containers near the start of the file:
//!
//!   ```text
//!   02 00 81 20 | u32 n | name \0 | type \0 | up to 8 × 00
//!   | 03 00 f7 00 03 00 00 00 | uuid (4 × u32 big-endian)
//!   ```
//!
//!   Products refer to a component by the UUID of its container *named*
//!   `CATProdCont` – no file name is stored ([`document`]).
//! * The product's own `CATProdCont` is an object stream (`OSMX`) of
//!   sections `7c <kind> <u32 length from the 7c>`: header `0c`, objects
//!   `04`, one side block per object `08`, extensions `0b`, strings `02`.
//!   Small numbers are `81..cf` (− 0x81), `d1..e3 xx` or `80 <u32>`; a
//!   string reference is `34..ff` (− 0x34) or `32 <u32 index + 1>`; doubles
//!   are `e6 <f64>`, `e7` / `e8` / `e9` (1, 0, −1).
//! * An object (`7c 05`) has a header `7c 06` (two flag bytes, class, name,
//!   `ea <u32 id>`) and attributes `7c 07`:
//!   `<name> <key> e8|e9 <flags> <type> [byte after e9] 37 <value> fe`
//!   (type `12`: a string, `07`: `<n> <0> n doubles`).
//! * An instance is an `ASMPRODUCT` with `_InstanceName`; its name is the
//!   part number. Its side block holds the link to the referenced document
//!   (`e5 <u32 length> 11 03 <uuid> …`) and the list `_BagRepsList`, whose
//!   `PRDBAGREP` carries `_Position`: X, Y and Z axis and origin (mm) of
//!   the instance in the product. Without it the instance sits at the
//!   origin.
//!
//! Everything is read defensively: every length is checked against the
//! bytes that are left, and an instance that does not add up is left out,
//! never a panic. No CATIA code or documentation was used.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::{
    io::{self, Read},
    path::Path,
};

use crate::nxasm::{Component, Kind};

/// Magic of a CATIA V5 document.
const MAGIC: &[u8; 8] = b"V5_CFV2\0";
/// The container table lies in the first bytes of the file.
pub const HEAD: usize = 64 * 1024;
/// Largest product read.
pub const MAX_PRODUCT: u64 = 64 * 1024 * 1024;
/// Object streams tried at most.
const MAX_STREAMS: usize = 64;
/// Instances read at most.
const MAX_INSTANCES: usize = 100_000;

const CONTAINER_ENTRY: [u8; 4] = [0x02, 0x00, 0x81, 0x20];
const UUID_TAG: [u8; 8] = [0x03, 0x00, 0xf7, 0x00, 0x03, 0x00, 0x00, 0x00];

/// The text form of a container UUID, as CATIA names its containers
/// (`Cont_<uuid>`): `hhhhhhhh_h_hhhhhhhh_h`.
pub fn uuid_text(raw: &[u8]) -> Option<String> {
    let w =
        |k: usize| -> Option<u32> { Some(u32::from_be_bytes(raw.get(k..k + 4)?.try_into().ok()?)) };
    Some(format!(
        "{:08x}_{:x}_{:08x}_{:x}",
        w(0)?,
        w(4)?,
        w(8)?,
        w(12)?
    ))
}

/// The containers of a CATIA document: (name, type, UUID).
pub fn containers(data: &[u8]) -> Vec<(String, String, String)> {
    let head = data.get(..data.len().min(HEAD)).unwrap_or_default();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = find(head, &CONTAINER_ENTRY, at) {
        at = found + 1;
        if let Some(entry) = container_at(head, found + CONTAINER_ENTRY.len() + 4) {
            out.push(entry);
        }
    }
    out
}

fn container_at(d: &[u8], at: usize) -> Option<(String, String, String)> {
    let word = |from: usize| -> Option<(String, usize)> {
        let rest = d.get(from..)?;
        let len = rest.iter().take(64).position(|&b| b == 0)?;
        let text = rest.get(..len)?;
        if len == 0 || !text.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_') {
            return None;
        }
        Some((String::from_utf8_lossy(text).into_owned(), from + len + 1))
    };
    let (name, at) = word(at)?;
    let (kind, mut at) = word(at)?;
    for _ in 0..8 {
        if d.get(at) != Some(&0) {
            break;
        }
        at += 1;
    }
    if d.get(at..at + UUID_TAG.len())? != UUID_TAG {
        return None;
    }
    let uuid = uuid_text(d.get(at + UUID_TAG.len()..at + UUID_TAG.len() + 16)?)?;
    Some((name, kind, uuid))
}

/// The UUID products refer to this document by (its container named
/// `CATProdCont`), from the first [`HEAD`] bytes of a CATIA file.
pub fn document(data: &[u8]) -> Option<String> {
    if !data.starts_with(MAGIC) {
        return None;
    }
    containers(data)
        .into_iter()
        .find(|(name, _, _)| name == "CATProdCont")
        .map(|(_, _, uuid)| uuid)
}

/// [`document`] of a file (reads only its first bytes).
pub fn document_of(path: &Path) -> io::Result<Option<String>> {
    let mut head = Vec::with_capacity(HEAD);
    std::fs::File::open(path)?
        .take(HEAD as u64)
        .read_to_end(&mut head)?;
    Ok(document(&head))
}

/// What a product file holds: its instances, or [`Kind::Other`].
pub fn kind_of(path: &Path) -> io::Result<Kind> {
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_PRODUCT)
        .read_to_end(&mut data)?;
    Ok(match components(&data) {
        Some(c) if !c.is_empty() => Kind::Assembly(c),
        _ => Kind::Other,
    })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// A small number: (value, next position).
fn varint(d: &[u8], at: usize) -> Option<(u64, usize)> {
    let b = *d.get(at)?;
    match b {
        0x80 => Some((u64::from(u32_at(d, at + 1)?), at + 5)),
        0x81..=0xcf => Some((u64::from(b - 0x81), at + 1)),
        0xd1..=0xe3 => Some((
            (u64::from(b - 0xd1) << 8) + u64::from(*d.get(at + 1)?),
            at + 2,
        )),
        _ => None,
    }
}

/// A string-table reference: (index, next position).
fn strref(d: &[u8], at: usize) -> Option<(usize, usize)> {
    match *d.get(at)? {
        0x32 => Some(((u32_at(d, at + 1)? as usize).checked_sub(1)?, at + 5)),
        b @ 0x34..=0xff => Some((usize::from(b - 0x34), at + 1)),
        _ => None,
    }
}

/// The bytes of a string-table reference.
fn strref_bytes(index: usize) -> Vec<u8> {
    match u8::try_from(index + 0x34) {
        Ok(b) => vec![b],
        Err(_) => {
            let mut v = vec![0x32];
            v.extend((index as u32 + 1).to_le_bytes());
            v
        }
    }
}

/// A number token: (value, next position).
fn number(d: &[u8], at: usize) -> Option<(f64, usize)> {
    match *d.get(at)? {
        0xe6 => Some((
            f64::from_le_bytes(d.get(at + 1..at + 9)?.try_into().ok()?),
            at + 9,
        )),
        0xe7 => Some((1.0, at + 1)),
        0xe8 => Some((0.0, at + 1)),
        0xe9 => Some((-1.0, at + 1)),
        _ => None,
    }
}

/// The section `7c <kind>` at `at`: (body start, end).
fn section(d: &[u8], at: usize, kind: u8) -> Option<(usize, usize)> {
    if d.get(at..at + 2)? != [0x7c, kind] {
        return None;
    }
    let end = at.checked_add(u32_at(d, at + 2)? as usize)?;
    (end >= at + 6 && end <= d.len()).then_some((at + 6, end))
}

/// The section `7c <kind>` at `at` or a few bytes later.
fn section_near(d: &[u8], at: usize, kind: u8) -> Option<(usize, usize, usize)> {
    (at..at + 8).find_map(|p| section(d, p, kind).map(|(s, e)| (p, s, e)))
}

/// One object of the stream.
struct Object {
    id: Option<u32>,
    class: Option<usize>,
    name: Option<usize>,
    attributes: (usize, usize),
}

/// The lists of an object's side block that matter.
#[derive(Default)]
struct Side {
    link: Option<String>,
    bags: Vec<u64>,
}

/// One object stream with its string table.
struct Stream<'a> {
    d: &'a [u8],
    strings: Vec<&'a [u8]>,
    objects: (usize, usize),
    sides: (usize, usize),
}

impl<'a> Stream<'a> {
    fn at(d: &'a [u8], osmx: usize) -> Option<Stream<'a>> {
        let head = find(d.get(..d.len().min(osmx + 0x200))?, &[0x7c, 0x0c], osmx)?;
        let (_, end) = section(d, head, 0x0c)?;
        let (_, obj_start, obj_end) = section_near(d, end, 0x04)?;
        let (_, side_start, side_end) = section_near(d, obj_end, 0x08)?;
        let (_, _, ext_end) = section_near(d, side_end, 0x0b)?;
        let (_, str_start, str_end) = section_near(d, ext_end, 0x02)?;
        let strings = strings(d, str_start, str_end)?;
        Some(Stream {
            d,
            strings,
            objects: (obj_start, obj_end),
            sides: (side_start, side_end),
        })
    }

    fn index(&self, text: &str) -> Option<usize> {
        self.strings.iter().position(|s| *s == text.as_bytes())
    }

    fn text(&self, index: Option<usize>) -> Option<String> {
        let s = self.strings.get(index?)?;
        Some(s.iter().map(|&b| char::from(b)).collect())
    }

    fn objects(&self) -> Vec<Object> {
        let d = self.d;
        let (mut p, end) = self.objects;
        let mut out = Vec::new();
        while p + 7 < end && out.len() <= MAX_INSTANCES * 4 {
            let Some((_, next)) = section(d, p, 0x05) else {
                break;
            };
            if next <= p {
                break;
            }
            if let Some(object) = self.object(p + 7) {
                out.push(object);
            }
            p = next;
        }
        out
    }

    fn object(&self, at: usize) -> Option<Object> {
        let d = self.d;
        let (h, hend) = section(d, at, 0x06)?;
        let (class, q) = strref(d, h + 2)?;
        let (name, q) = strref(d, q)?;
        let id = (d.get(q) == Some(&0xea))
            .then(|| u32_at(d, q + 1))
            .flatten();
        let attributes = section(d, hend, 0x07)?;
        Some(Object {
            id,
            class: Some(class),
            name: Some(name),
            attributes,
        })
    }

    /// One side block per object, in object order (`None` if the counts
    /// differ).
    fn sides(&self, count: usize, bags: Option<usize>) -> Option<Vec<Side>> {
        let d = self.d;
        let (mut p, end) = self.sides;
        let mut out = Vec::new();
        while p < end && out.len() <= count {
            let (s, e) = section(d, p, 0x09)?;
            out.push(side(d, s, e, bags));
            p = e;
        }
        (out.len() == count).then_some(out)
    }

    /// Right after `<name> <key> e8|e9 <flags> <type> [x] 37` of the
    /// attribute `name`, with its type.
    fn attribute(&self, object: &Object, name: Option<usize>) -> Option<(usize, u8)> {
        let d = self.d;
        let pattern = strref_bytes(name?);
        let (a, end) = object.attributes;
        let mut p = a;
        while p < end {
            let starts = p == a || d.get(p.wrapping_sub(1)) == Some(&0xfe);
            if starts && d.get(p..p + pattern.len()) == Some(pattern.as_slice()) {
                if let Some((_, q)) = varint(d, p + pattern.len()) {
                    if matches!(d.get(q), Some(0xe8 | 0xe9)) {
                        let kind = *d.get(q + 2)?;
                        let mut r = q + 3;
                        if d.get(r) != Some(&0x37) {
                            r += 1;
                        }
                        if d.get(r) == Some(&0x37) && r < end {
                            return Some((r + 1, kind));
                        }
                    }
                }
            }
            p += 1;
        }
        None
    }

    fn string_attribute(&self, object: &Object, name: Option<usize>) -> Option<String> {
        let (at, kind) = self.attribute(object, name)?;
        if kind != 0x12 {
            return None;
        }
        self.text(Some(strref(self.d, at)?.0))
    }

    /// The doubles of an array attribute: `None` when it is absent,
    /// `Some(None)` when it is there but cannot be read.
    fn doubles(&self, object: &Object, name: Option<usize>) -> Option<Option<Vec<f64>>> {
        let (at, kind) = self.attribute(object, name)?;
        let read = || -> Option<Vec<f64>> {
            if kind != 0x07 {
                return None;
            }
            let (n, mut p) = varint(self.d, at)?;
            (_, p) = varint(self.d, p)?;
            let mut values = Vec::new();
            for _ in 0..n.min(64) {
                let (v, next) = number(self.d, p)?;
                values.push(v);
                p = next;
            }
            (self.d.get(p) == Some(&0xfe)).then_some(values)
        };
        Some(read())
    }
}

/// The string table `<count> { <len + 1> bytes | 00 <u32 len> bytes }`.
fn strings(d: &[u8], start: usize, end: usize) -> Option<Vec<&[u8]>> {
    let (count, mut p) = varint(d, start)?;
    let mut out = Vec::new();
    while p < end {
        let len = *d.get(p)?;
        let (from, to) = if len == 0 {
            let n = u32_at(d, p + 1)? as usize;
            (p + 5, p.checked_add(5)?.checked_add(n)?)
        } else {
            (p + 1, p + usize::from(len))
        };
        out.push(d.get(from..to.min(end))?);
        p = to;
    }
    (out.len() as u64 == count).then_some(out)
}

/// A side block `<b0> <b1> [e5 <u32 L> link] <small numbers> 7c 0a { <name>
/// <count> <0> { <id> (80 <u32> | 81) } × count } × fe`.
fn side(d: &[u8], s: usize, e: usize, bags: Option<usize>) -> Side {
    let mut out = Side::default();
    let mut q = s + 2;
    if d.get(q) == Some(&0xe5) {
        let Some(len) = u32_at(d, q + 1) else {
            return out;
        };
        let start = q + 5;
        let end = start.saturating_add(len as usize).min(e);
        out.link = d.get(start..end).and_then(link);
        q = end;
    }
    while q < e && d.get(q..q + 2) != Some(&[0x7c, 0x0a]) {
        match varint(d, q) {
            Some((_, next)) => q = next,
            None => return out,
        }
    }
    let Some((mut r, end)) = section(d, q, 0x0a) else {
        return out;
    };
    while r < end && d.get(r) != Some(&0xfe) {
        let Some((name, next)) = strref(d, r) else {
            break;
        };
        let Some((count, next)) = varint(d, next) else {
            break;
        };
        let Some((_, mut next)) = varint(d, next) else {
            break;
        };
        let mut ids = Vec::new();
        for _ in 0..count.min(1_000_000) {
            let Some((id, after)) = varint(d, next) else {
                return out;
            };
            ids.push(id);
            next = if d.get(after) == Some(&0x80) {
                after + 5
            } else {
                after + 1
            };
            if next > end {
                return out;
            }
        }
        if Some(name) == bags {
            out.bags = ids;
        }
        r = next;
    }
    out
}

/// The link of a side block: the UUID of the target container.
fn link(blob: &[u8]) -> Option<String> {
    if blob.get(..2)? != [0x11, 0x03] {
        return None;
    }
    uuid_text(blob.get(2..18)?)
}

/// The placement of an instance from its 12 doubles (X, Y, Z axis and
/// origin), or `None` when they are no rotation and translation.
fn placement(values: &[f64]) -> Option<([[f64; 3]; 3], [f64; 3])> {
    let v: &[f64; 12] = values.try_into().ok()?;
    if !v.iter().all(|x| x.is_finite() && x.abs() < 1e9) {
        return None;
    }
    let rotation = [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    // the axes are unit vectors (a mirrored component is a document of its own)
    if rotation
        .iter()
        .any(|a| ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt() - 1.0).abs() > 1e-3)
    {
        return None;
    }
    Some((rotation, [v[9], v[10], v[11]]))
}

/// The instances of a product (the whole file): name = part number,
/// `document` = UUID of the referenced document. `None` when it is no
/// CATIA product.
pub fn components(data: &[u8]) -> Option<Vec<Component>> {
    if !data.starts_with(MAGIC) {
        return None;
    }
    let mut at = 0;
    let mut tried = 0;
    let stream = loop {
        let osmx = find(data, b"OSMX", at)?;
        at = osmx + 4;
        tried += 1;
        if tried > MAX_STREAMS {
            return None;
        }
        if let Some(s) = Stream::at(data, osmx) {
            if s.index("ASMPRODUCT").is_some() && s.index("_InstanceName").is_some() {
                break s;
            }
        }
    };
    let asm = stream.index("ASMPRODUCT");
    let bag_class = stream.index("PRDBAGREP");
    let instance_name = stream.index("_InstanceName");
    let position = stream.index("_Position");
    let objects = stream.objects();
    let by_id: std::collections::HashMap<u64, usize> = objects
        .iter()
        .enumerate()
        .filter_map(|(k, o)| Some((u64::from(o.id?), k)))
        .collect();
    let sides = stream.sides(objects.len(), stream.index("_BagRepsList"));
    let mut out = Vec::new();
    for (k, object) in objects.iter().enumerate() {
        if object.class != asm || out.len() >= MAX_INSTANCES {
            continue;
        }
        let Some(instance) = stream.string_attribute(object, instance_name) else {
            continue; // the product itself
        };
        let side = sides.as_ref().and_then(|s| s.get(k));
        let Some(document) = side.and_then(|s| s.link.clone()) else {
            continue; // a component without a document of its own
        };
        // the placement holder: from the side table, else the next object
        let listed = side
            .and_then(|s| s.bags.first())
            .and_then(|id| by_id.get(id))
            .and_then(|&k| objects.get(k));
        let bag = listed
            .or_else(|| objects.get(k + 1))
            .filter(|o| o.class == bag_class);
        let (rotation, translation) = match bag.and_then(|b| stream.doubles(b, position)) {
            None => (
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0; 3],
            ),
            Some(values) => match values.as_deref().and_then(placement) {
                Some(p) => p,
                None => continue,
            },
        };
        let part_number = stream.text(object.name).filter(|n| !n.trim().is_empty());
        out.push(Component {
            name: part_number.unwrap_or(instance),
            rotation,
            translation,
            layer: None,
            occurrence: None,
            document: Some(document),
        });
    }
    Some(out)
}

/// An instance of [`sample`]: part number, UUID of the referenced
/// document, placement (X, Y, Z axis and origin) or `None`.
pub type SampleInstance<'a> = (&'a str, [u8; 16], Option<[f64; 12]>);

/// A product as CATIA stores it, reduced to what [`components`] reads:
/// `own` is the product's container UUID, each instance (part number,
/// UUID of the referenced document, placement as X, Y, Z axis and origin
/// or `None` for none stored). For tests and the fuzz corpus.
#[doc(hidden)]
pub fn sample(own: [u8; 16], instances: &[SampleInstance]) -> Vec<u8> {
    let mut strings: Vec<String> = [
        "ASMPRODUCT",
        "PRDBAGREP",
        "Product1",
        "_Component",
        "_BagRepsList",
        "_InstanceName",
        "_Position",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut s = |text: String| -> Vec<u8> {
        let k = match strings.iter().position(|x| *x == text) {
            Some(k) => k,
            None => {
                strings.push(text);
                strings.len() - 1
            }
        };
        strref_bytes(k)
    };
    let small = |n: usize| -> Vec<u8> {
        if n < 0x4f {
            vec![0x81 + n as u8]
        } else {
            let mut v = vec![0x80];
            v.extend((n as u32).to_le_bytes());
            v
        }
    };
    let sect = |kind: u8, body: &[u8]| -> Vec<u8> {
        let mut v = vec![0x7c, kind];
        v.extend((body.len() as u32 + 6).to_le_bytes());
        v.extend(body);
        v
    };
    let object = |class: &[u8], name: &[u8], id: u32, attributes: &[u8]| -> Vec<u8> {
        let mut header = vec![0x00, 0x08];
        header.extend(class);
        header.extend(name);
        header.push(0xea);
        header.extend(id.to_le_bytes());
        let mut body = vec![0x00];
        body.extend(sect(0x06, &header));
        body.extend(sect(0x07, attributes));
        sect(0x05, &body)
    };
    let list = |name: &[u8], ids: &[u32]| -> Vec<u8> {
        let mut v = name.to_vec();
        v.extend(small(ids.len()));
        v.push(0x81);
        for id in ids {
            v.extend(small(*id as usize));
            v.push(0x81);
        }
        v
    };
    let (asm, bag, product, component, bags, instance_name, position, empty) = (
        s("ASMPRODUCT".into()),
        s("PRDBAGREP".into()),
        s("Product1".into()),
        s("_Component".into()),
        s("_BagRepsList".into()),
        s("_InstanceName".into()),
        s("_Position".into()),
        s(String::new()),
    );
    // root (id 2), then per instance k: the instance (id 10 + 2k) and its bag
    let mut objects = object(&asm, &product, 2, &[0xfe]);
    let ids: Vec<u32> = (0..instances.len() as u32).map(|k| 10 + 2 * k).collect();
    let mut root_lists = list(&component, &ids);
    root_lists.push(0xfe);
    let mut sides = sect(
        0x09,
        &[[0x14, 0x01, 0x82].as_slice(), &sect(0x0a, &root_lists)].concat(),
    );
    for (k, (part_number, document, at)) in instances.iter().enumerate() {
        let id = ids.get(k).copied().unwrap_or_default();
        let mut attributes = instance_name.clone();
        attributes.extend([0x82, 0xe8, 0xc0, 0x12, 0x37]);
        attributes.extend(s(format!("{part_number}.{}", k + 1)));
        attributes.extend([0xfe, 0xfe]);
        objects.extend(object(&asm, &s(part_number.to_string()), id, &attributes));
        let mut placed = Vec::new();
        if let Some(values) = at {
            placed.extend(&position);
            placed.extend([0x82, 0xe8, 0xf4, 0x07, 0x37, 0x8d, 0x81]);
            for v in values {
                placed.push(0xe6);
                placed.extend(v.to_le_bytes());
            }
            placed.push(0xfe);
        }
        placed.push(0xfe);
        objects.extend(object(&bag, &empty, id + 1, &placed));
        let mut target = vec![0x11, 0x03];
        target.extend(document);
        target.extend([7, 0, 0, 0, 5, 0, 9]);
        target.extend(2u32.to_le_bytes());
        target.push(0);
        target.extend([0x0c, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0]);
        let mut block = vec![0x14, 0x00, 0xe5];
        block.extend((target.len() as u32).to_le_bytes());
        block.extend(target);
        block.push(0x86);
        let mut lists = list(&bags, &[id + 1]);
        lists.push(0xfe);
        block.extend(sect(0x0a, &lists));
        sides.extend(sect(0x09, &block));
        sides.extend(sect(
            0x09,
            &[0x14, 0x01, 0x82, 0x7c, 0x0a, 7, 0, 0, 0, 0xfe],
        ));
    }
    let mut table = small(strings.len());
    for text in &strings {
        table.push(text.len() as u8 + 1);
        table.extend(text.as_bytes());
    }
    let mut out = MAGIC.to_vec();
    out.resize(0x800, 0);
    out.extend(CONTAINER_ENTRY);
    out.extend(4u32.to_le_bytes());
    out.extend(b"CATProdCont\0CATFeatCont\0");
    out.extend(UUID_TAG);
    out.extend(own);
    out.resize(0x1000, 0);
    out.extend(b"OSMX");
    out.extend([0xa4, 0x82, 0x00]);
    out.extend(sect(0x0c, &[]));
    objects.push(0x00);
    out.extend(sect(0x04, &objects));
    out.extend(sect(0x08, &sides));
    out.extend(sect(0x0b, &[]));
    out.extend(sect(0x02, &table));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: [u8; 16] = [
        0xbd, 0x77, 0xb6, 0xa4, 0, 0, 0x1b, 0xa4, 0x64, 0xce, 0x5d, 0x6a, 0, 0, 6, 0x24,
    ];
    const PART: [u8; 16] = [
        0xbd, 0x77, 0xb6, 0xa4, 0, 0, 0x1b, 0xa4, 0x64, 0xce, 0x51, 0xd8, 0, 0, 0, 0x89,
    ];

    #[test]
    fn uuids_read_like_catia_names_its_containers() {
        assert_eq!(uuid_text(&OWN).unwrap(), "bd77b6a4_1ba4_64ce5d6a_624");
        let product = sample(OWN, &[]);
        assert_eq!(document(&product).unwrap(), "bd77b6a4_1ba4_64ce5d6a_624");
        assert_eq!(document(b"not a CATIA file"), None);
    }

    #[test]
    fn instances_with_their_documents_and_placements() {
        // beam: X axis (0, -1, 0), Y axis (1, 0, 0), Z axis (0, 0, 1)
        let beam = [
            0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, -0.3, -480.0, 718.0,
        ];
        let product = sample(OWN, &[("frame", PART, None), ("beam", PART, Some(beam))]);
        let c = components(&product).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "frame");
        assert_eq!(c[0].document.as_deref(), Some("bd77b6a4_1ba4_64ce51d8_89"));
        assert_eq!(c[0].place([1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
        assert_eq!(c[1].name, "beam");
        // local x runs along the stored X axis: world = x X + y Y + z Z + origin
        assert_eq!(c[1].place([1.0, 0.0, 0.0]), [-0.3, -481.0, 718.0]);
        assert_eq!(c[1].place([0.0, 1.0, 0.0]), [0.7, -480.0, 718.0]);
    }

    #[test]
    fn a_broken_placement_leaves_the_instance_out() {
        let mut skew = [0.0; 12];
        skew[0] = 2.0;
        let product = sample(OWN, &[("a", PART, Some(skew)), ("b", PART, None)]);
        let c = components(&product).unwrap();
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn long_string_references_are_read() {
        assert_eq!(strref(&strref_bytes(5), 0), Some((5, 1)));
        assert_eq!(strref(&strref_bytes(500), 0), Some((500, 5)));
        assert_eq!(varint(&[0xd2, 0x3a], 0), Some((314, 2)));
        assert_eq!(varint(&[0x8d], 0), Some((12, 1)));
    }

    #[test]
    fn damaged_products_never_panic() {
        let beam = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 5.0, 6.0, 7.0];
        let product = sample(OWN, &[("frame", PART, None), ("beam", PART, Some(beam))]);
        // every byte that is not padding: the container entry (0x800) and
        // the object stream (0x1000 on); every third byte stays fast
        let entry = 0x800..0x800 + 64;
        let stream = 0x1000..product.len();
        for at in entry.chain(stream.step_by(3)) {
            let mut copy = product.clone();
            copy[at] ^= 0x5a;
            let _ = components(&copy);
            let _ = document(&copy);
        }
        for cut in (0x1000..product.len()).step_by(7) {
            let _ = components(&product[..cut]);
        }
    }
}
