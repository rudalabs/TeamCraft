//! Unit tests of the JT reader, stage by stage: byte and bit readers, the
//! int32 packet codecs, the coordinate and normal decoders, the dual mesh
//! coder, the segment and file structure, and the real JT data of the NIST
//! NX fixtures (decoded, then truncated and damaged at known offsets).
//!
//! Small inputs are built by hand with the writers below; the arithmetic
//! codec is checked against a reference encoder (Witten, Neal, Cleary).

use super::*;

// ---------------------------------------------------------------------------
// writers
// ---------------------------------------------------------------------------

/// Little-endian byte writer.
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn i32(mut self, v: i32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn i16(mut self, v: i16) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn f32(mut self, v: f32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn bytes(mut self, v: &[u8]) -> Self {
        self.0.extend_from_slice(v);
        self
    }
}

/// Packs (value, bits) pairs MSB first.
fn pack(fields: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut n = 0;
    for &(v, bits) in fields {
        let v = if bits == 32 {
            u64::from(v)
        } else {
            u64::from(v) & ((1 << bits) - 1)
        };
        acc = (acc << bits) | v;
        n += bits;
        while n >= 8 {
            out.push((acc >> (n - 8)) as u8);
            n -= 8;
            acc &= (1 << n) - 1;
        }
    }
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

/// An MSB-first bit stream as JT code text: 32-bit words, little endian.
fn words(bits: &[u8]) -> Vec<u8> {
    let mut padded = bits.to_vec();
    padded.resize(bits.len().div_ceil(4) * 4, 0);
    padded
        .chunks_exact(4)
        .flat_map(|w| w.iter().rev().copied())
        .collect()
}

/// A packet stored raw (codec 0): one little-endian word per value.
fn raw_packet(values: &[i32]) -> Vec<u8> {
    if values.is_empty() {
        return W::default().i32(0).0;
    }
    let mut w = W::default()
        .i32(values.len() as i32)
        .u8(0)
        .i32(32 * values.len() as i32);
    for &v in values {
        w = w.i32(v);
    }
    w.0
}

/// A bitlength packet (codec 1) around an MSB-first bit stream.
fn bitlength_packet(count: i32, stream: &[u8]) -> Vec<u8> {
    let code = words(stream);
    W::default()
        .i32(count)
        .u8(1)
        .i32(8 * code.len() as i32)
        .bytes(&code)
        .0
}

/// The fixed-width bitlength stream of `values` (each in `width` bits
/// above `min`).
fn fixed_width_stream(values: &[i32], min: i32, max: i32) -> Vec<u8> {
    let mut fields = vec![(0, 1), (32, 6), (32, 6), (min as u32, 32), (max as u32, 32)];
    let mut range = i64::from(max) - i64::from(min);
    let mut width = 1;
    range >>= 1;
    while range != 0 {
        width += 1;
        range >>= 1;
    }
    fields.extend(
        values
            .iter()
            .map(|&v| ((i64::from(v) - i64::from(min)) as u32, width)),
    );
    pack(&fields)
}

/// Lag-1 deltas: the inverse of [`lag1`].
fn unlag1(v: &[i32]) -> Vec<i32> {
    let mut out = v.to_vec();
    for i in 4..v.len() {
        out[i] = v[i].wrapping_sub(v[i - 1]);
    }
    out
}

/// Reference arithmetic encoder (16 bits, Witten/Neal/Cleary) for the
/// context `freq` (cumulative in order): the MSB-first bit stream.
fn arith_encode(symbols: &[usize], freq: &[u64]) -> Vec<u8> {
    let total: u64 = freq.iter().sum();
    let (mut low, mut high) = (0u64, 0xFFFFu64);
    let mut follow = 0usize;
    let mut bits: Vec<(u32, u32)> = Vec::new();
    fn emit(bit: u32, follow: &mut usize, bits: &mut Vec<(u32, u32)>) {
        bits.push((bit, 1));
        for _ in 0..*follow {
            bits.push((1 - bit, 1));
        }
        *follow = 0;
    }
    for &s in symbols {
        let cum_low: u64 = freq[..s].iter().sum();
        let cum_high = cum_low + freq[s];
        let range = high - low + 1;
        high = low + range * cum_high / total - 1;
        low += range * cum_low / total;
        loop {
            if high < 0x8000 {
                emit(0, &mut follow, &mut bits);
            } else if low >= 0x8000 {
                emit(1, &mut follow, &mut bits);
                low -= 0x8000;
                high -= 0x8000;
            } else if low >= 0x4000 && high < 0xC000 {
                follow += 1;
                low -= 0x4000;
                high -= 0x4000;
            } else {
                break;
            }
            low *= 2;
            high = 2 * high + 1;
        }
    }
    follow += 1;
    emit(u32::from(low >= 0x4000), &mut follow, &mut bits);
    bits.push((0, 16));
    pack(&bits)
}

/// A probability context (mk2) as `read_context` reads it.
fn context_bytes(entries: &[(i64, u32, i32)], bits: (u32, u32, u32), min: i32) -> Vec<u8> {
    let mut fields = vec![
        (entries.len() as u32, 16),
        (bits.0, 6),
        (bits.1, 6),
        (bits.2, 6),
        (min as u32, 32),
    ];
    for &(symbol, occ, value) in entries {
        fields.push(((symbol + 2) as u32, bits.0));
        fields.push((occ, bits.1));
        fields.push(((value - min) as u32, bits.2));
    }
    pack(&fields)
}

/// An arithmetic packet (codec 3) of `symbols` (indices into `entries`).
fn arith_packet(entries: &[(i64, u32, i32)], symbols: &[usize], oob: &[i32]) -> Vec<u8> {
    let freq: Vec<u64> = entries.iter().map(|e| u64::from(e.1)).collect();
    let code = words(&arith_encode(symbols, &freq));
    W::default()
        .i32(symbols.len() as i32)
        .u8(3)
        .i32(8 * code.len() as i32)
        .bytes(&code)
        .bytes(&context_bytes(entries, (8, 16, 16), -100))
        .bytes(&raw_packet(oob))
        .0
}

fn reader(data: &[u8]) -> Reader<'_> {
    Reader::at(data, 0)
}

fn decode(bytes: &[u8]) -> Result<Vec<i32>> {
    Packet::read(&mut reader(bytes), 0)?.decode()
}

fn is_damaged<T: std::fmt::Debug>(r: Result<T>) -> bool {
    match r {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => {
            let s = e.to_string();
            s.contains("damaged") || s.contains("ends early")
        }
    }
}

// ---------------------------------------------------------------------------
// byte and bit readers
// ---------------------------------------------------------------------------

#[test]
fn reader_reads_little_endian_and_stops_at_the_end() {
    let data = W::default()
        .u8(0xAB)
        .i16(-2)
        .i32(-70_000)
        .u32(0xDEAD_BEEF)
        .f32(1.5)
        .bytes(b"xyz")
        .0;
    let mut r = reader(&data);
    assert_eq!(r.u8().unwrap(), 0xAB);
    assert_eq!(r.i16().unwrap(), -2);
    assert_eq!(r.i32().unwrap(), -70_000);
    assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
    assert_eq!(r.f32().unwrap(), 1.5);
    assert_eq!(r.at, 15);
    assert!(r.i32().is_err(), "3 bytes left");
    assert_eq!(r.at, 15, "a failed read does not move");
    assert_eq!(r.take(3).unwrap(), b"xyz");
    assert!(r.u8().is_err());
    assert_eq!(r.take(0).unwrap(), b"");
    // a huge take is an error, not an overflow
    let mut r = Reader::at(&data, 5);
    assert!(r.take(usize::MAX).is_err());
    let mut r = Reader::at(&data, usize::MAX);
    assert!(r.take(1).is_err());
    assert!(r.u8().is_err());
}

#[test]
fn counts_must_fit_into_the_rest_of_the_data() {
    let mut data = W::default().i32(3).0;
    data.resize(4 + 3 * 28, 0);
    assert_eq!(reader(&data).count(28).unwrap(), 3, "exactly fits");
    assert!(is_damaged(reader(&data).count(29)), "one byte short");
    assert!(is_damaged(reader(&W::default().i32(-1).0).count(1)));
    assert!(is_damaged(reader(&W::default().i32(i32::MIN).0).count(0)));
    // no overflow of count × item size
    assert!(is_damaged(
        reader(&W::default().i32(i32::MAX).0).count(usize::MAX)
    ));
    // more than MAX_VALUES is refused even for empty items
    let too_many = W::default().i32(MAX_VALUES as i32 + 1).0;
    assert!(is_damaged(reader(&too_many).count(0)));
    let at_most = W::default().i32(MAX_VALUES as i32).0;
    assert_eq!(reader(&at_most).count(0).unwrap(), MAX_VALUES);
    assert!(reader(&[1, 0]).count(1).is_err(), "no count");
}

#[test]
fn bits_read_msb_first_and_stop_after_the_slack() {
    let mut b = Bits::new(vec![0b1010_0000, 0xFF]);
    assert_eq!(b.u(1).unwrap(), 1);
    assert_eq!(b.u(3).unwrap(), 0b010);
    assert_eq!(b.s(4).unwrap(), 0);
    assert_eq!(b.s(8).unwrap(), -1);
    assert_eq!(b.u(32).unwrap(), 0, "zeros past the end");
    assert_eq!(b.u(32).unwrap(), 0);
    assert!(b.u(1).is_err(), "more than the slack");
    // code text words are little endian, read from the top bit
    let mut w = Bits::words(&[0x01, 0x00, 0x00, 0x80]);
    assert_eq!(w.u(1).unwrap(), 1);
    assert_eq!(w.u(31).unwrap(), 1);
}

#[test]
fn bit_fields_of_every_width() {
    let mut b = Bits::new(vec![0xFF; 8]);
    assert_eq!(b.u(0).unwrap(), 0);
    assert_eq!(b.pos, 0, "zero bits read nothing");
    assert_eq!(b.s(0).unwrap(), 0);
    assert_eq!(b.u(32).unwrap(), u32::MAX);
    assert_eq!(b.s(32).unwrap(), -1);
    assert!(is_damaged(Bits::new(vec![0; 8]).u(33)), "wider than 32");
    // signed fields: the top bit is the sign
    let mut s = Bits::new(pack(&[(0b011, 3), (0b100, 3), (1, 1), (0x8000_0000, 32)]));
    assert_eq!(s.s(3).unwrap(), 3);
    assert_eq!(s.s(3).unwrap(), -4);
    assert_eq!(s.s(1).unwrap(), -1);
    assert_eq!(s.s(32).unwrap(), i32::MIN);
    // fields across byte borders
    let mut x = Bits::new(vec![0b0000_0111, 0b1111_0000]);
    x.u(5).unwrap();
    assert_eq!(x.u(7).unwrap(), 0b111_1111);
    // the slack is exactly BIT_SLACK bits
    let mut e = Bits::new(Vec::new());
    for _ in 0..BIT_SLACK / 32 {
        assert_eq!(e.u(32).unwrap(), 0);
    }
    assert!(e.u(1).is_err());
    // an incomplete last word of code text is dropped
    let mut w = Bits::words(&[0, 0, 0, 0x80, 0xFF, 0xFF]);
    assert_eq!(w.data.len(), 4);
    assert_eq!(w.u(1).unwrap(), 1);
}

#[test]
fn lag1_keeps_the_first_four_values() {
    assert_eq!(lag1(vec![5, 6, 7, 8, 1, 1, -2]), vec![5, 6, 7, 8, 9, 10, 8]);
    assert_eq!(lag1(vec![1, 2]), vec![1, 2]);
    assert_eq!(lag1(Vec::new()), Vec::<i32>::new());
    assert_eq!(
        lag1(vec![0, 0, 0, i32::MAX, 1]),
        vec![0, 0, 0, i32::MAX, i32::MIN],
        "wraps instead of overflowing"
    );
    let values = vec![3, -1, 4, 1, -5, 9, 2, 6, i32::MIN, i32::MAX];
    assert_eq!(lag1(unlag1(&values)), values);
}

// ---------------------------------------------------------------------------
// packets: framing and codecs
// ---------------------------------------------------------------------------

#[test]
fn empty_and_raw_packets() {
    let mut r = reader(&[0, 0, 0, 0, 0xEE]);
    let p = Packet::read(&mut r, 0).unwrap();
    assert!(matches!(p, Packet::Empty));
    assert_eq!(r.at, 4, "an empty packet is only its count");
    assert_eq!(p.decode().unwrap(), Vec::<i32>::new());

    let bytes = raw_packet(&[1, -2, i32::MAX]);
    let mut r = reader(&bytes);
    let p = Packet::read(&mut r, 0).unwrap();
    assert_eq!(r.at, bytes.len());
    assert_eq!(p.count(), 3);
    assert_eq!(p.decode().unwrap(), vec![1, -2, i32::MAX]);
    // a bit length of 33 takes two words
    let two = W::default().i32(1).u8(0).i32(33).i32(7).i32(8).0;
    assert_eq!(decode(&two).unwrap(), vec![7, 8]);
}

#[test]
fn packet_headers_are_checked() {
    assert!(
        is_damaged(decode(&W::default().i32(-1).0)),
        "negative count"
    );
    let too_many = W::default()
        .i32(MAX_VALUES as i32 + 1)
        .u8(0)
        .i32(32)
        .i32(0)
        .0;
    assert!(is_damaged(decode(&too_many)));
    let no_bits = W::default().i32(1).u8(0).i32(0).0;
    assert!(is_damaged(decode(&no_bits)), "zero bit length");
    let negative_bits = W::default().i32(1).u8(0).i32(-32).0;
    assert!(is_damaged(decode(&negative_bits)));
    let short = W::default().i32(2).u8(0).i32(64).i32(1).0;
    assert!(decode(&short).is_err(), "the code text is cut short");
    let huge = W::default().i32(1).u8(1).i32(i32::MAX).0;
    assert!(decode(&huge).is_err());
    for codec in [2u8, 5, 0xFF] {
        let unknown = W::default().i32(1).u8(codec).i32(32).i32(0).0;
        let err = decode(&unknown).unwrap_err().to_string();
        assert!(err.contains(&format!("unknown codec {codec}")), "{err}");
    }
}

#[test]
fn every_truncated_packet_is_an_error() {
    let entries = [(0, 3, 10), (1, 1, 20), (-2, 1, 0)];
    let packets = [
        raw_packet(&[1, 2, 3]),
        bitlength_packet(3, &fixed_width_stream(&[1, 2, 3], 1, 3)),
        arith_packet(&entries, &[0, 1, 2, 0], &[99]),
        chopped(3, 2, 5, &raw_packet(&[1, 2, 3]), &raw_packet(&[0, 1, 2])),
    ];
    for bytes in packets {
        assert!(decode(&bytes).is_ok());
        for cut in 0..bytes.len() {
            let mut r = reader(&bytes[..cut]);
            let result = Packet::read(&mut r, 0).and_then(Packet::decode);
            // an arithmetic packet cut inside its code text may still decode
            // (the look-ahead reads zeros), but never panics
            if bytes[4] != 3 || cut < 9 {
                assert!(result.is_err(), "cut at {cut} of {}", bytes.len());
            }
        }
    }
}

/// A chopped packet (codec 4): `chop` high bits, `span` bits in all.
fn chopped(count: i32, chop: u8, span: u8, high: &[u8], low: &[u8]) -> Vec<u8> {
    W::default()
        .i32(count)
        .u8(4)
        .u8(chop)
        .i32(10)
        .u8(span)
        .bytes(high)
        .bytes(low)
        .0
}

#[test]
fn chopped_packets_join_high_and_low_bits() {
    // shift = span - chop = 3: (h << 3 | l) + bias 10
    let p = chopped(3, 2, 5, &raw_packet(&[1, 2, 0]), &raw_packet(&[3, 0, 7]));
    assert_eq!(decode(&p).unwrap(), vec![21, 26, 17]);
    // halves of another length
    let p = chopped(3, 2, 5, &raw_packet(&[1, 2]), &raw_packet(&[3, 0, 7]));
    assert!(is_damaged(decode(&p)));
    let p = chopped(2, 2, 5, &raw_packet(&[1, 2]), &raw_packet(&[]));
    assert!(is_damaged(decode(&p)), "an empty half");
    // more chopped bits than the span, or a span wider than 32
    let p = chopped(1, 6, 5, &raw_packet(&[1]), &raw_packet(&[1]));
    assert!(is_damaged(decode(&p)));
    let p = chopped(1, 1, 33, &raw_packet(&[1]), &raw_packet(&[1]));
    assert!(is_damaged(decode(&p)));
    // the widest shift: 31 bits, the result wraps
    let p = chopped(1, 1, 32, &raw_packet(&[3]), &raw_packet(&[0]));
    assert_eq!(decode(&p).unwrap(), vec![i32::MIN.wrapping_add(10)]);
}

#[test]
fn nothing_chopped_reads_the_inner_packet_up_to_the_depth_limit() {
    let wrap = |inner: Vec<u8>| W::default().i32(1).u8(4).u8(0).bytes(&inner).0;
    let mut p = raw_packet(&[42]);
    for _ in 0..MAX_DEPTH {
        p = wrap(p);
    }
    assert_eq!(decode(&p).unwrap(), vec![42], "{MAX_DEPTH} levels");
    assert!(is_damaged(decode(&wrap(p))), "one level more");
    // a long chain of chopped packets is refused without deep recursion
    let mut deep = Vec::new();
    for _ in 0..10_000 {
        deep.extend(W::default().i32(1).u8(4).u8(0).0);
    }
    deep.extend(raw_packet(&[1]));
    assert!(is_damaged(decode(&deep)));
}

#[test]
fn fixed_width_bitlength() {
    // fixed width: min 3 bits = 1, max 3 bits = 3 → 2-bit fields + 1
    let mut stream = Bits::new(pack(&[
        (0, 1),
        (3, 6),
        (3, 6),
        (1, 3),
        (3, 3),
        (1, 2),
        (2, 2),
        (0, 2),
    ]));
    assert_eq!(bitlength(&mut stream, 3).unwrap(), vec![2, 3, 1]);
    // negative values, full 32-bit range
    let values = [i32::MIN, -1, 0, 1, i32::MAX];
    let mut b = Bits::new(fixed_width_stream(&values, i32::MIN, i32::MAX));
    assert_eq!(bitlength(&mut b, 5).unwrap(), values);
    let values = [-7, -3, -5];
    let mut b = Bits::new(fixed_width_stream(&values, -7, -3));
    assert_eq!(bitlength(&mut b, 3).unwrap(), values);
    // the same packet through the codec
    let p = bitlength_packet(3, &fixed_width_stream(&values, -7, -3));
    assert_eq!(decode(&p).unwrap(), values);
}

#[test]
fn bitlength_with_an_empty_range_repeats_the_minimum() {
    // max ≤ min: no value bits, every value is min
    let mut b = Bits::new(pack(&[(0, 1), (4, 6), (4, 6), (5, 4), (2, 4)]));
    assert_eq!(bitlength(&mut b, 4).unwrap(), vec![5; 4]);
    assert_eq!(b.pos, 21, "no value bits read");
    // field widths above 32 bits
    let mut b = Bits::new(pack(&[(0, 1), (33, 6), (3, 6)]));
    assert!(is_damaged(bitlength(&mut b, 1)));
    let mut b = Bits::new(pack(&[(0, 1), (3, 6), (63, 6)]));
    assert!(is_damaged(bitlength(&mut b, 1)));
    // values beyond the end of the code text
    let mut b = Bits::new(pack(&[
        (0, 1),
        (32, 6),
        (32, 6),
        (0, 32),
        (u32::MAX >> 1, 32),
    ]));
    assert!(is_damaged(bitlength(&mut b, 10)), "310 value bits, 3 left");
}

#[test]
fn variable_width_bitlength() {
    // mean 10, 3 change bits (escapes -4 and 3), 2 run bits
    let stream = pack(&[
        (1, 1),
        (10, 32),
        (3, 3),
        (2, 3),
        // width 0 + 2 = 2, run 3: 1, -1, -2
        (2, 3),
        (3, 2),
        (1, 2),
        (3, 2),
        (2, 2),
        // escape +3, then +1: width 6, run 1: -20
        (3, 3),
        (1, 3),
        (1, 2),
        ((-20i32) as u32, 6),
        // escape -4, then -2: width 0, run 3 → clamped to the 2 left: mean
        (4, 3),
        (6, 3),
        (3, 2),
    ]);
    let mut b = Bits::new(stream.clone());
    assert_eq!(bitlength(&mut b, 6).unwrap(), vec![11, 9, 8, -10, 10, 10]);
    let p = bitlength_packet(6, &stream);
    assert_eq!(decode(&p).unwrap(), vec![11, 9, 8, -10, 10, 10]);
    // fewer values asked: the run is clamped
    let mut b = Bits::new(stream);
    assert_eq!(bitlength(&mut b, 2).unwrap(), vec![11, 9]);
}

#[test]
fn variable_width_bitlength_never_loops_endlessly() {
    // zero change bits are refused
    let mut b = Bits::new(pack(&[(1, 1), (0, 32), (0, 3), (2, 3)]));
    assert!(is_damaged(bitlength(&mut b, 1)));
    // runs of length 0 forever: the code text runs out
    let mut b = Bits::new(pack(&[(1, 1), (0, 32), (2, 3), (0, 3)]));
    assert!(is_damaged(bitlength(&mut b, 5)));
    // an endless chain of escapes: runs out too
    let mut b = Bits::new(pack(&[(1, 1), (0, 32), (2, 3), (2, 3), (0x5555_5555, 32)]));
    assert!(is_damaged(bitlength(&mut b, 1)));
    // the width leaves 0..=32
    let mut fields = vec![(1, 1), (0, 32), (7, 3), (1, 3)];
    fields.extend([(63, 7), (10, 7)]);
    let mut b = Bits::new(pack(&fields));
    assert!(is_damaged(bitlength(&mut b, 1)), "width 73");
    let mut b = Bits::new(pack(&[(1, 1), (0, 32), (2, 3), (1, 3), (3, 2)]));
    assert!(is_damaged(bitlength(&mut b, 1)), "width -1");
}

#[test]
fn arithmetic_with_a_single_symbol_needs_no_bits() {
    let ctx = [Entry {
        symbol: 0,
        occurrences: 1,
        value: 7,
    }];
    let mut bits = Bits::new(vec![0, 0]);
    assert_eq!(arithmetic(&mut bits, 5, &ctx, &[]).unwrap(), vec![7; 5]);
    let escape = [Entry {
        symbol: -2,
        occurrences: 1,
        value: 0,
    }];
    let mut bits = Bits::new(vec![0, 0]);
    assert_eq!(
        arithmetic(&mut bits, 2, &escape, &[4, 9]).unwrap(),
        vec![4, 9]
    );
    let mut bits = Bits::new(vec![0, 0]);
    assert!(
        arithmetic(&mut bits, 3, &escape, &[4, 9]).is_err(),
        "out-of-band values run out"
    );
}

fn entries(list: &[(i64, u32, i32)]) -> Vec<Entry> {
    list.iter()
        .map(|&(symbol, occ, value)| Entry {
            symbol,
            occurrences: u64::from(occ),
            value,
        })
        .collect()
}

#[test]
fn arithmetic_decodes_what_the_reference_encoder_wrote() {
    let list = [(0, 5, 100), (1, 2, -7), (2, 1, 0), (3, 30, 42), (-2, 2, 0)];
    let ctx = entries(&list);
    let freq: Vec<u64> = ctx.iter().map(|e| e.occurrences).collect();
    // a pseudo-random sequence, with long runs of the frequent symbol
    // (underflow, E3) and rare symbols next to each other
    let mut seed = 12345u32;
    let mut symbols = Vec::new();
    for _ in 0..2000 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        symbols.push((seed >> 16) as usize % list.len());
    }
    symbols.extend([3; 200]);
    symbols.extend([2, 2, 2, 1, 1, 4, 4, 2]);
    let oob: Vec<i32> = (0..symbols.iter().filter(|&&s| s == 4).count() as i32)
        .map(|i| 1000 + i)
        .collect();
    let mut next_oob = oob.iter();
    let expected: Vec<i32> = symbols
        .iter()
        .map(|&s| match list[s] {
            (-2, _, _) => *next_oob.next().unwrap(),
            (_, _, v) => v,
        })
        .collect();
    let mut bits = Bits::new(arith_encode(&symbols, &freq));
    assert_eq!(
        arithmetic(&mut bits, symbols.len(), &ctx, &oob).unwrap(),
        expected
    );
    // the same through the packet codec (context and out-of-band packet)
    let p = arith_packet(&list, &symbols, &oob);
    assert_eq!(decode(&p).unwrap(), expected);
    // a two-symbol context at the limit of the total count
    let wide = [(0, 0xFFFE, 1), (1, 1, 2)];
    let s: Vec<usize> = (0..50).map(|i| usize::from(i % 7 == 0)).collect();
    let mut bits = Bits::new(arith_encode(&s, &[0xFFFE, 1]));
    let want: Vec<i32> = s.iter().map(|&i| wide[i].2).collect();
    assert_eq!(
        arithmetic(&mut bits, s.len(), &entries(&wide), &[]).unwrap(),
        want
    );
}

