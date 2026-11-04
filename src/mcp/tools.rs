//! The MCP tools and prompts of TeamCraft.
//!
//! Every tool calls the same `services::*` functions as the REST API, with
//! the rights of the signed-in user. Results are compact JSON (item numbers
//! and revision codes instead of ids) so they fit an assistant's context;
//! long lists are capped and flagged with `truncated`.
//!
//! Writes only prepare work: a draft change order and its affected
//! revisions. Submitting, reviewing, releasing and making obsolete are not
//! offered – those decisions stay with people.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Map, Value};

use crate::{
    auth::Actor,
    domain::{ChangeStatus, ItemType, LifecycleState, Priority, RevisionRule, Role},
    error::{AppError, AppResult},
    models::{BomLine, Change, FileRecord, Revision},
    services::{
        audit, bom, changes, files, impacts, insights, items, notifications, projects, references,
        revisions, Page,
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
        name: "my_worklist",
        title: "My worklist",
        description: "What is waiting for the signed-in user: own check-outs, reviews waiting for a \
            decision, open impacts (dependents to check) in their project groups, own open change orders \
            and the number of unread notifications.",
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
        name: "compare_revisions",
        title: "Compare two revisions",
        description: "What changed between two revisions of an item: attributes, files added, removed or \
            with new content, and BOM lines added, removed or with another quantity or find number. \
            Defaults: to = latest, from = the revision before `to`.",
        effect: READ,
        schema: schema_compare,
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
            "q": { "type": "string", "description": "Search text (number, name, material, file name or content)" },
            "type": { "type": "string", "enum": ["part", "assembly", "document", "software"] },
            "project": { "type": "string", "description": "Project group code, e.g. ROVER" },
            "state": { "type": "string", "enum": ["in_work", "in_review", "released", "obsolete"], "description": "State of the latest revision" },
            "system": { "type": "string", "description": "CAD program with files in the latest revision, e.g. solidworks, nx, creo, inventor, freecad" },
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
        "search_items" => search_items(st, &a).await,
        "get_item" => get_item(st, &a).await,
        "get_revision" => get_revision(st, &a).await,
        "get_bom" => get_bom(st, &a).await,
        "where_used" => where_used(st, &a).await,
        "item_history" => item_history(st, &a).await,
        "list_changes" => list_changes(st, &a).await,
        "get_change" => get_change(st, &a).await,
        "my_worklist" => my_worklist(st, me).await,
        "bom_health" => bom_health(st, &a).await,
        "find_duplicate_files" => find_duplicate_files(st, &a).await,
        "compare_revisions" => compare_revisions(st, &a).await,
        "create_change_draft" => create_change_draft(st, me, &a).await,
        "add_to_change" => add_to_change(st, me, &a).await,
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
    })
}

fn web_url(st: &AppState, path: &str) -> String {
    format!("{}{path}", st.config.public_url)
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// A revision by item number and code; `None` or `latest` = the newest.
async fn revision_at(st: &AppState, number: &str, code: Option<&str>) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, number).await?;
    match code {
        None => items::latest_revision(&st.db, item.id).await,
        Some(c) if c.eq_ignore_ascii_case("latest") => {
            items::latest_revision(&st.db, item.id).await
        }
        Some(c) => revisions::get_by_code(&st.db, item.id, c).await,
    }
}

async fn revision_arg(st: &AppState, a: &Args<'_>) -> AppResult<Revision> {
    let number = a.req_str("item")?;
    revision_at(st, &number, a.opt_str("revision")?.as_deref()).await
}

/// The revision a change order should carry: the given code, or the item's
/// in-work revision.
async fn change_revision(st: &AppState, reference: &str) -> AppResult<Revision> {
    let (number, code) = parse_item_ref(reference);
    match code {
        Some(code) => revision_at(st, &number, Some(&code)).await,
        None => {
            let item = items::get_by_number(&st.db, &number).await?;
            items::open_revision(&st.db, item.id).await?.ok_or_else(|| {
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

async fn search_items(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let project_id = match a.opt_str("project")? {
        Some(code) => Some(projects::get_by_code(&st.db, &code).await?.id),
        None => None,
    };
    let item_type = a
        .opt_str("type")?
        .map(|t| {
            ItemType::parse(&t).ok_or_else(|| {
                bad(format!(
                    "Unknown item type '{t}' (part, assembly, document, software)."
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
    };
    let (rows, total) = items::search(&st.db, &filter, Page::new(Some(limit), None)).await?;
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

async fn get_item(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, &a.req_str("item")?).await?;
    let revs = items::revisions(&st.db, item.id).await?;
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
        "url": web_url(st, &format!("/items/{}", item.id)),
    }))
}

async fn get_revision(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, a).await?;
    let (file_list, files_cut) = capped(files::list(&st.db, rev.id).await?, MAX_LIST);
    let (lines, bom_cut) = capped(bom::lines(&st.db, rev.id).await?, MAX_BOM_ROWS);
    let outgoing = references::outgoing(&st.db, rev.id).await?;
    let (incoming, incoming_cut) =
        capped(references::incoming(&st.db, rev.item_id).await?, MAX_LIST);
    let (cad, cad_cut) = capped(
        references::files_of_revision(&st.db, rev.id).await?,
        MAX_LIST,
    );
    let rev_changes = changes::for_revision(&st.db, rev.id).await?;
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
        "truncated": files_cut || bom_cut || incoming_cut || cad_cut,
        "url": web_url(st, &format!("/items/{}", rev.item_id)),
    }))
}

async fn get_bom(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    let tree = bom::tree(&st.db, rev.id, rule).await?;
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
                "state": n.revision.as_ref().map_or("unresolved", |r| r.state.as_str()),
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
        "total_mass_kg": tree.total_mass_kg,
        "mass_complete": tree.mass_complete,
        "total_cost": tree.total_cost,
        "cost_complete": tree.cost_complete,
        "rows": rows,
        "truncated": truncated,
    }))
}

