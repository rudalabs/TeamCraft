//! References inside CAD files: an assembly names its component files, a
//! drawing the model it documents, a STEP file its external parts, a KiCad
//! project its sheets.
//!
//! The names are found without the CAD programs' own libraries: TeamCraft
//! scans the file for text – ASCII/UTF-8 and UTF-16 (Windows programs such
//! as SolidWorks, Inventor, Solid Edge and CATIA store paths that way) – that
//! ends with the extension of a CAD file (`Bracket.SLDPRT`,
//! `C:\Rover\Motor mount.ipt`, `'nozzle.stp'`, `"power.kicad_sch"`). Paths
//! are reduced to the file name. The result is a list of referenced file
//! names; `services::references` matches them against the files in the
//! vault.
//!
//! This is a heuristic: a name that a CAD program keeps in its file after
//! the component was removed is still found, and compressed sections are not
//! read. The revision page therefore shows the names as "found in the file".

use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::Path;

use crate::cadformat::{self, FormatKind};

/// Bytes scanned per file.
pub const MAX_SCAN: u64 = 32 * 1024 * 1024;
/// References kept per file.
const MAX_REFERENCES: usize = 500;
/// Longest file name accepted.
const MAX_NAME: usize = 160;

/// Whether files of this format are scanned for references: assemblies,
/// drawings, multi-purpose models (NX, Fusion, FreeCAD, …), STEP/JT (external
/// references), PLM XML and KiCad projects/schematics.
pub fn scans(format_id: &str) -> bool {
    match cadformat::by_id(format_id) {
        Some(f) => {
            matches!(
                f.kind,
                FormatKind::Assembly | FormatKind::Drawing | FormatKind::Model
            ) || matches!(
                f.id,
                "step" | "jt" | "plmxml" | "kicad-project" | "kicad-schematic"
            )
        }
        None => false,
    }
}

/// Whether a file name ends with the extension of a file an assembly or a
/// drawing can reference (parts, assemblies, drawings, models, exchange,
/// mesh, visualisation and electronics formats – not simulation models or
/// results, documents, templates
/// or archives).
fn is_referencable(name: &str) -> bool {
    cadformat::detect(name, &[]).is_some_and(|f| {
        !matches!(
            f.kind,
            FormatKind::Document
                | FormatKind::Other
                | FormatKind::Simulation
                | FormatKind::Result
                | FormatKind::Manufacturing
        )
    })
}

/// Lower-case extensions of all referencable formats, longest first.
fn extensions() -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = cadformat::FORMATS
        .iter()
        .flat_map(|f| f.extensions.iter().copied())
        .filter(|e| is_referencable(&format!("x.{e}")))
        .collect();
    exts.sort_by_key(|e| std::cmp::Reverse(e.len()));
    exts.dedup();
    exts
}

/// Characters that end a file name when walking back from its extension.
fn is_name_boundary(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\\' | '/'
                | ':'
                | '"'
                | '\''
                | '<'
                | '>'
                | '|'
                | '*'
                | '?'
                | '='
                | ','
                | ';'
                | '['
                | ']'
                | '{'
                | '}'
        )
}

/// File names with a referencable extension inside one text run.
fn names_in(run: &str, exts: &[&str], out: &mut Vec<String>) {
    let chars: Vec<char> = run.chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    if lower.len() != chars.len() {
        return; // exotic case mappings: not a file name we can handle
    }
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '.' {
            i += 1;
            continue;
        }
        // The longest extension that matches here and ends the name.
        let matched = exts.iter().find(|ext| {
            let end = i + 1 + ext.chars().count();
            end <= lower.len()
                && lower[i + 1..end].iter().copied().eq(ext.chars())
                && chars
                    .get(end)
                    .is_none_or(|c| !(c.is_alphanumeric() || *c == '_'))
        });
        let Some(ext) = matched else {
            i += 1;
            continue;
        };
        let end = i + 1 + ext.chars().count();
        // Walk back to the start of the name (a single space may be part of it).
        let mut start = i;
        while start > 0 {
            let c = chars[start - 1];
            if is_name_boundary(c) || (c == ' ' && start >= 2 && chars[start - 2] == ' ') {
                break;
            }
            start -= 1;
        }
        let name: String = chars[start..end].iter().collect();
        let name = name
            .trim()
            .trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        let stem_len = name.len().saturating_sub(ext.len() + 1);
        if stem_len > 0
            && name.len() <= MAX_NAME
            && name[..stem_len].chars().any(char::is_alphanumeric)
        {
            out.push(name.to_string());
        }
        i = end;
    }
}

