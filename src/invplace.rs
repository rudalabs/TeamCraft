//! Autodesk Inventor assemblies (`.iam`): which files are placed where (see
//! `docs/src/research/inventor-display-preview.md`).
//!
//! * The stream `UFRxDoc` holds the table of referenced files (the author's
//!   absolute path and a file id per file) and one record per occurrence:
//!
//!   ```text
//!   u32 0 | u32 file id | u32 occurrence id | u32 number | u32 instance
//!   | u32 n, n × UTF-16 title | 6 × 00, 0x80 | flags (0x40: not visible) | …
//!   ```
//!
//!   The records are followed by property blocks whose layout is not known,
//!   so they are found by scanning for exactly this shape with known ids.
//!   Each block holds the occurrence's name record `01 05 01 00 00 00 05`
//!   (u32 n, n × UTF-16) and, 31 bytes after its end, the record
//!   `01 07 01 00 00 00 07` whose byte is 1 for a suppressed occurrence
//!   (the layout `invasm` reads too; at that place in 2 564 of 2 565
//!   occurrences of the sample set). Suppressed occurrences are left out.
//! * The segment `AmRxSegment` holds one section of class `232792BC` per
//!   occurrence: `u32 0, u16, u32 ref, [u32 0x203]`, a packed 4 × 4 matrix
//!   and a 124-byte tail with the occurrence id at offset 112 and its
//!   persistent key at offset 120. The matrix
//!   maps component to assembly coordinates as `p' = R·p + t` (column
//!   vectors, centimetres). Its mask has two bits per element (low half:
//!   one, high half: zero; both: minus one); elements without a bit follow
//!   as `f64`.
//!
//! * Flexible sub-assemblies are re-posed by their parent: a section of class
//!   `F17A4562` (`u32 0, u16`, an array of occurrence ids from this assembly
//!   down to the sub-assembly, an array of section refs `(index + 1) | 1 << 31`)
//!   points to sections of class `2238DCC5` (`u32 0, u16, u32 key, u32 kind,
//!   [u32 0x203]`, a packed matrix, 120 bytes): kind 2 replaces the local
//!   placement of the child with that key. Kind 1 (positional
//!   representations, empty path) is not applied.
//!
//! Sub-assemblies are files of their own; `services::assemblies` resolves
//! them and composes the placements, applying the overrides on the way down
//! ([`Override::pose`], [`Override::passed_down`]).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek};
use std::path::Path;

use crate::invpart::{self, f64_at, u16_at, u32_at};
use crate::nxasm::{Component, Kind};

/// The segment with the occurrence placements.
pub const PLACEMENTS: &str = "AmRxSegment";
/// Occurrence placement section.
const OCCURRENCE: u32 = 0x2327_92BC;
/// A set of overrides: a path of occurrence ids and refs to overrides.
const OVERRIDE_SET: u32 = 0xF17A_4562;
/// One override: child key, kind, placement.
const OVERRIDE: u32 = 0x2238_DCC5;
/// The override kind that re-poses a child of a flexible sub-assembly.
const REPOSE: u32 = 2;
/// The name record and the suppression record of an occurrence's block.
const NAME_RECORD: [u8; 7] = [1, 5, 1, 0, 0, 0, 5];
const SUPPRESSION_RECORD: [u8; 7] = [1, 7, 1, 0, 0, 0, 7];
/// Bytes from the end of the name record to the suppression record.
const SUPPRESSION_AFTER_NAME: usize = 31;
/// Longest property block searched for the records.
const MAX_BLOCK: usize = 4096;
/// Length of the tail after the matrix.
const TAIL: usize = 124;
/// Largest `UFRxDoc` read (the biggest seen: 300 KiB).
const MAX_DOC: u64 = 16 * 1024 * 1024;
/// Largest placement segment inflated.
const MAX_SEGMENT: u64 = 256 * 1024 * 1024;
/// The file table starts within this many bytes (seen: about 2 KiB).
const TABLE_WITHIN: usize = 64 * 1024;
/// Longest text read (UTF-16 code units).
const MAX_TEXT: usize = 4096;
/// Inventor lengths are centimetres.
const TO_MM: f64 = 10.0;

