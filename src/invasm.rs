//! Autodesk Inventor assemblies (`.iam`): how often the assembly places each
//! file it references, read from the file descriptor table in the `UFRxDoc`
//! stream of the compound file.
//!
//! The format is not documented. This reader only relies on what was
//! verified with 32 real Inventor 2025–2027 assemblies of one project (390
//! occurrences, 190 referenced files; the counts compared with Inventor's own
//! API, see `docs/src/research/inventor-instance-counts.md`):
//!
//! ```text
//! u32 n, n × UTF-16LE   full path of the referenced file
//! u32                   kind of the path (0x43 workspace, 0x4d library, …)
//! u32 m, m × UTF-16LE   workspace or library ("Workspace", "Content Center Files")
//! u16                   flags
//! u32 k, k × UTF-16LE   display name (may be empty)
//! 6 × 0
//! 2 × 16 bytes          identifiers
//! u32
//! u16 0
//! u16                   occurrences of the file in this assembly
//! 10 bytes              the next descriptor follows
//! ```
//!
//! The stream also keeps copies of the table from earlier saves (with the
//! same or older paths); only the first table counts – the descriptors that
//! follow each other without a gap.
//!
//! The descriptor counts include suppressed occurrences. Inventor's BOM
//! leaves those out, and so does this reader: the same stream holds one
//! property record per occurrence, in occurrence order (again followed by
//! copies from earlier saves, so only the first records as many as the
//! descriptors count are read):
//!
//! ```text
//! 01 05 01 00 00 00 05  u32 n, n × UTF-16LE   occurrence name ("Bolt" of "Bolt:3")
//! 4 bytes
//! 01 02 01 00 00 00 02  16 bytes, 4 bytes     identifier
//! 01 07 01 00 00 00 07  u8                    1 = suppressed
//! ```
//!
//! The occurrence name is the descriptor's display name, or the file name
//! without extension when that is empty. Every suppressed record is taken
//! off the count of the one file whose name it carries; a file with only
//! suppressed occurrences counts 0 (still referenced, not in the BOM). When
//! the records do not add up – fewer records than occurrences, a suppressed
//! record whose name fits no file or several – the descriptor counts stay as
//! they are: suppression is not guessed.
//!
//! Anything that does not fit is skipped, never trusted: a file whose
//! descriptor is not found has no count (not known), it is never guessed.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::io::{self, Read, Seek};

/// The stream with the file descriptors.
pub const DESCRIPTOR_STREAM: &str = "/UFRxDoc";
/// Bytes between the end of one descriptor and the start of the next.
const TABLE_GAP: usize = 10;
/// Longest path and longest workspace or display name read, in UTF-16 units.
const MAX_PATH: usize = 1024;
const MAX_NAME: usize = 512;

fn u32_at(data: &[u8], i: usize) -> Option<usize> {
    let bytes: [u8; 4] = data.get(i..i + 4)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(bytes)).ok()
}

