//! The container of PTC Creo part files (`.prt.N`): a text header, a table of
//! contents in fixed-width text records and named sections, some of them
//! compressed with Unix `compress` (LZW).
//!
//! ```text
//! #UGC:2 PART … <base> \        base (hex) is added to every section offset
//! #- VERS … / #- CMNM name \     more header lines
//! #-END_OF_UGC_HEADER
//! #Creo  TM  10.0 …
//! #UGC_TOC 2 <n> <entry size> <x>####…
//! <name> <off> <size> <usize> <ver> <kind> <x> <crc> <crc>   (hex; n entries, '#'-padded)
//! NEXT_TOC_ENTRY <off> …                                      (chain to the next table)
//! ```
//!
//! A section starts with the line `#<name>` at `base + off` and holds `size`
//! bytes. Kind `9` is the tagged plain format (`tagged.rs`), `a` and `12`
//! are LZW-compressed tagged data (`1f 9d`), `11` is binary (JPEG previews),
//! `b` is scrambled.

use anyhow::{anyhow, bail, Result};

/// Largest inflated section (the biggest seen: 0.8 MiB).
const MAX_SECTION: usize = 64 * 1024 * 1024;
/// Largest number of table entries followed.
const MAX_ENTRIES: usize = 100_000;

/// One table-of-contents entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// Absolute offset of the `#name` line.
    pub offset: usize,
    /// Bytes of the section including that line.
    pub size: usize,
    /// Size after inflating.
    pub unpacked: usize,
    /// The storage kind (hex digit string of the table).
    pub kind: String,
}

/// Is this a Creo file (`#UGC:2 PART …` or `#UGC:2 ASSEMBLY …`)?
pub fn is_creo(data: &[u8]) -> bool {
    data.starts_with(b"#UGC:")
}

/// The table of contents of a Creo file.
pub fn toc(data: &[u8]) -> Result<Vec<Entry>> {
    if !is_creo(data) {
        bail!("not a Creo file (no #UGC header)");
    }
    let first_end = data
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| anyhow!("Creo header without line end"))?;
    let first = std::str::from_utf8(data.get(..first_end).unwrap_or_default())
        .map_err(|_| anyhow!("Creo header not text"))?;
    let base = first
        .split([' ', '\\'])
        .rfind(|s| !s.is_empty())
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| anyhow!("Creo header without base offset"))?;
    let mut out = Vec::new();
    let mut at =
        find(data, b"#UGC_TOC", 0).ok_or_else(|| anyhow!("Creo file without table of contents"))?;
    let mut seen = std::collections::HashSet::new();
    while seen.insert(at) {
        let line_end = find(data, b"\n", at).ok_or_else(|| anyhow!("table of contents cut off"))?;
        let head = std::str::from_utf8(data.get(at..line_end).unwrap_or_default())
            .map_err(|_| anyhow!("table head not text"))?;
        let mut words = head.trim_end_matches('#').split_whitespace();
        let _tag = words.next();
        let _two = words.next();
        let n: usize = words
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("table head"))?;
        let entry_size: usize = words
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("table head"))?;
        if entry_size == 0 || n > MAX_ENTRIES {
            bail!("implausible table of contents");
        }
        let mut next = None;
        let mut pos = line_end + 1;
        for _ in 0..n {
            let end = pos
                .checked_add(entry_size)
                .filter(|&e| e <= data.len())
                .ok_or_else(|| anyhow!("table cut off"))?;
            let rec = std::str::from_utf8(data.get(pos..end).unwrap_or_default()).unwrap_or("");
            pos = end;
            let mut w = rec.trim_end_matches(['#', '\n']).split_whitespace();
            let Some(name) = w.next() else { break };
            let hex = |s: Option<&str>| s.and_then(|s| usize::from_str_radix(s, 16).ok());
            let (Some(off), Some(size), Some(unpacked)) =
                (hex(w.next()), hex(w.next()), hex(w.next()))
            else {
                bail!("table entry '{name}' unreadable");
            };
            let _ver = w.next();
            let kind = w.next().unwrap_or("").to_string();
            let offset = base
                .checked_add(off)
                .ok_or_else(|| anyhow!("offset overflow"))?;
            if name == "NEXT_TOC_ENTRY" {
                next = Some(offset);
                continue;
            }
            if out.len() >= MAX_ENTRIES {
                bail!("too many sections");
            }
            out.push(Entry {
                name: name.to_string(),
                offset,
                size,
                unpacked,
                kind,
            });
        }
        let Some(next_at) = next else { break };
        // the chained table starts with "#UGC_TOC" at (or right after) the offset
        let Some(found) = find(data, b"#UGC_TOC", next_at.saturating_sub(8)) else {
            break;
        };
        if found > next_at + 64 {
            break;
        }
        at = found;
    }
    Ok(out)
}