/// A referenced file of the table.
#[derive(Clone, Debug, PartialEq)]
struct FileRef {
    id: u32,
    path: String,
}

/// An occurrence record of `UFRxDoc`.
#[derive(Clone, Debug, PartialEq)]
struct Record {
    file: u32,
    id: u32,
    visible: bool,
    suppressed: bool,
    /// Where the record starts and where its header ends.
    start: usize,
    end: usize,
}

/// A placed occurrence of an Inventor assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    /// The occurrence id (paths of overrides use it).
    pub id: u32,
    /// The persistent key (overrides of a parent name the child by it).
    pub key: u32,
    /// Poses for components below this one, paths relative to its children.
    pub overrides: Vec<Override>,
}

/// A pose a parent assembly gives a component further down.
#[derive(Clone, Debug, PartialEq)]
pub struct Override {
    /// Occurrence ids from the current level down to the sub-assembly whose
    /// child is re-posed; empty: a child of the current level.
    pub path: Vec<u32>,
    /// The key of the re-posed child.
    pub key: u32,
    /// Rotation (row convention) and translation (mm) replacing the child's.
    pub rotation: [[f64; 3]; 3],
    pub translation: [f64; 3],
}

impl Override {
    /// The local placement of `child` with the first override for it (the
    /// outermost assembly's, as they come first).
    pub fn pose(overrides: &[Override], child: &Component) -> Component {
        let key = child.occurrence.as_ref().map(|o| o.key);
        match overrides
            .iter()
            .find(|o| o.path.is_empty() && Some(o.key) == key)
        {
            Some(o) => Component {
                rotation: o.rotation,
                translation: o.translation,
                ..child.clone()
            },
            None => child.clone(),
        }
    }

    /// The overrides for the children of the sub-assembly `child`: those of
    /// the levels above that lead through it, then its own.
    pub fn passed_down(overrides: &[Override], child: &Component) -> Vec<Override> {
        let Some(occurrence) = child.occurrence.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<Override> = overrides
            .iter()
            .filter(|o| o.path.first() == Some(&occurrence.id))
            .map(|o| Override {
                path: o.path.get(1..).unwrap_or_default().to_vec(),
                ..o.clone()
            })
            .collect();
        out.extend(occurrence.overrides.iter().cloned());
        out
    }
}

/// The placement of an occurrence: matrix and persistent key.
struct Placement {
    matrix: [[f64; 4]; 4],
    key: u32,
}

/// What an Inventor file is for the preview: an assembly with its visible
/// components (file name and placement), or something else.
pub fn kind_of(path: &Path) -> io::Result<Kind> {
    let file = io::BufReader::new(std::fs::File::open(path)?);
    let Ok(mut ole) = cfb::CompoundFile::open(file) else {
        return Ok(Kind::Other);
    };
    Ok(match components_in(&mut ole)? {
        Some(c) if !c.is_empty() => Kind::Assembly(c),
        _ => Kind::Other,
    })
}

/// The visible components of an assembly (bytes of the whole file);
/// `None` when it is no Inventor assembly.
pub fn components(data: &[u8]) -> Option<Vec<Component>> {
    let mut ole = cfb::CompoundFile::open(io::Cursor::new(data)).ok()?;
    components_in(&mut ole).ok().flatten()
}

