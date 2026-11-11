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
//! follow each other without a gap. Suppressed occurrences are counted too:
//! the table does not tell them apart (Inventor's own BOM leaves them out).
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

/// One file descriptor at `i`: the file name (last part of the path), its
/// occurrences and the offset after the descriptor.
fn descriptor_at(data: &[u8], i: usize) -> Option<(String, u32, usize)> {
    let (path, after_path) = text_at(data, i, 5, MAX_PATH)?;
    let name = path.rsplit(['\\', '/']).next()?.trim();
    if !path.contains(['\\', '/']) || !name.contains('.') || name.starts_with('.') {
        return None;
    }
    u32_at(data, after_path)?; // kind of the path
    let (_, after_workspace) = text_at(data, after_path + 4, 0, MAX_NAME)?;
    let (_, after_display) = text_at(data, after_workspace + 2, 0, MAX_NAME)?;
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
    Some((name.to_string(), u32::from(count), tail + 8))
}

/// The occurrence counts of the first descriptor table in the content of
/// the `UFRxDoc` stream: (file name, occurrences) in table order. A file
/// name that appears twice (two files of that name in different folders)
/// is listed twice.
pub fn occurrence_counts(data: &[u8]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let Some((name, count, end)) = descriptor_at(data, i) else {
            i += 1;
            continue;
        };
        out.push((name, count));
        // the table goes on only where the next descriptor follows directly
        let mut next = end + TABLE_GAP;
        while let Some((name, count, end)) = descriptor_at(data, next) {
            out.push((name, count));
            next = end + TABLE_GAP;
        }
        break;
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
    use std::io::{Cursor, Write};
    let descriptors: Vec<Vec<u8>> = entries
        .iter()
        .map(|(path, library, count)| tests::descriptor(path, *library, "", *count))
        .collect();
    let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    cfb.create_stream(DESCRIPTOR_STREAM)
        .unwrap()
        .write_all(&tests::table(&descriptors))
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
