//! Components and placements of Siemens NX assemblies (`.prt` without a
//! tessellation of their own), read from the `/Root/FastLoad/Structure`
//! stream of the container (see `nxpart`).
//!
//! The stream is an NX object serialisation ("OM"). The layout was worked
//! out from the NIST MTC assembly and checked instance for instance against
//! its JT export (part, rotation and translation; differences below
//! 0.00001 mm) and with real assemblies of nested sub-assemblies
//! (`docs/src/research/nx-jt-preview.md`):
//!
//! ```text
//! 0x0c   "OM"; object offsets count from 0x0e
//! 0x1f   u32 n, n bytes platform; u32 n, n bytes file name; u32 object count N
//! …      N u32 object offsets (increasing), u32 N, N u32 object tags
//! ```
//!
//! The largest object ("SaveData") holds, in this order: `01 <count>` and
//! `count` numbers = one record of 9 numbers per placed instance (rotation
//! row 0, rotation row 1, translation in mm); `01 <M+1>` and M component
//! names (`04 <len+2> text 00`: the file name without `.prt`, upper case);
//! `01 <N+1>` and the 1-based name index of each of the N instances.
//! Another object is an int→int hash map from instance to record:
//! `<n+1> 01 <cap+1> keys… 01 <w+1> state words… 01 <cap+1> values…`
//! (2 state bits per slot, 2 = used; keys and values stored +1). An
//! instance without a record sits at the origin.
//!
//! Numbers: a byte `t` with `t & 0x70` in `0x20..=0x30` starts 8 bytes = the
//! big-endian double with `t + 0x10` as its first byte; `0x40..=0x50` starts
//! 4 bytes = the first four bytes of a double with `t − 0x10` first; both
//! divided by 4. `00`, `01`, `81`, `80` are 0, 1, −1, −0. Integers are
//! variable-length (`00..7f`, `80..9f` + 1 byte, `a0..bf` + 2, `e0`/`ff` + 4).
//!
//! Rotations are row vectors: a point `p` of the component lands at
//! `p · R + t`. Everything is read defensively: a stream that does not fit
//! gives `None`, never a panic.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

/// The stream with the assembly structure.
pub const STRUCTURE_STREAM: &str = "/Root/FastLoad/Structure";

/// Instances read at most.
const MAX_INSTANCES: usize = 100_000;
/// Objects of the OM serialisation at most.
const MAX_OBJECTS: usize = 1 << 20;
/// Object offsets count from the byte after "OM".
const BASE: usize = 0x0e;

/// A component instance of an assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    /// The component's file name without `.prt`, as NX stores it (upper case).
    pub name: String,
    /// Rotation rows: a component point `p` lands at `p · rotation + translation`.
    pub rotation: [[f64; 3]; 3],
    /// Millimetres.
    pub translation: [f64; 3],
}

impl Component {
    /// Applies the placement to a point.
    pub fn place(&self, p: [f64; 3]) -> [f64; 3] {
        let r = &self.rotation;
        let t = &self.translation;
        [
            p[0] * r[0][0] + p[1] * r[1][0] + p[2] * r[2][0] + t[0],
            p[0] * r[0][1] + p[1] * r[1][1] + p[2] * r[2][1] + t[1],
            p[0] * r[0][2] + p[1] * r[1][2] + p[2] * r[2][2] + t[2],
        ]
    }

    /// This placement inside a parent placement (sub-assemblies).
    pub fn within(&self, parent: &Component) -> Component {
        let mut rotation = [[0.0; 3]; 3];
        for (row, out) in self.rotation.iter().zip(rotation.iter_mut()) {
            for (k, o) in out.iter_mut().enumerate() {
                *o = parent
                    .rotation
                    .iter()
                    .zip(row)
                    .map(|(prow, &a)| a * prow.get(k).copied().unwrap_or(0.0))
                    .sum();
            }
        }
        Component {
            name: self.name.clone(),
            rotation,
            translation: parent.place(self.translation),
        }
    }
}

/// The components of an NX assembly held in memory, or `None` when the
/// bytes are no NX file with a readable assembly structure.
pub fn components(bytes: &[u8]) -> Option<Vec<Component>> {
    let streams = crate::nxpart::streams(bytes);
    let stream = streams.iter().find(|s| s.name == STRUCTURE_STREAM)?;
    structure(crate::nxpart::data(bytes, stream)?)
}