fn components_in<F: Read + Seek>(
    ole: &mut cfb::CompoundFile<F>,
) -> io::Result<Option<Vec<Component>>> {
    let Some(segment) = invpart::segment(ole, PLACEMENTS, MAX_SEGMENT)? else {
        return Ok(None);
    };
    let (placements, overrides) = placements(&segment)?;
    let Ok(doc) = invpart::read_stream(ole, Path::new("/UFRxDoc"), MAX_DOC) else {
        return Ok(None);
    };
    let Some((files, end)) = file_table(&doc) else {
        return Ok(None);
    };
    let known: HashSet<u32> = placements.keys().copied().collect();
    let occurrences = occurrences(&doc, end, &files, &known);
    let names: HashMap<u32, &str> = files.iter().map(|f| (f.id, file_name(&f.path))).collect();
    let mut out = Vec::new();
    for o in occurrences {
        let (Some(name), Some(p)) = (names.get(&o.file), placements.get(&o.id)) else {
            continue;
        };
        if !o.visible || o.suppressed || name.is_empty() {
            continue;
        }
        let mut c = component(name, &p.matrix);
        c.occurrence = Some(Occurrence {
            id: o.id,
            key: p.key,
            overrides: overrides
                .iter()
                .filter(|v| v.path.first() == Some(&o.id))
                .map(|v| Override {
                    path: v.path.get(1..).unwrap_or_default().to_vec(),
                    ..v.clone()
                })
                .collect(),
        });
        out.push(c);
    }
    Ok(Some(out))
}

/// The file name of a stored (Windows or Unix) path.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// A `Component` (row-vector convention, mm) from an Inventor matrix.
fn component(name: &str, m: &[[f64; 4]; 4]) -> Component {
    let (rotation, translation) = pose(m);
    Component {
        name: name.to_string(),
        rotation,
        translation,
        layer: None,
        occurrence: None,
    }
}

/// Rotation rows and translation (mm) of an Inventor matrix.
fn pose(m: &[[f64; 4]; 4]) -> ([[f64; 3]; 3], [f64; 3]) {
    let mut rotation = [[0.0; 3]; 3];
    for (i, row) in rotation.iter_mut().enumerate() {
        for (k, r) in row.iter_mut().enumerate() {
            // p'_k = sum_i p_i rotation[i][k] = sum_i R[k][i] p_i
            *r = m
                .get(k)
                .and_then(|mrow| mrow.get(i))
                .copied()
                .unwrap_or(0.0);
        }
    }
    let t = |r: usize| m.get(r).and_then(|row| row.get(3)).copied().unwrap_or(0.0) * TO_MM;
    (rotation, [t(0), t(1), t(2)])
}

/// Placements by occurrence id, and overrides by path from this assembly.
type Placements = (HashMap<u32, Placement>, Vec<Override>);

/// The placement of every occurrence id of the segment, and the overrides
/// it gives components further down (paths from this assembly).
fn placements(segment: &invpart::Segment) -> io::Result<Placements> {
    let mut out = HashMap::new();
    // section index -> (child key, kind, matrix)
    let mut poses: HashMap<usize, (u32, u32, [[f64; 4]; 4])> = HashMap::new();
    let mut sets: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
    for section in segment.sections() {
        let s = section?;
        let p = s.payload;
        if s.class == OVERRIDE {
            let (Some(key), Some(kind)) = (u32_at(p, 6), u32_at(p, 10)) else {
                continue;
            };
            let at = if u32_at(p, 14) == Some(0x203) { 18 } else { 14 };
            if let Some((m, _)) = matrix(p, at) {
                poses.insert(s.index, (key, kind, m));
            }
            continue;
        }
        if s.class == OVERRIDE_SET {
            let mut c = invpart::Cursor::new(p, 6);
            let (Some(path), Some(refs)) = (c.array(segment), c.array(segment)) else {
                continue;
            };
            let words = |a: invpart::Array| -> Vec<u32> {
                a.bytes
                    .chunks_exact(4)
                    .filter_map(|w| w.try_into().ok().map(u32::from_le_bytes))
                    .collect()
            };
            sets.push((words(path), words(refs)));
            continue;
        }
        if s.class != OCCURRENCE {
            continue;
        }
        let mut at = 10;
        if u32_at(p, at) == Some(0x203) {
            at += 4;
        }
        let Some((m, end)) = matrix(p, at) else {
            continue;
        };
        if p.len().checked_sub(end) != Some(TAIL) {
            continue;
        }
        if let (Some(id), Some(key)) = (u32_at(p, end + 112), u32_at(p, end + 120)) {
            out.insert(id, Placement { matrix: m, key });
        }
    }
    let mut overrides = Vec::new();
    for (path, refs) in sets {
        if path.is_empty() {
            continue;
        }
        for r in refs {
            let index = usize::try_from(r & 0x7FFF_FFFF)
                .ok()
                .and_then(|i| i.checked_sub(1));
            let Some(&(key, kind, m)) = index.and_then(|i| poses.get(&i)) else {
                continue;
            };
            if kind != REPOSE {
                continue;
            }
            let (rotation, translation) = pose(&m);
            overrides.push(Override {
                path: path.clone(),
                key,
                rotation,
                translation,
            });
        }
    }
    Ok((out, overrides))
}

