//! SOLIDWORKS files from version 2015 on (`.sldprt`, `.sldasm`, `.slddrw`):
//! SOLIDWORKS' own section container.
//!
//! Up to SOLIDWORKS 2014 the files were OLE compound files; newer ones are a
//! sequence of Deflate-compressed sections, similar to the local entries of
//! a ZIP file. The format is not documented. This reader only relies on what
//! was verified with the NIST test files in `tests/fixtures/cad/nist`
//! (SOLIDWORKS 2018):
//!
//! ```text
//! section  14 00 06 00 08 00   marker (ZIP: version 2.0, flags 6, method 8 = Deflate)
//!          u32 LE              DOS time and date
//!          u32 LE              CRC-32 of the uncompressed data
//!          u32 LE              compressed size
//!          u32 LE              uncompressed size
//!          u32 LE              name length
//!          name                each byte with its two nibbles swapped ("Contents/CMgr")
//!          data                raw Deflate (no zlib header), compressed size bytes
//! ```
//!
//! Every section is preceded by the same four bytes (a per-file
//! *signature*). The file also holds records that look like section headers
//! but carry no data (often their "CRC" is four times, their "size" twice the
//! "compressed size"; they are not preceded by the signature) and a
//! directory at the end whose names are prefixed with binary fields. Neither
//! is needed: a section counts only if it follows the signature (once that
//! is known), its name is printable, its data lies inside the file and –
//! when it is read – it inflates to exactly the stored size with the stored
//! CRC-32.
//!
//! Sections used by TeamCraft:
//!
//! * `PreviewPNG`: the preview image (PNG, 640 × 480 in the test files);
//! * `swXmlContents/COMPINSTANCETREE` (assemblies): XML with the component
//!   files (`swFile swPath="…"`) and their instances (`swReference`);
//! * `SwDocContentMgr/SwDocContentMgrInfo`: the document manager's record of
//!   the referenced documents with their full paths – used for files without
//!   the component tree (drawings; not verified with a real drawing).
//!
//! Hostile input: offsets and sizes are never trusted. Sections are found by
//! a bounded search ([`MAX_SEARCH`] bytes looked at byte by byte, at most
//! [`MAX_SECTIONS`] sections, the file read in chunks that are each read
//! about once), the data of a section after the signature is skipped, and
//! inflating stops one byte after the stored size; sections larger than the
//! caller's limit are not inflated at all (decompression bombs end there),
//! and at most [`MAX_TRIES`] sections of one name are inflated.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};

use flate2::read::DeflateDecoder;

/// Section with the preview image (PNG).
pub const PREVIEW_SECTION: &str = "PreviewPNG";
/// Section with the component tree of an assembly (XML).
pub const COMPONENT_TREE_SECTION: &str = "swXmlContents/COMPINSTANCETREE";
/// Section with the referenced documents as recorded by the document manager.
pub const CONTENT_MANAGER_SECTION: &str = "SwDocContentMgr/SwDocContentMgrInfo";

const MARKER: [u8; 6] = [0x14, 0x00, 0x06, 0x00, 0x08, 0x00];
/// Marker and the five fixed fields before the name.
const FIXED: usize = MARKER.len() + 20;
/// The first section starts within these bytes of the file.
const FIRST_WITHIN: usize = 64;
const MAX_NAME: usize = 512;
/// Most sections read from the directory of one file.
pub const MAX_SECTIONS: usize = 100_000;
/// Most bytes looked at one by one while searching for section headers
/// (the data of recognised sections is skipped, not searched).
pub const MAX_SEARCH: u64 = 64 * 1024 * 1024;
/// Bytes before each section header (the same in all sections of a file).
const SIGNATURE: usize = 4;
const CHUNK: usize = 64 * 1024;
/// Most sections of the same name inflated by one [`SwContainer::read`]
/// (the NIST files have every name once).
pub const MAX_TRIES: usize = 4;

/// A section of the container: where its compressed data lies in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// Absolute offset of the compressed data.
    pub offset: u64,
    pub compressed: u64,
    pub size: u64,
    pub crc: u32,
    /// Found after the signature (or the section that revealed it); the
    /// sections found before are only plausible headers.
    pub confirmed: bool,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(b.try_into().ok()?))
}