async fn where_used(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, &a.req_str("item")?).await?;
    let levels = a.limit("levels", 1, i64::from(items::MAX_WHERE_USED_LEVELS))?;
    let rows = items::where_used_levels(&st.db, item.id, levels as i32).await?;
    let (rows, truncated) = capped(rows, MAX_LIST);
    Ok(json!({
        "item": item.number,
        "levels": levels,
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

async fn item_history(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, &a.req_str("item")?).await?;
    let limit = a.limit("limit", 50, 200)?;
    let mut ids: Vec<uuid::Uuid> = items::revisions(&st.db, item.id)
        .await?
        .iter()
        .map(|r| r.id)
        .collect();
    ids.push(item.id);
    let entries = audit::for_entities(&st.db, &ids, limit).await?;
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

async fn list_changes(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let status = a
        .opt_str("status")?
        .map(|s| ChangeStatus::parse(&s).ok_or_else(|| bad(format!("Unknown status '{s}'."))))
        .transpose()?;
    let limit = a.limit("limit", 20, 100)?;
    let (rows, total) = changes::list(&st.db, status, Page::new(Some(limit), None)).await?;
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
async fn change_overview(st: &AppState, change: &Change, with_impact: bool) -> AppResult<Value> {
    let affected = changes::items(&st.db, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    let readiness = changes::readiness(&st.db, change.id).await?;
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
        let ids: Vec<uuid::Uuid> = affected.iter().map(|i| i.revision_id).collect();
        let (deps, truncated) = capped(impacts::analysis(&st.db, &ids).await?, MAX_LIST);
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
        });
    }
    Ok(value)
}

async fn get_change(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, &a.req_str("change")?).await?;
    change_overview(st, &change, true).await
}

async fn my_worklist(st: &AppState, me: &Actor) -> AppResult<Value> {
    let checkouts = revisions::checked_out_by(&st.db, me.id).await?;
    let reviews = changes::pending_reviews(&st.db, me.id).await?;
    let (open_impacts, impacts_cut) = capped(impacts::open_for(&st.db, me).await?, MAX_LIST);
    let mine = changes::open_by(&st.db, me.id).await?;
    let unread = notifications::unread_count(&st.db, me.id).await?;
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
        "notifications_unread": unread,
    }))
}

async fn bom_health(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    Ok(json!(insights::bom_health(&st.db, rev.id, rule).await?))
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

async fn find_duplicate_files(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let limit = a.limit("limit", 100, 1000)?;
    let revision = match a.opt_str("item")? {
        Some(number) => Some(revision_at(st, &number, a.opt_str("revision")?.as_deref()).await?),
        None => None,
    };
    let rows = insights::duplicate_files(&st.db, revision.as_ref().map(|r| r.id), limit).await?;
    Ok(json!({
        "scope": revision.as_ref().map(revisions::label),
        "groups": group_duplicates(&rows),
        "truncated": rows.len() as i64 >= limit,
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

async fn snapshot(st: &AppState, rev: &Revision) -> AppResult<Snapshot> {
    let file_list = files::list(&st.db, rev.id).await?;
    let lines = bom::lines(&st.db, rev.id).await?;
    Ok(Snapshot::of(rev, &file_list, &lines))
}

async fn compare_revisions(st: &AppState, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, &a.req_str("item")?).await?;
    // newest first
    let revs = items::revisions(&st.db, item.id).await?;
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
    let old = snapshot(st, from).await?;
    let new = snapshot(st, to).await?;
    let mut value = compare(&old, &new);
    value["from_state"] = json!(from.state);
    value["to_state"] = json!(to.state);
    Ok(value)
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
async fn change_revisions(st: &AppState, refs: &[String]) -> AppResult<Vec<Revision>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        let rev = change_revision(st, r).await?;
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
    let revs = change_revisions(st, &refs).await?;
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
    let change = changes::get(&st.db, change.id).await?;
    let mut value = change_overview(st, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
}

async fn add_to_change(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, &a.req_str("change")?).await?;
    let refs = a.list("items", MAX_CHANGE_ITEMS)?;
    let with_dependencies = a.flag("include_dependencies", false)?;
    if refs.is_empty() && !with_dependencies {
        return Err(bad(
            "Give `items` to add, or include_dependencies=true to add the unreleased dependencies.",
        ));
    }
    let revs = change_revisions(st, &refs).await?;
    let dependencies = fill_change(st, me, &change, &revs, with_dependencies).await?;
    let change = changes::get(&st.db, change.id).await?;
    let mut value = change_overview(st, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
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
                 Produce a concise review for an engineer: Scope (what is released, in one line each); Risks (readiness \
                 blockers and warnings, dependents from the impact analysis that are not in the change, check-outs, \
                 missing files or mass); Questions for the author; a suggestion whether it looks ready. The decision \
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
                 Produce a checklist: Blocking (must be fixed or released together: unreleased or unresolved BOM lines and \
                 references, check-outs) and Warnings (missing mass or files, obsolete parts, duplicate files, open \
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
        assert_eq!(writes, vec!["create_change_draft", "add_to_change"]);
    }

    #[test]
    fn prompts_check_their_arguments() {
        assert_eq!(prompts().len(), PROMPTS.len());
        assert!(prompt("review_change", &json!({})).is_err());
        assert!(prompt("nope", &json!({})).is_err());
        let p = prompt("review_change", &json!({ "change": "ECO-000004" })).unwrap();
        let text = p["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("get_change with change=ECO-000004"));
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