/// A packed matrix at `at` and where it ends.
fn matrix(p: &[u8], at: usize) -> Option<([[f64; 4]; 4], usize)> {
    let mask = u32_at(p, at)?;
    let mut m = [[0.0; 4]; 4];
    let mut next = at + 4;
    for k in 0..16 {
        let one = mask >> k & 1 == 1;
        let zero = mask >> (16 + k) & 1 == 1;
        let v = match (one, zero) {
            (true, true) => -1.0,
            (true, false) => 1.0,
            (false, true) => 0.0,
            (false, false) => {
                let v = f64_at(p, next)?;
                next += 8;
                v
            }
        };
        if !v.is_finite() {
            return None;
        }
        *m.get_mut(k / 4)?.get_mut(k % 4)? = v;
    }
    (m.get(3) == Some(&[0.0, 0.0, 0.0, 1.0])).then_some((m, next))
}

/// UTF-16 text with a `u32` length; no control characters.
fn text(d: &[u8], at: usize, max: usize) -> Option<(String, usize)> {
    let n = usize::try_from(u32_at(d, at)?).ok()?;
    if n > max {
        return None;
    }
    let bytes = d.get(at + 4..at + 4 + 2 * n)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .filter_map(|c| c.try_into().ok().map(u16::from_le_bytes))
        .collect();
    let s = String::from_utf16(&units).ok()?;
    if s.chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return None;
    }
    Some((s, at + 4 + 2 * n))
}

/// One record of the file table at `at` and where it ends.
fn file_record(d: &[u8], at: usize) -> Option<(FileRef, usize)> {
    let (path, at) = text(d, at, MAX_TEXT)?;
    let at = at + 4; // library id
    let (_, at) = text(d, at, MAX_TEXT)?; // library name
    let mut at = at + 2;
    if u32_at(d, at)? >> 16 != 0 {
        if u16_at(d, at)? != 0 {
            return None;
        }
        at += 2;
    }
    let (_, at) = text(d, at, MAX_TEXT)?; // display name
    let count = usize::try_from(u32_at(d, at)?).ok()?;
    if count > 10_000 {
        return None;
    }
    let at = at + 4 + 6 * count + 4 + 32;
    let id = u32_at(d, at)?;
    let flags = u32_at(d, at + 12)?;
    let mut end = at + 16;
    if flags & 0x20 != 0 {
        // an iPart / iAssembly member: its name and the factory's file id
        let (_, e) = text(d, end, MAX_TEXT)?;
        end = e + 4;
    }
    (end <= d.len()).then_some((FileRef { id, path }, end))
}

/// The referenced-file table: `u32 n, text caption, u32, n records`. It is
/// found by trying the offsets near the start of the stream.
fn file_table(d: &[u8]) -> Option<(Vec<FileRef>, usize)> {
    let last = d.len().min(TABLE_WITHIN);
    'offsets: for start in 0x40..last {
        let n = usize::try_from(u32_at(d, start)?).ok()?;
        // a record takes at least 60 bytes
        if n == 0 || n > d.len() / 60 {
            continue;
        }
        let Some((_, at)) = text(d, start + 4, 256) else {
            continue;
        };
        let mut at = at + 4;
        match u32_at(d, at) {
            Some(len) if (3..2048).contains(&len) => {}
            _ => continue,
        }
        let mut files = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            let Some((f, end)) = file_record(d, at) else {
                continue 'offsets;
            };
            if !f
                .path
                .rsplit(['\\', '/'])
                .next()
                .is_some_and(|name| name.contains('.'))
            {
                continue 'offsets;
            }
            files.push(f);
            at = end;
        }
        return Some((files, at));
    }
    None
}

