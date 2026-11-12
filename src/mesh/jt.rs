//! The lightweight representation (JT) inside Siemens NX parts.
//!
//! NX saves a JT file (version 9.x) with the tessellated part into the
//! stream `/Root/FastLoad/JT` of its container (see `nxpart`). This reader
//! takes the finest level of detail (segment type 7, "LOD 0") from it:
//!
//! ```text
//! JT file   80 bytes version text ("Version 9.4 JT …"), u8 byte order (0 = LE),
//!           i32 reserved, i32 offset of the TOC, GUID of the LSG segment
//! TOC       i32 count, count × (GUID, i32 offset, i32 length, u32 attributes)
//!           the segment type is the high byte of the attributes
//! segment   GUID, i32 type, i32 length, then elements (shape LOD segments,
//!           types 6..=16, are never zlib compressed):
//! element   i32 length, GUID object type, u8 base type, data
//! ```
//!
//! The only element read is the *Tri-Strip Set Shape LOD* with
//! *topologically compressed* data (JT 9.5 / ISO 14306): 24 packets of
//! integer symbols (codecs bitlength, arithmetic, chopper) drive a dual
//! mesh coder that rebuilds the triangles; the vertex coordinates are
//! quantised per axis, the per-corner normals use Deering's sextant/octant
//! code. The algorithms follow the published JT 9.5 format reference.
//!
//! JT stores metres; the mesh is returned in millimetres like the rest of
//! NX. The normals are exact surface normals of the B-rep, so the coarse
//! LOD is refined along the true curvature (see `pn`).
//!
//! Everything is read defensively: sizes are capped, every index is checked
//! and a damaged file is an error, never a panic or an endless loop.

use anyhow::{anyhow, bail, ensure, Result};

use super::pn::Lod;
use super::{check_triangles, ends_early, Mesh};

/// Largest symbol count of one packet.
const MAX_VALUES: usize = 1 << 24;
/// Nested packets (chopper halves, out-of-band values) at most.
const MAX_DEPTH: u32 = 6;
/// Largest face degree / vertex valence of the dual mesh.
const MAX_DEGREE: i32 = 1 << 16;
/// Largest number of incidences (sum of degrees) of the dual mesh.
const MAX_INCIDENCES: usize = 3 * super::MAX_TRIANGLES;
/// Bits a decoder may read past the end of its code text (look-ahead of
/// the arithmetic decoder); more means the data is damaged.
const BIT_SLACK: usize = 64;
/// Object type of the Tri-Strip Set Shape LOD element.
const TRI_STRIP_SET_LOD: [u8; 16] = guid(
    0x10dd_10ab,
    0x2ac8,
    0x11d1,
    [0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59, 0x97],
);
/// JT lengths are metres.
const TO_MM: f64 = 1000.0;

/// A GUID as stored in the file (first three fields little endian).
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

fn damaged() -> anyhow::Error {
    anyhow!("The lightweight (JT) data of the NX part is damaged.")
}

/// The preview mesh of an NX part.
pub(crate) fn read_nx(data: &[u8]) -> Result<Mesh> {
    let streams = crate::nxpart::streams(data);
    let jt = streams
        .iter()
        .find(|s| s.name == crate::nxpart::JT_STREAM)
        .and_then(|s| crate::nxpart::data(data, s))
        .ok_or_else(|| {
            anyhow!(
                "This NX file has no lightweight representation (NX saves one for parts, \
                 not for assemblies and drawings) – the preview image is shown instead."
            )
        })?;
    let start = jt_start(jt).ok_or_else(damaged)?;
    read_jt(jt.get(start..).ok_or_else(damaged)?)
}

/// The preview mesh of a standalone JT file (Teamcenter, NX, CATIA, Creo
/// exports): the same reader as for the JT inside NX parts – JT 9 with
/// topologically compressed triangle LODs; other versions are reported.
pub(crate) fn read_file(data: &[u8]) -> Result<Mesh> {
    if !data.starts_with(b"Version ") {
        bail!("This is not a JT file (no 'Version' header).");
    }
    read_jt(data)
}

/// Where the JT file starts in the `/Root/FastLoad/JT` stream: after an
/// index of `u32 9, u32 count, u32 0` and `count` 16-byte entries (19 in a
/// small part, 259 in a large one). A stream laid out differently is
/// searched for the version text in its first 64 KiB.
fn jt_start(stream: &[u8]) -> Option<usize> {
    const VERSION: &[u8] = b"Version ";
    let count = usize::try_from(super::u32_le(stream, 4).ok()?).ok()?;
    let after_index = count.checked_mul(16).and_then(|n| n.checked_add(12));
    if let Some(at) = after_index {
        if stream
            .get(at..)
            .is_some_and(|rest| rest.starts_with(VERSION))
        {
            return Some(at);
        }
    }
    stream
        .get(..stream.len().min(64 * 1024))?
        .windows(VERSION.len())
        .position(|w| w == VERSION)
}

