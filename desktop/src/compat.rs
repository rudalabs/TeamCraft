//! Version handshake: does this desktop app fit the server it connects to?
//!
//! Before 1.0 a minor version may break the REST API, so app and server must
//! share major and minor version; patch versions never matter. From 1.0 on the
//! API only grows within a major version: a server with the same major and the
//! same or a newer minor works, an older minor may lack endpoints the app
//! uses. Pure and without dependencies, so the server's test suite includes
//! this file too (`tests/desktop_compat.rs`).

/// `major.minor` of a version string such as `0.2.0`, `1.4.2-rc.1` or `v0.3`.
pub fn parse(version: &str) -> Option<(u64, u64)> {
    let v = version.trim().trim_start_matches('v');
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor_part = parts.next()?;
    let digits: String = minor_part
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, digits.parse().ok()?))
}

/// A message for the user when app and server do not fit; `None` = fine.
/// An unreadable server version is not an error (a proxy may hide it).
pub fn notice(app: &str, server: &str) -> Option<String> {
    let (a, s) = (parse(app)?, parse(server)?);
    let fits = if a.0 == 0 || s.0 == 0 {
        a == s
    } else {
        a.0 == s.0 && s.1 >= a.1
    };
    if fits {
        return None;
    }
    let advice = if s < a {
        "Ask the administrator to upgrade the server, or install an older desktop app."
    } else {
        "Install the matching desktop app version from the project's releases."
    };
    Some(format!(
        "This desktop app is version {app}, the server runs {server}. They may not work together. {advice}"
    ))
}