/// The occurrence records after the file table whose ids have a placement.
fn occurrences(d: &[u8], from: usize, files: &[FileRef], placed: &HashSet<u32>) -> Vec<Record> {
    let ids: HashSet<u32> = files.iter().map(|f| f.id).collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut at = from;
    while at + 28 < d.len() && seen.len() < placed.len() {
        match occurrence_at(d, at, &ids, placed) {
            Some(o) if !seen.contains(&o.id) => {
                seen.insert(o.id);
                at = o.end;
                out.push(o);
            }
            _ => at += 1,
        }
    }
    // the property block of a record ends where the next record starts
    let ends: Vec<usize> = out
        .iter()
        .skip(1)
        .map(|o| o.start)
        .chain(std::iter::once(d.len()))
        .collect();
    for (o, &stop) in out.iter_mut().zip(&ends) {
        o.suppressed = suppressed(d, o.end, stop);
    }
    out
}

/// Whether the property block `from..to` marks its occurrence suppressed:
/// the suppression record 31 bytes after the end of the first name record,
/// with the byte 1. Anything else counts as not suppressed.
fn suppressed(d: &[u8], from: usize, to: usize) -> bool {
    let Some(block) = d.get(from..to.min(from.saturating_add(MAX_BLOCK))) else {
        return false;
    };
    let Some(at) = block
        .windows(NAME_RECORD.len())
        .position(|w| w == NAME_RECORD)
    else {
        return false;
    };
    let Some(n) = u32_at(block, at + 7).and_then(|n| usize::try_from(n).ok()) else {
        return false;
    };
    let Some(record) = n
        .checked_mul(2)
        .and_then(|b| b.checked_add(at + 11 + SUPPRESSION_AFTER_NAME))
    else {
        return false;
    };
    block.get(record..record + 7) == Some(&SUPPRESSION_RECORD[..])
        && block.get(record + 7) == Some(&1)
}