/// The finest level of detail of a JT 9 file.
fn read_jt(jt: &[u8]) -> Result<Mesh> {
    let version = String::from_utf8_lossy(super::range(jt, 0, 80)?);
    if !version.starts_with("Version 9.") {
        bail!(
            "The lightweight representation has JT {} – the preview reads JT 9.",
            version
                .trim_start_matches("Version ")
                .split_whitespace()
                .next()
                .unwrap_or("?")
        );
    }
    ensure!(
        super::bytes_at::<1>(jt, 80)? == [0],
        "Big-endian JT data is not supported."
    );
    let toc = usize::try_from(super::i32_le(jt, 85)?).map_err(|_| damaged())?;
    let mut r = Reader::at(jt, toc);
    let count = r.count(28)?;
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        r.take(16)?;
        let offset = usize::try_from(r.i32()?).map_err(|_| damaged())?;
        let length = usize::try_from(r.i32()?).map_err(|_| damaged())?;
        let kind = r.u32()? >> 24;
        segments.push((offset, length, kind));
    }
    // LOD 0 of every body; a file without one falls back to its finest LOD.
    let mut lods: Vec<Lod> = Vec::new();
    for &(offset, length, _) in segments.iter().filter(|s| s.2 == 7) {
        lods.extend(read_segment(jt, offset, length)?);
    }
    if lods.is_empty() {
        for &(offset, length, _) in segments.iter().filter(|s| (6..=16).contains(&s.2)) {
            if let Some(lod) = read_segment(jt, offset, length)? {
                if lods
                    .first()
                    .is_none_or(|l| l.triangles.len() < lod.triangles.len())
                {
                    lods = vec![lod];
                }
            }
        }
    }
    if lods.is_empty() {
        bail!("The lightweight representation of the NX part contains no triangles.");
    }
    let total: usize = lods.iter().map(|l| l.triangles.len()).sum();
    super::pn::to_mesh(lods, total, TO_MM)
}

/// The Tri-Strip Set element of a shape LOD segment, if it has one.
fn read_segment(jt: &[u8], offset: usize, length: usize) -> Result<Option<Lod>> {
    let end = offset.checked_add(length).ok_or_else(damaged)?;
    let segment = super::range(jt, offset, end)?;
    // segment header: GUID, type, length; then the element header
    let element_length = usize::try_from(super::i32_le(segment, 24)?).map_err(|_| damaged())?;
    let object_type: [u8; 16] = super::bytes_at(segment, 28)?;
    if object_type != TRI_STRIP_SET_LOD {
        return Ok(None);
    }
    let element_end = 28usize.checked_add(element_length).ok_or_else(damaged)?;
    let element = segment
        .get(..element_end)
        .ok_or_else(|| ends_early(element_end))?;
    let mut r = Reader::at(element, 45);
    read_tri_strip_set(&mut r).map(Some)
}

/// Tri-Strip Set Shape LOD element data (after the element header).
fn read_tri_strip_set(r: &mut Reader<'_>) -> Result<Lod> {
    r.i32()?; // object id
    r.i16()?; // base shape LOD version
    r.i16()?; // vertex shape LOD version
    r.take(8)?; // vertex bindings
    r.i16()?; // topo mesh version
    r.i32()?; // vertex records object id
    r.i16()?; // topologically compressed version
    let mut packets = Vec::with_capacity(24);
    for _ in 0..24 {
        packets.push(Packet::read(r, 0)?);
    }
    r.u32()?; // hash
    let symbols = Symbols::decode(packets)?;

    let bindings = r.take(8)?;
    let first = bindings.first().copied().unwrap_or(0);
    let coordinate_count = match first & 7 {
        1 => 2,
        2 => 3,
        4 => 4,
        _ => 0,
    };
    let has_normals = first & 8 != 0;
    r.take(4)?; // quantisation parameters
    let vertex_count = r.i32()?;
    if vertex_count == 0 {
        return Ok(Lod::default());
    }
    r.i32()?; // attribute count
    ensure!(coordinate_count == 3, "The JT shape has no 3D coordinates.");
    let points = read_coordinates(r)?;
    let normals = if has_normals {
        read_normals(r)?
    } else {
        Vec::new()
    };

    let mut decoder = MeshDecoder::new(symbols);
    decoder.run()?;
    let dual = decoder.mesh;
    let mut lod = Lod {
        points,
        normals,
        ..Lod::default()
    };
    let n_points = lod.points.len();
    for face in 0..dual.v_val.len() {
        if dual.v_grp.get(face).copied().unwrap_or(-1) < 0 {
            continue;
        }
        let valence = dual.valence(face)?;
        let mut corners = Vec::with_capacity(valence);
        let mut normal_ids = Vec::with_capacity(valence);
        for slot in 0..valence {
            let v = dual.face(face, slot)?;
            let vi = u32::try_from(v).map_err(|_| damaged())?;
            ensure!((vi as usize) < n_points, damaged());
            corners.push(vi);
            normal_ids.push(dual.vtx_face_attr(face, v)?.unwrap_or(u32::MAX));
        }
        let (Some((&c0, rest)), Some((&n0, nrest))) =
            (corners.split_first(), normal_ids.split_first())
        else {
            continue;
        };
        for (c, n) in rest.windows(2).zip(nrest.windows(2)) {
            if let ([c1, c2], [n1, n2]) = (c, n) {
                lod.triangles.push([c0, *c1, *c2]);
                lod.normal_triangles.push([n0, *n1, *n2]);
            }
        }
        check_triangles(lod.triangles.len())?;
    }
    Ok(lod)
}

/// Compressed vertex coordinates (quantised or exponent/mantissa floats).
fn read_coordinates(r: &mut Reader<'_>) -> Result<Vec<[f64; 3]>> {
    let count = r.count(1)?;
    let components = r.u8()?;
    ensure!(components == 3, "The JT shape has no 3D coordinates.");
    let mut quantizers = [(0f32, 0f32, 0u8); 3];
    for q in &mut quantizers {
        *q = (r.f32()?, r.f32()?, r.u8()?);
    }
    let mut axes: Vec<Vec<f64>> = Vec::with_capacity(3);
    if quantizers[0].2 != 0 {
        for (lo, hi, bits) in quantizers {
            let codes = lag1(Packet::read(r, 0)?.decode()?);
            ensure!(codes.len() == count, damaged());
            let max_code = if bits < 32 {
                f64::from(1u32 << bits)
            } else {
                f64::from(u32::MAX)
            };
            let step = (f64::from(hi) - f64::from(lo)) / max_code;
            axes.push(
                codes
                    .iter()
                    .map(|&c| f64::from(lo) + (f64::from(c as u32) - 0.5) * step)
                    .collect(),
            );
        }
    } else {
        for _ in 0..3 {
            axes.push(exp_mant(r, count, true)?);
        }
    }
    r.i32()?; // hash
    let [x, y, z] = <[Vec<f64>; 3]>::try_from(axes).map_err(|_| damaged())?;
    Ok(x.into_iter()
        .zip(y)
        .zip(z)
        .map(|((x, y), z)| [x, y, z])
        .collect())
}