fn u16_at(data: &[u8], i: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(i..i + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

/// A UTF-16LE string with a `u32` unit count at `i` (`min..=max` units, no
/// control characters); the string and the offset after it.
fn text_at(data: &[u8], i: usize, min: usize, max: usize) -> Option<(String, usize)> {
    let n = u32_at(data, i)?;
    if !(min..=max).contains(&n) {
        return None;
    }
    let end = i.checked_add(4)?.checked_add(n.checked_mul(2)?)?;
    let units: Vec<u16> = data
        .get(i + 4..end)?
        .chunks_exact(2)
        .map(|p| {
            u16::from_le_bytes([
                p.first().copied().unwrap_or(0),
                p.get(1).copied().unwrap_or(0),
            ])
        })
        .collect();
    let text: String = char::decode_utf16(units).collect::<Result<_, _>>().ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some((text, end))
}

/// One file descriptor of the table.
struct Descriptor {
    /// File name (last part of the path).
    name: String,
    /// Display name, may be empty.
    display: String,
    /// Occurrences, suppressed ones included.
    count: u32,
    /// Offset after the descriptor.
    end: usize,
}

impl Descriptor {
    /// Does an occurrence record with this name place this file? Its name is
    /// the display name, or the file name without extension.
    fn is_named(&self, occurrence: &str) -> bool {
        let occurrence = occurrence.trim().to_lowercase();
        let stem = self
            .name
            .rsplit_once('.')
            .map_or(self.name.as_str(), |(stem, _)| stem);
        (!self.display.trim().is_empty() && self.display.trim().to_lowercase() == occurrence)
            || stem.trim().to_lowercase() == occurrence
    }
}

/// One file descriptor at `i`.
fn descriptor_at(data: &[u8], i: usize) -> Option<Descriptor> {
    let (path, after_path) = text_at(data, i, 5, MAX_PATH)?;
    let name = path.rsplit(['\\', '/']).next()?.trim();
    if !path.contains(['\\', '/']) || !name.contains('.') || name.starts_with('.') {
        return None;
    }
    u32_at(data, after_path)?; // kind of the path
    let (_, after_workspace) = text_at(data, after_path + 4, 0, MAX_NAME)?;
    let (display, after_display) = text_at(data, after_workspace + 2, 0, MAX_NAME)?;
    if data.get(after_display..after_display + 6)? != [0u8; 6] {
        return None;
    }
    let tail = after_display + 6 + 32;
    u32_at(data, tail)?;
    if u16_at(data, tail + 4)? != 0 {
        return None;
    }
    let count = u16_at(data, tail + 6)?;
    if count == 0 {
        return None;
    }
    Some(Descriptor {
        name: name.to_string(),
        display,
        count: u32::from(count),
        end: tail + 8,
    })
}

/// The first descriptor table: the first descriptor and those that follow it
/// directly.
fn descriptor_table(data: &[u8]) -> Vec<Descriptor> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let Some(first) = descriptor_at(data, i) else {
            i += 1;
            continue;
        };
        let mut next = first.end + TABLE_GAP;
        out.push(first);
        while let Some(d) = descriptor_at(data, next) {
            next = d.end + TABLE_GAP;
            out.push(d);
        }
        break;
    }
    out
}

/// The tag of a property in an occurrence record: `01 t 01 00 00 00 t`.
fn tag(t: u8) -> [u8; 7] {
    [1, t, 1, 0, 0, 0, t]
}

/// The occurrence records in stream order: (occurrence name, suppressed).
fn occurrence_records(data: &[u8]) -> Vec<(String, bool)> {
    let (name_tag, id_tag, flag_tag) = (tag(0x05), tag(0x02), tag(0x07));
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(found) = data
        .get(i..)
        .and_then(|rest| rest.windows(7).position(|w| w == name_tag))
    {
        let at = i + found;
        i = at + 1;
        let Some((name, after_name)) = text_at(data, at + 7, 1, MAX_NAME) else {
            continue;
        };
        let id_at = after_name + 4;
        let flag_at = id_at + 7 + 20;
        if data.get(id_at..id_at + 7) != Some(&id_tag[..])
            || data.get(flag_at..flag_at + 7) != Some(&flag_tag[..])
        {
            continue;
        }
        match data.get(flag_at + 7) {
            Some(0) => out.push((name, false)),
            Some(1) => out.push((name, true)),
            _ => {}
        }
    }
    out
}

/// The occurrence counts of the first descriptor table in the content of
/// the `UFRxDoc` stream: (file name, occurrences) in table order, without
/// suppressed occurrences where the occurrence records tell them. A file
/// name that appears twice (two files of that name in different folders)
/// is listed twice.
pub fn occurrence_counts(data: &[u8]) -> Vec<(String, u32)> {
    let table = descriptor_table(data);
    let all: Vec<(String, u32)> = table.iter().map(|d| (d.name.clone(), d.count)).collect();
    let total: u32 = table.iter().map(|d| d.count).sum();
    let records = occurrence_records(data);
    let Some(current) = records.get(..usize::try_from(total).unwrap_or(usize::MAX)) else {
        return all; // fewer records than occurrences: suppression not known
    };
    let mut suppressed = vec![0u32; table.len()];
    for (name, _) in current.iter().filter(|(_, s)| *s) {
        let mut matches = table.iter().enumerate().filter(|(_, d)| d.is_named(name));
        match (matches.next(), matches.next()) {
            (Some((index, _)), None) => {
                if let Some(n) = suppressed.get_mut(index) {
                    *n += 1;
                }
            }
            _ => return all, // no file or several: not guessed
        }
    }
    let mut out = Vec::with_capacity(table.len());
    for (d, n) in table.iter().zip(&suppressed) {
        let Some(active) = d.count.checked_sub(*n) else {
            return all;
        };
        out.push((d.name.clone(), active));
    }
    out
}

