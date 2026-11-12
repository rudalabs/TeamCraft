//! Autodesk Inventor files (`.ipt`, `.iam`): the segments below the OLE
//! storage `RSeStorage` (see `docs/src/research/inventor-display-preview.md`).
//!
//! Every segment has a meta stream `M<id>` and a data stream `B<id>`:
//!
//! ```text
//! M<id>: u32 25 "RSe Meta Stream Version 8" | u16 | 16 bytes
//!        | u32 n, n × UTF-16 segment name | class UUID | 3 × u32
//!        | u32 n, ASCII date | u32 n, ASCII date | u8 | zlib/zstd body:
//!          7 × u16 | u32 sections, sections × u32 (bit 31 + length) | u32
//!          | u32 n, n × 10 bytes | u32 | u32 n, n × 28 bytes | u32
//!          | u32 classes, classes × (UUID, u16, u32, u16, u32)
//! B<id>: 18 bytes | zlib/zstd body: per section
//!          u32 class word (low byte: class table index) | payload
//!          | u32 payload length again | u8 0
//! ```
//!
//! The bodies are zlib (Inventor 2019–2024 as tested) or Zstandard (newer
//! files, issue #271), told apart by their magic bytes. Where the header of a
//! Zstandard stream ends is not known from real files: the frame is taken at
//! the zlib offset, else at the first Zstandard magic close to it.
//!
//! Classes are named by the first 32 bits of their UUID (`time_low`). Every
//! length is checked against the data before it is used, unpacked streams
//! are capped, and a damaged file is an error, never a panic.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::io::{self, Read, Seek};

use flate2::read::ZlibDecoder;

/// Largest meta or data stream read (compressed).
const MAX_RAW: u64 = 256 * 1024 * 1024;
/// Bytes before the zlib body of a data stream (Zstandard: see `unpack`).
const DATA_HEADER: usize = 18;
/// Magic number of a Zstandard frame (RFC 8878).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// How far before or after the zlib offset a Zstandard frame is looked for.
const ZSTD_SLACK: usize = 64;
/// Largest Zstandard window a frame may ask for: the decoder allocates it up
/// front, so it is also capped by the output limit (at least 1 MiB).
const MAX_ZSTD_WINDOW: u64 = 32 * 1024 * 1024;
/// Most Zstandard frames read one after the other from one stream.
const MAX_ZSTD_FRAMES: usize = 64;
/// Marker of a typed array.
pub const ARRAY: u32 = 0x3000_0002;
/// Element type of `u32` arrays.
pub const ELEM_U32: u32 = 0;
/// Element type of key arrays (`u32` too).
pub const ELEM_KEYS: u32 = 0x1000_0000;

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("Inventor file: {what}"))
}

pub(crate) fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

pub(crate) fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

pub(crate) fn f64_at(data: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(
        data.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn usize_at(data: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32_at(data, at)?).ok()
}

/// One segment: its section lengths, its class table and the inflated data.
#[derive(Debug)]
pub struct Segment {
    /// Payload length of every section, in order.
    lengths: Vec<usize>,
    /// Class (`time_low` of the UUID) per class-table index.
    pub classes: Vec<u32>,
    data: Vec<u8>,
}

/// One section of a segment.
#[derive(Clone, Copy, Debug)]
pub struct Section<'a> {
    /// Position in the segment.
    pub index: usize,
    /// Class (`time_low` of the UUID); 0 when the index is outside the table.
    pub class: u32,
    pub payload: &'a [u8],
}