#[test]
fn arithmetic_rejects_bad_contexts_and_code_text() {
    let mut bits = Bits::new(vec![0; 4]);
    assert!(is_damaged(arithmetic(&mut bits, 1, &[], &[])), "no entries");
    let zero = entries(&[(0, 0, 1)]);
    assert!(is_damaged(arithmetic(&mut bits, 1, &zero, &[])), "total 0");
    let big = entries(&[(0, 0x8000, 1), (1, 0x8000, 2)]);
    assert!(
        is_damaged(arithmetic(&mut bits, 1, &big, &[])),
        "total 2^16"
    );
    // count 0 reads only the first code word
    let two = entries(&[(0, 1, 1), (1, 1, 2)]);
    let mut bits = Bits::new(Vec::new());
    assert_eq!(
        arithmetic(&mut bits, 0, &two, &[]).unwrap(),
        Vec::<i32>::new()
    );
    assert_eq!(bits.pos, 16);
    // a code text that ends: the look-ahead slack, then an error – a
    // context of two equal symbols needs one bit per value
    let mut bits = Bits::new(vec![0x55; 2]);
    assert!(is_damaged(arithmetic(&mut bits, 1000, &two, &[])));
    // random code text against a skewed context: values or an error
    let skew = entries(&[(0, 1, 1), (1, 60_000, 2), (-2, 3, 0)]);
    for seed in 0..64u8 {
        let data: Vec<u8> = (0..32u8)
            .map(|i| seed.wrapping_mul(37) ^ i.wrapping_mul(11))
            .collect();
        let mut bits = Bits::new(data);
        if let Ok(v) = arithmetic(&mut bits, 100, &skew, &[5; 100]) {
            assert_eq!(v.len(), 100);
        }
    }
}

