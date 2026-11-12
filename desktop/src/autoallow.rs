//! Which permission requests of a local agent the desktop answers itself:
//! calls of TeamCraft's own **read-only** MCP tools.
//!
//! A local agent asks `session/request_permission` before it runs a tool.
//! For TeamCraft's read-only tools that question adds nothing: the agent
//! calls them with the desktop's token, and the server checks every call
//! against the signed-in user's rights (role, project-group read rights,
//! restricted groups that look non-existent). So [`decide`] answers
//! `allow_once` for exactly those calls – and asks the user for everything
//! else, including everything it is not sure about:
//!
//! * the request must name TeamCraft's MCP server – the name the desktop
//!   itself registers in `session/new` ([`MCP_SERVER_NAME`]) – and a tool
//!   that the server's own `tools/list` marks `readOnlyHint: true`
//!   ([`ReadOnlyTools`], fetched with the user's token for the session);
//! * every name the request carries must agree: one naming another server, a
//!   second TeamCraft tool or a name that cannot be parsed means asking;
//! * the tool's kind must be that of an MCP tool (`other`, or none) in the
//!   request and in the tool call the agent announced before, so a title
//!   such as `mcp__teamcraft__get_item` on a file read or a terminal command
//!   is not taken at its word;
//! * the request must offer `allow_once` – `allow_always` is never picked;
//! * no list (it failed to load, the agent got no TeamCraft server, the user
//!   asks for every tool) means asking.
//!
//! Pure functions over the request's JSON, tested below.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The name under which the desktop hands TeamCraft's MCP server to the
/// agent (`session/new` → `mcpServers[].name`). Agents prefix the server's
/// tools with it (`mcp__teamcraft__get_item`).
pub const MCP_SERVER_NAME: &str = "teamcraft";

/// Most tool names taken from one `tools/list` answer.
const MAX_TOOLS: usize = 1_000;

/// The read-only tools of TeamCraft's MCP server, by its own `tools/list`
/// (`annotations.readOnlyHint == true`), fetched with the signed-in user's
/// token when the agent session starts – the session belongs to one server
/// and one user, so the list does too.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadOnlyTools {
    names: BTreeSet<String>,
}

impl ReadOnlyTools {
    /// Reads a `tools/list` JSON-RPC response. `None` unless it carries a
    /// `result.tools` list (an error answer, a refusal, anything else).
    /// A name listed twice is left out (its annotations are ambiguous), as
    /// is every tool not marked `readOnlyHint: true` (a plain boolean).
    pub fn from_tools_list(response: &Value) -> Option<ReadOnlyTools> {
        if response.get("error").is_some_and(|e| !e.is_null()) {
            return None;
        }
        let tools = response["result"]["tools"].as_array()?;
        let mut seen: BTreeMap<&str, (usize, bool)> = BTreeMap::new();
        for t in tools.iter().take(MAX_TOOLS) {
            let Some(name) = t["name"].as_str().filter(|n| valid_tool_name(n)) else {
                continue;
            };
            let entry = seen.entry(name).or_insert((0, false));
            entry.0 += 1;
            entry.1 = t["annotations"]["readOnlyHint"].as_bool() == Some(true);
        }
        Some(ReadOnlyTools {
            names: seen
                .into_iter()
                .filter(|(_, (count, read_only))| *count == 1 && *read_only)
                .map(|(name, _)| name.to_string())
                .collect(),
        })
    }

    pub fn contains(&self, tool: &str) -> bool {
        self.names.contains(tool)
    }
}

/// How the desktop answers a permission request.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Show the request to the user (as before).
    Ask,
    /// Answer with this `allow_once` option: a read-only TeamCraft tool.
    Allow {
        /// The `optionId` of the `allow_once` option.
        option_id: String,
        /// The TeamCraft tool, e.g. `get_item`.
        tool: String,
    },
}

/// MCP tool names as TeamCraft uses them: letters, digits, `_`, `-`, and no
/// `__` (the separator agents put between server and tool).
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// What one name in a request says.
#[derive(Debug, PartialEq)]
enum Named {
    /// A tool of the server registered as `server`.
    Ours(String),
    /// A tool of some other server, or a qualified name that does not parse.
    Foreign,
    /// Not a qualified tool name at all (a description, arguments as JSON …).
    Nothing,
}