/// Runs of printable text in single-byte encodings (ASCII, UTF-8, Latin-1).
fn byte_runs(content: &[u8], skip: Option<&std::ops::Range<usize>>, mut f: impl FnMut(&str)) {
    let mut start = None;
    for (i, &b) in content.iter().enumerate() {
        let printable =
            ((0x20..0x7f).contains(&b) || b >= 0x80) && !skip.is_some_and(|s| s.contains(&i));
        match (printable, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s >= 5 {
                    f(&String::from_utf8_lossy(&content[s..i]));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        if content.len() - s >= 5 {
            f(&String::from_utf8_lossy(&content[s..]));
        }
    }
}

/// Runs of printable UTF-16LE text (Latin-1 and Latin Extended-A), at both
/// byte alignments.
fn utf16_runs(content: &[u8], skip: Option<&std::ops::Range<usize>>, mut f: impl FnMut(&str)) {
    for align in 0..2 {
        let mut run = String::new();
        let mut i = align;
        while i + 1 < content.len() {
            let unit = u16::from_le_bytes([content[i], content[i + 1]]);
            let printable = ((0x20..0x7f).contains(&unit) || (0xa0..0x180).contains(&unit))
                && !skip.is_some_and(|s| s.contains(&i) || s.contains(&(i + 1)));
            if printable {
                run.push(char::from_u32(u32::from(unit)).unwrap_or('?'));
            } else {
                if run.chars().count() >= 5 {
                    f(&run);
                }
                run.clear();
            }
            i += 2;
        }
        if run.chars().count() >= 5 {
            f(&run);
        }
    }
}

/// Finds `needle` in `haystack` at or after `from`.
fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|i| from + i)
}

