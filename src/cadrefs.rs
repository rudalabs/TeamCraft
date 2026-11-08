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
//! Two containers are read by their structure instead: NX parts
//! (`nxpart`: only the reference streams) and SOLIDWORKS 2015+ files
//! (`swpart`: the component tree of an assembly, otherwise the document
//! manager's section – both inflated from their compressed sections). The
//! rest of such a file (history, old names) is never scanned.
//!
//! For every other file this is a heuristic: a name that a CAD program keeps
//! in its file after the component was removed is still found, and
//! compressed sections of unknown containers are not read. The revision page
//! therefore shows the names as "found in the file".
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

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
/// or archives). Fluent meshes (`.msh.h5`, `.msh.gz`) are solver input like
/// the plain `.msh`, which the name alone does not classify: not collected.
fn is_referencable(name: &str) -> bool {
    cadformat::detect(name, &[]).is_some_and(|f| {
        !f.id.starts_with("fluent-")
            && !matches!(
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
    while let Some(&ch) = chars.get(i) {
        if ch != '.' {
            i += 1;
            continue;
        }
        // The longest extension that matches here and ends the name.
        let matched = exts.iter().find(|ext| {
            let end = i + 1 + ext.chars().count();
            lower
                .get(i + 1..end)
                .is_some_and(|l| l.iter().copied().eq(ext.chars()))
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
        while let Some(&c) = start.checked_sub(1).and_then(|s| chars.get(s)) {
            let double_space = c == ' ' && start >= 2 && chars.get(start - 2) == Some(&' ');
            if is_name_boundary(c) || double_space {
                break;
            }
            start -= 1;
        }
        let span: String = chars.get(start..end).unwrap_or_default().iter().collect();
        let name = span
            .trim()
            .trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        // The stem in characters: the extension matched in lower case may
        // have more bytes than `ext` (the Kelvin sign is three bytes).
        let stem = name.chars().count().saturating_sub(ext.chars().count() + 1);
        // Binary data that happens to end in `.jt` or the like: bytes that
        // are no text (U+FFFD after the lossy decoding) inside the name, or
        // right before a name without a letter (`�4.JT`). A real name after
        // a length byte (`�Bracket.SLDPRT`) stays.
        let binary = name.contains(char::REPLACEMENT_CHARACTER)
            || (span.contains(char::REPLACEMENT_CHARACTER)
                && !name.chars().take(stem).any(char::is_alphabetic));
        if stem > 0
            && !binary
            && name.len() <= MAX_NAME
            && name.chars().take(stem).any(char::is_alphanumeric)
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
                    f(&String::from_utf8_lossy(
                        content.get(s..i).unwrap_or_default(),
                    ));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        if content.len() - s >= 5 {
            f(&String::from_utf8_lossy(
                content.get(s..).unwrap_or_default(),
            ));
        }
    }
}

/// Runs of printable UTF-16LE text (Latin-1 and Latin Extended-A), at both
/// byte alignments.
fn utf16_runs(content: &[u8], skip: Option<&std::ops::Range<usize>>, mut f: impl FnMut(&str)) {
    for align in 0..2 {
        let mut run = String::new();
        let mut i = align;
        while let Some(&[lo, hi]) = content.get(i..i + 2) {
            let unit = u16::from_le_bytes([lo, hi]);
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
    if !content.get(start..)?.starts_with(b"ISO-10303-21;") {
        return None;
    }
    let header = find_bytes(content, start, b"HEADER;")?;
    let header_end = find_bytes(content, header + b"HEADER;".len(), b"ENDSEC;")?;
    let name = find_bytes(content, header + b"HEADER;".len(), b"FILE_NAME")?;
    if name >= header_end {
        return None;
    }
    let open = name + content.get(name..)?.iter().position(|&b| b == b'(')?;
    // Quoted strings may contain parentheses; `''` is an escaped quote.
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = open;
    while let Some(&b) = content.get(i) {
        match b {
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

/// Bytes of a STEP file read for its export name: the HEADER comes first.
pub const EXPORT_NAME_SCAN: u64 = 64 * 1024;

/// The name a STEP file was exported under – the first string of the
/// `FILE_NAME(...)` entity in its HEADER, reduced to the file name – when it
/// is the name of a referencable file and differs from `own_name`. Other
/// files may reference the file under that name (an assembly exported
/// together with its parts, a part renamed after the export); it is a
/// second name of the file itself and never a reference to another file.
pub fn step_export_name(content: &[u8], own_name: &str) -> Option<String> {
    let entity = step_file_name_entity(content)?;
    let text = content.get(entity)?;
    let open = text.iter().position(|&b| b == b'(')?;
    let rest = text.get(open + 1..)?;
    let quote = rest.iter().position(|b| !b.is_ascii_whitespace())?;
    if rest.get(quote) != Some(&b'\'') {
        return None;
    }
    // the string up to the closing quote; `''` is an escaped quote
    let mut value = Vec::new();
    let mut i = quote + 1;
    loop {
        match (rest.get(i), rest.get(i + 1)) {
            (Some(b'\''), Some(b'\'')) => {
                value.push(b'\'');
                i += 2;
            }
            (Some(b'\''), _) => break,
            (Some(&b), _) => {
                value.push(b);
                i += 1;
            }
            (None, _) => return None,
        }
    }
    let value = String::from_utf8_lossy(&value);
    let name = value.rsplit(['\\', '/']).next().unwrap_or_default().trim();
    let name = cadformat::normalize_name(name);
    let fits = !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.contains(char::REPLACEMENT_CHARACTER)
        && is_referencable(name)
        && name.to_lowercase() != cadformat::normalize_name(own_name).to_lowercase();
    fits.then(|| name.to_string())
}

/// [`step_export_name`] of a stored file (its first [`EXPORT_NAME_SCAN`]
/// bytes); `None` if it cannot be read.
pub async fn scan_export_name(path: &Path, own_name: &str) -> Option<String> {
    let path = path.to_path_buf();
    let own = own_name.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<Option<String>> {
        let mut head = Vec::new();
        std::fs::File::open(&path)?
            .take(EXPORT_NAME_SCAN)
            .read_to_end(&mut head)?;
        Ok(step_export_name(&head, &own))
    })
    .await
    .ok()
    .and_then(Result::ok)
    .flatten()
}

/// The CAD file names referenced in `content`, without `own_name`; each name
/// once (case-insensitive), in the order found.
pub fn find(content: &[u8], own_name: &str) -> Vec<String> {
    // NX parts: only the reference streams, never the history in the rest
    // of the file ("Part renamed from …").
    if let Some(streams) = crate::nxpart::reference_streams_in(content) {
        return from_nx_streams(&streams, own_name);
    }
    // SOLIDWORKS 2015+: only the inflated reference sections.
    if let Ok(Some(data)) = crate::swpart::reference_data(std::io::Cursor::new(content), MAX_SCAN) {
        return from_sw_sections(data, own_name);
    }
    finish(raw_names(content), own_name)
}

/// SOLIDWORKS 2015+ files: the component files of an assembly's component
/// tree; without one (drawings) the names in the document manager's
/// section; nothing if neither inflates.
fn from_sw_sections(data: crate::swpart::ReferenceData, own_name: &str) -> Vec<String> {
    let names = match (data.components, data.content_manager) {
        (Some(paths), _) => paths
            .iter()
            .map(|p| p.rsplit(['\\', '/']).next().unwrap_or(p).trim().to_string())
            .collect(),
        (None, Some(section)) => raw_names(&section),
        (None, None) => Vec::new(),
    };
    finish(names, own_name)
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
/// stream directory and SOLIDWORKS 2015+ files through their sections (with
/// seeks, so large files are read completely), everything else from its
/// first [`MAX_SCAN`] bytes.
pub async fn scan_file(path: &Path, own_name: &str) -> Vec<String> {
    let path = path.to_path_buf();
    let own = own_name.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
        let mut file = std::io::BufReader::new(std::fs::File::open(&path)?);
        if let Some(streams) = crate::nxpart::reference_streams(&mut file, MAX_SCAN)? {
            return Ok(from_nx_streams(&streams, &own));
        }
        if let Some(data) = crate::swpart::reference_data(&mut file, MAX_SCAN)? {
            return Ok(from_sw_sections(data, &own));
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
    fn fluent_meshes_are_not_referencable() {
        for name in ["pipe.msh.gz", "pipe.msh.h5", "pipe.msh", "pipe.cas.h5"] {
            assert!(!is_referencable(name), "{name}");
        }
        assert!(is_referencable("pipe.stl"));
        assert!(!extensions().contains(&"msh.gz"));
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

    #[test]
    fn paths_are_reduced_to_the_file_name() {
        // POSIX, Windows, UNC and URL-like paths: only the last component.
        let content = b"\x00/home/erika/rover/wheel.sldprt\x00\
            \\\\server\\share\\cad\\hub.ipt\x00file:///C:/lib/axle.step\x00";
        assert_eq!(
            find(content, "rover.sldasm"),
            vec!["wheel.sldprt", "hub.ipt", "axle.step"]
        );
    }

    #[test]
    fn the_file_itself_is_never_a_reference() {
        // Own name in another case, with a path and as a Creo iteration.
        let content = b"\x00C:\\work\\FRAME.ASM.4\x00frame.asm\x00bolt.prt.2\x00";
        assert_eq!(find(content, "frame.asm.7"), vec!["bolt.prt"]);
    }

    #[test]
    fn non_cad_files_are_not_references() {
        // Documents, simulation decks and results, manufacturing data,
        // templates: never component files of an assembly.
        let content = b"\x00manual.pdf\x00job.inp\x00result.frd\x00mesh.vtu\x00\
            nc.mfg\x00sheet.slddrt\x00project.ipj\x00real part.par\x00";
        assert_eq!(find(content, "a.asm"), vec!["real part.par"]);
    }

    #[test]
    fn names_end_at_separators_and_double_spaces() {
        // A single space is part of a name, two spaces end it; quotes,
        // `=` and `;` are separators.
        let content =
            b"text  Motor mount.ipt\x00name=\"Shaft 20.ipt\";ref='nut.ipt'\x00[plate.ipt]";
        assert_eq!(
            find(content, "x.iam"),
            vec!["Motor mount.ipt", "Shaft 20.ipt", "nut.ipt", "plate.ipt"]
        );
    }

    #[test]
    fn the_longest_extension_wins() {
        // `.kicad_sch` is not cut to some shorter extension, `.catproduct`
        // not to `.cat…`; a name followed by letters is no match at all.
        let content = b"\x00power.kicad_sch\x00Top.CATProduct\x00shaft.stpx\x00";
        assert_eq!(
            find(content, "main.kicad_pro"),
            vec!["power.kicad_sch", "Top.CATProduct"]
        );
    }

    #[test]
    fn names_without_a_stem_or_too_long_are_dropped() {
        let long = format!("\x00{}.ipt\x00", "x".repeat(MAX_NAME));
        assert!(
            find(long.as_bytes(), "a.iam").is_empty(),
            "longer than MAX_NAME"
        );
        let fits = format!("\x00{}.ipt\x00", "x".repeat(MAX_NAME - 4));
        assert_eq!(find(fits.as_bytes(), "a.iam").len(), 1);
        // only punctuation before the extension
        assert!(find(b"\x00___.ipt\x00--.stp\x00", "a.iam").is_empty());
    }

    #[test]
    fn short_runs_are_ignored() {
        // Printable runs shorter than five characters are binary noise.
        assert!(find(b"\x00a.st\x00", "x.asm").is_empty());
        assert_eq!(find(b"\x00ab.stp\x00", "x.asm"), vec!["ab.stp"]);
    }

    #[test]
    fn step_header_name_is_only_skipped_inside_the_header() {
        // A FILE_NAME-like string in the DATA section is not the header entity.
        let step = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nENDSEC;\nDATA;\n\
            #1=FILE_NAME('wheel.stp');\nENDSEC;\n";
        assert_eq!(find(step, "rover.stp"), vec!["wheel.stp"]);
        // Parentheses and escaped quotes inside the header name are handled.
        let step = b"ISO-10303-21;\nHEADER;\n\
            FILE_NAME('it''s (v2).stp','2026',('a(b)'),(''),'','','');\nENDSEC;\nDATA;\n\
            #5=DOCUMENT_FILE('rim.stp','',$,#6,'','');\nENDSEC;\n";
        assert_eq!(find(step, "rover.stp"), vec!["rim.stp"]);
        // Without the ISO header the same text is an ordinary string.
        let not_step = b"FILE_NAME('export.stp','2026');";
        assert_eq!(find(not_step, "x.stp"), vec!["export.stp"]);
    }

    #[test]
    fn solidworks_sections_reduce_component_paths() {
        let data = crate::swpart::ReferenceData {
            components: Some(vec![
                "C:\\p\\Bolt.SLDPRT".into(),
                "D:/lib/Nut.SLDPRT".into(),
                "C:\\p\\bolt.sldprt".into(),
                "C:\\p\\Top.SLDASM".into(),
            ]),
            content_manager: Some(b"\x00ignored.SLDPRT\x00".to_vec()),
        };
        assert_eq!(
            from_sw_sections(data, "top.sldasm"),
            vec!["Bolt.SLDPRT", "Nut.SLDPRT"],
            "the component tree wins over the document manager"
        );
        let drawing = crate::swpart::ReferenceData {
            components: None,
            content_manager: Some(b"\x00C:\\p\\Bracket.SLDPRT\x00".to_vec()),
        };
        assert_eq!(
            from_sw_sections(drawing, "b.slddrw"),
            vec!["Bracket.SLDPRT"]
        );
        let empty = crate::swpart::ReferenceData {
            components: None,
            content_manager: None,
        };
        assert!(from_sw_sections(empty, "b.slddrw").is_empty());
    }

    #[test]
    fn nx_streams_skip_the_name_the_part_was_saved_with() {
        // Each stream starts with the part itself (here under an old name).
        let streams = vec![
            b"\x00old_name.prt\x00wheel.prt\x00hub.prt\x00".to_vec(),
            b"\x00old_name.prt\x00HUB.prt\x00axle.prt\x00".to_vec(),
        ];
        assert_eq!(
            from_nx_streams(&streams, "new_name.prt"),
            vec!["wheel.prt", "hub.prt", "axle.prt"]
        );
    }

    #[tokio::test]
    async fn scan_file_reads_stored_files() {
        let path = std::env::temp_dir().join(format!("teamcraft-cadrefs-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"\x00bracket.SLDPRT\x00frame.SLDASM\x00").unwrap();
        assert_eq!(
            scan_file(&path, "frame.sldasm").await,
            vec!["bracket.SLDPRT"]
        );
        std::fs::remove_file(&path).ok();
        assert!(
            scan_file(&path, "frame.sldasm").await.is_empty(),
            "missing file"
        );
    }

    /// Regression: the extension is compared in lower case, and the Kelvin
    /// sign `K` (U+212A, three bytes in UTF-8) lower-cases to an ASCII `k`.
    /// The name's stem was then cut at a byte position inside that sign – a
    /// panic on a file that holds such text.
    #[test]
    fn a_non_ascii_letter_in_the_extension_does_not_panic() {
        let content = "board a.\u{212A}icad_pcb and other text".as_bytes();
        let names = find(content, "x.kicad_pro");
        assert!(names.iter().all(|n| n.starts_with('a')), "{names:?}");
        // the same text as UTF-16 is not even read as a name (Latin only)
        let utf16: Vec<u8> = "a.\u{212A}icad_pcb "
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        find(&utf16, "x.kicad_pro");
    }

    fn step(header_name: &str) -> Vec<u8> {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
             FILE_NAME({header_name},'2026-11-06',(''),(''),'','','');\nENDSEC;\nDATA;\nENDSEC;\n"
        )
        .into_bytes()
    }

    #[test]
    fn the_step_export_name_is_a_second_name_of_the_file() {
        assert_eq!(
            step_export_name(&step("'bracket.stp'"), "bracket_v2.stp").as_deref(),
            Some("bracket.stp")
        );
        // a path is reduced to the file name, spaces around it are dropped
        assert_eq!(
            step_export_name(&step("  'C:\\Export\\Rover\\Wheel hub.STEP' "), "x.stp").as_deref(),
            Some("Wheel hub.STEP")
        );
        assert_eq!(
            step_export_name(&step("'/srv/out/it''s (v2).stp'"), "x.stp").as_deref(),
            Some("it's (v2).stp")
        );
        // a native name the exporting program kept is a name as well
        assert_eq!(
            step_export_name(&step("'Bracket.SLDPRT'"), "bracket.stp").as_deref(),
            Some("Bracket.SLDPRT")
        );
        // not a second name: the same name, no name, no file name, a
        // document, a name that is not text, a broken or missing entity
        for header in [
            "'BRACKET_V2.stp'",
            "''",
            "'   '",
            "'Part1'",
            "'C:\\Export\\'",
            "'notes.pdf'",
            "$",
            "'unterminated.stp",
        ] {
            assert_eq!(
                step_export_name(&step(header), "bracket_v2.stp"),
                None,
                "{header}"
            );
        }
        let latin1 = b"ISO-10303-21;\nHEADER;\nFILE_NAME('Halterung_f\xfcr.stp','x');\nENDSEC;\n";
        assert_eq!(step_export_name(latin1, "x.stp"), None);
        let no_step = b"FILE_NAME('bracket.stp','2026');";
        assert_eq!(step_export_name(no_step, "x.stp"), None);
        let in_data =
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('X'));\nENDSEC;\nDATA;\n#1=FILE_NAME('b.stp');\n";
        assert_eq!(step_export_name(in_data, "x.stp"), None);
        // never an outgoing reference of the file itself
        assert!(find(&step("'bracket.stp'"), "bracket_v2.stp").is_empty());
    }

    #[test]
    fn binary_noise_is_no_file_name() {
        // `.JT` after bytes that are no text (seen in a JT file's
        // compressed segments): no name
        let mut content = vec![0x01, 0x9f, b'4', b'.', b'J', b'T', b'\n'];
        // inside a name as well
        content.extend(b"\x02ab\x9fcd.stp\x00");
        assert!(
            find(&content, "x.jt").is_empty(),
            "{:?}",
            find(&content, "x.jt")
        );
        // a real name after a length byte stays, also a numeric one in text
        let mut content = vec![0x01, 0x8e];
        content.extend(b"Bracket.SLDPRT\x00\x004711.stp\x00");
        assert_eq!(find(&content, "x.sldasm"), ["Bracket.SLDPRT", "4711.stp"]);
    }

    /// The documented limits (found by cargo mutants: a behavioural test
    /// would have to scan more than 32 MiB).
    #[test]
    fn scan_limits() {
        assert_eq!(MAX_SCAN, 32 * 1024 * 1024);
        assert_eq!(EXPORT_NAME_SCAN, 64 * 1024);
    }
}
