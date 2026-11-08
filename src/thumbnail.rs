//! Preview images that CAD programs store inside their own files – the
//! thumbnails Autodesk Vault or Teamcenter show for native files (findings in
//! `docs/src/research/cad-previews.md`). Only the image is read, never the
//! geometry:
//!
//! * Autodesk Inventor (`.ipt`, `.iam`, `.idw`, `.ipn`) and Siemens Solid Edge
//!   (`.par`, `.psm`, `.asm`, `.pwd`, `.dft`) files are OLE compound files
//!   ([MS-CFB]); the preview is a PNG or a Windows DIB in one of their streams,
//!   normally the thumbnail property of the summary information ([MS-OLEPS]);
//! * SolidWorks files up to 2014 are OLE files as well (`PreviewPNG`), newer
//!   ones keep the PNG in a Deflate-compressed `PreviewPNG` section of their
//!   own container (`swpart`);
//! * NX parts keep a JPEG in their stream container (`nxpart`). It is small
//!   and poor, so NX parts get a thumbnail rendered from their 3D preview
//!   instead ([`rendered`], [`render_mesh`]); the JPEG stays the fallback
//!   for NX files without one (assemblies, drawings).
//!
//! Every candidate is validated (PNG chunk structure and checksums, plausible
//! DIB header and sizes); the summary information wins, then streams named
//! like a preview, then the largest image. A DIB is served as BMP, which all
//! browsers and web views display. The layouts follow the documented
//! containers and open-source readers (e.g. `cad-preview`); they were not
//! tested with real files of every CAD program version.
//!
//! [MS-CFB]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cfb/
//! [MS-OLEPS]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-oleps/
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::{
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
};

/// Bump to invalidate every cached thumbnail (2: SOLIDWORKS 2015+ previews,
/// cached as "none" before; 3: NX parts rendered from their 3D preview
/// instead of the embedded JPEG; 4: SOLIDWORKS parts and assemblies rendered
/// from their 3D preview; 5: Inventor parts and assemblies rendered from
/// their 3D preview).
pub const CACHE_VERSION: u32 = 5;

const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Streams larger than this hold geometry, not previews.
const MAX_STREAM: u64 = 8 * 1024 * 1024;
/// Bytes of a file in its own container searched for an embedded PNG.
const MAX_SCAN: u64 = 64 * 1024 * 1024;
const MIN_SIDE: u32 = 8;
const MAX_SIDE: u32 = 4096;

/// Formats whose files carry a preview image.
pub fn supports(format_id: &str) -> bool {
    (format_id.starts_with("inventor-") && format_id != "inventor-project")
        || format_id.starts_with("solidedge-")
        || matches!(
            format_id,
            "solidworks-part" | "solidworks-assembly" | "solidworks-drawing" | "nx-part"
        )
}

/// An image found in a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Thumbnail {
    /// `image/png` or `image/bmp`.
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Found {
    thumbnail: Thumbnail,
    area: u64,
}

fn better(best: &mut Option<Found>, candidate: Option<Found>) {
    let Some(c) = candidate else { return };
    let replace = match best {
        None => true,
        Some(b) => {
            c.area > b.area
                || (c.area == b.area
                    && c.thumbnail.content_type == "image/png"
                    && b.thumbnail.content_type != "image/png")
        }
    };
    if replace {
        *best = Some(c);
    }
}

/// The preview image of a CAD file, if it has one.
pub fn extract(path: &Path) -> io::Result<Option<Thumbnail>> {
    extract_from(io::BufReader::new(File::open(path)?))
}

