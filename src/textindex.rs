//! Text extraction for the full-text search over file contents.
//!
//! * STEP (`.step`, `.stp`, `.p21`) and IGES (`.iges`, `.igs`): the quoted strings of
//!   the file – product names, descriptions, authors, organisation, layer and
//!   entity names – without the numeric geometry.
//! * Plain text formats (txt, md, csv, json, xml, yaml, log, Teamcenter PLM XML,
//!   OpenSCAD, motor thrust curves, …): the text itself, line by line, with its
//!   decimal numbers – a text or CSV report reads like a PDF one (charset: byte
//!   order mark, (mostly valid) UTF-8, else Windows-1252).
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
        | "rst" | "py" | "rs" | "c" | "h" | "cpp" | "hpp" | "m"
        // solver logs, report files, print output and scripts (Fluent,
        // STAR-CCM+, Nastran)
        | "out" | "trn" | "f06" | "f04" | "pch" | "jou" | "java" => Kind::PlainText,
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

/// Reads (the beginning of) a stored file and extracts its text. A large
/// plain-text file (solver log, print output) keeps its end as well: the
/// text of the beginning, the marker [`TAIL_MARK`] and the text of the last
/// `TAIL_READ` (64 KiB) bytes – where a solver writes its status and final
/// values. The end is only added when the beginning was text and the end is
/// text in the same encoding (no NUL, no UTF-16 byte order mark at the start).
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
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut content = Vec::new();
    if file.take(MAX_READ).read_to_end(&mut content).await.is_err() {
        return String::new();
    }
    let tail = if kind_of(filename) == Kind::PlainText && len > 2 * TAIL_READ {
        read_tail(path, TAIL_READ).await
    } else {
        Vec::new()
    };
    // CPU work on up to MAX_READ bytes: off the async worker threads, so
    // other requests are not held up by a large upload
    let filename = filename.to_string();
    tokio::task::spawn_blocking(move || {
        let text = extract(&filename, &content);
        let cut = len > MAX_READ || text.chars().count() >= MAX_TEXT;
        if cut && tail_usable(&content, &text, &tail) {
            with_tail(text, &tail)
        } else {
            text
        }
    })
    .await
    .unwrap_or_default()
}

/// Bytes read from the end of a large plain-text file.
const TAIL_READ: u64 = 64 * 1024;

/// Whether the end of a file may follow the text of its beginning: the
/// beginning gave text (a binary file gives none), the end holds no NUL
/// (binary data, zero padding, UTF-16), and the beginning has no byte order
/// mark other than UTF-8's – the end has no mark of its own and would be
/// decoded in another encoding.
fn tail_usable(head: &[u8], head_text: &str, tail: &[u8]) -> bool {
    let same_encoding = encoding_rs::Encoding::for_bom(head)
        .is_none_or(|(encoding, _)| encoding == encoding_rs::UTF_8);
    !tail.is_empty() && !head_text.is_empty() && !tail.contains(&0) && same_encoding
}

/// Between the beginning and the end of a file whose middle was left out.
pub const TAIL_MARK: &str = "\n[… middle of the file left out …]\n";

/// The text of a file's beginning followed by [`TAIL_MARK`] and the text of
/// its end (`tail` starts at a line), together at most [`MAX_TEXT`]
/// characters: the end keeps at most a quarter of them.
fn with_tail(head_text: String, tail: &[u8]) -> String {
    let tail_text = plain_text(&crate::htmltext::decode(tail, false));
    let tail_chars = tail_text.chars().count();
    let keep = MAX_TEXT / 4;
    let tail_text: String = tail_text
        .chars()
        .skip(tail_chars.saturating_sub(keep))
        .collect();
    if tail_text.is_empty() {
        return head_text;
    }
    let room = MAX_TEXT
        .saturating_sub(tail_text.chars().count())
        .saturating_sub(TAIL_MARK.chars().count());
    let mut out: String = head_text.chars().take(room).collect();
    out.push_str(TAIL_MARK);
    out.push_str(&tail_text);
    out
}

/// The last `max` bytes of a file, starting at a line: the first, cut line
/// is dropped (unless the whole file fits). Never reads more than `max`
/// bytes, whatever the file's size; empty on any error.
pub async fn read_tail(path: &Path, max: u64) -> Vec<u8> {
    use tokio::io::AsyncSeekExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().await.map(|m| m.len()) else {
        return Vec::new();
    };
    let start = len.saturating_sub(max);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return Vec::new();
    }
    let mut tail = Vec::new();
    if file.take(max).read_to_end(&mut tail).await.is_err() {
        return Vec::new();
    }
    if start == 0 {
        return tail;
    }
    from_next_line(tail)
}