#[test]
fn contexts_are_read_bit_by_bit_and_end_byte_aligned() {
    let list = [(0, 5, -3), (-2, 1, 0), (7, 60_000, 250)];
    let mut data = context_bytes(&list, (4, 16, 9), -10);
    // 16 + 18 + 32 header bits, 3 × 29 entry bits = 153 bits → 20 bytes
    assert_eq!(data.len(), 20);
    data.extend([0xEE; 3]);
    let mut r = reader(&data);
    let ctx = read_context(&mut r).unwrap();
    assert_eq!(r.at, 20);
    let got: Vec<(i64, u64, i32)> = ctx
        .iter()
        .map(|e| (e.symbol, e.occurrences, e.value))
        .collect();
    assert_eq!(got, vec![(0, 5, -3), (-2, 1, 0), (7, 60_000, 250)]);
    // more entries than bits left
    let mut short = context_bytes(&list, (4, 16, 9), -10);
    short.truncate(15);
    assert!(is_damaged(read_context(&mut reader(&short))));
    // a header alone, cut short
    assert!(is_damaged(read_context(&mut reader(&[0xFF, 0xFF]))));
    // 65535 entries of zero bits: allowed, nothing to read
    let empty = pack(&[(0xFFFF, 16), (0, 6), (0, 6), (0, 6), (0, 32)]);
    let mut r = reader(&empty);
    assert_eq!(read_context(&mut r).unwrap().len(), 0xFFFF);
    assert_eq!(r.at, empty.len());
}

// ---------------------------------------------------------------------------
// coordinates and normals
// ---------------------------------------------------------------------------

/// A float as its exponent (with sign) and mantissa.
fn split_float(v: f32) -> (i32, i32) {
    let bits = v.to_bits();
    ((bits >> 23) as i32, (bits & 0x7F_FFFF) as i32)
}

/// Exponent and mantissa packets of one axis.
fn exp_mant_packets(values: &[f32], predicted: bool) -> Vec<u8> {
    let (mut e, mut m): (Vec<i32>, Vec<i32>) = values.iter().map(|&v| split_float(v)).unzip();
    if predicted {
        e = unlag1(&e);
        m = unlag1(&m);
    }
    [raw_packet(&e), raw_packet(&m)].concat()
}

/// Coordinates stored as floats (no quantisation).
fn float_coordinates(points: &[[f32; 3]]) -> Vec<u8> {
    let mut w = W::default().i32(points.len() as i32).u8(3);
    for _ in 0..3 {
        w = w.f32(0.0).f32(0.0).u8(0);
    }
    for axis in 0..3 {
        let values: Vec<f32> = points.iter().map(|p| p[axis]).collect();
        w = w.bytes(&exp_mant_packets(&values, true));
    }
    w.i32(0).0
}

