//! Update notice: is a newer TeamCraft release out?
//!
//! At most once a day (and only while the user leaves it switched on in the
//! settings) the app asks the GitHub releases API for the latest release and
//! compares its version with its own. A newer one shows a notice with a link
//! to the release page – nothing is downloaded or installed by the app.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The latest published release (drafts and pre-releases are never "latest").
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/rudalabs/TeamCraft/releases/latest";
/// What the notice links to.
pub const RELEASES_PAGE: &str = "https://github.com/rudalabs/TeamCraft/releases/latest";
/// Time between two checks.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const TIMEOUT: Duration = Duration::from_secs(10);

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A check is due a day after the last one (or when the clock went back).
pub fn due(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        Some(last) => now < last || now - last >= CHECK_INTERVAL_SECS,
    }
}

/// `major.minor.patch` and whether it is a pre-release (`1.2.0-rc.1`);
/// `None` for anything else.
fn parse(version: &str) -> Option<([u64; 3], bool)> {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, !pre.is_empty()),
        None => (version, false),
    };
    let core = core.split('+').next()?;
    let mut parts = core.split('.');
    let mut numbers = [0u64; 3];
    for n in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *n = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((numbers, pre))
}

/// The version of a release tag (`v0.3.0` → `0.3.0`) if it is a plain
/// version; anything else from the network is ignored.
pub fn tag_version(tag: &str) -> Option<String> {
    let version = tag.trim().trim_start_matches('v');
    let plain = version.len() <= 32
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+');
    (plain && parse(version).is_some()).then(|| version.to_string())
}

/// Is `latest` newer than `current`? A release is newer than its own
/// pre-releases (`0.3.0` > `0.3.0-rc.1`); unreadable versions never are.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some((l, l_pre)), Some((c, c_pre))) => l > c || (l == c && c_pre && !l_pre),
        _ => false,
    }
}

/// Asks GitHub for the latest release version; `Ok(None)` when there is no
/// (readable) release yet.
pub async fn latest_release() -> Result<Option<String>, String> {
    // its own client: a short timeout and no redirects (a moved repository
    // shows no notice rather than following an answer to another host)
    let client = reqwest::Client::builder()
        .user_agent(concat!("TeamCraft-Desktop/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // no release published yet
    }
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let release: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    if release["draft"] == true || release["prerelease"] == true {
        return Ok(None);
    }
    Ok(release["tag_name"].as_str().and_then(tag_version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.0"), "numeric, not text order");
        assert!(is_newer("0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("0.3.0-rc.1", "0.3.0"));
        assert!(!is_newer("garbage", "0.2.0"));
        assert!(!is_newer("0.3", "0.2.0"));
        assert!(!is_newer("0.3.0.1", "0.2.0"));
    }

    #[test]
    fn tags() {
        assert_eq!(tag_version("v0.3.0").as_deref(), Some("0.3.0"));
        assert_eq!(tag_version("1.2.3-rc.1").as_deref(), Some("1.2.3-rc.1"));
        assert_eq!(tag_version("v1.2"), None);
        assert_eq!(tag_version("v1.2.3<script>"), None);
        assert_eq!(tag_version(&format!("v1.2.3-{}", "x".repeat(40))), None);
        assert_eq!(tag_version("nightly"), None);
    }

    #[test]
    fn at_most_daily() {
        let day = CHECK_INTERVAL_SECS;
        assert!(due(None, 1_000));
        assert!(!due(Some(1_000), 1_000 + day - 1));
        assert!(due(Some(1_000), 1_000 + day));
        assert!(due(Some(1_000 + day), 1_000), "the clock went back");
    }
}
