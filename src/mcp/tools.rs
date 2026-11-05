//! The MCP tools and prompts of TeamCraft.
//!
//! Every tool calls the same `services::*` functions as the REST API, with
//! the rights of the signed-in user. Results are compact JSON (item numbers
//! and revision codes instead of ids) so they fit an assistant's context;
//! long lists are capped and flagged with `truncated`.
//!
//! Writes only prepare work: a draft change order and its affected
//! revisions, a new change request (which every signed-in user may raise)
//! and comments on one. Submitting, reviewing, releasing, making obsolete
//! and triaging or deciding change requests are not offered – those
//! decisions stay with people.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Map, Value};

use crate::{
    auth::Actor,
    domain::{
        self, ChangeStatus, ItemType, LifecycleState, Priority, RequestReason, RequestStatus,
        RevisionRule, Role,
    },
    error::{AppError, AppResult},
    models::{BomLine, Change, ChangeRequest, FileRecord, Revision},
    services::{
        audit, bom, changes, files, impacts, insights, interchange, items, notifications, projects,
        references, requests, reviewers, revisions, similar, simulations, Page,
    },
    AppState,
};

/// Most rows of an expanded BOM in one result.
pub const MAX_BOM_ROWS: usize = 2000;
/// Most entries of other lists in one result.
pub const MAX_LIST: usize = 500;
/// Upper bound for the text of one result; longer output is cut.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Most item references in one `create_change_draft` / `add_to_change` call.
const MAX_CHANGE_ITEMS: usize = 100;
/// Most simulations in one `list_simulations` result without `item` (each
/// costs two queries).
const MAX_SIMULATIONS: usize = 50;
/// Newest comments of a change request in one result.
const MAX_COMMENTS: usize = 100;
/// Longest comment and request description (as the services allow).
const MAX_COMMENT_CHARS: usize = 10_000;
const MAX_REQUEST_TEXT: usize = 20_000;
/// History entries of a change request in one result.
const REQUEST_HISTORY: i64 = 50;
/// Open change requests and simulations shown with an item or revision.
const MAX_RELATED: usize = 50;

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Only reads.
    Read,
    /// Creates or adds something; never deletes or overwrites.
    Write { idempotent: bool },
}

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    effect: Effect,
    schema: fn() -> Value,
}

const READ: Effect = Effect::Read;

const TOOLS: &[Tool] = &[
    Tool {
        name: "search_items",
        title: "Search items",
        description: "Find items (parts, assemblies, documents, software) by number, name, description, \
            material, file name or file content. Each hit carries its latest revision and state and the \
            newest released revision.",
        effect: READ,
        schema: schema_search_items,
    },
    Tool {
        name: "get_item",
        title: "Item and its revisions",
        description: "Master data of an item and all its revisions (newest first) with state, check-out \
            and release date.",
        effect: READ,
        schema: schema_item,
    },
    Tool {
        name: "get_revision",
        title: "Revision details",
        description: "Everything about one revision: attributes, files (format, version, size, uploader), \
            single-level BOM lines, item references (drawings, specifications), references found inside its \
            CAD files matched to items, who references it, and the change orders it is part of.",
        effect: READ,
        schema: schema_revision,
    },
    Tool {
        name: "get_bom",
        title: "Expanded BOM",
        description: "The multi-level BOM below a revision as flat rows (level, find number, item, \
            revision, quantity) with mass and cost roll-ups. Revision rule: latest, released or as_released \
            (default: as_released for released revisions, latest otherwise).",
        effect: READ,
        schema: schema_bom,
    },
    Tool {
        name: "where_used",
        title: "Where used",
        description: "The parent revisions whose BOM uses an item, with quantity and the revision they were \
            released with. `levels` > 1 walks further up (parents of parents); every row names its level \
            and the item used on that line (`via`).",
        effect: READ,
        schema: schema_where_used,
    },
    Tool {
        name: "replace_preview",
        title: "Preview replacing an item in assemblies",
        description: "What replacing an item by another one in every assembly that uses it would do: one row 
            per BOM line of the latest revision of each parent with the action (replace = in work and free, 
            checked_out, no_permission, in_review, needs_revision = released, cycle). Read only – the 
            replacement itself is done by a person on the where-used page.",
        effect: READ,
        schema: schema_replace_preview,
    },
    Tool {
        name: "item_history",
        title: "Item history",
        description: "Audit log of an item and its revisions, newest first: who did what when (check-outs, \
            uploads, BOM edits, releases).",
        effect: READ,
        schema: schema_history,
    },
    Tool {
        name: "list_changes",
        title: "List change orders",
        description: "Change orders, newest first, optionally filtered by status (draft, in_review, \
            rejected, released, cancelled).",
        effect: READ,
        schema: schema_list_changes,
    },
    Tool {
        name: "get_change",
        title: "Change order details",
        description: "A change order with its revisions, reviews, release readiness (blocking unreleased \
            dependencies and warnings) and impact analysis (items that depend on its revisions).",
        effect: READ,
        schema: schema_change,
    },
    Tool {
        name: "suggest_reviewers",
        title: "Suggest reviewers",
        description: "Use when preparing or reviewing a change order to propose who should review it: \
            ranked CAD editors and admins with plain reasons (recent edits of the affected items, reviews of \
            earlier changes on the same items, owners of dependents, project group managers, requesters of \
            linked change requests). Excludes the author, deactivated users and assigned reviewers. Never \
            assigns anybody – the author adds reviewers in TeamCraft.",
        effect: READ,
        schema: schema_suggest_reviewers,
    },
    Tool {
        name: "my_worklist",
        title: "My worklist",
        description: "What is waiting for the signed-in user: own check-outs, reviews waiting for a \
            decision, open impacts (dependents to check) in their project groups, own open change orders, \
            open change requests assigned to or raised by them and the number of unread notifications.",
        effect: READ,
        schema: schema_none,
    },
    Tool {
        name: "bom_health",
        title: "BOM health check",
        description: "Checks every line of the expanded BOM below a revision: unresolved lines, obsolete \
            or unreleased revisions, parts without mass, revisions without files, check-outs, open impacts \
            and cycles. Use rule=released before a release.",
        effect: READ,
        schema: schema_bom,
    },
    Tool {
        name: "find_duplicate_files",
        title: "Find duplicate files",
        description: "Files whose identical content (SHA-256) is stored in the latest revisions of more \
            than one item – copies that drift apart. Optionally only the contents of one revision.",
        effect: READ,
        schema: schema_duplicates,
    },
    Tool {
        name: "find_similar_parts",
        title: "Find similar parts",
        description: "Parts whose 3D model has a similar shape and size – \"do we already have a part like \
            this?\". Use it before creating a new item or a create_change_draft for a new part, to reuse an \
            existing one instead of a near-duplicate; find_duplicate_files finds only identical contents. \
            Compares the revision's STEP or mesh file (or `file`) with the latest revisions of all other \
            items by oriented bounding box extents and a D2 shape distribution; each hit has a score (0–100), \
            its shape and size parts and a one-line explanation. ignore_scale=true compares proportions \
            only (scaled variants). Status `no_shape_file`: the revision has no STEP or mesh file; \
            `pending`: the signature is still being computed, call again shortly.",
        effect: READ,
        schema: schema_similar,
    },
    Tool {
        name: "compare_revisions",
        title: "Compare two revisions",
        description: "What changed between two revisions of an item: attributes, files added, removed or \
            with new content, and BOM lines added, removed or with another quantity or find number. \
            Defaults: to = latest, from = the revision before `to`.",
        effect: READ,
        schema: schema_compare,
    },
    Tool {
        name: "list_change_requests",
        title: "List change requests",
        description: "Engineering change requests (ECR: reported problems and proposed changes before a \
            change order), newest first. Filters: status (a status, `open` = not finished, or `all`), scope \
            (`mine` = raised by the user, `assigned` = assigned to the user) and item (requests naming it).",
        effect: READ,
        schema: schema_list_requests,
    },
    Tool {
        name: "get_change_request",
        title: "Change request details",
        description: "A change request with its description, affected items (with the revision the problem \
            was found in and the latest state), the discussion (newest 100 comments, oldest first), \
            attachments (names and sizes, no contents), the change orders implementing it, its history \
            (newest 50 entries, newest first) and what the signed-in user may do with it (`you_may` – for \
            the user in TeamCraft; the tools cannot triage or decide).",
        effect: READ,
        schema: schema_request,
    },
    Tool {
        name: "list_simulations",
        title: "List simulations",
        description: "Simulations (FEA, CFD, … items of type simulation) with an `out_of_date` reason per \
            basis. With `item`: the simulations whose latest revision is based on that part or assembly, \
            with result and failure counts, whether a check is open and only their bases on that item \
            (get_simulation shows all). Without: simulation items (filter by text and project group, at most \
            50 checked) with their latest revision's bases and pass/fail summary. out_of_date_only=true keeps \
            only those that no longer match their bases (with `item`: their bases on that item).",
        effect: READ,
        schema: schema_list_simulations,
    },
    Tool {
        name: "get_simulation",
        title: "Simulation details",
        description: "One simulation revision: its basis links (item revision and file version it was \
            computed with, role, and why it is out of date: newer release, obsolete basis or newer file \
            version) and its result values with unit, limits, load case, solver and pass/fail.",
        effect: READ,
        schema: schema_revision,
    },
    Tool {
        name: "compare_simulation_results",
        title: "Compare simulation results",
        description: "Result values of two revisions of a simulation matched by name and load case: added, \
            removed, changed (with delta and percent) and unchanged, with pass/fail on both sides. \
            Defaults: to = latest, from = the revision before `to`.",
        effect: READ,
        schema: schema_compare_results,
    },
    Tool {
        name: "create_change_draft",
        title: "Draft a change order",
        description: "Creates a DRAFT change order and adds the given revisions (default: each item's \
            in-work revision) and, unless include_dependencies=false, their unreleased BOM children and \
            referenced items. Returns the release readiness. Never submits: the user adds reviewers and \
            submits in TeamCraft. If an item cannot be added, the draft is cancelled again.",
        effect: Effect::Write { idempotent: false },
        schema: schema_create_change,
    },
    Tool {
        name: "add_to_change",
        title: "Add to a change order",
        description: "Adds revisions (default: each item's in-work revision) to one of the user's draft \
            change orders, optionally with their unreleased dependencies. Returns the release readiness.",
        effect: Effect::Write { idempotent: true },
        schema: schema_add_to_change,
    },
    Tool {
        name: "create_change_request",
        title: "Raise a change request",
        description: "Raises an engineering change request (ECR): a reported problem or proposed \
            improvement on items, before any change order. Every signed-in user may raise one. It starts as \
            `new`; CAD editors of the affected project groups triage, accept or reject it in TeamCraft – this \
            tool never decides. Create it only when the user asks for it.",
        effect: Effect::Write { idempotent: false },
        schema: schema_create_request,
    },
    Tool {
        name: "comment_change_request",
        title: "Comment on a change request",
        description: "Adds a comment to the discussion of a change request in the user's name (the \
            requester, assignee and watchers are notified). Post only text the user asked to post.",
        effect: Effect::Write { idempotent: false },
        schema: schema_comment_request,
    },
];

/// `tools/list`.
pub fn definitions() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            let (read_only, idempotent) = match t.effect {
                Effect::Read => (true, true),
                Effect::Write { idempotent } => (false, idempotent),
            };
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": read_only,
                    "destructiveHint": false,
                    "idempotentHint": idempotent,
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}

pub fn exists(name: &str) -> bool {
    TOOLS.iter().any(|t| t.name == name)
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn p_item() -> Value {
    json!({ "type": "string", "description": "Item number, e.g. PRT-000012" })
}

fn p_revision() -> Value {
    json!({ "type": "string", "description": "Revision code (A, B, …) or `latest` (default)" })
}

fn p_rule() -> Value {
    json!({
        "type": "string",
        "enum": ["latest", "released", "as_released"],
        "description": "Revision rule for the child lines",
    })
}

fn p_limit(default: i64, max: i64) -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": max, "default": default })
}

fn p_items() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "maxItems": MAX_CHANGE_ITEMS,
        "description": "Item numbers, optionally with a revision code: PRT-000012 or PRT-000012/B. \
            Without a code the item's in-work revision is used.",
    })
}

fn schema_none() -> Value {
    object(json!({}), &[])
}