/// Floats split into exponent and mantissa packets.
fn exp_mant(r: &mut Reader<'_>, count: usize, predicted: bool) -> Result<Vec<f64>> {
    let mut exponents = Packet::read(r, 0)?.decode()?;
    let mut mantissas = Packet::read(r, 0)?.decode()?;
    if predicted {
        exponents = lag1(exponents);
        mantissas = lag1(mantissas);
    }
    ensure!(
        exponents.len() == count && mantissas.len() == count,
        damaged()
    );
    Ok(exponents
        .iter()
        .zip(&mantissas)
        .map(|(&e, &m)| f64::from(f32::from_bits(((e as u32) << 23) | m as u32)))
        .collect())
}

/// Compressed normals (Deering code or exponent/mantissa floats).
fn read_normals(r: &mut Reader<'_>) -> Result<Vec<[f64; 3]>> {
    let count = r.count(1)?;
    let components = r.u8()?;
    let bits = r.u8()?;
    let normals = if bits != 0 {
        ensure!(bits <= 13, damaged());
        let sextants = Packet::read(r, 0)?.decode()?;
        let octants = Packet::read(r, 0)?.decode()?;
        let thetas = Packet::read(r, 0)?.decode()?;
        let psis = Packet::read(r, 0)?.decode()?;
        ensure!(
            [&sextants, &octants, &thetas, &psis]
                .iter()
                .all(|v| v.len() == count),
            damaged()
        );
        let table = DeeringTable::new();
        let shift = 13 - u32::from(bits);
        let mut out = Vec::with_capacity(count);
        for (((&s, &o), &t), &p) in sextants.iter().zip(&octants).zip(&thetas).zip(&psis) {
            out.push(table.normal(s, o, t, p, shift)?);
        }
        out
    } else {
        ensure!(components == 3, damaged());
        let x = exp_mant(r, count, false)?;
        let y = exp_mant(r, count, false)?;
        let z = exp_mant(r, count, false)?;
        x.into_iter()
            .zip(y)
            .zip(z)
            .map(|((x, y), z)| [x, y, z])
            .collect()
    };
    r.i32()?; // hash
    Ok(normals)
}

/// Lookup tables of the Deering normal code.
struct DeeringTable {
    theta: Vec<(f64, f64)>,
    psi: Vec<(f64, f64)>,
}

impl DeeringTable {
    const BITS: u32 = 13;

    fn new() -> Self {
        let size = 1usize << Self::BITS;
        let max_psi = 0.615_479_709_f64;
        let mut theta = Vec::with_capacity(size + 1);
        let mut psi = Vec::with_capacity(size + 1);
        for i in 0..=size {
            let t = (max_psi * (size - i) as f64 / size as f64).tan().asin();
            let p = max_psi * i as f64 / size as f64;
            theta.push(t.sin_cos());
            psi.push(p.sin_cos());
        }
        DeeringTable { theta, psi }
    }

    fn normal(
        &self,
        sextant: i32,
        octant: i32,
        theta: i32,
        psi: i32,
        shift: u32,
    ) -> Result<[f64; 3]> {
        let index = |code: i32| -> Result<usize> {
            let code = usize::try_from(code).map_err(|_| damaged())?;
            code.checked_shl(shift).ok_or_else(damaged)
        };
        let (ts, tc) = *self
            .theta
            .get(index(theta.wrapping_add(sextant & 1))?)
            .ok_or_else(damaged)?;
        let (ps, pc) = *self.psi.get(index(psi)?).ok_or_else(damaged)?;
        let (x, y, z) = (pc * tc, ps, pc * ts);
        let [x, y, z] = match sextant {
            1 => [z, y, x],
            2 => [y, z, x],
            3 => [y, x, z],
            4 => [z, x, y],
            5 => [x, z, y],
            _ => [x, y, z],
        };
        Ok([
            if octant & 4 != 0 { x } else { -x },
            if octant & 2 != 0 { y } else { -y },
            if octant & 1 != 0 { z } else { -z },
        ])
    }
}

// ---------------------------------------------------------------------------
// readers
// ---------------------------------------------------------------------------

/// Little-endian reader over a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], at: usize) -> Self {
        Reader { data, at }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| ends_early(self.at))?;
        let s = super::range(self.data, self.at, end)?;
        self.at = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let v = super::bytes_at(self.data, self.at)?;
        self.at += N;
        Ok(v)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn i16(&mut self) -> Result<i16> {
        self.array().map(i16::from_le_bytes)
    }

    fn i32(&mut self) -> Result<i32> {
        self.array().map(i32::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }

    fn f32(&mut self) -> Result<f32> {
        self.array().map(f32::from_le_bytes)
    }

    /// A count of items of at least `item_bytes` bytes each that must fit
    /// into the rest of the data.
    fn count(&mut self, item_bytes: usize) -> Result<usize> {
        let n = usize::try_from(self.i32()?).map_err(|_| damaged())?;
        let rest = self.data.len().saturating_sub(self.at);
        ensure!(
            n <= MAX_VALUES && n.saturating_mul(item_bytes) <= rest,
            damaged()
        );
        Ok(n)
    }
}

