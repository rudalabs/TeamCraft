//! Instance branding: administrators replace the built-in logos.
//!
//! Two slots exist – the *wide* logo (top left of the web header and the
//! desktop title bar) and the *square* logo (web footer and tab icon, desktop
//! navigation rail). Without an upload the built-in TeamCraft logos are shown.
//!
//! Uploads are small images (PNG, JPEG, WebP or SVG, at most
//! [`MAX_LOGO_BYTES`]) kept in the database, so a backup contains them. The
//! type is decided from the content, never from the file name or the
//! `Content-Type` of the upload. SVG files are shown only through `<img>`
//! and are served with a sandboxing policy; files with scripts, event
//! handlers or external references are refused.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    services::audit,
};

/// Largest accepted logo file.
pub const MAX_LOGO_BYTES: usize = 512 * 1024;

/// Largest side of a bitmap logo in pixels.
pub const MAX_LOGO_PIXELS: u32 = 4096;

/// Longest header name (characters).
pub const MAX_NAME_CHARS: usize = 40;

/// Longest subtitle (characters).
pub const MAX_SUBTITLE_CHARS: usize = 20;

/// The subtitle next to the name unless an administrator changes it.
pub const DEFAULT_SUBTITLE: &str = "PLM";

/// A logo slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wide,
    Square,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Wide, Kind::Square];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Wide => "wide",
            Kind::Square => "square",
        }
    }

    pub fn parse(value: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Wide => "Wide logo",
            Kind::Square => "Square logo",
        }
    }
}

/// A stored logo.
#[derive(Debug, sqlx::FromRow)]
pub struct Logo {
    pub content_type: String,
    pub data: Vec<u8>,
    pub sha256: String,
}

/// Which slots hold a custom logo, as the version token (first 12 hex digits
/// of the SHA-256) used to bust caches. `None` = the built-in logo.
/// Also the header texts an administrator may change.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Branding {
    /// Header name set by an administrator; `None` = the configured instance
    /// name (`TEAMCRAFT_INSTANCE_NAME`, default "TeamCraft").
    pub display_name: Option<String>,
    /// Text after the name in the header: "PLM" unless changed, may be empty.
    pub subtitle: String,
    pub wide: Option<String>,
    pub square: Option<String>,
    /// Content type of the wide logo (clients that build the image from
    /// bytes, such as the desktop app, need it: an untyped SVG is not shown).
    pub wide_type: Option<String>,
    /// Content type of the square logo.
    pub square_type: Option<String>,
}

impl Default for Branding {
    fn default() -> Self {
        Branding {
            display_name: None,
            subtitle: DEFAULT_SUBTITLE.to_string(),
            wide: None,
            square: None,
            wide_type: None,
            square_type: None,
        }
    }
}

impl Branding {
    pub fn get(&self, kind: Kind) -> Option<&str> {
        match kind {
            Kind::Wide => self.wide.as_deref(),
            Kind::Square => self.square.as_deref(),
        }
    }

    /// The name to show: the custom one, else `fallback` (the instance name).
    pub fn name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.display_name.as_deref().unwrap_or(fallback)
    }
}

/// The custom slots (one cheap query; used for every page).
pub async fn current(pool: &PgPool) -> AppResult<Branding> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT kind, substr(sha256, 1, 12), content_type FROM branding_logos")
            .fetch_all(pool)
            .await?;
    let mut branding = Branding::default();
    for (kind, token, content_type) in rows {
        match Kind::parse(&kind) {
            Some(Kind::Wide) => {
                branding.wide = Some(token);
                branding.wide_type = Some(content_type);
            }
            Some(Kind::Square) => {
                branding.square = Some(token);
                branding.square_type = Some(content_type);
            }
            None => {}
        }
    }
    let texts: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM branding_text")
        .fetch_all(pool)
        .await?;
    for (key, value) in texts {
        match key.as_str() {
            "display_name" if !value.is_empty() => branding.display_name = Some(value),
            "subtitle" => branding.subtitle = value,
            _ => {}
        }
    }
    Ok(branding)
}

