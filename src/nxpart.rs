//! Siemens NX part files (`.prt`, NX 1980 and later): the `SPLMSSTR`
//! stream container.
//!
//! The format is not documented. This reader only relies on what was
//! verified with the NIST test files in `tests/fixtures/cad/nist`:
//!
//! ```text
//! 0x00  "SPLMSSTR"
//! 0x11  u64 LE  offset of the FOOTER block
//! 0x19  "HEADER"  u32 count, count × entry
//! ...   stream data
//! foot  "FOOTER"  u32 count, count × entry
//! entry u32 LE name length, name (UTF-8 path below /Root/), u64 offset, u64 length
//! ```
//!
//! Streams used by TeamCraft:
//!
//! * `/Root/UG_PART/ExternalReferences`, `/Root/FastLoad/ExternalReferences`:
//!   the file names of the components (and the part itself). The rest of
//!   the file also keeps history such as `Part renamed from "…\old.prt"`,
//!   which is why a scan of the whole file finds false references;
//! * `/Root/images/preview`: the preview image (JPEG).
//!
//! Folder entries end in `/` and have no data. Anything that does not fit
//! (short file, entry out of bounds) is skipped, never trusted. Files are
//! read with seeks: only the directory and the requested streams.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::io::{self, Read, Seek, SeekFrom};

/// Magic bytes at the start of an NX part.
pub const MAGIC: &[u8; 8] = b"SPLMSSTR";
/// Streams with the names of the referenced component files.
pub const REFERENCE_STREAMS: [&str; 2] = [
    "/Root/UG_PART/ExternalReferences",
    "/Root/FastLoad/ExternalReferences",
];
/// Stream with the preview image.
pub const PREVIEW_STREAM: &str = "/Root/images/preview";
/// Stream with the lightweight representation (a JT 9 file after a short
/// index), read by `mesh` for the 3D preview of parts.
pub const JT_STREAM: &str = "/Root/FastLoad/JT";

const FOOTER_OFFSET_AT: usize = 0x11;
/// Bytes at the start searched for the header block.
const HEAD: usize = 64 * 1024;
/// Largest footer block read.
const MAX_FOOTER: u64 = 4 * 1024 * 1024;
const MAX_NAME: usize = 4096;
const MAX_ENTRIES: usize = 100_000;

/// A stream of the container: absolute offset and length in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub name: String,
    pub offset: u64,
    pub len: u64,
}

/// True if the bytes start like an NX part container.
pub fn is_nx_part(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

fn u32_at(bytes: &[u8], at: usize) -> Option<usize> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    let b = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(b.try_into().ok()?))
}

/// Reads one directory block (`HEADER` or `FOOTER`) that starts at `at` of
/// `buf`. Entries pointing beyond `file_len` are dropped.
fn block(buf: &[u8], at: usize, tag: &[u8; 6], file_len: u64, out: &mut Vec<Stream>) {
    let Some(mut pos) = at.checked_add(tag.len()) else {
        return;
    };
    if buf.get(at..pos) != Some(tag) {
        return;
    }
    let Some(count) = u32_at(buf, pos) else {
        return;
    };
    pos += 4;
    for _ in 0..count.min(MAX_ENTRIES) {
        let Some(name_len) = u32_at(buf, pos).filter(|n| (1..=MAX_NAME).contains(n)) else {
            return;
        };
        pos += 4;
        let Some(name) = buf
            .get(pos..pos + name_len)
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            return;
        };
        pos += name_len;
        let (Some(offset), Some(len)) = (u64_at(buf, pos), u64_at(buf, pos + 8)) else {
            return;
        };
        pos += 16;
        let in_bounds = offset.checked_add(len).is_some_and(|end| end <= file_len);
        if !name.ends_with('/') && len > 0 && in_bounds && !out.iter().any(|s| s.name == name) {
            out.push(Stream {
                name: name.to_string(),
                offset,
                len,
            });
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Header streams from the first bytes of a file.
fn header_streams(head: &[u8], file_len: u64, out: &mut Vec<Stream>) {
    if let Some(at) = find(head.get(..256).unwrap_or(head), b"HEADER") {
        block(head, at, b"HEADER", file_len, out);
    }
}

/// The data streams of an NX part held in memory, or an empty list if the
/// bytes are not one. A file cut short keeps the streams inside the bytes.
pub fn streams(bytes: &[u8]) -> Vec<Stream> {
    let mut out = Vec::new();
    if !is_nx_part(bytes) {
        return out;
    }
    let file_len = bytes.len() as u64;
    header_streams(bytes, file_len, &mut out);
    if let Some(at) = u64_at(bytes, FOOTER_OFFSET_AT).and_then(|a| usize::try_from(a).ok()) {
        block(bytes, at, b"FOOTER", file_len, &mut out);
    }
    out
}

/// The data of a stream of an NX part held in memory.
pub fn data<'a>(bytes: &'a [u8], stream: &Stream) -> Option<&'a [u8]> {
    let start = usize::try_from(stream.offset).ok()?;
    let end = start.checked_add(usize::try_from(stream.len).ok()?)?;
    bytes.get(start..end)
}