/// Normals stored as floats.
fn float_normals(normals: &[[f32; 3]]) -> Vec<u8> {
    let mut w = W::default().i32(normals.len() as i32).u8(3).u8(0);
    for axis in 0..3 {
        let values: Vec<f32> = normals.iter().map(|p| p[axis]).collect();
        w = w.bytes(&exp_mant_packets(&values, false));
    }
    w.i32(0).0
}

#[test]
fn float_coordinates_are_exact() {
    let points = [
        [0.0, 1.0, -2.5],
        [1e-3, -0.25, 3.0],
        [-7.75, 0.5, 1e6],
        [0.1, 0.2, 0.3],
        [0.4, -0.5, 0.6],
        [123.456, -1e-7, 0.0],
    ];
    let bytes = float_coordinates(&points);
    let mut r = reader(&bytes);
    let got = read_coordinates(&mut r).unwrap();
    assert_eq!(r.at, bytes.len());
    let want: Vec<[f64; 3]> = points.iter().map(|p| p.map(f64::from)).collect();
    assert_eq!(got, want);
}

#[test]
fn quantised_coordinates_take_the_middle_of_their_cell() {
    // x in [0, 1] with 2 bits: cells of 0.25, code c → (c - 0.5) × 0.25;
    // y in [-8, 8] with 4 bits; z with 32 bits (step = range / u32::MAX)
    let codes_x = [1, 2, 3, 4, 2];
    let codes_y = [0, 16, 8, 1, 3];
    let codes_z = [1, 1, 1, 1, 1];
    let w = W::default()
        .i32(5)
        .u8(3)
        .f32(0.0)
        .f32(1.0)
        .u8(2)
        .f32(-8.0)
        .f32(8.0)
        .u8(4)
        .f32(0.0)
        .f32(4.0)
        .u8(32)
        .bytes(&raw_packet(&unlag1(&codes_x)))
        .bytes(&raw_packet(&unlag1(&codes_y)))
        .bytes(&raw_packet(&unlag1(&codes_z)))
        .i32(0);
    let got = read_coordinates(&mut reader(&w.0)).unwrap();
    let xs: Vec<f64> = got.iter().map(|p| p[0]).collect();
    assert_eq!(xs, vec![0.125, 0.375, 0.625, 0.875, 0.375]);
    let ys: Vec<f64> = got.iter().map(|p| p[1]).collect();
    assert_eq!(ys, vec![-8.5, 7.5, -0.5, -7.5, -5.5]);
    let z = 0.5 * 4.0 / f64::from(u32::MAX);
    assert!(got.iter().all(|p| (p[2] - z).abs() < 1e-15), "{got:?}");
}

#[test]
fn coordinate_headers_and_counts_are_checked() {
    let good = float_coordinates(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
    assert_eq!(read_coordinates(&mut reader(&good)).unwrap().len(), 2);
    // two components
    let mut two = good.clone();
    two[4] = 2;
    assert!(read_coordinates(&mut reader(&two)).is_err());
    // a count that does not match the packets
    let mut other = good.clone();
    other[..4].copy_from_slice(&3i32.to_le_bytes());
    assert!(is_damaged(read_coordinates(&mut reader(&other))));
    // more points than bytes
    let mut huge = good.clone();
    huge[..4].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(is_damaged(read_coordinates(&mut reader(&huge))));
    // quantised axes of different lengths
    let w = W::default()
        .i32(2)
        .u8(3)
        .f32(0.0)
        .f32(1.0)
        .u8(8)
        .f32(0.0)
        .f32(1.0)
        .u8(8)
        .f32(0.0)
        .f32(1.0)
        .u8(8)
        .bytes(&raw_packet(&[1, 2]))
        .bytes(&raw_packet(&[1]))
        .bytes(&raw_packet(&[1, 2]))
        .i32(0);
    assert!(is_damaged(read_coordinates(&mut reader(&w.0))));
    for cut in 0..good.len() {
        assert!(
            read_coordinates(&mut reader(&good[..cut])).is_err(),
            "{cut}"
        );
    }
}

#[test]
fn float_normals_and_their_checks() {
    let normals = [[0.0, 0.0, 1.0], [0.6, -0.8, 0.0], [-1.0, 0.0, 0.0]];
    let bytes = float_normals(&normals);
    let mut r = reader(&bytes);
    let got = read_normals(&mut r).unwrap();
    assert_eq!(r.at, bytes.len());
    assert_eq!(got, normals.map(|n| n.map(f64::from)).to_vec());
    let mut two = bytes.clone();
    two[4] = 2;
    assert!(
        is_damaged(read_normals(&mut reader(&two))),
        "two components"
    );
    for cut in 0..bytes.len() {
        assert!(read_normals(&mut reader(&bytes[..cut])).is_err(), "{cut}");
    }
}

fn deering_normals(bits: u8, codes: &[[i32; 4]]) -> Vec<u8> {
    let column = |i: usize| raw_packet(&codes.iter().map(|c| c[i]).collect::<Vec<_>>());
    W::default()
        .i32(codes.len() as i32)
        .u8(3)
        .u8(bits)
        .bytes(&column(0))
        .bytes(&column(1))
        .bytes(&column(2))
        .bytes(&column(3))
        .i32(0)
        .0
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-8)
}

#[test]
fn deering_codes_give_unit_normals_in_every_sextant_and_octant() {
    let table = DeeringTable::new();
    let h = std::f64::consts::FRAC_1_SQRT_2;
    // theta 0, psi 0: 45° in the first sextant
    assert!(close(table.normal(0, 7, 0, 0, 0).unwrap(), [h, 0.0, h]));
    // the octant bits flip x (4), y (2), z (1)
    assert!(close(table.normal(0, 0, 0, 0, 0).unwrap(), [-h, 0.0, -h]));
    assert!(close(table.normal(0, 3, 0, 0, 0).unwrap(), [-h, 0.0, h]));
    // the end of the psi table: the corner direction of the sextant
    let n = table.normal(0, 7, 8192, 8192, 0).unwrap();
    let (s, c) = (1.0 / 3f64.sqrt(), (2.0 / 3f64).sqrt());
    assert!(close(n, [c, s, 0.0]), "{n:?}");
    // odd sextants read theta one further, and permute the axes
    assert!(close(
        table.normal(1, 7, 8191, 0, 0).unwrap(),
        [0.0, 0.0, 1.0]
    ));
    let base = table.normal(0, 7, 100, 3000, 0).unwrap();
    let [x, y, z] = base;
    let perms = [
        [x, y, z],
        [z, y, x],
        [y, z, x],
        [y, x, z],
        [z, x, y],
        [x, z, y],
    ];
    for (sextant, want) in perms.iter().enumerate() {
        let theta = 100 - (sextant as i32 & 1);
        let n = table.normal(sextant as i32, 7, theta, 3000, 0).unwrap();
        assert!(close(n, *want), "sextant {sextant}: {n:?} {want:?}");
    }
    // a sweep: every code is a unit vector
    for sextant in 0..6 {
        for octant in 0..8 {
            for code in (0..=64).step_by(7) {
                let n = table.normal(sextant, octant, code, 64 - code, 7).unwrap();
                let len = n.iter().map(|v| v * v).sum::<f64>().sqrt();
                assert!((len - 1.0).abs() < 1e-9, "{sextant} {octant} {code}");
            }
        }
    }
}

#[test]
fn deering_codes_out_of_the_table_are_errors() {
    let table = DeeringTable::new();
    assert!(is_damaged(table.normal(0, 7, 8193, 0, 0)));
    assert!(is_damaged(table.normal(1, 7, 8192, 0, 0)), "theta + 1");
    assert!(is_damaged(table.normal(0, 7, 0, 8193, 0)));
    assert!(is_damaged(table.normal(0, 7, -1, 0, 0)));
    assert!(is_damaged(table.normal(0, 7, 0, i32::MIN, 0)));
    assert!(is_damaged(table.normal(0, 7, 0, 3, 12)), "3 << 12 > 8192");
    assert!(
        is_damaged(table.normal(0, 7, 1, 0, 64)),
        "shift out of range"
    );
    // unknown sextants fall back to the identity, octants use three bits
    assert_eq!(
        table.normal(9, -1, 10, 20, 0).unwrap(),
        table.normal(0, 7, 11, 20, 0).unwrap()
    );
}

#[test]
fn deering_normals_are_read_with_their_bit_count() {
    // 6 bits: codes are scaled by 2^(13 - 6)
    let bytes = deering_normals(6, &[[0, 7, 0, 0], [0, 7, 64, 64], [3, 0, 10, 5]]);
    let got = read_normals(&mut reader(&bytes)).unwrap();
    let table = DeeringTable::new();
    assert_eq!(got[0], table.normal(0, 7, 0, 0, 7).unwrap());
    assert_eq!(got[1], table.normal(0, 7, 64, 64, 7).unwrap());
    assert_eq!(got[2], table.normal(3, 0, 10, 5, 7).unwrap());
    // more than 13 bits
    let bytes = deering_normals(14, &[[0, 7, 0, 0]]);
    assert!(is_damaged(read_normals(&mut reader(&bytes))));
    // a code beyond the table at this bit count
    let bytes = deering_normals(6, &[[0, 7, 65, 0]]);
    assert!(is_damaged(read_normals(&mut reader(&bytes))));
    // columns of different lengths
    let mut bytes = deering_normals(6, &[[0, 7, 0, 0], [0, 7, 1, 1]]);
    bytes[..4].copy_from_slice(&1i32.to_le_bytes());
    assert!(is_damaged(read_normals(&mut reader(&bytes))));
}

// ---------------------------------------------------------------------------
// dual mesh coder
// ---------------------------------------------------------------------------

