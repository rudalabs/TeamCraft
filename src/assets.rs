//! Static files embedded into the binary – stylesheets, scripts, logos and
//! fonts – and how they are served.
//!
//! Two sources: the explicit list below (`static/`, `media/`: files of the web
//! UI) and `shared/`, the files the web UI shares with the desktop app.
//! `build.rs` walks `shared/` and writes its table into `OUT_DIR`; every file
//! there is served at `/static/shared/<path>` (README, tests and test vectors
//! are skipped – the same rule as in `desktop/build.rs`).
//!
//! Cache busting: pages link every asset with `?v=<assets version>`, a hash
//! over all embedded files. A request with the current version may be cached
//! for a year (`immutable`); any other request – an outdated link, a font
//! referenced from CSS – has to be revalidated (`no-cache` + `ETag`). So a
//! browser never shows an old stylesheet or logo after the server was updated.
//!
//! Shared scripts are linked through a versioned path instead,
//! `/static/shared/v/<assets version>/js/…`: ES modules import each other with
//! relative URLs, which drop a `?v=` query but stay inside the versioned
//! directory. The same rules apply – the current version is `immutable`, any
//! other version is still served, with `no-cache`, so an outdated page works.

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
const TXT: &str = "text/plain; charset=utf-8";

/// URL prefix of the files from `shared/`.
pub const SHARED_PREFIX: &str = "/static/shared/";

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

/// Files of the web UI only.
static LISTED: &[Asset] = assets![
    "/static/style.css" => "static/style.css", CSS;
    "/static/app.js" => "static/app.js", JS;
    "/static/media/logos/TeamCraft_Icon.svg" => "media/logos/TeamCraft_Icon.svg", SVG;
    "/static/media/logos/TeamCraft_Logo_Blue.svg" => "media/logos/TeamCraft_Logo_Blue.svg", SVG;
    "/static/media/logos/TeamCraft_Logo_White.svg" => "media/logos/TeamCraft_Logo_White.svg", SVG;
    "/static/media/icons/Green_tick.svg" => "media/icons/Green_tick.svg", SVG;
    "/static/media/icons/Red_cross.svg" => "media/icons/Red_cross.svg", SVG;
    "/static/media/icons/White_plus.svg" => "media/icons/White_plus.svg", SVG;
    "/static/media/icons/Yellow_circle.svg" => "media/icons/Yellow_circle.svg", SVG;
];

/// Files from `shared/`, listed by `build.rs`.
const SHARED: &[Asset] = include!(concat!(env!("OUT_DIR"), "/shared_assets.rs"));
const _: () = assert!(!SHARED.is_empty(), "build.rs found no files in shared/");

/// Every embedded asset.
pub fn all() -> impl Iterator<Item = &'static Asset> {
    LISTED.iter().chain(SHARED.iter())
}

/// URL of the favicon (the TeamCraft icon).
pub const FAVICON: &str = "/static/media/logos/TeamCraft_Icon.svg";

fn hashes() -> &'static (String, Vec<String>) {
    static HASHES: OnceLock<(String, Vec<String>)> = OnceLock::new();
    HASHES.get_or_init(|| {
        let mut whole = Sha256::new();
        let per_asset = all()
            .map(|a| {
                whole.update(a.url.as_bytes());
                whole.update(a.body);
                hex::encode(&Sha256::digest(a.body)[..8])
            })
            .collect();
        (hex::encode(&whole.finalize()[..6]), per_asset)
    })
}

/// Version of all embedded assets (12 hex digits), used as `?v=` in links
/// and in the versioned path of shared scripts.
pub fn version() -> &'static str {
    &hashes().0
}

pub fn find(url: &str) -> Option<&'static Asset> {
    all().find(|a| a.url == url)
}

/// The asset for a request path, and whether the request names the current
/// version (`?v=<version>`, or the path `/static/shared/v/<version>/…`).
pub fn resolve(path: &str, query: Option<&str>) -> Option<(&'static Asset, bool)> {
    let versioned = path
        .strip_prefix(SHARED_PREFIX)
        .and_then(|rest| rest.strip_prefix("v/"))
        .and_then(|rest| rest.split_once('/'));
    if let Some((v, rest)) = versioned {
        let asset = find(&format!("{SHARED_PREFIX}{rest}"))?;
        return Some((asset, v == version()));
    }
    let current = query.is_some_and(|q| q.split('&').any(|p| p == format!("v={}", version())));
    find(path).map(|asset| (asset, current))
}