/// An NX part opened from a file (or any seekable reader).
pub struct NxPart<R> {
    reader: R,
    streams: Vec<Stream>,
}

impl<R: Read + Seek> NxPart<R> {
    /// Reads the directory. `None` if the file is not an NX part.
    pub fn open(mut reader: R) -> io::Result<Option<Self>> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut head = Vec::new();
        (&mut reader).take(HEAD as u64).read_to_end(&mut head)?;
        if !is_nx_part(&head) {
            return Ok(None);
        }
        let mut streams = Vec::new();
        header_streams(&head, file_len, &mut streams);
        if let Some(at) = u64_at(&head, FOOTER_OFFSET_AT).filter(|&a| a < file_len) {
            reader.seek(SeekFrom::Start(at))?;
            let mut foot = Vec::new();
            (&mut reader).take(MAX_FOOTER).read_to_end(&mut foot)?;
            block(&foot, 0, b"FOOTER", file_len, &mut streams);
        }
        Ok(Some(Self { reader, streams }))
    }

    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    /// The data of the named stream, if it exists and is at most `max`
    /// bytes long.
    pub fn read(&mut self, name: &str, max: u64) -> io::Result<Option<Vec<u8>>> {
        let Some(s) = self.streams.iter().find(|s| s.name == name).cloned() else {
            return Ok(None);
        };
        if s.len > max {
            return Ok(None);
        }
        self.reader.seek(SeekFrom::Start(s.offset))?;
        let mut data = Vec::with_capacity(s.len as usize);
        (&mut self.reader).take(s.len).read_to_end(&mut data)?;
        Ok((data.len() as u64 == s.len).then_some(data))
    }
}