/// The 24 symbol streams of the mesh coder (all empty).
fn no_symbols() -> Vec<Vec<i32>> {
    vec![Vec::new(); 24]
}

/// Indices of the symbol streams.
const DEGREES: usize = 0;
const VALENCES: usize = 8;
const GROUPS: usize = 9;
const MASKS: usize = 11;
const SPLIT_FACES: usize = 22;
const SPLIT_POSITIONS: usize = 23;

fn symbols(streams: &[Vec<i32>]) -> Symbols {
    let packets = streams
        .iter()
        .map(|s| Packet::read(&mut reader(&raw_packet(s)), 0).unwrap())
        .collect();
    Symbols::decode(packets).unwrap()
}

/// One polygon with `n` corners, each mesh vertex of degree 1 (the
/// degree contexts: 3 corners → 1, then 0; 4 → 4, then 3; 5 → 6; more → 7).
fn polygon(n: usize) -> Vec<Vec<i32>> {
    let mut s = no_symbols();
    let contexts: Vec<usize> = (0..n)
        .map(|i| match (n, i) {
            (3, 0) => 1,
            (3, _) => 0,
            (4, 0) => 4,
            (4, _) => 3,
            (5, _) => 6,
            _ => 7,
        })
        .collect();
    for c in contexts {
        s[DEGREES + c].push(1);
    }
    s[VALENCES] = vec![n as i32];
    s[GROUPS] = vec![0];
    s
}

/// A closed tetrahedron: four triangles, every vertex of degree 3.
fn tetrahedron() -> Vec<Vec<i32>> {
    let mut s = no_symbols();
    s[DEGREES + 1] = vec![3];
    s[DEGREES] = vec![3, 3, 3];
    s[VALENCES] = vec![3; 4];
    s[GROUPS] = vec![0; 4];
    s
}

/// Two triangles back to back (a closed "pillow"), vertices of degree 2.
fn pillow() -> Vec<Vec<i32>> {
    let mut s = no_symbols();
    s[DEGREES + 1] = vec![2];
    s[DEGREES] = vec![2, 2];
    s[VALENCES] = vec![3, 3];
    s[GROUPS] = vec![0, 0];
    s
}

/// The polygons of the decoded dual mesh as mesh vertex lists.
fn run(streams: &[Vec<i32>]) -> Result<Vec<Vec<i32>>> {
    let mut d = MeshDecoder::new(symbols(streams));
    d.run()?;
    let m = &d.mesh;
    (0..m.v_val.len())
        .map(|v| (0..m.valence(v)?).map(|s| m.face(v, s)).collect())
        .collect()
}

/// Every directed edge once and its reverse once.
fn closed(polygons: &[Vec<i32>]) -> bool {
    let mut edges = std::collections::HashMap::<(i32, i32), i32>::new();
    for p in polygons {
        for (i, &a) in p.iter().enumerate() {
            let b = p[(i + 1) % p.len()];
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    edges
        .iter()
        .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
}

#[test]
fn the_symbol_streams_come_in_their_order() {
    let streams: Vec<Vec<i32>> = (0..24).map(|i| vec![i, 100 + i, 1, 1, 1]).collect();
    let s = symbols(&streams);
    assert_eq!(s.degrees[0][0], 0);
    assert_eq!(s.degrees[7][0], 7);
    assert_eq!(s.valences[0], 8);
    assert_eq!(s.groups[0], 9);
    assert_eq!(s.attr_masks[0][0], 11);
    assert_eq!(s.attr_masks[7][0], 18);
    assert_eq!(s.attr_masks_30[0], 19);
    assert_eq!(s.attr_masks_4[0], 20);
    assert_eq!(s.attr_masks_large[0], 21);
    // split faces (22) are lag-1 predicted, the others are not
    assert_eq!(s.split_faces, vec![22, 122, 1, 1, 2]);
    assert_eq!(s.split_positions, vec![23, 123, 1, 1, 1]);
    assert_eq!(s.valences, vec![8, 108, 1, 1, 1]);
    // fewer than 24 packets
    let short: Vec<Packet> = (0..23).map(|_| Packet::Empty).collect();
    assert!(Symbols::decode(short).is_err());
}

#[test]
fn single_polygons_of_every_degree_context() {
    for n in [3, 4, 5, 6, 9] {
        let polygons = run(&polygon(n)).unwrap();
        assert_eq!(polygons, vec![(0..n as i32).collect::<Vec<_>>()], "{n}");
    }
    // a symbol in the wrong degree context is missing where it is needed
    let mut s = polygon(3);
    s[DEGREES + 1].clear();
    s[DEGREES + 2].push(1);
    assert!(is_damaged(run(&s)));
}

#[test]
fn closed_meshes_are_rebuilt_with_consistent_orientation() {
    let tet = run(&tetrahedron()).unwrap();
    assert_eq!(
        tet,
        vec![vec![0, 1, 2], vec![2, 1, 3], vec![2, 3, 0], vec![3, 1, 0]]
    );
    assert!(closed(&tet));
    let pillow = run(&pillow()).unwrap();
    assert_eq!(pillow, vec![vec![0, 1, 2], vec![2, 1, 0]]);
    assert!(closed(&pillow));
}

#[test]
fn the_mesh_coder_stops_at_the_end_of_the_valences() {
    // no valences: nothing
    assert_eq!(run(&no_symbols()).unwrap(), Vec::<Vec<i32>>::new());
    // a negative valence ends the mesh, the rest is ignored
    let mut s = polygon(3);
    s[VALENCES] = vec![3, -1, 3];
    assert_eq!(run(&s).unwrap().len(), 1);
    // a polygon without corners
    let mut s = no_symbols();
    s[VALENCES] = vec![0, 0];
    assert_eq!(run(&s).unwrap(), vec![Vec::<i32>::new(), Vec::new()]);
    // a valence above the limit
    let mut s = polygon(3);
    s[VALENCES] = vec![MAX_DEGREE + 1];
    assert!(is_damaged(run(&s)));
}

#[test]
fn the_mesh_coder_rejects_missing_or_bad_symbols() {
    // the tetrahedron without its last vertex: an open face needs a polygon
    let mut s = tetrahedron();
    s[VALENCES].pop();
    assert!(is_damaged(run(&s)));
    // a degree symbol missing, negative or above the limit
    for bad in [None, Some(-1), Some(MAX_DEGREE + 1)] {
        let mut s = tetrahedron();
        s[DEGREES].pop();
        s[DEGREES].extend(bad);
        assert!(is_damaged(run(&s)), "{bad:?}");
    }
    // degree 0 asks for a split, but there is no split symbol
    let mut s = polygon(3);
    s[DEGREES + 1] = vec![0];
    assert!(is_damaged(run(&s)));
    // the third corner splits: to no active face (offset 0, past the two
    // active faces, negative), without a position, or at a slot the face
    // (of degree 1) does not have
    for (offset, position) in [
        (0, Some(0)),
        (3, Some(0)),
        (-1, Some(0)),
        (1, None),
        (1, Some(1)),
        (2, Some(-1)),
    ] {
        let mut s = polygon(3);
        s[DEGREES] = vec![1, 0];
        s[SPLIT_FACES] = vec![offset];
        s[SPLIT_POSITIONS] = position.into_iter().collect();
        assert!(is_damaged(run(&s)), "{offset} {position:?}");
    }
}

#[test]
fn the_mesh_coder_has_an_incidence_limit() {
    let mut mesh = DualMesh::default();
    assert!(is_damaged(mesh.new_vtx(MAX_INCIDENCES + 1, 0)));
    let mut counter = 0;
    assert!(is_damaged(mesh.new_face(
        MAX_INCIDENCES + 1,
        Vec::new(),
        &mut counter
    )));
    assert!(
        mesh.v_val.is_empty() && mesh.f_deg.is_empty(),
        "nothing added"
    );
    // slots are checked
    let v = mesh.new_vtx(2, 0).unwrap() as usize;
    let f = mesh.new_face(3, vec![0b101], &mut counter).unwrap();
    assert_eq!(counter, 2, "two attributes");
    assert!(is_damaged(mesh.face(v, 2)));
    assert!(is_damaged(mesh.vtx(f, 3)));
    assert!(is_damaged(mesh.vtx(-1, 0)));
    assert!(is_damaged(mesh.vtx(7, 0)));
    assert!(is_damaged(mesh.set_vtx_face(v, 2, f)));
    assert!(is_damaged(mesh.set_face_vtx(f, 3, 0)));
    assert!(is_damaged(mesh.valence(1)));
    assert!(!mesh.is_valid_face(-1) && !mesh.is_valid_face(1) && mesh.is_valid_face(f));
    assert!(is_damaged(mesh.vtx_face_attr(0, 9)));
}

#[test]
fn attributes_are_counted_per_face_and_found_per_corner() {
    let mut mesh = DualMesh::default();
    let mut counter = 10;
    // face 0: degree 3, attributes at slots 0 and 2
    let f = mesh.new_face(3, vec![0b101, 0], &mut counter).unwrap();
    for (slot, v) in [(0, 4), (1, 5), (2, 6)] {
        mesh.set_face_vtx(f, slot, v).unwrap();
    }
    assert_eq!(mesh.f_empty[0], 0);
    assert_eq!(counter, 12);
    // slot 0 starts the first attribute, slot 1 shares it, slot 2 the second
    assert_eq!(mesh.vtx_face_attr(4, f).unwrap(), Some(10));
    assert_eq!(mesh.vtx_face_attr(5, f).unwrap(), Some(10));
    assert_eq!(mesh.vtx_face_attr(6, f).unwrap(), Some(11));
    assert_eq!(mesh.vtx_face_attr(7, f).unwrap(), None, "not a corner");
    // a face without attributes
    let g = mesh.new_face(2, vec![0], &mut counter).unwrap();
    assert_eq!(mesh.vtx_face_attr(0, g).unwrap(), None);
    // a mask past the degree counts nothing
    let h = mesh.new_face(1, vec![0b10, 1], &mut counter).unwrap();
    assert_eq!(mesh.f_attr[idx(h).unwrap()], (12, 0));
    // the counter saturates instead of wrapping
    let mut full = u32::MAX - 1;
    mesh.new_face(3, vec![0b111], &mut full).unwrap();
    assert_eq!(full, u32::MAX);
}

#[test]
fn polygon_masks_of_large_degrees() {
    // degree 70: the mask comes from the large-mask stream, in 3 words
    let mut s = polygon(3);
    s[DEGREES + 1] = vec![70];
    s[DEGREES] = vec![1, 1];
    s[MASKS + 7] = vec![0b11, 0b1];
    s[MASKS + 7 + 3] = vec![1, 2, 3, 99];
    let mut d = MeshDecoder::new(symbols(&s));
    d.run().unwrap_or_default();
    assert_eq!(d.mesh.f_deg[0], 70);
    assert_eq!(d.mesh.f_mask[0], vec![1, 2, 3]);
    assert_eq!(d.mask_large_pos, 3);
    // degree 64: low bits, plus the high streams at bits 30 and 60
    let mut s = polygon(3);
    s[DEGREES + 1] = vec![64];
    s[DEGREES] = vec![1, 1];
    s[MASKS + 7] = vec![0b11];
    s[MASKS + 8] = vec![0b1];
    s[MASKS + 9] = vec![0b1];
    let mut d = MeshDecoder::new(symbols(&s));
    d.run().unwrap_or_default();
    assert_eq!(d.mesh.f_mask[0], vec![(1 << 30) | 0b11, 1 << 28]);
    // degree 2 reads attribute mask context 0
    let mut s = pillow();
    s[MASKS] = vec![0b01, 0b10, 0b11];
    let mut d = MeshDecoder::new(symbols(&s));
    d.run().unwrap();
    let masks: Vec<u32> = d.mesh.f_mask.iter().map(|m| m[0]).collect();
    assert_eq!(masks, vec![0b01, 0b10, 0b11]);
    assert_eq!(d.attr_counter, 4);
}

// ---------------------------------------------------------------------------
// tri-strip set elements, segments and the file
// ---------------------------------------------------------------------------

/// The element data of a Tri-Strip Set Shape LOD.
fn tri_strip_data(
    streams: &[Vec<i32>],
    points: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
) -> Vec<u8> {
    let mut w = W::default()
        .i32(1)
        .i16(1)
        .i16(1)
        .bytes(&[0; 8])
        .i16(1)
        .i32(2)
        .i16(1);
    for s in streams {
        w = w.bytes(&raw_packet(s));
    }
    let binding = 2 | if normals.is_some() { 8 } else { 0 };
    w = w
        .u32(0)
        .bytes(&[binding, 0, 0, 0, 0, 0, 0, 0])
        .bytes(&[0; 4])
        .i32(points.len() as i32);
    if points.is_empty() {
        return w.0;
    }
    w = w.i32(0).bytes(&float_coordinates(points));
    if let Some(n) = normals {
        w = w.bytes(&float_normals(n));
    }
    w.0
}

/// A shape LOD segment around element data.
fn segment(kind: i32, object: [u8; 16], data: &[u8]) -> Vec<u8> {
    let element = W::default()
        .i32(17 + data.len() as i32)
        .bytes(&object)
        .u8(1)
        .bytes(data)
        .0;
    W::default()
        .bytes(&[0x5E; 16])
        .i32(kind)
        .i32(24 + element.len() as i32)
        .bytes(&element)
        .0
}

/// A JT 9 file with the segments (TOC type, bytes).
fn jt_file(version: &str, segments: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut head = version.as_bytes().to_vec();
    head.resize(80, b' ');
    let toc = 80 + 1 + 4 + 4 + 16;
    let mut body = Vec::new();
    let mut entries = W::default().i32(segments.len() as i32);
    let mut offset = toc + 4 + 28 * segments.len();
    for (kind, bytes) in segments {
        entries = entries
            .bytes(&[0x5E; 16])
            .i32(offset as i32)
            .i32(bytes.len() as i32)
            .u32(kind << 24);
        offset += bytes.len();
        body.extend(bytes);
    }
    W::default()
        .bytes(&head)
        .u8(0)
        .i32(0)
        .i32(toc as i32)
        .bytes(&[0x15; 16])
        .bytes(&entries.0)
        .bytes(&body)
        .0
}

const TRIANGLE: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [0.001, 0.0, 0.0], [0.0, 0.002, 0.0]];
const TETRA: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.01, 0.0, 0.0],
    [0.0, 0.01, 0.0],
    [0.0, 0.0, 0.01],
];