/// The section whose header starts at `buf[0]` (the file offset of `buf[0]`
/// is `at`), if the header is plausible: the marker, a printable name and
/// data inside the file. Header-like records without data are rejected.
fn parse_header(buf: &[u8], at: u64, file_len: u64) -> Option<Section> {
    if !buf.starts_with(&MARKER) {
        return None;
    }
    let crc = u32_at(buf, 10)?;
    let compressed = u32_at(buf, 14)?;
    let size = u32_at(buf, 18)?;
    let name_len = usize::try_from(u32_at(buf, 22)?).ok()?;
    if !(1..=MAX_NAME).contains(&name_len) {
        return None;
    }
    // Records without data: "CRC" = 4 × and "size" = 2 × "compressed size".
    if compressed > 0
        && u64::from(crc) == 4 * u64::from(compressed)
        && u64::from(size) == 2 * u64::from(compressed)
    {
        return None;
    }
    let raw = buf.get(FIXED..FIXED + name_len)?;
    let name: String = raw.iter().map(|b| char::from(b.rotate_left(4))).collect();
    if !name.chars().all(|c| (' '..='~').contains(&c)) || name.trim().is_empty() {
        return None;
    }
    let offset = at.checked_add((FIXED + name_len) as u64)?;
    let end = offset.checked_add(u64::from(compressed))?;
    if compressed == 0 || end > file_len {
        return None;
    }
    Some(Section {
        name,
        offset,
        compressed: u64::from(compressed),
        size: u64::from(size),
        crc,
        confirmed: false,
    })
}