/// The data of the reference streams of an NX part (each at most `max`
/// bytes), or `None` if it is not an NX part or has none. Each stream starts
/// with the name of the part itself, followed by its components.
pub fn reference_streams<R: Read + Seek>(reader: R, max: u64) -> io::Result<Option<Vec<Vec<u8>>>> {
    let Some(mut part) = NxPart::open(reader)? else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for name in REFERENCE_STREAMS {
        if let Some(data) = part.read(name, max)? {
            out.push(data);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

/// The reference streams of an NX part held in memory.
pub fn reference_streams_in(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let streams = streams(bytes);
    let out: Vec<Vec<u8>> = REFERENCE_STREAMS
        .iter()
        .filter_map(|name| streams.iter().find(|s| s.name == *name))
        .filter_map(|s| data(bytes, s).map(<[u8]>::to_vec))
        .collect();
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(name: &str) -> Vec<u8> {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/mtc-assembly/nx"
        );
        std::fs::read(format!("{dir}/{name}")).unwrap()
    }

    #[test]
    fn reads_the_directory_of_the_nist_assembly() {
        let bytes = fixture("NIST mtc crada assembly.prt");
        let names: Vec<String> = streams(&bytes).into_iter().map(|s| s.name).collect();
        for expected in REFERENCE_STREAMS
            .iter()
            .copied()
            .chain([PREVIEW_STREAM, "/Root/part/attrs"])
        {
            assert!(
                names.iter().any(|n| n == expected),
                "{expected} in {names:?}"
            );
        }
        // Folders are not streams.
        assert!(names.iter().all(|n| !n.ends_with('/')));
        // The seeking reader sees the same directory.
        let part = NxPart::open(Cursor::new(&bytes)).unwrap().unwrap();
        let seek_names: Vec<&str> = part.streams().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(seek_names, names);
    }

    #[test]
    fn preview_stream_is_a_jpeg() {
        let bytes = fixture("91274A118 SHCS.prt");
        let mut part = NxPart::open(Cursor::new(&bytes)).unwrap().unwrap();
        let jpeg = part.read(PREVIEW_STREAM, 1 << 20).unwrap().unwrap();
        assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]));
        assert!(part.read(PREVIEW_STREAM, 10).unwrap().is_none(), "max size");
        assert!(part.read("/Root/none", 1 << 20).unwrap().is_none());
    }

    #[test]
    fn other_files_and_garbage_are_not_nx_parts() {
        assert!(streams(b"#UGC:2 ASSEMBLY creo").is_empty());
        assert!(NxPart::open(Cursor::new(b"ISO-10303-21;".to_vec()))
            .unwrap()
            .is_none());
        // Magic only, truncated directory, bogus footer offset: no panic, no streams.
        let mut fake = MAGIC.to_vec();
        fake.extend_from_slice(&[0xFF; 9]);
        fake.extend_from_slice(&u64::MAX.to_le_bytes());
        fake.extend_from_slice(b"HEADER\x05\x00\x00\x00\x10\x00");
        assert!(streams(&fake).is_empty());
        assert!(NxPart::open(Cursor::new(fake))
            .unwrap()
            .unwrap()
            .streams()
            .is_empty());
    }

    /// A synthetic container: the directory entries in the HEADER block, the
    /// data after it and an empty FOOTER at the end. Entries with `None`
    /// data are folders (`len` 0).
    fn synthetic(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
        let dir_len: usize = 6 + 4 + entries.iter().map(|(n, _)| 4 + n.len() + 16).sum::<usize>();
        let mut offset = (0x19 + dir_len) as u64;
        let mut out = MAGIC.to_vec();
        out.resize(FOOTER_OFFSET_AT, 0);
        out.extend_from_slice(&0u64.to_le_bytes()); // patched below
        out.extend_from_slice(b"HEADER");
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (name, data) in entries {
            let len = data.map_or(0, |d| d.len() as u64);
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            offset += len;
        }
        for (_, data) in entries {
            out.extend_from_slice(data.unwrap_or_default());
        }
        let footer = out.len() as u64;
        out[FOOTER_OFFSET_AT..FOOTER_OFFSET_AT + 8].copy_from_slice(&footer.to_le_bytes());
        out.extend_from_slice(b"FOOTER\x00\x00\x00\x00");
        out
    }

    #[test]
    fn synthetic_directory_round_trip() {
        let bytes = synthetic(&[
            ("/Root/", None),
            (
                "/Root/UG_PART/ExternalReferences",
                Some(b"\x00top.prt\x00wheel.prt\x00"),
            ),
            (
                "/Root/history",
                Some(b"Part renamed from \"C:\\old\\ghost.prt\""),
            ),
        ]);
        let s = streams(&bytes);
        assert_eq!(
            s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["/Root/UG_PART/ExternalReferences", "/Root/history"],
            "folders have no data"
        );
        assert_eq!(
            data(&bytes, &s[0]).unwrap(),
            b"\x00top.prt\x00wheel.prt\x00"
        );
        // The seeking reader sees the same directory.
        let mut part = NxPart::open(Cursor::new(bytes.clone())).unwrap().unwrap();
        assert_eq!(part.streams(), &s[..]);
        assert!(
            part.read("/Root/history", 3).unwrap().is_none(),
            "larger than max"
        );
        assert_eq!(
            reference_streams(Cursor::new(bytes.clone()), 1 << 20).unwrap(),
            reference_streams_in(&bytes)
        );
        // Only the reference stream is scanned, without the part itself and
        // without the history of the file.
        assert_eq!(
            crate::cadrefs::find(&bytes, "renamed.prt"),
            vec!["wheel.prt"]
        );
    }

    #[test]
    fn entries_beyond_the_file_are_dropped() {
        let mut bytes = synthetic(&[(
            "/Root/FastLoad/ExternalReferences",
            Some(b"\x00a.prt\x00b.prt"),
        )]);
        // Patch the length of the only entry to point past the end.
        let len_at = 0x19 + 6 + 4 + 4 + "/Root/FastLoad/ExternalReferences".len() + 8;
        bytes[len_at..len_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(streams(&bytes).is_empty());
        assert!(reference_streams_in(&bytes).is_none());
        // Without reference streams an NX part has no references to read.
        let plain = synthetic(&[("/Root/other", Some(b"\x00wheel.prt\x00"))]);
        assert!(reference_streams_in(&plain).is_none());
    }

    #[test]
    fn nx_preview_jpeg_becomes_the_thumbnail() {
        // SOI, SOF0 with 120 × 90, EOI.
        let mut jpeg = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 90, 0x00, 120,
        ];
        jpeg.extend_from_slice(&[0; 12]);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        let bytes = synthetic(&[(PREVIEW_STREAM, Some(&jpeg))]);
        let path = std::env::temp_dir().join(format!("teamcraft-nx-{}.prt", uuid::Uuid::new_v4()));
        std::fs::write(&path, &bytes).unwrap();
        let t = crate::thumbnail::extract(&path).unwrap().unwrap();
        assert_eq!((t.content_type, t.bytes), ("image/jpeg", jpeg));
        // A preview that is too small is no thumbnail.
        let mut tiny = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 4, 0x00, 4];
        tiny.extend_from_slice(&[0; 12]);
        std::fs::write(&path, synthetic(&[(PREVIEW_STREAM, Some(&tiny))])).unwrap();
        assert_eq!(crate::thumbnail::extract(&path).unwrap(), None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn truncated_files_keep_the_streams_inside() {
        let bytes = fixture("NIST mtc crada assembly.prt");
        let cut = &bytes[..bytes.len() / 2];
        let s = streams(cut);
        assert!(!s.is_empty());
        assert!(s.iter().all(|s| s.offset + s.len <= cut.len() as u64));
    }
}