fn lod_of(streams: &[Vec<i32>], points: &[[f32; 3]]) -> Result<Lod> {
    read_tri_strip_set(&mut reader(&tri_strip_data(streams, points, None)))
}

#[test]
fn a_tri_strip_set_becomes_triangles_and_normal_corners() {
    let lod = lod_of(&tetrahedron(), &TETRA).unwrap();
    assert_eq!(
        lod.triangles,
        vec![[0, 1, 2], [2, 1, 3], [2, 3, 0], [3, 1, 0]]
    );
    assert_eq!(lod.points, TETRA.map(|p| p.map(f64::from)).to_vec());
    assert!(lod.normals.is_empty() && !lod.normals_valid());
    assert_eq!(lod.normal_triangles, vec![[u32::MAX; 3]; 4]);
    // a pentagon is a fan of three triangles
    let five = [
        [0.0; 3],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.5, 2.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let lod = lod_of(&polygon(5), &five).unwrap();
    assert_eq!(lod.triangles, vec![[0, 1, 2], [0, 2, 3], [0, 3, 4]]);
    // normals: one attribute per corner of every mesh vertex
    let mut s = polygon(3);
    s[MASKS] = vec![1, 1, 1];
    let normals = [[0.0, 0.0, 1.0]; 3];
    let data = tri_strip_data(&s, &TRIANGLE, Some(&normals));
    let lod = read_tri_strip_set(&mut reader(&data)).unwrap();
    assert_eq!(lod.normal_triangles, vec![[0, 1, 2]]);
    assert!(lod.normals_valid());
    // a normal index past the normals
    let data = tri_strip_data(&s, &TRIANGLE, Some(&normals[..2]));
    assert!(!read_tri_strip_set(&mut reader(&data))
        .unwrap()
        .normals_valid());
}

#[test]
fn hole_polygons_are_not_triangles() {
    // the fourth triangle of the tetrahedron is a hole (group -1)
    let mut s = tetrahedron();
    s[GROUPS] = vec![0, 0, 0, -1];
    assert_eq!(lod_of(&s, &TETRA).unwrap().triangles.len(), 3);
    // a missing group is a hole too
    s[GROUPS] = vec![0, 0];
    assert_eq!(lod_of(&s, &TETRA).unwrap().triangles.len(), 2);
}

#[test]
fn triangle_corners_must_be_points() {
    // three points for the four vertices of the tetrahedron
    assert!(is_damaged(lod_of(&tetrahedron(), &TETRA[..3])));
    assert!(is_damaged(lod_of(&polygon(4), &TRIANGLE)));
}

#[test]
fn tri_strip_set_headers_are_checked() {
    // no vertices: an empty level of detail, the coordinates are not read
    let lod = lod_of(&tetrahedron(), &[]).unwrap();
    assert!(lod.points.is_empty() && lod.triangles.is_empty());
    // 2D or 4D coordinates, or none
    let good = tri_strip_data(&tetrahedron(), &TETRA, None);
    let bindings = good.len() - float_coordinates(&TETRA).len() - 4 - 4 - 4 - 8;
    assert_eq!(good[bindings], 2);
    for coords in [0u8, 1, 4, 7] {
        let mut bad = good.clone();
        bad[bindings] = coords;
        let err = read_tri_strip_set(&mut reader(&bad)).unwrap_err();
        assert!(
            err.to_string().contains("no 3D coordinates"),
            "{coords}: {err}"
        );
    }
    // normals announced but missing
    let mut bad = good.clone();
    bad[bindings] = 2 | 8;
    assert!(read_tri_strip_set(&mut reader(&bad)).is_err());
    // fewer than 24 packets before the vertex data
    for cut in 0..good.len() {
        assert!(
            read_tri_strip_set(&mut reader(&good[..cut])).is_err(),
            "{cut}"
        );
    }
}

#[test]
fn segments_hold_one_tri_strip_set_element() {
    let data = tri_strip_data(&tetrahedron(), &TETRA, None);
    let seg = segment(7, TRI_STRIP_SET_LOD, &data);
    let lod = read_segment(&seg, 0, seg.len()).unwrap().unwrap();
    assert_eq!(lod.triangles.len(), 4);
    // inside a larger file, at an offset
    let mut file = vec![0xAA; 33];
    file.extend(&seg);
    file.extend([0xBB; 7]);
    assert!(read_segment(&file, 33, seg.len()).unwrap().is_some());
    // another element type is skipped
    let mut other = TRI_STRIP_SET_LOD;
    other[0] ^= 1;
    let seg2 = segment(7, other, &data);
    assert!(read_segment(&seg2, 0, seg2.len()).unwrap().is_none());
    // out of range, overflowing, cut short
    assert!(read_segment(&seg, 1, seg.len()).is_err());
    assert!(read_segment(&seg, usize::MAX, 2).is_err());
    assert!(read_segment(&seg, 0, 43).is_err(), "no object type");
    assert!(
        read_segment(&seg, 0, seg.len() - 1).is_err(),
        "element longer"
    );
    let mut negative = seg.clone();
    negative[24..28].copy_from_slice(&(-1i32).to_le_bytes());
    assert!(is_damaged(read_segment(&negative, 0, seg.len())));
    let mut huge = seg.clone();
    huge[24..28].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(read_segment(&huge, 0, seg.len()).is_err());
    // an element shorter than its data
    let mut short = seg.clone();
    short[24..28].copy_from_slice(&40i32.to_le_bytes());
    assert!(read_segment(&short, 0, seg.len()).is_err());
}

fn tetra_segment(kind: i32) -> Vec<u8> {
    segment(
        kind,
        TRI_STRIP_SET_LOD,
        &tri_strip_data(&tetrahedron(), &TETRA, None),
    )
}

#[test]
fn a_jt_file_is_read_in_millimetres_and_refined() {
    let file = jt_file("Version 9.5 JT", &[(7, tetra_segment(7))]);
    let mesh = read_file(&file).unwrap();
    // four triangles, no normals: not refined
    assert_eq!(mesh.triangle_count(), 4);
    assert_eq!(mesh.parts, 1);
    assert_eq!(
        mesh.positions,
        vec![0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0]
    );
    assert_eq!(mesh.indices, vec![0, 1, 2, 2, 1, 3, 2, 3, 0, 3, 1, 0]);
    super::super::read("jt", &file).unwrap();
    // with valid normals the LOD is refined 3 × 3
    let mut s = polygon(3);
    s[MASKS] = vec![1, 1, 1];
    let data = tri_strip_data(&s, &TRIANGLE, Some(&[[0.0, 0.0, 1.0]; 3]));
    let file = jt_file(
        "Version 9.5 JT",
        &[(7, segment(7, TRI_STRIP_SET_LOD, &data))],
    );
    let mesh = read_file(&file).unwrap();
    assert_eq!(mesh.triangle_count(), 9);
    // flat normals: the refined points stay in the triangle (millimetres)
    let b = mesh.bounds();
    let want = [0.0, 0.0, 0.0, 1.0, 2.0, 0.0];
    assert!(
        b.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-5),
        "{b:?}"
    );
}