/// Reads a tool name qualified with its MCP server:
///
/// * `mcp__<server>__<tool>` – Claude Code (also through its ACP adapter, in
///   the title and in `_meta.claudeCode.toolName`) and other agents that
///   follow its convention;
/// * `<tool> (<server> MCP Server)` – Gemini CLI's display name of an MCP tool;
/// * `<server>__<tool>` – Gemini CLI's fully qualified MCP tool name.
///
/// Names are compared exactly (no case folding, no trimming).
fn parse_name(text: &str, server: &str) -> Named {
    if let Some(rest) = text.strip_prefix("mcp__") {
        return match rest.split_once("__") {
            Some((s, tool)) if s == server && valid_tool_name(tool) => Named::Ours(tool.into()),
            _ => Named::Foreign,
        };
    }
    if let Some(head) = text.strip_suffix(" MCP Server)") {
        return match head.rsplit_once(" (") {
            Some((tool, s)) if s == server && valid_tool_name(tool) => Named::Ours(tool.into()),
            _ => Named::Foreign,
        };
    }
    if !text.chars().any(char::is_whitespace) {
        if let Some((s, tool)) = text.split_once("__") {
            return if s == server && valid_tool_name(tool) {
                Named::Ours(tool.into())
            } else {
                Named::Foreign
            };
        }
    }
    Named::Nothing
}

/// The kinds an MCP tool call has: agents report MCP tools as `other`;
/// a missing kind is allowed (the protocol's default is `other`).
fn mcp_kind(kind: &Value) -> bool {
    match kind {
        Value::Null => true,
        Value::String(k) => k == "other",
        _ => false,
    }
}