impl Segment {
    /// The sections in order; `Err` at the first section whose framing does
    /// not check out.
    pub fn sections(&self) -> Sections<'_> {
        Sections {
            segment: self,
            index: 0,
            at: 0,
        }
    }

    /// Element size of an array element type (`None`: unknown class).
    pub fn element_size(&self, elem: u32) -> Option<usize> {
        match elem {
            ELEM_U32 | ELEM_KEYS => Some(4),
            e if e & 0xFFFF_FF00 == 0x100 => {
                let class = *self.classes.get(usize::try_from(e & 0xFF).ok()?)?;
                match class {
                    POINT3F | VECTOR3F => Some(12),
                    POINT2F => Some(8),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The class of an array element type (`ELEM_U32`/`ELEM_KEYS` as is).
    pub fn element_class(&self, elem: u32) -> Option<u32> {
        if elem & 0xFFFF_FF00 == 0x100 {
            self.classes
                .get(usize::try_from(elem & 0xFF).ok()?)
                .copied()
        } else {
            Some(elem)
        }
    }
}

/// Array element class: `f32` point (x, y, z).
pub const POINT3F: u32 = 0xF272_10C1;
/// Array element class: `f32` unit vector.
pub const VECTOR3F: u32 = 0x8D9D_0B25;
/// Array element class: `f32` 2D point.
pub const POINT2F: u32 = 0x68FC_ABB1;

/// Iterator over the sections of a [`Segment`].
pub struct Sections<'a> {
    segment: &'a Segment,
    index: usize,
    at: usize,
}

impl<'a> Iterator for Sections<'a> {
    type Item = io::Result<Section<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let len = *self.segment.lengths.get(self.index)?;
        let data = &self.segment.data;
        let result = (|| {
            let word = u32_at(data, self.at).ok_or_else(|| bad("section cut short"))?;
            let start = self.at + 4;
            let end = start
                .checked_add(len)
                .filter(|&e| e <= data.len())
                .ok_or_else(|| bad("section cut short"))?;
            if usize_at(data, end) != Some(len) {
                return Err(bad("section length check failed"));
            }
            // a flag byte follows every section; 0 in every file seen
            match data.get(end + 4) {
                None | Some(0) => {}
                Some(_) => return Err(bad("unsupported section trailer")),
            }
            let class = usize::try_from(word & 0xFF)
                .ok()
                .and_then(|k| self.segment.classes.get(k).copied())
                .unwrap_or(0);
            let section = Section {
                index: self.index,
                class,
                payload: data.get(start..end).unwrap_or_default(),
            };
            self.at = end + 5;
            Ok(section)
        })();
        self.index += 1;
        if result.is_err() {
            // stop after the first damaged section
            self.index = usize::MAX;
        }
        Some(result)
    }
}

/// Reads typed values from a section payload.
pub struct Cursor<'a> {
    pub data: &'a [u8],
    pub at: usize,
}

/// A typed array: its element type and raw elements.
#[derive(Clone, Copy, Debug)]
pub struct Array<'a> {
    pub elem: u32,
    pub count: usize,
    pub bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], at: usize) -> Cursor<'a> {
        Cursor { data, at }
    }

    pub fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.data, self.at)?;
        self.at += 4;
        Some(v)
    }

    /// The array at the cursor; element sizes come from the segment's
    /// class table, so unknown element classes end the read.
    pub fn array(&mut self, segment: &Segment) -> Option<Array<'a>> {
        if u32_at(self.data, self.at)? != ARRAY {
            return None;
        }
        let count = usize_at(self.data, self.at + 4)?;
        if count == 0 {
            self.at += 8;
            return Some(Array {
                elem: ELEM_U32,
                count: 0,
                bytes: &[],
            });
        }
        let elem = u32_at(self.data, self.at + 12)?;
        let size = segment.element_size(elem)?;
        let start = self.at + 16;
        let end = start.checked_add(count.checked_mul(size)?)?;
        let bytes = self.data.get(start..end)?;
        self.at = end;
        Some(Array { elem, count, bytes })
    }
}

/// The segment named `name` (e.g. `PmGraphicsSegment`), inflating at most
/// `limit` bytes; `Ok(None)` when the file has no such segment.
pub fn segment<F: Read + Seek>(
    ole: &mut cfb::CompoundFile<F>,
    name: &str,
    limit: u64,
) -> io::Result<Option<Segment>> {
    let metas: Vec<(String, std::path::PathBuf)> = match ole.read_storage("/RSeStorage") {
        Ok(entries) => entries
            .filter(|e| e.is_stream() && e.name().starts_with('M') && e.len() <= MAX_RAW)
            .map(|e| {
                let id = e.name().get(1..).unwrap_or_default().to_string();
                (id, e.path().to_path_buf())
            })
            .collect(),
        Err(_) => return Ok(None),
    };
    for (id, path) in metas {
        let raw = read_stream(ole, &path, MAX_RAW)?;
        let Some((seg_name, body_at)) = meta_head(&raw) else {
            continue;
        };
        if seg_name != name {
            continue;
        }
        let body = unpack(&raw, body_at, limit)?;
        let (lengths, classes) = meta_body(&body).ok_or_else(|| bad("damaged meta stream"))?;
        let data_path = std::path::PathBuf::from(format!("/RSeStorage/B{id}"));
        let raw = read_stream(ole, &data_path, MAX_RAW)?;
        let data = unpack(&raw, DATA_HEADER, limit)?;
        return Ok(Some(Segment {
            lengths,
            classes,
            data,
        }));
    }
    Ok(None)
}