fn schema_search_items() -> Value {
    object(
        json!({
            "q": { "type": "string", "description": "Search text (number, name, material, attribute values, file name or content)" },
            "type": { "type": "string", "enum": ["part", "assembly", "document", "software", "simulation"] },
            "project": { "type": "string", "description": "Project group code, e.g. ROVER" },
            "state": { "type": "string", "enum": ["in_work", "in_review", "released", "obsolete"], "description": "State of the latest revision" },
            "system": { "type": "string", "description": "CAD program with files in the latest revision, e.g. solidworks, nx, creo, inventor, freecad" },
            "class": { "type": "string", "enum": ["make", "purchased", "standard"], "description": "Classification: in-house, purchased or standard part" },
            "attr": { "type": "string", "description": "Custom attribute key (see the attributes of a revision), together with value" },
            "value": { "type": "string", "description": "Attribute value: text contains, yes/no, a number, >=10, <=20 or 10..20; empty = has any value" },
            "limit": p_limit(20, 100),
        }),
        &[],
    )
}

fn schema_item() -> Value {
    object(json!({ "item": p_item() }), &["item"])
}

fn schema_revision() -> Value {
    object(
        json!({ "item": p_item(), "revision": p_revision() }),
        &["item"],
    )
}

fn schema_bom() -> Value {
    object(
        json!({ "item": p_item(), "revision": p_revision(), "rule": p_rule() }),
        &["item"],
    )
}

fn schema_replace_preview() -> Value {
    object(
        json!({
            "item": p_item(),
            "with": { "type": "string", "description": "Item number of the replacement (its successor or an alternate)" },
        }),
        &["item", "with"],
    )
}

fn schema_where_used() -> Value {
    object(
        json!({
            "item": p_item(),
            "levels": p_limit(1, i64::from(items::MAX_WHERE_USED_LEVELS)),
        }),
        &["item"],
    )
}

fn schema_history() -> Value {
    object(
        json!({ "item": p_item(), "limit": p_limit(50, 200) }),
        &["item"],
    )
}

fn schema_list_changes() -> Value {
    object(
        json!({
            "status": { "type": "string", "enum": ["draft", "in_review", "rejected", "released", "cancelled"] },
            "limit": p_limit(20, 100),
        }),
        &[],
    )
}

fn schema_change() -> Value {
    object(
        json!({ "change": { "type": "string", "description": "Change number, e.g. ECO-000004" } }),
        &["change"],
    )
}

fn schema_suggest_reviewers() -> Value {
    object(
        json!({
            "change": { "type": "string", "description": "Change number, e.g. ECO-000004" },
            "limit": p_limit(reviewers::DEFAULT_LIMIT as i64, reviewers::MAX_LIMIT as i64),
        }),
        &["change"],
    )
}

fn schema_duplicates() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Only contents of this item's revision (optional)" },
            "revision": p_revision(),
            "limit": p_limit(100, 1000),
        }),
        &[],
    )
}

fn schema_similar() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Item number, e.g. PRT-000012 (or PRT-000012/B)" },
            "revision": p_revision(),
            "file": { "type": "string", "description": "File name of the 3D model to compare by (default: the STEP file, else the first mesh)" },
            "ignore_scale": { "type": "boolean", "description": "Compare proportions and shape only (default false)" },
            "limit": p_limit(10, 50),
        }),
        &["item"],
    )
}

fn schema_compare() -> Value {
    object(
        json!({
            "item": p_item(),
            "from": { "type": "string", "description": "Older revision code (default: the one before `to`)" },
            "to": { "type": "string", "description": "Newer revision code or `latest` (default)" },
        }),
        &["item"],
    )
}

fn schema_create_change() -> Value {
    object(
        json!({
            "title": { "type": "string", "maxLength": 200 },
            "description": { "type": "string", "description": "Reason and scope of the change" },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "critical"], "default": "normal" },
            "items": p_items(),
            "include_dependencies": { "type": "boolean", "default": true, "description": "Also add unreleased BOM children and referenced items, level by level" },
        }),
        &["title"],
    )
}

fn schema_add_to_change() -> Value {
    object(
        json!({
            "change": { "type": "string", "description": "Number of a draft change order of the user" },
            "items": p_items(),
            "include_dependencies": { "type": "boolean", "default": false },
        }),
        &["change"],
    )
}

/// Status filter of `list_change_requests`: every status plus `open` and `all`.
const REQUEST_STATUS_FILTERS: &[&str] = &[
    "open",
    "all",
    "new",
    "in_triage",
    "accepted",
    "rejected",
    "deferred",
    "implemented",
    "withdrawn",
];

const REQUEST_REASONS: &[&str] = &[
    "defect",
    "improvement",
    "cost",
    "manufacturing",
    "supplier",
    "safety",
    "customer",
    "other",
];

fn p_request() -> Value {
    json!({ "type": "string", "description": "Change request number, e.g. ECR-000004" })
}

fn schema_list_requests() -> Value {
    object(
        json!({
            "status": { "type": "string", "enum": REQUEST_STATUS_FILTERS, "default": "all" },
            "scope": { "type": "string", "enum": ["mine", "assigned"], "description": "mine = raised by me, assigned = assigned to me" },
            "item": { "type": "string", "description": "Only requests that name this item number" },
            "limit": p_limit(20, 100),
        }),
        &[],
    )
}

fn schema_request() -> Value {
    object(json!({ "request": p_request() }), &["request"])
}

fn schema_create_request() -> Value {
    object(
        json!({
            "title": { "type": "string", "maxLength": 200, "description": "The problem in one line" },
            "description": { "type": "string", "maxLength": MAX_REQUEST_TEXT, "description": "Problem, where it was found and the proposed solution" },
            "reason": { "type": "string", "enum": REQUEST_REASONS, "default": "other" },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "critical"], "default": "normal" },
            "due_date": { "type": "string", "format": "date", "description": "Wanted by (YYYY-MM-DD), optional" },
            "items": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": requests::MAX_ITEMS,
                "description": "Affected item numbers, optionally with the revision the problem was found in: \
                    PRT-000012 or PRT-000012/B",
            },
        }),
        &["title"],
    )
}

fn schema_comment_request() -> Value {
    object(
        json!({
            "request": p_request(),
            "body": { "type": "string", "maxLength": MAX_COMMENT_CHARS },
        }),
        &["request", "body"],
    )
}

fn schema_compare_results() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Simulation number, e.g. SIM-000003" },
            "from": { "type": "string", "description": "Older revision code (default: the one before `to`)" },
            "to": { "type": "string", "description": "Newer revision code or `latest` (default)" },
            "changes_only": { "type": "boolean", "default": false, "description": "Leave out unchanged values" },
        }),
        &["item"],
    )
}

fn schema_list_simulations() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Part or assembly number: the simulations based on it" },
            "q": { "type": "string", "description": "Without item: search text for simulation items" },
            "project": { "type": "string", "description": "Without item: project group code" },
            "out_of_date_only": { "type": "boolean", "default": false },
            "limit": p_limit(20, MAX_SIMULATIONS as i64),
        }),
        &[],
    )
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Cuts `s` to at most `max` bytes at a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A successful `CallToolResult`: the value as JSON text and as structured
/// content (non-objects are wrapped in `{"result": …}`). Output above
/// [`MAX_TEXT_BYTES`] is cut and the structured copy left out.
pub fn success(value: Value) -> Value {
    let structured = match value {
        Value::Object(_) => value,
        other => json!({ "result": other }),
    };
    let text = structured.to_string();
    if text.len() > MAX_TEXT_BYTES {
        let text = format!(
            "{}… [output cut at {MAX_TEXT_BYTES} bytes – narrow the request, e.g. with a smaller limit]",
            cut(&text, MAX_TEXT_BYTES)
        );
        return json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    }
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false,
    })
}

/// A failed `CallToolResult`: the assistant sees the message and can react.
pub fn failure(message: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": message.into() }], "isError": true })
}

/// User errors are shown as they are; internal errors are logged and
/// reported, the assistant only gets a generic message.
fn from_error(tool: &str, e: AppError) -> Value {
    if e.status().is_server_error() {
        tracing::error!(tool, error = %e, "MCP tool failed");
        crate::notify::report_error(&format!("MCP tool {tool}: {e}"));
    }
    failure(e.public_message())
}

/// Keeps at most `max` entries; the flag tells whether some were dropped.
fn capped<T>(mut list: Vec<T>, max: usize) -> (Vec<T>, bool) {
    let truncated = list.len() > max;
    list.truncate(max);
    (list, truncated)
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

fn bad(message: impl Into<String>) -> AppError {
    AppError::BadRequest(message.into())
}

/// Typed access to the `arguments` object of a tool call.
pub struct Args<'a>(Option<&'a Map<String, Value>>);

impl<'a> Args<'a> {
    pub fn new(value: &'a Value) -> Self {
        Args(value.as_object())
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.and_then(|m| m.get(key)).filter(|v| !v.is_null())
    }

    /// A trimmed, non-empty string (`None` when missing or empty).
    pub fn opt_str(&self, key: &str) -> AppResult<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => {
                let s = s.trim();
                Ok((!s.is_empty()).then(|| s.to_string()))
            }
            Some(_) => Err(bad(format!("`{key}` must be a string."))),
        }
    }

    pub fn req_str(&self, key: &str) -> AppResult<String> {
        self.opt_str(key)?
            .ok_or_else(|| bad(format!("`{key}` is required.")))
    }

    /// A positive count, clamped to `1..=max`; numeric strings are accepted.
    pub fn limit(&self, key: &str, default: i64, max: i64) -> AppResult<i64> {
        let n = match self.get(key) {
            None => default,
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| bad(format!("`{key}` must be an integer.")))?,
            Some(Value::String(s)) => s
                .trim()
                .parse()
                .map_err(|_| bad(format!("`{key}` must be an integer.")))?,
            Some(_) => return Err(bad(format!("`{key}` must be an integer."))),
        };
        Ok(n.clamp(1, max))
    }

    pub fn flag(&self, key: &str, default: bool) -> AppResult<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(true),
                "false" | "no" | "0" => Ok(false),
                _ => Err(bad(format!("`{key}` must be true or false."))),
            },
            Some(_) => Err(bad(format!("`{key}` must be true or false."))),
        }
    }

    /// A list of strings; a single string counts as a list of one.
    pub fn list(&self, key: &str, max: usize) -> AppResult<Vec<String>> {
        let out: Vec<String> = match self.get(key) {
            None => Vec::new(),
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| bad(format!("`{key}` must be a list of strings.")))
                })
                .collect::<AppResult<_>>()?,
            Some(_) => return Err(bad(format!("`{key}` must be a list of strings."))),
        };
        let out: Vec<String> = out
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if out.len() > max {
            return Err(bad(format!("`{key}` takes at most {max} entries.")));
        }
        Ok(out)
    }

    fn rule(&self) -> AppResult<Option<RevisionRule>> {
        self.opt_str("rule")?
            .map(|r| {
                RevisionRule::parse(&r)
                    .ok_or_else(|| bad("rule must be latest, released or as_released."))
            })
            .transpose()
    }
}