/// Bits, most significant first; reading past the end gives zeros up to
/// [`BIT_SLACK`] bits, then an error.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(data: Vec<u8>) -> Self {
        Bits { data, pos: 0 }
    }

    /// Code text: little-endian 32-bit words, each read from its top bit.
    fn words(raw: &[u8]) -> Self {
        let mut data = Vec::with_capacity(raw.len());
        for w in raw.chunks_exact(4) {
            data.extend(w.iter().rev());
        }
        Bits::new(data)
    }

    fn u(&mut self, n: u32) -> Result<u32> {
        ensure!(n <= 32, damaged());
        let n = n as usize;
        let end = self.pos.checked_add(n).ok_or_else(damaged)?;
        ensure!(end <= self.data.len() * 8 + BIT_SLACK, damaged());
        let mut v: u64 = 0;
        let mut need = n;
        while need > 0 {
            let offset = self.pos & 7;
            let avail = 8 - offset;
            let take = avail.min(need);
            let byte = u64::from(self.data.get(self.pos >> 3).copied().unwrap_or(0));
            v = (v << take) | ((byte >> (avail - take)) & ((1 << take) - 1));
            need -= take;
            self.pos += take;
        }
        Ok(v as u32)
    }

    fn s(&mut self, n: u32) -> Result<i32> {
        if n == 0 {
            return Ok(0);
        }
        let v = i64::from(self.u(n)?);
        Ok(if v & (1 << (n - 1)) != 0 {
            (v - (1i64 << n)) as i32
        } else {
            v as i32
        })
    }
}

// ---------------------------------------------------------------------------
// Int32 compressed data packets (mk2)
// ---------------------------------------------------------------------------

/// One entry of a probability context: symbol, count, value.
#[derive(Clone, Copy, Debug)]
struct Entry {
    symbol: i64,
    occurrences: u64,
    value: i32,
}

#[derive(Debug)]
enum Packet {
    Empty,
    Raw(Vec<i32>),
    Chopped {
        count: usize,
        bias: i32,
        shift: u32,
        high: Box<Packet>,
        low: Box<Packet>,
    },
    Bitlength {
        count: usize,
        bits: Bits,
    },
    Arithmetic {
        count: usize,
        bits: Bits,
        context: Vec<Entry>,
        out_of_band: Box<Packet>,
    },
}

impl std::fmt::Debug for Bits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bits({} bytes)", self.data.len())
    }
}

impl Packet {
    fn read(r: &mut Reader<'_>, depth: u32) -> Result<Packet> {
        ensure!(depth <= MAX_DEPTH, damaged());
        let count = usize::try_from(r.i32()?).map_err(|_| damaged())?;
        ensure!(count <= MAX_VALUES, damaged());
        if count == 0 {
            return Ok(Packet::Empty);
        }
        let codec = r.u8()?;
        if codec == 4 {
            let chop = r.u8()?;
            if chop == 0 {
                // nothing chopped: the packet follows with its own count
                return Packet::read(r, depth + 1);
            }
            let bias = r.i32()?;
            let span = r.u8()?;
            ensure!(chop <= span && span <= 32, damaged());
            let high = Packet::read(r, depth + 1)?;
            let low = Packet::read(r, depth + 1)?;
            ensure!(high.count() == count && low.count() == count, damaged());
            return Ok(Packet::Chopped {
                count,
                bias,
                shift: u32::from(span - chop),
                high: Box::new(high),
                low: Box::new(low),
            });
        }
        let bit_length = usize::try_from(r.i32()?).map_err(|_| damaged())?;
        ensure!(bit_length > 0, damaged());
        let words = bit_length.div_ceil(32);
        let raw = r.take(words.checked_mul(4).ok_or_else(damaged)?)?;
        match codec {
            0 => Ok(Packet::Raw(
                super::quads(raw).map(i32::from_le_bytes).collect(),
            )),
            1 => Ok(Packet::Bitlength {
                count,
                bits: Bits::words(raw),
            }),
            3 => {
                let context = read_context(r)?;
                let out_of_band = Packet::read(r, depth + 1)?;
                Ok(Packet::Arithmetic {
                    count,
                    bits: Bits::words(raw),
                    context,
                    out_of_band: Box::new(out_of_band),
                })
            }
            other => bail!("The JT data uses the unknown codec {other}."),
        }
    }

    fn count(&self) -> usize {
        match self {
            Packet::Empty => 0,
            Packet::Raw(v) => v.len(),
            Packet::Chopped { count, .. }
            | Packet::Bitlength { count, .. }
            | Packet::Arithmetic { count, .. } => *count,
        }
    }

    fn decode(self) -> Result<Vec<i32>> {
        match self {
            Packet::Empty => Ok(Vec::new()),
            Packet::Raw(v) => Ok(v),
            Packet::Chopped {
                bias,
                shift,
                high,
                low,
                ..
            } => {
                let high = high.decode()?;
                let low = low.decode()?;
                Ok(high
                    .iter()
                    .zip(&low)
                    .map(|(&h, &l)| {
                        let h = if shift >= 32 { 0 } else { (h as u32) << shift };
                        ((h | l as u32) as i32).wrapping_add(bias)
                    })
                    .collect())
            }
            Packet::Bitlength { count, mut bits } => bitlength(&mut bits, count),
            Packet::Arithmetic {
                count,
                mut bits,
                context,
                out_of_band,
            } => arithmetic(&mut bits, count, &context, &out_of_band.decode()?),
        }
    }
}

/// Probability context (mk2): a bit stream on the bytes, ending byte aligned.
fn read_context(r: &mut Reader<'_>) -> Result<Vec<Entry>> {
    let mut bits = Bits::new(r.data.get(r.at..).unwrap_or_default().to_vec());
    let entries = bits.u(16)? as usize;
    let symbol_bits = bits.u(6)?;
    let occurrence_bits = bits.u(6)?;
    let value_bits = bits.u(6)?;
    let min_value = bits.s(32)?;
    let needed = entries.saturating_mul((symbol_bits + occurrence_bits + value_bits) as usize);
    ensure!(needed <= bits.data.len() * 8, damaged());
    let mut context = Vec::with_capacity(entries);
    for _ in 0..entries {
        let symbol = i64::from(bits.u(symbol_bits)?) - 2;
        let occurrences = u64::from(bits.u(occurrence_bits)?);
        let value = (bits.u(value_bits)? as i32).wrapping_add(min_value);
        context.push(Entry {
            symbol,
            occurrences,
            value,
        });
    }
    ensure!(bits.pos <= bits.data.len() * 8, damaged());
    r.at += bits.pos.div_ceil(8);
    Ok(context)
}

