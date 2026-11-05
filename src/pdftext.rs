//! Text of PDF reports for the full-text search and the assistant
//! (`read_file_text`); see `docs/src/research/pdf-text-extraction.md`.
//!
//! A PDF is untrusted input, so it is never parsed in the server process: the
//! hidden `teamcraft text-extract` child process does it, with a time limit,
//! the memory limit of the 3D previews and a size cap before the file is
//! opened (in tests, where there is no such binary, on a blocking thread).
//! The result is bounded as well – [`MAX_PAGES`] pages, [`MAX_CHARS`]
//! characters. There is no OCR: a scanned PDF has no text.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock,
    },
    time::Duration,
};

use tokio::sync::Semaphore;

/// A PDF above this size is not opened (CFD reports are small; big PDFs are scans).
pub const MAX_PDF_BYTES: u64 = 32 * 1024 * 1024;

/// Pages read.
pub const MAX_PAGES: usize = 100;

/// Characters kept (the cap of every extracted text).
pub const MAX_CHARS: usize = 200_000;

/// Time limit of one extraction.
pub const TIME_LIMIT: Duration = Duration::from_secs(60);

/// Extractions at the same time.
const PARALLEL: usize = 2;

static SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(PARALLEL));

/// Address space of the child, set at start-up from the preview limits.
static MEMORY_BYTES: AtomicU64 = AtomicU64::new(1024 * 1024 * 1024);

/// Takes the memory limit of the child process (0 = unlimited).
pub fn configure(memory_bytes: u64) {
    MEMORY_BYTES.store(memory_bytes, Ordering::Relaxed);
}

/// Whether a file name is a PDF.
pub fn is_pdf(filename: &str) -> bool {
    filename
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("pdf"))
}

/// The text of a PDF held in memory: at most [`MAX_PAGES`] pages and
/// [`MAX_CHARS`] characters; pages that cannot be read are skipped. A parser
/// panic is an error, not a crash.
pub fn extract(bytes: &[u8]) -> anyhow::Result<String> {
    let bytes = bytes.to_vec();
    let result = std::panic::catch_unwind(move || -> anyhow::Result<String> {
        let doc = pdf_oxide::PdfDocument::from_bytes(bytes)
            .map_err(|e| anyhow::anyhow!("not a readable PDF: {e}"))?;
        let pages = doc
            .page_count()
            .map_err(|e| anyhow::anyhow!("not a readable PDF: {e}"))?;
        let mut out = String::new();
        for page in 0..pages.min(MAX_PAGES) {
            if let Ok(text) = doc.extract_text(page) {
                out.push_str(&text);
                out.push('\n');
            }
            if out.chars().count() > MAX_CHARS {
                break;
            }
        }
        Ok(tidy(&out))
    });
    match result {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("the PDF could not be read")),
    }
}

/// Control characters (NUL included) become spaces, line breaks stay (tables
/// keep their rows), runs of blank lines collapse, the length is capped.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_CHARS + 16));
    let mut blank_lines = 0;
    for line in text.lines() {
        let line: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        out.push_str(line);
        out.push('\n');
        if out.chars().count() > MAX_CHARS {
            break;
        }
    }
    out.trim().chars().take(MAX_CHARS).collect()
}

/// One job of the child process: the text of `input` into `output`. A file
/// above `max_input_bytes` is refused before it is opened. Returns the number
/// of characters.
pub fn run_job(input: &Path, output: &Path, max_input_bytes: u64) -> anyhow::Result<usize> {
    let size = std::fs::metadata(input)?.len();
    if size > max_input_bytes {
        anyhow::bail!(
            "This PDF is too large to read ({} MB, at most {} MB).",
            size.div_ceil(1024 * 1024),
            max_input_bytes / (1024 * 1024)
        );
    }
    let text = extract(&std::fs::read(input)?)?;
    std::fs::write(output, text.as_bytes())?;
    Ok(text.chars().count())
}

/// The text of a stored PDF; empty if it has none or cannot be read (a
/// scanned or damaged file never fails an upload).
pub async fn extract_file(path: &Path) -> String {
    let Ok(_slot) = SLOTS.acquire().await else {
        return String::new();
    };
    let out = path.with_extension(format!("{}.txt.tmp", uuid::Uuid::new_v4().simple()));
    let outcome = match crate::preview::worker_exe() {
        Some(exe) => run_worker(&exe, path, &out).await,
        None => run_in_process(path, &out).await,
    };
    let text = match outcome {
        Ok(()) => tokio::fs::read(&out)
            .await
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .chars()
                    .take(MAX_CHARS)
                    .collect()
            })
            .unwrap_or_default(),
        Err(message) => {
            tracing::debug!(%message, "no text from the PDF");
            String::new()
        }
    };
    let _ = tokio::fs::remove_file(&out).await;
    text
}

