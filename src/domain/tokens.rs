//! Scopes of personal access tokens (threat model F2).
//!
//! A scope narrows what a token may do; it never widens the rights of the
//! token's user. The REST API and the MCP endpoint enforce it centrally in
//! the `ApiUser` extractor (`auth::ApiCaller`) and in the MCP dispatcher.

use serde::{Deserialize, Serialize};

/// The path of the MCP endpoint (the only one an `mcp_only` token reaches).
pub const MCP_PATH: &str = "/api/v1/mcp";

/// What a personal access token may do with the rights of its user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// Everything the user may do (REST and MCP, writes included).
    #[default]
    Full,
    /// Only reads: `GET`/`HEAD` requests and the read-only MCP tools.
    ReadOnly,
    /// Only the MCP endpoint (`POST /api/v1/mcp`), with all its tools.
    McpOnly,
}

impl TokenScope {
    pub const ALL: [TokenScope; 3] = [TokenScope::Full, TokenScope::ReadOnly, TokenScope::McpOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            TokenScope::Full => "full",
            TokenScope::ReadOnly => "read_only",
            TokenScope::McpOnly => "mcp_only",
        }
    }

    /// Accepts the stored names and their spellings with a dash.
    pub fn parse(s: &str) -> Option<TokenScope> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "full" => Some(TokenScope::Full),
            "read_only" => Some(TokenScope::ReadOnly),
            "mcp_only" => Some(TokenScope::McpOnly),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TokenScope::Full => "full access",
            TokenScope::ReadOnly => "read-only",
            TokenScope::McpOnly => "MCP only",
        }
    }

    /// Whether a request with this HTTP method to this path (the full path,
    /// e.g. `/api/v1/items`) is within the scope. A `read_only` token may
    /// post to the MCP endpoint; which tools it may call is checked there
    /// ([`TokenScope::allows_tool`]).
    pub fn allows_request(self, method: &str, path: &str) -> bool {
        let read = matches!(method, "GET" | "HEAD");
        let mcp = method == "POST" && path == MCP_PATH;
        match self {
            TokenScope::Full => true,
            TokenScope::ReadOnly => read || mcp,
            TokenScope::McpOnly => mcp,
        }
    }

    /// Whether an MCP tool may be called (`read_only` = the tool only reads).
    pub fn allows_tool(self, read_only: bool) -> bool {
        self != TokenScope::ReadOnly || read_only
    }

    /// Why a request outside the scope was refused (403).
    pub fn refusal(self) -> &'static str {
        match self {
            TokenScope::Full => "",
            TokenScope::ReadOnly => {
                "This token is read-only: it may send GET requests and call the read-only MCP tools."
            }
            TokenScope::McpOnly => "This token may only call the MCP endpoint (POST /api/v1/mcp).",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_round_trip() {
        for s in TokenScope::ALL {
            assert_eq!(TokenScope::parse(s.as_str()), Some(s));
        }
        assert_eq!(TokenScope::parse(" Read-Only "), Some(TokenScope::ReadOnly));
        assert_eq!(TokenScope::parse("mcp-only"), Some(TokenScope::McpOnly));
        for bad in ["", "read", "admin", "write"] {
            assert_eq!(TokenScope::parse(bad), None, "{bad:?}");
        }
        assert_eq!(TokenScope::default(), TokenScope::Full);
    }

    #[test]
    fn full_tokens_reach_everything() {
        for (m, p) in [
            ("GET", "/api/v1/items"),
            ("POST", "/api/v1/items"),
            ("DELETE", "/api/v1/tokens/x"),
            ("POST", MCP_PATH),
        ] {
            assert!(TokenScope::Full.allows_request(m, p), "{m} {p}");
        }
        assert!(TokenScope::Full.allows_tool(false));
    }

    #[test]
    fn read_only_tokens_only_read() {
        let s = TokenScope::ReadOnly;
        assert!(s.allows_request("GET", "/api/v1/items"));
        assert!(s.allows_request("HEAD", "/api/v1/files/x/content"));
        assert!(s.allows_request("POST", MCP_PATH));
        for (m, p) in [
            ("POST", "/api/v1/items"),
            ("PATCH", "/api/v1/items/PRT-000001"),
            ("PUT", "/api/v1/projects/ROVER/visibility"),
            ("DELETE", "/api/v1/tokens/x"),
            ("POST", "/api/v1/assistant"),
            ("POST", "/api/v1/mcp/"),
            ("OPTIONS", "/api/v1/items"),
        ] {
            assert!(!s.allows_request(m, p), "{m} {p}");
        }
        assert!(s.allows_tool(true));
        assert!(!s.allows_tool(false));
    }

    #[test]
    fn mcp_only_tokens_only_reach_the_mcp_endpoint() {
        let s = TokenScope::McpOnly;
        assert!(s.allows_request("POST", MCP_PATH));
        for (m, p) in [
            ("GET", "/api/v1/items"),
            ("GET", "/api/v1/me"),
            ("GET", MCP_PATH),
            ("POST", "/api/v1/items"),
            ("POST", "/api/v1/assistant"),
        ] {
            assert!(!s.allows_request(m, p), "{m} {p}");
        }
        assert!(s.allows_tool(true));
        assert!(s.allows_tool(false));
    }
}