/// The asset as a response, honouring `If-None-Match` and the cache rules
/// described at the top of this module (`current`: see [`resolve`]).
pub fn respond(asset: &'static Asset, current: bool, headers: &HeaderMap) -> Response {
    let index = all().position(|a| std::ptr::eq(a, asset)).unwrap_or(0);
    let etag = format!("\"{}\"", hashes().1[index]);
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
        let mut urls: Vec<_> = all().map(|a| a.url).collect();
        let count = urls.len();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), count, "unique URLs");
        assert!(find("/static/style.css").is_some());
        assert!(find("/static/shared/css/skins.css").is_some());
        assert!(find("/static/shared/js/viewer.js").is_some());
        assert!(find(FAVICON).is_some());
        assert!(find("/static/../Cargo.toml").is_none());
        assert!(find("/static/shared/../../Cargo.toml").is_none());
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
        let (asset, current) =
            resolve("/static/style.css", Some(&format!("v={}", version()))).unwrap();
        assert!(current);
        let fresh = respond(asset, current, &HeaderMap::new());
        assert_eq!(fresh.status(), StatusCode::OK);
        assert!(fresh.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable"));
        let (asset, current) = resolve("/static/style.css", Some("v=0.1.0")).unwrap();
        assert!(!current);
        let old = respond(asset, current, &HeaderMap::new());
        assert_eq!(old.headers()[header::CACHE_CONTROL], "no-cache");
        let etag = old.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let again = respond(asset, false, &headers);
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
    }

    /// Shared scripts under `/static/shared/v/<version>/…`: the current
    /// version is immutable, any other version is served for revalidation.
    #[test]
    fn versioned_shared_paths() {
        let viewer = find("/static/shared/js/viewer.js").unwrap();
        let path = format!("/static/shared/v/{}/js/viewer.js", version());
        let (asset, current) = resolve(&path, None).unwrap();
        assert!(std::ptr::eq(asset, viewer) && current);
        let (asset, current) = resolve("/static/shared/v/000000000000/js/viewer.js", None).unwrap();
        assert!(std::ptr::eq(asset, viewer) && !current);
        let (_, current) = resolve("/static/shared/js/viewer.js", None).unwrap();
        assert!(!current);
        assert!(resolve("/static/shared/v/x/js/missing.js", None).is_none());
        assert!(resolve("/static/shared/v/x/../../style.css", None).is_none());
        // only shared files have a versioned path
        assert!(resolve(
            &format!("/static/shared/v/{}/../style.css", version()),
            None
        )
        .is_none());
        assert_eq!(viewer.content_type, JS);
    }

    /// Every file of a directory below the repository root, recursively,
    /// as `/`-separated paths relative to it.
    fn files_below(dir: &str) -> Vec<String> {
        fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let rel = path.strip_prefix(root).unwrap().to_str().unwrap();
                    out.push(rel.replace('\\', "/"));
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        out.sort();
        out
    }

    /// The README, tests and test vectors of `shared/` stay in the repository
    /// (same rule as `build.rs` and `desktop/build.rs`).
    fn shared_servable(rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        !(rel.starts_with("test-vectors/")
            || name == "README.md"
            || name.ends_with(".test.js")
            || name.starts_with('.'))
    }

    /// The server embeds exactly the servable files of `shared/`, byte for
    /// byte, at `/static/shared/<path>` (the desktop app has the mirror test
    /// for its copy in `desktop/src/lib.rs`).
    #[test]
    fn shared_files_are_served() {
        let expected: Vec<String> = files_below("shared")
            .into_iter()
            .filter(|rel| shared_servable(rel))
            .collect();
        assert!(expected.contains(&"css/skins.css".to_string()));
        let mut served: Vec<String> = all()
            .filter_map(|a| a.url.strip_prefix(SHARED_PREFIX))
            .map(str::to_string)
            .collect();
        served.sort();
        assert_eq!(served, expected, "shared/ and the embedded table differ");
        for rel in &expected {
            let disk =
                std::fs::read(format!("{}/shared/{rel}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            assert_eq!(
                find(&format!("{SHARED_PREFIX}{rel}")).unwrap().body,
                disk,
                "{rel}"
            );
        }
    }

    /// Files in `shared/` are copied into another program's bundle, so every
    /// stylesheet, script and SVG names its licence in the first line.
    #[test]
    fn shared_files_carry_spdx_header() {
        let mut checked = 0;
        for rel in files_below("shared") {
            if !(rel.ends_with(".css") || rel.ends_with(".js") || rel.ends_with(".svg")) {
                continue;
            }
            let text =
                std::fs::read_to_string(format!("{}/shared/{rel}", env!("CARGO_MANIFEST_DIR")))
                    .unwrap();
            let first = text.lines().next().unwrap_or("");
            let ok = if rel.ends_with(".svg") {
                first.contains("<!-- SPDX-License-Identifier: MPL-2.0 -->")
            } else {
                first == "/* SPDX-License-Identifier: MPL-2.0 */"
            };
            assert!(
                ok,
                "shared/{rel}: the first line must be the SPDX MPL-2.0 header"
            );
            checked += 1;
        }
        assert!(checked >= 2);
    }

    /// Every design token the stylesheets use without a fallback is declared
    /// somewhere (skins.css, style.css, or the desktop app.css that loads
    /// skins.css): an undefined `var(--x)` silently drops the declaration.
    #[test]
    fn css_tokens_are_defined() {
        let sheets = [
            (
                "shared/css/skins.css",
                include_str!("../shared/css/skins.css"),
            ),
            ("static/style.css", include_str!("../static/style.css")),
            ("desktop/ui/app.css", include_str!("../desktop/ui/app.css")),
        ];
        let declared: std::collections::HashSet<&str> = sheets
            .iter()
            .flat_map(|(_, css)| css.match_indices("--").map(|(start, _)| &css[start..]))
            .filter_map(|s| {
                let (name, rest) = custom_property(s)?;
                rest.trim_start().starts_with(':').then_some(name)
            })
            .collect();
        assert!(declared.contains("--radius-sm") && declared.contains("--primary-from"));
        for (file, css) in sheets {
            for (start, _) in css.match_indices("var(") {
                let after = &css[start + 4..];
                let Some((name, rest)) = custom_property(after) else {
                    continue;
                };
                if rest.trim_start().starts_with(',') {
                    continue; // has a fallback
                }
                assert!(declared.contains(name), "{file}: {name} is not declared");
            }
        }
    }

    /// One set of density tokens (shared/css/skins.css): the desktop app
    /// selects the compact set instead of declaring its own control and row
    /// heights or body text size.
    #[test]
    fn one_density_token_set() {
        let skins = include_str!("../shared/css/skins.css");
        let compact = skins
            .split(":root[data-density=\"compact\"] {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("compact density block");
        for token in [
            "--fs-body",
            "--lh-body",
            "--ctl-h:",
            "--ctl-h-sm",
            "--row-h:",
        ] {
            assert!(compact.contains(token), "compact density sets {token}");
        }
        let app_css = include_str!("../desktop/ui/app.css");
        for own in ["--d-ctl", "--d-row", "--ctl-h:", "--row-h:", "--fs-body:"] {
            assert!(!app_css.contains(own), "desktop/ui/app.css declares {own}");
        }
        let index = include_str!("../desktop/ui/index.html");
        assert!(
            index.contains("<html lang=\"en\" data-skin=\"teamcraft\" data-density=\"compact\">")
        );
    }

    /// `--name` at the start of `s` and the text after it.
    fn custom_property(s: &str) -> Option<(&str, &str)> {
        let body = s.strip_prefix("--")?;
        let len = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(body.len());
        (len > 0).then(|| (&s[..len + 2], &body[len..]))
    }

    /// SVGs must scale (viewBox) and must not contain active content.
    #[test]
    fn media_svgs_scale_and_are_inert() {
        for asset in all().filter(|a| a.content_type == SVG) {
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
