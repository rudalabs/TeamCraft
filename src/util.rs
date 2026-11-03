//! Small string helpers without external dependencies.

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn pct_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reduces an uploaded file name to a safe base name.
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(200)
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

/// `Content-Disposition` value that forces a download and keeps non-ASCII
/// file names intact (RFC 6266 / RFC 5987).
pub fn content_disposition(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        pct_encode(filename)
    )
}

/// Escapes a CSV field (RFC 4180). Fields starting with characters that
/// spreadsheet programs interpret as formulas are prefixed with `'`.
pub fn csv_field(value: &str) -> String {
    let value = if value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if value.contains([',', '"', '\n', '\r', ';']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

/// Formats a number with up to `decimals` decimals, trimming trailing zeros.
pub fn format_number(value: f64, decimals: usize) -> String {
    let s = format!("{value:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Human readable byte size (binary units).
pub fn format_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{} {}", format_number(value, 1), UNITS[unit])
    }
}

/// Only accept local, absolute paths as redirect targets (prevents open
/// redirects via `?next=https://evil.example`).
pub fn safe_redirect_target(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_string(),
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding() {
        assert_eq!(pct_encode("abc-._~"), "abc-._~");
        assert_eq!(pct_encode("a b/ä"), "a%20b%2F%C3%A4");
    }

    #[test]
    fn filenames() {
        assert_eq!(
            sanitize_filename("C:\\Users\\x\\drawing.pdf"),
            "drawing.pdf"
        );
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("  .hidden "), "hidden");
        assert_eq!(sanitize_filename("a\u{0}b.txt"), "ab.txt");
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(
            content_disposition("Düse \"v2\".step"),
            "attachment; filename=\"D_se _v2_.step\"; filename*=UTF-8''D%C3%BCse%20%22v2%22.step"
        );
    }

    #[test]
    fn csv_escaping() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(1.5, 3), "1.5");
        assert_eq!(format_number(2.0, 3), "2");
        assert_eq!(format_number(0.12345, 3), "0.123");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5 MiB");
    }

    #[test]
    fn redirect_targets() {
        assert_eq!(safe_redirect_target(Some("/items?q=1")), "/items?q=1");
        assert_eq!(safe_redirect_target(Some("//evil.example")), "/");
        assert_eq!(safe_redirect_target(Some("https://evil.example")), "/");
        assert_eq!(safe_redirect_target(Some("/\\evil.example")), "/");
        assert_eq!(safe_redirect_target(None), "/");
    }
}