#[test]
fn every_lod_zero_is_a_part() {
    let file = jt_file(
        "Version 9.5 JT",
        &[
            (7, tetra_segment(7)),
            (8, tetra_segment(8)),
            (7, tetra_segment(7)),
            (2, vec![0; 3]),
        ],
    );
    let mesh = read_file(&file).unwrap();
    assert_eq!(mesh.parts, 2, "the type-8 LOD is not read");
    assert_eq!(mesh.triangle_count(), 8);
    assert_eq!(&mesh.indices[12..15], &[4, 5, 6], "indices shifted");
}

#[test]
fn without_lod_zero_the_finest_lod_is_taken() {
    let quad = [
        [0.0; 3],
        [0.001, 0.0, 0.0],
        [0.001, 0.001, 0.0],
        [0.0, 0.001, 0.0],
    ];
    let square = segment(
        9,
        TRI_STRIP_SET_LOD,
        &tri_strip_data(&polygon(4), &quad, None),
    );
    let file = jt_file(
        "Version 9.5 JT",
        &[
            (9, square.clone()),
            (12, tetra_segment(12)),
            (9, square.clone()),
        ],
    );
    let mesh = read_file(&file).unwrap();
    assert_eq!(
        (mesh.parts, mesh.triangle_count()),
        (1, 4),
        "the tetrahedron"
    );
    // only the first of equally fine LODs
    let file = jt_file("Version 9.5 JT", &[(16, square.clone()), (6, square)]);
    assert_eq!(read_file(&file).unwrap().parts, 1);
    // segments of other types (5, 17) are not shape LODs
    let file = jt_file(
        "Version 9.5 JT",
        &[(5, tetra_segment(5)), (17, tetra_segment(17))],
    );
    let err = read_file(&file).unwrap_err().to_string();
    assert!(err.contains("contains no triangles"), "{err}");
    // a damaged LOD 0 is an error, not a fallback
    let mut bad = tetra_segment(7);
    let n = bad.len();
    bad.truncate(n - 10);
    let file = jt_file("Version 9.5 JT", &[(7, bad), (8, tetra_segment(8))]);
    assert!(read_file(&file).is_err());
}

#[test]
fn an_empty_lod_is_no_geometry() {
    let empty = segment(
        7,
        TRI_STRIP_SET_LOD,
        &tri_strip_data(&tetrahedron(), &[], None),
    );
    let file = jt_file("Version 9.5 JT", &[(7, empty)]);
    assert_eq!(read_jt(&file).unwrap().triangle_count(), 0);
    let err = super::super::read("jt", &file).unwrap_err().to_string();
    assert!(err.contains("no geometry"), "{err}");
}

