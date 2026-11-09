//! Argument schemas of the tools (`inputSchema` in `tools/list`).

use super::*;

pub(super) fn object(properties: Value, required: &[&str]) -> Value {
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

pub(super) fn schema_none() -> Value {
    object(json!({}), &[])
}

pub(super) fn schema_search_items() -> Value {
    object(
        json!({
            "q": { "type": "string", "description": "Search text (number, name, material, attribute values, file name or content)" },
            "type": { "type": "string", "enum": ["part", "assembly", "document", "software", "simulation", "as_built"] },
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

pub(super) fn schema_item() -> Value {
    object(json!({ "item": p_item() }), &["item"])
}

pub(super) fn schema_revision() -> Value {
    object(
        json!({ "item": p_item(), "revision": p_revision() }),
        &["item"],
    )
}

pub(super) fn schema_read_file_text() -> Value {
    object(
        json!({
            "item": p_item(),
            "revision": p_revision(),
            "file": { "type": "string", "description": "File name in the revision, e.g. spec.txt" },
            "version": { "type": "integer", "minimum": 1, "description": "File version (default: the current one)" },
            "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "First character to return" },
            "limit": p_limit(DEFAULT_TEXT_CHARS, files::MAX_TEXT_PAGE_CHARS),
        }),
        &["item", "file"],
    )
}

pub(super) fn schema_impact_summary() -> Value {
    object(
        json!({
            "item": p_item(),
            "levels": p_limit(3, MAX_IMPACT_LEVELS),
        }),
        &["item"],
    )
}

pub(super) fn schema_bom() -> Value {
    object(
        json!({ "item": p_item(), "revision": p_revision(), "rule": p_rule() }),
        &["item"],
    )
}

pub(super) fn schema_mass_properties() -> Value {
    object(
        json!({
            "item": p_item(),
            "revision": p_revision(),
            "rule": p_rule(),
            "reference": {
                "type": "string",
                "description": "Point the second inertia tensor is taken about: `x,y,z` in metres in the \
                    frame of the geometry files (default the origin), e.g. the nose tip",
            },
        }),
        &["item"],
    )
}

pub(super) fn schema_replace_preview() -> Value {
    object(
        json!({
            "item": p_item(),
            "with": { "type": "string", "description": "Item number of the replacement (its successor or an alternate)" },
        }),
        &["item", "with"],
    )
}

pub(super) fn schema_where_used() -> Value {
    object(
        json!({
            "item": p_item(),
            "levels": p_limit(1, i64::from(items::MAX_WHERE_USED_LEVELS)),
        }),
        &["item"],
    )
}

pub(super) fn schema_history() -> Value {
    object(
        json!({ "item": p_item(), "limit": p_limit(50, 200) }),
        &["item"],
    )
}

pub(super) fn schema_list_changes() -> Value {
    object(
        json!({
            "status": { "type": "string", "enum": ["draft", "in_review", "rejected", "released", "cancelled"] },
            "limit": p_limit(20, 100),
        }),
        &[],
    )
}

pub(super) fn schema_change() -> Value {
    object(
        json!({ "change": { "type": "string", "description": "Change number, e.g. ECO-000004" } }),
        &["change"],
    )
}

pub(super) fn schema_suggest_reviewers() -> Value {
    object(
        json!({
            "change": { "type": "string", "description": "Change number, e.g. ECO-000004" },
            "limit": p_limit(reviewers::DEFAULT_LIMIT as i64, reviewers::MAX_LIMIT as i64),
        }),
        &["change"],
    )
}

pub(super) fn schema_duplicates() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Only contents of this item's revision (optional)" },
            "revision": p_revision(),
            "limit": p_limit(100, 1000),
        }),
        &[],
    )
}

pub(super) fn schema_similar() -> Value {
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

pub(super) fn schema_compare() -> Value {
    object(
        json!({
            "item": p_item(),
            "from": { "type": "string", "description": "Older revision code (default: the one before `to`)" },
            "to": { "type": "string", "description": "Newer revision code or `latest` (default)" },
        }),
        &["item"],
    )
}

pub(super) fn schema_create_change() -> Value {
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

pub(super) fn schema_add_to_change() -> Value {
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
pub(super) const REQUEST_STATUS_FILTERS: &[&str] = &[
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

pub(super) const REQUEST_REASONS: &[&str] = &[
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

pub(super) fn schema_list_requests() -> Value {
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

pub(super) fn schema_request() -> Value {
    object(json!({ "request": p_request() }), &["request"])
}

pub(super) fn schema_create_request() -> Value {
    object(
        json!({
            "title": { "type": "string", "maxLength": 200, "description": "The problem in one line" },
            "description": { "type": "string", "maxLength": MAX_REQUEST_TEXT, "description": "Problem, where it was found and the proposed solution" },
            "reason": { "type": "string", "enum": REQUEST_REASONS, "default": "other" },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "critical"], "default": "normal" },
            "due_date": { "type": "string", "description": "Wanted by (YYYY-MM-DD), optional" },
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

pub(super) fn schema_comment_request() -> Value {
    object(
        json!({
            "request": p_request(),
            "body": { "type": "string", "maxLength": MAX_COMMENT_CHARS },
        }),
        &["request", "body"],
    )
}

pub(super) fn schema_propose_values() -> Value {
    object(
        json!({
            "item": { "type": "string", "description": "Simulation number, e.g. SIM-000003" },
            "revision": { "type": "string", "description": "Revision code (default: latest)" },
            "values": {
                "type": "array",
                "minItems": 1,
                "maxItems": result_proposals::MAX_PER_CALL,
                "items": object(
                    json!({
                        "name": { "type": "string", "maxLength": 200, "description": "Result name, e.g. Max. von Mises stress" },
                        "value": { "type": "number" },
                        "unit": { "type": "string", "maxLength": 32 },
                        "limit_min": { "type": "number" },
                        "limit_max": { "type": "number" },
                        "load_case": { "type": "string", "maxLength": 200 },
                        "solver": { "type": "string", "maxLength": 100 },
                        "solver_version": { "type": "string", "maxLength": 100 },
                        "file": { "type": "string", "description": "File name in the revision the value was read from" },
                        "quote": { "type": "string", "maxLength": result_proposals::MAX_QUOTE_CHARS, "description": "The text of the report that holds the value, exactly as read_file_text returned it" },
                    }),
                    &["name", "value", "file", "quote"],
                ),
            },
        }),
        &["item", "values"],
    )
}

pub(super) fn schema_compare_results() -> Value {
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

pub(super) fn schema_list_simulations() -> Value {
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