/// [`extract`] for any seekable reader (the fuzz targets pass bytes).
pub fn extract_from<R: Read + Seek>(mut file: R) -> io::Result<Option<Thumbnail>> {
    let mut head = [0u8; 1024];
    let n = read_up_to(&mut file, &mut head)?;
    let head = head.get(..n).unwrap_or(&head);
    file.rewind()?;
    if head.starts_with(&OLE_MAGIC) {
        return Ok(from_compound_file(file));
    }
    if head.starts_with(crate::nxpart::MAGIC) {
        return from_nx_part(file);
    }
    if crate::swpart::is_container(head) {
        return from_sw_container(file);
    }
    let mut data = Vec::new();
    file.take(MAX_SCAN).read_to_end(&mut data)?;
    Ok(find_in(&data, false).map(|f| f.thumbnail))
}

/// SOLIDWORKS 2015+ files: the PNG of the `PreviewPNG` section.
fn from_sw_container<R: Read + Seek>(file: R) -> io::Result<Option<Thumbnail>> {
    let Some(mut sw) = crate::swpart::SwContainer::open(file)? else {
        return Ok(None);
    };
    let Some(data) = sw.read(crate::swpart::PREVIEW_SECTION, MAX_STREAM)? else {
        return Ok(None);
    };
    Ok(png_at(&data).map(|f| f.thumbnail))
}

/// NX parts: the JPEG in the `/Root/images/preview` stream.
fn from_nx_part<R: Read + Seek>(file: R) -> io::Result<Option<Thumbnail>> {
    let Some(mut part) = crate::nxpart::NxPart::open(file)? else {
        return Ok(None);
    };
    let Some(data) = part.read(crate::nxpart::PREVIEW_STREAM, MAX_STREAM)? else {
        return Ok(None);
    };
    Ok(jpeg_size(&data)
        .filter(|&(w, h)| plausible(w, h))
        .map(|_| Thumbnail {
            content_type: "image/jpeg",
            bytes: data,
        }))
}

/// Width and height of a baseline or progressive JPEG, read from its frame
/// header (SOFn); `None` if the segment structure is broken.
fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    loop {
        // Fill bytes before a marker are allowed.
        while *data.get(i)? == 0xFF && *data.get(i + 1)? == 0xFF {
            i += 1;
        }
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
        if len < 2 {
            return None;
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let h = u16::from_be_bytes([*data.get(i + 5)?, *data.get(i + 6)?]);
            let w = u16::from_be_bytes([*data.get(i + 7)?, *data.get(i + 8)?]);
            return Some((u32::from(w), u32::from(h)));
        }
        if marker == 0xDA || marker == 0xD9 {
            return None; // image data before any frame header
        }
        i += 2 + len;
    }
}