/// What an NX file is for the 3D preview, read with seeks (only the
/// directory and the structure stream).
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// It has a tessellation of its own (`/Root/FastLoad/JT`).
    Part,
    /// An assembly with these components.
    Assembly(Vec<Component>),
    /// Neither (a drawing, a damaged file, no NX file).
    Other,
}

/// The [`Kind`] of the NX file at `path`.
pub fn kind_of(path: &std::path::Path) -> std::io::Result<Kind> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let Some(mut part) = crate::nxpart::NxPart::open(file)? else {
        return Ok(Kind::Other);
    };
    if part
        .streams()
        .iter()
        .any(|s| s.name == crate::nxpart::JT_STREAM)
    {
        return Ok(Kind::Part);
    }
    let Some(stream) = part.read(STRUCTURE_STREAM, 64 * 1024 * 1024)? else {
        return Ok(Kind::Other);
    };
    Ok(match structure(&stream) {
        Some(c) if !c.is_empty() => Kind::Assembly(c),
        _ => Kind::Other,
    })
}

/// The components described by a `Structure` stream.
pub fn structure(s: &[u8]) -> Option<Vec<Component>> {
    if s.get(0x0c..0x0e)? != b"OM" {
        return None;
    }
    let objects = object_table(s)?;
    let (start, end) = objects
        .iter()
        .copied()
        .max_by_key(|&(a, b)| b.saturating_sub(a))?;
    let records = find_records(s, start, end);
    let names_from = records.as_ref().map_or(start, |r| r.1);
    let (names, index) = find_names(s, names_from, end)?;
    let records = records.map(|r| r.0).unwrap_or_default();
    let n = index.len();
    let map = if records.is_empty() {
        None
    } else {
        Some(record_map(s, &objects, n, records.len())?)
    };
    let mut out = Vec::with_capacity(n);
    for (i, &name_index) in index.iter().enumerate() {
        let name = names.get(name_index.checked_sub(1)?)?.clone();
        let record = map
            .as_ref()
            .and_then(|m| m.get(i).copied().flatten())
            .and_then(|r| records.get(r));
        out.push(match record {
            Some(&(rotation, translation)) => Component {
                name,
                rotation,
                translation,
            },
            None => Component {
                name,
                rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                translation: [0.0; 3],
            },
        });
    }
    Some(out)
}

fn u32_at(s: &[u8], at: usize) -> Option<usize> {
    let b = s.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_le_bytes(b.try_into().ok()?)).ok()
}

/// Object extents `(start, end)` from the object table.
fn object_table(s: &[u8]) -> Option<Vec<(usize, usize)>> {
    // header: two length-prefixed strings, then the object count
    let mut p: usize = 0x1f;
    for _ in 0..2 {
        p = p.checked_add(4)?.checked_add(u32_at(s, p)?)?;
    }
    let n = u32_at(s, p)?;
    if n == 0 || n > MAX_OBJECTS || n.checked_mul(8)? >= s.len() {
        return None;
    }
    let table_bytes = 4 * n;
    let last = s.len().checked_sub(table_bytes * 2 + 4)?;
    for at in p + 4..=last {
        if u32_at(s, at + table_bytes) != Some(n) {
            continue;
        }
        let offsets: Option<Vec<usize>> = (0..n).map(|k| u32_at(s, at + 4 * k)).collect();
        let Some(offsets) = offsets else { continue };
        let increasing = offsets.windows(2).all(|w| matches!(w, [a, b] if a < b));
        let (Some(&first), Some(&end)) = (offsets.first(), offsets.last()) else {
            continue;
        };
        if increasing && first + BASE > at && end + BASE < s.len() {
            let starts: Vec<usize> = offsets.iter().map(|o| o + BASE).collect();
            let mut out = Vec::with_capacity(n);
            for (k, &a) in starts.iter().enumerate() {
                out.push((a, starts.get(k + 1).copied().unwrap_or(s.len())));
            }
            return Some(out);
        }
    }
    None
}