/// Checks one header text: trimmed, plain text without control characters
/// (so no line breaks) and without `<` / `>`, at most `max` characters.
/// Returns the trimmed text (possibly empty).
fn clean_text(label: &str, raw: &str, max: usize) -> AppResult<String> {
    let value = raw.trim();
    if value.chars().count() > max {
        return Err(AppError::BadRequest(format!(
            "The {label} may have at most {max} characters."
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "The {label} must be a single line of plain text."
        )));
    }
    if value.contains(['<', '>']) {
        return Err(AppError::BadRequest(format!(
            "The {label} must not contain markup (< or >)."
        )));
    }
    Ok(value.to_string())
}

/// Validates the display name (empty after trimming = back to the default)
/// and the subtitle (empty = none). Pure.
pub fn validate_names(display_name: &str, subtitle: &str) -> AppResult<(String, String)> {
    Ok((
        clean_text("display name", display_name, MAX_NAME_CHARS)?,
        clean_text("subtitle", subtitle, MAX_SUBTITLE_CHARS)?,
    ))
}

/// Sets the header name and subtitle (administrators). An empty display name
/// restores the default name; a subtitle equal to [`DEFAULT_SUBTITLE`]
/// restores the default subtitle; an empty subtitle shows none.
///
/// Invariants: admin only; the rows and the audit entry (`branding.update`)
/// are written in one transaction.
pub async fn set_names(
    pool: &PgPool,
    actor: &Actor,
    display_name: &str,
    subtitle: &str,
) -> AppResult<()> {
    actor.require(Role::Admin)?;
    let (display_name, subtitle) = validate_names(display_name, subtitle)?;
    let mut tx = pool.begin().await?;
    for (key, value, is_default) in [
        (
            "display_name",
            display_name.as_str(),
            display_name.is_empty(),
        ),
        ("subtitle", subtitle.as_str(), subtitle == DEFAULT_SUBTITLE),
    ] {
        if is_default {
            sqlx::query("DELETE FROM branding_text WHERE key = $1")
                .bind(key)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO branding_text (key, value, updated_by, updated_at)
                 VALUES ($1, $2, $3, now())
                 ON CONFLICT (key) DO UPDATE
                    SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by,
                        updated_at = now()",
            )
            .bind(key)
            .bind(value)
            .bind(actor.id)
            .execute(&mut *tx)
            .await?;
        }
    }
    audit::record(
        &mut tx,
        Some(actor),
        "branding.update",
        "branding",
        None::<Uuid>,
        "Changed the header name".to_string(),
        serde_json::json!({ "display_name": display_name, "subtitle": subtitle }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The stored logo of a slot, if any.
pub async fn get(pool: &PgPool, kind: Kind) -> AppResult<Option<Logo>> {
    Ok(sqlx::query_as::<_, Logo>(
        "SELECT content_type, data, sha256 FROM branding_logos WHERE kind = $1",
    )
    .bind(kind.as_str())
    .fetch_optional(pool)
    .await?)
}

/// What an uploaded file is, from its content.
fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if looks_like_svg(data) {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn looks_like_svg(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("<?xml")
        || lower.starts_with("<svg")
        || lower.starts_with("<!--")
        || lower.starts_with("<!doctype svg"))
        && lower.contains("<svg")
}

/// Refuses SVG content that can run code or load other resources.
fn check_svg(data: &[u8]) -> AppResult<()> {
    let lower = String::from_utf8_lossy(data).to_ascii_lowercase();
    let squeezed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    let forbidden = [
        "<script",
        "<foreignobject",
        "<iframe",
        "<embed",
        "<object",
        "<audio",
        "<video",
        "<animate",
        "<set",
        "<!entity",
        "javascript:",
        "data:text/html",
        "@import",
    ];
    for needle in forbidden {
        if squeezed.contains(needle) {
            return Err(AppError::BadRequest(format!(
                "The SVG contains '{needle}': scripts, animations and external references are not allowed in a logo."
            )));
        }
    }
    // References: `href`, `xlink:href` and `src` values and CSS `url(…)` may
    // point only inside the file (`#id`) or carry an embedded image
    // (`data:image/…`). Namespace declarations (`xmlns:inkscape="http://…"`)
    // and metadata URLs (`rdf:resource`, comments) are only text and stay.
    for value in reference_attribute_values(&lower) {
        if !local_reference(value) {
            return Err(AppError::BadRequest(format!(
                "The SVG links to '{}': a logo may reference only parts of itself (#id) or embedded images.",
                shorten(value)
            )));
        }
    }
    for (i, _) in squeezed.match_indices("url(") {
        let rest = &squeezed[i + 4..];
        let arg = rest.split(')').next().unwrap_or(rest);
        if !local_reference(arg.trim_matches(|c| c == '"' || c == '\'')) {
            return Err(AppError::BadRequest(format!(
                "The SVG contains 'url({})': a logo may reference only parts of itself (#id) or embedded images.",
                shorten(arg)
            )));
        }
    }
    // event handler attributes: onload=, onclick=, …
    let b = lower.as_bytes();
    for (i, _) in lower.match_indices("on") {
        let starts_attribute =
            i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'' | b'/');
        if !starts_attribute {
            continue;
        }
        let rest = &b[i + 2..];
        let letters = rest.iter().take_while(|c| c.is_ascii_alphabetic()).count();
        let after = rest[letters..].iter().find(|c| !c.is_ascii_whitespace());
        if letters > 0 && after == Some(&b'=') {
            return Err(AppError::BadRequest(
                "The SVG contains an event handler attribute (onload, onclick, …).".into(),
            ));
        }
    }
    Ok(())
}

/// Whether a reference stays inside the file: `#id` or an embedded image.
/// Anything else – `http(s)://`, `//host`, `file:`, relative paths, values
/// written with character references (`&#…;`) – is refused.
fn local_reference(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('#') || value.starts_with("data:image/")
}

fn shorten(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() > 60 {
        format!("{}…", value.chars().take(60).collect::<String>())
    } else {
        value.to_string()
    }
}

/// The values of the attributes that load something: `href`, any `…:href`
/// (`xlink:href`) and `src`. Input is the lower-cased SVG text; text that only
/// looks like such an attribute is checked too (refusing is the safe side).
fn reference_attribute_values(lower: &str) -> Vec<&str> {
    let b = lower.as_bytes();
    let mut values = Vec::new();
    for (eq, _) in lower.match_indices('=') {
        // attribute name before the `=` (whitespace allowed in between)
        let mut end = eq;
        while end > 0 && b[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0
            && (b[start - 1].is_ascii_alphanumeric() || matches!(b[start - 1], b':' | b'-' | b'_'))
        {
            start -= 1;
        }
        let name = &lower[start..end];
        if !(name == "href" || name == "src" || name.ends_with(":href")) {
            continue;
        }
        // the value after the `=`: quoted, or up to whitespace / `>`
        let rest = lower[eq + 1..].trim_start();
        let value = match rest.as_bytes().first() {
            Some(&q @ (b'"' | b'\'')) => {
                let inner = &rest[1..];
                &inner[..inner.find(q as char).unwrap_or(inner.len())]
            }
            _ => {
                let len = rest
                    .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                    .unwrap_or(rest.len());
                &rest[..len]
            }
        };
        values.push(value);
    }
    values
}

/// Pixel size of a PNG (IHDR), JPEG (SOFn) or WebP (VP8 / VP8L / VP8X);
/// `None` if it cannot be read.
fn bitmap_size(content_type: &str, data: &[u8]) -> Option<(u32, u32)> {
    match content_type {
        "image/png" if data.len() >= 24 => Some((
            u32::from_be_bytes(data[16..20].try_into().ok()?),
            u32::from_be_bytes(data[20..24].try_into().ok()?),
        )),
        "image/jpeg" => {
            let mut i = 2;
            while i + 9 < data.len() {
                if data[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = data[i + 1];
                if marker == 0xFF {
                    i += 1;
                    continue;
                }
                if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                    let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                    let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                    return Some((w, h));
                }
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len.max(2);
            }
            None
        }
        "image/webp" => webp_size(data),
        _ => None,
    }
}

/// Canvas size of a WebP file from its first chunk.
fn webp_size(data: &[u8]) -> Option<(u32, u32)> {
    let chunk = data.get(12..16)?;
    let body = data.get(20..)?;
    match chunk {
        // lossy: frame tag (3 bytes), start code 9D 01 2A, 14-bit sizes
        b"VP8 " => {
            if body.get(3..6)? != [0x9D, 0x01, 0x2A] {
                return None;
            }
            let w = u16::from_le_bytes([*body.get(6)?, *body.get(7)?]) & 0x3FFF;
            let h = u16::from_le_bytes([*body.get(8)?, *body.get(9)?]) & 0x3FFF;
            Some((w as u32, h as u32))
        }
        // lossless: signature 0x2F, then width-1 and height-1 in 14 bits each
        b"VP8L" => {
            if *body.first()? != 0x2F {
                return None;
            }
            let bits = u32::from_le_bytes(body.get(1..5)?.try_into().ok()?);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        // extended: flags (4 bytes), canvas width-1 and height-1 in 24 bits each
        b"VP8X" => {
            let s = body.get(4..10)?;
            let w = u32::from_le_bytes([s[0], s[1], s[2], 0]) + 1;
            let h = u32::from_le_bytes([s[3], s[4], s[5], 0]) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

/// Checks an upload and returns its content type.
pub fn validate(data: &[u8]) -> AppResult<&'static str> {
    if data.is_empty() {
        return Err(AppError::BadRequest("Choose an image file.".into()));
    }
    if data.len() > MAX_LOGO_BYTES {
        return Err(AppError::BadRequest(format!(
            "The logo is {} KiB; the limit is {} KiB. Reduce the image size.",
            data.len().div_ceil(1024),
            MAX_LOGO_BYTES / 1024
        )));
    }
    let content_type = sniff(data).ok_or_else(|| {
        AppError::BadRequest("The file is not a PNG, JPEG, WebP or SVG image.".into())
    })?;
    if content_type == "image/svg+xml" {
        check_svg(data)?;
    }
    if content_type != "image/svg+xml" {
        // a bitmap whose size cannot be read is refused: every client would
        // decode it without knowing how large it gets
        let (w, h) = bitmap_size(content_type, data).ok_or_else(|| {
            AppError::BadRequest(
                "The image size cannot be read; the file is damaged or of an unusual kind. Save it again as PNG."
                    .into(),
            )
        })?;
        if w == 0 || h == 0 || w > MAX_LOGO_PIXELS || h > MAX_LOGO_PIXELS {
            return Err(AppError::BadRequest(format!(
                "The image is {w}×{h} pixels; each side must be between 1 and {MAX_LOGO_PIXELS}."
            )));
        }
    }
    Ok(content_type)
}

/// Replaces the logo of a slot (administrators).
///
/// Invariants: admin only; the row is replaced in one statement together with
/// its audit entry (`branding.set`) in one transaction.
pub async fn set(pool: &PgPool, actor: &Actor, kind: Kind, data: &[u8]) -> AppResult<()> {
    actor.require(Role::Admin)?;
    let content_type = validate(data)?;
    let sha256 = hex::encode(Sha256::digest(data));
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO branding_logos (kind, content_type, data, sha256, updated_by, updated_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (kind) DO UPDATE
            SET content_type = EXCLUDED.content_type, data = EXCLUDED.data,
                sha256 = EXCLUDED.sha256, updated_by = EXCLUDED.updated_by,
                updated_at = now()",
    )
    .bind(kind.as_str())
    .bind(content_type)
    .bind(data)
    .bind(&sha256)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "branding.set",
        "branding",
        None::<Uuid>,
        format!("Replaced the {} logo", kind.as_str()),
        serde_json::json!({ "kind": kind.as_str(), "content_type": content_type,
                            "bytes": data.len(), "sha256": sha256 }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Back to the built-in logo of a slot (administrators). Returns whether a
/// custom logo existed.
///
/// Invariants: admin only; deletion and audit entry (`branding.reset`) in one
/// transaction; resetting an empty slot changes nothing and writes nothing.
pub async fn reset(pool: &PgPool, actor: &Actor, kind: Kind) -> AppResult<bool> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query("DELETE FROM branding_logos WHERE kind = $1")
        .bind(kind.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if deleted {
        audit::record(
            &mut tx,
            Some(actor),
            "branding.reset",
            "branding",
            None::<Uuid>,
            format!("Restored the built-in {} logo", kind.as_str()),
            serde_json::json!({ "kind": kind.as_str() }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    /// SOI, an APP0 segment, SOF0 with the size and a few bytes after it.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0, 0xFF, 0xC0, 0x00, 0x0B, 8,
        ];
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
        v
    }

    fn riff(chunk: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&((body.len() + 12) as u32).to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(chunk);
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn webp_lossy(w: u16, h: u16) -> Vec<u8> {
        let mut body = vec![0x30, 0x01, 0x00, 0x9D, 0x01, 0x2A];
        body.extend_from_slice(&w.to_le_bytes());
        body.extend_from_slice(&h.to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        riff(b"VP8 ", &body)
    }

    fn webp_lossless(w: u32, h: u32) -> Vec<u8> {
        let bits = (w - 1) | ((h - 1) << 14);
        let mut body = vec![0x2F];
        body.extend_from_slice(&bits.to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        riff(b"VP8L", &body)
    }

    fn webp_extended(w: u32, h: u32) -> Vec<u8> {
        let mut body = vec![0x10, 0, 0, 0];
        body.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        body.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        riff(b"VP8X", &body)
    }

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#036"/></svg>"##;

    /// What Inkscape 1.x writes with "Save" (Inkscape SVG): namespace
    /// declarations, a comment with a URL, `sodipodi:namedview`, RDF metadata
    /// with licence URLs, gradients via `url(#…)`, `xlink:href` to an id and
    /// an embedded PNG.
    const INKSCAPE_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!-- Created with Inkscape (http://www.inkscape.org/) -->

<svg
   width="600"
   height="120"
   viewBox="0 0 158.75 31.75"
   version="1.1"
   id="svg5"
   inkscape:version="1.2.2 (b0a8486541, 2022-12-01)"
   sodipodi:docname="logo.svg"
   inkscape:export-filename="logo.png"
   inkscape:export-xdpi="96"
   inkscape:export-ydpi="96"
   xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
   xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
   xmlns:xlink="http://www.w3.org/1999/xlink"
   xmlns="http://www.w3.org/2000/svg"
   xmlns:svg="http://www.w3.org/2000/svg"
   xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
   xmlns:cc="http://creativecommons.org/ns#"
   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <sodipodi:namedview
     id="namedview7"
     pagecolor="#ffffff"
     bordercolor="#666666"
     borderopacity="1.0"
     inkscape:showpageshadow="2"
     inkscape:pageopacity="0.0"
     inkscape:pagecheckerboard="0"
     inkscape:deskcolor="#d1d1d1"
     inkscape:document-units="mm"
     showgrid="false"
     inkscape:zoom="1.2"
     inkscape:window-width="1920"
     inkscape:window-height="1011"
     inkscape:window-maximized="1"
     inkscape:current-layer="layer1" />
  <defs
     id="defs2">
    <linearGradient
       inkscape:collect="always"
       id="linearGradient1">
      <stop
         style="stop-color:#003366;stop-opacity:1"
         offset="0"
         id="stop1" />
      <stop
         style="stop-color:#0066cc;stop-opacity:1"
         offset="1"
         id="stop2" />
    </linearGradient>
  </defs>
  <metadata
     id="metadata1">
    <rdf:RDF>
      <cc:Work
         rdf:about="">
        <dc:format>image/svg+xml</dc:format>
        <dc:type
           rdf:resource="http://purl.org/dc/dcmitype/StillImage" />
        <cc:license
           rdf:resource="http://creativecommons.org/licenses/by-sa/4.0/" />
      </cc:Work>
    </rdf:RDF>
  </metadata>
  <g
     inkscape:label="Layer 1"
     inkscape:groupmode="layer"
     id="layer1">
    <rect
       style="fill:url(#linearGradient1);fill-opacity:1;stroke:none"
       id="rect1"
       width="31.75"
       height="31.75"
       x="0"
       y="0"
       rx="4" />
    <path
       style="fill:#ffffff;stroke:none"
       d="M 6,6 H 26 V 10 H 18 V 26 H 14 V 10 H 6 Z"
       id="path1"
       sodipodi:nodetypes="ccccccccc"
       inkscape:connector-curvature="0" />
    <use
       x="0"
       y="0"
       xlink:href="#path1"
       id="use1"
       transform="translate(40)" />
    <image
       width="8"
       height="8"
       preserveAspectRatio="none"
       xlink:href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
       id="image1"
       x="140"
       y="2" />
    <text
       xml:space="preserve"
       style="font-size:12px;font-family:sans-serif;fill:#003366"
       x="70"
       y="20"
       id="text1"><tspan
         sodipodi:role="line"
         id="tspan1"
         x="70"
         y="20">ACME</tspan></text>
  </g>
</svg>
"##;

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(
            validate_names("  Rocket Team  ", " PDM ").unwrap(),
            ("Rocket Team".to_string(), "PDM".to_string())
        );
        // empty is allowed: the default name / no subtitle
        assert_eq!(
            validate_names("   ", "").unwrap(),
            (String::new(), String::new())
        );
        // limits count characters, not bytes
        assert!(
            validate_names(&"ä".repeat(MAX_NAME_CHARS), &"ö".repeat(MAX_SUBTITLE_CHARS)).is_ok()
        );
        assert!(validate_names(&"x".repeat(MAX_NAME_CHARS + 1), "").is_err());
        assert!(validate_names("", &"x".repeat(MAX_SUBTITLE_CHARS + 1)).is_err());
        for bad in ["a\nb", "a\tb", "a\u{0}b", "<b>x</b>", "a>b", "a\r"] {
            // a trailing line break is trimmed, so only inner ones are refused
            let r = validate_names(bad, "");
            if bad == "a\r" {
                assert!(r.is_ok());
            } else {
                assert!(r.is_err(), "{bad:?}");
            }
            assert!(validate_names("", bad).is_err() || bad == "a\r");
        }
        assert!(validate_names("Tom & Jerry's \"Shop\"", "").is_ok());
    }

    #[test]
    fn the_name_falls_back_to_the_instance_name() {
        let mut b = Branding::default();
        assert_eq!(b.name("Env"), "Env");
        assert_eq!(b.subtitle, "PLM");
        b.display_name = Some("Custom".into());
        assert_eq!(b.name("Env"), "Custom");
    }

    #[test]
    fn kinds_round_trip() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("icon"), None);
    }

    #[test]
    fn accepts_png_jpeg_webp_and_plain_svg() {
        assert_eq!(validate(&png(600, 120)).unwrap(), "image/png");
        assert_eq!(validate(&jpeg(600, 120)).unwrap(), "image/jpeg");
        assert_eq!(validate(&webp_lossy(600, 120)).unwrap(), "image/webp");
        assert_eq!(validate(&webp_lossless(600, 120)).unwrap(), "image/webp");
        assert_eq!(validate(&webp_extended(600, 120)).unwrap(), "image/webp");
        assert_eq!(validate(SVG).unwrap(), "image/svg+xml");
        let with_xml = [b"<?xml version=\"1.0\"?>\n".as_slice(), SVG].concat();
        assert_eq!(validate(&with_xml).unwrap(), "image/svg+xml");
    }

    #[test]
    fn webp_size_is_read_from_every_kind_of_first_chunk() {
        assert_eq!(webp_size(&webp_lossy(300, 120)), Some((300, 120)));
        assert_eq!(webp_size(&webp_lossless(300, 120)), Some((300, 120)));
        assert_eq!(webp_size(&webp_extended(300, 120)), Some((300, 120)));
        assert_eq!(webp_size(&webp_lossless(16384, 1)), Some((16384, 1)));
        assert_eq!(
            webp_size(&webp_extended(16383, 16383)),
            Some((16383, 16383))
        );
    }

    #[test]
    fn huge_webp_and_bitmaps_without_a_readable_size_are_refused() {
        // a 16383 × 16383 WebP fits into a few hundred bytes
        assert!(validate(&webp_lossless(16383, 16383)).is_err());
        assert!(validate(&webp_extended(MAX_LOGO_PIXELS + 1, 10)).is_err());
        assert!(validate(&webp_lossy(10, MAX_LOGO_PIXELS as u16 + 1)).is_err());
        assert!(validate(&webp_extended(MAX_LOGO_PIXELS, MAX_LOGO_PIXELS)).is_ok());
        // headers only: the size cannot be read
        assert!(validate(b"RIFF\0\0\0\0WEBPVP8 ").is_err());
        assert!(validate(&riff(b"VP8 ", &[0x30, 0x01, 0x00, 0, 0, 0, 1, 0, 1, 0])).is_err());
        assert!(validate(&riff(b"VP8L", &[0x00, 1, 2, 3, 4])).is_err());
        assert!(validate(&riff(b"ALPH", &[0; 16])).is_err());
        // a JPEG without a frame header
        assert!(validate(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 2]).is_err());
        assert!(validate(&jpeg(MAX_LOGO_PIXELS as u16 + 1, 10)).is_err());
        assert!(validate(&jpeg(10, 0)).is_err());
    }

    #[test]
    fn accepts_an_inkscape_svg_with_namespaces_and_metadata() {
        assert_eq!(validate(INKSCAPE_SVG.as_bytes()).unwrap(), "image/svg+xml");
    }

    #[test]
    fn refuses_external_references_in_an_inkscape_svg() {
        let linked = [
            // linked instead of embedded image (Inkscape's "link" import)
            (
                r#"xlink:href="data:image/png;base64,"#,
                r#"xlink:href="file:///home/me/logo.png" data-x=""#,
            ),
            (
                r#"xlink:href="data:image/png;base64,"#,
                r#"xlink:href="https://cdn.example.com/x.png" data-x=""#,
            ),
            (r##"xlink:href="#path1""##, r##"href="//evil.example/a.svg#p""##),
            (r##"xlink:href="#path1""##, r#"xlink:href = 'logo.svg#p'"#),
            // character references hide the scheme
            (
                r##"xlink:href="#path1""##,
                r#"xlink:href="&#104;ttps://evil.example/""#,
            ),
            (
                "fill:url(#linearGradient1)",
                "fill:url(https://evil.example/g.svg#g)",
            ),
            (
                "fill:url(#linearGradient1)",
                "fill:url( 'http://evil.example/g.svg#g' )",
            ),
            ("fill:url(#linearGradient1)", "fill:url(//evil.example/g)"),
            // still refused: scripts, handlers, foreignObject, entities
            ("<defs", "<script>alert(1)</script><defs"),
            (r#"id="rect1""#, r#"id="rect1" onload="alert(1)""#),
            ("<defs", "<foreignObject><p>x</p></foreignObject><defs"),
            (
                "<svg\n",
                "<!DOCTYPE svg [<!ENTITY ns_extend \"http://ns.adobe.com/Extensibility/1.0/\">]>\n<svg\n",
            ),
        ];
        for (from, to) in linked {
            assert!(INKSCAPE_SVG.contains(from), "test fixture lacks {from}");
            let bad = INKSCAPE_SVG.replacen(from, to, 1);
            assert!(validate(bad.as_bytes()).is_err(), "accepted: {to}");
        }
    }

    #[test]
    fn reference_attributes_are_found_in_every_spelling() {
        let svg = r##"<svg a:href = "#a" src='x' href=y/><use xlink:href="#b"/><x nohref="z" hreflang="w"/></svg>"##;
        assert_eq!(reference_attribute_values(svg), vec!["#a", "x", "y", "#b"]);
    }

    #[test]
    fn refuses_other_files_and_empty_uploads() {
        assert!(validate(b"").is_err());
        assert!(validate(b"GIF89a....").is_err());
        assert!(validate(b"<html><svg></svg></html>").is_err());
        assert!(validate(b"MZ\x90\0 an executable").is_err());
        assert!(validate(b"just text").is_err());
    }

    #[test]
    fn refuses_oversized_files_and_huge_bitmaps() {
        let mut big = png(10, 10);
        big.resize(MAX_LOGO_BYTES + 1, 0);
        assert!(validate(&big).is_err());
        assert!(validate(&png(MAX_LOGO_PIXELS + 1, 10)).is_err());
        assert!(validate(&png(0, 10)).is_err());
        assert!(validate(&png(MAX_LOGO_PIXELS, MAX_LOGO_PIXELS)).is_ok());
    }

    #[test]
    fn refuses_active_svg_content() {
        for bad in [
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect onclick = 'x()'/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><image href="http://evil.example/x.png"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(x)</style></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><SCRIPT>1</SCRIPT></svg>"#,
        ] {
            assert!(validate(bad.as_bytes()).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn plain_attributes_that_contain_on_are_not_event_handlers() {
        let ok = br#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0" fill-rule="evenodd" stroke-linejoin="round" stroke-width="2"/><g transform="rotate(5)" font-family="Arial"/></svg>"#;
        assert!(validate(ok).is_ok());
    }

    #[test]
    fn jpeg_size_is_read_from_the_frame_header() {
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0, 0xFF, 0xC0, 0x00, 0x0B, 8, 0x00, 0x78, 0x01,
            0x2C, 1, 1, 0x11, 0,
        ];
        assert_eq!(bitmap_size("image/jpeg", &jpeg), Some((300, 120)));
    }
}