/// Bitlength codec (fixed or variable field width).
fn bitlength(bits: &mut Bits, count: usize) -> Result<Vec<i32>> {
    let mut out = Vec::with_capacity(count);
    if bits.u(1)? == 0 {
        let min_bits = bits.u(6)?;
        let max_bits = bits.u(6)?;
        ensure!(min_bits <= 32 && max_bits <= 32, damaged());
        let min = bits.s(min_bits)?;
        let max = bits.s(max_bits)?;
        let mut range = i64::from(max) - i64::from(min);
        if range <= 0 {
            out.resize(count, min);
            return Ok(out);
        }
        let mut width = 1;
        range >>= 1;
        while range != 0 {
            width += 1;
            range >>= 1;
        }
        for _ in 0..count {
            out.push((bits.u(width)? as i32).wrapping_add(min));
        }
        return Ok(out);
    }
    let mean = bits.s(32)?;
    let change_bits = bits.u(3)?;
    let run_bits = bits.u(3)?;
    ensure!(change_bits > 0, damaged());
    let max_decrement = -(1i32 << (change_bits - 1));
    let max_increment = (1i32 << (change_bits - 1)) - 1;
    let mut width: i32 = 0;
    while out.len() < count {
        loop {
            let change = bits.s(change_bits)?;
            width = width.checked_add(change).ok_or_else(damaged)?;
            if change != max_decrement && change != max_increment {
                break;
            }
        }
        ensure!((0..=32).contains(&width), damaged());
        let run = bits.u(run_bits)? as usize;
        let run = run.min(count - out.len());
        for _ in 0..run {
            out.push(if width > 0 {
                bits.s(width as u32)?.wrapping_add(mean)
            } else {
                mean
            });
        }
    }
    Ok(out)
}

/// Arithmetic codec with one probability context; escape symbols (-2)
/// take the next out-of-band value.
fn arithmetic(bits: &mut Bits, count: usize, context: &[Entry], oob: &[i32]) -> Result<Vec<i32>> {
    let total: u64 = context.iter().map(|e| e.occurrences).sum();
    ensure!(total > 0 && total <= 0xFFFF, damaged());
    let mut out = Vec::with_capacity(count);
    let mut oob = oob.iter();
    let mut low: u32 = 0;
    let mut high: u32 = 0xFFFF;
    let mut code: u32 = bits.u(16)?;
    while out.len() < count {
        let range = u64::from(high - low) + 1;
        ensure!(code >= low, damaged());
        let scaled = ((u64::from(code - low) + 1) * total - 1) / range;
        let mut sym_high = 0u64;
        let mut chosen = None;
        for e in context {
            sym_high += e.occurrences;
            if sym_high > scaled {
                chosen = Some(e);
                break;
            }
        }
        let e = chosen.ok_or_else(damaged)?;
        let sym_low = sym_high - e.occurrences;
        out.push(if e.symbol == -2 {
            *oob.next().ok_or_else(damaged)?
        } else {
            e.value
        });
        let new_high =
            (low + ((range * sym_high / total) as u32 & 0xFFFF)).wrapping_sub(1) & 0xFFFF;
        low = (low + ((range * sym_low / total) as u32 & 0xFFFF)) & 0xFFFF;
        high = new_high;
        ensure!(high >= low, damaged());
        let mask = (high ^ low) & 0xFFFF;
        if mask & 0x8000 == 0 {
            let mut shift = 1;
            let mut m = !mask & 0xFFFF;
            while m & 0x4000 != 0 {
                shift += 1;
                m = (m << 1) & 0xFFFF;
            }
            low = (low << shift) & 0xFFFF;
            high = ((high << shift) | ((1 << shift) - 1)) & 0xFFFF;
            code = ((code << shift) & 0xFFFF) | bits.u(shift)?;
        }
        if high & 0x4000 == 0 && low & 0x4000 != 0 {
            let mut shift = 1;
            let mut m = 0x2000;
            while m != 0 && high & m == 0 && low & m != 0 {
                shift += 1;
                m >>= 1;
            }
            low = (low << shift) & 0xFFFF;
            high = ((high << shift) | ((1 << shift) - 1)) & 0xFFFF;
            code = ((code << shift) & 0xFFFF) | bits.u(shift)?;
            low &= 0x7FFF;
            high |= 0x8000;
            code ^= 0x8000;
        }
    }
    Ok(out)
}

/// Lag-1 prediction (the first four values are stored as they are).
fn lag1(mut v: Vec<i32>) -> Vec<i32> {
    for i in 4..v.len() {
        if let (Some(&prev), Some(cur)) = (v.get(i - 1), v.get(i)) {
            let next = prev.wrapping_add(*cur);
            if let Some(slot) = v.get_mut(i) {
                *slot = next;
            }
        }
    }
    v
}

// ---------------------------------------------------------------------------
// dual mesh coder
// ---------------------------------------------------------------------------

/// The decoded symbol streams that drive the mesh coder.
struct Symbols {
    degrees: [Vec<i32>; 8],
    valences: Vec<i32>,
    groups: Vec<i32>,
    attr_masks: [Vec<i32>; 8],
    attr_masks_30: Vec<i32>,
    attr_masks_4: Vec<i32>,
    attr_masks_large: Vec<i32>,
    split_faces: Vec<i32>,
    split_positions: Vec<i32>,
}