/// `PRT-000012` or `PRT-000012/B` → number and optional revision code.
/// Item numbers never contain a slash.
pub fn parse_item_ref(s: &str) -> (String, Option<String>) {
    match s.trim().split_once('/') {
        Some((number, code)) if !code.trim().is_empty() => {
            (number.trim().to_string(), Some(code.trim().to_string()))
        }
        Some((number, _)) => (number.trim().to_string(), None),
        None => (s.trim().to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// `tools/call`: runs a tool and wraps the outcome as `CallToolResult`.
pub async fn call(st: &AppState, me: &Actor, name: &str, args: &Value) -> Value {
    let a = Args::new(args);
    let result = match name {
        "search_items" => search_items(st, me, &a).await,
        "get_item" => get_item(st, me, &a).await,
        "get_revision" => get_revision(st, me, &a).await,
        "get_bom" => get_bom(st, me, &a).await,
        "where_used" => where_used(st, me, &a).await,
        "replace_preview" => replace_preview(st, me, &a).await,
        "item_history" => item_history(st, me, &a).await,
        "list_changes" => list_changes(st, me, &a).await,
        "get_change" => get_change(st, me, &a).await,
        "suggest_reviewers" => suggest_reviewers(st, me, &a).await,
        "my_worklist" => my_worklist(st, me).await,
        "bom_health" => bom_health(st, me, &a).await,
        "find_duplicate_files" => find_duplicate_files(st, me, &a).await,
        "find_similar_parts" => find_similar_parts(st, me, &a).await,
        "compare_revisions" => compare_revisions(st, me, &a).await,
        "create_change_draft" => create_change_draft(st, me, &a).await,
        "add_to_change" => add_to_change(st, me, &a).await,
        "list_change_requests" => list_change_requests(st, me, &a).await,
        "get_change_request" => get_change_request(st, me, &a).await,
        "create_change_request" => create_change_request(st, me, &a).await,
        "comment_change_request" => comment_change_request(st, me, &a).await,
        "list_simulations" => list_simulations(st, me, &a).await,
        "get_simulation" => get_simulation(st, me, &a).await,
        "compare_simulation_results" => compare_simulation_results(st, me, &a).await,
        _ => Err(bad(format!("Unknown tool `{name}`."))),
    };
    match result {
        Ok(value) => success(value),
        Err(e) => from_error(name, e),
    }
}

// ---------------------------------------------------------------------------
// Compact views
// ---------------------------------------------------------------------------

fn rev_brief(r: &Revision) -> Value {
    json!({
        "item": r.item_number,
        "revision": r.code,
        "name": r.name,
        "type": r.item_type,
        "state": r.state,
        "project": r.project_code,
        "checked_out_by": r.checked_out_by_name,
    })
}

fn rev_full(r: &Revision) -> Value {
    json!({
        "item": r.item_number,
        "revision": r.code,
        "name": r.name,
        "type": r.item_type,
        "unit": r.unit,
        "state": r.state,
        "project": r.project_code,
        "description": r.description,
        "material": r.material,
        "mass_kg": r.mass_kg,
        "unit_cost": r.unit_cost,
        "classification": r.classification,
        "attributes": r.attributes,
        "checked_out_by": r.checked_out_by_name,
        "checked_out_at": r.checked_out_at,
        "created_by": r.created_by_name,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "released_at": r.released_at,
    })
}

fn change_brief(c: &Change) -> Value {
    json!({
        "number": c.number,
        "title": c.title,
        "status": c.status,
        "priority": c.priority,
        "created_by": c.created_by_name,
        "created_at": c.created_at,
        "updated_at": c.updated_at,
        "submitted_at": c.submitted_at,
        "closed_at": c.closed_at,
    })
}

fn file_brief(f: &FileRecord) -> Value {
    let info = f.format_info.as_ref();
    json!({
        "filename": f.filename,
        "format": info.map(|i| i.label),
        "cad_program": info.map(|i| i.system_name),
        "kind": info.map(|i| i.kind_label),
        "version": f.version,
        "size_bytes": f.size_bytes,
        "sha256": f.sha256,
        "uploaded_by": f.uploaded_by_name,
        "uploaded_at": f.uploaded_at,
    })
}

fn bom_line_brief(l: &BomLine) -> Value {
    json!({
        "find_number": l.find_number,
        "item": l.child_number,
        "name": l.child_name,
        "type": l.child_type,
        "quantity": l.quantity,
        "unit": l.child_unit,
        "latest_revision": l.child_latest_code,
        "latest_state": l.child_latest_state,
        "released_with": l.released_child_code,
        "notes": l.notes,
        "restricted": l.restricted,
    })
}

fn web_url(st: &AppState, path: &str) -> String {
    format!("{}{path}", st.config.public_url)
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// A revision by item number and code; `None` or `latest` = the newest.
async fn revision_at(
    st: &AppState,
    me: &Actor,
    number: &str,
    code: Option<&str>,
) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, me, number).await?;
    match code {
        None => items::latest_revision(&st.db, me, item.id).await,
        Some(c) if c.eq_ignore_ascii_case("latest") => {
            items::latest_revision(&st.db, me, item.id).await
        }
        Some(c) => revisions::get_by_code(&st.db, me, item.id, c).await,
    }
}

async fn revision_arg(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Revision> {
    let number = a.req_str("item")?;
    revision_at(st, me, &number, a.opt_str("revision")?.as_deref()).await
}

/// The revision a change order should carry: the given code, or the item's
/// in-work revision.
async fn change_revision(st: &AppState, me: &Actor, reference: &str) -> AppResult<Revision> {
    let (number, code) = parse_item_ref(reference);
    match code {
        Some(code) => revision_at(st, me, &number, Some(&code)).await,
        None => {
            let item = items::get_by_number(&st.db, me, &number).await?;
            items::open_revision(&st.db, me, item.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!(
                        "{} has no in-work revision; revise it first.",
                        item.number
                    ))
                })
        }
    }
}

/// Default rule: as released for released (and obsolete) revisions.
fn default_rule(rev: &Revision) -> RevisionRule {
    if matches!(rev.state.as_str(), "released" | "obsolete") {
        RevisionRule::AsReleased
    } else {
        RevisionRule::Latest
    }
}

// ---------------------------------------------------------------------------
// Read tools
// ---------------------------------------------------------------------------

async fn search_items(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let project_id = match a.opt_str("project")? {
        Some(code) => Some(projects::get_by_code(&st.db, me, &code).await?.id),
        None => None,
    };
    let item_type = a
        .opt_str("type")?
        .map(|t| {
            ItemType::parse(&t).ok_or_else(|| {
                bad(format!(
                    "Unknown item type '{t}' (part, assembly, document, software, simulation)."
                ))
            })
        })
        .transpose()?;
    let state = a
        .opt_str("state")?
        .map(|s| {
            LifecycleState::parse(&s).ok_or_else(|| {
                bad(format!(
                    "Unknown state '{s}' (in_work, in_review, released, obsolete)."
                ))
            })
        })
        .transpose()?;
    let system = match a.opt_str("system")? {
        Some(s) if crate::cadformat::system(&s).is_some() => Some(s),
        Some(s) => return Err(bad(format!("Unknown CAD program '{s}'."))),
        None => None,
    };
    let limit = a.limit("limit", 20, 100)?;
    let filter = items::ItemFilter {
        query: a.opt_str("q")?,
        item_type,
        project_id,
        state,
        system,
        classification: domain::Classification::parse_field(a.opt_str("class")?.as_deref())?,
        attribute: a.opt_str("attr")?,
        attribute_value: a.opt_str("value")?,
    };
    let (rows, total) = items::search(&st.db, me, &filter, Page::new(Some(limit), None)).await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "number": r.number,
                "type": r.item_type,
                "name": r.name,
                "project": r.project_code,
                "latest_revision": r.latest_code,
                "latest_state": r.latest_state,
                "released_revision": r.released_code,
                "matched_file": r.matched_file,
            })
        })
        .collect();
    Ok(json!({
        "total": total,
        "truncated": total > list.len() as i64,
        "items": list,
    }))
}

async fn get_item(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let revs = items::revisions(&st.db, me, item.id).await?;
    let (open_requests, requests_cut) = open_requests_of(st, me, item.id).await?;
    let (based, based_cut) = if item.item_type == ItemType::Simulation.as_str() {
        (Vec::new(), false)
    } else {
        simulations_of(st, me, item.id).await?
    };
    let (alternates, alternate_for, successor, predecessors) = tokio::try_join!(
        interchange::alternates(&st.db, me, item.id),
        interchange::alternate_for(&st.db, me, item.id),
        interchange::successor(&st.db, me, item.id),
        interchange::predecessors(&st.db, me, item.id),
    )?;
    Ok(json!({
        "item": {
            "number": item.number,
            "type": item.item_type,
            "unit": item.unit,
            "project": item.project_code,
            "created_by": item.created_by_name,
            "created_at": item.created_at,
        },
        "revisions": revs.iter().map(|r| json!({
            "revision": r.code,
            "name": r.name,
            "state": r.state,
            "checked_out_by": r.checked_out_by_name,
            "created_at": r.created_at,
            "released_at": r.released_at,
        })).collect::<Vec<_>>(),
        "open_change_requests": open_requests,
        "alternates": alternates.iter().map(|a| linked_brief(a, true)).collect::<Vec<_>>(),
        "alternate_for": alternate_for.iter().map(|a| linked_brief(a, false)).collect::<Vec<_>>(),
        "superseded_by": successor.as_ref().map(|s| linked_brief(s, false)),
        "supersedes": predecessors.iter().map(|p| linked_brief(p, false)).collect::<Vec<_>>(),
        "simulations": based,
        "truncated": requests_cut || based_cut,
        "url": web_url(st, &format!("/items/{}", item.id)),
    }))
}