/// A variable-length integer and its size.
fn varint(s: &[u8], at: usize) -> Option<(usize, usize)> {
    let b = *s.get(at)?;
    let byte = |k: usize| s.get(at + k).map(|&v| usize::from(v));
    Some(match b {
        0x00..=0x7f => (usize::from(b), 1),
        0x80..=0x9f => ((usize::from(b & 0x1f) << 8) | byte(1)?, 2),
        0xa0..=0xbf => (
            (usize::from(b & 0x1f) << 16) | (byte(1)? << 8) | byte(2)?,
            3,
        ),
        0xe0 | 0xff => {
            let v = u32::from_be_bytes(s.get(at + 1..at + 5)?.try_into().ok()?);
            (usize::try_from(v).ok()?, 5)
        }
        _ => return None,
    })
}

/// A number token and its size.
fn number(s: &[u8], at: usize) -> Option<(f64, usize)> {
    let t = *s.get(at)?;
    // the double's first byte, then `rest` of its bytes from the stream
    let double = |first: u8, rest: &[u8]| {
        let mut b = [0u8; 8];
        for (d, &v) in b.iter_mut().zip(std::iter::once(&first).chain(rest)) {
            *d = v;
        }
        f64::from_be_bytes(b) / 4.0
    };
    let v = match t & 0x70 {
        0x20 | 0x30 => (double(t.wrapping_add(0x10), s.get(at + 1..at + 8)?), 8),
        0x40 | 0x50 => (double(t.wrapping_sub(0x10), s.get(at + 1..at + 4)?), 4),
        _ => match t {
            0x00 | 0x80 => (0.0, 1),
            0x01 => (1.0, 1),
            0x81 => (-1.0, 1),
            _ => return None,
        },
    };
    (v.0.is_finite()).then_some((if v.0.abs() < 1e-9 { 0.0 } else { v.0 }, v.1))
}

type Placement = ([[f64; 3]; 3], [f64; 3]);

/// One record of 9 numbers with an orthonormal rotation, and its end.
fn record(s: &[u8], at: usize) -> Option<(Placement, usize)> {
    let mut v = [0.0f64; 9];
    let mut p = at;
    for slot in &mut v {
        let (x, n) = number(s, p)?;
        *slot = x;
        p += n;
    }
    let [a0, a1, a2, b0, b1, b2, x, y, z] = v;
    let (r0, r1) = ([a0, a1, a2], [b0, b1, b2]);
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let unit = |a| (dot(a, a) - 1.0).abs() < 1e-3;
    if !(unit(r0)
        && unit(r1)
        && dot(r0, r1).abs() < 1e-3
        && [x, y, z].iter().all(|c| c.abs() < 1e9))
    {
        return None;
    }
    let r2 = [a1 * b2 - a2 * b1, a2 * b0 - a0 * b2, a0 * b1 - a1 * b0];
    Some((([r0, r1, r2], [x, y, z]), p))
}

/// The placement records (`01 <count>` + count numbers) in `start..end`
/// and where they end.
fn find_records(s: &[u8], start: usize, end: usize) -> Option<(Vec<Placement>, usize)> {
    for p in start..end {
        if s.get(p) != Some(&0x01) {
            continue;
        }
        let Some((count, n)) = varint(s, p + 1) else {
            continue;
        };
        // usually the count itself, sometimes stored one higher
        for numbers in [count, count.wrapping_sub(1)] {
            if numbers == 0 || numbers % 9 != 0 || numbers / 9 > MAX_INSTANCES {
                continue;
            }
            let mut out = Vec::new();
            let mut q = p + 1 + n;
            while out.len() < numbers / 9 {
                let Some((rec, next)) = record(s, q) else {
                    break;
                };
                out.push(rec);
                q = next;
            }
            if out.len() == numbers / 9 && q <= end {
                return Some((out, q));
            }
        }
    }
    None
}