fn read_up_to<R: Read>(file: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while let Some(rest) = buf.get_mut(n..).filter(|r| !r.is_empty()) {
        match file.read(rest)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Summary information first, then streams named like a preview, then the
/// largest image of any stream.
fn from_compound_file<F: Read + Seek>(inner: F) -> Option<Thumbnail> {
    let mut cfb = cfb::CompoundFile::open(inner).ok()?;
    let mut streams: Vec<(u8, PathBuf)> = cfb
        .walk()
        .filter(|e| e.is_stream() && e.len() <= MAX_STREAM)
        .map(|e| {
            let name = e.name().to_ascii_lowercase();
            let rank = if name.ends_with("summaryinformation") {
                0
            } else if name.contains("preview") || name.contains("thumbnail") {
                1
            } else {
                2
            };
            (rank, e.path().to_path_buf())
        })
        .collect();
    streams.sort_by_key(|(rank, _)| *rank);
    let mut best: Option<Found> = None;
    let mut best_rank = u8::MAX;
    for (rank, path) in streams {
        if best.is_some() && rank > best_rank {
            break; // a preview of a better-ranked stream was found
        }
        let mut data = Vec::new();
        let Ok(mut stream) = cfb.open_stream(&path) else {
            continue;
        };
        if stream.read_to_end(&mut data).is_err() {
            continue;
        }
        let found = find_in(&data, true);
        if found.is_some() && best.is_none() {
            best_rank = rank;
        }
        better(&mut best, found);
    }
    best.map(|f| f.thumbnail)
}

/// The largest valid image in `data` (PNGs; DIBs too when `dibs`).
fn find_in(data: &[u8], dibs: bool) -> Option<Found> {
    let mut best = None;
    let mut from = 0;
    while let Some(pos) = data
        .get(from..)
        .and_then(|rest| rest.windows(PNG_MAGIC.len()).position(|w| w == PNG_MAGIC))
    {
        let start = from + pos;
        if let Some(rest) = data.get(start..) {
            better(&mut best, png_at(rest));
        }
        from = start + 1;
    }
    if dibs {
        for i in 0..data.len().saturating_sub(40) {
            let Some(rest) = data.get(i..) else { break };
            if matches!(le_u32(rest, 0), Some(40 | 108 | 124)) {
                better(&mut best, dib_at(rest));
            }
        }
    }
    best
}

/// Four bytes at `o` as an array (`None` past the end).
fn quad(data: &[u8], o: usize) -> Option<[u8; 4]> {
    data.get(o..o.checked_add(4)?)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
}

fn le_u32(data: &[u8], o: usize) -> Option<u32> {
    quad(data, o).map(u32::from_le_bytes)
}

fn plausible(width: u32, height: u32) -> bool {
    (MIN_SIDE..=MAX_SIDE).contains(&width) && (MIN_SIDE..=MAX_SIDE).contains(&height)
}

/// A complete PNG at the start of `data`: every chunk complete with a valid
/// checksum, `IHDR` first, `IEND` last.
fn png_at(data: &[u8]) -> Option<Found> {
    let be32 = |o: usize| quad(data, o).map(u32::from_be_bytes);
    let mut pos = PNG_MAGIC.len();
    let (mut width, mut height) = (0, 0);
    loop {
        let len = usize::try_from(be32(pos)?).ok()?;
        let end = pos.checked_add(12)?.checked_add(len)?;
        let kind = data.get(pos + 4..pos + 8)?;
        if end > data.len() || !kind.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
        if crc32fast::hash(data.get(pos + 4..end - 4)?) != be32(end - 4)? {
            return None;
        }
        if pos == PNG_MAGIC.len() {
            if kind != b"IHDR" || len != 13 {
                return None;
            }
            width = be32(pos + 8)?;
            height = be32(pos + 12)?;
        }
        pos = end;
        if kind == b"IEND" {
            break;
        }
    }
    if !plausible(width, height) {
        return None;
    }
    Some(Found {
        area: u64::from(width) * u64::from(height),
        thumbnail: Thumbnail {
            content_type: "image/png",
            bytes: data.get(..pos)?.to_vec(),
        },
    })
}

/// A device-independent bitmap at the start of `data` (BITMAPINFOHEADER, V4
/// or V5, uncompressed or bit fields), returned as a BMP file.
fn dib_at(data: &[u8]) -> Option<Found> {
    let le32 = |o: usize| le_u32(data, o);
    let le16 = |o: usize| {
        data.get(o..o + 2)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map(u16::from_le_bytes)
    };
    let header = usize::try_from(le32(0)?).ok()?;
    if !matches!(header, 40 | 108 | 124) || data.len() < header {
        return None;
    }
    let width = le32(4)? as i32;
    let height = le32(8)? as i32;
    let (planes, bits, compression) = (le16(12)?, le16(14)?, le32(16)?);
    let (image_size, colors_used) = (le32(20)?, le32(32)?);
    if planes != 1
        || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32)
        || !(compression == 0 || (compression == 3 && matches!(bits, 16 | 32)))
        || colors_used > 256
    {
        return None;
    }
    let (w, h) = (u32::try_from(width).ok()?, height.unsigned_abs());
    if !plausible(w, h) {
        return None;
    }
    let palette = 4 * if bits <= 8 && colors_used == 0 {
        1usize << bits
    } else {
        colors_used as usize
    };
    let masks = if compression == 3 && header == 40 {
        12
    } else {
        0
    };
    let stride = (w as usize * bits as usize).div_ceil(32) * 4;
    let pixels = stride.checked_mul(h as usize)?;
    // The size field is 0 for uncompressed bitmaps or close to the real size.
    if image_size != 0 && !(pixels / 2..=pixels * 2).contains(&(image_size as usize)) {
        return None;
    }
    let total = header + masks + palette + pixels;
    if total > data.len() {
        return None;
    }
    let file_size = u32::try_from(14 + total).ok()?;
    let offset = u32::try_from(14 + header + masks + palette).ok()?;
    let mut bmp = Vec::with_capacity(14 + total);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&offset.to_le_bytes());
    bmp.extend_from_slice(data.get(..total)?);
    Some(Found {
        area: u64::from(w) * u64::from(h),
        thumbnail: Thumbnail {
            content_type: "image/bmp",
            bytes: bmp,
        },
    })
}

