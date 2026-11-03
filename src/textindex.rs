//! Text extraction for the full-text search over file contents.
//!
//! * STEP (`.step`, `.stp`) and IGES (`.iges`, `.igs`): the quoted strings of
//!   the file – product names, descriptions, authors, organisation, layer and
//!   entity names – without the numeric geometry.
//! * Plain text formats (txt, md, csv, json, xml, yaml, ASCII STL, KiCad, …):
//!   the text itself.
//! * Everything else (PDF, binary CAD, images): nothing (not indexed yet).
//!
//! At most [`MAX_READ`] bytes are read and [`MAX_TEXT`] characters are kept.

use std::path::Path;

use tokio::io::AsyncReadExt;

/// Bytes read from the beginning of a file.
const MAX_READ: u64 = 4 * 1024 * 1024;
/// Characters kept for indexing.
const MAX_TEXT: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    QuotedStrings,
    PlainText,
    None,
}

fn kind_of(filename: &str) -> Kind {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "step" | "stp" | "iges" | "igs" => Kind::QuotedStrings,
        "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "log" | "stl" | "obj" | "kicad_sch" | "kicad_pcb" | "kicad_pro" | "net" | "sch"
        | "html" | "htm" | "tex" | "rst" | "py" | "rs" | "c" | "h" | "cpp" | "hpp" | "m" => {
            Kind::PlainText
        }
        _ => Kind::None,
    }
}

/// Extracts searchable text from raw file content.
pub fn extract(filename: &str, content: &[u8]) -> String {
    let text = match kind_of(filename) {
        Kind::None => return String::new(),
        // Drop decimal numbers (e.g. the facet coordinates of ASCII STL files);
        // integers like "912" in "DIN 912" stay searchable.
        Kind::PlainText => String::from_utf8_lossy(content)
            .split_whitespace()
            .filter(|w| !is_decimal_number(w))
            .collect::<Vec<_>>()
            .join(" "),
        // Quoted STEP/IGES strings are names – keep them as they are ("1.4404").
        Kind::QuotedStrings => quoted_strings(&String::from_utf8_lossy(content)),
    };
    text.chars().take(MAX_TEXT).collect()
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
    let mut content = Vec::new();
    if file.take(MAX_READ).read_to_end(&mut content).await.is_err() {
        return String::new();
    }
    extract(filename, &content)
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
        FILE_NAME('nx-eng-001.step','2026-11-03T10:00:00',('Erika'),('WARR e.V.'),'','','');\nENDSEC;\nDATA;\n\
        #10=PRODUCT('NX-ENG-001','Engine mount','',(#20));\n#11=CARTESIAN_POINT('',(0.,1.5,2.));\n\
        #12=PRODUCT('M6','Bolt M6x20 DIN 912','It''s steel',(#20));\nENDSEC;\nEND-ISO-10303-21;";

    #[test]
    fn step_names_are_extracted() {
        let text = extract("Mount.STEP", STEP.as_bytes());
        assert!(text.contains("Engine mount bracket"));
        assert!(text.contains("Bolt M6x20 DIN 912"));
        assert!(text.contains("It's steel"), "escaped quotes");
        assert!(text.contains("WARR e.V."));
        assert!(!text.contains("1.5"), "geometry is not indexed");
    }

    #[test]
    fn text_and_binary_files() {
        assert_eq!(
            extract("notes.md", b"Thrust 2 kN, DIN 912\nburn time 4.2 s"),
            "Thrust 2 kN, DIN 912 burn time s"
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
        assert_eq!(extract("drawing.pdf", b"%PDF-1.7 binary"), "");
        assert_eq!(extract("no_extension", b"text"), "");
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