fn occurrence_at(
    d: &[u8],
    at: usize,
    files: &HashSet<u32>,
    placed: &HashSet<u32>,
) -> Option<Record> {
    let v = |k: usize| u32_at(d, at + 4 * k);
    let (zero, file, id, number, instance) = (v(0)?, v(1)?, v(2)?, v(3)?, v(4)?);
    if zero != 0
        || !placed.contains(&id)
        || (file != 0 && !files.contains(&file))
        || !(1..=0xFFFF).contains(&number)
        || !(1..=0xFFFF).contains(&instance)
    {
        return None;
    }
    let (title, end) = text(d, at + 20, 1024)?;
    if file == 0 && !title.ends_with(&format!(":{number}")) {
        return None;
    }
    let flags = d.get(end..end + 8)?;
    if flags.get(..6)? != [0; 6] || flags.get(6)? & 0x80 == 0 {
        return None;
    }
    let visible = flags.get(7)? & 0x40 == 0;
    Some(Record {
        file,
        id,
        visible,
        suppressed: false,
        start: at,
        end: end + 8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invpart::tests::{array, file};

    fn text16(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = (units.len() as u32).to_le_bytes().to_vec();
        for u in units {
            out.extend(u.to_le_bytes());
        }
        out
    }

    fn file_rec(id: u32, path: &str) -> Vec<u8> {
        let mut r = text16(path);
        r.extend(113u32.to_le_bytes());
        r.extend(text16("Workspace"));
        r.extend(0u16.to_le_bytes());
        r.extend(text16(""));
        r.extend(0u32.to_le_bytes()); // n × 6 bytes
        r.extend(0u32.to_le_bytes()); // 2 × u16
        r.extend([0x11; 32]); // two GUIDs
        r.extend(id.to_le_bytes());
        r.extend(1u32.to_le_bytes());
        r.extend(0x23u32.to_le_bytes());
        r.extend(1u32.to_le_bytes());
        r
    }

    fn occ_rec(
        file: u32,
        id: u32,
        number: u32,
        title: &str,
        visible: bool,
        suppressed: bool,
    ) -> Vec<u8> {
        let mut r = vec![0; 4];
        for v in [file, id, number, 1] {
            r.extend(v.to_le_bytes());
        }
        r.extend(text16(title));
        r.extend([0, 0, 0, 0, 0, 0, 0x80, if visible { 0x20 } else { 0x60 }]);
        // property blocks: name record, identifier record, suppression record
        r.extend([0xEE; 37]);
        r.extend(NAME_RECORD);
        r.extend(text16(title.split(':').next().unwrap_or_default()));
        r.extend([0; 4]);
        r.extend([1, 2, 1, 0, 0, 0, 2]);
        r.extend([0x33; 20]);
        r.extend(SUPPRESSION_RECORD);
        r.push(u8::from(suppressed));
        r.extend([0xEE; 11]);
        r
    }

    /// `UFRxDoc` with the given files and occurrences.
    fn doc(files: &[(u32, &str)], occs: &[(u32, u32, &str, bool)]) -> Vec<u8> {
        doc_with(files, occs, &[])
    }

    /// `UFRxDoc` whose occurrences `suppressed` are suppressed.
    fn doc_with(
        files: &[(u32, &str)],
        occs: &[(u32, u32, &str, bool)],
        suppressed: &[u32],
    ) -> Vec<u8> {
        let mut d = vec![0x55; 0x50];
        d.extend((files.len() as u32).to_le_bytes());
        d.extend(text16(""));
        d.extend(0u32.to_le_bytes());
        for (id, path) in files {
            d.extend(file_rec(*id, path));
        }
        d.extend([0, 0, 0, 0, 0, 0]);
        d.extend((occs.len() as u32).to_le_bytes());
        for (k, (file, id, title, visible)) in occs.iter().enumerate() {
            d.extend(occ_rec(
                *file,
                *id,
                k as u32 + 1,
                title,
                *visible,
                suppressed.contains(id),
            ));
        }
        d.extend([0; 29]);
        d
    }

    /// A placement section for `id`: rotation about z by 90° and `t` (cm).
    fn placement(id: u32, t: [f64; 3], first: bool) -> Vec<u8> {
        let mut p = vec![0u8; 10];
        if first {
            p.extend(0x203u32.to_le_bytes());
        }
        // R = [[0,-1,0],[1,0,0],[0,0,1]]: elements 0,5 zero; 1 minus one;
        // 4 one; 10 one; 2,6,8,9 zero; 3,7,11 stored; 12..14 zero; 15 one
        let one: u32 = 1 << 1 | 1 << 4 | 1 << 10 | 1 << 15;
        let zero: u32 = 1 << 0
            | 1 << 1
            | 1 << 2
            | 1 << 5
            | 1 << 6
            | 1 << 8
            | 1 << 9
            | 1 << 12
            | 1 << 13
            | 1 << 14;
        p.extend((one | zero << 16).to_le_bytes());
        for v in t {
            p.extend(v.to_le_bytes());
        }
        let mut tail = vec![0u8; TAIL];
        tail[112..116].copy_from_slice(&id.to_le_bytes());
        tail[120..124].copy_from_slice(&(id + 100).to_le_bytes());
        p.extend(tail);
        p
    }

    /// A translation only (cm) as a packed matrix.
    fn shift(t: [f64; 3]) -> Vec<u8> {
        let one: u32 = 1 | 1 << 5 | 1 << 10 | 1 << 15;
        let zero: u32 = [1, 2, 4, 6, 8, 9, 12, 13, 14]
            .iter()
            .map(|b| 1u32 << b)
            .sum();
        let mut p = (one | zero << 16).to_le_bytes().to_vec();
        for v in t {
            p.extend(v.to_le_bytes());
        }
        p
    }

    /// An override section: child `key`, `kind`, translation (cm).
    fn override_pose(key: u32, kind: u32, t: [f64; 3], first: bool) -> Vec<u8> {
        let mut p = vec![0u8; 6];
        p.extend(key.to_le_bytes());
        p.extend(kind.to_le_bytes());
        if first {
            p.extend(0x203u32.to_le_bytes());
        }
        p.extend(shift(t));
        p.extend([0u8; 120]);
        p
    }

    /// An override set: a path of occurrence ids and section refs.
    fn override_set(path: &[u32], refs: &[usize]) -> Vec<u8> {
        let words = |v: Vec<u32>| v.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let mut p = vec![0u8; 6];
        p.extend(array(0, path.len(), &words(path.to_vec())));
        let refs: Vec<u32> = refs.iter().map(|&i| (i as u32 + 1) | 0x8000_0000).collect();
        p.extend(array(0, refs.len(), &words(refs.clone())));
        p
    }

    fn assembly(placements: Vec<(u8, Vec<u8>)>, doc: Vec<u8>) -> Vec<u8> {
        file(
            &[(
                PLACEMENTS,
                &[OCCURRENCE, 0xFDCD_32FE, OVERRIDE_SET, OVERRIDE],
                placements,
            )],
            &[("/UFRxDoc", doc)],
        )
    }

    #[test]
    fn suppressed_occurrences_are_left_out_and_overrides_go_down() {
        // sections: 0 placement Sub, 1 placement Bolt, 2 placement Bolt,
        // 3 override (key 777, kind 2), 4 override (kind 1), 5 set [1] -> 3, 4
        let bytes = assembly(
            vec![
                (0, placement(1, [0.0; 3], true)),
                (0, placement(2, [1.0, 0.0, 0.0], false)),
                (0, placement(3, [2.0, 0.0, 0.0], false)),
                (3, override_pose(777, 2, [0.0, 0.0, 9.0], true)),
                (3, override_pose(778, 1, [5.0, 5.0, 5.0], false)),
                (2, override_set(&[1], &[3, 4])),
            ],
            doc_with(
                &[(1, r"C:ASub.iam"), (2, r"C:ABolt.ipt")],
                &[
                    (1, 1, "Sub:1", true),
                    (2, 2, "Bolt:1", true),
                    (2, 3, "Bolt:2", true),
                ],
                &[3],
            ),
        );
        let c = components(&bytes).unwrap();
        assert_eq!(c.len(), 2, "the suppressed Bolt:2 is left out");
        let sub = &c[0];
        let occurrence = sub.occurrence.as_ref().unwrap();
        assert_eq!((occurrence.id, occurrence.key), (1, 101));
        assert_eq!(occurrence.overrides.len(), 1, "kind 1 is not applied");
        assert_eq!(occurrence.overrides[0].path, Vec::<u32>::new());
        assert_eq!(occurrence.overrides[0].translation, [0.0, 0.0, 90.0]);
        assert!(c[1].occurrence.as_ref().unwrap().overrides.is_empty());

        // a child of Sub with key 777 takes the pose Sub's parent gives it
        let mut child = component(
            "Pin.ipt",
            &[
                [1., 0., 0., 3.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        );
        child.occurrence = Some(Occurrence {
            id: 5,
            key: 777,
            overrides: Vec::new(),
        });
        let below = Override::passed_down(&[], sub);
        assert_eq!(Override::pose(&below, &child).translation, [0.0, 0.0, 90.0]);
        // other children keep their own placement
        child.occurrence = Some(Occurrence {
            id: 6,
            key: 9,
            overrides: Vec::new(),
        });
        assert_eq!(Override::pose(&below, &child).translation, [30.0, 0.0, 0.0]);
        // an override two levels down passes through Sub when the path leads there
        let deep = vec![Override {
            path: vec![1, 5],
            ..occurrence.overrides[0].clone()
        }];
        let through = Override::passed_down(&deep, sub);
        assert_eq!(through[0].path, vec![5]);
        assert!(Override::passed_down(&deep, &c[1]).is_empty());
    }

    #[test]
    fn components_carry_file_names_and_placements_in_millimetres() {
        let bytes = assembly(
            vec![
                (0, placement(1, [1.0, 2.0, 3.0], true)),
                (1, vec![1, 2, 3]),
                (0, placement(2, [0.0, 0.0, 0.0], false)),
                (0, placement(3, [5.0, 0.0, 0.0], false)),
            ],
            doc(
                &[
                    (1, r"C:\Work\Pump\Shaft.ipt"),
                    (2, r"C:\Work\Pump\Sub\Gear Box.iam"),
                ],
                &[
                    (1, 1, "Shaft:1", true),
                    (2, 2, "", true),
                    (1, 3, "Shaft:2", false),
                ],
            ),
        );
        let c = components(&bytes).unwrap();
        assert_eq!(c.len(), 2, "the hidden Shaft:2 is left out");
        assert_eq!(c[0].name, "Shaft.ipt");
        assert_eq!(c[1].name, "Gear Box.iam");
        assert_eq!(c[0].translation, [10.0, 20.0, 30.0]);
        // R maps x to y: (1, 0, 0) lands at (0, 1, 0) + t
        let p = c[0].place([1.0, 0.0, 0.0]);
        assert!(
            (p[0] - 10.0).abs() < 1e-9 && (p[1] - 21.0).abs() < 1e-9 && (p[2] - 30.0).abs() < 1e-9,
            "{p:?}"
        );
    }

    #[test]
    fn identity_and_minus_one_elements() {
        let mut p = 0x7BDE_8421u32.to_le_bytes().to_vec();
        let (m, end) = matrix(&p, 0).unwrap();
        assert_eq!(end, 4);
        assert_eq!(
            m,
            [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.]
            ]
        );
        // a bottom row other than 0 0 0 1 is refused
        p = (0x7BDE_8421u32 & !(1 << 15) & !(1 << 31))
            .to_le_bytes()
            .to_vec();
        p.extend(2.0f64.to_le_bytes());
        assert!(matrix(&p, 0).is_none());
        // both bits: minus one
        let mirror = 0x7BDE_8421u32 | 1 << 16;
        assert_eq!(matrix(&mirror.to_le_bytes(), 0).unwrap().0[0][0], -1.0);
    }

    #[test]
    fn files_that_are_no_inventor_assemblies() {
        assert!(components(b"ISO-10303-21;").is_none());
        assert!(components(&[]).is_none());
        // a part: no placement segment
        let part = file(&[("PmGraphicsSegment", &[1], vec![])], &[]);
        assert!(components(&part).is_none());
        // placements but no readable file table
        let bytes = assembly(vec![(0, placement(1, [0.0; 3], true))], vec![0; 200]);
        assert!(components(&bytes).is_none());
    }

    #[test]
    fn damaged_assemblies_never_panic() {
        let bytes = assembly(
            vec![(0, placement(1, [1.0, 2.0, 3.0], true))],
            doc(&[(1, r"C:\A\Shaft.ipt")], &[(1, 1, "Shaft:1", true)]),
        );
        assert_eq!(components(&bytes).unwrap().len(), 1);
        let d = doc(&[(1, r"C:\A\Shaft.ipt")], &[(1, 1, "Shaft:1", true)]);
        for cut in 0..d.len() {
            if let Some((files, end)) = file_table(&d[..cut]) {
                let _ = occurrences(&d[..cut], end, &files, &HashSet::from([1]));
            }
        }
        let p = placement(1, [1.0, 2.0, 3.0], true);
        for cut in 0..p.len() {
            let _ = matrix(&p[..cut], 14);
        }
    }
}