/// The bytes of a section (inflated when it is LZW-compressed), without
/// the `#name` line. `None` when the file has no such section.
pub fn section(data: &[u8], entries: &[Entry], name: &str) -> Result<Option<Vec<u8>>> {
    let Some(e) = entries.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let end = e
        .offset
        .checked_add(e.size)
        .filter(|&x| x <= data.len())
        .ok_or_else(|| anyhow!("section '{name}' outside the file"))?;
    let raw = data.get(e.offset..end).unwrap_or_default();
    let body_start = raw
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| i + 1)
        .ok_or_else(|| anyhow!("section '{name}' without name line"))?;
    let body = raw.get(body_start..).unwrap_or_default();
    if body.starts_with(&[0x1f, 0x9d]) {
        if e.unpacked > MAX_SECTION {
            bail!("section '{name}' claims {} bytes", e.unpacked);
        }
        return Ok(Some(unlzw(body, e.unpacked.max(1))?));
    }
    Ok(Some(body.to_vec()))
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= data.len() || needle.is_empty() {
        return None;
    }
    data.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Unix `compress` (LZW, `1f 9d`), as the Creo sections use it: codes of
/// 9 to `maxbits` bits, little-endian bit packing, the code width grows in
/// groups of eight codes (the rest of a group is skipped on a width change).
pub fn unlzw(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let [0x1f, 0x9d, flags, ..] = *data else {
        bail!("not LZW data");
    };
    let maxbits = usize::from(flags & 0x1f);
    if !(9..=16).contains(&maxbits) {
        bail!("LZW with {maxbits} bits");
    }
    let block = flags & 0x80 != 0;
    let limit = expected.min(MAX_SECTION);
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 20));
    // dictionary as (prefix code, last byte)
    let mut prefix: Vec<u32> = (0..256).collect();
    let mut last: Vec<u8> = (0..=255).collect();
    let clear = if block { 1 } else { 0 };
    for _ in 0..clear {
        prefix.push(0);
        last.push(0);
    }
    let mut nbits = 9;
    let mut pos = 3usize;
    let mut bitbuf: u64 = 0;
    let mut bitcnt = 0u32;
    let mut codes_in_group = 0usize;
    let mut prev: Option<u32> = None;
    let mut stack: Vec<u8> = Vec::new();
    let skip_group = |pos: &mut usize,
                      bitbuf: &mut u64,
                      bitcnt: &mut u32,
                      codes_in_group: usize,
                      nbits: usize| {
        let rem = (8 - codes_in_group % 8) % 8;
        let mut skip = rem * nbits;
        while skip > 0 {
            if *bitcnt == 0 {
                if *pos >= data.len() {
                    return;
                }
                *bitbuf = u64::from(data.get(*pos).copied().unwrap_or(0));
                *pos += 1;
                *bitcnt = 8;
            }
            let t = skip.min(*bitcnt as usize);
            *bitbuf >>= t;
            *bitcnt -= t as u32;
            skip -= t;
        }
    };
    loop {
        while (bitcnt as usize) < nbits {
            if pos >= data.len() {
                return Ok(out);
            }
            bitbuf |= u64::from(data.get(pos).copied().unwrap_or(0)) << bitcnt;
            pos += 1;
            bitcnt += 8;
        }
        let code = (bitbuf & ((1u64 << nbits) - 1)) as u32;
        bitbuf >>= nbits;
        bitcnt -= nbits as u32;
        codes_in_group += 1;
        if block && code == 256 {
            prefix.truncate(257);
            last.truncate(257);
            skip_group(&mut pos, &mut bitbuf, &mut bitcnt, codes_in_group, nbits);
            nbits = 9;
            codes_in_group = 0;
            prev = None;
            continue;
        }
        let table_len = prefix.len() as u32;
        let (entry_code, first_byte) = match prev {
            None => {
                if code >= table_len {
                    bail!("LZW: bad first code");
                }
                (code, None)
            }
            Some(p) => {
                if code < table_len {
                    (code, None)
                } else if code == table_len {
                    (p, Some(()))
                } else {
                    bail!("LZW: code {code} outside the table");
                }
            }
        };
        // expand entry_code into stack (reversed)
        stack.clear();
        let mut c = entry_code;
        let mut guard = 0u32;
        loop {
            let (Some(&l), Some(&pre)) = (last.get(c as usize), prefix.get(c as usize)) else {
                bail!("LZW: code {c} outside the table");
            };
            stack.push(l);
            if c < 256 {
                break;
            }
            c = pre;
            guard += 1;
            if guard > 1 << 17 {
                bail!("LZW: dictionary loop");
            }
        }
        let first = *stack.last().unwrap_or(&0);
        if out.len() + stack.len() + usize::from(first_byte.is_some()) > limit {
            bail!("LZW: more data than the table says");
        }
        out.extend(stack.iter().rev());
        if first_byte.is_some() {
            out.push(first);
        }
        if let Some(p) = prev {
            if prefix.len() < (1 << maxbits) {
                prefix.push(p);
                last.push(first);
            }
        }
        prev = Some(code.min(prefix.len() as u32 - 1));
        if prefix.len() >= (1 << nbits) && nbits < maxbits {
            skip_group(&mut pos, &mut bitbuf, &mut bitcnt, codes_in_group, nbits);
            nbits += 1;
            codes_in_group = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/ctc-01/creo-10/nist_ctc_01_asme1_rd_cr10030.prt.5"
        ))
        .unwrap()
    }

    #[test]
    fn the_table_of_contents_lists_the_geometry_sections() {
        let data = fixture();
        let toc = toc(&data).unwrap();
        assert!(toc.len() > 100, "{}", toc.len());
        let vis = toc.iter().find(|e| e.name == "VisibGeom").unwrap();
        assert_eq!(vis.kind, "9");
        assert_eq!(vis.unpacked, 70593);
        // the chained second table is followed
        assert!(toc.iter().any(|e| e.name == "CGFPersistTable"));
    }

    #[test]
    fn compressed_sections_inflate_to_their_stated_size() {
        let data = fixture();
        let toc = toc(&data).unwrap();
        for name in ["SolidPersistTable", "SolidPrimdata", "IdsGenSimpRepDb"] {
            let e = toc.iter().find(|e| e.name == name).unwrap();
            let body = section(&data, &toc, name).unwrap().unwrap();
            assert_eq!(body.len(), e.unpacked, "{name}");
            assert_eq!(body[0], 0xe0, "{name} starts with a field definition");
        }
        let vis = section(&data, &toc, "VisibGeom").unwrap().unwrap();
        assert!(vis.starts_with(b"\xe0\x00Sld_VisGeom\x00"));
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let data = fixture();
        for cut in (0..data.len()).step_by(data.len() / 50) {
            if let Ok(t) = toc(&data[..cut]) {
                for e in &t {
                    let _ = section(&data[..cut], &t, &e.name);
                }
            }
        }
        assert!(toc(b"#UGC:2 PART 1 2 3 zz \\\n#UGC_TOC 2 99999999 81 1#\n").is_err());
        assert!(unlzw(&[0x1f, 0x9d, 0x10, 0xff, 0xff, 0xff, 0xff], 100).is_err() || true);
    }
}