impl Symbols {
    fn decode(packets: Vec<Packet>) -> Result<Symbols> {
        let mut decoded = Vec::with_capacity(24);
        for (i, p) in packets.into_iter().enumerate() {
            let v = p.decode()?;
            // vertex flags (10) and split faces (22) are lag-1 predicted
            decoded.push(if i == 10 || i == 22 { lag1(v) } else { v });
        }
        let mut it = decoded.into_iter();
        let mut next = || it.next().ok_or_else(damaged);
        let degrees = [
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
        ];
        let valences = next()?;
        let groups = next()?;
        next()?; // vertex flags
        let attr_masks = [
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
        ];
        Ok(Symbols {
            degrees,
            valences,
            groups,
            attr_masks,
            attr_masks_30: next()?,
            attr_masks_4: next()?,
            attr_masks_large: next()?,
            split_faces: next()?,
            split_positions: next()?,
        })
    }
}

/// Vertex-face incidence of the dual mesh (dual vertices are the polygons
/// of the mesh, dual faces its vertices).
#[derive(Default)]
struct DualMesh {
    v_val: Vec<usize>,
    v_grp: Vec<i32>,
    v_vfi: Vec<usize>,
    f_deg: Vec<usize>,
    f_empty: Vec<usize>,
    f_fvi: Vec<usize>,
    f_mask: Vec<Vec<u32>>,
    f_attr: Vec<(u32, usize)>,
    vf: Vec<i32>,
    fv: Vec<i32>,
}

fn idx(i: i32) -> Result<usize> {
    usize::try_from(i).map_err(|_| damaged())
}

impl DualMesh {
    fn new_vtx(&mut self, valence: usize, group: i32) -> Result<i32> {
        ensure!(self.vf.len() + valence <= MAX_INCIDENCES, damaged());
        self.v_val.push(valence);
        self.v_grp.push(group);
        self.v_vfi.push(self.vf.len());
        self.vf.resize(self.vf.len() + valence, -1);
        i32::try_from(self.v_val.len() - 1).map_err(|_| damaged())
    }

    fn new_face(&mut self, degree: usize, mask: Vec<u32>, attr_counter: &mut u32) -> Result<i32> {
        ensure!(self.fv.len() + degree <= MAX_INCIDENCES, damaged());
        let attrs = (0..degree)
            .filter(|&s| mask.get(s / 32).is_some_and(|w| w >> (s % 32) & 1 != 0))
            .count();
        self.f_deg.push(degree);
        self.f_empty.push(degree);
        self.f_fvi.push(self.fv.len());
        self.f_mask.push(mask);
        self.f_attr.push((*attr_counter, attrs));
        *attr_counter = attr_counter.saturating_add(attrs as u32);
        self.fv.resize(self.fv.len() + degree, -1);
        i32::try_from(self.f_deg.len() - 1).map_err(|_| damaged())
    }

    fn valence(&self, v: usize) -> Result<usize> {
        self.v_val.get(v).copied().ok_or_else(damaged)
    }

    fn degree(&self, f: i32) -> Result<usize> {
        self.f_deg.get(idx(f)?).copied().ok_or_else(damaged)
    }

    fn slot_vf(&self, v: usize, s: usize) -> Result<usize> {
        ensure!(s < self.valence(v)?, damaged());
        Ok(self.v_vfi.get(v).ok_or_else(damaged)? + s)
    }

    fn slot_fv(&self, f: i32, s: usize) -> Result<usize> {
        ensure!(s < self.degree(f)?, damaged());
        Ok(self.f_fvi.get(idx(f)?).ok_or_else(damaged)? + s)
    }

    fn face(&self, v: usize, s: usize) -> Result<i32> {
        self.vf
            .get(self.slot_vf(v, s)?)
            .copied()
            .ok_or_else(damaged)
    }

    fn vtx(&self, f: i32, s: usize) -> Result<i32> {
        self.fv
            .get(self.slot_fv(f, s)?)
            .copied()
            .ok_or_else(damaged)
    }

    fn set_vtx_face(&mut self, v: usize, s: usize, f: i32) -> Result<()> {
        let i = self.slot_vf(v, s)?;
        *self.vf.get_mut(i).ok_or_else(damaged)? = f;
        Ok(())
    }

    fn set_face_vtx(&mut self, f: i32, s: usize, v: i32) -> Result<()> {
        let i = self.slot_fv(f, s)?;
        let cell = self.fv.get_mut(i).ok_or_else(damaged)?;
        if *cell != v {
            let empty = self.f_empty.get_mut(idx(f)?).ok_or_else(damaged)?;
            *empty = empty.saturating_sub(1);
        }
        *cell = v;
        Ok(())
    }

    fn is_valid_face(&self, f: i32) -> bool {
        usize::try_from(f)
            .ok()
            .and_then(|f| self.f_deg.get(f))
            .is_some_and(|&d| d != 0)
    }

    fn find_vtx_slot(&self, f: i32, v: i32) -> Result<Option<usize>> {
        let start = *self.f_fvi.get(idx(f)?).ok_or_else(damaged)?;
        let deg = self.degree(f)?;
        let slots = self.fv.get(start..start + deg).ok_or_else(damaged)?;
        Ok(slots.iter().position(|&x| x == v))
    }

    fn find_face_slot(&self, v: usize, f: i32) -> Result<Option<usize>> {
        let start = *self.v_vfi.get(v).ok_or_else(damaged)?;
        let val = self.valence(v)?;
        let slots = self.vf.get(start..start + val).ok_or_else(damaged)?;
        Ok(slots.iter().position(|&x| x == f))
    }