/// The bytes after the first line break (a cut line is no line); nothing
/// when there is no line break at all.
fn from_next_line(bytes: Vec<u8>) -> Vec<u8> {
    match bytes.iter().position(|b| *b == b'\n') {
        Some(at) => bytes.get(at + 1..).unwrap_or_default().to_vec(),
        None => Vec::new(),
    }
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
        assert_eq!(
            extract(
                "solve.log",
                &["T = 85 °C, 3 µm, Düse\n".as_bytes(), b"\xFF step 12\n"].concat()
            ),
            "T = 85 °C, 3 µm, Düse\n\u{fffd} step 12",
            "UTF-8 with one stray byte stays UTF-8"
        );
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

    #[test]
    fn solver_text_files_are_plain_text() {
        for name in [
            "run.trn",
            "x.F06",
            "x.f04",
            "x.pch",
            "run.jou",
            "Macro.java",
            "report-file-0.out",
        ] {
            assert!(indexes(name), "{name}");
        }
        assert_eq!(
            extract("run.trn", b"  500  1.234e-05\n"),
            "  500  1.234e-05"
        );
        assert_eq!(extract("a.out", b"\x7fELF\x02\x01\x00\x00"), "", "binary");
    }

    #[test]
    fn the_end_of_a_large_text_follows_the_beginning() {
        let head = "a".repeat(MAX_TEXT);
        let out = with_tail(head, b"Iteration 500\nsolution is converged\n");
        assert_eq!(out.chars().count(), MAX_TEXT);
        assert!(out.ends_with(&format!("{TAIL_MARK}Iteration 500\nsolution is converged")));
        // the end keeps at most a quarter of the text, its last part
        let long_tail = format!("{}\nlast line", "b".repeat(MAX_TEXT / 2));
        let out = with_tail("head".into(), long_tail.as_bytes());
        assert!(out.starts_with("head\n[…"));
        assert!(out.ends_with("last line"));
        assert!(out.chars().count() <= MAX_TEXT);
        // an empty end changes nothing
        assert_eq!(with_tail("head".into(), b""), "head");
        assert_eq!(from_next_line(b"cut line\nwhole\n".to_vec()), b"whole\n");
        assert!(from_next_line(b"no line break".to_vec()).is_empty());
    }

    #[test]
    fn the_end_follows_only_a_text_beginning_in_the_same_encoding() {
        assert!(tail_usable(b"Iteration 1\n", "Iteration 1", b"converged\n"));
        assert!(
            tail_usable(b"\xEF\xBB\xBFlog\n", "log", b"converged\n"),
            "UTF-8 BOM"
        );
        // binary file: the beginning gave no text
        assert!(!tail_usable(b"\x7fELF\x02\x01\x00", "", b"converged\n"));
        // the end holds binary data or zero padding
        assert!(!tail_usable(b"log\n", "log", b"end\x00\x00\x00"));
        // UTF-16: the end has no byte order mark of its own
        assert!(!tail_usable(b"\xFF\xFEl\x00", "l", b"e\x00n\x00d\x00"));
        assert!(!tail_usable(b"\xFE\xFF\x00l", "l", b"end"));
        assert!(!tail_usable(b"log\n", "log", b""));
    }

    #[tokio::test]
    async fn read_tail_starts_at_a_line_and_reads_at_most_max() {
        let dir = std::env::temp_dir().join(format!("tc-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.trn");
        std::fs::write(&path, b"first line\nsecond line\nthird\n").unwrap();
        assert_eq!(read_tail(&path, 12).await, b"third\n");
        assert_eq!(
            read_tail(&path, 1000).await,
            b"first line\nsecond line\nthird\n"
        );
        assert!(read_tail(&dir.join("missing"), 10).await.is_empty());
        // a large file: the text keeps the beginning and the end
        let mut big = "x 1.5\n".repeat(1_000_000);
        big.push_str("solution is converged\n");
        std::fs::write(&path, big.as_bytes()).unwrap();
        let text = extract_file(&path, "run.trn").await;
        assert!(text.starts_with("x 1.5\n"));
        assert!(
            text.ends_with(&format!("{}solution is converged", "")),
            "{}",
            &text[text.len() - 40..]
        );
        assert!(text.contains(TAIL_MARK));
        assert!(text.chars().count() <= MAX_TEXT);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