fn find_marker(hay: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = hay.get(from..)?.iter().position(|&b| b == MARKER[0]) {
        let at = from + i;
        if hay.get(at..at + MARKER.len())? == MARKER {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// True if the first bytes of a file start like a SOLIDWORKS 2015+ file: a
/// plausible section header within the first `FIRST_WITHIN` bytes (the
/// first 600 bytes of a file are enough).
pub fn is_container(head: &[u8]) -> bool {
    first_section_at(head).is_some()
}

fn first_section_at(head: &[u8]) -> Option<usize> {
    let window = &head[..head.len().min(FIRST_WITHIN + MARKER.len())];
    let mut from = 0;
    while let Some(i) = find_marker(&window[from..]) {
        let at = from + i;
        // The first records are header-like records without data; their
        // marker and name are enough to recognise the container.
        if plausible_record(&head[at..]) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Marker, a name length in range and a printable name.
fn plausible_record(buf: &[u8]) -> bool {
    let Some(name_len) = u32_at(buf, 22).and_then(|n| usize::try_from(n).ok()) else {
        return false;
    };
    (1..=MAX_NAME).contains(&name_len)
        && buf
            .get(FIXED..FIXED + name_len)
            .is_some_and(|raw| raw.iter().all(|b| (0x20..0x7f).contains(&b.rotate_left(4))))
}

/// Reads up to `buf.len()` bytes at `pos`; returns how many were read.
fn read_at<R: Read + Seek>(reader: &mut R, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
    reader.seek(SeekFrom::Start(pos))?;
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Inflates raw Deflate data of a section: `Some` only if it yields exactly
/// `size` bytes with the CRC-32 `crc`. Never produces more than `size + 1`
/// bytes, whatever the data claims.
pub fn inflate<R: Read>(compressed: R, size: u64, crc: u32) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::with_capacity(usize::try_from(size.min(1 << 20)).unwrap_or(0));
    match DeflateDecoder::new(compressed)
        .take(size.saturating_add(1))
        .read_to_end(&mut out)
    {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::InvalidInput
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::UnexpectedEof
            ) =>
        {
            return Ok(None)
        }
        Err(e) => return Err(e),
    }
    Ok((out.len() as u64 == size && crc32fast::hash(&out) == crc).then_some(out))
}

/// A buffered view of the file for the walk: refilled only when the
/// requested bytes are not inside it.
#[derive(Default)]
struct Window {
    start: u64,
    data: Vec<u8>,
    /// The buffer ends at the end of the file.
    at_eof: bool,
}

impl Window {
    /// Up to `len` bytes from `pos` (fewer only at the end of the file).
    fn get<R: Read + Seek>(&mut self, reader: &mut R, pos: u64, len: usize) -> io::Result<&[u8]> {
        let rest = self.rest(reader, pos, len)?;
        Ok(&rest[..len.min(rest.len())])
    }

    /// The buffered bytes from `pos` on, at least `min` of them (fewer only
    /// at the end of the file). The buffer is refilled only when fewer are
    /// buffered, so a search that moves forward through the file reads each
    /// chunk about once – not once per marker it looks at.
    fn rest<R: Read + Seek>(&mut self, reader: &mut R, pos: u64, min: usize) -> io::Result<&[u8]> {
        let end = self.start + self.data.len() as u64;
        let inside = pos >= self.start
            && pos <= end
            && (self.at_eof || pos.saturating_add(min as u64) <= end)
            && !self.data.is_empty();
        if !inside {
            let want = CHUNK.max(min);
            self.data.resize(want, 0);
            let n = read_at(reader, pos, &mut self.data)?;
            self.data.truncate(n);
            self.at_eof = n < want;
            self.start = pos;
        }
        let from = usize::try_from(pos - self.start).unwrap_or(usize::MAX);
        Ok(self.data.get(from..).unwrap_or_default())
    }
}

/// A SOLIDWORKS 2015+ file opened from a file (or any seekable reader).
pub struct SwContainer<R> {
    reader: R,
    sections: Vec<Section>,
}

impl<R: Read + Seek> SwContainer<R> {
    /// Finds the sections. `None` if the file is not a SOLIDWORKS 2015+ file.
    pub fn open(mut reader: R) -> io::Result<Option<Self>> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        let mut head = vec![0u8; FIRST_WITHIN + FIXED + MAX_NAME];
        let n = read_at(&mut reader, 0, &mut head)?;
        head.truncate(n);
        let Some(first) = first_section_at(&head) else {
            return Ok(None);
        };
        let sections = Self::walk(&mut reader, first as u64, file_len)?;
        Ok(Some(Self { reader, sections }))
    }

    /// Walks from header to header. Every section of a file is preceded by
    /// the same four bytes (its *signature*); the header-like records without
    /// data are not. Until the signature is known – two sections in a row,
    /// the second four bytes after the data of the first, both preceded by
    /// the same bytes – the file is searched byte by byte; afterwards only
    /// headers with the signature count and their data is skipped. The byte
    /// search is bounded by [`MAX_SEARCH`].
    fn walk(reader: &mut R, mut pos: u64, file_len: u64) -> io::Result<Vec<Section>> {
        let mut sections = Vec::new();
        let mut searched = 0u64;
        let mut window = Window::default();
        let mut signature: Option<[u8; 4]> = None;
        while pos < file_len && searched < MAX_SEARCH && sections.len() < MAX_SECTIONS {
            // Search the next marker from `pos` in what is buffered (at least
            // a whole header, unless the file ends earlier).
            let chunk = window.rest(reader, pos, FIXED + MAX_NAME)?;
            let n = chunk.len();
            let Some(i) = find_marker(chunk) else {
                if n < MARKER.len() {
                    break;
                }
                // Keep the last bytes: a marker may straddle the chunks.
                let step = (n - (MARKER.len() - 1)) as u64;
                searched += step;
                pos += step;
                continue;
            };
            searched += i as u64 + 1;
            let at = pos + i as u64;
            let before: Option<[u8; 4]> = match at.checked_sub(4) {
                Some(b) => window.get(reader, b, 4)?.try_into().ok(),
                None => None,
            };
            let header = window.get(reader, at, FIXED + MAX_NAME)?;
            let section = parse_header(header, at, file_len)
                .filter(|_| signature.is_none() || before == signature);
            let Some(mut section) = section else {
                pos = at + 1;
                continue;
            };
            let end = section.offset + section.compressed;
            if signature.is_some() {
                section.confirmed = true;
                sections.push(section);
                pos = end;
                continue;
            }
            let after = window.get(reader, end, SIGNATURE + MARKER.len())?;
            if after.len() == SIGNATURE + MARKER.len()
                && after[SIGNATURE..] == MARKER
                && before.is_some_and(|b| after[..SIGNATURE] == b)
            {
                signature = before;
                section.confirmed = true;
                pos = end;
            } else {
                pos = at + 1;
            }
            sections.push(section);
        }
        Ok(sections)
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The uncompressed data of the named section, if it exists, is at most
    /// `max` bytes long and inflates correctly (size and CRC-32). Should the
    /// name occur more than once, the first section that inflates wins –
    /// confirmed sections first (real files also hold header-like records
    /// with the names of real sections before the signature is known). Of
    /// each kind only the first [`MAX_TRIES`] are tried: each may inflate up
    /// to `max` bytes before it fails, so a file full of bogus copies of one
    /// section must not multiply that work.
    pub fn read(&mut self, name: &str, max: u64) -> io::Result<Option<Vec<u8>>> {
        let named = |confirmed: bool| {
            self.sections
                .iter()
                .filter(move |s| s.name == name && s.size <= max && s.confirmed == confirmed)
                .take(MAX_TRIES)
                .cloned()
        };
        let candidates: Vec<Section> = named(true).chain(named(false)).collect();
        for s in candidates {
            self.reader.seek(SeekFrom::Start(s.offset))?;
            let input = (&mut self.reader).take(s.compressed);
            if let Some(data) = inflate(input, s.size, s.crc)? {
                return Ok(Some(data));
            }
        }
        Ok(None)
    }
}

/// What a SOLIDWORKS 2015+ file says about the files it references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReferenceData {
    /// Component files of an assembly (paths as stored), from the component
    /// tree; `None` if the file has no component tree.
    pub components: Option<Vec<String>>,
    /// The document manager's section, to be scanned for file names when
    /// there is no component tree.
    pub content_manager: Option<Vec<u8>>,
}

/// The reference sections of a SOLIDWORKS 2015+ file (each at most `max`
/// bytes uncompressed), or `None` if the file is not one.
pub fn reference_data<R: Read + Seek>(reader: R, max: u64) -> io::Result<Option<ReferenceData>> {
    let Some(mut file) = SwContainer::open(reader)? else {
        return Ok(None);
    };
    let components = file
        .read(COMPONENT_TREE_SECTION, max)?
        .map(|xml| components(&xml));
    let content_manager = match components {
        Some(_) => None,
        None => file.read(CONTENT_MANAGER_SECTION, max)?,
    };
    Ok(Some(ReferenceData {
        components,
        content_manager,
    }))
}

/// XML as text: UTF-8, or UTF-16 with a byte order mark.
fn xml_text(xml: &[u8]) -> String {
    let utf16 = |bytes: &[u8], be: bool| {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| {
                if be {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match xml {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [b'<', 0, ..] => utf16(xml, false),
        _ => String::from_utf8_lossy(xml.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(xml)).into_owned(),
    }
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// The files of the direct components of an assembly, from its component
/// tree (`swXmlContents/COMPINSTANCETREE`), each once in the order of the
/// tree:
///
/// ```text
/// <swFile id="6" swPath="Z:\…\box.SLDPRT"/>                      the files
/// <swModel id="5" swFileRef="6"/>                                 a model of a file
/// <swModel id="2" swFileRef="3">                                  the assembly itself
///   <swReference swModelRef="5" swIsVirtualComponent="NO"/>       an instance
/// </swModel>
/// ```
///
/// The assembly's own models are those that no instance refers to; only
/// their instances count (sub-assemblies list their own components).
/// Virtual components live inside the assembly and have no file. Broken XML
/// yields the components read up to the error.
pub fn components(xml: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    let text = xml_text(xml);
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut files: HashMap<String, String> = HashMap::new();
    let mut model_file: HashMap<String, String> = HashMap::new();
    // (model containing the instance, model the instance refers to)
    let mut instances: Vec<(String, String)> = Vec::new();
    let mut current_model: Option<String> = None;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        match &event {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"swFile" => {
                    if let (Some(id), Some(path)) = (attr(e, b"id"), attr(e, b"swPath")) {
                        files.insert(id, path);
                    }
                }
                b"swModel" => {
                    let id = attr(e, b"id").unwrap_or_default();
                    if let Some(file) = attr(e, b"swFileRef") {
                        model_file.insert(id.clone(), file);
                    }
                    if matches!(event, Event::Start(_)) {
                        current_model = Some(id);
                    }
                }
                b"swReference" => {
                    let is_virtual = attr(e, b"swIsVirtualComponent")
                        .is_some_and(|v| v.eq_ignore_ascii_case("YES"));
                    if let (Some(parent), Some(target), false) =
                        (current_model.clone(), attr(e, b"swModelRef"), is_virtual)
                    {
                        instances.push((parent, target));
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == b"swModel" => current_model = None,
            _ => {}
        }
        if instances.len() > 1_000_000 {
            break;
        }
    }
    let used: HashSet<&str> = instances.iter().map(|(_, t)| t.as_str()).collect();
    let mut seen = HashSet::new();
    instances
        .iter()
        .filter(|(parent, _)| !used.contains(parent.as_str()))
        .filter_map(|(_, target)| files.get(model_file.get(target)?))
        .filter(|path| seen.insert(path.to_lowercase()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn fixture(name: &str) -> Vec<u8> {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/mtc-assembly/solidworks"
        );
        std::fs::read(format!("{dir}/{name}")).unwrap()
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    const SIG: [u8; 4] = [0xda, 0x1f, 0x8b, 0xe1];

    /// A section as SOLIDWORKS writes it (without the signature before it).
    fn section(name: &str, data: &[u8]) -> Vec<u8> {
        let packed = deflate(data);
        raw_section(name, &packed, data.len() as u32, crc32fast::hash(data))
    }

    fn raw_section(name: &str, packed: &[u8], size: u32, crc: u32) -> Vec<u8> {
        let mut out = MARKER.to_vec();
        out.extend_from_slice(&[0xc3, 0xf8, 0xf8, 0x33]);
        for v in [crc, packed.len() as u32, size, name.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend(name.bytes().map(|b| b.rotate_left(4)));
        out.extend_from_slice(packed);
        out
    }

    /// A file start and the sections, each after the signature.
    fn container(sections: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x32, 0x34, 0x09, 0x2c, 0, 0, 0, 4, 0xc1];
        for s in sections {
            out.extend_from_slice(&SIG);
            out.extend_from_slice(s);
        }
        out
    }

    #[test]
    fn records_without_the_signature_are_ignored_once_it_is_known() {
        let mut bytes = container(&[section("One", b"one"), section("Two", b"two")]);
        // Not after the signature: a stray header (e.g. in the directory).
        bytes.extend_from_slice(&[9, 9, 9, 9]);
        bytes.extend(section("Stray", b"stray"));
        bytes.extend_from_slice(&SIG);
        bytes.extend(section("Three", b"three"));
        // A file of its own in the system temp folder: the repository's
        // `target/` need not exist (CARGO_TARGET_DIR elsewhere), and a shared
        // name would race with parallel test runs.
        let p = std::env::temp_dir().join(format!("teamcraft-swpart-{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&p, &bytes).unwrap();
        let sw = SwContainer::open(std::fs::File::open(&p).unwrap())
            .unwrap()
            .unwrap();
        let names: Vec<&str> = sw.sections().iter().map(|s| s.name.as_str()).collect();
        let _ = std::fs::remove_file(&p);
        assert_eq!(names, ["One", "Two", "Three"]);
    }

    /// Every section that inflates correctly, found by trying every marker
    /// in the file (slow, the reference for the walk).
    fn brute_force(bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for at in 0..bytes.len() {
            if let Some(s) = parse_header(&bytes[at..], at as u64, bytes.len() as u64) {
                let data = &bytes[s.offset as usize..(s.offset + s.compressed) as usize];
                if inflate(data, s.size, s.crc).unwrap().is_some() {
                    out.push(s.name);
                }
            }
        }
        out
    }

    fn all_fixtures() -> Vec<std::path::PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let ext = p
                    .extension()
                    .map(|x| x.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if p.is_dir() {
                    walk(&p, out);
                } else if matches!(ext.as_str(), "sldprt" | "sldasm" | "slddrw") {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        walk(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cad")),
            &mut out,
        );
        assert!(out.len() >= 10, "{out:?}");
        out
    }

    #[test]
    fn the_walk_finds_every_section_of_the_nist_files() {
        for path in all_fixtures() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(is_container(&bytes[..600]), "{path:?}");
            let mut file = SwContainer::open(Cursor::new(&bytes)).unwrap().unwrap();
            let mut good = Vec::new();
            for s in file.sections().to_vec() {
                if file.read(&s.name, u64::MAX).unwrap().is_some() && !good.contains(&s.name) {
                    good.push(s.name);
                }
            }
            let mut expected = brute_force(&bytes);
            expected.sort();
            expected.dedup();
            good.sort();
            assert_eq!(good, expected, "{path:?}");
            for name in [PREVIEW_SECTION, CONTENT_MANAGER_SECTION, "Header2"] {
                assert!(good.iter().any(|n| n == name), "{name} in {path:?}");
            }
        }
    }

    #[test]
    fn every_nist_file_has_a_png_preview() {
        for path in all_fixtures() {
            let file = std::fs::File::open(&path).unwrap();
            let mut sw = SwContainer::open(io::BufReader::new(file))
                .unwrap()
                .unwrap();
            let png = sw.read(PREVIEW_SECTION, 8 << 20).unwrap().unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{path:?}");
            assert!(
                sw.read(PREVIEW_SECTION, 1000).unwrap().is_none(),
                "max size"
            );
            assert!(sw.read("Nope", 1 << 20).unwrap().is_none());
        }
    }

    #[test]
    fn the_assembly_names_exactly_its_eight_parts() {
        let bytes = fixture("nist_mtc_crada_assembly_rev-D.SLDASM");
        let data = reference_data(Cursor::new(&bytes), 32 << 20)
            .unwrap()
            .unwrap();
        let mut names: Vec<String> = data
            .components
            .unwrap()
            .iter()
            .map(|p| p.rsplit('\\').next().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "90591A141_ZINC PLATED STEEL HEX NUT.SLDPRT",
                "91274A118_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
                "91274A141_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
                "91304A112_BLUE FLAT-HEAD SOCKET CAP SCREW.SLDPRT",
                "nist_mtc_crada_box_rev-D.SLDPRT",
                "nist_mtc_crada_cover_rev-B.SLDPRT",
                "nist_mtc_crada_plate_rev-A.SLDPRT",
                "nist_mtc_crada_polycarb-window.SLDPRT",
            ]
        );
        assert!(data.content_manager.is_none());
        // Parts have no component tree.
        let part = reference_data(
            Cursor::new(fixture("nist_mtc_crada_box_rev-D.SLDPRT")),
            32 << 20,
        )
        .unwrap()
        .unwrap();
        assert!(part.components.is_none());
        assert!(part.content_manager.is_some());
    }

    #[test]
    fn component_tree_counts_direct_components_only() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <swSolidWorks><swHeader>
              <swFile id="1" swPath="C:\p\Top.SLDASM"/>
              <swFile id="2" swPath="C:\p\Sub.SLDASM"/>
              <swFile id="3" swPath="C:\p\Bolt.SLDPRT"/>
              <swFile id="4" swPath="C:\p\Nut.SLDPRT"/>
              <swFile id="9" swPath="C:\p\Unused.SLDPRT"/>
            </swHeader><swModelList>
              <swModel id="10" swFileRef="3"/>
              <swModel id="11" swFileRef="4"/>
              <swModel id="12" swFileRef="2">
                <swReference swModelRef="11" swIsVirtualComponent="NO"/>
              </swModel>
              <swModel id="13" swFileRef="1">
                <swReference swModelRef="10" swIsVirtualComponent="NO"/>
                <swReference swModelRef="10" swIsVirtualComponent="NO"/>
                <swReference swModelRef="12" swIsVirtualComponent="NO"/>
                <swReference swModelRef="11" swIsVirtualComponent="YES"/>
                <swReference swModelRef="99"/>
              </swModel>
            </swModelList></swSolidWorks>"#;
        assert_eq!(
            components(xml.as_bytes()),
            ["C:\\p\\Bolt.SLDPRT", "C:\\p\\Sub.SLDASM"]
        );
        // UTF-16 with BOM gives the same result.
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(components(&utf16), components(xml.as_bytes()));
        // Garbage and truncated XML: no panic, nothing or what was read.
        assert!(components(b"\x00\x01\xff<<<").is_empty());
        assert!(components(&xml.as_bytes()[..xml.len() / 3]).is_empty());
    }

    #[test]
    fn synthetic_container_round_trip() {
        let tree = br#"<swSolidWorks><swFile id="1" swPath="D:\a\Wheel.SLDPRT"/><swModel id="2" swFileRef="1"/><swModel id="3"><swReference swModelRef="2"/></swModel></swSolidWorks>"#;
        let bytes = container(&[
            section("Contents/Config-0", &[7u8; 5000]),
            section(COMPONENT_TREE_SECTION, tree),
            section(PREVIEW_SECTION, b"\x89PNG not really"),
        ]);
        let mut sw = SwContainer::open(Cursor::new(&bytes)).unwrap().unwrap();
        let names: Vec<&str> = sw.sections().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["Contents/Config-0", COMPONENT_TREE_SECTION, PREVIEW_SECTION]
        );
        assert_eq!(
            sw.read(PREVIEW_SECTION, 100).unwrap().unwrap(),
            b"\x89PNG not really"
        );
        let refs = reference_data(Cursor::new(&bytes), 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(refs.components.unwrap(), ["D:\\a\\Wheel.SLDPRT"]);
    }

    #[test]
    fn other_files_and_garbage_are_not_containers() {
        for bytes in [
            b"".to_vec(),
            b"ISO-10303-21;".to_vec(),
            b"SPLMSSTR".to_vec(),
            vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0],
            MARKER.to_vec(),
            // marker with a name length beyond the bytes
            [MARKER.to_vec(), vec![0; 16], vec![0xff; 4]].concat(),
        ] {
            assert!(!is_container(&bytes));
            assert!(SwContainer::open(Cursor::new(bytes)).unwrap().is_none());
        }
        // The marker deep inside a file is not the start of a container.
        let mut late = vec![0u8; 200];
        late.extend(section(PREVIEW_SECTION, b"x"));
        assert!(SwContainer::open(Cursor::new(late)).unwrap().is_none());
    }

    #[test]
    fn hostile_sections_are_rejected() {
        let good = section("Good", b"hello hello hello");
        // Sizes beyond the end of the file.
        let mut beyond = raw_section("Beyond", &deflate(b"abc"), 3, crc32fast::hash(b"abc"));
        beyond[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
        // Wrong CRC, wrong size, data that is not Deflate.
        let bad_crc = raw_section("BadCrc", &deflate(b"abc"), 3, 12345);
        let bad_size = raw_section("BadSize", &deflate(b"abcdef"), 3, crc32fast::hash(b"abc"));
        let not_deflate = raw_section("Garbage", &[0xff; 64], 10, 0);
        // A decompression bomb: 64 MiB of zeros claimed as 10 bytes.
        let zeros = vec![0u8; 64 << 20];
        let bomb = raw_section("Bomb", &deflate(&zeros), 10, crc32fast::hash(&zeros[..10]));
        // An honest but huge section: refused by the caller's limit.
        let huge = raw_section(
            "Huge",
            &deflate(&zeros),
            zeros.len() as u32,
            crc32fast::hash(&zeros),
        );
        // A header-like record without data and a non-printable name.
        let pseudo = raw_section("Pseudo", &[0u8; 20], 40, 80);
        let unprintable = raw_section("\u{1}\u{2}", &deflate(b"x"), 1, crc32fast::hash(b"x"));
        let bytes = container(&[
            good,
            bad_crc,
            bad_size,
            not_deflate,
            bomb,
            huge,
            pseudo,
            unprintable,
            beyond,
        ]);
        let mut sw = SwContainer::open(Cursor::new(&bytes)).unwrap().unwrap();
        let names: Vec<String> = sw.sections().iter().map(|s| s.name.clone()).collect();
        assert!(
            !names.iter().any(|n| n == "Pseudo" || n == "Beyond"),
            "{names:?}"
        );
        assert!(!names.iter().any(|n| n.contains('\u{1}')), "{names:?}");
        assert_eq!(sw.read("Good", 100).unwrap().unwrap(), b"hello hello hello");
        for name in ["BadCrc", "BadSize", "Garbage", "Bomb"] {
            assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
            assert!(sw.read(name, u64::MAX).unwrap().is_none(), "{name}");
        }
        assert!(sw.read("Huge", 8 << 20).unwrap().is_none(), "limit");
    }

    #[test]
    fn truncated_files_keep_the_sections_inside() {
        let bytes = fixture("nist_mtc_crada_assembly_rev-D.SLDASM");
        for cut in [700, bytes.len() / 3, bytes.len() / 2, bytes.len() - 1] {
            let part = &bytes[..cut];
            let Some(mut sw) = SwContainer::open(Cursor::new(part)).unwrap() else {
                continue;
            };
            assert!(sw
                .sections()
                .iter()
                .all(|s| s.offset + s.compressed <= cut as u64));
            // Reading never fails on a cut file.
            for s in sw.sections().to_vec() {
                sw.read(&s.name, 1 << 20).unwrap();
            }
            let _ = reference_data(Cursor::new(part), 1 << 20).unwrap();
        }
        // A cut inside the compressed data of one section.
        let s = section(PREVIEW_SECTION, &[9u8; 10_000]);
        let bytes = container(&[s]);
        let cut = &bytes[..bytes.len() - 50];
        let sw = SwContainer::open(Cursor::new(cut)).unwrap();
        assert!(sw.is_none_or(|sw| sw.sections().is_empty()));
    }

    #[test]
    fn search_is_bounded_on_marker_floods() {
        // Thousands of markers with bogus headers: no panic, no sections.
        let mut bytes = container(&[section("A", b"a")]);
        for _ in 0..20_000 {
            bytes.extend_from_slice(&MARKER);
            bytes.extend_from_slice(&[0xff; 26]);
        }
        let mut reader = CountingReader {
            inner: Cursor::new(&bytes),
            reads: 0,
        };
        let sw = SwContainer::open(&mut reader).unwrap().unwrap();
        assert_eq!(sw.sections().len(), 1);
        // Each chunk is read about once, not once per marker (20 000 here):
        // on a real file every refill is a seek and a read system call.
        let chunks = bytes.len() / CHUNK + 1;
        assert!(reader.reads <= 4 * chunks + 8, "{} reads", reader.reads);
    }

    /// Counts the read calls that reach the file.
    struct CountingReader<R> {
        inner: R,
        reads: usize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.inner.read(buf)
        }
    }

    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn bogus_copies_of_a_section_are_tried_only_a_few_times() {
        // Wrong CRCs: each copy inflates completely before it is refused.
        let bad = || raw_section("Dup", &deflate(b"abc"), 3, 1);
        let mut sections: Vec<Vec<u8>> = (0..MAX_TRIES).map(|_| bad()).collect();
        sections.push(section("Dup", b"abc"));
        let bytes = container(&sections);
        let mut sw = SwContainer::open(Cursor::new(&bytes)).unwrap().unwrap();
        assert_eq!(sw.sections().len(), MAX_TRIES + 1);
        assert!(sw.read("Dup", 100).unwrap().is_none(), "beyond the tries");
        // Within the tries the first good copy wins.
        let bytes = container(&[bad(), section("Dup", b"abc"), bad()]);
        let mut sw = SwContainer::open(Cursor::new(&bytes)).unwrap().unwrap();
        assert_eq!(sw.read("Dup", 100).unwrap().unwrap(), b"abc");
    }

    #[test]
    fn damaged_nist_files_never_panic() {
        // Truncations and flipped bytes all over two real files: whatever
        // comes out, every section lies inside the file and reading works.
        for name in [
            "nist_mtc_crada_assembly_rev-D.SLDASM",
            "nist_mtc_crada_box_rev-D.SLDPRT",
        ] {
            let bytes = fixture(name);
            let check = |data: &[u8]| {
                let Some(mut sw) = SwContainer::open(Cursor::new(data)).unwrap() else {
                    return;
                };
                assert!(sw
                    .sections()
                    .iter()
                    .all(|s| s.offset + s.compressed <= data.len() as u64));
                for name in [PREVIEW_SECTION, "Header2", "Contents/Config-0"] {
                    sw.read(name, 1 << 20).unwrap();
                }
                let _ = reference_data(Cursor::new(data), 1 << 20).unwrap();
            };
            let step = bytes.len() / 60;
            for cut in (0..bytes.len()).step_by(step) {
                check(&bytes[..cut]);
            }
            let mut state = 0x2545_f491_4f6c_dd1du64;
            for _ in 0..60 {
                let mut damaged = bytes.clone();
                for _ in 0..8 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let at = (state % damaged.len() as u64) as usize;
                    damaged[at] = (state >> 32) as u8;
                }
                check(&damaged);
            }
        }
    }
}