    /// The attribute (normal) of the polygon `v` at the mesh vertex `f`.
    fn vtx_face_attr(&self, v: usize, f: i32) -> Result<Option<u32>> {
        let fi = idx(f)?;
        let (first, count) = *self.f_attr.get(fi).ok_or_else(damaged)?;
        if count == 0 {
            return Ok(None);
        }
        let mask = self.f_mask.get(fi).ok_or_else(damaged)?;
        let deg = self.degree(f)?;
        let mut slot = count - 1;
        for s in 0..deg {
            if mask.get(s / 32).is_some_and(|w| w >> (s % 32) & 1 != 0) {
                slot += 1;
            }
            if slot >= count {
                slot -= count;
            }
            if self.vtx(f, s)? == v as i32 {
                return Ok(Some(first.saturating_add(slot as u32)));
            }
        }
        Ok(None)
    }
}

fn inc(v: usize, n: usize) -> usize {
    if v + 1 >= n {
        0
    } else {
        v + 1
    }
}

fn dec(v: usize, n: usize) -> usize {
    if v == 0 {
        n.saturating_sub(1)
    } else {
        v - 1
    }
}

/// Rebuilds the dual mesh from the symbol streams.
struct MeshDecoder {
    s: Symbols,
    degree_pos: [usize; 8],
    mask_pos: [usize; 8],
    valence_pos: usize,
    group_pos: usize,
    mask_large_pos: usize,
    split_face_pos: usize,
    split_pos_pos: usize,
    mesh: DualMesh,
    active: Vec<i32>,
    removed: Vec<bool>,
    attr_counter: u32,
}

fn next_symbol(v: &[i32], pos: &mut usize) -> Option<i32> {
    let s = v.get(*pos).copied();
    if s.is_some() {
        *pos += 1;
    }
    s
}

impl MeshDecoder {
    fn new(s: Symbols) -> Self {
        MeshDecoder {
            s,
            degree_pos: [0; 8],
            mask_pos: [0; 8],
            valence_pos: 0,
            group_pos: 0,
            mask_large_pos: 0,
            split_face_pos: 0,
            split_pos_pos: 0,
            mesh: DualMesh::default(),
            active: Vec::new(),
            removed: Vec::new(),
            attr_counter: 0,
        }
    }

    fn run(&mut self) -> Result<()> {
        while let Some(v) = self.io_vtx()? {
            for slot in 0..self.mesh.valence(v)? {
                self.activate_f(v, slot)?;
            }
            while let Some(f) = self.next_active_face()? {
                self.complete_v(f)?;
                let fi = idx(f)?;
                if self.removed.len() <= fi {
                    self.removed.resize(fi + 1, false);
                }
                if let Some(r) = self.removed.get_mut(fi) {
                    *r = true;
                }
            }
        }
        Ok(())
    }

    fn is_removed(&self, f: i32) -> bool {
        usize::try_from(f)
            .ok()
            .and_then(|f| self.removed.get(f))
            .copied()
            .unwrap_or(false)
    }

    /// A new dual vertex (polygon) with its valence, or `None` at the end.
    fn io_vtx(&mut self) -> Result<Option<usize>> {
        let Some(valence) = next_symbol(&self.s.valences, &mut self.valence_pos) else {
            return Ok(None);
        };
        if valence < 0 {
            return Ok(None);
        }
        ensure!(valence <= MAX_DEGREE, damaged());
        let group = next_symbol(&self.s.groups, &mut self.group_pos).unwrap_or(-1);
        let v = self.mesh.new_vtx(valence as usize, group)?;
        Ok(Some(idx(v)?))
    }

    fn face_context(&self, v: usize) -> Result<usize> {
        let val = self.mesh.valence(v)?;
        // only valences 3 and 4 look at the faces: scanning the slots of
        // every polygon for each of its slots would be quadratic
        match val {
            5 => return Ok(6),
            3 | 4 => {}
            _ => return Ok(7),
        }
        let (mut known, mut total) = (0usize, 0usize);
        for s in 0..val {
            let f = self.mesh.face(v, s)?;
            if self.mesh.is_valid_face(f) {
                known += 1;
                total += self.mesh.degree(f)?;
            }
        }
        Ok(match val {
            3 => match total.cmp(&(known * 6)) {
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal => 1,
                std::cmp::Ordering::Greater => 2,
            },
            4 => match total.cmp(&(known * 4)) {
                std::cmp::Ordering::Less => 3,
                std::cmp::Ordering::Equal => 4,
                std::cmp::Ordering::Greater => 5,
            },
            5 => 6,
            _ => 7,
        })
    }

    /// A new dual face (mesh vertex) for slot of `v`, or `None` when the
    /// face already exists (a split follows).
    fn io_face(&mut self, v: usize) -> Result<Option<i32>> {
        let ctx = self.face_context(v)?;
        let degrees = self.s.degrees.get(ctx).ok_or_else(damaged)?;
        let pos = self.degree_pos.get_mut(ctx).ok_or_else(damaged)?;
        let degree = next_symbol(degrees, pos).unwrap_or(-1);
        if degree == 0 {
            return Ok(None);
        }
        ensure!(degree > 0 && degree <= MAX_DEGREE, damaged());
        let degree = degree as usize;
        let mask = if degree <= 64 {
            let c = degree.saturating_sub(2).min(7);
            let pos = *self.mask_pos.get(c).ok_or_else(damaged)?;
            let low = self
                .s
                .attr_masks
                .get(c)
                .and_then(|m| m.get(pos))
                .copied()
                .unwrap_or(0) as u32 as u64;
            if let Some(p) = self.mask_pos.get_mut(c) {
                *p += 1;
            }
            let mut mask = low;
            if c == 7 {
                let hi30 = self.s.attr_masks_30.get(pos).copied().unwrap_or(0) as u32 as u64;
                let hi4 = self.s.attr_masks_4.get(pos).copied().unwrap_or(0) as u32 as u64;
                mask |= (hi30 << 30) | (hi4 << 60);
            }
            vec![mask as u32, (mask >> 32) as u32]
        } else {
            let words = degree.div_ceil(32);
            let start = self.mask_large_pos.min(self.s.attr_masks_large.len());
            let end = (start + words).min(self.s.attr_masks_large.len());
            self.mask_large_pos = end;
            self.s
                .attr_masks_large
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .map(|&w| w as u32)
                .collect()
        };
        let f = self.mesh.new_face(degree, mask, &mut self.attr_counter)?;
        Ok(Some(f))
    }

