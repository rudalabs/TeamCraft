//! Version handshake: does this desktop app fit the server it connects to?
//! Also the header the server knows a local agent by (shared constants) and
//! the choice of the Agent view's backend (company AI or local agent).
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

/// Header on the MCP connection the desktop hands to a local agent (ACP), so
/// the server can name the reason precisely when its administrator turned
/// local agents off (`TEAMCRAFT_LOCAL_AGENTS=off`). Here, in this
/// dependency-free file, so the server's test suite pins it to
/// `LOCAL_AGENT_HEADER` in `src/config.rs`.
pub const LOCAL_AGENT_HEADER: &str = "X-TeamCraft-Client";
/// Value of [`LOCAL_AGENT_HEADER`] on a local agent's MCP calls.
pub const LOCAL_AGENT_HEADER_VALUE: &str = "desktop-agent";

/// Where the desktop's Agent view sends questions. The server's policy
/// (`GET /api/v1/me` → `capabilities`) decides which are available; the
/// user's pick is only a preference among them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentBackend {
    /// The server's assistant (`POST /api/v1/assistant`): the company AI –
    /// the provider key stays on the server.
    Company,
    /// A local agent (ACP) the user configured.
    Local,
    /// Neither: local agents are off and the server has no assistant.
    Off,
}

impl AgentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentBackend::Company => "company",
            AgentBackend::Local => "local",
            AgentBackend::Off => "off",
        }
    }

    /// A backend the user can pick (`company` or `local`).
    pub fn parse(value: &str) -> Option<AgentBackend> {
        match value {
            "company" => Some(AgentBackend::Company),
            "local" => Some(AgentBackend::Local),
            _ => None,
        }
    }
}

/// The Agent view's backend and the ones the user may switch between
/// (company first). The policy always wins over the stored pick: local
/// agents turned off by the administrator leave only the company AI, or
/// nothing ([`AgentBackend::Off`]). With both available the stored pick
/// counts; without one, a set-up local agent, else the company AI.
pub fn choose_backend(
    local_allowed: bool,
    company: bool,
    has_agents: bool,
    stored: Option<AgentBackend>,
) -> (AgentBackend, Vec<AgentBackend>) {
    let mut available = Vec::new();
    if company {
        available.push(AgentBackend::Company);
    }
    if local_allowed {
        available.push(AgentBackend::Local);
    }
    let chosen = match stored {
        Some(pick) if available.contains(&pick) => pick,
        _ if available.len() == 2 && has_agents => AgentBackend::Local,
        _ => available.first().copied().unwrap_or(AgentBackend::Off),
    };
    (chosen, available)
}
