//! Text extraction for the full-text search over file contents.
//!
//! * STEP (`.step`, `.stp`, `.p21`) and IGES (`.iges`, `.igs`): the quoted strings of
//!   the file – product names, descriptions, authors, organisation, layer and
//!   entity names – without the numeric geometry.
//! * Plain text formats (txt, md, csv, json, xml, yaml, log, Teamcenter PLM XML,
//!   OpenSCAD, motor thrust curves, …): the text itself, line by line, with its
//!   decimal numbers – a text or CSV report reads like a PDF one (charset: byte
//!   order mark, UTF-8, else Windows-1252).
//! * Geometry as text (ASCII STL, OBJ, KiCad boards and symbols): the words
//!   without the decimal numbers (coordinates are noise in a search).
//! * PDF: the text layer, read in an isolated process (see `pdftext`); scanned
//!   PDFs have none.
//! * HTML (`.html`, `.htm`, the reports of FEA/CFD tools): the readable text with
//!   table rows as lines, read in the same isolated process (see `htmltext`).
//! * Everything else (binary CAD, images): nothing (not indexed yet).
//!
//! At most `MAX_READ` bytes are read and `MAX_TEXT` characters are kept.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::path::Path;

use tokio::io::AsyncReadExt;

/// Bytes read from the beginning of a file.
const MAX_READ: u64 = 4 * 1024 * 1024;
/// Characters kept for indexing.
const MAX_TEXT: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    QuotedStrings,
    /// The text as it is (lines and decimal numbers kept).
    PlainText,
    /// Text full of coordinates: words only, decimal numbers dropped.
    Geometry,
    /// PDF: read in an isolated process (`pdftext`), only by [`extract_file`].
    Pdf,
    /// HTML: read in an isolated process (`htmltext`), only by [`extract_file`].
    Html,
    None,
}

fn kind_of(filename: &str) -> Kind {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => Kind::Pdf,
        "html" | "htm" => Kind::Html,
        "stl" | "obj" | "kicad_sch" | "kicad_pcb" | "kicad_sym" | "kicad_mod" => Kind::Geometry,
        "step" | "stp" | "p21" | "iges" | "igs" => Kind::QuotedStrings,
        "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "log" | "kicad_pro" | "net" | "sch" | "plmxml" | "scad" | "eng" | "rse" | "tex"
        | "rst" | "py" | "rs" | "c" | "h" | "cpp" | "hpp" | "m" => Kind::PlainText,
        _ => Kind::None,
    }
}

/// Whether the content of such a file is indexed for the full-text search.
pub fn indexes(filename: &str) -> bool {
    kind_of(filename) != Kind::None
}

/// Extracts searchable text from raw file content.
pub fn extract(filename: &str, content: &[u8]) -> String {
    let text = match kind_of(filename) {
        Kind::None | Kind::Pdf | Kind::Html => return String::new(),
        // A text extension with binary content (binary STL): nothing to index.
        Kind::PlainText | Kind::Geometry
            if content.contains(&0) && encoding_rs::Encoding::for_bom(content).is_none() =>
        {
            return String::new()
        }
        Kind::PlainText => return plain_text(&crate::htmltext::decode(content, false)),
        // Drop decimal numbers (e.g. the facet coordinates of ASCII STL files);
        // integers like "912" in "DIN 912" stay searchable.
        Kind::Geometry => String::from_utf8_lossy(content)
            .split_whitespace()
            .filter(|w| !is_decimal_number(w))
            .collect::<Vec<_>>()
            .join(" "),
        // Quoted STEP/IGES strings are names – keep them as they are ("1.4404").
        Kind::QuotedStrings => quoted_strings(&String::from_utf8_lossy(content)),
    };
    // PostgreSQL TEXT cannot hold NUL; other control characters are noise.
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT)
        .collect()
}

/// A text file as text: lines kept (rows of a table or report stay rows),
/// control characters except tabs become spaces (PostgreSQL TEXT cannot hold
/// NUL), trailing blanks and runs of blank lines go, at most [`MAX_TEXT`]
/// characters.
fn plain_text(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut blank = 0;
    for line in text.lines() {
        let line: String = line
            .chars()
            .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
            .collect();
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
        chars += line.chars().count() + 1;
        if chars > MAX_TEXT {
            break;
        }
    }
    out.trim_matches('\n').chars().take(MAX_TEXT).collect()
}