/// A whole stream of the compound file, at most `limit` bytes.
pub fn read_stream<F: Read + Seek>(
    ole: &mut cfb::CompoundFile<F>,
    path: &std::path::Path,
    limit: u64,
) -> io::Result<Vec<u8>> {
    let stream = ole.open_stream(path)?;
    if stream.len() > limit {
        return Err(bad("stream too large"));
    }
    let mut out = Vec::new();
    stream.take(limit).read_to_end(&mut out)?;
    Ok(out)
}

/// The compressed body of a meta or data stream whose zlib body starts at
/// `at`: Zstandard when a frame starts there, zlib when a zlib header does,
/// else the first Zstandard frame within [`ZSTD_SLACK`] bytes of `at`.
fn unpack(raw: &[u8], at: usize, limit: u64) -> io::Result<Vec<u8>> {
    let body = raw.get(at..).unwrap_or_default();
    if body.starts_with(&ZSTD_MAGIC) {
        return unzstd(body, limit);
    }
    if !zlib_header(body) {
        let from = at.saturating_sub(ZSTD_SLACK);
        let to = at
            .saturating_add(ZSTD_SLACK + ZSTD_MAGIC.len())
            .min(raw.len());
        if let Some(near) = raw.get(from..to).and_then(|w| {
            w.windows(ZSTD_MAGIC.len())
                .position(|m| m == ZSTD_MAGIC)
                .and_then(|k| raw.get(from + k..))
        }) {
            return unzstd(near, limit);
        }
    }
    inflate(body, limit)
}

/// A zlib header: Deflate with a window of at most 32 KiB and a check sum
/// that divides by 31 (RFC 1950).
fn zlib_header(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => {
            cmf & 0x0F == 8 && cmf >> 4 <= 7 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0
        }
        _ => false,
    }
}

fn inflate(data: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    ZlibDecoder::new(data)
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|_| bad("damaged compressed data"))?;
    if out.len() as u64 > limit {
        return Err(bad("segment larger than the preview reads"));
    }
    Ok(out)
}

/// Zstandard frames, one after the other while the next one starts right
/// behind; the output is capped like the zlib output.
fn unzstd(data: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = data;
    for _ in 0..MAX_ZSTD_FRAMES {
        if !rest.starts_with(&ZSTD_MAGIC) {
            break;
        }
        let left = limit.saturating_add(1).saturating_sub(out.len() as u64);
        let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
            &mut rest,
            MAX_ZSTD_WINDOW.min(limit.max(1 << 20)),
        )
        .map_err(|_| bad("damaged compressed data"))?;
        (&mut decoder)
            .take(left)
            .read_to_end(&mut out)
            .map_err(|_| bad("damaged compressed data"))?;
        if out.len() as u64 > limit {
            return Err(bad("segment larger than the preview reads"));
        }
        if !decoder.decoder.is_finished() {
            // the frame stopped before its end: no further frame can follow
            break;
        }
    }
    Ok(out)
}

/// The segment name and where the zlib body starts.
fn meta_head(raw: &[u8]) -> Option<(String, usize)> {
    const VERSION: &[u8] = b"RSe Meta Stream Version ";
    let n = usize_at(raw, 0)?;
    if !raw.get(4..4 + n)?.starts_with(VERSION) {
        return None;
    }
    // u16 version, 16 bytes
    let mut at = 4 + n + 2 + 16;
    let chars = usize_at(raw, at)?;
    if chars > 256 {
        return None;
    }
    at += 4;
    let units: Vec<u16> = raw
        .get(at..at + 2 * chars)?
        .chunks_exact(2)
        .filter_map(|c| c.try_into().ok().map(u16::from_le_bytes))
        .collect();
    let name = String::from_utf16(&units).ok()?;
    at += 2 * chars + 16 + 12;
    for _ in 0..2 {
        let len = usize_at(raw, at)?;
        if len > 256 {
            return None;
        }
        at += 4 + len;
    }
    Some((name, at + 1))
}