#[test]
fn jt_headers_are_checked() {
    let good = jt_file("Version 9.5 JT", &[(7, tetra_segment(7))]);
    assert!(read_jt(&good).is_ok());
    // other versions are named
    for (version, shown) in [
        ("Version 8.1 JT", "JT 8.1"),
        ("Version 10.5 JT", "JT 10.5"),
        ("Version ", "JT ?"),
        ("Version 9", "JT 9 "),
    ] {
        let mut v = good.clone();
        let mut head = version.as_bytes().to_vec();
        head.resize(80, b' ');
        v[..80].copy_from_slice(&head);
        let err = read_jt(&v).unwrap_err().to_string();
        assert!(err.contains(shown), "{version}: {err}");
    }
    // big endian
    let mut big = good.clone();
    big[80] = 1;
    let err = read_jt(&big).unwrap_err().to_string();
    assert!(err.contains("Big-endian"), "{err}");
    // the TOC offset: negative, past the end, in the middle of nowhere
    for toc in [-1, i32::MAX, good.len() as i32, 0] {
        let mut bad = good.clone();
        bad[85..89].copy_from_slice(&toc.to_le_bytes());
        assert!(read_jt(&bad).is_err(), "{toc}");
    }
    // the TOC count: negative, more entries than bytes
    let one_too_many = ((good.len() - 109) / 28 + 1) as i32;
    for count in [-1, one_too_many, i32::MAX] {
        let mut bad = good.clone();
        bad[105..109].copy_from_slice(&count.to_le_bytes());
        assert!(is_damaged(read_jt(&bad)), "{count}");
    }
    // segment offset and length: negative, past the end
    for (at, value) in [
        (125, -1),
        (125, i32::MAX),
        (129, -1),
        (129, i32::MAX),
        (129, 10),
    ] {
        let mut bad = good.clone();
        bad[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(read_jt(&bad).is_err(), "{at} = {value}");
    }
    // cut anywhere
    for cut in 0..good.len() {
        assert!(read_jt(&good[..cut]).is_err(), "{cut}");
    }
}

#[test]
fn not_jt_or_garbage_is_an_error() {
    assert!(read_jt(b"Version 10.5 JT").is_err());
    let mut fake = b"Version 9.4 JT".to_vec();
    fake.resize(200, 0);
    assert!(read_jt(&fake).is_err());
    assert!(read_nx(b"SPLMSSTR").is_err());
    assert!(read_file(b"SPLMSSTR").is_err());
    let newer = read_file(&[b"Version 10.0 JT".as_slice(), &[b' '; 80]].concat());
    assert!(newer.unwrap_err().to_string().contains("10.0"));
    let err = read_file(b"").unwrap_err().to_string();
    assert!(err.contains("not a JT file"), "{err}");
    let err = read_file(b"version 9.5 JT").unwrap_err().to_string();
    assert!(err.contains("not a JT file"), "case matters: {err}");
}

#[test]
fn the_jt_file_starts_after_the_stream_index() {
    // a large part: 259 index entries put the JT file past 4 KiB
    for count in [19u32, 259] {
        let mut stream = Vec::new();
        stream.extend(9u32.to_le_bytes());
        stream.extend(count.to_le_bytes());
        stream.extend(0u32.to_le_bytes());
        stream.resize(12 + 16 * count as usize, 0x11);
        stream.extend(b"Version 9.4 JT DM");
        assert_eq!(jt_start(&stream), Some(12 + 16 * count as usize));
    }
    // another layout: found by searching
    let mut odd = vec![0u8; 100];
    odd.extend(b"Version 9.4 JT");
    assert_eq!(jt_start(&odd), Some(100));
    assert_eq!(jt_start(b"short"), None);
}

#[test]
fn the_jt_search_is_bounded() {
    // an index count far past the end: searched instead
    let mut stream = W::default().u32(9).u32(u32::MAX).u32(0).0;
    stream.extend(b"Version 9.5 JT");
    assert_eq!(jt_start(&stream), Some(12));
    // a version text inside the index: the one after the index wins
    let mut stream = W::default().u32(9).u32(1).u32(0).0;
    stream.extend(b"Version 9.4 JT\0\0");
    stream.extend(b"Version 9.5 JT");
    assert_eq!(jt_start(&stream), Some(28));
    // no count at all
    assert_eq!(jt_start(b"Version"), None);
    // the version text only after 64 KiB is not found
    let mut far = W::default().u32(9).u32(0).u32(0).0;
    far.resize(64 * 1024, 0);
    far.extend(b"Version 9.5 JT");
    assert_eq!(jt_start(&far), None);
    let mut near = far.clone();
    near.drain(..10);
    assert_eq!(jt_start(&near), Some(64 * 1024 - 10));
}

// ---------------------------------------------------------------------------
// the real JT data of the NIST NX parts
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/cad/nist/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

const HEX_NUT: &str = "mtc-assembly/nx/90591A141 HEX NUT.prt";

/// The JT file inside an NX part.
fn jt_of(nx: &[u8]) -> &[u8] {
    let streams = crate::nxpart::streams(nx);
    let s = streams
        .iter()
        .find(|s| s.name == crate::nxpart::JT_STREAM)
        .unwrap();
    let stream = crate::nxpart::data(nx, s).unwrap();
    &stream[jt_start(stream).unwrap()..]
}

/// The TOC entries (offset, length, type) of a JT file, read by the test.
fn toc(jt: &[u8]) -> Vec<(usize, usize, u32)> {
    let le = |at: usize| u32::from_le_bytes(jt[at..at + 4].try_into().unwrap());
    let at = le(85) as usize;
    (0..le(at) as usize)
        .map(|i| {
            let e = at + 4 + 28 * i;
            (le(e + 16) as usize, le(e + 20) as usize, le(e + 24) >> 24)
        })
        .collect()
}

#[test]
fn standalone_jt_files_are_read() {
    let nx = fixture(HEX_NUT);
    let mesh = super::super::read("jt", jt_of(&nx)).unwrap();
    assert_eq!(
        mesh.triangle_count(),
        read_nx(&nx).unwrap().triangle_count()
    );
    // the standalone JT of the assembly is JT 10.5: reported, not read
    let jt = fixture("mtc-assembly/nx/NIST mtc crada assembly.jt");
    let err = read_file(&jt).unwrap_err().to_string();
    assert!(err.contains("JT 10.5"), "{err}");
}

#[test]
fn an_nx_assembly_has_no_jt_stream() {
    let nx = fixture("mtc-assembly/nx/NIST mtc crada assembly.prt");
    let err = read_nx(&nx).unwrap_err().to_string();
    assert!(err.contains("no lightweight representation"), "{err}");
}

/// The TOC index and (offset, length) of the one LOD 0 of the hex nut.
fn nut_lod0(jt: &[u8]) -> (usize, usize, usize) {
    let segments = toc(jt);
    let lod0: Vec<usize> = (0..segments.len())
        .filter(|&i| segments[i].2 == 7)
        .collect();
    assert_eq!(lod0.len(), 1);
    let (offset, length, _) = segments[lod0[0]];
    (lod0[0], offset, length)
}

/// Where the TOC entry `i` of a JT file starts.
fn toc_entry(jt: &[u8], i: usize) -> usize {
    u32::from_le_bytes(jt[85..89].try_into().unwrap()) as usize + 4 + 28 * i
}

#[test]
fn the_hex_nut_decodes_to_known_lods() {
    let nx = fixture(HEX_NUT);
    let jt = jt_of(&nx);
    // LOD 0 (type 7) and two coarser LODs (8, 9): triangles, points, normals
    let mut found = Vec::new();
    for (offset, length, kind) in toc(jt) {
        if let Some(lod) = read_segment(jt, offset, length).unwrap() {
            assert!(lod.normals_valid(), "type {kind}");
            let n = lod.points.len() as u32;
            assert!(lod.triangles.iter().flatten().all(|&i| i < n));
            // Deering normals are unit vectors
            for normal in &lod.normals {
                let len = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
                assert!((len - 1.0).abs() < 1e-6, "{normal:?}");
            }
            // a 3/8" nut: every point within a few centimetres (metres)
            assert!(lod.points.iter().flatten().all(|c| c.abs() < 0.05));
            found.push((
                kind,
                lod.triangles.len(),
                lod.points.len(),
                lod.normals.len(),
            ));
        }
    }
    found.sort_unstable();
    assert_eq!(
        found,
        vec![
            (7, 2286, 1143, 2146),
            (8, 898, 449, 868),
            (9, 358, 179, 968)
        ]
    );
    // the symbol streams of LOD 0: bitlength and arithmetic packets, splits
    let (_, offset, length) = nut_lod0(jt);
    let element = &jt[offset..offset + length];
    let mut r = Reader::at(element, 45 + 24);
    let packets: Vec<Packet> = (0..24).map(|_| Packet::read(&mut r, 0).unwrap()).collect();
    let mut codecs = [0usize; 5];
    for p in &packets {
        codecs[match p {
            Packet::Empty => 0,
            Packet::Raw(_) => 1,
            Packet::Bitlength { .. } => 2,
            Packet::Arithmetic { .. } => 3,
            Packet::Chopped { .. } => 4,
        }] += 1;
    }
    assert_eq!(codecs, [7, 0, 7, 10, 0]);
    let symbols = Symbols::decode(packets).unwrap();
    assert_eq!(symbols.split_faces.len(), 83, "the split path is used");
    assert_eq!(symbols.valences, vec![3; 2286], "one polygon per triangle");
}

#[test]
fn the_hex_nut_lod_is_refined_three_times_three() {
    let nx = fixture(HEX_NUT);
    let mesh = read_nx(&nx).unwrap();
    assert_eq!(mesh.triangle_count(), 2286 * 9);
    assert_eq!(mesh.parts, 1);
    // without LOD 0 the finest other LOD (type 8) is taken
    let mut jt = jt_of(&nx).to_vec();
    let (i, _, _) = nut_lod0(&jt);
    let attributes = toc_entry(&jt, i) + 24;
    jt[attributes + 3] = 0;
    assert_eq!(read_jt(&jt).unwrap().triangle_count(), 898 * 9);
}

#[test]
fn the_hex_nut_shape_data_ends_where_it_is_read() {
    let nx = fixture(HEX_NUT);
    let jt = jt_of(&nx);
    let (_, offset, length) = nut_lod0(jt);
    // the segment cut to `cut` bytes, its element length to match
    let cut_to = |cut: usize| {
        let mut seg = jt[offset..offset + cut].to_vec();
        seg[24..28].copy_from_slice(&((cut - 28) as i32).to_le_bytes());
        read_segment(&seg, 0, cut)
    };
    // the tri-strip data ends at byte 31633; what follows is not read
    const END: usize = 31_633;
    assert!(END < length);
    assert_eq!(cut_to(END).unwrap().unwrap().triangles.len(), 2286);
    for cut in (45..END).step_by(397).chain(END - 40..END) {
        assert!(cut_to(cut).is_err(), "cut at {cut}");
    }
}

#[test]
fn damaged_hex_nut_headers_are_errors() {
    let nx = fixture(HEX_NUT);
    let good = jt_of(&nx).to_vec();
    let (i, offset, _) = nut_lod0(&good);
    let entry = toc_entry(&good, i);
    let with = |at: usize, value: i32| {
        let mut jt = good.clone();
        jt[at..at + 4].copy_from_slice(&value.to_le_bytes());
        read_jt(&jt)
    };
    // the TOC entry of LOD 0: offset and length
    assert!(is_damaged(with(entry + 16, -1)));
    assert!(with(entry + 16, good.len() as i32).is_err());
    assert!(is_damaged(with(entry + 20, -1)));
    assert!(with(entry + 20, i32::MAX).is_err());
    // the element length, the count of the first packet
    assert!(is_damaged(with(offset + 24, -1)));
    assert!(with(offset + 24, i32::MAX).is_err());
    let first_packet = offset + 45 + 24;
    assert!(is_damaged(with(first_packet, -1)));
    assert!(is_damaged(with(first_packet, MAX_VALUES as i32 + 1)));
    // the codec of the first packet
    assert_ne!(&good[first_packet..first_packet + 4], &[0; 4]);
    let mut jt = good.clone();
    jt[first_packet + 4] = 9;
    let err = read_jt(&jt).unwrap_err().to_string();
    assert!(err.contains("unknown codec 9"), "{err}");
    // the version, the byte order
    let mut jt = good.clone();
    jt[8] = b'8';
    let err = read_jt(&jt).unwrap_err().to_string();
    assert!(err.contains("JT 8."), "{err}");
    let mut jt = good;
    jt[80] = 1;
    assert!(read_jt(&jt).is_err());
}

/// Flipped bytes in the shape data: an error or a mesh, never a panic or
/// an endless loop.
#[test]
fn flipped_bytes_in_the_hex_nut_never_panic() {
    let nx = fixture(HEX_NUT);
    let jt = jt_of(&nx);
    let (_, offset, length) = nut_lod0(jt);
    let segment = &jt[offset..offset + length];
    let mut errors = 0;
    for at in (45..length).step_by(331) {
        for flip in [0x01, 0x80, 0xFF] {
            let mut seg = segment.to_vec();
            seg[at] ^= flip;
            if let Ok(Some(lod)) = read_segment(&seg, 0, length) {
                let n = lod.points.len() as u32;
                assert!(lod.triangles.iter().flatten().all(|&i| i < n), "{at}");
            } else {
                errors += 1;
            }
        }
    }
    assert!(errors > 0);
}

#[test]
fn a_damaged_jt_stream_in_an_nx_part_is_an_error() {
    let nx = fixture(HEX_NUT);
    let streams = crate::nxpart::streams(&nx);
    let s = streams
        .iter()
        .find(|s| s.name == crate::nxpart::JT_STREAM)
        .unwrap();
    let start = s.offset as usize;
    let stream = crate::nxpart::data(&nx, s).unwrap();
    let at = start + jt_start(stream).unwrap();
    // no version text: the JT file is not found
    let mut copy = nx.clone();
    copy[at] = b'X';
    let err = read_nx(&copy).unwrap_err().to_string();
    assert!(
        err.contains("lightweight (JT) data of the NX part is damaged"),
        "{err}"
    );
    // a TOC offset past the end of the stream
    let mut copy = nx.clone();
    copy[at + 85..at + 89].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(read_nx(&copy).is_err());
    // the part cut inside the JT stream
    assert!(read_nx(&nx[..at + 1000]).is_err());
}

/// The mesh coder looks at the faces of a polygon only for valences 3 and
/// 4: a polygon of the largest valence takes linear time, not quadratic
/// (65536 corners took minutes before, now a fraction of a second).
#[test]
fn a_polygon_of_the_largest_valence_decodes_in_linear_time() {
    let n = MAX_DEGREE as usize;
    let start = std::time::Instant::now();
    let polygons = run(&polygon(n)).unwrap();
    assert_eq!(polygons.len(), 1);
    assert_eq!(polygons[0].len(), n);
    assert!(polygons[0].iter().enumerate().all(|(i, &f)| f == i as i32));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        start.elapsed()
    );
    // two large polygons sharing every corner: a closed pillow
    let mut s = no_symbols();
    s[DEGREES + 7] = vec![2; 1024];
    s[VALENCES] = vec![1024, 1024];
    s[GROUPS] = vec![0, 0];
    let pillow = run(&s).unwrap();
    assert!(closed(&pillow));
    assert_eq!(pillow[1].len(), 1024);
}
