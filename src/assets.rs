//! Static files embedded into the binary – stylesheets, scripts, logos and
//! fonts – and how they are served.
//!
//! Cache busting: pages link every asset with `?v=<assets version>`, a hash
//! over all embedded files. A request with the current version may be cached
//! for a year (`immutable`); any other request – an outdated link, a font
//! referenced from CSS – has to be revalidated (`no-cache` + `ETag`). So a
//! browser never shows an old stylesheet or logo after the server was updated.

use std::sync::OnceLock;

use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml";
const WOFF2: &str = "font/woff2";

/// One embedded file and the URL path it is served at.
pub struct Asset {
    pub url: &'static str,
    pub content_type: &'static str,
    pub body: &'static [u8],
}

macro_rules! assets {
    ($( $url:literal => $file:literal, $ct:expr; )*) => {
        &[$( Asset { url: $url, content_type: $ct, body: include_bytes!(concat!("../", $file)) } ),*]
    };
}

pub static ASSETS: &[Asset] = assets![
    "/static/skins.css" => "static/skins.css", CSS;
    "/static/style.css" => "static/style.css", CSS;
    "/static/app.js" => "static/app.js", JS;
    "/static/viewer.js" => "static/viewer.js", JS;
    "/static/fonts/montserrat-latin-wght-normal.woff2" => "static/fonts/montserrat-latin-wght-normal.woff2", WOFF2;
    "/static/fonts/montserrat-latin-ext-wght-normal.woff2" => "static/fonts/montserrat-latin-ext-wght-normal.woff2", WOFF2;
    "/static/fonts/oswald-latin-wght-normal.woff2" => "static/fonts/oswald-latin-wght-normal.woff2", WOFF2;
    "/static/fonts/oswald-latin-ext-wght-normal.woff2" => "static/fonts/oswald-latin-ext-wght-normal.woff2", WOFF2;
    "/static/media/logos/TeamCraft_Icon.svg" => "media/logos/TeamCraft_Icon.svg", SVG;
    "/static/media/logos/TeamCraft_Logo_Blue.svg" => "media/logos/TeamCraft_Logo_Blue.svg", SVG;
    "/static/media/logos/TeamCraft_Logo_White.svg" => "media/logos/TeamCraft_Logo_White.svg", SVG;
    "/static/media/icons/Green_tick.svg" => "media/icons/Green_tick.svg", SVG;
    "/static/media/icons/Red_cross.svg" => "media/icons/Red_cross.svg", SVG;
    "/static/media/icons/White_plus.svg" => "media/icons/White_plus.svg", SVG;
    "/static/media/icons/Yellow_circle.svg" => "media/icons/Yellow_circle.svg", SVG;
];

/// URL of the favicon (the TeamCraft icon).
pub const FAVICON: &str = "/static/media/logos/TeamCraft_Icon.svg";

fn hashes() -> &'static (String, Vec<String>) {
    static HASHES: OnceLock<(String, Vec<String>)> = OnceLock::new();
    HASHES.get_or_init(|| {
        let mut all = Sha256::new();
        let per_asset = ASSETS
            .iter()
            .map(|a| {
                all.update(a.url.as_bytes());
                all.update(a.body);
                hex::encode(&Sha256::digest(a.body)[..8])
            })
            .collect();
        (hex::encode(&all.finalize()[..6]), per_asset)
    })
}

/// Version of all embedded assets (12 hex digits), used as `?v=` in links.
pub fn version() -> &'static str {
    &hashes().0
}

pub fn find(url: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.url == url)
}

/// The asset as a response, honouring `If-None-Match` and the cache rules
/// described at the top of this module.
pub fn respond(asset: &'static Asset, query: Option<&str>, headers: &HeaderMap) -> Response {
    let index = ASSETS
        .iter()
        .position(|a| std::ptr::eq(a, asset))
        .unwrap_or(0);
    let etag = format!("\"{}\"", hashes().1[index]);
    let current = query
        .map(|q| q.split('&').any(|p| p == format!("v={}", version())))
        .unwrap_or(false);
    let cache = if current {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            )],
            asset.body,
        )
            .into_response()
    };
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_lookup() {
        assert_eq!(version().len(), 12);
        assert!(version().chars().all(|c| c.is_ascii_hexdigit()));
        let mut urls: Vec<_> = ASSETS.iter().map(|a| a.url).collect();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), ASSETS.len(), "unique URLs");
        assert!(find("/static/style.css").is_some());
        assert!(find(FAVICON).is_some());
        assert!(find("/static/../Cargo.toml").is_none());
        // every skin logo is embedded
        for skin in crate::domain::SKINS {
            assert!(
                find(&format!("/static/media/{}", skin.logo)).is_some(),
                "{}",
                skin.logo
            );
        }
    }

    #[test]
    fn cache_rules() {
        let asset = find("/static/style.css").unwrap();
        let fresh = respond(asset, Some(&format!("v={}", version())), &HeaderMap::new());
        assert_eq!(fresh.status(), StatusCode::OK);
        assert!(fresh.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable"));
        let old = respond(asset, Some("v=0.1.0"), &HeaderMap::new());
        assert_eq!(old.headers()[header::CACHE_CONTROL], "no-cache");
        let etag = old.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let again = respond(asset, None, &headers);
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
    }

    /// SVGs must scale (viewBox) and must not contain active content.
    #[test]
    fn media_svgs_scale_and_are_inert() {
        for asset in ASSETS.iter().filter(|a| a.content_type == SVG) {
            let svg = std::str::from_utf8(asset.body).expect("SVG is UTF-8");
            let root = svg
                .find("<svg")
                .map(|start| &svg[start..])
                .and_then(|s| s.split_once('>'))
                .map(|(tag, _)| tag)
                .unwrap_or_else(|| panic!("{} has no <svg> element", asset.url));
            assert!(
                root.contains("viewBox"),
                "{}: without a viewBox the icon does not scale in every browser",
                asset.url
            );
            let lower = svg.to_ascii_lowercase();
            for active in [
                "<script",
                "javascript:",
                "onload=",
                "onclick=",
                "<foreignobject",
            ] {
                assert!(!lower.contains(active), "{} contains {active}", asset.url);
            }
        }
    }
}
