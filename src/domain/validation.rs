//! Validation helpers and the working-folder rules.

use crate::error::AppError;

/// The first `max` characters of `s`, with "…" if it was cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Item numbers: 1–64 characters `A-Z 0-9 . _ -`, starting with a letter or
/// digit. They are normalised to upper case.
pub fn normalize_item_number(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_uppercase();
    let valid = !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid item number '{raw}': use 1–64 characters A–Z, 0–9, '.', '_' or '-', starting with a letter or digit."
        )))
    }
}

/// Usernames: 2–64 characters `a-z 0-9 . _ -`, normalised to lower case.
pub fn normalize_username(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_lowercase();
    let valid = (2..=64).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid username '{raw}': use 2–64 characters a–z, 0–9, '.', '_' or '-'."
        )))
    }
}

/// Project codes: 1–32 characters `A-Z 0-9 _ -`, normalised to upper case.
pub fn normalize_project_code(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_uppercase();
    let valid = (1..=32).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid project code '{raw}': use 1–32 characters A–Z, 0–9, '_' or '-'."
        )))
    }
}

pub const MIN_PASSWORD_LEN: usize = 10;

pub fn validate_password(password: &str) -> Result<(), AppError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(AppError::BadRequest(format!(
            "Passwords must be at least {MIN_PASSWORD_LEN} characters long."
        )));
    }
    if password.len() > 1024 {
        return Err(AppError::BadRequest("Password is too long.".into()));
    }
    Ok(())
}

/// Requires a non-empty, trimmed text of at most `max` characters.
pub fn require_text(field: &str, value: &str, max: usize) -> Result<String, AppError> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::BadRequest(format!("{field} must not be empty.")));
    }
    limit_text(field, v, max)
}

// ---------------------------------------------------------------------------
// Working folder: where a file sits inside its project group
// ---------------------------------------------------------------------------

/// Longest folder path of a file (characters).
pub const MAX_FOLDER_LEN: usize = 400;
/// Deepest folder nesting of a file.
pub const MAX_FOLDER_DEPTH: usize = 32;
/// Longest single folder name.
pub const MAX_FOLDER_SEGMENT: usize = 120;

/// Normalises the folder of a file inside its project group – the place it
/// gets in the desktop working folder `<workspace>/<GROUP>/<folder>/<file>`.
///
/// Backslashes become `/`, empty and `.` segments as well as leading and
/// trailing slashes are dropped (`""` is the top of the group). Refused:
/// `..`, names that are not valid on Windows, macOS and Linux (`<>:"|?*`,
/// control characters, trailing dots or spaces, `CON`, `NUL`, `COM1`, …) and
/// paths longer than [`MAX_FOLDER_LEN`] or deeper than [`MAX_FOLDER_DEPTH`].
pub fn normalize_folder(raw: &str) -> Result<String, AppError> {
    let bad = |why: String| AppError::BadRequest(format!("Folder '{}': {why}", raw.trim()));
    let mut segments = Vec::new();
    for segment in raw.trim().split(['/', '\\']) {
        let segment = segment.trim_start();
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(bad(
                "'..' is not allowed – folders are relative to the project group.".into(),
            ));
        }
        if let Some(c) = segment
            .chars()
            .find(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(bad(format!(
                "the character {c:?} is not allowed in folder names."
            )));
        }
        if segment.ends_with(['.', ' ']) {
            return Err(bad(format!(
                "'{segment}' ends with a dot or a space (not valid on Windows)."
            )));
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or(segment)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return Err(bad(format!("'{segment}' is a reserved name on Windows.")));
        }
        if segment.chars().count() > MAX_FOLDER_SEGMENT {
            return Err(bad(format!(
                "folder names must be at most {MAX_FOLDER_SEGMENT} characters."
            )));
        }
        segments.push(segment);
    }
    if segments.len() > MAX_FOLDER_DEPTH {
        return Err(bad(format!(
            "at most {MAX_FOLDER_DEPTH} folder levels are allowed."
        )));
    }
    let folder = segments.join("/");
    if folder.chars().count() > MAX_FOLDER_LEN {
        return Err(bad(format!(
            "the path must be at most {MAX_FOLDER_LEN} characters."
        )));
    }
    Ok(folder)
}

/// Trims a text and checks it is at most `max` characters.
pub fn limit_text(field: &str, value: &str, max: usize) -> Result<String, AppError> {
    let v = value.trim();
    if v.chars().count() > max {
        return Err(AppError::BadRequest(format!(
            "{field} must be at most {max} characters."
        )));
    }
    Ok(v.to_string())
}

/// Validates an optional non-negative number such as a mass or a cost.
pub fn validate_non_negative(field: &str, value: Option<f64>) -> Result<Option<f64>, AppError> {
    match value {
        Some(v) if !v.is_finite() || v < 0.0 => Err(AppError::BadRequest(format!(
            "{field} must be a non-negative number."
        ))),
        other => Ok(other),
    }
}

/// Validates a BOM quantity.
pub fn validate_quantity(value: f64) -> Result<f64, AppError> {
    if value.is_finite() && value > 0.0 && value < 1e9 {
        Ok(value)
    } else {
        Err(AppError::BadRequest(
            "Quantity must be a positive number.".into(),
        ))
    }
}

/// Parses user supplied decimal numbers, accepting both `1.5` and `1,5`.
/// Empty input yields `None`.
pub fn parse_decimal(field: &str, raw: Option<&str>) -> Result<Option<f64>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    raw.replace(',', ".")
        .parse::<f64>()
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("{field}: '{raw}' is not a number.")))
}

/// Escapes `%`, `_` and `\` so user input can be used inside a LIKE pattern.
pub fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for c in query.trim().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}