async fn run_worker(exe: &Path, input: &Path, output: &Path) -> Result<(), String> {
    let child = tokio::process::Command::new(exe)
        .arg("text-extract")
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--max-input-bytes")
        .arg(MAX_PDF_BYTES.to_string())
        .arg("--max-memory-bytes")
        .arg(MEMORY_BYTES.load(Ordering::Relaxed).to_string())
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("the text extractor could not be started: {e}"))?;
    match tokio::time::timeout(TIME_LIMIT, child.wait_with_output()).await {
        Err(_) => Err(format!("no result within {} s", TIME_LIMIT.as_secs())),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("the extractor stopped unexpectedly")
            .to_string()),
    }
}

async fn run_in_process(input: &Path, output: &Path) -> Result<(), String> {
    let (input, output) = (input.to_path_buf(), output.to_path_buf());
    let task = tokio::task::spawn_blocking(move || {
        run_job(&input, &output, MAX_PDF_BYTES)
            .map(drop)
            .map_err(|e| e.to_string())
    });
    match tokio::time::timeout(TIME_LIMIT, task).await {
        Err(_) => Err("timed out".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(r)) => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF with the given lines of text (Helvetica).
    pub(crate) fn sample_pdf(lines: &[&str]) -> Vec<u8> {
        let mut content = String::from("BT /F1 12 Tf 14 TL 50 740 Td\n");
        for line in lines {
            let escaped = line
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            content.push_str(&format!("({escaped}) Tj T*\n"));
        }
        content.push_str("ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }

    #[test]
    fn the_text_of_a_pdf_is_found() {
        let pdf = sample_pdf(&[
            "Pressure drop: 388.25 Pa",
            "Max. von Mises stress 182.5 MPa",
        ]);
        let text = extract(&pdf).unwrap();
        assert!(text.contains("388.25"), "{text:?}");
        assert!(text.contains("von Mises"), "{text:?}");
    }

    #[test]
    fn damaged_and_hostile_files_fail_cleanly() {
        let pdf = sample_pdf(&["Pressure drop: 388.25 Pa"]);
        for cut in [0, 5, 40, pdf.len() / 2, pdf.len() - 10] {
            // an error or whatever text survives – never a panic
            let _ = extract(&pdf[..cut]);
        }
        assert!(extract(b"not a pdf at all").is_err());
        assert!(extract(&[]).is_err());
        let mut flipped = pdf.clone();
        for i in (0..flipped.len()).step_by(7) {
            flipped[i] ^= 0xFF;
        }
        let _ = extract(&flipped);
    }

    #[test]
    fn tidy_keeps_rows_and_drops_noise() {
        assert_eq!(tidy("a\u{0}b  \n\n\n\nc\t d\n"), "a b\n\nc  d");
        let long = "x".repeat(MAX_CHARS * 2);
        assert_eq!(tidy(&long).chars().count(), MAX_CHARS);
    }

    #[test]
    fn names_and_size_cap() {
        assert!(is_pdf("report.PDF") && is_pdf("a.b.pdf") && !is_pdf("pdf") && !is_pdf(".pdf"));
        let dir = std::env::temp_dir().join(format!("tc-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.pdf");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(5 * 1024 * 1024)
            .unwrap();
        let e = run_job(&big, &dir.join("out.txt"), 1024 * 1024).unwrap_err();
        assert!(e.to_string().contains("too large to read"), "{e}");
        assert!(!dir.join("out.txt").exists(), "never opened");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stored_pdfs_are_read_in_process_during_tests() {
        let dir = std::env::temp_dir().join(format!("tc-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");
        std::fs::write(&path, sample_pdf(&["Mass flow 2.25 kg/s"])).unwrap();
        assert!(extract_file(&path).await.contains("2.25"));
        std::fs::write(&path, b"junk").unwrap();
        assert_eq!(extract_file(&path).await, "");
        std::fs::remove_dir_all(&dir).ok();
    }
}