/// The component names and the 1-based name index of every instance.
fn find_names(s: &[u8], start: usize, end: usize) -> Option<(Vec<String>, Vec<usize>)> {
    'candidates: for p in start..end {
        if s.get(p) != Some(&0x01) {
            continue;
        }
        let Some((m, n)) = varint(s, p + 1) else {
            continue;
        };
        let m = m.wrapping_sub(1);
        if m == 0 || m > MAX_INSTANCES || s.get(p + 1 + n) != Some(&0x04) {
            continue;
        }
        let mut q = p + 1 + n;
        let mut names = Vec::with_capacity(m.min(1024));
        for _ in 0..m {
            if s.get(q) != Some(&0x04) {
                continue 'candidates;
            }
            let Some((len, k)) = varint(s, q + 1) else {
                continue 'candidates;
            };
            let Some(len) = len.checked_sub(2) else {
                continue 'candidates;
            };
            let text_at = q + 1 + k;
            let (Some(text), Some(&0)) = (s.get(text_at..text_at + len), s.get(text_at + len))
            else {
                continue 'candidates;
            };
            let Ok(text) = std::str::from_utf8(text) else {
                continue 'candidates;
            };
            if text.is_empty() || text.chars().any(char::is_control) {
                continue 'candidates;
            }
            names.push(text.to_string());
            q = text_at + len + 1;
        }
        if s.get(q) != Some(&0x01) {
            continue;
        }
        let Some((count, k)) = varint(s, q + 1) else {
            continue;
        };
        let count = count.wrapping_sub(1);
        if count == 0 || count > MAX_INSTANCES {
            continue;
        }
        let mut r = q + 1 + k;
        let mut index = Vec::with_capacity(count);
        for _ in 0..count {
            let Some((v, k)) = varint(s, r) else {
                continue 'candidates;
            };
            if v == 0 || v > m {
                continue 'candidates;
            }
            index.push(v);
            r += k;
        }
        return Some((names, index));
    }
    None
}