/// `1.5`, `-0.25`, `1.000000e+00` – but not `912` or `M6`.
fn is_decimal_number(word: &str) -> bool {
    word.contains('.')
        && word.chars().any(|c| c.is_ascii_digit())
        && word
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
}

/// Reads (the beginning of) a stored file and extracts its text.
pub async fn extract_file(path: &Path, filename: &str) -> String {
    if kind_of(filename) == Kind::None {
        return String::new();
    }
    let Ok(file) = tokio::fs::File::open(path).await else {
        return String::new();
    };
    match kind_of(filename) {
        Kind::Pdf => return crate::pdftext::extract_file(file).await,
        Kind::Html => return crate::htmltext::extract_file(file).await,
        _ => {}
    }
    let mut content = Vec::new();
    if file.take(MAX_READ).read_to_end(&mut content).await.is_err() {
        return String::new();
    }
    // CPU work on up to MAX_READ bytes: off the async worker threads, so
    // other requests are not held up by a large upload
    let filename = filename.to_string();
    tokio::task::spawn_blocking(move || extract(&filename, &content))
        .await
        .unwrap_or_default()
}

/// The contents of all `'…'` strings (STEP uses `''` for a quote inside a string).
fn quoted_strings(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut current = String::new();
        loop {
            match chars.next() {
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    current.push('\'');
                }
                Some('\'') | None => break,
                Some(ch) => current.push(ch),
            }
        }
        let current = current.trim();
        if !current.is_empty() && current.chars().any(char::is_alphabetic) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(current);
        }
    }
    out
}

