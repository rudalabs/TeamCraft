//! Autodesk Fusion design archives (`.f3d`).
//!
//! A design is a ZIP archive with one folder per document
//! (`FusionAssetName[Active]/…`). Most entries are compressed with
//! Zstandard (ZIP method 93), a few are stored; Deflate is accepted too.
//! The `zip` crate reads the directory; Zstandard entries are inflated with
//! `ruzstd` (pure Rust) and checked against the stored size and CRC-32.
//!
//! Entries used by TeamCraft (see `mesh::f3d` and
//! `docs/src/research/fusion-ogs-preview.md`):
//!
//! * `OGS.BlobFolder/OGS/DefaultScene/world` and `stream_mesh_NNN`: the
//!   display scene of Fusion's graphics system with the triangles of every
//!   face (the 3D preview);
//! * `Previews/big.png`: the thumbnail Fusion rendered when saving.
//!
//! Hostile input: an entry is inflated only when its stored size is within
//! the caller's limit, inflating stops one byte after that size, and the
//! result must have exactly the stored size and CRC-32.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::io::{self, Read, Seek};

use zip::CompressionMethod;

/// Folder of the display scene inside a document folder.
pub const SCENE_DIR: &str = "OGS.BlobFolder/OGS/DefaultScene/";
/// The scene graph of the display scene.
pub const SCENE_WORLD: &str = "world";
/// The thumbnail Fusion stores.
pub const PREVIEW_PNG: &str = "Previews/big.png";
/// Document folders start with this name; the open document is `[Active]`.
const ACTIVE: &str = "FusionAssetName[Active]/";
const DOCUMENT: &str = "FusionAssetName[";
const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";