/// The instance → record map among the objects: `Some(record)` per
/// instance, `None` for an instance without a record.
fn record_map(
    s: &[u8],
    objects: &[(usize, usize)],
    instances: usize,
    records: usize,
) -> Option<Vec<Option<usize>>> {
    // Instances at the origin may be missing from the map, and instances
    // with the same placement may share one record: the map is the one
    // whose keys are instances and whose values use every record.
    for &(start, end) in objects {
        let Some(map) = int_map(s, start, end) else {
            continue;
        };
        if map.len() > instances || map.len() < records {
            continue;
        }
        let mut out = vec![None; instances];
        let mut seen = vec![false; records];
        let mut ok = true;
        for (key, value) in map {
            let Some(slot) = out.get_mut(key) else {
                ok = false;
                break;
            };
            let Some(value) = value else { continue };
            match seen.get_mut(value) {
                Some(used) => {
                    *used = true;
                    *slot = Some(value);
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && seen.iter().all(|&used| used) {
            return Some(out);
        }
    }
    None
}

/// An int → int hash map object: (key, value) pairs of the used slots;
/// a stored 0 (= −1) value is `None`.
fn int_map(s: &[u8], start: usize, end: usize) -> Option<Vec<(usize, Option<usize>)>> {
    let mut p = start;
    let next = |p: &mut usize| -> Option<usize> {
        let (v, n) = varint(s, *p)?;
        *p += n;
        (*p <= end).then_some(v)
    };
    let n = next(&mut p)?.checked_sub(1)?;
    if s.get(p) != Some(&0x01) {
        return None;
    }
    p += 1;
    let cap = next(&mut p)?.checked_sub(1)?;
    if !cap.is_power_of_two() || cap > 1 << 20 || n > cap {
        return None;
    }
    let mut keys = Vec::with_capacity(cap);
    for _ in 0..cap {
        keys.push(next(&mut p)?);
    }
    if s.get(p) != Some(&0x01) {
        return None;
    }
    p += 1;
    let words = next(&mut p)?.checked_sub(1)?;
    if words != cap.div_ceil(16) {
        return None;
    }
    let mut state = Vec::with_capacity(words);
    for _ in 0..words {
        state.push(next(&mut p)?);
    }
    if s.get(p) != Some(&0x01) {
        return None;
    }
    p += 1;
    if next(&mut p)?.checked_sub(1)? != cap {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for (slot, key) in keys.iter().enumerate() {
        let value = next(&mut p)?;
        let word = state.get(slot / 16)?;
        if (word >> (2 * (slot % 16))) & 3 == 2 {
            out.push((key.checked_sub(1)?, value.checked_sub(1)));
        }
    }
    (out.len() == n).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/cad/nist/mtc-assembly/nx")
                .join(name),
        )
        .unwrap()
    }

    /// Checked against the JT export of the same assembly (`NIST mtc
    /// crada assembly.jt`): every instance, its part and its placement.
    #[test]
    fn the_nist_assembly_places_its_36_components() {
        let c = components(&fixture("NIST mtc crada assembly.prt")).unwrap();
        assert_eq!(c.len(), 36);
        let count = |n: &str| c.iter().filter(|x| x.name.ends_with(n)).count();
        assert_eq!(count("HEX NUT"), 10);
        assert_eq!(count("91274A118 SHCS"), 14);
        assert_eq!(count("91274A141 SHCS"), 4);
        assert_eq!(count("FHSCS"), 4);
        for single in ["BOX", "COVER", "PLATE", "POLYCARB"] {
            assert_eq!(count(single), 1, "{single}");
        }
        // the first nut (NX tag 2035) – values of the JT export
        let nut = c.first().unwrap();
        assert!(nut.name.ends_with("HEX NUT"));
        let close = |a: f64, b: f64| (a - b).abs() < 1e-4;
        assert!(close(nut.translation[0], 57.350284) && close(nut.translation[1], 75.387787));
        assert!(close(nut.translation[2], 88.875));
        assert_eq!(
            nut.rotation[2],
            [0.0, 0.0, -1.0],
            "third row = row 0 × row 1"
        );
        // the box has no record: at the origin
        let bx = c.iter().find(|x| x.name.ends_with("BOX")).unwrap();
        assert_eq!(bx.translation, [0.0; 3]);
        // every rotation is orthonormal
        for x in &c {
            for row in x.rotation {
                assert!(close(row.iter().map(|v| v * v).sum(), 1.0), "{x:?}");
            }
        }
    }

    #[test]
    fn parts_and_garbage_have_no_structure() {
        assert!(components(&fixture("90591A141 HEX NUT.prt"))
            .is_none_or(|c| c.is_empty() || c.len() < 2));
        assert!(components(b"SPLMSSTR").is_none());
        assert!(structure(&[0; 64]).is_none());
        let mut s = vec![0u8; 0x0c];
        s.extend(b"OM");
        s.resize(0x30, 0);
        assert!(structure(&s).is_none());
    }

    #[test]
    fn damaged_structures_never_panic() {
        let data = fixture("NIST mtc crada assembly.prt");
        let streams = crate::nxpart::streams(&data);
        let st = streams.iter().find(|s| s.name == STRUCTURE_STREAM).unwrap();
        let s = crate::nxpart::data(&data, st).unwrap().to_vec();
        for at in (0..s.len()).step_by(7) {
            let mut copy = s.clone();
            if let Some(b) = copy.get_mut(at) {
                *b ^= 0xA5;
            }
            let _ = structure(&copy);
            let _ = structure(s.get(..at).unwrap_or_default());
        }
    }

    #[test]
    fn numbers_and_placements() {
        assert_eq!(
            number(&[0x30, 0x77, 0xc3, 0x33, 0x33, 0x33, 0x33, 0x34], 0).map(|v| v.1),
            Some(8)
        );
        let (v, _) = number(&[0x30, 0x77, 0xc3, 0x33, 0x33, 0x33, 0x33, 0x34], 0).unwrap();
        assert!((v - 95.05).abs() < 1e-9, "380.2 / 4");
        assert_eq!(number(&[0x81], 0), Some((-1.0, 1)));
        assert_eq!(varint(&[0x87, 0xf0], 0), Some((0x7f0, 2)));
        let quarter = Component {
            name: "A".into(),
            rotation: [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            translation: [10.0, 0.0, 0.0],
        };
        assert_eq!(quarter.place([1.0, 0.0, 0.0]), [10.0, 1.0, 0.0]);
        let nested = quarter.within(&quarter);
        assert_eq!(
            nested.place([1.0, 0.0, 0.0]),
            quarter.place(quarter.place([1.0, 0.0, 0.0]))
        );
    }
}