async fn get_revision(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let (file_list, files_cut) = capped(files::list(&st.db, me, rev.id).await?, MAX_LIST);
    let (lines, bom_cut) = capped(bom::lines(&st.db, me, rev.id).await?, MAX_BOM_ROWS);
    let outgoing = references::outgoing(&st.db, me, rev.id).await?;
    let (incoming, incoming_cut) = capped(
        references::incoming(&st.db, me, rev.item_id).await?,
        MAX_LIST,
    );
    let (cad, cad_cut) = capped(
        references::files_of_revision(&st.db, me, rev.id).await?,
        MAX_LIST,
    );
    let (rev_changes, _) = changes::for_revision(&st.db, me, rev.id).await?;
    let (open_requests, requests_cut) = open_requests_of(st, me, rev.item_id).await?;
    // A simulation shows what it is based on; anything else the
    // simulations based on it.
    let (simulation, based, based_cut) = if rev.item_type == ItemType::Simulation.as_str() {
        (
            Some(simulation_state(st, me, &rev).await?),
            Vec::new(),
            false,
        )
    } else {
        let (based, cut) = simulations_of(st, me, rev.item_id).await?;
        (None, based, cut)
    };
    Ok(json!({
        "revision": rev_full(&rev),
        "files": file_list.iter().map(file_brief).collect::<Vec<_>>(),
        "bom": lines.iter().map(bom_line_brief).collect::<Vec<_>>(),
        "references": outgoing.iter().map(|r| json!({
            "kind": r.kind,
            "item": r.to_number,
            "name": r.to_name,
            "latest_revision": r.to_latest_code,
            "latest_state": r.to_latest_state,
            "released_with": r.released_to_code,
            "note": r.note,
        })).collect::<Vec<_>>(),
        "referenced_by": incoming.iter().map(|r| json!({
            "kind": r.kind,
            "item": r.from_number,
            "revision": r.from_code,
            "name": r.from_name,
            "state": r.from_state,
        })).collect::<Vec<_>>(),
        "cad_file_references": cad.iter().map(|f| json!({
            "file": f.filename,
            "references": f.references.iter().map(|n| json!({
                "name": n.name,
                "found_in": n.matches.iter().map(|m| json!({
                    "item": m.item_number,
                    "revision": m.revision_code,
                    "state": m.revision_state,
                    "file": m.filename,
                    "same_revision": m.same_revision,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "changes": rev_changes.iter().map(change_brief).collect::<Vec<_>>(),
        "open_change_requests": open_requests,
        "simulation": simulation,
        "simulations": based,
        "truncated": files_cut || bom_cut || incoming_cut || cad_cut || requests_cut || based_cut,
        "url": web_url(st, &format!("/items/{}", rev.item_id)),
    }))
}

async fn get_bom(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    let tree = bom::tree(&st.db, me, rev.id, rule).await?;
    let (rows, truncated) = capped(bom::flatten(&tree.children), MAX_BOM_ROWS);
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            let n = &row.node;
            json!({
                "level": n.level,
                "find_number": n.find_number,
                "item": n.item_number,
                "name": n.revision.as_ref().map(|r| r.name.clone()),
                "type": n.item_type,
                "revision": n.revision.as_ref().map(|r| r.code.clone()),
                "state": n.revision.as_ref().map_or(if n.restricted { "restricted" } else { "unresolved" }, |r| r.state.as_str()),
                "restricted": n.restricted,
                "quantity": n.quantity,
                "unit": n.unit,
                "unit_mass_kg": n.unit_mass_kg,
                "total_mass_kg": n.total_mass_kg,
                "total_cost": n.total_cost,
                "has_children": row.has_children,
                "notes": n.notes,
                "cycle": n.cycle,
            })
        })
        .collect();
    Ok(json!({
        "root": format!("{}/{}", tree.root_item_number, tree.root.code),
        "root_state": tree.root.state,
        "rule": rule.as_str(),
        "line_count": tree.line_count,
        "unresolved_count": tree.unresolved_count,
        "restricted_lines": tree.restricted_lines,
        "rollup_incomplete": tree.rollup_incomplete,
        "total_mass_kg": tree.total_mass_kg,
        "mass_complete": tree.mass_complete,
        "total_cost": tree.total_cost,
        "cost_complete": tree.cost_complete,
        "rows": rows,
        "truncated": truncated,
    }))
}

async fn where_used(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let levels = a.limit("levels", 1, i64::from(items::MAX_WHERE_USED_LEVELS))?;
    let rows = items::where_used_levels(&st.db, me, item.id, levels as i32).await?;
    let hidden_parents = items::hidden_parents(&st.db, me, item.id).await?;
    let (rows, truncated) = capped(rows, MAX_LIST);
    Ok(json!({
        "item": item.number,
        "levels": levels,
        "hidden_parents": hidden_parents,
        "used_in": rows.iter().map(|row| (row.level, &row.child_number, &row.usage)).map(|(level, via, p)| json!({
            "level": level,
            "via": via,
            "item": p.parent_number,
            "revision": p.parent_code,
            "name": p.parent_name,
            "state": p.parent_state,
            "find_number": p.find_number,
            "quantity": p.quantity,
            "released_with": p.released_child_code,
        })).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// An alternate, successor or predecessor in a compact form.
fn linked_brief(l: &interchange::LinkedItem, with_direction: bool) -> Value {
    if l.restricted {
        return json!({ "item": "restricted item" });
    }
    let mut v = json!({
        "item": l.number,
        "name": l.name,
        "latest_revision": l.latest_code,
        "latest_state": l.latest_state,
        "note": l.note,
    });
    if with_direction {
        v["two_way"] = json!(l.two_way);
    }
    v
}

async fn replace_preview(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let plan = interchange::plan_replace(&st.db, me, item.id, &a.req_str("with")?).await?;
    let (rows, truncated) = capped(plan.rows, MAX_LIST);
    Ok(json!({
        "item": plan.old_number,
        "with": plan.new_number,
        "would_replace": plan.replaced,
        "hidden_parents": plan.hidden,
        "lines": rows.iter().map(|r| json!({
            "assembly": r.parent_number,
            "revision": r.parent_code,
            "name": r.parent_name,
            "state": r.parent_state,
            "find_number": r.find_number,
            "quantity": r.quantity,
            "action": r.action,
            "checked_out_by": r.checked_out_by_name,
        })).collect::<Vec<_>>(),
        "truncated": truncated,
        "url": web_url(st, &format!(
            "/items/{}/where-used?with={}",
            item.id,
            crate::util::pct_encode(&plan.new_number)
        )),
    }))
}

async fn item_history(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let limit = a.limit("limit", 50, 200)?;
    let mut ids: Vec<uuid::Uuid> = items::revisions(&st.db, me, item.id)
        .await?
        .iter()
        .map(|r| r.id)
        .collect();
    ids.push(item.id);
    let entries = audit::for_entities(&st.db, me, &ids, limit).await?;
    Ok(json!({
        "item": item.number,
        "entries": entries.iter().map(|e| json!({
            "at": e.at,
            "user": e.username,
            "action": e.action,
            "summary": e.summary,
        })).collect::<Vec<_>>(),
    }))
}

async fn list_changes(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let status = a
        .opt_str("status")?
        .map(|s| ChangeStatus::parse(&s).ok_or_else(|| bad(format!("Unknown status '{s}'."))))
        .transpose()?;
    let limit = a.limit("limit", 20, 100)?;
    let (rows, total) = changes::list(&st.db, me, status, Page::new(Some(limit), None)).await?;
    Ok(json!({
        "total": total,
        "truncated": total > rows.len() as i64,
        "changes": rows.iter().map(change_brief).collect::<Vec<_>>(),
    }))
}

fn dependency_brief(d: &crate::services::readiness::Dependency) -> Value {
    json!({
        "of": d.revision_label,
        "relation": d.relation,
        "item": d.item_number,
        "name": d.item_name,
        "latest_revision": d.latest_code,
        "latest_state": d.latest_state,
        "problem": d.problem,
        "in_other_change": d.other_change,
        "addable": d.addable,
    })
}

/// A change order with its revisions, reviews and release readiness, and
/// optionally the impact analysis.
async fn change_overview(
    st: &AppState,
    me: &Actor,
    change: &Change,
    with_impact: bool,
) -> AppResult<Value> {
    let affected = changes::items(&st.db, me, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    let readiness = changes::readiness(&st.db, me, change.id).await?;
    let mut value = json!({
        "change": change_brief(change),
        "description": change.description,
        "items": affected.iter().map(|i| json!({
            "item": i.item_number,
            "revision": i.code,
            "name": i.name,
            "state": i.state,
            "checked_out_by": i.checked_out_by_name,
        })).collect::<Vec<_>>(),
        "reviews": reviews.iter().map(|r| json!({
            "reviewer": r.reviewer_username,
            "name": r.reviewer_name,
            "decision": r.decision,
            "comment": r.comment,
            "decided_at": r.decided_at,
        })).collect::<Vec<_>>(),
        "readiness": {
            "ready": readiness.is_ready(),
            "blockers": readiness.blockers.iter().map(dependency_brief).collect::<Vec<_>>(),
            "warnings": readiness.warnings.iter().map(dependency_brief).collect::<Vec<_>>(),
        },
        "url": web_url(st, &format!("/changes/{}", change.id)),
    });
    if with_impact {
        let (deps, hidden) = changes::impact(&st.db, me, change.id).await?;
        let (deps, truncated) = capped(deps, MAX_LIST);
        value["impact"] = json!({
            "dependents": deps.iter().map(|d| json!({
                "level": d.level,
                "item": d.item_number,
                "name": d.item_name,
                "revision": d.revision_code,
                "state": d.revision_state,
                "relation": d.relation,
                "via": d.via_number,
                "in_change": d.in_change,
            })).collect::<Vec<_>>(),
            "truncated": truncated,
            "hidden_dependents": hidden,
        });
    }
    Ok(value)
}

async fn get_change(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    change_overview(st, me, &change, true).await
}

async fn suggest_reviewers(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    let limit = a.limit(
        "limit",
        reviewers::DEFAULT_LIMIT as i64,
        reviewers::MAX_LIMIT as i64,
    )?;
    let suggestions = reviewers::suggest(&st.db, me, change.id, limit as usize).await?;
    Ok(json!({
        "change": change.number,
        "status": change.status,
        "suggestions": suggestions.iter().map(|s| json!({
            "username": s.username,
            "name": s.display_name,
            "role": s.role,
            "score": s.score,
            "reasons": s.reasons,
        })).collect::<Vec<_>>(),
        "note": "Suggestions only; reviewers are added by the author of the change in TeamCraft.",
    }))
}

async fn my_worklist(st: &AppState, me: &Actor) -> AppResult<Value> {
    let checkouts = revisions::checked_out_by(&st.db, me).await?;
    let reviews = changes::pending_reviews(&st.db, me).await?;
    let (open_impacts, impacts_cut) = capped(impacts::open_for(&st.db, me).await?, MAX_LIST);
    let mine = changes::open_by(&st.db, me).await?;
    let unread = notifications::unread_count(&st.db, me).await?;
    let (assigned, assigned_cut) = capped(requests::assigned_to(&st.db, me).await?, MAX_LIST);
    let (raised, raised_cut) = capped(requests::open_by(&st.db, me).await?, MAX_LIST);
    Ok(json!({
        "user": { "username": me.username, "name": me.display_name, "role": me.role.as_str() },
        "checkouts": checkouts.iter().map(|r| {
            let mut v = rev_brief(r);
            v["checked_out_at"] = json!(r.checked_out_at);
            v
        }).collect::<Vec<_>>(),
        "reviews_waiting": reviews.iter().map(change_brief).collect::<Vec<_>>(),
        "open_impacts": open_impacts.iter().map(|i| json!({
            "item": i.item_number,
            "revision": i.revision_code,
            "name": i.item_name,
            "because": format!("{}/{} was {}", i.source_number, i.source_code, i.event),
            "relation": i.relation,
            "detail": i.detail,
            "change": i.change_number,
            "since": i.created_at,
        })).collect::<Vec<_>>(),
        "open_impacts_truncated": impacts_cut,
        "my_open_changes": mine.iter().map(change_brief).collect::<Vec<_>>(),
        "change_requests_assigned": assigned.iter().map(request_brief).collect::<Vec<_>>(),
        "my_open_change_requests": raised.iter().map(request_brief).collect::<Vec<_>>(),
        "change_requests_truncated": assigned_cut || raised_cut,
        "notifications_unread": unread,
    }))
}

async fn bom_health(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    Ok(json!(insights::bom_health(&st.db, me, rev.id, rule).await?))
}

/// Duplicate contents grouped by SHA-256 (largest first, as returned).
pub fn group_duplicates(rows: &[insights::DuplicateFile]) -> Vec<Value> {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: BTreeMap<&str, Vec<&insights::DuplicateFile>> = BTreeMap::new();
    for r in rows {
        let entry = groups.entry(r.sha256.as_str()).or_default();
        if entry.is_empty() {
            order.push(r.sha256.as_str());
        }
        entry.push(r);
    }
    order
        .into_iter()
        .map(|sha| {
            let copies = &groups[sha];
            json!({
                "sha256": sha,
                "size_bytes": copies[0].size_bytes,
                "copies": copies.iter().map(|c| json!({
                    "item": c.item_number,
                    "revision": c.revision_code,
                    "state": c.revision_state,
                    "filename": c.filename,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

async fn find_duplicate_files(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let limit = a.limit("limit", 100, 1000)?;
    let revision = match a.opt_str("item")? {
        Some(number) => {
            Some(revision_at(st, me, &number, a.opt_str("revision")?.as_deref()).await?)
        }
        None => None,
    };
    let rows =
        insights::duplicate_files(&st.db, me, revision.as_ref().map(|r| r.id), limit).await?;
    Ok(json!({
        "scope": revision.as_ref().map(revisions::label),
        "groups": group_duplicates(&rows),
        "truncated": rows.len() as i64 >= limit,
    }))
}

async fn find_similar_parts(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let (number, code) = parse_item_ref(&a.req_str("item")?);
    let code = match a.opt_str("revision")? {
        Some(c) => Some(c),
        None => code,
    };
    let rev = revision_at(st, me, &number, code.as_deref()).await?;
    let options = similar::Options {
        limit: a.limit("limit", 10, similar::MAX_LIMIT)?,
        ignore_scale: a.flag("ignore_scale", false)?,
    };
    let file = a.opt_str("file")?;
    let report = similar::for_revision(
        st,
        me,
        &rev,
        file.as_deref(),
        options,
        std::time::Duration::from_secs(60),
    )
    .await?;
    Ok(json!({
        "source": revisions::label(&rev),
        "status": report.status,
        "message": report.message,
        "file": report.source.as_ref().map(|s| &s.filename),
        "signature": report.signature,
        "ignore_scale": report.ignore_scale,
        "similar": report.candidates.iter().map(|c| json!({
            "item": c.item_number,
            "revision": c.revision_code,
            "name": c.name,
            "type": c.item_type,
            "state": c.state,
            "project": c.project_code,
            "file": c.filename,
            "score": c.score,
            "shape": c.shape,
            "size": c.size,
            "identical": c.identical,
            "explanation": c.explanation,
        })).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Revision comparison (pure)
// ---------------------------------------------------------------------------

/// A file of a revision as far as the comparison is concerned.
#[derive(Clone, Debug)]
pub struct FileState {
    pub filename: String,
    pub sha256: String,
    pub version: i32,
    pub size_bytes: i64,
}

/// A BOM line as far as the comparison is concerned.
#[derive(Clone, Debug)]
pub struct LineState {
    pub child: String,
    pub name: String,
    pub find_number: i32,
    pub quantity: f64,
}

/// What [`compare`] needs of a revision.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub label: String,
    /// (field, value) pairs in display order.
    pub attributes: Vec<(&'static str, Value)>,
    pub files: Vec<FileState>,
    pub bom: Vec<LineState>,
}

impl Snapshot {
    pub fn of(rev: &Revision, file_list: &[FileRecord], lines: &[BomLine]) -> Snapshot {
        Snapshot {
            label: revisions::label(rev),
            attributes: vec![
                ("name", json!(rev.name)),
                ("description", json!(rev.description)),
                ("material", json!(rev.material)),
                ("mass_kg", json!(rev.mass_kg)),
                ("unit_cost", json!(rev.unit_cost)),
            ],
            files: file_list
                .iter()
                .map(|f| FileState {
                    filename: f.filename.clone(),
                    sha256: f.sha256.clone(),
                    version: f.version,
                    size_bytes: f.size_bytes,
                })
                .collect(),
            bom: lines
                .iter()
                .map(|l| LineState {
                    child: l.child_number.clone(),
                    name: l.child_name.clone(),
                    find_number: l.find_number,
                    quantity: l.quantity,
                })
                .collect(),
        }
    }
}

/// The BOM lines of one child item, summed up (an item may appear on
/// several lines).
struct ChildUse<'a> {
    name: &'a str,
    quantity: f64,
    find_numbers: BTreeSet<i32>,
}

fn child_uses(lines: &[LineState]) -> BTreeMap<&str, ChildUse<'_>> {
    let mut out: BTreeMap<&str, ChildUse> = BTreeMap::new();
    for l in lines {
        let e = out.entry(l.child.as_str()).or_insert(ChildUse {
            name: &l.name,
            quantity: 0.0,
            find_numbers: BTreeSet::new(),
        });
        e.quantity += l.quantity;
        e.find_numbers.insert(l.find_number);
    }
    out
}

/// What changed from `old` to `new`: attributes, files (matched by name,
/// compared by content hash) and BOM lines (matched by child item).
pub fn compare(old: &Snapshot, new: &Snapshot) -> Value {
    let attributes: Vec<Value> = old
        .attributes
        .iter()
        .zip(&new.attributes)
        .filter(|((_, a), (_, b))| a != b)
        .map(|((field, a), (_, b))| json!({ "field": field, "from": a, "to": b }))
        .collect();

    let key = |f: &FileState| f.filename.to_lowercase();
    let old_files: BTreeMap<String, &FileState> = old.files.iter().map(|f| (key(f), f)).collect();
    let new_files: BTreeMap<String, &FileState> = new.files.iter().map(|f| (key(f), f)).collect();
    let mut files_added = Vec::new();
    let mut files_changed = Vec::new();
    let mut files_unchanged = 0usize;
    for (k, f) in &new_files {
        match old_files.get(k) {
            None => files_added.push(json!({
                "filename": f.filename, "version": f.version, "size_bytes": f.size_bytes,
            })),
            Some(o) if o.sha256 != f.sha256 => files_changed.push(json!({
                "filename": f.filename,
                "from_version": o.version,
                "to_version": f.version,
                "from_size_bytes": o.size_bytes,
                "to_size_bytes": f.size_bytes,
            })),
            Some(_) => files_unchanged += 1,
        }
    }
    let files_removed: Vec<Value> = old_files
        .iter()
        .filter(|(k, _)| !new_files.contains_key(*k))
        .map(|(_, f)| json!({ "filename": f.filename, "version": f.version }))
        .collect();

    let old_bom = child_uses(&old.bom);
    let new_bom = child_uses(&new.bom);
    let mut bom_added = Vec::new();
    let mut bom_changed = Vec::new();
    for (child, n) in &new_bom {
        match old_bom.get(child) {
            None => bom_added.push(json!({
                "item": child, "name": n.name, "quantity": n.quantity, "find_numbers": n.find_numbers,
            })),
            Some(o) => {
                let quantity = (o.quantity - n.quantity).abs() > 1e-9;
                let find = o.find_numbers != n.find_numbers;
                if quantity || find {
                    bom_changed.push(json!({
                        "item": child,
                        "name": n.name,
                        "from_quantity": o.quantity,
                        "to_quantity": n.quantity,
                        "from_find_numbers": o.find_numbers,
                        "to_find_numbers": n.find_numbers,
                    }));
                }
            }
        }
    }
    let bom_removed: Vec<Value> = old_bom
        .iter()
        .filter(|(c, _)| !new_bom.contains_key(*c))
        .map(|(c, o)| json!({ "item": c, "name": o.name, "quantity": o.quantity }))
        .collect();

    let identical = attributes.is_empty()
        && files_added.is_empty()
        && files_changed.is_empty()
        && files_removed.is_empty()
        && bom_added.is_empty()
        && bom_changed.is_empty()
        && bom_removed.is_empty();
    json!({
        "from": old.label,
        "to": new.label,
        "identical": identical,
        "attributes": attributes,
        "files": {
            "added": files_added,
            "removed": files_removed,
            "new_content": files_changed,
            "unchanged": files_unchanged,
        },
        "bom": {
            "added": bom_added,
            "removed": bom_removed,
            "changed": bom_changed,
        },
    })
}

async fn snapshot(st: &AppState, me: &Actor, rev: &Revision) -> AppResult<Snapshot> {
    let file_list = files::list(&st.db, me, rev.id).await?;
    let lines = bom::lines(&st.db, me, rev.id).await?;
    Ok(Snapshot::of(rev, &file_list, &lines))
}

/// The two revisions of `item` a comparison is about: `to` (default the
/// latest) and `from` (default the revision before `to`).
async fn revision_pair(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<(Revision, Revision)> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    // newest first
    let revs = items::revisions(&st.db, me, item.id).await?;
    let find = |code: &str| {
        revs.iter()
            .find(|r| r.code.eq_ignore_ascii_case(code))
            .ok_or_else(|| AppError::NotFound(format!("{} has no revision {code}.", item.number)))
    };
    let to = match a.opt_str("to")? {
        Some(c) if !c.eq_ignore_ascii_case("latest") => find(&c)?,
        _ => revs
            .first()
            .ok_or_else(|| AppError::NotFound(format!("{} has no revisions.", item.number)))?,
    };
    let from = match a.opt_str("from")? {
        Some(c) => find(&c)?,
        None => revs.iter().find(|r| r.seq < to.seq).ok_or_else(|| {
            bad(format!(
                "{} is the first revision; there is nothing before it to compare with.",
                revisions::label(to)
            ))
        })?,
    };
    Ok((from.clone(), to.clone()))
}

async fn compare_revisions(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let (from, to) = revision_pair(st, me, a).await?;
    let (from, to) = (&from, &to);
    let old = snapshot(st, me, from).await?;
    let new = snapshot(st, me, to).await?;
    let mut value = compare(&old, &new);
    value["from_state"] = json!(from.state);
    value["to_state"] = json!(to.state);
    Ok(value)
}

// ---------------------------------------------------------------------------
// Change requests
// ---------------------------------------------------------------------------

fn request_brief(r: &ChangeRequest) -> Value {
    json!({
        "number": r.number,
        "title": r.title,
        "status": r.status,
        "reason": r.reason,
        "priority": r.priority,
        "requested_by": r.requested_by_name,
        "assignee": r.assignee_name,
        "due_date": r.due_date,
        "item_count": r.item_count,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "closed_at": r.closed_at,
    })
}

/// The `status` argument of `list_change_requests`: a status, `open` (not
/// finished) or `all` (default) → (status filter, open only).
pub fn request_status_filter(raw: Option<&str>) -> AppResult<(Option<RequestStatus>, bool)> {
    match raw.map(str::trim).unwrap_or("all") {
        "" | "all" => Ok((None, false)),
        "open" => Ok((None, true)),
        s => RequestStatus::parse(s)
            .map(|status| (Some(status), false))
            .ok_or_else(|| {
                bad(format!(
                    "Unknown status '{s}' ({}).",
                    REQUEST_STATUS_FILTERS.join(", ")
                ))
            }),
    }
}

/// The `scope` argument: `mine` → (requested by me, –), `assigned` →
/// (–, assigned to me), none → no filter.
pub fn request_scope(
    raw: Option<&str>,
    me: uuid::Uuid,
) -> AppResult<(Option<uuid::Uuid>, Option<uuid::Uuid>)> {
    match raw.map(str::trim).unwrap_or("") {
        "" => Ok((None, None)),
        "mine" => Ok((Some(me), None)),
        "assigned" => Ok((None, Some(me))),
        s => Err(bad(format!("Unknown scope '{s}' (mine or assigned)."))),
    }
}

/// Keeps the newest `max` entries of a list ordered oldest first.
fn newest<T>(mut list: Vec<T>, max: usize) -> (Vec<T>, bool) {
    let truncated = list.len() > max;
    if truncated {
        list.drain(..list.len() - max);
    }
    (list, truncated)
}

async fn list_change_requests(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let (status, open_only) = request_status_filter(a.opt_str("status")?.as_deref())?;
    let (requested_by, assignee) = request_scope(a.opt_str("scope")?.as_deref(), me.id)?;
    let item_id = match a.opt_str("item")? {
        Some(number) => Some(items::get_by_number(&st.db, me, &number).await?.id),
        None => None,
    };
    let limit = a.limit("limit", 20, 100)?;
    let filter = requests::RequestFilter {
        status,
        open_only,
        requested_by,
        assignee,
        item_id,
    };
    let (rows, total) = requests::list(&st.db, me, &filter, Page::new(Some(limit), None)).await?;
    Ok(json!({
        "total": total,
        "truncated": total > rows.len() as i64,
        "change_requests": rows.iter().map(request_brief).collect::<Vec<_>>(),
    }))
}

/// A change request with everything an assistant needs to discuss it.
async fn request_overview(st: &AppState, me: &Actor, request: &ChangeRequest) -> AppResult<Value> {
    let id = request.id;
    let ids = [id];
    let (affected, comments, attachments, linked, history, can) = tokio::try_join!(
        requests::items(&st.db, me, id),
        requests::comments(&st.db, id),
        requests::attachments(&st.db, id),
        requests::linked_changes(&st.db, me, id),
        // one more than shown tells whether older entries exist
        audit::for_entities(&st.db, me, &ids, REQUEST_HISTORY + 1),
        requests::abilities_of(&st.db, me, request),
    )?;
    let (comments, comments_cut) = newest(comments, MAX_COMMENTS);
    let (history, history_cut) = capped(history, REQUEST_HISTORY as usize);
    let mut brief = request_brief(request);
    brief["description"] = json!(request.description);
    if request.decided_at.is_some() {
        brief["decision"] = json!({
            "by": request.decided_by_name,
            "at": request.decided_at,
            "comment": request.decision_comment,
        });
    }
    Ok(json!({
        "change_request": brief,
        "items": affected.iter().map(|i| json!({
            "item": i.item_number,
            "name": i.name,
            "type": i.item_type,
            "project": i.project_code,
            "found_in_revision": i.revision_code,
            "found_in_state": i.revision_state,
            "latest_revision": i.latest_code,
            "latest_state": i.latest_state,
            "note": i.note,
        })).collect::<Vec<_>>(),
        "comments": comments.iter().map(|c| json!({
            "author": c.author_name,
            "at": c.created_at,
            "body": c.body,
        })).collect::<Vec<_>>(),
        "comments_truncated": comments_cut,
        "attachments": attachments.iter().map(|f| json!({
            "filename": f.filename,
            "content_type": f.content_type,
            "size_bytes": f.size_bytes,
            "uploaded_by": f.uploaded_by_name,
            "uploaded_at": f.uploaded_at,
        })).collect::<Vec<_>>(),
        "changes": linked.0.iter().map(change_brief).collect::<Vec<_>>(),
        "hidden_changes": linked.1,
        "history": history.iter().map(|e| json!({
            "at": e.at,
            "user": e.username,
            "action": e.action,
            "summary": e.summary,
        })).collect::<Vec<_>>(),
        "history_truncated": history_cut,
        "you_may": can,
        "url": web_url(st, &format!("/requests/{}", request.id)),
    }))
}

async fn get_change_request(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let request = requests::get_by_number(&st.db, me, &a.req_str("request")?).await?;
    request_overview(st, me, &request).await
}

/// Open change requests on an item, for `get_item` / `get_revision`.
async fn open_requests_of(
    st: &AppState,
    me: &Actor,
    item_id: uuid::Uuid,
) -> AppResult<(Vec<Value>, bool)> {
    let (rows, cut) = capped(
        requests::open_for_item(&st.db, me, item_id).await?,
        MAX_RELATED,
    );
    Ok((rows.iter().map(request_brief).collect(), cut))
}

// ---------------------------------------------------------------------------
// Simulations
// ---------------------------------------------------------------------------

fn basis_brief(b: &simulations::SimulationBasis) -> Value {
    json!({
        "role": b.role,
        "item": b.basis_number,
        "revision": b.basis_code,
        "name": b.basis_name,
        "type": b.basis_type,
        "state": b.basis_state,
        "file": (!b.file_name.is_empty()).then_some(&b.file_name),
        "file_version": b.file_version,
        "file_current_version": b.file_current_version,
        "newest_released_revision": b.released_code,
        "note": b.note,
        "out_of_date": b.out_of_date,
    })
}

fn result_brief(r: &simulations::SimulationResult) -> Value {
    json!({
        "name": r.name,
        "value": r.value,
        "unit": r.unit,
        "lower_limit": r.lower_limit,
        "upper_limit": r.upper_limit,
        "load_case": r.load_case,
        "status": r.status.map(|s| s.as_str()),
        "solver": r.solver,
        "solver_version": r.solver_version,
        "note": r.note,
    })
}

/// The basis links of one simulation, grouped (the service returns one row
/// per link, ordered by simulation number).
pub fn group_based(rows: &[simulations::BasedSimulation]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for row in rows {
        let same = out
            .last()
            .is_some_and(|v| v["simulation"] == row.simulation_number.as_str());
        if !same {
            out.push(json!({
                "simulation": row.simulation_number,
                "revision": row.simulation_code,
                "name": row.simulation_name,
                "state": row.simulation_state,
                "result_count": row.result_count,
                "failed_count": row.failed_count,
                "check_open": false,
                "out_of_date": false,
                "bases": [],
            }));
        }
        let entry = out.last_mut().expect("pushed above");
        if row.open_impact_id.is_some() {
            entry["check_open"] = json!(true);
        }
        if row.basis.out_of_date.is_some() {
            entry["out_of_date"] = json!(true);
        }
        if let Some(bases) = entry["bases"].as_array_mut() {
            bases.push(basis_brief(&row.basis));
        }
    }
    out
}

/// Simulations based on an item, for `get_item` / `get_revision`.
async fn simulations_of(
    st: &AppState,
    me: &Actor,
    item_id: uuid::Uuid,
) -> AppResult<(Vec<Value>, bool)> {
    let (rows, _) = simulations::based_on_item(&st.db, me, item_id).await?;
    Ok(capped(group_based(&rows), MAX_RELATED))
}

fn ensure_simulation(rev: &Revision) -> AppResult<()> {
    if rev.item_type == ItemType::Simulation.as_str() {
        Ok(())
    } else {
        Err(bad(format!(
            "{} is a {}, not a simulation; list_simulations with item={} lists the simulations based on it.",
            revisions::label(rev),
            rev.item_type,
            rev.item_number
        )))
    }
}

/// Basis links and results of a simulation revision.
async fn simulation_state(st: &AppState, me: &Actor, rev: &Revision) -> AppResult<Value> {
    let (bases, results) = tokio::try_join!(
        simulations::bases(&st.db, me, rev.id),
        simulations::results(&st.db, me, rev.id),
    )?;
    let reasons: Vec<&String> = bases
        .iter()
        .filter_map(|b| b.out_of_date.as_ref())
        .collect();
    let (shown, results_cut) = capped(results.iter().collect::<Vec<_>>(), simulations::MAX_RESULTS);
    Ok(json!({
        "out_of_date": !reasons.is_empty(),
        "out_of_date_reasons": reasons,
        "summary": simulations::summarize(&results),
        "bases": bases.iter().map(basis_brief).collect::<Vec<_>>(),
        "results": shown.into_iter().map(result_brief).collect::<Vec<_>>(),
        "results_truncated": results_cut,
    }))
}

async fn list_simulations(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let out_of_date_only = a.flag("out_of_date_only", false)?;
    let limit = a.limit("limit", 20, MAX_SIMULATIONS as i64)? as usize;
    if let Some(number) = a.opt_str("item")? {
        let item = items::get_by_number(&st.db, me, &number).await?;
        let (rows, _) = simulations::based_on_item(&st.db, me, item.id).await?;
        let mut list = group_based(&rows);
        if out_of_date_only {
            list.retain(|s| s["out_of_date"] == true);
        }
        let (list, truncated) = capped(list, limit);
        let mut value = json!({
            "based_on": item.number,
            "simulations": list,
            "truncated": truncated,
        });
        if item.item_type == ItemType::Simulation.as_str() {
            value["hint"] = json!(format!(
                "{} is itself a simulation: get_simulation shows its bases and results.",
                item.number
            ));
        }
        return Ok(value);
    }
    let project_id = match a.opt_str("project")? {
        Some(code) => Some(projects::get_by_code(&st.db, me, &code).await?.id),
        None => None,
    };
    // With the filter every simulation up to the cap is checked.
    let scan = if out_of_date_only {
        MAX_SIMULATIONS
    } else {
        limit
    };
    let filter = items::ItemFilter {
        query: a.opt_str("q")?,
        item_type: Some(ItemType::Simulation),
        project_id,
        state: None,
        system: None,
        ..Default::default()
    };
    let (rows, total) =
        items::search(&st.db, me, &filter, Page::new(Some(scan as i64), None)).await?;
    let mut list = Vec::new();
    for sim in &rows {
        let (bases, results) = tokio::try_join!(
            simulations::bases(&st.db, me, sim.latest_revision_id),
            simulations::results(&st.db, me, sim.latest_revision_id),
        )?;
        let reasons: Vec<String> = bases.iter().filter_map(|b| b.out_of_date.clone()).collect();
        if out_of_date_only && reasons.is_empty() {
            continue;
        }
        list.push(json!({
            "simulation": sim.number,
            "revision": sim.latest_code,
            "name": sim.name,
            "state": sim.latest_state,
            "project": sim.project_code,
            "out_of_date": !reasons.is_empty(),
            "out_of_date_reasons": reasons,
            "based_on": bases.iter().map(|b| format!("{}/{}", b.basis_number, b.basis_code)).collect::<Vec<_>>(),
            "results": simulations::summarize(&results),
        }));
    }
    let (list, cut) = capped(list, limit);
    Ok(json!({
        "total_simulations": total,
        "checked": rows.len(),
        "simulations": list,
        "truncated": cut || total > rows.len() as i64,
    }))
}

async fn get_simulation(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    ensure_simulation(&rev)?;
    let mut value = simulation_state(st, me, &rev).await?;
    value["simulation"] = rev_brief(&rev);
    value["url"] = json!(web_url(st, &format!("/items/{}", rev.item_id)));
    Ok(value)
}

/// One matched result value of [`simulations::compare_results`].
pub fn diff_brief(d: &simulations::ResultDiff) -> Value {
    let side = |r: &Option<simulations::SimulationResult>| {
        r.as_ref().map(|r| {
            json!({
                "value": r.value,
                "unit": r.unit,
                "lower_limit": r.lower_limit,
                "upper_limit": r.upper_limit,
                "status": r.status.map(|s| s.as_str()),
            })
        })
    };
    json!({
        "change": d.change,
        "name": d.name,
        "load_case": d.load_case,
        "unit": d.unit,
        "before": side(&d.before),
        "after": side(&d.after),
        "delta": d.delta,
        "delta_percent": d.delta_percent.map(|p| (p * 100.0).round() / 100.0),
    })
}

async fn compare_simulation_results(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let (from, to) = revision_pair(st, me, a).await?;
    ensure_simulation(&to)?;
    let changes_only = a.flag("changes_only", false)?;
    let diffs = simulations::compare(&st.db, &from, &to).await?;
    let summary = simulations::compare_summary(&diffs);
    let shown: Vec<&simulations::ResultDiff> = diffs
        .iter()
        .filter(|d| !changes_only || d.change != crate::services::bom_compare::Change::Unchanged)
        .collect();
    let (shown, truncated) = capped(shown, simulations::MAX_RESULTS);
    Ok(json!({
        "from": revisions::label(&from),
        "to": revisions::label(&to),
        "from_state": from.state,
        "to_state": to.state,
        "summary": summary,
        "diffs": shown.into_iter().map(diff_brief).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// Write tools
// ---------------------------------------------------------------------------

/// Prefixes the message of a user error; internal errors stay as they are.
fn with_context(e: AppError, prefix: &str) -> AppError {
    match e {
        AppError::NotFound(m) => AppError::NotFound(format!("{prefix}{m}")),
        AppError::BadRequest(m) => AppError::BadRequest(format!("{prefix}{m}")),
        AppError::Conflict(m) => AppError::Conflict(format!("{prefix}{m}")),
        AppError::Forbidden(m) => AppError::Forbidden(format!("{prefix}{m}")),
        other => other,
    }
}

/// Resolves the item references of a call, each revision once.
async fn change_revisions(st: &AppState, me: &Actor, refs: &[String]) -> AppResult<Vec<Revision>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        let rev = change_revision(st, me, r).await?;
        if seen.insert(rev.id) {
            out.push(rev);
        }
    }
    Ok(out)
}

/// Adds the revisions (and dependencies) to a draft; returns the labels of
/// the dependencies that were added.
async fn fill_change(
    st: &AppState,
    me: &Actor,
    change: &Change,
    revs: &[Revision],
    with_dependencies: bool,
) -> AppResult<Vec<String>> {
    let mut added: Vec<String> = Vec::new();
    for rev in revs {
        if let Err(e) = changes::add_item(&st.db, me, change.id, rev.id).await {
            let prefix = if added.is_empty() {
                String::new()
            } else {
                format!("(added before: {}) ", added.join(", "))
            };
            return Err(with_context(e, &prefix));
        }
        added.push(revisions::label(rev));
    }
    if with_dependencies {
        changes::add_dependencies(&st.db, me, change.id).await
    } else {
        Ok(Vec::new())
    }
}

const NEXT_STEPS: &str =
    "The change order is a draft. To release it, the user opens it in TeamCraft, \
    adds reviewers and submits it; unresolved blockers must be released already or added first.";

async fn create_change_draft(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    me.require(Role::Editor)?;
    let title = a.req_str("title")?;
    let description = a.opt_str("description")?.unwrap_or_default();
    let priority = a.opt_str("priority")?.unwrap_or_else(|| "normal".into());
    let priority = Priority::parse(&priority)
        .ok_or_else(|| bad("priority must be low, normal, high or critical."))?;
    let refs = a.list("items", MAX_CHANGE_ITEMS)?;
    let with_dependencies = a.flag("include_dependencies", true)?;
    // Everything that can be checked up front is, so that a mistake does
    // not even create a draft.
    let revs = change_revisions(st, me, &refs).await?;
    for rev in &revs {
        revisions::ensure_project_access(rev, me)?;
    }
    let change = changes::create(
        &st.db,
        me,
        changes::ChangeInput {
            title,
            description,
            priority,
        },
    )
    .await?;
    let dependencies = match fill_change(st, me, &change, &revs, with_dependencies).await {
        Ok(d) => d,
        Err(e) => {
            if let Err(cancel) = changes::cancel(&st.db, me, change.id).await {
                tracing::error!(change = %change.number, error = %cancel, "cancelling a half-made MCP draft failed");
            }
            return Err(with_context(
                e,
                &format!(
                    "Nothing was created ({} was cancelled again): ",
                    change.number
                ),
            ));
        }
    };
    let change = changes::get(&st.db, me, change.id).await?;
    let mut value = change_overview(st, me, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
}

async fn add_to_change(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    let refs = a.list("items", MAX_CHANGE_ITEMS)?;
    let with_dependencies = a.flag("include_dependencies", false)?;
    if refs.is_empty() && !with_dependencies {
        return Err(bad(
            "Give `items` to add, or include_dependencies=true to add the unreleased dependencies.",
        ));
    }
    let revs = change_revisions(st, me, &refs).await?;
    let dependencies = fill_change(st, me, &change, &revs, with_dependencies).await?;
    let change = changes::get(&st.db, me, change.id).await?;
    let mut value = change_overview(st, me, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
}

pub fn request_reason(raw: Option<&str>) -> AppResult<RequestReason> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("other");
    RequestReason::parse(raw).ok_or_else(|| {
        bad(format!(
            "Unknown reason '{raw}' ({}).",
            REQUEST_REASONS.join(", ")
        ))
    })
}

/// An affected item of a new request: `PRT-000012` or `PRT-000012/B`.
async fn affected_item(
    st: &AppState,
    me: &Actor,
    reference: &str,
) -> AppResult<requests::AffectedItem> {
    let (number, code) = parse_item_ref(reference);
    Ok(match code {
        Some(code) => {
            let rev = revision_at(st, me, &number, Some(&code)).await?;
            requests::AffectedItem {
                item_id: rev.item_id,
                revision_id: Some(rev.id),
                note: String::new(),
            }
        }
        None => requests::AffectedItem {
            item_id: items::get_by_number(&st.db, me, &number).await?.id,
            revision_id: None,
            note: String::new(),
        },
    })
}

const REQUEST_NEXT_STEPS: &str = "The change request is new. CAD editors of the affected project groups \
    triage it in TeamCraft and accept, defer or reject it; an accepted request is implemented by a change \
    order. The assistant cannot decide it.";

async fn create_change_request(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let title = a.req_str("title")?;
    let description = a.opt_str("description")?.unwrap_or_default();
    let reason = request_reason(a.opt_str("reason")?.as_deref())?;
    let priority = a.opt_str("priority")?.unwrap_or_else(|| "normal".into());
    let priority = Priority::parse(&priority)
        .ok_or_else(|| bad("priority must be low, normal, high or critical."))?;
    let due_date = requests::parse_due_date(a.opt_str("due_date")?.as_deref())?;
    let refs = a.list("items", requests::MAX_ITEMS as usize)?;
    // Resolve every item before anything is written.
    let mut affected = Vec::with_capacity(refs.len());
    for r in &refs {
        affected.push(affected_item(st, me, r).await?);
    }
    let request = requests::create(
        &st.db,
        me,
        requests::RequestInput {
            title,
            description,
            reason,
            priority,
            due_date,
        },
        affected,
    )
    .await?;
    let mut value = request_overview(st, me, &request).await?;
    value["next_steps"] = json!(REQUEST_NEXT_STEPS);
    Ok(value)
}

async fn comment_change_request(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let request = requests::get_by_number(&st.db, me, &a.req_str("request")?).await?;
    let body = a.req_str("body")?;
    let comment = requests::comment(&st.db, me, request.id, &body).await?;
    Ok(json!({
        "change_request": request.number,
        "status": request.status,
        "comment": {
            "author": comment.author_name,
            "at": comment.created_at,
            "body": comment.body,
        },
        "url": web_url(st, &format!("/requests/{}", request.id)),
    }))
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// (name, description, required)
    arguments: &'static [(&'static str, &'static str, bool)],
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "release_notes",
        title: "Release notes",
        description: "Release notes for a revision: what changed compared with the previous one.",
        arguments: &[
            ("item", "Item number, e.g. ASM-000003", true),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
    Prompt {
        name: "review_change",
        title: "Review a change order",
        description: "Prepares the review of a change order: scope, risks, open questions.",
        arguments: &[("change", "Change number, e.g. ECO-000004", true)],
    },
    Prompt {
        name: "release_check",
        title: "Release check",
        description: "Checks whether a revision and its structure are ready to be released.",
        arguments: &[
            ("item", "Item number", true),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
    Prompt {
        name: "worklist_triage",
        title: "Worklist triage",
        description: "Orders what is waiting for me into a short to-do list.",
        arguments: &[],
    },
    Prompt {
        name: "triage_requests",
        title: "Prepare request triage",
        description:
            "Summarises the open change requests assigned to me (and new unassigned ones) so I \
            can triage them; the decisions stay mine.",
        arguments: &[],
    },
    Prompt {
        name: "simulation_status",
        title: "Simulation status",
        description:
            "Whether the simulations of a part or assembly – or one simulation – still match \
            their bases, and how the results look.",
        arguments: &[
            (
                "item",
                "Part, assembly or simulation number, e.g. SIM-000003",
                true,
            ),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
];

/// `prompts/list`.
pub fn prompts() -> Vec<Value> {
    PROMPTS
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "title": p.title,
                "description": p.description,
                "arguments": p.arguments.iter().map(|(name, description, required)| json!({
                    "name": name, "description": description, "required": required,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// `prompts/get`: the prompt as one user message.
pub fn prompt(name: &str, args: &Value) -> Result<Value, String> {
    let p = PROMPTS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("unknown prompt `{name}`"))?;
    let arg = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    for (key, _, required) in p.arguments {
        if *required && arg(key).is_none() {
            return Err(format!("prompt `{name}` needs the argument `{key}`"));
        }
    }
    let revision = arg("revision").unwrap_or_else(|| "latest".into());
    let text = match name {
        "release_notes" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Write release notes for {item} revision {revision} in TeamCraft.\n\
                 1. Call compare_revisions with item={item} and to={revision} (it compares with the revision before).\n\
                 2. Call get_revision for {item}/{revision} for the description and the change orders it belongs to; \
                 if there is one, call get_change for its title and reason.\n\
                 Produce: a one-line summary; then short sections Attributes, Files (added, removed, new content) and BOM \
                 (added, removed, quantity changes), leaving out empty sections; then Open points (e.g. unreleased children, \
                 missing mass). Quote item numbers and revisions exactly. Do not invent reasons the data does not give."
            )
        }
        "review_change" => {
            let change = arg("change").unwrap_or_default();
            format!(
                "Help me review change order {change} in TeamCraft.\n\
                 1. Call get_change with change={change}.\n\
                 2. For each revision in it, call compare_revisions (item, to=its revision) to see what it changes; \
                 for assemblies also call bom_health with rule=released.\n\
                 3. Call suggest_reviewers with change={change} to see who else knows the affected items.\n\
                 Produce a concise review for an engineer: Scope (what is released, in one line each); Risks (readiness \
                 blockers and warnings, dependents from the impact analysis that are not in the change, check-outs, \
                 missing files or mass); Questions for the author; people who should also look at it (only from \
                 suggest_reviewers, with its reason; it never assigns them); a suggestion whether it looks ready. The decision \
                 (approve or reject) is mine – you cannot and should not make it."
            )
        }
        "release_check" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Check whether {item} revision {revision} is ready to be released.\n\
                 1. Call get_revision for {item}/{revision}.\n\
                 2. Call bom_health with item={item}, revision={revision}, rule=released.\n\
                 3. Call find_duplicate_files with item={item}, revision={revision}.\n\
                 4. If it is a new part (revision A), call find_similar_parts with item={item}, revision={revision} for \
                 existing parts of nearly the same shape (score 90 or more).\n\
                 Produce a checklist: Blocking (must be fixed or released together: unreleased or unresolved BOM lines and \
                 references, check-outs) and Warnings (missing mass or files, obsolete parts, duplicate files, near-duplicate \
                 existing parts, open \
                 checks). End with the items a change order would need. Offer to draft it with create_change_draft, but \
                 only create it if I say so."
            )
        }
        "worklist_triage" => "Triage my TeamCraft worklist.\n\
             Call my_worklist. Produce a short ordered to-do list: reviews waiting for my decision first (oldest \
             first), then open impacts to check, then check-outs that have been open long (remind me to check in or \
             cancel), then my own change orders that are drafts or were rejected. One line per entry with the item or \
             change number and the next action. Say plainly if there is nothing to do."
            .to_string(),
        "triage_requests" => "Prepare the triage of my TeamCraft change requests.\n\
             1. Call list_change_requests with scope=assigned and status=open.\n\
             2. Call list_change_requests with status=new for requests nobody has looked at yet.\n\
             3. For each request found (at most 10, highest priority and oldest due date first), call \
             get_change_request; for an affected item that looks central, call get_item for its state, open \
             change orders and simulations.\n\
             Produce one short block per request: number and title; the problem in one sentence; affected items \
             with their latest state; what the discussion already settled and what is open; duplicates or \
             related requests; a suggested next step (ask the requester, check a simulation, candidate for \
             accepting, deferring or rejecting – with the reason). You cannot triage, accept, defer or reject: \
             those decisions are mine and are made in TeamCraft. Say plainly if there is nothing to triage."
            .to_string(),
        "simulation_status" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Report the simulation status of {item} revision {revision} in TeamCraft.\n\
                 1. Call get_item with item={item}. If it is a simulation, call get_simulation with item={item} \
                 and revision={revision}, and compare_simulation_results with item={item} and to={revision} when \
                 there is an earlier revision.\n\
                 2. Otherwise call list_simulations with item={item}, then get_simulation for each simulation \
                 that is out of date or has failed results.\n\
                 Produce: a table of the simulations (number, revision, state, up to date or the out-of-date \
                 reason, results passed/failed); then the failed values with value, limit and load case; then \
                 what changed in the results between the revisions, if compared; then a recommendation which \
                 simulations need to be re-run or re-checked. Do not invent values the data does not give."
            )
        }
        _ => unreachable!("prompt names are checked above"),
    };
    Ok(json!({
        "description": p.description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, sha: &str, version: i32) -> FileState {
        FileState {
            filename: name.into(),
            sha256: sha.into(),
            version,
            size_bytes: 10,
        }
    }

    fn line(child: &str, find: i32, qty: f64) -> LineState {
        LineState {
            child: child.into(),
            name: format!("{child} name"),
            find_number: find,
            quantity: qty,
        }
    }

    fn snap(
        label: &str,
        mass: Option<f64>,
        files: Vec<FileState>,
        bom: Vec<LineState>,
    ) -> Snapshot {
        Snapshot {
            label: label.into(),
            attributes: vec![("name", json!("Bracket")), ("mass_kg", json!(mass))],
            files,
            bom,
        }
    }

    #[test]
    fn compare_finds_attribute_file_and_bom_changes() {
        let a = snap(
            "ASM-1/A",
            Some(1.0),
            vec![
                file("top.SLDASM", "aa", 3),
                file("old.pdf", "bb", 1),
                file("same.step", "cc", 1),
            ],
            vec![
                line("PRT-1", 10, 2.0),
                line("PRT-2", 20, 1.0),
                line("PRT-3", 30, 4.0),
            ],
        );
        let b = snap(
            "ASM-1/B",
            Some(1.5),
            vec![
                file("Top.sldasm", "dd", 4),
                file("same.step", "cc", 1),
                file("new.dxf", "ee", 1),
            ],
            vec![
                line("PRT-1", 10, 1.0),
                line("PRT-1", 11, 2.0),
                line("PRT-3", 30, 4.0),
                line("PRT-4", 40, 1.0),
            ],
        );
        let c = compare(&a, &b);
        assert_eq!(c["identical"], false);
        assert_eq!(
            c["attributes"],
            json!([{ "field": "mass_kg", "from": 1.0, "to": 1.5 }])
        );
        assert_eq!(c["files"]["added"][0]["filename"], "new.dxf");
        assert_eq!(c["files"]["removed"][0]["filename"], "old.pdf");
        // matched case-insensitively, compared by content
        assert_eq!(c["files"]["new_content"][0]["from_version"], 3);
        assert_eq!(c["files"]["new_content"][0]["to_version"], 4);
        assert_eq!(c["files"]["unchanged"], 1);
        assert_eq!(c["bom"]["added"][0]["item"], "PRT-4");
        assert_eq!(c["bom"]["removed"][0]["item"], "PRT-2");
        // PRT-1: two lines summed up, a new find number
        let changed = c["bom"]["changed"].as_array().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["item"], "PRT-1");
        assert_eq!(changed[0]["from_quantity"], 2.0);
        assert_eq!(changed[0]["to_quantity"], 3.0);
        assert_eq!(changed[0]["to_find_numbers"], json!([10, 11]));
    }

    #[test]
    fn compare_of_equal_snapshots_is_identical() {
        let a = snap(
            "P/A",
            None,
            vec![file("a.prt", "x", 1)],
            vec![line("C", 1, 1.0)],
        );
        let b = snap(
            "P/B",
            None,
            vec![file("a.prt", "x", 1)],
            vec![line("C", 1, 1.0)],
        );
        let c = compare(&a, &b);
        assert_eq!(c["identical"], true);
        assert_eq!(c["files"]["unchanged"], 1);
    }

    #[test]
    fn item_references_split_number_and_revision() {
        assert_eq!(parse_item_ref("PRT-000012"), ("PRT-000012".into(), None));
        assert_eq!(
            parse_item_ref(" PRT-000012/B "),
            ("PRT-000012".into(), Some("B".into()))
        );
        assert_eq!(parse_item_ref("PRT-000012/"), ("PRT-000012".into(), None));
    }

    #[test]
    fn arguments_are_typed_and_bounded() {
        let v = json!({
            "q": "  bolt ", "empty": " ", "n": 500, "s": "7", "f": 2.0, "bad": "x",
            "yes": "true", "list": ["A", " ", "B"], "one": "C", "null": null,
        });
        let a = Args::new(&v);
        assert_eq!(a.opt_str("q").unwrap().as_deref(), Some("bolt"));
        assert_eq!(a.opt_str("empty").unwrap(), None);
        assert_eq!(a.opt_str("null").unwrap(), None);
        assert!(a.opt_str("n").is_err());
        assert!(a.req_str("missing").is_err());
        assert_eq!(a.limit("n", 20, 100).unwrap(), 100);
        assert_eq!(a.limit("s", 20, 100).unwrap(), 7);
        assert_eq!(a.limit("f", 20, 100).unwrap(), 2);
        assert_eq!(a.limit("missing", 20, 100).unwrap(), 20);
        assert!(a.limit("bad", 20, 100).is_err());
        assert!(a.flag("yes", false).unwrap());
        assert!(a.flag("missing", true).unwrap());
        assert!(a.flag("q", true).is_err());
        assert_eq!(a.list("list", 5).unwrap(), vec!["A", "B"]);
        assert_eq!(a.list("one", 5).unwrap(), vec!["C"]);
        assert!(a.list("list", 1).is_err());
        assert!(a.list("n", 5).is_err());
        // a non-object counts as no arguments
        let none = json!("nope");
        assert_eq!(Args::new(&none).limit("limit", 3, 9).unwrap(), 3);
    }

    #[test]
    fn results_are_wrapped_and_cut() {
        let ok = success(json!({ "a": 1 }));
        assert_eq!(ok["isError"], false);
        assert_eq!(ok["structuredContent"]["a"], 1);
        assert_eq!(ok["content"][0]["text"], "{\"a\":1}");
        let list = success(json!([1, 2]));
        assert_eq!(list["structuredContent"]["result"], json!([1, 2]));

        let big = success(json!({ "s": "ä".repeat(MAX_TEXT_BYTES) }));
        let text = big["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < MAX_TEXT_BYTES + 200);
        assert!(text.contains("output cut"));
        assert!(big.get("structuredContent").is_none());

        let err = failure("nope");
        assert_eq!(err["isError"], true);
        assert_eq!(err["content"][0]["text"], "nope");
        assert_eq!(cut("aä", 2), "a");
        let (v, t) = capped(vec![1, 2, 3], 2);
        assert_eq!((v, t), (vec![1, 2], true));
    }

    #[test]
    fn internal_errors_stay_hidden() {
        let r = from_error("x", AppError::Other(anyhow::anyhow!("secret detail")));
        assert_eq!(r["isError"], true);
        assert!(!r["content"][0]["text"].as_str().unwrap().contains("secret"));
        let r = from_error("x", AppError::Conflict("checked out by Erika".into()));
        assert_eq!(r["content"][0]["text"], "checked out by Erika");
    }

    #[test]
    fn tool_definitions_are_complete() {
        let defs = definitions();
        assert_eq!(defs.len(), TOOLS.len());
        let mut names = HashSet::new();
        for d in &defs {
            assert!(names.insert(d["name"].as_str().unwrap().to_string()));
            assert_eq!(d["inputSchema"]["type"], "object");
            assert_eq!(d["annotations"]["destructiveHint"], false);
            for req in d["inputSchema"]["required"].as_array().unwrap() {
                let key = req.as_str().unwrap();
                assert!(d["inputSchema"]["properties"].get(key).is_some(), "{key}");
            }
        }
        for name in ["submit_change", "approve_change", "release", "obsolete"] {
            assert!(!exists(name));
        }
        let writes: Vec<&str> = defs
            .iter()
            .filter(|d| d["annotations"]["readOnlyHint"] == false)
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            writes,
            vec![
                "create_change_draft",
                "add_to_change",
                "create_change_request",
                "comment_change_request"
            ]
        );
        // change requests are raised and discussed, never decided
        for name in [
            "decide_change_request",
            "accept_change_request",
            "triage_change_request",
            "reject_change_request",
        ] {
            assert!(!exists(name));
        }
        let def = |name: &str| defs.iter().find(|d| d["name"] == name).unwrap().clone();
        // the shape sibling of find_duplicate_files: read-only, by item
        let similar = def("find_similar_parts");
        assert_eq!(similar["annotations"]["readOnlyHint"], true);
        assert_eq!(similar["inputSchema"]["required"], json!(["item"]));
        assert!(similar["description"]
            .as_str()
            .unwrap()
            .contains("create_change_draft"));
        // reviewers are suggested, never assigned
        let suggest = def("suggest_reviewers");
        assert_eq!(suggest["annotations"]["readOnlyHint"], true);
        assert!(suggest["description"]
            .as_str()
            .unwrap()
            .contains("Never assigns"));
        assert!(!exists("add_reviewer") && !exists("assign_reviewers"));
        let create = def("create_change_request");
        assert_eq!(create["annotations"]["idempotentHint"], false);
        assert_eq!(
            create["inputSchema"]["properties"]["items"]["maxItems"],
            requests::MAX_ITEMS
        );
        assert_eq!(
            create["inputSchema"]["properties"]["reason"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            RequestReason::ALL.len()
        );
        let list = def("list_change_requests");
        assert_eq!(
            list["inputSchema"]["properties"]["status"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            RequestStatus::ALL.len() + 2
        );
        assert_eq!(
            def("list_simulations")["inputSchema"]["properties"]["limit"]["maximum"],
            MAX_SIMULATIONS
        );
        for read in [
            "get_change_request",
            "get_simulation",
            "compare_simulation_results",
        ] {
            assert_eq!(def(read)["annotations"]["readOnlyHint"], true, "{read}");
        }
    }

    #[test]
    fn request_arguments_are_checked() {
        assert_eq!(request_status_filter(None).unwrap(), (None, false));
        assert_eq!(request_status_filter(Some("all")).unwrap(), (None, false));
        assert_eq!(request_status_filter(Some(" open ")).unwrap(), (None, true));
        assert_eq!(
            request_status_filter(Some("in_triage")).unwrap(),
            (Some(RequestStatus::InTriage), false)
        );
        assert!(request_status_filter(Some("decided")).is_err());
        // every listed filter parses
        for s in REQUEST_STATUS_FILTERS {
            assert!(request_status_filter(Some(s)).is_ok(), "{s}");
        }

        let me = uuid::Uuid::new_v4();
        assert_eq!(request_scope(None, me).unwrap(), (None, None));
        assert_eq!(request_scope(Some("mine"), me).unwrap(), (Some(me), None));
        assert_eq!(
            request_scope(Some("assigned"), me).unwrap(),
            (None, Some(me))
        );
        assert!(request_scope(Some("everyone"), me).is_err());

        assert_eq!(request_reason(None).unwrap(), RequestReason::Other);
        assert_eq!(request_reason(Some(" ")).unwrap(), RequestReason::Other);
        assert_eq!(
            request_reason(Some("Defect")).unwrap(),
            RequestReason::Defect
        );
        assert!(request_reason(Some("boredom")).is_err());
        for r in REQUEST_REASONS {
            assert!(request_reason(Some(r)).is_ok(), "{r}");
        }

        // the newest comments are kept, in their order
        assert_eq!(newest(vec![1, 2, 3, 4], 2), (vec![3, 4], true));
        assert_eq!(newest(vec![1, 2], 5), (vec![1, 2], false));

        // the item list of a new request is bounded like the service
        let many: Vec<String> = (0..=requests::MAX_ITEMS)
            .map(|i| format!("PRT-{i}"))
            .collect();
        let v = json!({ "items": many });
        assert!(Args::new(&v)
            .list("items", requests::MAX_ITEMS as usize)
            .is_err());
    }

    fn sim_result(name: &str, value: f64, upper: Option<f64>) -> simulations::SimulationResult {
        simulations::SimulationResult {
            id: uuid::Uuid::new_v4(),
            revision_id: uuid::Uuid::nil(),
            position: 0,
            name: name.into(),
            value,
            unit: "MPa".into(),
            lower_limit: None,
            upper_limit: upper,
            load_case: "LC1".into(),
            solver: String::new(),
            solver_version: String::new(),
            note: String::new(),
            updated_at: chrono::Utc::now(),
            status: Some(crate::domain::result_status(value, None, upper)),
        }
    }

    #[test]
    fn result_diffs_are_compact() {
        let before = [sim_result("Stress", 200.0, Some(250.0))];
        let after = [
            sim_result("Stress", 260.0, Some(250.0)),
            sim_result("Mass", 1.0, None),
        ];
        let diffs = simulations::compare_results(&before, &after);
        let views: Vec<Value> = diffs.iter().map(diff_brief).collect();
        let stress = views.iter().find(|v| v["name"] == "Stress").unwrap();
        assert_eq!(stress["change"], "changed");
        assert_eq!(stress["before"]["status"], "pass");
        assert_eq!(stress["after"]["status"], "fail");
        assert_eq!(stress["delta"], 60.0);
        assert_eq!(stress["delta_percent"], 30.0);
        let mass = views.iter().find(|v| v["name"] == "Mass").unwrap();
        assert_eq!(mass["change"], "added");
        assert_eq!(mass["before"], Value::Null);
        assert_eq!(mass["after"]["status"], "none");
    }

    fn based(sim: &str, basis: &str, out_of_date: Option<&str>) -> simulations::BasedSimulation {
        simulations::BasedSimulation {
            simulation_item_id: uuid::Uuid::nil(),
            simulation_number: sim.into(),
            simulation_code: "A".into(),
            simulation_state: "released".into(),
            simulation_name: format!("{sim} static"),
            result_count: 3,
            failed_count: 1,
            open_impact_id: out_of_date.map(|_| uuid::Uuid::nil()),
            basis: simulations::SimulationBasis {
                id: uuid::Uuid::new_v4(),
                revision_id: uuid::Uuid::nil(),
                role: "geometry".into(),
                note: String::new(),
                basis_item_id: uuid::Uuid::nil(),
                basis_number: basis.into(),
                basis_type: "part".into(),
                basis_revision_id: uuid::Uuid::nil(),
                basis_code: "A".into(),
                basis_seq: 1,
                basis_state: "released".into(),
                basis_name: "Bracket".into(),
                file_version_id: None,
                file_id: None,
                file_name: String::new(),
                file_version: None,
                file_current_version: None,
                released_revision_id: None,
                released_code: Some("A".into()),
                released_seq: Some(1),
                created_at: chrono::Utc::now(),
                out_of_date: out_of_date.map(str::to_string),
                basis_project_id: None,
                restricted: false,
            },
        }
    }

    #[test]
    fn based_simulations_are_grouped() {
        let g = group_based(&[
            based("SIM-1", "PRT-1", None),
            based(
                "SIM-1",
                "PRT-2",
                Some("PRT-2/B was released (simulated: A)"),
            ),
            based("SIM-2", "PRT-1", None),
        ]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0]["simulation"], "SIM-1");
        assert_eq!(g[0]["bases"].as_array().unwrap().len(), 2);
        assert_eq!(g[0]["out_of_date"], true);
        assert_eq!(g[0]["check_open"], true);
        assert_eq!(g[0]["bases"][0]["file"], Value::Null);
        assert_eq!(g[1]["out_of_date"], false);
        assert_eq!(g[1]["failed_count"], 1);
    }

    #[test]
    fn prompts_check_their_arguments() {
        assert_eq!(prompts().len(), PROMPTS.len());
        assert!(prompt("review_change", &json!({})).is_err());
        assert!(prompt("nope", &json!({})).is_err());
        let p = prompt("review_change", &json!({ "change": "ECO-000004" })).unwrap();
        let text = p["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("get_change with change=ECO-000004"));
        assert!(text.contains("suggest_reviewers with change=ECO-000004"));
        let p = prompt("release_notes", &json!({ "item": "ASM-1" })).unwrap();
        let text = p["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("to=latest"));
        // every tool a prompt mentions exists
        for p in PROMPTS {
            let args = json!({ "item": "X", "change": "Y" });
            let text = prompt(p.name, &args).unwrap()["messages"][0]["content"]["text"]
                .as_str()
                .unwrap()
                .to_string();
            for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if word.contains('_') && word.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    let known = exists(word) || ["in_work", "as_released"].contains(&word);
                    assert!(known, "prompt {} mentions unknown tool {word}", p.name);
                }
            }
        }
    }

    #[test]
    fn duplicates_are_grouped_by_content() {
        let row = |sha: &str, item: &str| insights::DuplicateFile {
            sha256: sha.into(),
            filename: "a.step".into(),
            size_bytes: 5,
            item_number: item.into(),
            revision_code: "A".into(),
            revision_state: "released".into(),
        };
        let g = group_duplicates(&[
            row("zz", "P-1"),
            row("zz", "P-2"),
            row("aa", "P-3"),
            row("aa", "P-4"),
        ]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0]["sha256"], "zz");
        assert_eq!(g[0]["copies"].as_array().unwrap().len(), 2);
    }
}