/// The cache directory and file name stem of a blob's thumbnail.
fn cache_base(cache_dir: &Path, sha256: &str) -> io::Result<(PathBuf, String)> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid content hash",
        ));
    }
    let dir = cache_dir.join(sha256.get(..2).unwrap_or_default());
    Ok((dir, format!("{sha256}.thumb.v{CACHE_VERSION}")))
}

/// A thumbnail already in the cache: `Some(None)` when the blob is known
/// to have none, `None` when it was never looked at.
pub fn lookup(cache_dir: &Path, sha256: &str) -> io::Result<Option<Option<Thumbnail>>> {
    let (dir, base) = cache_base(cache_dir, sha256)?;
    for (ext, content_type) in [
        ("png", "image/png"),
        ("bmp", "image/bmp"),
        ("jpg", "image/jpeg"),
    ] {
        if let Ok(bytes) = std::fs::read(dir.join(format!("{base}.{ext}"))) {
            return Ok(Some(Some(Thumbnail {
                content_type,
                bytes,
            })));
        }
    }
    if dir.join(format!("{base}.none")).exists() {
        return Ok(Some(None));
    }
    Ok(None)
}

/// Stores a thumbnail (rendered or found in the file) in the cache.
pub fn store(cache_dir: &Path, sha256: &str, found: Option<&Thumbnail>) -> io::Result<()> {
    let (dir, base) = cache_base(cache_dir, sha256)?;
    std::fs::create_dir_all(&dir)?;
    let (name, bytes): (String, &[u8]) = match found {
        Some(t) if t.content_type == "image/png" => (format!("{base}.png"), &t.bytes),
        Some(t) if t.content_type == "image/jpeg" => (format!("{base}.jpg"), &t.bytes),
        Some(t) => (format!("{base}.bmp"), &t.bytes),
        None => (format!("{base}.none"), b""),
    };
    // write to a temporary name first, so a reader never sees half a file
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(name))
}

/// The thumbnail of a vault blob, cached next to the 3D previews
/// (`previews/ab/<sha256>.thumb.v<N>.png|bmp|jpg|none`).
pub fn cached(cache_dir: &Path, sha256: &str, blob: &Path) -> io::Result<Option<Thumbnail>> {
    if let Some(known) = lookup(cache_dir, sha256)? {
        return Ok(known);
    }
    let found = extract(blob)?;
    store(cache_dir, sha256, found.as_ref())?;
    Ok(found)
}

/// Side of the thumbnails rendered from a 3D preview.
pub const RENDERED_SIZE: u32 = 512;

/// Formats whose thumbnail is rendered from their 3D preview instead of the
/// image stored in the file: NX parts (the embedded JPEG is small and poor,
/// the tessellation in the file is good), SOLIDWORKS and Inventor parts and
/// assemblies (one look for every CAD system). Files without a 3D preview
/// (drawings, assemblies none of whose parts are in TeamCraft) keep the
/// stored image.
pub fn rendered(format_id: &str) -> bool {
    matches!(
        format_id,
        "nx-part"
            | "solidworks-part"
            | "solidworks-assembly"
            | "inventor-part"
            | "inventor-assembly"
    )
}