/// Section lengths and class table of an inflated meta body.
fn meta_body(body: &[u8]) -> Option<(Vec<usize>, Vec<u32>)> {
    let mut at = 14;
    let n = usize_at(body, at)?;
    at += 4;
    if n.checked_mul(4)? > body.len() {
        return None;
    }
    let mut lengths = Vec::with_capacity(n);
    for k in 0..n {
        let word = u32_at(body, at + 4 * k)?;
        lengths.push(usize::try_from(word & 0x7FFF_FFFF).ok()?);
    }
    at += 4 * n + 4;
    for size in [10usize, 28] {
        let count = usize_at(body, at)?;
        at = at.checked_add(4)?.checked_add(count.checked_mul(size)?)?;
        at = at.checked_add(4)?;
        if at > body.len() {
            return None;
        }
    }
    let count = usize_at(body, at)?;
    at += 4;
    if count > 256 || at.checked_add(count.checked_mul(28)?)? > body.len() {
        return None;
    }
    let classes = (0..count)
        .map(|k| u32_at(body, at + 28 * k))
        .collect::<Option<Vec<u32>>>()?;
    Some((lengths, classes))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A segment to write: (name, classes, sections as (class index, payload)).
    pub(crate) type SegmentSpec<'a> = (&'a str, &'a [u32], Vec<(u8, Vec<u8>)>);

    /// Writes a minimal Inventor-like compound file with the given segments:
    /// (name, classes, sections as (class index, payload)).
    pub(crate) fn file(segments: &[SegmentSpec], extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut ole = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        ole.create_storage("/RSeStorage").unwrap();
        for (k, (name, classes, sections)) in segments.iter().enumerate() {
            let mut meta = Vec::new();
            let version = b"RSe Meta Stream Version 8";
            meta.extend((version.len() as u32).to_le_bytes());
            meta.extend(version);
            meta.extend([8, 0]);
            meta.extend([0; 16]);
            let units: Vec<u16> = name.encode_utf16().collect();
            meta.extend((units.len() as u32).to_le_bytes());
            for u in units {
                meta.extend(u.to_le_bytes());
            }
            meta.extend([0; 16 + 12]);
            for _ in 0..2 {
                meta.extend(19u32.to_le_bytes());
                meta.extend(b"01/01/2026 00:00:00");
            }
            meta.push(1);
            let mut body = vec![0u8; 14];
            body.extend((sections.len() as u32).to_le_bytes());
            for (_, p) in sections {
                body.extend((0x8000_0000 | p.len() as u32).to_le_bytes());
            }
            body.extend((4 * sections.len() as u32 + 4).to_le_bytes());
            for _ in 0..2 {
                body.extend(0u32.to_le_bytes());
                body.extend(4u32.to_le_bytes());
            }
            body.extend((classes.len() as u32).to_le_bytes());
            for c in classes.iter() {
                body.extend(c.to_le_bytes());
                body.extend([0; 24]);
            }
            meta.extend(zlib(&body));
            let mut data = Vec::new();
            for (class, p) in sections {
                data.extend([*class, 1, 0, 0]);
                data.extend(p);
                data.extend((p.len() as u32).to_le_bytes());
                data.push(0);
            }
            let mut stream = vec![0u8; DATA_HEADER];
            stream.extend(zlib(&data));
            ole.create_stream(format!("/RSeStorage/Mseg{k}"))
                .unwrap()
                .write_all(&meta)
                .unwrap();
            ole.create_stream(format!("/RSeStorage/Bseg{k}"))
                .unwrap()
                .write_all(&stream)
                .unwrap();
        }
        for (name, bytes) in extra {
            ole.create_stream(name).unwrap().write_all(bytes).unwrap();
        }
        ole.flush().unwrap();
        ole.into_inner().into_inner()
    }

    pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(data).unwrap();
        z.finish().unwrap()
    }

    pub(crate) fn zstd(data: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
    }

    /// The same Inventor file with every `RSeStorage` segment repacked as
    /// Zstandard (as issue #271 reports for newer files), the frame `shift`
    /// bytes behind (or before) where the zlib body started.
    pub(crate) fn as_zstd(bytes: &[u8], shift: isize) -> Vec<u8> {
        use std::io::Write;
        let mut ole = cfb::CompoundFile::open(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let streams: Vec<(String, std::path::PathBuf)> = ole
            .read_storage("/RSeStorage")
            .unwrap()
            .filter(|e| e.is_stream())
            .map(|e| (e.name().to_string(), e.path().to_path_buf()))
            .collect();
        for (name, path) in streams {
            let at = match name.chars().next() {
                Some('M') => {
                    meta_head(&read_stream(&mut ole, &path, MAX_RAW).unwrap())
                        .unwrap()
                        .1
                }
                Some('B') => DATA_HEADER,
                _ => continue,
            };
            let raw = read_stream(&mut ole, &path, MAX_RAW).unwrap();
            let body = unpack(&raw, at, MAX_RAW).unwrap();
            let mut out = raw[..at.saturating_add_signed(shift.min(0))].to_vec();
            out.extend(vec![0u8; shift.max(0) as usize]);
            out.extend(zstd(&body));
            ole.create_stream(&path).unwrap().write_all(&out).unwrap();
        }
        ole.flush().unwrap();
        ole.into_inner().into_inner()
    }

    /// A typed array as Inventor writes it.
    pub(crate) fn array(elem: u32, count: usize, bytes: &[u8]) -> Vec<u8> {
        let mut out = ARRAY.to_le_bytes().to_vec();
        out.extend((count as u32).to_le_bytes());
        if count > 0 {
            out.extend((count as u32).to_le_bytes());
            out.extend(elem.to_le_bytes());
            out.extend(bytes);
        }
        out
    }

    fn open(bytes: Vec<u8>) -> cfb::CompoundFile<std::io::Cursor<Vec<u8>>> {
        cfb::CompoundFile::open(std::io::Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn segments_are_found_by_name_and_walked() {
        let bytes = file(
            &[
                ("PmAppSegment", &[1], vec![(0, vec![9; 3])]),
                (
                    "PmGraphicsSegment",
                    &[0xAAAA_0001, POINT3F],
                    vec![(1, vec![1, 2]), (0, array(0x101, 1, &[0; 12]))],
                ),
            ],
            &[],
        );
        let mut ole = open(bytes);
        let seg = segment(&mut ole, "PmGraphicsSegment", 1 << 20)
            .unwrap()
            .unwrap();
        let sections: Vec<Section> = seg.sections().collect::<io::Result<_>>().unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].class, POINT3F);
        assert_eq!(sections[0].payload, [1, 2]);
        assert_eq!(sections[1].class, 0xAAAA_0001);
        let a = Cursor::new(sections[1].payload, 0).array(&seg).unwrap();
        assert_eq!((a.count, a.bytes.len()), (1, 12));
        assert_eq!(seg.element_class(a.elem), Some(POINT3F));
        assert!(segment(&mut ole, "AmRxSegment", 1 << 20).unwrap().is_none());
    }

    #[test]
    fn damaged_segments_are_errors_not_panics() {
        let bytes = file(
            &[(
                "PmGraphicsSegment",
                &[1],
                vec![(0, vec![7; 40]), (0, vec![8; 4])],
            )],
            &[],
        );
        // every truncation of the whole file
        for cut in (0..bytes.len()).step_by(97) {
            if let Ok(mut ole) =
                cfb::CompoundFile::open(std::io::Cursor::new(bytes[..cut].to_vec()))
            {
                if let Ok(Some(seg)) = segment(&mut ole, "PmGraphicsSegment", 1 << 20) {
                    for s in seg.sections() {
                        let _ = s;
                    }
                }
            }
        }
        // a wrong length check stops the walk with an error
        let seg = Segment {
            lengths: vec![2, 2],
            classes: vec![1],
            data: vec![0, 1, 0, 0, 5, 5, 3, 0, 0, 0, 0],
        };
        let all: Vec<_> = seg.sections().collect();
        assert_eq!(all.len(), 1);
        assert!(all[0].is_err());
        // an inflate limit
        let mut ole = open(file(
            &[("PmGraphicsSegment", &[1], vec![(0, vec![0; 5000])])],
            &[],
        ));
        assert!(segment(&mut ole, "PmGraphicsSegment", 100).is_err());
    }

    /// The segments of the walk test above, written with zlib.
    fn two_segments() -> Vec<u8> {
        file(
            &[
                ("PmAppSegment", &[1], vec![(0, vec![9; 3])]),
                (
                    "PmGraphicsSegment",
                    &[0xAAAA_0001, POINT3F],
                    vec![(1, vec![1, 2]), (0, array(0x101, 1, &[0; 12]))],
                ),
            ],
            &[],
        )
    }

    fn sections_of(bytes: Vec<u8>) -> io::Result<Vec<(u32, Vec<u8>)>> {
        let mut ole = open(bytes);
        let seg =
            segment(&mut ole, "PmGraphicsSegment", 1 << 20)?.ok_or_else(|| bad("no segment"))?;
        seg.sections()
            .map(|s| s.map(|s| (s.class, s.payload.to_vec())))
            .collect()
    }

    #[test]
    fn zstandard_segments_read_like_zlib_ones() {
        let zlib = sections_of(two_segments()).unwrap();
        assert_eq!(zlib.len(), 2);
        // the frame where the zlib body was, and a few bytes off either way
        for shift in [0, 1, 4, 16, -2, -1] {
            let packed = as_zstd(&two_segments(), shift);
            assert_eq!(sections_of(packed).unwrap(), zlib, "shift {shift}");
        }
    }

    #[test]
    fn zstandard_bodies_are_told_apart_from_zlib_by_their_magic() {
        let data = b"PmGraphicsSegment ".repeat(50);
        assert_eq!(unpack(&zlib(&data), 0, 1 << 20).unwrap(), data);
        assert_eq!(unpack(&zstd(&data), 0, 1 << 20).unwrap(), data);
        // frames one after the other
        let mut two = zstd(&data[..300]);
        two.extend(zstd(&data[300..]));
        assert_eq!(unpack(&two, 0, 1 << 20).unwrap(), data);
        // a header byte before the frame (the u8 of the meta stream)
        let mut late = vec![2u8];
        late.extend(zstd(&data));
        assert_eq!(unpack(&late, 0, 1 << 20).unwrap(), data);
        // a frame too far behind the expected start is not searched for
        let mut far = vec![0u8; ZSTD_SLACK + 1];
        far.extend(zstd(&data));
        assert!(unpack(&far, 0, 1 << 20).is_err());
        assert!(zlib_header(&zlib(&data)) && !zlib_header(&zstd(&data)));
    }

    #[test]
    fn damaged_zstandard_segments_are_errors_not_panics() {
        let data: Vec<u8> = (0..20_000u32)
            .flat_map(|k| (k % 251).to_le_bytes())
            .collect();
        let packed = zstd(&data);
        // the output limit holds for Zstandard too
        assert!(unpack(&packed, 0, 1000).is_err());
        assert_eq!(unpack(&packed, 0, data.len() as u64).unwrap(), data);
        // every truncation and bytes flipped all over the frame
        for cut in 0..packed.len() {
            let _ = unpack(&packed[..cut], 0, 1 << 20);
        }
        for at in 0..packed.len() {
            let mut bad = packed.clone();
            bad[at] ^= 0x5A;
            let _ = unpack(&bad, 0, 1 << 20);
        }
        // a frame header asking for a huge window
        let mut huge = ZSTD_MAGIC.to_vec();
        huge.extend([0x00, 0xF8, 0, 0, 0, 0]);
        assert!(unpack(&huge, 0, 1 << 30).is_err());
        // the whole file, cut and corrupted
        let bytes = as_zstd(&two_segments(), 0);
        for cut in (0..bytes.len()).step_by(61) {
            if let Ok(mut ole) =
                cfb::CompoundFile::open(std::io::Cursor::new(bytes[..cut].to_vec()))
            {
                if let Ok(Some(seg)) = segment(&mut ole, "PmGraphicsSegment", 1 << 20) {
                    for s in seg.sections() {
                        let _ = s;
                    }
                }
            }
        }
    }

    #[test]
    fn arrays_with_unknown_elements_or_short_data_end_the_read() {
        let seg = Segment {
            lengths: vec![],
            classes: vec![POINT3F, 0x1234_5678],
            data: vec![],
        };
        let ok = array(0x100, 2, &[0; 24]);
        assert_eq!(Cursor::new(&ok, 0).array(&seg).unwrap().count, 2);
        assert!(Cursor::new(&ok[..30], 0).array(&seg).is_none());
        assert!(Cursor::new(&array(0x101, 1, &[0; 12]), 0)
            .array(&seg)
            .is_none());
        let mut huge = array(0x100, 1, &[0; 12]);
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Cursor::new(&huge, 0).array(&seg).is_none());
        let empty = array(0, 0, &[]);
        let mut c = Cursor::new(&empty, 0);
        assert_eq!(c.array(&seg).unwrap().count, 0);
        assert_eq!(c.at, 8);
    }
}