/// Builds a prefix `tsquery` (`flan:* & m6:*`) from free text, or `None` if
/// the input contains no searchable word.
pub fn prefix_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(12)
        .map(|t| format!("{}:*", t.to_lowercase()))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" & "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('Engine mount bracket'),'2;1');\n\
        FILE_NAME('nx-eng-001.step','2026-11-03T10:00:00',('Erika'),('Example Lab'),'','','');\nENDSEC;\nDATA;\n\
        #10=PRODUCT('NX-ENG-001','Engine mount','',(#20));\n#11=CARTESIAN_POINT('',(0.,1.5,2.));\n\
        #12=PRODUCT('M6','Bolt M6x20 DIN 912','It''s steel',(#20));\nENDSEC;\nEND-ISO-10303-21;";

    #[test]
    fn step_names_are_extracted() {
        let text = extract("Mount.STEP", STEP.as_bytes());
        assert!(text.contains("Engine mount bracket"));
        assert!(text.contains("Bolt M6x20 DIN 912"));
        assert!(text.contains("It's steel"), "escaped quotes");
        assert!(text.contains("Example Lab"));
        assert!(!text.contains("1.5"), "geometry is not indexed");
    }

    #[test]
    fn text_and_binary_files() {
        assert_eq!(
            extract("notes.md", b"Thrust 2 kN, DIN 912\r\nburn time 4.2 s"),
            "Thrust 2 kN, DIN 912\nburn time 4.2 s",
            "text keeps its lines and decimal numbers"
        );
        let stl = b"solid hub\n facet normal 0.0 0.0 1.0\n  outer loop\n   vertex 1.5e+01 -2.25 0.0\n  endloop\n endfacet\nendsolid hub";
        assert_eq!(
            extract("hub.stl", stl),
            "solid hub facet normal outer loop vertex endloop endfacet endsolid hub"
        );
        assert!(
            is_decimal_number("1.000000e+00")
                && !is_decimal_number("M6")
                && !is_decimal_number("912")
        );
        let mut binary_stl = b"solid exported by CAD".to_vec();
        binary_stl.resize(84, 0);
        binary_stl.extend_from_slice(&[0x00, 0x00, 0x80, 0x3f, 0x01, 0x00]);
        assert_eq!(extract("hub.stl", &binary_stl), "", "binary STL");
        assert_eq!(
            extract("part.step", b"#1=PRODUCT('Hub\0\x07 v2','',(#2));"),
            "Hub   v2",
            "no NUL or control characters reach the database"
        );
        assert_eq!(extract("drawing.pdf", b"%PDF-1.7 binary"), "");
        assert_eq!(extract("no_extension", b"text"), "");
    }

    #[test]
    fn which_files_are_indexed() {
        for name in [
            "a.STEP",
            "a.stp",
            "a.p21",
            "a.IGES",
            "a.igs",
            "notes.TXT",
            "bom.csv",
            "x.kicad_sch",
            "motor.eng",
            "part.scad",
            "data.plmxml",
            "report.pdf",
        ] {
            assert!(indexes(name), "{name}");
        }
        for name in ["a.sldprt", "a.png", "README", "a.step.gz"] {
            assert!(!indexes(name), "{name}");
        }
    }

    #[test]
    fn iges_and_quoted_string_rules() {
        // Strings without letters (numbers, blanks) are geometry, not names.
        assert_eq!(
            extract("a.igs", b"'1.5' '  ' 'Flange DN50' '1.4404 steel' '42'"),
            "Flange DN50 1.4404 steel"
        );
        // An unterminated string at the end is still taken.
        assert_eq!(extract("a.stp", b"#1=PRODUCT('Hub"), "Hub");
        // Strings are trimmed.
        assert_eq!(extract("a.stp", b"('  Bolt  ')"), "Bolt");
    }

    #[test]
    fn plain_text_is_the_real_text() {
        assert_eq!(
            extract("a.csv", b"M6;912;-0.5;1e5;3.;v1.2"),
            "M6;912;-0.5;1e5;3.;v1.2"
        );
        assert_eq!(
            extract("a.txt", b"len 12.5 mm -3.0 +1.0e-3 ISO 4762"),
            "len 12.5 mm -3.0 +1.0e-3 ISO 4762",
            "a report keeps its numbers"
        );
        assert_eq!(
            extract("summary.tsv", b"case\tdp_pa  \n\n\n\nA\t388.25\x07\n"),
            "case\tdp_pa\n\nA\t388.25",
            "tabs stay, blank runs collapse, control characters go"
        );
        assert_eq!(extract("a.txt", b""), "");
        assert_eq!(
            extract("a.txt", "Düse \u{fffd}".as_bytes()),
            "Düse \u{fffd}",
            "non-ASCII text is kept"
        );
        assert_eq!(extract("a.log", b"85\xB0C"), "85°C", "Windows-1252");
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "p 1.5".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(extract("a.txt", &utf16), "p 1.5", "UTF-16 with BOM");
        assert_eq!(extract("a.txt", b"a\0b"), "", "binary content");
    }

    #[test]
    fn geometry_text_drops_decimal_numbers() {
        assert_eq!(
            extract("board.kicad_pcb", b"(at 12.5 -3.0) (layer F.Cu) DIN 912"),
            "(at -3.0) (layer F.Cu) DIN 912"
        );
        assert!(!is_decimal_number("1.2.3a") && is_decimal_number("1.2.3"));
        assert!(!is_decimal_number("-") && !is_decimal_number("."));
    }

    #[test]
    fn html_is_not_read_in_process() {
        assert_eq!(extract("report.html", b"<p>1.5</p>"), "");
        assert!(indexes("report.HTM") && indexes("r.html"));
    }

    #[test]
    fn extracted_text_is_bounded() {
        let big = "word ".repeat(MAX_TEXT);
        assert_eq!(extract("a.txt", big.as_bytes()).chars().count(), MAX_TEXT);
        let lines = "word\n".repeat(MAX_TEXT);
        assert_eq!(extract("a.txt", lines.as_bytes()).chars().count(), MAX_TEXT);
        assert_eq!(extract("a.stl", big.as_bytes()).chars().count(), MAX_TEXT);
    }

    #[tokio::test]
    async fn stored_files_are_read() {
        let path = std::env::temp_dir().join(format!("teamcraft-text-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, STEP).unwrap();
        assert!(extract_file(&path, "m.step").await.contains("Engine mount"));
        assert_eq!(
            extract_file(&path, "m.bin").await,
            "",
            "not indexed by name"
        );
        std::fs::remove_file(&path).ok();
        assert_eq!(extract_file(&path, "m.step").await, "", "missing file");
    }

    #[test]
    fn prefix_queries_are_bounded() {
        let many = (0..20)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let q = prefix_query(&many).unwrap();
        assert_eq!(q.matches(":*").count(), 12, "at most twelve terms");
        assert_eq!(
            prefix_query("A:B"),
            Some("a:* & b:*".into()),
            "no tsquery syntax passes"
        );
        assert_eq!(prefix_query(""), None);
    }

    #[test]
    fn prefix_queries() {
        assert_eq!(prefix_query("Flan M6"), Some("flan:* & m6:*".into()));
        assert_eq!(
            prefix_query("nx-eng-001"),
            Some("nx:* & eng:* & 001:*".into())
        );
        assert_eq!(prefix_query("  ' & | !  "), None);
        assert_eq!(prefix_query("Düse"), Some("düse:*".into()));
    }
}