/// Whether `head` (the first bytes of a file) starts a Fusion archive: a ZIP
/// whose first entry is a Fusion document folder or manifest.
pub fn is_archive(head: &[u8]) -> bool {
    let Some(name) = head
        .get(26..28)
        .and_then(|n| <[u8; 2]>::try_from(n).ok())
        .map(|n| usize::from(u16::from_le_bytes(n)))
        .and_then(|len| head.get(30..30 + len))
    else {
        return false;
    };
    head.starts_with(ZIP_MAGIC)
        && (name.starts_with(DOCUMENT.as_bytes())
            || name == b"Manifest.dat"
            || name == b"Properties.dat")
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// An opened design archive.
pub struct Archive<R> {
    zip: zip::ZipArchive<R>,
    names: Vec<String>,
}

impl<R: Read + Seek> Archive<R> {
    /// Opens a Fusion archive; `None` when the file is not a ZIP with a
    /// Fusion document folder.
    pub fn open(reader: R) -> io::Result<Option<Self>> {
        let Ok(zip) = zip::ZipArchive::new(reader) else {
            return Ok(None);
        };
        let names: Vec<String> = zip
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
            .collect();
        let archive = Archive { zip, names };
        if archive.names().any(|n| n.starts_with(DOCUMENT)) {
            Ok(Some(archive))
        } else {
            Ok(None)
        }
    }

    /// The entry names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    /// The full name of `path` (relative to a document folder) in the open
    /// document, or else in the first document that has it.
    pub fn entry(&self, path: &str) -> Option<String> {
        let active = format!("{ACTIVE}{path}");
        if self.names().any(|n| n == active) {
            return Some(active);
        }
        let mut found: Vec<&str> = self
            .names()
            .filter(|n| n.starts_with(DOCUMENT) && n.ends_with(path))
            .filter(|n| {
                n.find("]/")
                    .is_some_and(|end| n.get(end + 2..) == Some(path))
            })
            .collect();
        found.sort_unstable();
        found.first().map(|n| (*n).to_string())
    }

    /// The content of the entry `name`; `None` when there is no such entry.
    /// Entries larger than `limit` bytes are an error, not read.
    pub fn read(&mut self, name: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let (method, size, crc) = match self.zip.by_name_raw(name.as_bytes()) {
            Ok(file) => (file.compression(), file.size(), file.crc32()),
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(invalid(format!("damaged archive entry {name}: {e}"))),
        };
        if size > limit {
            return Err(invalid(format!(
                "the archive entry {name} is larger than {limit} bytes"
            )));
        }
        let capacity = usize::try_from(size).unwrap_or(0);
        let mut data = Vec::with_capacity(capacity.min(64 * 1024 * 1024));
        if method == CompressionMethod::ZSTD {
            let raw = self
                .zip
                .by_name_raw(name.as_bytes())
                .map_err(|e| invalid(format!("damaged archive entry {name}: {e}")))?;
            let decoder = ruzstd::decoding::StreamingDecoder::new(raw)
                .map_err(|e| invalid(format!("damaged Zstandard data in {name}: {e}")))?;
            decoder
                .take(size.saturating_add(1))
                .read_to_end(&mut data)
                .map_err(|e| invalid(format!("damaged Zstandard data in {name}: {e}")))?;
        } else if method == CompressionMethod::Stored || method == CompressionMethod::Deflated {
            let file = self
                .zip
                .by_name(name)
                .map_err(|e| invalid(format!("damaged archive entry {name}: {e}")))?;
            file.take(size.saturating_add(1))
                .read_to_end(&mut data)
                .map_err(|e| invalid(format!("damaged archive entry {name}: {e}")))?;
        } else {
            return Err(invalid(format!(
                "the archive entry {name} uses an unsupported compression ({method:?})"
            )));
        }
        if data.len() as u64 != size || crc32fast::hash(&data) != crc {
            return Err(invalid(format!(
                "the archive entry {name} does not match its stored size and checksum"
            )));
        }
        Ok(Some(data))
    }
}

/// Builders for small design archives, for the tests and the fuzz corpus
/// (no real Fusion file is committed: their licences are unknown).
#[doc(hidden)]
#[allow(clippy::indexing_slicing)]
pub mod sample {
    /// One face mesh: positions (centimetres), its normal, triangle indices.
    pub type Face = (Vec<[f32; 3]>, [f32; 3], Vec<u32>);

    /// A ZIP archive of `entries` (name, method, raw data as stored,
    /// uncompressed data for the CRC and size). Methods: 0 stored, 8
    /// Deflate, 93 Zstandard – the caller compresses.
    pub fn zip(entries: &[(&str, u16, &[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, method, raw, plain) in entries {
            let offset = out.len() as u32;
            let crc = crc32fast::hash(plain);
            let header = |sig: &[u8], central: bool| {
                let mut h = Vec::new();
                h.extend_from_slice(sig);
                if central {
                    h.extend_from_slice(&63u16.to_le_bytes());
                }
                h.extend_from_slice(&20u16.to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes());
                h.extend_from_slice(&method.to_le_bytes());
                h.extend_from_slice(&[0, 0, 0x21, 0]);
                h.extend_from_slice(&crc.to_le_bytes());
                h.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                h.extend_from_slice(&(plain.len() as u32).to_le_bytes());
                h.extend_from_slice(&(name.len() as u16).to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes());
                if central {
                    // comment length, disk, internal and external attributes
                    h.extend_from_slice(&[0; 10]);
                    h.extend_from_slice(&offset.to_le_bytes());
                }
                h.extend_from_slice(name.as_bytes());
                h
            };
            out.extend(header(b"PK\x03\x04", false));
            out.extend_from_slice(raw);
            central.extend(header(b"PK\x01\x02", true));
        }
        let start = out.len() as u32;
        let count = entries.len() as u16;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// `data` as one Zstandard frame of raw (uncompressed) blocks – valid
    /// Zstandard that every decoder reads, without needing an encoder.
    pub fn zstd_raw(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x28, 0xb5, 0x2f, 0xfd];
        // frame header: single segment, 8-byte content size
        out.push(0b1110_0000);
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        let mut chunks = data.chunks(100_000).peekable();
        if chunks.peek().is_none() {
            out.extend_from_slice(&[1, 0, 0]);
        }
        while let Some(chunk) = chunks.next() {
            let last = u32::from(chunks.peek().is_none());
            let header = ((chunk.len() as u32) << 3) | last;
            out.extend_from_slice(&header.to_le_bytes()[..3]);
            out.extend_from_slice(chunk);
        }
        out
    }

    fn utf16(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        for c in s.encode_utf16() {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }

    /// A display scene: `faces` are triangle meshes (positions in
    /// centimetres, one normal per face, indices) of client type `Face`,
    /// closed by one `Body`; a sketch profile and an edge are added as
    /// noise. Returns `(world, stream_mesh_000)`.
    pub fn scene(faces: &[Face]) -> (Vec<u8>, Vec<u8>) {
        let mut world = Vec::new();
        let mut stream = Vec::new();
        utf16(&mut world, "ARenderList");
        let mut first_format = true;
        let mut mesh = |world: &mut Vec<u8>,
                        stream: &mut Vec<u8>,
                        context: &str,
                        primitive: u32,
                        points: &[[f32; 3]],
                        normal: [f32; 3],
                        indices: Option<&[u32]>| {
            utf16(world, context);
            world.extend_from_slice(&[0; 9]);
            utf16(world, "OgsSimpleMesh");
            world.extend_from_slice(&[0; 24]);
            world.extend_from_slice(&primitive.to_le_bytes());
            world.extend_from_slice(&[1, 0, 1]);
            utf16(world, "VertexFormat");
            utf16(world, "000001C7C09A9610");
            if first_format {
                first_format = false;
                world.extend_from_slice(&3u32.to_le_bytes());
                for (kind, semantic, offset) in [(2u32, 0u32, 0u32), (2, 1, 3), (1, 2, 6)] {
                    for v in [kind, semantic, offset, 0, 0, 0] {
                        world.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
            let start = stream.len() as u32;
            for p in points {
                for v in p.iter().chain(&normal).chain(&[0.0, 0.0]) {
                    stream.extend_from_slice(&v.to_le_bytes());
                }
            }
            for v in [1, 0, start, stream.len() as u32 - start] {
                world.extend_from_slice(&v.to_le_bytes());
            }
            match indices {
                Some(indices) => {
                    world.push(1);
                    utf16(world, "AIndexBuffer");
                    utf16(world, &format!("000001D04B6E{:04X}", stream.len() % 0xffff));
                    let start = stream.len() as u32;
                    for i in indices {
                        stream.extend_from_slice(&i.to_le_bytes());
                    }
                    world.extend_from_slice(&0u32.to_le_bytes());
                    world.push(0);
                    world.extend_from_slice(&start.to_le_bytes());
                    world.extend_from_slice(&(stream.len() as u32 - start).to_le_bytes());
                }
                None => world.push(0),
            }
            utf16(world, "DepthPriorityAttribute");
            world.extend_from_slice(&[0; 12]);
        };
        let square = [
            [0.0, 0.0, 0.0],
            [5.0, 0.0, 0.0],
            [5.0, 5.0, 0.0],
            [0.0, 5.0, 0.0],
        ];
        mesh(
            &mut world,
            &mut stream,
            "ProfileFace",
            4,
            &square,
            [0.0, 0.0, 1.0],
            Some(&[0u32, 1, 2, 2, 3, 0][..]),
        );
        utf16(&mut world, "Faces");
        for (points, normal, indices) in faces {
            mesh(
                &mut world,
                &mut stream,
                "Face",
                4,
                points,
                *normal,
                Some(indices.as_slice()),
            );
        }
        utf16(&mut world, "Body");
        mesh(
            &mut world,
            &mut stream,
            "Edge",
            3,
            &square[..2],
            [0.0, 0.0, 1.0],
            None,
        );
        utf16(&mut world, "WorldSerializerEndMark");
        (world, stream)
    }

    /// The six faces of an axis-aligned cube with edge `a` (centimetres).
    pub fn cube_faces(a: f32) -> Vec<Face> {
        let mut faces = Vec::new();
        for axis in 0..3 {
            for side in [0.0f32, 1.0] {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let mut points = Vec::new();
                for (du, dv) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                    let mut p = [0.0; 3];
                    p[axis] = side * a;
                    p[u] = du * a;
                    p[v] = dv * a;
                    points.push(p);
                }
                let mut normal = [0.0; 3];
                normal[axis] = if side > 0.5 { 1.0 } else { -1.0 };
                let indices = if side > 0.5 {
                    vec![0, 1, 2, 2, 3, 0]
                } else {
                    vec![0, 2, 1, 2, 0, 3]
                };
                faces.push((points, normal, indices));
            }
        }
        faces
    }

    /// A design archive with a cube of edge `a` cm in its display scene
    /// (Zstandard entries like Fusion writes) and a stored PNG thumbnail.
    pub fn cube_archive(a: f32, png: &[u8]) -> Vec<u8> {
        let (world, stream) = scene(&cube_faces(a));
        let world_name = format!("FusionAssetName[Active]/{}{}", super::SCENE_DIR, "world");
        let stream_name = format!(
            "FusionAssetName[Active]/{}{}",
            super::SCENE_DIR,
            "stream_mesh_000"
        );
        let png_name = format!("FusionAssetName[Active]/{}", super::PREVIEW_PNG);
        let manifest = b"manifest".to_vec();
        let (zm, zw, zs) = (zstd_raw(&manifest), zstd_raw(&world), zstd_raw(&stream));
        zip(&[
            ("FusionAssetName[Active]/", 0, &[][..], &[][..]),
            ("Manifest.dat", 93, &zm[..], &manifest[..]),
            (&world_name, 93, &zw[..], &world[..]),
            (&stream_name, 93, &zs[..], &stream[..]),
            (&png_name, 0, png, png),
        ])
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn zstandard_and_stored_entries_are_read_and_checked() {
        let archive = sample::cube_archive(1.0, b"\x89PNG fake");
        assert!(is_archive(&archive));
        let mut a = Archive::open(Cursor::new(archive.clone()))
            .unwrap()
            .unwrap();
        let world = a.entry(&format!("{SCENE_DIR}{SCENE_WORLD}")).unwrap();
        assert_eq!(world, format!("FusionAssetName[Active]/{SCENE_DIR}world"));
        let data = a.read(&world, 1 << 20).unwrap().unwrap();
        assert!(data.len() > 1000);
        let png = a.entry(PREVIEW_PNG).unwrap();
        assert_eq!(a.read(&png, 100).unwrap().unwrap(), b"\x89PNG fake");
        assert_eq!(a.read("missing", 100).unwrap(), None);
        // larger than the limit: refused, not read
        assert!(a.read(&world, 10).is_err());
    }

    #[test]
    fn a_wrong_checksum_or_size_is_an_error() {
        let plain = b"display scene".to_vec();
        let mut other = plain.clone();
        other[0] ^= 1;
        // stored data that does not match the CRC of the central directory
        let z = sample::zstd_raw(&other);
        let bad = sample::zip(&[
            ("FusionAssetName[Active]/", 0, &[][..], &[][..]),
            ("FusionAssetName[Active]/x", 93, &z[..], &plain[..]),
        ]);
        let mut a = Archive::open(Cursor::new(bad)).unwrap().unwrap();
        assert!(a.read("FusionAssetName[Active]/x", 100).is_err());
        // a Zstandard frame that is not one
        let garbage = sample::zip(&[
            ("FusionAssetName[Active]/", 0, &[][..], &[][..]),
            (
                "FusionAssetName[Active]/x",
                93,
                &b"not zstd at all"[..],
                &plain[..],
            ),
        ]);
        let mut a = Archive::open(Cursor::new(garbage)).unwrap().unwrap();
        assert!(a.read("FusionAssetName[Active]/x", 100).is_err());
    }

    #[test]
    fn other_zip_files_and_garbage_are_not_fusion_archives() {
        let plain = b"x".to_vec();
        let other = sample::zip(&[("3D/3dmodel.model", 0, &plain[..], &plain[..])]);
        assert!(!is_archive(&other));
        assert!(Archive::open(Cursor::new(other)).unwrap().is_none());
        assert!(Archive::open(Cursor::new(b"PK\x03\x04garbage".to_vec()))
            .unwrap()
            .is_none());
        assert!(!is_archive(b""));
        assert!(!is_archive(b"PK\x03\x04"));
    }

    #[test]
    fn another_document_folder_is_used_without_an_active_one() {
        let plain = b"w".to_vec();
        let name = format!("FusionAssetName[Other]/{SCENE_DIR}world");
        let archive = sample::zip(&[(&name, 0, &plain[..], &plain[..])]);
        let a = Archive::open(Cursor::new(archive)).unwrap().unwrap();
        assert_eq!(a.entry(&format!("{SCENE_DIR}world")), Some(name));
        assert_eq!(a.entry("Previews/big.png"), None);
    }
}
