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
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Branding {
    pub wide: Option<String>,
    pub square: Option<String>,
}

impl Branding {
    pub fn get(&self, kind: Kind) -> Option<&str> {
        match kind {
            Kind::Wide => self.wide.as_deref(),
            Kind::Square => self.square.as_deref(),
        }
    }
}

/// The custom slots (one cheap query; used for every page).
pub async fn current(pool: &PgPool) -> AppResult<Branding> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, substr(sha256, 1, 12) FROM branding_logos")
            .fetch_all(pool)
            .await?;
    let mut branding = Branding::default();
    for (kind, token) in rows {
        match Kind::parse(&kind) {
            Some(Kind::Wide) => branding.wide = Some(token),
            Some(Kind::Square) => branding.square = Some(token),
            None => {}
        }
    }
    Ok(branding)
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
        "http://",
        "https://",
        "@import",
    ];
    for needle in forbidden {
        // `http://www.w3.org/…` namespaces are normal; everything else is not
        let hit = if needle.starts_with("http") {
            squeezed
                .match_indices(needle)
                .any(|(i, _)| !squeezed[i..].starts_with("http://www.w3.org/"))
        } else {
            squeezed.contains(needle)
        };
        if hit {
            return Err(AppError::BadRequest(format!(
                "The SVG contains '{needle}': scripts, animations and external references are not allowed in a logo."
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

/// Pixel size of a PNG (IHDR) or JPEG (SOFn); `None` if it cannot be read.
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
    if let Some((w, h)) = bitmap_size(content_type, data) {
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

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#036"/></svg>"##;

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
        assert_eq!(
            validate(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 2]).unwrap(),
            "image/jpeg"
        );
        assert_eq!(validate(b"RIFF\0\0\0\0WEBPVP8 ").unwrap(), "image/webp");
        assert_eq!(validate(SVG).unwrap(), "image/svg+xml");
        let with_xml = [b"<?xml version=\"1.0\"?>\n".as_slice(), SVG].concat();
        assert_eq!(validate(&with_xml).unwrap(), "image/svg+xml");
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