/// How often an Inventor assembly places each referenced file: (file name,
/// occurrences). `None` when the file is not a compound file or has no
/// descriptor stream; reads at most `max` bytes of the stream.
pub fn file_occurrence_counts<R: Read + Seek>(
    reader: R,
    max: u64,
) -> io::Result<Option<Vec<(String, u32)>>> {
    let Ok(mut cfb) = cfb::CompoundFile::open(reader) else {
        return Ok(None);
    };
    let Ok(stream) = cfb.open_stream(DESCRIPTOR_STREAM) else {
        return Ok(None);
    };
    let mut data = Vec::new();
    stream.take(max).read_to_end(&mut data)?;
    Ok(Some(occurrence_counts(&data)))
}

/// An Inventor-like compound file whose descriptor table lists `entries`
/// (path, from a library, occurrences) – for tests of the callers.
#[cfg(test)]
pub(crate) fn test_assembly(entries: &[(&str, bool, u16)]) -> Vec<u8> {
    test_assembly_with_records(entries, &[])
}

/// Like [`test_assembly`], followed by occurrence records (name, suppressed).
#[cfg(test)]
pub(crate) fn test_assembly_with_records(
    entries: &[(&str, bool, u16)],
    records: &[(&str, bool)],
) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let descriptors: Vec<Vec<u8>> = entries
        .iter()
        .map(|(path, library, count)| tests::descriptor(path, *library, "", *count))
        .collect();
    let mut data = tests::table(&descriptors);
    for (name, suppressed) in records {
        data.extend(tests::record(name, *suppressed));
    }
    let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    cfb.create_stream(DESCRIPTOR_STREAM)
        .unwrap()
        .write_all(&data)
        .unwrap();
    cfb.flush().unwrap();
    cfb.into_inner().into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn text(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = u32::try_from(units.len()).unwrap().to_le_bytes().to_vec();
        out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        out
    }

    /// One descriptor in the layout of a real Inventor 2027 assembly.
    pub(super) fn descriptor(path: &str, library: bool, display: &str, count: u16) -> Vec<u8> {
        let mut d = text(path);
        d.extend(if library { 0x4du32 } else { 0x43 }.to_le_bytes());
        d.extend(text(if library {
            "Content Center Files"
        } else {
            "Workspace"
        }));
        d.extend(if library { 3u16 } else { 1 }.to_le_bytes());
        d.extend(text(display));
        d.extend([0u8; 6]);
        d.extend((0..32u8).map(|b| b.wrapping_mul(37)));
        d.extend(0x0001_3652u32.to_le_bytes());
        d.extend(0u16.to_le_bytes());
        d.extend(count.to_le_bytes());
        d
    }

    pub(super) fn table(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                out.extend([0x2f, 0, 0, 0, 1, 0, 4, 0, 0, 0]); // the 10 bytes between descriptors
            }
            out.extend_from_slice(e);
        }
        out
    }

    /// One occurrence record in the layout of a real Inventor 2027 assembly
    /// (with some of the properties around the three that are read).
    pub(super) fn record(name: &str, suppressed: bool) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(tag(0x12));
        r.extend([0x5au8; 16]);
        r.extend([0u8; 4]);
        r.extend(tag(0x19));
        r.extend(if suppressed { 5u64 } else { 1 }.to_le_bytes());
        r.extend(tag(0x05));
        r.extend(text(name));
        r.extend([0u8; 4]);
        r.extend(tag(0x02));
        r.extend([0xa7u8; 16]);
        r.extend([0u8; 4]);
        r.extend(tag(0x07));
        r.push(u8::from(suppressed));
        r.extend([0u8; 4]);
        r.extend(tag(0x1e));
        r.extend(text(name));
        r.extend([0u8; 30]);
        r
    }

    fn with_records(table_entries: &[Vec<u8>], records: &[(&str, bool)]) -> Vec<u8> {
        let mut data = table(table_entries);
        data.extend([0x33u8; 64]);
        for (name, suppressed) in records {
            data.extend(record(name, *suppressed));
        }
        data
    }

    fn suppression_table() -> Vec<Vec<u8>> {
        vec![
            descriptor("C:\\Rover\\Wheel\\Wheel.iam", false, "", 4),
            descriptor("C:\\Rover\\Frame.ipt", false, "", 1),
            descriptor(
                "C:\\CC\\ISO 4762\\ISO 4762 - M6 x 20.ipt",
                true,
                "ISO 4762 M6 x 20",
                3,
            ),
        ]
    }

    #[test]
    fn suppressed_occurrences_are_not_counted() {
        let data = with_records(
            &suppression_table(),
            &[
                ("Wheel", false),
                ("Wheel", true),
                ("Frame", true),
                ("ISO 4762 M6 x 20", false),
                ("Wheel", false),
                ("ISO 4762 M6 x 20", false),
                ("WHEEL", false), // names compare without case
                ("ISO 4762 M6 x 20", false),
                // records of an earlier save: not read
                ("Wheel", true),
                ("ISO 4762 M6 x 20", true),
            ],
        );
        assert_eq!(
            occurrence_counts(&data),
            vec![
                ("Wheel.iam".to_string(), 3),
                // only suppressed occurrences: still listed, with 0
                ("Frame.ipt".to_string(), 0),
                ("ISO 4762 - M6 x 20.ipt".to_string(), 3),
            ]
        );
    }

    #[test]
    fn suppression_that_does_not_add_up_is_not_guessed() {
        let all = vec![
            ("Wheel.iam".to_string(), 4),
            ("Frame.ipt".to_string(), 1),
            ("ISO 4762 - M6 x 20.ipt".to_string(), 3),
        ];
        // fewer records than occurrences
        let few = with_records(&suppression_table(), &[("Wheel", true), ("Frame", false)]);
        assert_eq!(occurrence_counts(&few), all);
        // a suppressed record whose name fits no file
        let mut unknown: Vec<(&str, bool)> = vec![("Wheel", false); 4];
        unknown.extend([
            ("Bracket", true),
            ("ISO 4762 M6 x 20", false),
            ("ISO 4762 M6 x 20", false),
        ]);
        assert_eq!(
            occurrence_counts(&with_records(&suppression_table(), &unknown)),
            all
        );
        // more suppressed records than the file has occurrences
        let mut too_many: Vec<(&str, bool)> = vec![("Frame", true); 2];
        too_many.extend(vec![("Wheel", false); 3]);
        too_many.extend(vec![("ISO 4762 M6 x 20", false); 3]);
        assert_eq!(
            occurrence_counts(&with_records(&suppression_table(), &too_many)),
            all
        );
        // a name that fits two files (same display name)
        let twins = vec![
            descriptor("C:\\Rover\\Washer A.ipt", false, "DIN 988 S7", 1),
            descriptor("C:\\Rover\\Washer B.ipt", false, "DIN 988 S7", 1),
        ];
        assert_eq!(
            occurrence_counts(&with_records(
                &twins,
                &[("DIN 988 S7", true), ("DIN 988 S7", false)]
            )),
            vec![
                ("Washer A.ipt".to_string(), 1),
                ("Washer B.ipt".to_string(), 1)
            ]
        );
        // a record whose flag is neither 0 nor 1 is not a record (were it
        // read as suppressed, Frame would count 0)
        let mut odd = record("Frame", true);
        // after the flag: 4 bytes, tag 1e, the name again, 30 bytes
        let flag_at = odd.len() - 30 - (4 + 2 * "Frame".len()) - 7 - 4 - 1;
        assert_eq!(odd.get(flag_at), Some(&1));
        if let Some(b) = odd.get_mut(flag_at) {
            *b = 2;
        }
        let mut data = table(&suppression_table());
        for _ in 0..4 {
            data.extend(record("Wheel", false));
        }
        data.extend(odd);
        for _ in 0..3 {
            data.extend(record("ISO 4762 M6 x 20", false));
        }
        assert_eq!(occurrence_counts(&data), all);
    }

    fn stream() -> Vec<u8> {
        // header with the assembly's own path, then the current table, then
        // an older copy with other paths and counts after a gap
        let mut data = vec![0x07, 0x00, 0x00, 0x00, 0x01, 0x00];
        data.extend(text("C:\\Rover\\Rover.iam"));
        data.extend([0x11u8; 40]);
        data.extend(table(&[
            descriptor("C:\\Rover\\Wheel\\Wheel.iam", false, "Rad", 4),
            descriptor("C:\\Rover\\Frame.ipt", false, "", 1),
            descriptor(
                "C:\\CC\\de-DE\\DIN 912\\DIN 912 - M6 x 20.ipt",
                true,
                "",
                16,
            ),
            descriptor("C:\\Rover\\Old\\Frame.ipt", false, "", 2),
        ]));
        data.extend([0x22u8; 300]);
        data.extend(table(&[
            descriptor("D:\\Vault\\Rover\\Wheel\\Wheel.iam", false, "Rad", 3),
            descriptor("D:\\Vault\\Rover\\Frame.ipt", false, "", 1),
        ]));
        data
    }

    #[test]
    fn counts_from_the_first_descriptor_table() {
        assert_eq!(
            occurrence_counts(&stream()),
            vec![
                ("Wheel.iam".to_string(), 4),
                ("Frame.ipt".to_string(), 1),
                ("DIN 912 - M6 x 20.ipt".to_string(), 16),
                // the same name from another folder is a file of its own
                ("Frame.ipt".to_string(), 2),
            ]
        );
    }

    #[test]
    fn broken_descriptors_are_not_counted() {
        // padding not zero
        let mut bad = descriptor("C:\\Rover\\Frame.ipt", false, "", 1);
        let pad = bad.len() - 8 - 32 - 6; // first of the six zero bytes
        if let Some(b) = bad.get_mut(pad) {
            *b = 1;
        }
        assert!(occurrence_counts(&bad).is_empty());
        // zero occurrences, a name without extension, no path at all
        assert!(occurrence_counts(&descriptor("C:\\Rover\\Frame.ipt", false, "", 0)).is_empty());
        assert!(occurrence_counts(&descriptor("C:\\Rover\\Frame", false, "", 1)).is_empty());
        assert!(occurrence_counts(&descriptor("Frame.ipt", false, "", 1)).is_empty());
        // cut off in the middle
        let full = descriptor("C:\\Rover\\Frame.ipt", false, "", 1);
        assert!(occurrence_counts(full.get(..full.len() - 3).unwrap()).is_empty());
        assert!(occurrence_counts(&[]).is_empty());
    }

    #[test]
    fn a_gap_ends_the_table() {
        let mut data = descriptor("C:\\Rover\\Frame.ipt", false, "", 2);
        data.extend([0u8; 11]); // one byte more than between descriptors
        data.extend(descriptor("C:\\Rover\\Wheel.iam", false, "", 5));
        assert_eq!(occurrence_counts(&data), vec![("Frame.ipt".to_string(), 2)]);
    }

    #[test]
    fn reads_the_stream_of_a_compound_file() {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        cfb.create_storage("/RSeStorage").unwrap();
        cfb.create_stream("/RSeStorage/RSeSegInfo")
            .unwrap()
            .write_all(b"x")
            .unwrap();
        cfb.create_stream(DESCRIPTOR_STREAM)
            .unwrap()
            .write_all(&stream())
            .unwrap();
        cfb.flush().unwrap();
        let bytes = cfb.into_inner().into_inner();
        let counts = file_occurrence_counts(Cursor::new(bytes), 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(counts.len(), 4);
        assert_eq!(counts.first(), Some(&("Wheel.iam".to_string(), 4)));
        // not a compound file, or one without the stream
        assert_eq!(
            file_occurrence_counts(Cursor::new(b"plain text".to_vec()), 1 << 20).unwrap(),
            None
        );
        let mut other = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        other
            .create_stream("/Other")
            .unwrap()
            .write_all(b"x")
            .unwrap();
        other.flush().unwrap();
        assert_eq!(
            file_occurrence_counts(Cursor::new(other.into_inner().into_inner()), 1 << 20).unwrap(),
            None
        );
    }
}