/// A PNG thumbnail drawn from a cached preview mesh (`TCMESH01`).
pub fn render_mesh(mesh_bytes: &[u8]) -> Option<Thumbnail> {
    let mesh = crate::mesh::Mesh::decode(mesh_bytes).ok()?;
    mesh.check().ok()?;
    crate::mesh::render::png(&mesh, RENDERED_SIZE).map(|bytes| Thumbnail {
        content_type: "image/png",
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    /// A minimal valid PNG (`w` × `h`, one IDAT chunk – its content is not
    /// decoded by the extractor).
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
        }
        let mut out = PNG_MAGIC.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(
            &mut out,
            b"IDAT",
            &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A 24-bit DIB (BITMAPINFOHEADER + pixels).
    fn dib(w: u32, h: u32) -> Vec<u8> {
        let stride = (w as usize * 24).div_ceil(32) * 4;
        let mut out = Vec::new();
        for v in [40u32, w, h] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        for v in [0u32, 0, 2835, 2835, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend((0..stride * h as usize).map(|i| (i % 251) as u8));
        out
    }

    fn compound(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        for (name, data) in streams {
            if let Some(parent) = Path::new(name).parent().filter(|p| *p != Path::new("/")) {
                if !cfb.exists(parent) {
                    cfb.create_storage_all(parent).unwrap();
                }
            }
            cfb.create_stream(name).unwrap().write_all(data).unwrap();
        }
        cfb.flush().unwrap();
        cfb.into_inner().into_inner()
    }

    /// A file or directory below the system temp dir, removed when dropped.
    struct Temp(PathBuf);

    impl Temp {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp_name() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "teamcraft-thumb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write_temp(bytes: &[u8]) -> Temp {
        let path = temp_name();
        std::fs::write(&path, bytes).unwrap();
        Temp(path)
    }

    #[test]
    fn inventor_style_png_in_the_summary_information() {
        // VT_CF clipboard data: size, format tag, then the image
        let mut summary = vec![0u8; 48];
        summary.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
        summary.extend_from_slice(&png(512, 512));
        let file = compound(&[
            ("/\u{5}SummaryInformation", summary),
            // an appearance texture elsewhere must not win
            ("/RSeStorage/Texture", png(1024, 1024)),
        ]);
        let t = from_compound_file(Cursor::new(file)).unwrap();
        assert_eq!(t.content_type, "image/png");
        assert_eq!(t.bytes, png(512, 512));
    }

    #[test]
    fn solid_edge_style_dib_becomes_a_bmp() {
        let mut summary = vec![1, 2, 3, 4, 0xFF, 0xFF, 0xFF, 0xFF, 8, 0, 0, 0];
        summary.extend_from_slice(&dib(96, 64));
        let file = compound(&[("/\u{5}SummaryInformation", summary)]);
        let t = from_compound_file(Cursor::new(file)).unwrap();
        assert_eq!(t.content_type, "image/bmp");
        assert_eq!(&t.bytes[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(t.bytes[2..6].try_into().unwrap()) as usize,
            t.bytes.len()
        );
        assert_eq!(u32::from_le_bytes(t.bytes[10..14].try_into().unwrap()), 54);
    }

    #[test]
    fn preview_streams_and_plain_files() {
        // old SolidWorks: PreviewPNG stream
        let file = compound(&[("/PreviewPNG", png(640, 480)), ("/Contents", vec![7; 4096])]);
        let t = from_compound_file(Cursor::new(file)).unwrap();
        assert_eq!(t.bytes, png(640, 480));
        // own container with an embedded PNG
        let mut bytes = vec![0x13; 1000];
        bytes.extend_from_slice(&png(640, 480));
        bytes.extend_from_slice(&[0x42; 100]);
        let f = write_temp(&bytes);
        assert_eq!(extract(f.path()).unwrap().unwrap().bytes, png(640, 480));
        // nothing valid: truncated or broken checksum
        let mut broken = png(64, 64);
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        let f = write_temp(&broken);
        assert_eq!(extract(f.path()).unwrap(), None);
        let f = write_temp(&png(64, 64)[..40]);
        assert_eq!(extract(f.path()).unwrap(), None);
        // implausible sizes are ignored
        assert!(png_at(&png(2, 2)).is_none());
        assert!(dib_at(&dib(4, 4)).is_none());
    }

    #[test]
    fn results_are_cached() {
        let dir = Temp(temp_name());
        std::fs::create_dir_all(dir.path()).unwrap();
        let blob = write_temp(&compound(&[("/PreviewPNG", png(32, 32))]));
        let sha = "ab".repeat(32);
        let first = cached(dir.path(), &sha, blob.path()).unwrap().unwrap();
        assert!(dir
            .path()
            .join("ab")
            .join(format!("{sha}.thumb.v{CACHE_VERSION}.png"))
            .exists());
        drop(blob); // the cache answers without the blob
        let missing = dir.path().join("gone");
        assert_eq!(cached(dir.path(), &sha, &missing).unwrap().unwrap(), first);
        let empty = write_temp(b"no preview here");
        let other = "cd".repeat(32);
        assert_eq!(cached(dir.path(), &other, empty.path()).unwrap(), None);
        assert!(dir
            .path()
            .join("cd")
            .join(format!("{other}.thumb.v{CACHE_VERSION}.none"))
            .exists());
        assert!(cached(dir.path(), "../x", empty.path()).is_err());
        assert!(supports("inventor-assembly") && supports("solidedge-weldment"));
        assert!(supports("nx-part"));
        assert!(!supports("inventor-project") && !supports("step"));
    }

    #[test]
    fn the_largest_image_wins() {
        let mut data = vec![0x11; 50];
        data.extend(png(64, 64));
        data.extend([0x22; 10]);
        data.extend(png(256, 128));
        data.extend([0x33; 10]);
        data.extend(png(100, 100));
        let found = find_in(&data, false).unwrap();
        assert_eq!(found.thumbnail.bytes, png(256, 128));
        assert_eq!(found.area, 256 * 128);
        // A bigger DIB only counts when DIBs are looked for.
        let mut with_dib = data.clone();
        with_dib.extend(dib(400, 300));
        assert_eq!(
            find_in(&with_dib, false).unwrap().thumbnail.content_type,
            "image/png"
        );
        assert_eq!(
            find_in(&with_dib, true).unwrap().thumbnail.content_type,
            "image/bmp"
        );
        assert!(find_in(b"nothing", true).is_none());
    }

    #[test]
    fn size_limits() {
        assert!(plausible(MIN_SIDE, MAX_SIDE) && plausible(MAX_SIDE, MIN_SIDE));
        assert!(!plausible(MIN_SIDE - 1, 64) && !plausible(64, MAX_SIDE + 1));
        assert!(png_at(&png(8, 8)).is_some());
        assert!(png_at(&png(4097, 16)).is_none());
    }

    #[test]
    fn png_structure_is_checked() {
        // The first chunk must be IHDR.
        let good = png(32, 32);
        let mut renamed = good.clone();
        renamed[12..16].copy_from_slice(b"IHDX");
        assert!(png_at(&renamed).is_none());
        // Chunk types are letters only.
        let mut bad_kind = good.clone();
        bad_kind[12] = b'1';
        assert!(png_at(&bad_kind).is_none());
        // Without IEND the image is incomplete.
        assert!(png_at(&good[..good.len() - 12]).is_none());
        // Trailing bytes after IEND are not part of the image.
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"garbage");
        assert_eq!(png_at(&trailing).unwrap().thumbnail.bytes, good);
    }

    #[test]
    fn dib_variants() {
        // Top-down bitmaps have a negative height.
        let mut top_down = dib(16, 16);
        top_down[8..12].copy_from_slice(&(-16i32).to_le_bytes());
        let found = dib_at(&top_down).unwrap();
        assert_eq!(found.area, 256);
        // Run-length compression is not supported.
        let mut rle = dib(16, 16);
        rle[16..20].copy_from_slice(&1u32.to_le_bytes());
        assert!(dib_at(&rle).is_none());
        // A size field far from the real size means it is no bitmap.
        let mut bogus_size = dib(16, 16);
        bogus_size[20..24].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(dib_at(&bogus_size).is_none());
        // Missing pixels.
        let full = dib(16, 16);
        assert!(dib_at(&full[..full.len() - 1]).is_none());
        // Two planes are invalid.
        let mut planes = dib(16, 16);
        planes[12..14].copy_from_slice(&2u16.to_le_bytes());
        assert!(dib_at(&planes).is_none());
    }

    #[test]
    fn eight_bit_dibs_carry_their_palette() {
        let (w, h) = (16u32, 8u32);
        let mut d = Vec::new();
        for v in [40u32, w, h] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&8u16.to_le_bytes());
        for v in [0u32, 0, 0, 0, 0, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend(vec![0x40; 256 * 4]); // palette of 256 colours
        d.extend(vec![1; (w * h) as usize]);
        let bmp = dib_at(&d).unwrap().thumbnail.bytes;
        let offset = u32::from_le_bytes(bmp[10..14].try_into().unwrap());
        assert_eq!(offset, 14 + 40 + 1024, "pixels follow the palette");
        assert_eq!(bmp.len(), 14 + d.len());
    }

    #[test]
    fn jpeg_size_skips_other_segments() {
        // A DHT (0xC4) segment before the frame header is not a frame header.
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x04, 0xAA, 0xBB];
        d.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x20, 0x00, 0x40]);
        d.extend_from_slice(&[0; 10]);
        assert_eq!(jpeg_size(&d), Some((64, 32)));
        // A segment length below 2 is broken.
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 0, 0]), None);
    }

    #[test]
    fn missing_files_are_errors_and_plain_files_have_no_preview() {
        assert!(extract(&temp_name()).is_err());
        let f = write_temp(b"just some text without images");
        assert_eq!(extract(f.path()).unwrap(), None);
    }

    /// SOI, APP0 (JFIF, 16 bytes), fill byte, SOF0 with `w` × `h`.
    fn jpeg_header(w: u16, h: u16) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        d.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        d.extend_from_slice(&[0xFF, 0xFF, 0xC0, 0x00, 0x11, 0x08]);
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&[0x03; 10]);
        d
    }

    #[test]
    fn jpeg_size_reads_the_frame_header() {
        assert_eq!(jpeg_size(&jpeg_header(320, 200)), Some((320, 200)));
        // Progressive (SOF2) works the same way.
        let mut p = jpeg_header(64, 48);
        assert_eq!(p[22], 0xC0);
        p[22] = 0xC2;
        assert_eq!(jpeg_size(&p), Some((64, 48)));
        // Broken or cut-off data never panics.
        assert_eq!(jpeg_size(b"\xFF\xD8\xFF\xE0\x00"), None);
        assert_eq!(jpeg_size(b"\xFF\xD8\xFF\xE0\x00\x01"), None);
        assert_eq!(jpeg_size(b"\xFF\xD8\xFF\xDA\x00\x08"), None);
        assert_eq!(jpeg_size(b"not a jpeg"), None);
        let full = jpeg_header(320, 200);
        for cut in 0..full.len() {
            let _ = jpeg_size(&full[..cut]);
        }
    }
}
