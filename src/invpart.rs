//! Autodesk Inventor files (`.ipt`, `.iam`): the segments below the OLE
//! storage `RSeStorage` (see `docs/src/research/inventor-display-preview.md`).
//!
//! Every segment has a meta stream `M<id>` and a data stream `B<id>`:
//!
//! ```text
//! M<id>: u32 25 "RSe Meta Stream Version 8" | u16 | 16 bytes
//!        | u32 n, n × UTF-16 segment name | class UUID | 3 × u32
//!        | u32 n, ASCII date | u32 n, ASCII date | u8 codec | compressed body:
//!          7 × u16 | u32 sections, sections × u32 (bit 31 + length) | u32
//!          | u32 n, n × 10 bytes | u32 | u32 n, n × 28 bytes | u32
//!          | u32 classes, classes × (UUID, u16, u32, u16, u32)
//! B<id>: 16 bytes, u8 4, u8 codec | compressed body: per section
//!          u32 class word (low byte: class table index) | payload
//!          | u32 payload length again | u8 0
//! ```
//!
//! The codec byte is 1 for zlib (up to Inventor 2025) and 2 for one Zstandard
//! frame (Inventor 2026 and later, see
//! `docs/src/research/inventor-zstd-streams.md`); the reader goes by the
//! frame magic of the body itself.
//!
//! Classes are named by the first 32 bits of their UUID (`time_low`). Every
//! length is checked against the data before it is used, decompressed streams
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
/// Bytes before the compressed body of a data stream.
const DATA_HEADER: usize = 18;
/// Marker of a typed array.
pub const ARRAY: u32 = 0x3000_0002;
/// Element type of plain `u32` arrays in files of Inventor 2019–2022.
pub const ELEM_U32: u32 = 0;
/// Element types of `u32` arrays in files of Inventor 2027, which name the
/// kind of number where older files have 0: three kinds of integers (8 for
/// triangle indices and occurrence ids) and 16 for refs to sections.
const U32_2027: [u32; 4] = [4, 8, 0xE, 0x10];
/// Element type of key arrays (`u32` too).
pub const ELEM_KEYS: u32 = 0x1000_0000;

/// Whether an array element type is a plain `u32` (no key array).
pub fn is_u32(elem: u32) -> bool {
    elem == ELEM_U32 || U32_2027.contains(&elem)
}

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

/// One segment: its section lengths, its class table and the decompressed data.
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
            ELEM_KEYS => Some(4),
            e if is_u32(e) => Some(4),
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

    /// The class of an array element type (the `u32` types as they are).
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

/// The segment named `name` (e.g. `PmGraphicsSegment`), decompressing at most
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
        let body = decompress(raw.get(body_at..).unwrap_or_default(), limit)?;
        let (lengths, classes) = meta_body(&body).ok_or_else(|| bad("damaged meta stream"))?;
        let data_path = std::path::PathBuf::from(format!("/RSeStorage/B{id}"));
        let raw = read_stream(ole, &data_path, MAX_RAW)?;
        let data = decompress(raw.get(DATA_HEADER..).unwrap_or_default(), limit)?;
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