    fn activate_f(&mut self, v: usize, slot: usize) -> Result<()> {
        if let Some(f) = self.io_face(v)? {
            self.mesh.set_vtx_face(v, slot, f)?;
            self.mesh.set_face_vtx(f, 0, v as i32)?;
            self.active.push(f);
        } else {
            let offset =
                next_symbol(&self.s.split_faces, &mut self.split_face_pos).ok_or_else(damaged)?;
            let offset = usize::try_from(offset).map_err(|_| damaged())?;
            ensure!(offset > 0 && offset <= self.active.len(), damaged());
            let f = *self
                .active
                .get(self.active.len() - offset)
                .ok_or_else(damaged)?;
            let pos = next_symbol(&self.s.split_positions, &mut self.split_pos_pos)
                .ok_or_else(damaged)?;
            let pos = usize::try_from(pos).map_err(|_| damaged())?;
            self.mesh.set_vtx_face(v, slot, f)?;
            self.add_vtx_to_face(v, slot, f, pos)?;
        }
        Ok(())
    }

    fn complete_v(&mut self, f: i32) -> Result<()> {
        while let Some(slot) = self.mesh.find_vtx_slot(f, -1)? {
            let v = self.io_vtx()?.ok_or_else(damaged)?;
            self.mesh.set_vtx_face(v, 0, f)?;
            self.add_vtx_to_face(v, 0, f, slot)?;
            self.complete_f(v, slot)?;
        }
        Ok(())
    }

    fn complete_f(&mut self, v: usize, vslot: usize) -> Result<()> {
        let val = self.mesh.valence(v)?;
        let mut vp = self.mesh.face(v, 0)?;
        let mut jp = vslot;
        let mut i = 1;
        while i < val {
            let vn = self.mesh.face(v, i)?;
            if vn == -1 {
                break;
            }
            jp = dec(jp, self.mesh.degree(vp)?);
            let v2 = self.mesh.vtx(vp, jp)?;
            if v2 == -1 {
                break;
            }
            let jn = self.mesh.find_vtx_slot(vn, v2)?.ok_or_else(damaged)?;
            let jn = dec(jn, self.mesh.degree(vn)?);
            self.add_vtx_to_face(v, i, vn, jn)?;
            vp = vn;
            jp = jn;
            i += 1;
            if i >= val {
                return Ok(());
            }
        }
        if i >= val {
            return Ok(());
        }
        let first_open = i;
        let mut vp = self.mesh.face(v, 0)?;
        let mut jp = vslot;
        let mut i = val - 1;
        loop {
            let vn = self.mesh.face(v, i)?;
            if vn == -1 {
                break;
            }
            jp = inc(jp, self.mesh.degree(vp)?);
            let v2 = self.mesh.vtx(vp, jp)?;
            if v2 == -1 {
                break;
            }
            let jn = self.mesh.find_vtx_slot(vn, v2)?.ok_or_else(damaged)?;
            let jn = inc(jn, self.mesh.degree(vn)?);
            self.add_vtx_to_face(v, i, vn, jn)?;
            vp = vn;
            jp = jn;
            if i == 0 || i - 1 < first_open {
                return Ok(());
            }
            i -= 1;
        }
        for slot in first_open..=i {
            self.activate_f(v, slot)?;
        }
        Ok(())
    }

    fn add_vtx_to_face(&mut self, v: usize, fslot: usize, f: i32, vslot: usize) -> Result<()> {
        let deg = self.mesh.degree(f)?;
        ensure!(vslot < deg, damaged());
        let ccw = inc(vslot, deg);
        let cw = dec(vslot, deg);
        self.mesh.set_face_vtx(f, vslot, v as i32)?;
        let val = self.mesh.valence(v)?;
        let fp = self.mesh.vtx(f, cw)?;
        if fp != -1 {
            let fpu = idx(fp)?;
            let ip = self.mesh.find_face_slot(fpu, f)?.ok_or_else(damaged)?;
            let s = inc(fslot, val);
            if self.mesh.face(v, s)? == -1 {
                let ip = dec(ip, self.mesh.valence(fpu)?);
                let g = self.mesh.face(fpu, ip)?;
                self.mesh.set_vtx_face(v, s, g)?;
            }
        }
        let fn_ = self.mesh.vtx(f, ccw)?;
        if fn_ != -1 {
            let fnu = idx(fn_)?;
            let ix = self.mesh.find_face_slot(fnu, f)?.ok_or_else(damaged)?;
            let s = dec(fslot, val);
            if self.mesh.face(v, s)? == -1 {
                let ix = inc(ix, self.mesh.valence(fnu)?);
                let g = self.mesh.face(fnu, ix)?;
                self.mesh.set_vtx_face(v, s, g)?;
            }
        }
        Ok(())
    }

    /// The active face with the fewest open slots among the 16 newest.
    fn next_active_face(&mut self) -> Result<Option<i32>> {
        while self.active.last().is_some_and(|&f| self.is_removed(f)) {
            self.active.pop();
        }
        let mut best = None;
        let mut lowest = usize::MAX;
        let mut i = self.active.len();
        while i > 0 && i > self.active.len().saturating_sub(16) {
            i -= 1;
            let f = *self.active.get(i).ok_or_else(damaged)?;
            if self.is_removed(f) {
                self.active.remove(i);
                continue;
            }
            let empty = *self.mesh.f_empty.get(idx(f)?).ok_or_else(damaged)?;
            if empty < lowest {
                lowest = empty;
                best = Some(f);
            }
        }
        Ok(best)
    }
}

#[cfg(test)]
mod tests;