/// Decides a `session/request_permission` request (`params`): [`Decision::Allow`]
/// only for a read-only tool of the TeamCraft MCP server registered as
/// `server`; see the module documentation for every case that asks.
/// `announced_kind` is the kind of the tool call the agent announced earlier
/// with the same `toolCallId` (`session/update`), if any.
pub fn decide(
    params: &Value,
    server: &str,
    tools: Option<&ReadOnlyTools>,
    announced_kind: Option<&str>,
) -> Decision {
    let Some(tools) = tools else {
        return Decision::Ask;
    };
    let Some(option_id) = params["options"].as_array().and_then(|options| {
        options
            .iter()
            .find(|o| o["kind"].as_str() == Some("allow_once"))
            .and_then(|o| o["optionId"].as_str())
            .map(str::to_string)
    }) else {
        return Decision::Ask;
    };
    let call = &params["toolCall"];
    if !call.is_object() || !mcp_kind(&call["kind"]) {
        return Decision::Ask;
    }
    if announced_kind.is_some_and(|k| k != "other") {
        return Decision::Ask;
    }
    // every place an agent names the tool; all that name one must agree
    let names = [
        &call["title"],
        &call["_meta"]["claudeCode"]["toolName"],
        &call["_meta"]["toolName"],
    ];
    let mut tool: Option<String> = None;
    for name in names {
        let named = match name {
            Value::Null => Named::Nothing,
            Value::String(text) => parse_name(text, server),
            // a name that is not a string: something unexpected
            _ => Named::Foreign,
        };
        match named {
            Named::Nothing => {}
            Named::Foreign => return Decision::Ask,
            Named::Ours(t) => match &tool {
                Some(seen) if *seen != t => return Decision::Ask,
                _ => tool = Some(t),
            },
        }
    }
    match tool {
        Some(tool) if tools.contains(&tool) => Decision::Allow { option_id, tool },
        _ => Decision::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `tools/list` answer like the server's: two read-only tools, one
    /// write tool (draft change), one without annotations.
    fn list() -> ReadOnlyTools {
        ReadOnlyTools::from_tools_list(&json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [
            { "name": "get_item", "annotations": { "readOnlyHint": true } },
            { "name": "search_items", "annotations": { "readOnlyHint": true } },
            { "name": "create_draft_change", "annotations": { "readOnlyHint": false } },
            { "name": "no_annotations" },
        ] } }))
        .expect("a list")
    }

    fn options() -> Value {
        json!([
            { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
            { "optionId": "once", "name": "Allow", "kind": "allow_once" },
            { "optionId": "no", "name": "Reject", "kind": "reject_once" },
        ])
    }

    fn request(tool_call: Value) -> Value {
        json!({ "sessionId": "s1", "toolCall": tool_call, "options": options() })
    }

    fn allow(tool: &str) -> Decision {
        Decision::Allow {
            option_id: "once".into(),
            tool: tool.into(),
        }
    }

    fn decide_with(params: &Value) -> Decision {
        decide(params, MCP_SERVER_NAME, Some(&list()), None)
    }

    #[test]
    fn the_read_only_set_comes_from_the_annotations() {
        let l = list();
        assert!(l.contains("get_item") && l.contains("search_items"));
        assert!(!l.contains("create_draft_change"));
        assert!(!l.contains("no_annotations"));
        assert_eq!(l.names.len(), 2);
        // an error answer, a refusal, garbage: no list at all
        for bad in [
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32600, "message": "off" } }),
            json!({ "jsonrpc": "2.0", "id": 1, "result": {} }),
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": "get_item" } }),
            json!("tools"),
        ] {
            assert_eq!(ReadOnlyTools::from_tools_list(&bad), None, "{bad}");
        }
        // a tool listed twice is ambiguous; a "true" string is no boolean;
        // a name with the server separator is not a TeamCraft tool name
        let odd = ReadOnlyTools::from_tools_list(&json!({ "result": { "tools": [
            { "name": "get_item", "annotations": { "readOnlyHint": true } },
            { "name": "get_item", "annotations": { "readOnlyHint": false } },
            { "name": "get_bom", "annotations": { "readOnlyHint": "true" } },
            { "name": "x__y", "annotations": { "readOnlyHint": true } },
            { "name": "where_used", "annotations": { "readOnlyHint": true } },
        ] } }))
        .unwrap();
        assert!(!odd.contains("get_item"));
        assert!(!odd.contains("get_bom"));
        assert!(!odd.contains("x__y"));
        assert!(odd.contains("where_used"));
    }

    #[test]
    fn claude_code_names_are_recognised() {
        // the ACP adapter: the tool name as title, kind `other`
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "kind": "other", "rawInput": { "item": "PRT-000001" } }),
        );
        assert_eq!(decide_with(&r), allow("get_item"));
        // without a kind, and with the name only in `_meta`
        let r = request(json!({ "toolCallId": "t1", "title": "Get an item",
                                "_meta": { "claudeCode": { "toolName": "mcp__teamcraft__search_items" } } }));
        assert_eq!(decide_with(&r), allow("search_items"));
        // title and `_meta` agree
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "_meta": { "claudeCode": { "toolName": "mcp__teamcraft__get_item" } } }),
        );
        assert_eq!(decide_with(&r), allow("get_item"));
    }

    #[test]
    fn gemini_cli_names_are_recognised() {
        let r = request(
            json!({ "toolCallId": "t1", "title": "get_item (teamcraft MCP Server)",
                                "kind": "other" }),
        );
        assert_eq!(decide_with(&r), allow("get_item"));
        let r = request(
            json!({ "toolCallId": "t1", "title": "teamcraft__search_items",
                                "kind": "other" }),
        );
        assert_eq!(decide_with(&r), allow("search_items"));
        // Gemini's usual title is the call's arguments – no server named: ask
        let r = request(
            json!({ "toolCallId": "t1", "title": "{\"item\":\"PRT-000001\"}",
                                "kind": "other" }),
        );
        assert_eq!(decide_with(&r), Decision::Ask);
    }

    #[test]
    fn generic_agents_name_the_tool_in_meta() {
        let r = request(json!({ "toolCallId": "t1", "title": "Looking up the item",
                                "_meta": { "toolName": "mcp__teamcraft__get_item" } }));
        assert_eq!(decide_with(&r), allow("get_item"));
        // a bare tool name says nothing about the server
        let r = request(json!({ "toolCallId": "t1", "title": "get_item", "kind": "other" }));
        assert_eq!(decide_with(&r), Decision::Ask);
    }

    #[test]
    fn teamcraft_write_tools_are_asked() {
        for title in [
            "mcp__teamcraft__create_draft_change",
            "create_draft_change (teamcraft MCP Server)",
            "mcp__teamcraft__no_annotations",
            "mcp__teamcraft__not_in_the_list",
        ] {
            let r = request(json!({ "toolCallId": "t1", "title": title, "kind": "other" }));
            assert_eq!(decide_with(&r), Decision::Ask, "{title}");
        }
    }

    #[test]
    fn another_servers_tool_of_the_same_name_is_asked() {
        for title in [
            "mcp__plm__get_item",
            "mcp__teamcraft2__get_item",
            "mcp__TeamCraft__get_item",
            "mcp__teamcraft__get_item__x",
            "get_item (other MCP Server)",
            "other__get_item",
            "mcp__get_item",
        ] {
            let r = request(json!({ "toolCallId": "t1", "title": title, "kind": "other" }));
            assert_eq!(decide_with(&r), Decision::Ask, "{title}");
        }
        // the names in one request disagree: another server …
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "_meta": { "claudeCode": { "toolName": "mcp__other__get_item" } } }),
        );
        assert_eq!(decide_with(&r), Decision::Ask);
        // … or another TeamCraft tool
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "_meta": { "claudeCode": { "toolName": "mcp__teamcraft__create_draft_change" } } }),
        );
        assert_eq!(decide_with(&r), Decision::Ask);
        // a server registered under another name matches only that name
        let r = request(json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item" }));
        assert_eq!(decide(&r, "plm", Some(&list()), None), Decision::Ask);
        assert_eq!(
            decide(
                &request(json!({ "toolCallId": "t1", "title": "mcp__plm__get_item" })),
                "plm",
                Some(&list()),
                None
            ),
            allow("get_item")
        );
    }

    #[test]
    fn without_a_list_everything_is_asked() {
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "kind": "other" }),
        );
        assert_eq!(decide(&r, MCP_SERVER_NAME, None, None), Decision::Ask);
        // an empty list (e.g. a server without read-only tools) allows nothing
        let empty = ReadOnlyTools::from_tools_list(&json!({ "result": { "tools": [] } })).unwrap();
        assert_eq!(
            decide(&r, MCP_SERVER_NAME, Some(&empty), None),
            Decision::Ask
        );
    }

    #[test]
    fn without_allow_once_nothing_is_allowed() {
        let call =
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item", "kind": "other" });
        for options in [
            json!([{ "optionId": "always", "kind": "allow_always" },
                   { "optionId": "no", "kind": "reject_once" }]),
            json!([{ "optionId": "no", "kind": "reject_once" }]),
            json!([{ "kind": "allow_once" }]),
            json!([]),
            Value::Null,
        ] {
            let r = json!({ "sessionId": "s1", "toolCall": call, "options": options });
            assert_eq!(decide_with(&r), Decision::Ask, "{options}");
        }
    }

    #[test]
    fn a_spoofed_title_on_another_kind_is_asked() {
        for kind in [
            "read", "edit", "delete", "move", "search", "execute", "think", "fetch",
        ] {
            let r = request(
                json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                    "kind": kind, "rawInput": { "command": "rm -rf ~" } }),
            );
            assert_eq!(decide_with(&r), Decision::Ask, "{kind}");
        }
        let r = request(
            json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                                "kind": 3 }),
        );
        assert_eq!(decide_with(&r), Decision::Ask);
        // the request leaves the kind out, but the announced tool call was a
        // terminal command
        let r = request(json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item" }));
        assert_eq!(
            decide(&r, MCP_SERVER_NAME, Some(&list()), Some("execute")),
            Decision::Ask
        );
        assert_eq!(
            decide(&r, MCP_SERVER_NAME, Some(&list()), Some("other")),
            allow("get_item")
        );
    }

    #[test]
    fn odd_requests_are_asked() {
        for r in [
            json!({ "sessionId": "s1", "options": options() }),
            json!({ "sessionId": "s1", "toolCall": "mcp__teamcraft__get_item", "options": options() }),
            request(json!({ "toolCallId": "t1" })),
            request(json!({ "toolCallId": "t1", "title": 42 })),
            request(
                json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get_item",
                            "_meta": { "toolName": ["mcp__teamcraft__get_item"] } }),
            ),
            request(json!({ "toolCallId": "t1", "title": " mcp__teamcraft__get_item" })),
            request(json!({ "toolCallId": "t1", "title": "mcp__teamcraft__" })),
            request(json!({ "toolCallId": "t1", "title": "mcp__teamcraft__get item" })),
        ] {
            assert_eq!(decide_with(&r), Decision::Ask, "{r}");
        }
    }
}