/// Start of a Zstandard frame (RFC 8878): the streams of Inventor 2026 and
/// later. Earlier files hold zlib streams, which never start like this.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// The body of a meta or data stream, at most `limit` bytes: zlib up to
/// Inventor 2025, one Zstandard frame from Inventor 2026 on.
fn decompress(data: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    if data.starts_with(&ZSTD_MAGIC) {
        let mut frame = ruzstd::decoding::StreamingDecoder::new(data)
            .map_err(|_| bad("damaged compressed data"))?;
        // 0: the frame does not say how long its content is
        let declared = frame.decoder.content_size();
        if declared > limit {
            return Err(bad("segment larger than the preview reads"));
        }
        (&mut frame)
            .take(limit.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(|_| bad("damaged compressed data"))?;
        if declared != 0 && declared != out.len() as u64 {
            return Err(bad("damaged compressed data"));
        }
    } else {
        ZlibDecoder::new(data)
            .take(limit.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(|_| bad("damaged compressed data"))?;
    }
    if out.len() as u64 > limit {
        return Err(bad("segment larger than the preview reads"));
    }
    Ok(out)
}

/// The segment name and where the compressed body starts.
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

/// Section lengths and class table of a decompressed meta body.
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

    /// How the streams of a test file are compressed.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) enum Codec {
        /// Up to Inventor 2025.
        Zlib,
        /// Inventor 2026 and later.
        Zstd,
    }

    impl Codec {
        /// The byte before a compressed body.
        fn byte(self) -> u8 {
            match self {
                Codec::Zlib => 1,
                Codec::Zstd => 2,
            }
        }

        fn compress(self, data: &[u8]) -> Vec<u8> {
            match self {
                Codec::Zlib => zlib(data),
                Codec::Zstd => zstd(data),
            }
        }
    }

    /// Writes a minimal Inventor-like compound file with zlib streams:
    /// (name, classes, sections as (class index, payload)).
    pub(crate) fn file(segments: &[SegmentSpec], extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        file_with(Codec::Zlib, segments, extra)
    }

    /// [`file`] with the streams compressed by `codec`.
    pub(crate) fn file_with(
        codec: Codec,
        segments: &[SegmentSpec],
        extra: &[(&str, Vec<u8>)],
    ) -> Vec<u8> {
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
            meta.push(codec.byte());
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
            meta.extend(codec.compress(&body));
            let mut data = Vec::new();
            for (class, p) in sections {
                data.extend([*class, 1, 0, 0]);
                data.extend(p);
                data.extend((p.len() as u32).to_le_bytes());
                data.push(0);
            }
            let mut stream = vec![0u8; DATA_HEADER];
            stream[DATA_HEADER - 2..].copy_from_slice(&[4, codec.byte()]);
            stream.extend(codec.compress(&data));
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

    /// One Zstandard frame with compressed blocks.
    pub(crate) fn zstd(data: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
    }

    /// A Zstandard frame as Inventor writes it, by hand: single segment,
    /// `declared` as the 4-byte content size, no checksum, one raw block.
    fn raw_frame(declared: u32, data: &[u8]) -> Vec<u8> {
        let mut frame = ZSTD_MAGIC.to_vec();
        frame.push(0xA0);
        frame.extend(declared.to_le_bytes());
        let header = (data.len() as u32) << 3 | 1;
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(data);
        frame
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
    fn zstandard_streams_read_like_zlib_streams() {
        let segments: [SegmentSpec; 2] = [
            ("PmAppSegment", &[1], vec![(0, vec![9; 3])]),
            (
                "PmGraphicsSegment",
                &[0xAAAA_0001, POINT3F],
                vec![(1, vec![1, 2]), (0, array(0x101, 1, &[0; 12]))],
            ),
        ];
        let read = |codec: Codec| {
            let mut ole = open(file_with(codec, &segments, &[]));
            let seg = segment(&mut ole, "PmGraphicsSegment", 1 << 20)
                .unwrap()
                .unwrap();
            let sections: Vec<(u32, Vec<u8>)> = seg
                .sections()
                .map(|s| s.map(|s| (s.class, s.payload.to_vec())))
                .collect::<io::Result<_>>()
                .unwrap();
            (seg.classes.clone(), sections)
        };
        let old = read(Codec::Zlib);
        assert_eq!(old.1.len(), 2);
        assert_eq!(read(Codec::Zstd), old);
    }

    #[test]
    fn a_zstandard_body_is_told_by_its_first_bytes() {
        let text = b"sections, sections, sections and more sections".repeat(40);
        // compressed blocks, as the encoder writes them
        assert_eq!(decompress(&zstd(&text), 1 << 20).unwrap(), text);
        // the frame Inventor writes: content size up front, raw block
        assert_eq!(
            decompress(&raw_frame(text.len() as u32, &text), 1 << 20).unwrap(),
            text
        );
        // zlib as before, and neither of the two is an error
        assert_eq!(decompress(&zlib(&text), 1 << 20).unwrap(), text);
        for junk in [&b""[..], b"\x28\xb5\x2f", b"plain text", &ZSTD_MAGIC] {
            assert!(decompress(junk, 1 << 20).is_err(), "{junk:?}");
        }
    }

    #[test]
    fn zstandard_bodies_are_capped_and_checked() {
        let data = vec![7u8; 5000];
        let limit_error = |r: io::Result<Vec<u8>>| {
            let e = r.unwrap_err().to_string();
            assert!(e.contains("larger than the preview reads"), "{e}");
        };
        // exactly at the limit is fine, one byte less is not
        assert_eq!(decompress(&zstd(&data), 5000).unwrap().len(), 5000);
        limit_error(decompress(&zstd(&data), 4999));
        // a frame that declares more than the limit is refused up front,
        // whatever it really holds
        limit_error(decompress(&raw_frame(1 << 20, b"tiny"), 4999));
        // … also by the decoder, which takes no window above 128 MiB
        assert!(decompress(&raw_frame(1 << 30, b"tiny"), u64::MAX).is_err());
        // … and one that yields another length than it declares is damaged
        for declared in [3u32, 5] {
            let e = decompress(&raw_frame(declared, b"four"), 1 << 20).unwrap_err();
            assert!(e.to_string().contains("damaged compressed data"), "{e}");
        }
        // every truncation of a frame is an error, never a panic
        let frame = zstd(&b"abcdefgh".repeat(300));
        for cut in 0..frame.len() {
            assert!(decompress(&frame[..cut], 1 << 20).is_err(), "cut at {cut}");
        }
        // … and so is every flipped byte, or it still decodes within the limit
        for at in 0..frame.len() {
            let mut bad = frame.clone();
            bad[at] ^= 0x5A;
            if let Ok(out) = decompress(&bad, 4096) {
                assert!(out.len() <= 4096);
            }
        }
    }

    #[test]
    fn the_array_types_of_inventor_2027_are_u32() {
        let seg = Segment {
            lengths: vec![],
            classes: vec![POINT3F],
            data: vec![],
        };
        for elem in [ELEM_U32, 4, 8, 0xE, 0x10] {
            assert!(is_u32(elem), "{elem}");
            let a = array(elem, 3, &[0; 12]);
            let read = Cursor::new(&a, 0).array(&seg).unwrap();
            assert_eq!((read.count, read.bytes.len(), read.elem), (3, 12, elem));
        }
        // keys are four bytes too, but no plain numbers
        assert!(!is_u32(ELEM_KEYS));
        assert_eq!(seg.element_size(ELEM_KEYS), Some(4));
        // bytes (6) and unknown types end the read
        for elem in [6, 5, 0x20, 0x11] {
            assert!(!is_u32(elem), "{elem}");
            assert!(Cursor::new(&array(elem, 3, &[0; 12]), 0)
                .array(&seg)
                .is_none());
        }
        // the second count is a capacity: the data holds `count` elements
        let mut a = array(8, 2, &[1, 0, 0, 0, 2, 0, 0, 0]);
        a[8..12].copy_from_slice(&7u32.to_le_bytes());
        let mut c = Cursor::new(&a, 0);
        assert_eq!(c.array(&seg).unwrap().bytes, [1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(c.at, a.len());
    }

    #[test]
    fn damaged_segments_are_errors_not_panics() {
        for codec in [Codec::Zlib, Codec::Zstd] {
            let bytes = file_with(
                codec,
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
        // a size limit
        let mut ole = open(file(
            &[("PmGraphicsSegment", &[1], vec![(0, vec![0; 5000])])],
            &[],
        ));
        assert!(segment(&mut ole, "PmGraphicsSegment", 100).is_err());
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