/// Byte range of the `FILE_NAME(...)` entity in the HEADER of an ISO
/// 10303-21 (STEP) file, if there is one. It carries the name the exporting
/// program chose, not a reference to another file.
fn step_file_name_entity(content: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = content
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(content.len());
    if !content[start..].starts_with(b"ISO-10303-21;") {
        return None;
    }
    let header = find_bytes(content, start, b"HEADER;")?;
    let header_end = find_bytes(content, header + b"HEADER;".len(), b"ENDSEC;")?;
    let name = find_bytes(content, header + b"HEADER;".len(), b"FILE_NAME")?;
    if name >= header_end {
        return None;
    }
    let open = name + content[name..].iter().position(|&b| b == b'(')?;
    // Quoted strings may contain parentheses; `''` is an escaped quote.
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = open;
    while i < content.len() {
        match content[i] {
            b'\'' if in_string && content.get(i + 1) == Some(&b'\'') => i += 1,
            b'\'' => in_string = !in_string,
            b'(' if !in_string => depth += 1,
            b')' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(name..i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The CAD file names referenced in `content`, without `own_name`; each name
/// once (case-insensitive), in the order found.
pub fn find(content: &[u8], own_name: &str) -> Vec<String> {
    // NX parts: only the reference streams, never the history in the rest
    // of the file ("Part renamed from …").
    if let Some(streams) = crate::nxpart::reference_streams_in(content) {
        return from_nx_streams(&streams, own_name);
    }
    finish(raw_names(content), own_name)
}

/// All names with a referencable extension, in the order found.
fn raw_names(content: &[u8]) -> Vec<String> {
    let exts = extensions();
    // The export name in the STEP header is not a reference to another file.
    let skip = step_file_name_entity(content);
    let mut found = Vec::new();
    byte_runs(content, skip.as_ref(), |run| {
        names_in(run, &exts, &mut found)
    });
    utf16_runs(content, skip.as_ref(), |run| {
        names_in(run, &exts, &mut found)
    });
    found
}

/// Normalised, without the file itself, each name once.
fn finish(found: Vec<String>, own_name: &str) -> Vec<String> {
    let own = cadformat::normalize_name(own_name).to_lowercase();
    let mut seen = HashSet::new();
    found
        .into_iter()
        .map(|n| cadformat::normalize_name(&n).to_string())
        .filter(|n| n.to_lowercase() != own && is_referencable(n))
        .filter(|n| seen.insert(n.to_lowercase()))
        .take(MAX_REFERENCES)
        .collect()
}

/// Each reference stream of an NX part starts with the part itself – under
/// the name it was saved with, which differs from the file name after a
/// rename – followed by its components.
fn from_nx_streams(streams: &[Vec<u8>], own_name: &str) -> Vec<String> {
    let names = streams
        .iter()
        .flat_map(|s| raw_names(s).into_iter().skip(1))
        .collect();
    finish(names, own_name)
}

/// Reads a stored file and finds its references: NX parts through their
/// stream directory (with seeks, so large parts are read completely),
/// everything else from its first [`MAX_SCAN`] bytes.
pub async fn scan_file(path: &Path, own_name: &str) -> Vec<String> {
    let path = path.to_path_buf();
    let own = own_name.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
        let mut file = std::io::BufReader::new(std::fs::File::open(&path)?);
        if let Some(streams) = crate::nxpart::reference_streams(&mut file, MAX_SCAN)? {
            return Ok(from_nx_streams(&streams, &own));
        }
        let mut content = Vec::new();
        file.seek(std::io::SeekFrom::Start(0))?;
        file.take(MAX_SCAN).read_to_end(&mut content)?;
        Ok(find(&content, &own))
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn which_formats_are_scanned() {
        for id in [
            "solidworks-assembly",
            "solidworks-drawing",
            "inventor-assembly",
            "catia-product",
            "creo-assembly",
            "solidedge-assembly",
            "nx-part",
            "fusion-distributed",
            "dwg",
            "step",
            "jt",
            "kicad-project",
        ] {
            assert!(scans(id), "{id}");
        }
        for id in [
            "solidworks-part",
            "stl",
            "pdf",
            "parasolid-text",
            "",
            "nope",
        ] {
            assert!(!scans(id), "{id}");
        }
    }

    #[test]
    fn windows_paths_in_utf16() {
        // SolidWorks / Inventor style: UTF-16LE paths between binary data.
        let mut content = vec![0x01, 0x00, 0x7f, 0x13];
        content.extend(utf16("C:\\Users\\erika\\Rover\\Motor mount.SLDPRT"));
        content.extend([0x00, 0x00, 0x05, 0x00]);
        content.extend(utf16("D:\\Lib\\ISO 4762 M6 x 20.ipt"));
        content.extend([0xff, 0xfe, 0x00, 0x00]);
        content.extend(utf16("Halterung_für_Düse.SLDPRT"));
        content.extend([0x00, 0x00]);
        content.extend(utf16("Rover.SLDASM")); // the file itself
        let refs = find(&content, "rover.sldasm");
        assert_eq!(
            refs,
            vec![
                "Motor mount.SLDPRT",
                "ISO 4762 M6 x 20.ipt",
                "Halterung_für_Düse.SLDPRT"
            ]
        );
    }

    #[test]
    fn odd_alignment_and_ascii() {
        let mut content = vec![0x00];
        content.extend(utf16("bracket.CATPart"));
        content.extend([0x00, 0x00, 0x09]);
        content.extend(b"#UGC:2 ASSEMBLY\x00\x03BRACKET.PRT\x00\x02FRAME_SIDE.PRT.3\x00");
        let refs = find(&content, "stage.asm");
        assert_eq!(
            refs,
            vec!["BRACKET.PRT", "FRAME_SIDE.PRT", "bracket.CATPart"]
        );
    }

    #[test]
    fn text_formats() {
        let step = b"ISO-10303-21;\nDATA;\n#5=DOCUMENT_FILE('nozzle.stp','',$,#6,'','');\n\
            #7=APPLIED_EXTERNAL_IDENTIFICATION_ASSIGNMENT('injector plate.STEP',#8,(#9));\n\
            #10=PRODUCT('NOZ','Nozzle 1.5 mm','',(#2));\nENDSEC;";
        assert_eq!(
            find(step, "engine.step"),
            vec!["nozzle.stp", "injector plate.STEP"]
        );
        let kicad = b"(kicad_sch (sheet (property \"Sheetfile\" \"power.kicad_sch\") \
            (property \"Sheetfile\" \"sensors/imu.kicad_sch\")))";
        assert_eq!(
            find(kicad, "main.kicad_sch"),
            vec!["power.kicad_sch", "imu.kicad_sch"]
        );
    }

    #[test]
    fn step_header_name_is_not_a_reference() {
        let step = b"ISO-10303-21;\nHEADER;\n\
            FILE_DESCRIPTION((''),'2;1');\n\
            FILE_NAME('export name.stp','2021-01-01',(''),(''),'','','');\n\
            FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n\
            #5=DOCUMENT_FILE('export name.stp','',$,#6,'','');\n\
            #7=DOCUMENT_FILE('nozzle.stp','',$,#6,'','');\nENDSEC;\nEND-ISO-10303-21;\n";
        assert_eq!(
            find(step, "stored.stp"),
            vec!["export name.stp", "nozzle.stp"]
        );
    }

    #[test]
    fn no_false_friends() {
        // Version numbers, documents, templates, extensions inside words.
        let content = b"Version 2.5 released\x00see manual.pdf\x00Part.prtdot\x00\
            stl.stlfile\x00.SLDPRT\x00file.x_tx\x00  -  .stp\x00";
        assert!(
            find(content, "a.sldasm").is_empty(),
            "{:?}",
            find(content, "a.sldasm")
        );
        assert!(find(b"", "a.sldasm").is_empty());
        // Each name once, regardless of case; Creo iterations collapse.
        let content =
            b"\x00bolt.sldprt\x00BOLT.SLDPRT\x00Bolt.sldprt\x00frame.prt.2\x00frame.prt.3\x00";
        assert_eq!(find(content, "x.sldasm"), vec!["bolt.sldprt", "frame.prt"]);
    }

    #[test]
    fn many_references_are_capped() {
        let mut content = Vec::new();
        for i in 0..800 {
            content.extend(format!("\x00part-{i}.ipt").as_bytes());
        }
        assert_eq!(find(&content, "big.iam").len(), MAX_REFERENCES);
    }
}
