//! Read access control end to end: a restricted project group with an item,
//! files, a BOM, references, a change order and a change request, linked to
//! an internal group. Two outsiders (a CAD editor of the internal group and
//! a viewer) call **every** web route, REST route and MCP tool; no marker of
//! the restricted group may appear anywhere, hidden objects answer "not
//! found", and nothing changes. The routes are read from the router sources,
//! so a new route is exercised (or fails the test for want of a placeholder
//! rule) without anybody having to remember it.

mod common;

use std::collections::BTreeSet;

use axum::http::{header, Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::domain::Role;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------
// Route table from the sources
// ---------------------------------------------------------------------------

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// `(METHOD, path)` of every `.route("…", …)` in a router source.
fn routes_of(source: &str, prefix: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    while let Some(found) = source[at..].find(".route(") {
        let start = at + found + ".route(".len();
        // the call ends where the parentheses balance again
        let mut depth = 1;
        let mut end = start;
        while end < bytes.len() && depth > 0 {
            match bytes[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        let call = &source[start..end];
        at = end;
        let Some(q1) = call.find('"') else { continue };
        let Some(q2) = call[q1 + 1..].find('"') else {
            continue;
        };
        let path = &call[q1 + 1..q1 + 1 + q2];
        let rest = &call[q1 + 2 + q2..];
        for m in METHODS {
            let needle = format!("{m}(");
            let mut from = 0;
            while let Some(i) = rest[from..].find(&needle) {
                let i = from + i;
                let before = rest[..i].chars().last();
                if before.is_none_or(|c| !(c.is_alphanumeric() || c == '_')) {
                    out.push((m.to_ascii_uppercase(), format!("{prefix}{path}")));
                    break;
                }
                from = i + needle.len();
            }
        }
    }
    out
}

fn all_routes() -> Vec<(String, String)> {
    let root = env!("CARGO_MANIFEST_DIR");
    let read = |p: &str| std::fs::read_to_string(format!("{root}/{p}")).expect("router source");
    let mut routes = routes_of(&read("src/web/mod.rs"), "");
    routes.extend(routes_of(&read("src/api/mod.rs"), "/api/v1"));
    routes.extend(routes_of(&read("src/lib.rs"), ""));
    routes.sort();
    routes.dedup();
    routes
}

/// Routes that serve no repository data (or would end the session).
fn skipped(method: &str, path: &str) -> bool {
    path.starts_with("/static/")
        || path == "/favicon.ico"
        || (method == "POST" && path == "/logout")
}

/// The identifiers of the restricted fixture.
struct Hidden {
    group_code: String,
    group_id: Uuid,
    item_number: String,
    item_id: Uuid,
    revision_id: Uuid,
    file_id: Uuid,
    version_id: Uuid,
    line_id: Uuid,
    reference_id: Uuid,
    change_number: String,
    change_id: Uuid,
    request_number: String,
    request_id: Uuid,
    attachment_id: Uuid,
    /// An open impact on the hidden item.
    impact_id: Uuid,
    /// A result value of a simulation in the group.
    result_id: Uuid,
    /// A member's notification about the hidden item.
    notification_id: Uuid,
    /// A member's staged BOM import.
    import_id: Uuid,
    /// The caller (for `{user_id}`).
    caller: Uuid,
}

/// Fills a route template with the hidden objects; `None` for a
/// placeholder without a rule (a new route kind – add a rule).
fn fill(path: &str, h: &Hidden) -> Option<String> {
    let random = Uuid::new_v4().to_string();
    let segments: Vec<&str> = path.split('/').collect();
    let mut out = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if !(seg.starts_with('{') && seg.ends_with('}')) {
            out.push(seg.to_string());
            continue;
        }
        let before = if i > 0 { segments[i - 1] } else { "" };
        let value = match (*seg, before) {
            ("{number}", "items") | ("{number}", "i") => h.item_number.clone(),
            ("{number}", "changes") => h.change_number.clone(),
            ("{number}", "change-requests") => h.request_number.clone(),
            ("{code}", "revisions") => "A".into(),
            ("{code}", "projects") => h.group_code.clone(),
            ("{id}", "items") => h.item_id.to_string(),
            ("{id}", "revisions") => h.revision_id.to_string(),
            ("{id}", "files") => h.file_id.to_string(),
            ("{id}", "file-versions") => h.version_id.to_string(),
            ("{id}", "changes") => h.change_id.to_string(),
            ("{id}", "requests") => h.request_id.to_string(),
            ("{id}", "request-attachments") | ("{id}", "change-request-attachments") => {
                h.attachment_id.to_string()
            }
            ("{id}", "projects") => h.group_id.to_string(),
            ("{id}", "bom-lines") => h.line_id.to_string(),
            ("{id}", "references") => h.reference_id.to_string(),
            ("{id}", "impacts") => h.impact_id.to_string(),
            ("{id}", "simulation-results") => h.result_id.to_string(),
            ("{id}", "notifications") => h.notification_id.to_string(),
            ("{id}", "import") => h.import_id.to_string(),
            ("{revision_id}", _) => h.revision_id.to_string(),
            ("{item_id}", _) => h.item_id.to_string(),
            ("{user_id}", _) => h.caller.to_string(),
            // attribute definitions belong to no project group
            ("{key}", "attributes") => "vendor".into(),
            // an alternate: by number in the REST API, by id in the web UI
            ("{alternate}", _) if path.starts_with("/api/") => h.item_number.clone(),
            ("{alternate}", _) => h.item_id.to_string(),
            // objects of no fixture: unknown ids must not reveal anything either
            ("{id}", _) | ("{delivery_id}", _) => random.clone(),
            _ => return None,
        };
        out.push(value);
    }
    Some(out.join("/"))
}

#[test]
fn every_route_has_a_placeholder_rule() {
    let routes = all_routes();
    assert!(routes.len() > 150, "found the routes: {}", routes.len());
    assert!(routes.contains(&("GET".into(), "/api/v1/items/{number}".into())));
    assert!(routes.contains(&("POST".into(), "/revisions/{id}/checkout".into())));
    let h = Hidden {
        group_code: "G".into(),
        group_id: Uuid::nil(),
        item_number: "X".into(),
        item_id: Uuid::nil(),
        revision_id: Uuid::nil(),
        file_id: Uuid::nil(),
        version_id: Uuid::nil(),
        line_id: Uuid::nil(),
        reference_id: Uuid::nil(),
        change_number: "C".into(),
        change_id: Uuid::nil(),
        request_number: "R".into(),
        request_id: Uuid::nil(),
        attachment_id: Uuid::nil(),
        impact_id: Uuid::nil(),
        result_id: Uuid::nil(),
        notification_id: Uuid::nil(),
        import_id: Uuid::nil(),
        caller: Uuid::nil(),
    };
    let missing: Vec<String> = routes
        .iter()
        .filter(|(m, p)| !skipped(m, p) && fill(p, &h).is_none())
        .map(|(m, p)| format!("{m} {p}"))
        .collect();
    assert!(
        missing.is_empty(),
        "routes with a placeholder the leak test cannot fill – add a rule to fill():\n{}",
        missing.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// The first marker found in `text`, ignoring identifiers the caller sent
/// itself (`sent`: path segments and argument values).
fn leak(text: &str, markers: &[String], sent: &[String]) -> Option<String> {
    let mut text = text.to_lowercase();
    for s in sent {
        if !s.is_empty() {
            text = text.replace(&s.to_lowercase(), "");
        }
    }
    markers
        .iter()
        .find(|m| text.contains(&m.to_lowercase()))
        .cloned()
}

/// Decodes `%XX` escapes (flash cookies and redirect targets are encoded).
fn pct_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sent_of(uri: &str) -> Vec<String> {
    uri.split(['/', '?', '&', '='])
        .filter(|s| s.len() > 3)
        .map(str::to_string)
        .collect()
}

/// The text of a response, with ZIP archives (Pack and Go, XLSX) unpacked.
fn text_of(r: &common::Response) -> String {
    let mut text = r.text();
    if r.body.starts_with(b"PK") {
        if let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(r.body.clone())) {
            for i in 0..zip.len() {
                if let Ok(mut entry) = zip.by_index(i) {
                    text.push('\n');
                    text.push_str(&entry.name().map(|n| n.to_string()).unwrap_or_default());
                    let mut bytes = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut entry, &mut bytes);
                    text.push('\n');
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
    }
    text
}

/// Whether a response refuses the call: an HTTP error, a redirect (the web
/// UI's flash message) or an MCP / JSON-RPC error. Only a refusal may echo
/// the identifiers the caller sent ("Unknown item X"); a successful answer
/// that contains a hidden number or id leaks it, even if it was sent.
fn is_refusal(r: &common::Response) -> bool {
    if !r.status.is_success() {
        return true;
    }
    let Ok(v) = serde_json::from_slice::<Value>(&r.body) else {
        return false;
    };
    v.get("error").is_some() || v["result"]["isError"] == true
}

fn assert_clean(who: &str, what: &str, r: &common::Response, markers: &[String], sent: &[String]) {
    let sent: &[String] = if is_refusal(r) { sent } else { &[] };
    assert_clean_echoing(who, what, r, markers, sent);
}

/// [`assert_clean`] for an answer that repeats what the caller sent by
/// design (an MCP prompt is a template filled with its arguments).
fn assert_clean_echoing(
    who: &str,
    what: &str,
    r: &common::Response,
    markers: &[String],
    sent: &[String],
) {
    let mut seen = text_of(r);
    for name in [
        header::LOCATION,
        header::SET_COOKIE,
        header::CONTENT_DISPOSITION,
    ] {
        for v in r.headers.get_all(name) {
            seen.push('\n');
            seen.push_str(&pct_decode(v.to_str().unwrap_or("")));
        }
    }
    if let Some(m) = leak(&seen, markers, sent) {
        panic!(
            "{who}: {what} leaks {m} (status {}):\n{}",
            r.status,
            &seen[..seen.len().min(2000)]
        );
    }
}

const COUNTED: [&str; 22] = [
    "simulation_results",
    "staged_imports",
    "items",
    "revisions",
    "files",
    "file_versions",
    "bom_lines",
    "item_references",
    "changes",
    "change_items",
    "change_reviews",
    "change_requests",
    "change_request_items",
    "change_request_comments",
    "change_request_attachments",
    "change_request_changes",
    "simulation_bases",
    "watches",
    "favorites",
    "project_members",
    "impacts",
    "audit_log",
];

async fn counts(app: &common::TestApp) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in COUNTED {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(&app.pool)
            .await
            .expect("count");
        out.push((t.to_string(), n));
    }
    // a restricted group stays restricted
    let restricted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE visibility = 'restricted'")
            .fetch_one(&app.pool)
            .await
            .expect("count");
    out.push(("restricted groups".into(), restricted));
    out
}

async fn post_ok(app: &common::TestApp, uri: &str, token: &str, body: Value) -> Value {
    let r = app.api(Method::POST, uri, token, Some(body)).await;
    assert!(r.status.is_success(), "{uri}: {} {}", r.status, r.text());
    r.json()
}

async fn id_of(app: &common::TestApp, sql: &str, arg: &str) -> Uuid {
    sqlx::query_scalar(sql)
        .bind(arg)
        .fetch_one(&app.pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn outsiders_see_nothing_of_a_restricted_group() {
    let app = app_or_skip!();
    let m = format!("zq{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mu = m.to_ascii_uppercase();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, editor) = app.user("member_editor", Role::Editor).await;
    let (_, viewer) = app.user("member_viewer", Role::Viewer).await;
    let (out_editor_actor, out_editor) = app
        .user_in("outsider_editor", Role::Editor, &["ROVER"])
        .await;
    let (out_viewer_actor, out_viewer) = app.user("outsider_viewer", Role::Viewer).await;

    // --- the group (internal while the links are made), its members -------
    let code = format!("G{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &editor,
        json!({ "code": code, "name": format!("Group {m}"), "description": format!("about {m}") }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/projects/{code}/members"),
        &editor,
        json!({ "username": "member_viewer" }),
    )
    .await;

    // --- the restricted item with a file -------------------------------------
    let hid = format!("LK{mu}");
    post_ok(
        &app,
        "/api/v1/items",
        &editor,
        json!({ "item_type": "assembly", "number": hid, "name": format!("Hidden {m}"),
                "description": format!("text {m}"), "material": format!("alloy {m}"),
                "project": code }),
    )
    .await;
    let hid_rev = format!("/api/v1/items/{hid}/revisions/A");
    let r = app
        .api_upload(
            &format!("{hid_rev}/files"),
            &editor,
            &format!("{m}.txt"),
            format!("secret content {m}").as_bytes(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file_id: Uuid = r.json()[0]["id"].as_str().unwrap().parse().unwrap();

    // --- an internal part used by the hidden item, an internal assembly using it
    let prt = post_ok(
        &app,
        "/api/v1/items",
        &out_editor,
        json!({ "item_type": "part", "name": "Open part", "project": "ROVER" }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    app.api(
        Method::POST,
        &format!("/api/v1/items/{prt}/revisions/A/checkin"),
        &out_editor,
        None,
    )
    .await;
    let line = post_ok(
        &app,
        &format!("{hid_rev}/bom"),
        &editor,
        json!({ "child": prt, "quantity": 2.0, "notes": format!("note {m}") }),
    )
    .await;
    let line_id: Uuid = line["id"].as_str().unwrap().parse().unwrap();
    let reference = post_ok(
        &app,
        &format!("{hid_rev}/references"),
        &editor,
        json!({ "item": prt, "kind": "reference", "note": format!("ref {m}") }),
    )
    .await;
    let reference_id: Uuid = reference["id"].as_str().unwrap().parse().unwrap();
    app.api(Method::POST, &format!("{hid_rev}/checkin"), &editor, None)
        .await;
    let asm = post_ok(
        &app,
        "/api/v1/items",
        &out_editor,
        json!({ "item_type": "assembly", "name": "Open assembly", "project": "ROVER" }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    let asm_rev = format!("/api/v1/items/{asm}/revisions/A");
    post_ok(
        &app,
        &format!("{asm_rev}/bom"),
        &out_editor,
        json!({ "child": hid, "quantity": 3.0, "find_number": 30,
                "notes": format!("the hidden line {m}") }),
    )
    .await;
    post_ok(
        &app,
        &format!("{asm_rev}/references"),
        &out_editor,
        json!({ "item": hid, "kind": "drawing", "note": format!("see sheet 2 {m}") }),
    )
    .await;
    app.api(
        Method::POST,
        &format!("{asm_rev}/checkin"),
        &out_editor,
        None,
    )
    .await;

    // --- a change order, a change request with comment and attachment -------
    let chg = post_ok(
        &app,
        "/api/v1/changes",
        &editor,
        json!({ "title": format!("Change {m}"), "description": format!("why {m}") }),
    )
    .await["number"]
        .as_str()
        .unwrap()
        .to_string();
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg}/items"),
        &editor,
        json!({ "item": hid }),
    )
    .await;
    let ecr = post_ok(
        &app,
        "/api/v1/change-requests",
        &editor,
        json!({ "title": format!("Request {m}"), "description": format!("problem {m}"),
                "items": [{ "item": hid }] }),
    )
    .await["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr}/comments"),
        &editor,
        json!({ "body": format!("comment {m}") }),
    )
    .await;
    let r = app
        .api_upload(
            &format!("/api/v1/change-requests/{ecr}/attachments"),
            &editor,
            &format!("{m}.pdf"),
            format!("%PDF-1.4 {m}").as_bytes(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let attachment_id: Uuid = r.json()["id"].as_str().unwrap().parse().unwrap();

    // --- the outsider watched the item while it was open; webhooks exist -----
    post_ok(
        &app,
        &format!("/api/v1/items/{hid}/watch"),
        &out_editor,
        json!({}),
    )
    .await;
    let mut hooks = Vec::new();
    for project in [Value::Null, json!("ROVER")] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/webhooks",
                &admin,
                Some(json!({ "name": "ERP", "url": "https://ci.example.org/hook",
                             "events": ["change_request.created", "change.submitted"],
                             "project": project })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        hooks.push(
            r.json()["webhook"]["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| r.json()["id"].as_str().unwrap().to_string()),
        );
    }

    // --- restrict the group (managers may, other users may not) --------------
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            &out_editor,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    // still internal: the outsider sees the group, but is no manager
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            &editor,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["visibility"], "restricted");

    // --- objects of the group that routes address by their own id ------------
    let sim = post_ok(
        &app,
        "/api/v1/items",
        &editor,
        json!({ "item_type": "simulation", "name": "Load case", "project": code }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{sim}/revisions/A/simulation/results"),
            &editor,
            Some(json!({ "results": [{ "name": "Stress", "value": 182.5, "unit": "MPa" }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let result_id = id_of(
        &app,
        "SELECT sr.id FROM simulation_results sr JOIN revisions r ON r.id = sr.revision_id
         JOIN items i ON i.id = r.item_id WHERE i.number = $1",
        &sim,
    )
    .await;
    let impact_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id,
                              event, relation, detail)
         SELECT $1, i.id, r.id, si.id, sr.id, 'released', 'bom', $4
         FROM items i JOIN revisions r ON r.item_id = i.id,
              items si JOIN revisions sr ON sr.item_id = si.id
         WHERE i.number = $2 AND si.number = $3",
    )
    .bind(impact_id)
    .bind(&hid)
    .bind(&prt)
    .bind(format!("impact {m}"))
    .execute(&app.pool)
    .await
    .unwrap();
    let member_id = app.actor("member_editor").await.id;
    let hid_rev_id = id_of(
        &app,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
        &hid,
    )
    .await;
    let notification_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO notifications (id, user_id, kind, title, link, subject_type, subject_ref,
                                    subject_kind, subject_id)
         SELECT $1, $2, 'item_update', $3, $4, 'revision', $5, 'item', i.id
         FROM items i WHERE i.number = $6",
    )
    .bind(notification_id)
    .bind(member_id)
    .bind(format!("Notice {m}"))
    .bind(format!("/revisions/{hid_rev_id}"))
    .bind(format!("{hid}/A"))
    .bind(&hid)
    .execute(&app.pool)
    .await
    .unwrap();
    let import_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO staged_imports (id, user_id, kind, filename, size_bytes, content, options)
         VALUES ($1, $2, 'bom_table', $3, 7, $4, $5)",
    )
    .bind(import_id)
    .bind(member_id)
    .bind(format!("{m}.csv"))
    .bind(b"a;b;c\n1".to_vec())
    .bind(json!({ "target": hid_rev_id }))
    .execute(&app.pool)
    .await
    .unwrap();
    // what the outsiders' calls must leave as it is (counts do not show it)
    let id_rows = || {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT concat_ws(' | ',
                    (SELECT row_to_json(x)::text FROM impacts x WHERE x.id = $1),
                    (SELECT row_to_json(x)::text FROM simulation_results x WHERE x.id = $2),
                    (SELECT row_to_json(x)::text FROM notifications x WHERE x.id = $3),
                    (SELECT x.id::text FROM staged_imports x WHERE x.id = $4))",
            )
            .bind(impact_id)
            .bind(result_id)
            .bind(notification_id)
            .bind(import_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };

    let hidden_base = Hidden {
        group_code: code.clone(),
        group_id: id_of(&app, "SELECT id FROM projects WHERE code = $1", &code).await,
        item_number: hid.clone(),
        item_id: id_of(&app, "SELECT id FROM items WHERE number = $1", &hid).await,
        revision_id: id_of(
            &app,
            "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
            &hid,
        )
        .await,
        file_id,
        version_id: id_of(
            &app,
            "SELECT id FROM file_versions WHERE file_id = $1::text::uuid",
            &file_id.to_string(),
        )
        .await,
        line_id,
        reference_id,
        change_number: chg.clone(),
        change_id: id_of(&app, "SELECT id FROM changes WHERE number = $1", &chg).await,
        request_number: ecr.clone(),
        request_id: id_of(
            &app,
            "SELECT id FROM change_requests WHERE number = $1",
            &ecr,
        )
        .await,
        attachment_id,
        impact_id,
        result_id,
        notification_id,
        import_id,
        caller: Uuid::nil(),
    };
    // (the item number and the group code contain `m`)
    let markers: Vec<String> = vec![
        m.clone(),
        chg.clone(),
        ecr.clone(),
        hidden_base.group_id.to_string(),
        hidden_base.item_id.to_string(),
        hidden_base.revision_id.to_string(),
        file_id.to_string(),
        hidden_base.version_id.to_string(),
        hidden_base.change_id.to_string(),
        hidden_base.request_id.to_string(),
        attachment_id.to_string(),
        impact_id.to_string(),
        result_id.to_string(),
        notification_id.to_string(),
        import_id.to_string(),
        sim.clone(),
    ];

    // --- positive control: members and admins see it -------------------------
    for token in [&viewer, &editor, &admin] {
        let r = app
            .api(Method::GET, &format!("/api/v1/items/{hid}"), token, None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(r.text().contains(&m), "members see the item");
        let r = app
            .api(Method::GET, &format!("/api/v1/items?q={m}"), token, None)
            .await;
        assert_eq!(r.json()["total"], 1, "{}", r.text());
    }

    // --- events in the restricted group after the restriction ---------------
    let deliveries = |id: String| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE webhook_id = $1::text::uuid",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before_hooks = (
        deliveries(hooks[0].clone()).await,
        deliveries(hooks[1].clone()).await,
    );
    post_ok(
        &app,
        "/api/v1/change-requests",
        &editor,
        json!({ "title": format!("Second {m}"), "items": [{ "item": hid }] }),
    )
    .await;
    app.api(Method::POST, &format!("{hid_rev}/checkout"), &editor, None)
        .await;
    app.api(Method::POST, &format!("{hid_rev}/checkin"), &editor, None)
        .await;
    assert_eq!(
        before_hooks,
        (
            deliveries(hooks[0].clone()).await,
            deliveries(hooks[1].clone()).await
        ),
        "neither the global nor the ROVER webhook receives restricted events"
    );

    // --- targeted checks -------------------------------------------------------
    for (who, token) in [
        ("outsider_editor", &out_editor),
        ("outsider_viewer", &out_viewer),
    ] {
        let r = app
            .api(Method::GET, &format!("/api/v1/items/{hid}"), token, None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{who}");
        let unknown = app
            .api(Method::GET, "/api/v1/items/LKNOPE0000", token, None)
            .await;
        assert_eq!(
            r.text().replace(&hid, "X"),
            unknown.text().replace("LKNOPE0000", "X"),
            "{who}: hidden and unknown items answer alike"
        );
        let r = app
            .api(Method::GET, &format!("/api/v1/items?q={m}"), token, None)
            .await;
        assert_eq!(r.json()["total"], 0, "{who}: search (incl. file content)");
        let r = app
            .api(Method::GET, "/api/v1/items?q=secret%20content", token, None)
            .await;
        assert_clean(who, "content search", &r, &markers, &[]);
        let r = app
            .api(Method::GET, &format!("{asm_rev}/bom"), token, None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.text());
        let lines = r.json();
        let line = lines
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["find_number"] == 30)
            .expect("the line stays")
            .clone();
        assert_eq!(line["restricted"], true, "{who}: placeholder {line}");
        assert_eq!(line["quantity"], 3.0);
        // the notes describe the hidden child
        assert_eq!(line["notes"], "", "{who}: {line}");
        let r = app
            .api(
                Method::GET,
                &format!("{asm_rev}/bom?expand=true"),
                token,
                None,
            )
            .await;
        assert_eq!(r.json()["restricted_lines"], 1, "{who}: {}", r.text());
        assert_eq!(r.json()["rollup_incomplete"], true);
        assert_clean(who, "expanded BOM", &r, &markers, &[]);
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/items/{prt}/where-used"),
                token,
                None,
            )
            .await;
        assert_eq!(r.json().as_array().unwrap().len(), 0, "{who}: {}", r.text());
        assert_eq!(r.header("x-hidden-count").as_deref(), Some("1"), "{who}");
        let r = app.api(Method::GET, "/api/v1/me", token, None).await;
        assert!(r.json().get("hidden_projects").is_none());
        assert_clean(who, "/me", &r, &markers, &[]);
        // file bytes, also with a conditional request
        for uri in [
            format!("/api/v1/files/{file_id}/content"),
            format!("/files/{file_id}/thumbnail"),
        ] {
            let request = axum::http::Request::builder()
                .uri(&uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::IF_NONE_MATCH, "\"x\"")
                .body(axum::body::Body::empty())
                .unwrap();
            let r = app.send(request).await;
            assert!(
                !r.status.is_success() && r.status != StatusCode::NOT_MODIFIED,
                "{who} {uri}"
            );
            assert!(r.header("x-content-sha256").is_none());
        }
        let r = app
            .api(Method::GET, "/api/v1/notifications", token, None)
            .await;
        assert_clean(who, "notifications", &r, &markers, &[]);
    }
    // a non-manager member of another group cannot copy or move it out either
    let r = app
        .api(
            Method::POST,
            &format!("{hid_rev}/save-as"),
            &out_editor,
            Some(json!({ "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());

    // --- every route, as both outsiders ----------------------------------------
    let routes = all_routes();
    let before = counts(&app).await;
    let before_rows = id_rows().await;
    assert_eq!(before_rows.matches(" | ").count(), 3, "{before_rows}");
    for (who, token, actor) in [
        ("outsider_editor", &out_editor, &out_editor_actor),
        ("outsider_viewer", &out_viewer, &out_viewer_actor),
    ] {
        let cookie = app.login(who).await;
        let h = Hidden {
            caller: actor.id,
            ..clone_hidden(&hidden_base)
        };
        for (method, path) in &routes {
            if skipped(method, path) || !path.contains('{') {
                continue;
            }
            let uri = fill(path, &h).expect("rule (see every_route_has_a_placeholder_rule)");
            let r = call(&app, method, &uri, token, &cookie, &h).await;
            let what = format!("{method} {path}");
            let mut sent = sent_of(&uri);
            sent.extend(probe_sent(&h));
            assert_clean(who, &what, &r, &markers, &sent);
            if method == "GET" && targets_hidden(&uri, &h) {
                assert!(
                    !(r.status.is_success() || r.status.is_redirection()),
                    "{who}: {what} answers {} for a hidden object: {}",
                    r.status,
                    r.text()
                );
            }
        }
    }
    assert_eq!(before, counts(&app).await, "outsiders changed nothing");
    assert_eq!(
        before_rows,
        id_rows().await,
        "impact, result, notification and import unchanged"
    );
    // routes without a path parameter: lists, searches, forms, creation
    // (no count check here: tokens, saved searches and the like are created)
    for (who, token, actor) in [
        ("outsider_editor", &out_editor, &out_editor_actor),
        ("outsider_viewer", &out_viewer, &out_viewer_actor),
    ] {
        let cookie = app.login(who).await;
        let h = Hidden {
            caller: actor.id,
            ..clone_hidden(&hidden_base)
        };
        let sent = probe_sent(&h);
        for (method, path) in &routes {
            if skipped(method, path) || path.contains('{') {
                continue;
            }
            let r = call(&app, method, path, token, &cookie, &h).await;
            assert_clean(who, &format!("{method} {path}"), &r, &markers, &sent);
        }
        // (a fresh session: a route above may have ended the old one)
        let cookie = app.login(who).await;
        // the internal items that use and are used by the hidden one
        for number in [&asm, &prt] {
            let rev_id = id_of(
                &app,
                "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
                number,
            )
            .await;
            for uri in [
                format!("/revisions/{rev_id}"),
                format!("/revisions/{rev_id}/bom"),
                format!("/revisions/{rev_id}/bom.csv"),
                format!("/revisions/{rev_id}/package.zip"),
            ] {
                let r = app.get_page(&uri, Some(&cookie)).await;
                assert!(r.status.is_success(), "{who} {uri}: {}", r.status);
                assert_clean(who, &uri, &r, &markers, &[]);
            }
            for uri in [
                format!("/api/v1/items/{number}"),
                format!("/api/v1/items/{number}/history"),
                format!("/api/v1/items/{number}/revisions/A"),
                format!("/api/v1/items/{number}/revisions/A/references"),
                format!("/api/v1/items/{number}/revisions/A/workset"),
                format!("/api/v1/items/{number}/revisions/A/package"),
                format!("/api/v1/items/{number}/where-used?levels=5"),
            ] {
                let r = app.api(Method::GET, &uri, token, None).await;
                assert!(r.status.is_success(), "{who} {uri}: {}", r.status);
                assert_clean(who, &uri, &r, &markers, &[]);
            }
            for name in [
                "get_item",
                "get_revision",
                "get_bom",
                "where_used",
                "item_history",
                "bom_health",
            ] {
                let r = app
                    .api(
                        Method::POST,
                        "/api/v1/mcp",
                        token,
                        Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                     "params": { "name": name, "arguments": { "item": number } } })),
                    )
                    .await;
                assert_clean(who, &format!("MCP {name} {number}"), &r, &markers, &[]);
            }
        }
        for uri in [
            "/vault?folder=locked".to_string(),
            "/vault?folder=none".to_string(),
            format!("/vault?folder={}", hidden_base.group_id),
            "/items?q=hidden".to_string(),
            format!("/requests?item={hid}"),
            "/notifications".to_string(),
        ] {
            let r = app.get_page(&uri, Some(&cookie)).await;
            // (a filter form shows the value typed in)
            assert_clean_echoing(who, &uri, &r, &markers, &sent_of(&uri));
        }
    }

    // --- MCP: every tool and prompt --------------------------------------------
    let args = json!({
        "item": hid, "revision": "A", "with": "A", "change": chg, "request": ecr,
        "number": ecr, "q": "hidden", "query": "hidden", "items": [hid], "title": "probe",
        "body": "probe", "comment": "probe", "levels": 3, "project": code, "from": "A",
        "to": "A", "file": format!("{m}.txt"),
    });
    let sent = vec![
        hid.clone(),
        chg.clone(),
        ecr.clone(),
        code.clone(),
        format!("{m}.txt"),
    ];
    let tools: BTreeSet<String> = teamcraft::mcp::tools::definitions()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(tools.len() >= 22, "{tools:?}");
    for (who, token) in [
        ("outsider_editor", &out_editor),
        ("outsider_viewer", &out_viewer),
    ] {
        for name in &tools {
            let r = app
                .api(
                    Method::POST,
                    "/api/v1/mcp",
                    token,
                    Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                 "params": { "name": name, "arguments": args } })),
                )
                .await;
            assert_clean(who, &format!("MCP {name}"), &r, &markers, &sent);
        }
        // full text (also the file content), the group filter
        for search in [
            json!({ "q": m }),
            json!({ "q": "secret content" }),
            json!({ "project": code }),
        ] {
            let r = app
                .api(
                    Method::POST,
                    "/api/v1/mcp",
                    token,
                    Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                 "params": { "name": "search_items", "arguments": search } })),
                )
                .await;
            assert_clean(who, &format!("MCP search {search}"), &r, &markers, &sent);
        }
        for p in teamcraft::mcp::tools::prompts() {
            let r = app
                .api(
                    Method::POST,
                    "/api/v1/mcp",
                    token,
                    Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
                                 "params": { "name": p["name"], "arguments": args } })),
                )
                .await;
            assert_clean_echoing(who, "MCP prompt", &r, &markers, &sent);
        }
    }
    app.teardown().await;
}

fn clone_hidden(h: &Hidden) -> Hidden {
    Hidden {
        group_code: h.group_code.clone(),
        group_id: h.group_id,
        item_number: h.item_number.clone(),
        item_id: h.item_id,
        revision_id: h.revision_id,
        file_id: h.file_id,
        version_id: h.version_id,
        line_id: h.line_id,
        reference_id: h.reference_id,
        change_number: h.change_number.clone(),
        change_id: h.change_id,
        request_number: h.request_number.clone(),
        request_id: h.request_id,
        attachment_id: h.attachment_id,
        impact_id: h.impact_id,
        result_id: h.result_id,
        notification_id: h.notification_id,
        import_id: h.import_id,
        caller: h.caller,
    }
}

/// Whether a filled route addresses one of the hidden objects (and not
/// only a random id).
fn targets_hidden(uri: &str, h: &Hidden) -> bool {
    let ids = [
        h.group_id,
        h.item_id,
        h.revision_id,
        h.file_id,
        h.version_id,
        h.line_id,
        h.reference_id,
        h.change_id,
        h.request_id,
        h.attachment_id,
        h.impact_id,
        h.result_id,
        h.notification_id,
        // (not the staged import: an import that cannot be opened sends the
        // user back to the upload form with a message, like a gone one)
    ];
    let segments: Vec<&str> = uri.split('/').collect();
    ids.iter()
        .any(|id| segments.contains(&id.to_string().as_str()))
        || [
            &h.item_number,
            &h.change_number,
            &h.request_number,
            &h.group_code,
        ]
        .iter()
        .any(|n| segments.contains(&n.as_str()))
}

/// Body fields that name the hidden objects (BOM child, reference target,
/// simulation basis, compare partner, change to link, target group, …):
/// numbers for the REST API, ids for the web forms.
fn probe_fields(h: &Hidden) -> (Value, Vec<(&'static str, String)>) {
    let api = json!({
        "item": h.item_number, "child": h.item_number, "with": h.item_number,
        "basis": h.item_number, "target": h.item_number, "change": h.change_number,
        "request": h.request_number, "project": h.group_code,
        "item_id": h.item_id, "revision_id": h.revision_id, "change_id": h.change_id,
        "project_id": h.group_id, "file_id": h.file_id,
    });
    let form = vec![
        ("item_number", h.item_number.clone()),
        ("child", h.item_number.clone()),
        ("with", h.revision_id.to_string()),
        ("change", h.change_id.to_string()),
        ("item_id", h.item_id.to_string()),
        ("revision_id", h.revision_id.to_string()),
        ("change_id", h.change_id.to_string()),
        ("project_id", h.group_id.to_string()),
        ("file_id", h.file_id.to_string()),
    ];
    (api, form)
}

/// Identifiers the probe bodies send (a route may echo them).
fn probe_sent(h: &Hidden) -> Vec<String> {
    let (api, form) = probe_fields(h);
    let mut sent: Vec<String> = api
        .as_object()
        .unwrap()
        .values()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    sent.extend(form.into_iter().map(|(_, v)| v));
    sent
}

/// One call: the REST API with the token, the web UI with the session.
/// Writes carry the probe fields, so hidden ids in bodies are exercised too.
async fn call(
    app: &common::TestApp,
    method: &str,
    uri: &str,
    token: &str,
    cookie: &str,
    h: &Hidden,
) -> common::Response {
    let method = Method::from_bytes(method.as_bytes()).unwrap();
    let (api, form) = probe_fields(h);
    if uri.starts_with("/api/") {
        let body = (method != Method::GET).then_some(api);
        return app.api(method, uri, token, body).await;
    }
    if method == Method::GET {
        app.get_page(uri, Some(cookie)).await
    } else {
        let fields: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        app.post_form(uri, Some(cookie), Some("http://localhost:8080"), &fields)
            .await
    }
}

/// Restricted items without a group membership are hidden, but items
/// without any group stay visible to everybody; members who are viewers
/// read; the actor serialization never names hidden groups.
#[tokio::test]
async fn viewer_members_read_and_ungrouped_items_stay_visible() {
    let app = app_or_skip!();
    let (_, editor) = app.user("erika", Role::Editor).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let (_, other) = app.user("otto", Role::Viewer).await;
    post_ok(
        &app,
        "/api/v1/projects",
        &editor,
        json!({ "code": "SECRET", "name": "Secret", "visibility": "restricted" }),
    )
    .await;
    post_ok(
        &app,
        "/api/v1/projects/SECRET/members",
        &editor,
        json!({ "username": "vera" }),
    )
    .await;
    let number = post_ok(
        &app,
        "/api/v1/items",
        &editor,
        json!({ "item_type": "part", "name": "Classified", "project": "SECRET" }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "viewer members read");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}"),
            &other,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app.api(Method::GET, "/api/v1/projects", &other, None).await;
    assert!(!r.text().contains("SECRET"), "{}", r.text());
    let r = app.api(Method::GET, "/api/v1/me", &viewer, None).await;
    assert_eq!(r.json()["restricted_projects"].as_array().unwrap().len(), 1);
    // an item without a group is visible to everybody
    sqlx::query("UPDATE items SET project_id = NULL WHERE number = $1")
        .bind(&number)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}"),
            &other,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "items without a group stay visible"
    );
    app.teardown().await;
}

/// Only managers of a restricted group (and admins) change its visibility
/// or move items out of it; the change is audited.
#[tokio::test]
async fn visibility_is_managed_and_audited() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, manager) = app.user_in("mia", Role::Editor, &["ROVER"]).await;
    let (_, member) = app.user("max", Role::Editor).await;
    post_ok(
        &app,
        "/api/v1/projects",
        &manager,
        json!({ "code": "VAULT7", "name": "Vault seven" }),
    )
    .await;
    post_ok(
        &app,
        "/api/v1/projects/VAULT7/members",
        &manager,
        json!({ "username": "max" }),
    )
    .await;
    // a plain member may not restrict the group
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/VAULT7/visibility",
            &member,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/VAULT7/visibility",
            &manager,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'project.visibility'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
    // moving an item out of the restricted group: managers only
    let number = post_ok(
        &app,
        "/api/v1/items",
        &member,
        json!({ "item_type": "part", "name": "Inside", "project": "VAULT7" }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id)
         SELECT p.id, u.id FROM projects p, users u WHERE p.code = 'ROVER' AND u.username = 'max'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let rover = app.project_id("ROVER").await;
    let item = app.actor("max").await;
    let item_id: Uuid = sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(&number)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let moved = teamcraft::services::items::update_master(
        &app.pool,
        &item,
        item_id,
        Some(rover),
        "each",
        None,
    )
    .await;
    assert!(
        matches!(moved, Err(teamcraft::error::AppError::Forbidden(_))),
        "{moved:?}"
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{number}/revisions/A/save-as"),
            &member,
            Some(json!({ "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // admins open it again
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/VAULT7/visibility",
            &admin,
            Some(json!({ "visibility": "internal" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

async fn new_item(app: &common::TestApp, token: &str, item_type: &str, project: &str) -> String {
    post_ok(
        app,
        "/api/v1/items",
        token,
        json!({ "item_type": item_type, "name": "Item", "project": project }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string()
}

fn rev_of(number: &str) -> String {
    format!("/api/v1/items/{number}/revisions/A")
}

fn number_of(v: &Value, key: &str) -> String {
    v[key]["number"]
        .as_str()
        .or_else(|| v["number"].as_str())
        .unwrap()
        .to_string()
}

/// Texts that name other objects – audit entries, notifications, readiness,
/// conflict messages, request notes, numbering previews and lookups by id –
/// tell outsiders nothing about a restricted group (review findings).
#[tokio::test]
async fn texts_naming_restricted_objects_are_redacted() {
    let app = app_or_skip!();
    let m = format!("zr{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mu = m.to_ascii_uppercase();
    let (_, mona) = app.user_in("mona", Role::Editor, &["ROVER"]).await;
    let (rex_actor, _) = app.user("rex", Role::Editor).await;
    let (otto_actor, otto) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let code = format!("S{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &mona,
        json!({ "code": code, "name": "Secret group" }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/projects/{code}/members"),
        &mona,
        json!({ "username": "rex" }),
    )
    .await;
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/numbering"),
            &mona,
            Some(json!({ "scheme": "{PROJECT}-{TYPE}-{SEQ:4}" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // an internal part of otto (he watches it as its creator)
    let prt = new_item(&app, &otto, "part", "ROVER").await;
    app.api(
        Method::POST,
        &format!("{}/checkin", rev_of(&prt)),
        &otto,
        None,
    )
    .await;
    // two assemblies of the group use it (their numbers carry the code)
    let h1 = new_item(&app, &mona, "assembly", &code).await;
    let h2 = new_item(&app, &mona, "assembly", &code).await;
    for h in [&h1, &h2] {
        post_ok(
            &app,
            &format!("{}/bom", rev_of(h)),
            &mona,
            json!({ "child": prt, "quantity": 1.0 }),
        )
        .await;
    }
    // otto watches h1 while the group is internal and is told of its check-in
    post_ok(&app, &format!("/api/v1/items/{h1}/watch"), &otto, json!({})).await;
    let r = app
        .api_upload(
            &format!("{}/checkin", rev_of(&h1)),
            &mona,
            &format!("{m}.txt"),
            b"content",
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    app.api(
        Method::POST,
        &format!("{}/checkin", rev_of(&h2)),
        &mona,
        None,
    )
    .await;
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert!(r.text().contains(&h1), "told while internal: {}", r.text());
    // a request of otto on his part; mona adds h1 with a note
    let ecr = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &otto,
            json!({ "title": "Part problem", "items": [{ "item": prt }] }),
        )
        .await,
        "change_request",
    );
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr}/items"),
        &mona,
        json!({ "item": h1, "note": format!("cracks {m}") }),
    )
    .await;

    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            &mona,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let markers = vec![m.clone()];

    // older notifications about the group are hidden now, also from the count
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert_clean("otto", "old notifications", &r, &markers, &[]);
    assert_eq!(r.json()["unread"], 0, "{}", r.text());
    // the note on the hidden item is not shown to the requester
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr}"),
            &otto,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_clean("otto", "request with a restricted item", &r, &markers, &[]);

    // a change with the internal part and h1, released by rex
    let chg = number_of(
        &post_ok(
            &app,
            "/api/v1/changes",
            &mona,
            json!({ "title": format!("Change {m}") }),
        )
        .await,
        "change",
    );
    for n in [&prt, &h1] {
        post_ok(
            &app,
            &format!("/api/v1/changes/{chg}/items"),
            &mona,
            json!({ "item": n }),
        )
        .await;
    }
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg}/reviewers"),
        &mona,
        json!({ "username": "rex" }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg}/submit"),
        &mona,
        json!({}),
    )
    .await;
    let change_id = id_of(&app, "SELECT id FROM changes WHERE number = $1", &chg).await;
    // (loaded again: the membership was added after the user was created)
    let rex_actor = app.actor(&rex_actor.username).await;
    teamcraft::services::changes::review(
        &app.pool,
        &rex_actor,
        change_id,
        teamcraft::domain::Decision::Approved,
        "",
    )
    .await
    .expect("release");
    // the release flagged h2: the history of the part names it only for members
    for (who, token, sees) in [("mona", &mona, true), ("otto", &otto, false)] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/items/{prt}/history"),
                token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.text());
        let flagged =
            r.json().as_array().unwrap().iter().any(|e| {
                e["action"] == "impact.flag" && e["summary"].as_str().unwrap().contains(&h2)
            });
        assert_eq!(flagged, sees, "{who}: {}", r.text());
        if !sees {
            assert_clean(who, "part history", &r, &markers, &[]);
        }
    }
    // the release notice of the watched part names neither change nor title
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert!(
        r.text().contains(&format!("{prt}/A was released")),
        "{}",
        r.text()
    );
    assert_clean("otto", "release notice", &r, &markers, &[]);
    assert!(!r.text().contains(&chg), "{}", r.text());
    let cookie = app.login("otto").await;
    let r = app.get_page("/", Some(&cookie)).await;
    assert_clean("otto", "dashboard activity", &r, &markers, &[]);

    // an open change hidden from otto holds his second part
    let prt2 = new_item(&app, &otto, "part", "ROVER").await;
    app.api(
        Method::POST,
        &format!("{}/checkin", rev_of(&prt2)),
        &otto,
        None,
    )
    .await;
    let h3 = new_item(&app, &mona, "part", &code).await;
    let chg2 = number_of(
        &post_ok(
            &app,
            "/api/v1/changes",
            &mona,
            json!({ "title": format!("Second {m}") }),
        )
        .await,
        "change",
    );
    for n in [&prt2, &h3] {
        post_ok(
            &app,
            &format!("/api/v1/changes/{chg2}/items"),
            &mona,
            json!({ "item": n }),
        )
        .await;
    }
    let chg2_id = id_of(&app, "SELECT id FROM changes WHERE number = $1", &chg2).await;
    let chg3 = number_of(
        &post_ok(&app, "/api/v1/changes", &otto, json!({ "title": "Mine" })).await,
        "change",
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg3}/items"),
            &otto,
            Some(json!({ "item": prt2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(!r.text().contains(&chg2), "{}", r.text());
    assert!(r.text().contains("another open change"), "{}", r.text());
    // the readiness of otto's assembly using that part does not name it either
    let asm = new_item(&app, &otto, "assembly", "ROVER").await;
    post_ok(
        &app,
        &format!("{}/bom", rev_of(&asm)),
        &otto,
        json!({ "child": prt2, "quantity": 1.0 }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg3}/items"),
        &otto,
        json!({ "item": asm }),
    )
    .await;
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg3}/readiness"),
            &otto,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&chg2), "{}", r.text());
    assert!(
        r.text().contains("another open change order"),
        "{}",
        r.text()
    );

    // linking the hidden change by its id answers "not found" and links nothing
    let ecr2 = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &otto,
            json!({ "title": "Without items" }),
        )
        .await,
        "change_request",
    );
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr2}/triage"),
        &otto,
        json!({}),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr2}/decide"),
        &otto,
        json!({ "decision": "accept" }),
    )
    .await;
    let ecr2_id = id_of(
        &app,
        "SELECT id FROM change_requests WHERE number = $1",
        &ecr2,
    )
    .await;
    let r = app
        .post_form(
            &format!("/requests/{ecr2_id}/link-change"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("change", &chg2_id.to_string())],
        )
        .await;
    assert_clean(
        "otto",
        "link a hidden change",
        &r,
        &[m.clone(), chg2.clone()],
        &[],
    );
    let linked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM change_request_changes WHERE request_id = $1")
            .bind(ecr2_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(linked, 0, "nothing is linked");

    // ids of hidden objects answer like unknown ones (the actor is loaded
    // again: its hidden groups are read at sign-in)
    let otto_actor = app.actor(&otto_actor.username).await;
    let h1_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &h1).await;
    let group_id = app.project_id(&code).await;
    let add = |item_id| {
        teamcraft::services::requests::add_item(
            &app.pool,
            &otto_actor,
            ecr2_id,
            teamcraft::services::requests::AffectedItem {
                item_id,
                revision_id: None,
                note: String::new(),
            },
        )
    };
    assert_eq!(
        add(h1_id).await.unwrap_err().to_string(),
        add(Uuid::new_v4()).await.unwrap_err().to_string(),
        "request item by id"
    );
    let create = |project_id| {
        teamcraft::services::items::create(
            &app.pool,
            &otto_actor,
            teamcraft::services::items::NewItem {
                number: None,
                item_type: teamcraft::domain::ItemType::Part,
                unit: "each".into(),
                project_id: Some(project_id),
                name: "Probe".into(),
                description: String::new(),
                material: String::new(),
                mass_kg: None,
                unit_cost: None,
                classification: Default::default(),
                attributes: Vec::new(),
            },
        )
    };
    let hidden = create(group_id).await.unwrap_err().to_string();
    assert_eq!(
        hidden,
        create(Uuid::new_v4()).await.unwrap_err().to_string()
    );
    assert!(!hidden.contains(&code), "{hidden}");
    let numbering =
        |id| teamcraft::services::projects::set_number_scheme(&app.pool, &otto_actor, id, "");
    assert_eq!(
        numbering(group_id).await.unwrap_err().to_string(),
        numbering(Uuid::new_v4()).await.unwrap_err().to_string(),
        "numbering of a hidden group"
    );

    // the assignee must see an item that is added later
    let ecr3 = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &mona,
            json!({ "title": "Assigned", "items": [{ "item": prt }] }),
        )
        .await,
        "change_request",
    );
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr3}/assign"),
        &mona,
        json!({ "username": "otto" }),
    )
    .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr3}/items"),
            &mona,
            Some(json!({ "item": h1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // a typed-in preview scheme never reads the group's private counter:
    // it shows the two assemblies made while the group was internal, not
    // the one made since
    new_item(&app, &mona, "assembly", &code).await;
    let scheme = format!("{code}-%7BTYPE%7D-%7BSEQ:4%7D");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/projects/{code}/numbering"),
            &mona,
            None,
        )
        .await;
    assert!(
        r.text().contains(&format!("{code}-ASM-0004")),
        "{}",
        r.text()
    );
    for (who, token, next) in [("otto", &otto, "0003"), ("mona", &mona, "0003")] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/projects/ROVER/numbering?scheme={scheme}"),
                token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.text());
        let number = r.json()["next"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["item_type"] == "assembly")
            .expect("assembly")["number"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(number, format!("{code}-ASM-{next}"), "{who}");
    }
    app.teardown().await;
}

/// Creates a change with `items`, `reviewer` as its reviewer, submits it and
/// has the reviewer approve it (release). Returns the change number and id.
async fn release(
    app: &common::TestApp,
    author: &str,
    reviewer: &str,
    items: &[&String],
) -> (String, Uuid) {
    let chg = number_of(
        &post_ok(
            app,
            "/api/v1/changes",
            author,
            json!({ "title": "Release" }),
        )
        .await,
        "change",
    );
    for n in items {
        post_ok(
            app,
            &format!("/api/v1/changes/{chg}/items"),
            author,
            json!({ "item": n }),
        )
        .await;
    }
    post_ok(
        app,
        &format!("/api/v1/changes/{chg}/reviewers"),
        author,
        json!({ "username": reviewer }),
    )
    .await;
    post_ok(
        app,
        &format!("/api/v1/changes/{chg}/submit"),
        author,
        json!({}),
    )
    .await;
    let id = id_of(app, "SELECT id FROM changes WHERE number = $1", &chg).await;
    let reviewer = app.actor(reviewer).await;
    teamcraft::services::changes::review(
        &app.pool,
        &reviewer,
        id,
        teamcraft::domain::Decision::Approved,
        "",
    )
    .await
    .expect("release");
    (chg, id)
}

async fn mcp_call(app: &common::TestApp, token: &str, name: &str, args: Value) -> common::Response {
    app.api(
        Method::POST,
        "/api/v1/mcp",
        token,
        Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": { "name": name, "arguments": args } })),
    )
    .await
}

/// Second review round: impacts and their releasing changes, writes on
/// placeholder rows, notes, list order, folder ids, Save As, deleted items,
/// numbering, webhooks queued before a restriction, the confidential flag
/// at delivery time, and the assistant's tool loop.
#[tokio::test]
async fn second_review_round_leaks_are_closed() {
    use teamcraft::assistant::{McpTools, Tools};

    let app = app_or_skip!();
    let m = format!("zs{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mu = m.to_ascii_uppercase();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, mona) = app.user_in("mona", Role::Editor, &["ROVER"]).await;
    app.user("rex", Role::Editor).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;
    let (_, otto) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let code = format!("S{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &mona,
        json!({ "code": code, "name": "Secret group" }),
    )
    .await;
    for user in ["rex", "vera"] {
        post_ok(
            &app,
            &format!("/api/v1/projects/{code}/members"),
            &mona,
            json!({ "username": user }),
        )
        .await;
    }
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/numbering"),
            &mona,
            Some(json!({ "scheme": "{PROJECT}-{TYPE}-{SEQ:4}" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // --- items: an internal part and assembly, hidden parts ----------------
    let prt = new_item(&app, &mona, "part", "ROVER").await;
    let asm = new_item(&app, &mona, "assembly", "ROVER").await;
    let hp = post_ok(
        &app,
        "/api/v1/items",
        &mona,
        json!({ "item_type": "part", "name": format!("Hidden {m}"), "project": code }),
    )
    .await["item_number"]
        .as_str()
        .unwrap()
        .to_string();
    let hq = new_item(&app, &mona, "part", &code).await;
    let r = app
        .api_upload(
            &format!("{}/files", rev_of(&hp)),
            &mona,
            "plain.txt",
            b"plain",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    post_ok(
        &app,
        &format!("{}/bom", rev_of(&asm)),
        &mona,
        json!({ "child": prt, "quantity": 1.0, "find_number": 10 }),
    )
    .await;
    post_ok(
        &app,
        &format!("{}/bom", rev_of(&asm)),
        &mona,
        json!({ "child": hp, "quantity": 2.0, "find_number": 20,
                "notes": format!("secret note {m}") }),
    )
    .await;
    post_ok(
        &app,
        &format!("{}/references", rev_of(&asm)),
        &mona,
        json!({ "item": hp, "kind": "drawing", "note": format!("ref note {m}") }),
    )
    .await;
    for n in [&prt, &asm, &hp, &hq] {
        let r = app
            .api(Method::POST, &format!("{}/checkin", rev_of(n)), &mona, None)
            .await;
        assert!(r.status.is_success(), "{n}: {}", r.text());
    }
    // otto's request on an internal part that sorts after the hidden one;
    // mona adds the hidden part while the group is still internal
    let zzz = format!("ZZZ-{}", &Uuid::new_v4().simple().to_string()[..6]).to_uppercase();
    post_ok(
        &app,
        "/api/v1/items",
        &otto,
        json!({ "item_type": "part", "number": zzz, "name": "Open", "project": "ROVER" }),
    )
    .await;
    let ecr_o = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &otto,
            json!({ "title": "Open request", "items": [{ "item": zzz }] }),
        )
        .await,
        "change_request",
    );
    post_ok(
        &app,
        &format!("/api/v1/change-requests/{ecr_o}/items"),
        &mona,
        json!({ "item": hp }),
    )
    .await;
    // a hidden item named by a request (deleting it is refused below)
    let hr = new_item(&app, &mona, "part", &code).await;
    post_ok(
        &app,
        "/api/v1/change-requests",
        &mona,
        json!({ "title": format!("About {m}"), "items": [{ "item": hr }] }),
    )
    .await;
    // vera watches hq; a global webhook for releases
    post_ok(&app, &format!("/api/v1/items/{hq}/watch"), &vera, json!({})).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &admin,
            Some(json!({ "name": "ERP", "url": "https://ci.example.org/hook",
                         "events": ["revision.released"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // --- hq released while the group is internal: queued, then restricted ---
    release(&app, &mona, "rex", &[&hq]).await;
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            &mona,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let group_id = app.project_id(&code).await;
    // the stored flag is stale; delivery evaluates it again
    let confidential: Vec<(bool, bool)> = sqlx::query_as(&format!(
        "SELECT n.confidential, {} FROM notifications n JOIN users u ON u.id = n.user_id
         WHERE u.username = 'vera' AND n.title LIKE '%' || $1 || '%'",
        teamcraft::services::notifications::confidential_now("n")
    ))
    .bind(&hq)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert!(!confidential.is_empty(), "vera was told of the release");
    assert!(
        confidential.iter().all(|(stored, now)| !stored && *now),
        "{confidential:?}"
    );
    // the queued delivery is withheld, not sent
    let dispatcher = teamcraft::webhooks::Dispatcher::new(
        "https://plm.example.org",
        &teamcraft::config::WebhookConfig::default(),
    )
    .unwrap();
    dispatcher.run_once(&app.pool).await.unwrap();
    let withheld: Vec<(String, String)> = sqlx::query_as(
        "SELECT status, last_error FROM webhook_deliveries
         WHERE event = 'revision.released' AND payload -> 'revision' ->> 'item_number' = $1",
    )
    .bind(&hq)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        withheld,
        vec![(
            "failed".to_string(),
            teamcraft::webhooks::WITHHELD_ERROR.to_string()
        )]
    );

    // --- a change with the internal part and a hidden one is released -------
    let (chg1, _) = release(&app, &mona, "rex", &[&prt, &hp]).await;
    let markers = vec![m.clone(), chg1.clone(), group_id.to_string()];
    // the internal revision's webhook event leaves the mixed change out
    let with_change: Vec<bool> = sqlx::query_scalar(
        "SELECT payload ? 'change' FROM webhook_deliveries
         WHERE event = 'revision.released' AND payload -> 'revision' ->> 'item_number' = $1",
    )
    .bind(&prt)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(with_change, vec![false]);
    let to_hidden: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_deliveries
         WHERE event = 'revision.released' AND payload -> 'revision' ->> 'item_number' = $1",
    )
    .bind(&hp)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(to_hidden, 0);

    // impacts on asm: the hidden source and the hidden change are redacted
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/impacts"),
            &mona,
            None,
        )
        .await;
    assert!(
        r.json()
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["change_number"] == chg1.as_str()),
        "members see the change: {}",
        r.text()
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/impacts"),
            &otto,
            None,
        )
        .await;
    let impacts = r.json();
    assert_eq!(impacts.as_array().unwrap().len(), 2, "{}", r.text());
    assert!(impacts
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["change_number"].is_null() && i["change_id"].is_null()));
    assert_clean("otto", "item impacts", &r, &markers, &[]);
    let r = app.api(Method::GET, "/api/v1/impacts", &otto, None).await;
    assert_clean("otto", "open impacts", &r, &markers, &[]);
    let r = app.api(Method::GET, "/api/v1/worklist", &otto, None).await;
    assert_clean("otto", "worklist", &r, &markers, &[]);
    let r = mcp_call(&app, &otto, "my_worklist", json!({})).await;
    assert_clean("otto", "MCP my_worklist", &r, &markers, &[]);
    let asm_rev_id = id_of(
        &app,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
        &asm,
    )
    .await;
    let cookie = app.login("otto").await;
    let r = app
        .get_page(&format!("/revisions/{asm_rev_id}"), Some(&cookie))
        .await;
    assert!(r.status.is_success(), "{}", r.status);
    assert_clean("otto", "revision page", &r, &markers, &[]);
    let r = app.get_page("/", Some(&cookie)).await;
    assert_clean("otto", "dashboard", &r, &markers, &[]);
    // notes of placeholder rows are gone (BOM, references)
    for uri in [
        format!("{}/bom", rev_of(&asm)),
        format!("{}/bom?expand=true", rev_of(&asm)),
        format!("{}/references", rev_of(&asm)),
    ] {
        let r = app.api(Method::GET, &uri, &otto, None).await;
        assert!(r.status.is_success(), "{uri}: {}", r.text());
        assert_clean("otto", &uri, &r, &markers, &[]);
    }
    // the request lists the placeholder after the visible item
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr_o}"),
            &otto,
            None,
        )
        .await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2, "{}", r.text());
    assert_eq!(items[0]["item_number"], zzz.as_str());
    assert_eq!(items[1]["restricted"], true);

    // "acknowledge all" answers redacted; the history names the real source
    let r = app
        .api(
            Method::POST,
            &format!("{}/impacts/acknowledge", rev_of(&asm)),
            &otto,
            Some(json!({ "note": "form, fit and function unchanged" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["acknowledged"].as_array().unwrap().len(), 2);
    assert_clean("otto", "acknowledge all", &r, &markers, &[]);

    // --- otto edits and removes the placeholder rows -------------------------
    let r = app
        .api(
            Method::POST,
            &format!("{}/checkout", rev_of(&asm)),
            &otto,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{}/bom", rev_of(&asm)), &otto, None)
        .await;
    let line_id = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["restricted"] == true)
        .expect("placeholder")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/bom-lines/{line_id}"),
            &otto,
            Some(json!({ "quantity": 5.0, "notes": "" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_clean("otto", "update placeholder", &r, &markers, &[]);
    let notes: String = sqlx::query_scalar("SELECT notes FROM bom_lines WHERE id = $1::text::uuid")
        .bind(&line_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(notes, format!("secret note {m}"), "the notes stay");
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/bom-lines/{line_id}"),
            &otto,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("{}/references", rev_of(&asm)),
            &otto,
            None,
        )
        .await;
    let ref_id = r.json()["outgoing"][0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/references/{ref_id}"),
            &otto,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_clean("otto", "remove placeholder reference", &r, &markers, &[]);
    let hp_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &hp).await;
    let related: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log
         WHERE action IN ('bom.update', 'bom.remove', 'reference.remove', 'impact.acknowledge')
           AND related_item_id = $1",
    )
    .bind(hp_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(related, 4, "the writes name the real item");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/history"),
            &mona,
            None,
        )
        .await;
    for action in ["bom.remove", "reference.remove", "impact.acknowledge"] {
        assert!(
            r.json()
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == action && e["summary"].as_str().unwrap().contains(&hp)),
            "members read {action} with the item: {}",
            r.text()
        );
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/history"),
            &otto,
            None,
        )
        .await;
    assert_clean("otto", "assembly history", &r, &markers, &[]);

    // --- folder ids: hidden and unknown answer alike -------------------------
    let hidden = app
        .get_page(&format!("/vault?folder={group_id}"), Some(&cookie))
        .await;
    let unknown = app
        .get_page(&format!("/vault?folder={}", Uuid::new_v4()), Some(&cookie))
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.status, hidden.status);

    // --- Save As out of the group: the copy does not name its source ---------
    let r = app
        .api(
            Method::POST,
            &format!("{}/save-as", rev_of(&hp)),
            &mona,
            Some(json!({ "project": "ROVER", "name": "Public copy" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let copy = r.json()["item_number"].as_str().unwrap().to_string();
    let hp_rev = id_of(
        &app,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
        &hp,
    )
    .await;
    let copy_markers = vec![m.clone(), hp_rev.to_string(), hp_id.to_string()];
    for uri in [
        format!("/api/v1/items/{copy}/history"),
        format!("/api/v1/items/{copy}/revisions/A"),
    ] {
        let r = app.api(Method::GET, &uri, &otto, None).await;
        assert!(r.status.is_success(), "{uri}: {}", r.text());
        assert_clean("otto", &uri, &r, &copy_markers, &[]);
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{copy}/revisions/A"),
            &otto,
            None,
        )
        .await;
    let file_id = r.json()["files"][0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/versions"),
            &otto,
            None,
        )
        .await;
    assert!(
        r.text().contains("Copied from a restricted item"),
        "{}",
        r.text()
    );
    assert_clean("otto", "copied file versions", &r, &copy_markers, &[]);
    let r = mcp_call(&app, &otto, "item_history", json!({ "item": copy })).await;
    assert_clean("otto", "MCP copy history", &r, &copy_markers, &[]);

    // --- a deleted hidden item stays redacted in the change history ----------
    let mona_actor = app.actor("mona").await;
    let prt2 = new_item(&app, &mona, "part", "ROVER").await;
    let hd = new_item(&app, &mona, "part", &code).await;
    let chg2 = number_of(
        &post_ok(&app, "/api/v1/changes", &mona, json!({ "title": "Mixed" })).await,
        "change",
    );
    for n in [&prt2, &hd] {
        post_ok(
            &app,
            &format!("/api/v1/changes/{chg2}/items"),
            &mona,
            json!({ "item": n }),
        )
        .await;
    }
    let chg2_id = id_of(&app, "SELECT id FROM changes WHERE number = $1", &chg2).await;
    let hd_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &hd).await;
    let hd_rev = id_of(
        &app,
        "SELECT id FROM revisions WHERE item_id = $1::text::uuid",
        &hd_id.to_string(),
    )
    .await;
    teamcraft::services::changes::remove_item(&app.pool, &mona_actor, chg2_id, hd_rev)
        .await
        .expect("remove from the change");
    teamcraft::services::items::delete(&app.pool, &mona_actor, hd_id)
        .await
        .expect("delete");
    let r = app
        .get_page(&format!("/changes/{chg2_id}"), Some(&cookie))
        .await;
    assert!(
        r.status.is_success(),
        "the change is visible now: {}",
        r.status
    );
    assert_clean("otto", "change of a deleted item", &r, &markers, &[]);
    let r = mcp_call(&app, &otto, "get_change", json!({ "change": chg2 })).await;
    assert_clean("otto", "MCP change of a deleted item", &r, &markers, &[]);
    // an item named by a request cannot be deleted (the request would open)
    let hr_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &hr).await;
    let deleted = teamcraft::services::items::delete(&app.pool, &mona_actor, hr_id).await;
    assert!(
        matches!(deleted, Err(teamcraft::error::AppError::Conflict(_))),
        "{deleted:?}"
    );

    // --- numbering: the restricted group's counter stays private ------------
    let ocode = format!("O{}", &Uuid::new_v4().simple().to_string()[..6]).to_uppercase();
    post_ok(
        &app,
        "/api/v1/projects",
        &otto,
        json!({ "code": ocode, "name": "Otto's group" }),
    )
    .await;
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{ocode}/numbering"),
            &otto,
            Some(json!({ "scheme": format!("{code}-{{TYPE}}-{{SEQ:4}}") })),
        )
        .await;
    // nothing is refused because of a restricted group (that would tell
    // about it); the restricted group counts privately instead: three
    // parts while it was internal, one since
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/projects/{ocode}/numbering"),
            &otto,
            None,
        )
        .await;
    assert!(
        r.text().contains(&format!("{code}-PRT-0004")),
        "{}",
        r.text()
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/projects/{code}/numbering"),
            &mona,
            None,
        )
        .await;
    assert!(
        r.text().contains(&format!("{code}-PRT-0005")),
        "{}",
        r.text()
    );
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{ocode}/numbering"),
            &otto,
            Some(json!({ "scheme": "{PROJECT}-{TYPE}-{SEQ:4}" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // --- the assistant's tools ---------------------------------------------
    let comments = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM change_request_comments")
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let tools = McpTools::new(&app.state, &mona_actor);
    let read = tools.call("get_item", &json!({ "item": hp })).await;
    assert_ne!(read["isError"], true, "{read}");
    let before = comments().await;
    let wrote = tools
        .call(
            "comment_change_request",
            &json!({ "request": ecr_o, "body": format!("copied {m}") }),
        )
        .await;
    assert_eq!(wrote["isError"], true, "{wrote}");
    assert_eq!(
        comments().await,
        before,
        "nothing written after a restricted read"
    );
    let otto_actor = app.actor("otto").await;
    let tools = McpTools::new(&app.state, &otto_actor);
    tools.call("get_item", &json!({ "item": prt })).await;
    let wrote = tools
        .call(
            "comment_change_request",
            &json!({ "request": ecr_o, "body": "fine" }),
        )
        .await;
    assert_ne!(wrote["isError"], true, "{wrote}");
    assert_eq!(comments().await, before + 1);
    // a membership removed during a run applies to the next tool call
    let vera_actor = app.actor("vera").await;
    let tools = McpTools::new(&app.state, &vera_actor);
    let read = tools.call("get_item", &json!({ "item": hp })).await;
    assert_ne!(read["isError"], true, "{read}");
    sqlx::query(
        "DELETE FROM project_members
         WHERE project_id = $1 AND user_id = (SELECT id FROM users WHERE username = 'vera')",
    )
    .bind(group_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let read = tools.call("get_item", &json!({ "item": hp })).await;
    assert_eq!(read["isError"], true, "{read}");
    app.teardown().await;
}

/// The id of an item's revision A.
async fn rev_id_of(app: &common::TestApp, number: &str) -> Uuid {
    id_of(
        app,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = 'A'",
        number,
    )
    .await
}

async fn checkin(app: &common::TestApp, token: &str, number: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("{}/checkin", rev_of(number)),
            token,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{number}: {}", r.text());
}

/// The next generated part number of a project group (numbering preview).
async fn next_part(app: &common::TestApp, token: &str, project: &str) -> String {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/projects/{project}/numbering"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["next"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["item_type"] == "part")
        .unwrap()["number"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn set_visibility(app: &common::TestApp, token: &str, project: &str, visibility: &str) {
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{project}/visibility"),
            token,
            Some(json!({ "visibility": visibility })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn set_scheme(app: &common::TestApp, token: &str, project: &str, scheme: &str) {
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{project}/numbering"),
            token,
            Some(json!({ "scheme": scheme })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Third review round: item ids that exist answer like unknown ones,
/// deleting a public item named by a hidden request, locks of former
/// members, the reviewer and assignee pickers on changes with hidden items,
/// the assistant's outsider (identity, write results), private number
/// counters and notifications written before their subject was stored.
#[tokio::test]
async fn third_review_round_leaks_are_closed() {
    use teamcraft::assistant::{McpTools, Tools};

    let app = app_or_skip!();
    let m = format!("zt{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mu = m.to_ascii_uppercase();
    let (_, mona) = app.user_in("mona", Role::Editor, &["ROVER"]).await;
    let (_, otto) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let (_, rex) = app.user_in("rex", Role::Editor, &["ROVER"]).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;
    let code = format!("S{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &mona,
        json!({ "code": code, "name": "Secret group" }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/projects/{code}/members"),
        &mona,
        json!({ "username": "rex" }),
    )
    .await;
    set_visibility(&app, &mona, &code, "restricted").await;
    let group_id = app.project_id(&code).await;
    let mona_actor = app.actor("mona").await;
    let otto_actor = app.actor("otto").await;

    // --- /items/{id}: a hidden item answers like an unknown id ---------------
    let hs1 = new_item(&app, &mona, "part", &code).await;
    checkin(&app, &mona, &hs1).await;
    let hs1_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &hs1).await;
    let cookie = app.login("otto").await;
    let hidden = app
        .get_page(&format!("/items/{hs1_id}"), Some(&cookie))
        .await;
    let unknown = app
        .get_page(&format!("/items/{}", Uuid::new_v4()), Some(&cookie))
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.status, hidden.status);
    for page in [&hidden, &unknown] {
        let text = page.text();
        assert!(text.contains("Item not found."), "{text}");
        assert!(!text.contains("no revisions"), "{text}");
    }

    // --- a public item named by a hidden request can be deleted --------------
    let pub_o = new_item(&app, &otto, "part", "ROVER").await;
    let pub_o_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &pub_o).await;
    let ecr_h = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &mona,
            json!({ "title": format!("About {m}"),
                    "items": [{ "item": hs1 }, { "item": pub_o }] }),
        )
        .await,
        "change_request",
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr_h}"),
            &otto,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "the request is hidden");
    teamcraft::services::items::delete(&app.pool, &otto_actor, pub_o_id)
        .await
        .expect("no hidden request blocks deleting a public item");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr_h}"),
            &mona,
            None,
        )
        .await;
    assert_eq!(
        r.json()["items"].as_array().unwrap().len(),
        1,
        "{}",
        r.text()
    );

    // --- rex works in the group, then is removed -----------------------------
    let hs_lock = new_item(&app, &rex, "part", &code).await; // stays checked out
    let pub_r = new_item(&app, &rex, "part", "ROVER").await;
    let hs_r = new_item(&app, &rex, "part", &code).await;
    for n in [&pub_r, &hs_r] {
        checkin(&app, &rex, n).await;
    }
    let chg_r = number_of(
        &post_ok(&app, "/api/v1/changes", &rex, json!({ "title": "Rex" })).await,
        "change",
    );
    for n in [&pub_r, &hs_r] {
        post_ok(
            &app,
            &format!("/api/v1/changes/{chg_r}/items"),
            &rex,
            json!({ "item": n }),
        )
        .await;
    }
    let ecr_r = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &rex,
            json!({ "title": "Rex asks", "items": [{ "item": pub_r }, { "item": hs_r }] }),
        )
        .await,
        "change_request",
    );
    let suggested = format!("/api/v1/changes/{chg_r}/suggested-reviewers");
    let r = app.api(Method::GET, &suggested, &rex, None).await;
    assert!(
        r.json()["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["username"] == "mona"),
        "members get the group's manager suggested: {}",
        r.text()
    );
    // a member's lock is theirs (or an admin's) to release
    let cancel = format!("{}/cancel-checkout", rev_of(&hs_lock));
    let r = app.api(Method::POST, &cancel, &mona, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let rex_id = app.actor("rex").await.id;
    let held =
        teamcraft::services::projects::remove_member(&app.pool, &mona_actor, group_id, rex_id)
            .await
            .expect("remove rex");
    assert_eq!(held, 1);
    // a former member's lock: a manager releases it, nobody else
    let r = app.api(Method::POST, &cancel, &otto, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = app.api(Method::POST, &cancel, &mona, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let lock: Option<Uuid> =
        sqlx::query_scalar("SELECT checked_out_by FROM revisions WHERE id = $1")
            .bind(rev_id_of(&app, &hs_lock).await)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(lock, None);

    // --- reviewer and assignee pickers tell rex nothing about the group -----
    let mut answers = Vec::new();
    for user in ["otto", "mona"] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg_r}/reviewers"),
                &rex,
                Some(json!({ "username": user })),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}: {}", r.text());
        answers.push(r.text());
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/change-requests/{ecr_r}/assign"),
                &rex,
                Some(json!({ "username": user })),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}: {}", r.text());
        answers.push(r.text());
    }
    assert_eq!(answers[0], answers[2], "member and non-member alike");
    assert_eq!(answers[1], answers[3], "member and non-member alike");
    let r = app.api(Method::GET, &suggested, &rex, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["suggestions"], json!([]), "{}", r.text());

    // --- the assistant ------------------------------------------------------
    let comments = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM change_request_comments")
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let pub_q = new_item(&app, &otto, "part", "ROVER").await;
    let ecr_pub = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &otto,
            json!({ "title": "Public", "items": [{ "item": pub_q }] }),
        )
        .await,
        "change_request",
    );
    // a request mona sees as its requester (it names only a hidden item)
    let ecr_m = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &mona,
            json!({ "title": format!("Hidden ask {m}"), "items": [{ "item": hs1 }] }),
        )
        .await,
        "change_request",
    );
    let comment = json!({ "request": ecr_pub, "body": "copied" });
    let before = comments().await;
    let tools = McpTools::new(&app.state, &mona_actor);
    let read = tools
        .call("list_change_requests", &json!({ "scope": "mine" }))
        .await;
    assert!(read.to_string().contains(&ecr_m), "{read}");
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_eq!(wrote["isError"], true, "identity reads taint: {wrote}");
    // a draft the assistant fills with a restricted item taints the run
    let hw = new_item(&app, &mona, "part", &code).await;
    checkin(&app, &mona, &hw).await;
    let tools = McpTools::new(&app.state, &mona_actor);
    let draft = tools
        .call(
            "create_change_draft",
            &json!({ "title": "Draft", "items": [hw] }),
        )
        .await;
    assert_ne!(draft["isError"], true, "{draft}");
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_eq!(wrote["isError"], true, "write results taint: {wrote}");
    assert_eq!(comments().await, before, "nothing written");
    // a public draft does not
    let pw = new_item(&app, &mona, "part", "ROVER").await;
    checkin(&app, &mona, &pw).await;
    let tools = McpTools::new(&app.state, &mona_actor);
    let draft = tools
        .call(
            "create_change_draft",
            &json!({ "title": "Public draft", "items": [pw] }),
        )
        .await;
    assert_ne!(draft["isError"], true, "{draft}");
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_ne!(wrote["isError"], true, "{wrote}");
    assert_eq!(comments().await, before + 1);

    // --- numbering: a restricted group counts privately ---------------------
    let ncode = format!("N{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &mona,
        json!({ "code": ncode, "name": "Numbered" }),
    )
    .await;
    set_scheme(&app, &mona, &ncode, "{PROJECT}-{TYPE}-{SEQ:3}").await;
    // otto's group takes the same expansion while the group is internal
    let ocode = format!("O{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &otto,
        json!({ "code": ocode, "name": "Squat" }),
    )
    .await;
    set_scheme(&app, &otto, &ocode, &format!("{ncode}-{{TYPE}}-{{SEQ:3}}")).await;
    let num = |n: u32| format!("{ncode}-PRT-{n:03}");
    for _ in 0..2 {
        new_item(&app, &mona, "part", &ncode).await;
    }
    assert_eq!(next_part(&app, &otto, &ocode).await, num(3));
    set_visibility(&app, &mona, &ncode, "restricted").await;
    assert_eq!(new_item(&app, &mona, "part", &ncode).await, num(3));
    assert_eq!(
        next_part(&app, &otto, &ocode).await,
        num(3),
        "numbers counted while restricted are not shown to others"
    );
    assert_eq!(next_part(&app, &mona, &ncode).await, num(4));
    // otto's scheme runs into the hidden number: refused, not skipped (a
    // skip would count the hidden items and use up the group's numbers)
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &otto,
            Some(json!({ "item_type": "part", "name": "Squat", "project": ocode })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains(&num(3)), "{}", r.text());
    assert!(!r.text().contains(&num(4)), "{}", r.text());
    assert_eq!(next_part(&app, &otto, &ocode).await, num(3), "counter kept");
    assert_eq!(next_part(&app, &mona, &ncode).await, num(4));
    assert_eq!(new_item(&app, &mona, "part", &ncode).await, num(4));
    // opened again: the counters merge; restricted again: private again
    set_visibility(&app, &mona, &ncode, "internal").await;
    assert_eq!(next_part(&app, &otto, &ocode).await, num(5));
    set_visibility(&app, &mona, &ncode, "restricted").await;
    assert_eq!(new_item(&app, &mona, "part", &ncode).await, num(5));
    assert_eq!(next_part(&app, &otto, &ocode).await, num(5));
    assert_eq!(next_part(&app, &mona, &ncode).await, num(6));
    // a number typed in in an internal group is still skipped
    set_scheme(&app, &otto, &ocode, "{PROJECT}-{TYPE}-{SEQ:3}").await;
    post_ok(
        &app,
        "/api/v1/items",
        &otto,
        json!({ "item_type": "part", "number": format!("{ocode}-PRT-001"),
                "name": "Typed", "project": "ROVER" }),
    )
    .await;
    assert_eq!(
        new_item(&app, &otto, "part", &ocode).await,
        format!("{ocode}-PRT-002")
    );

    // --- notifications written before their subject was stored --------------
    let hs1_rev = rev_id_of(&app, &hs1).await;
    let ecr_m_id = id_of(
        &app,
        "SELECT id FROM change_requests WHERE number = $1",
        &ecr_m,
    )
    .await;
    let vera_id = app.actor("vera").await.id;
    for (link, subject_type, subject_ref) in [
        (
            format!("/revisions/{hs1_rev}"),
            "revision",
            format!("{hs1}/A"),
        ),
        (
            format!("/requests/{ecr_m_id}"),
            "change_request",
            ecr_m.clone(),
        ),
    ] {
        sqlx::query(
            "INSERT INTO notifications (id, user_id, kind, title, link, subject_type, subject_ref)
             VALUES ($1, $2, 'item_update', $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(vera_id)
        .bind(format!("Old {m} {subject_ref}"))
        .bind(&link)
        .bind(subject_type)
        .bind(&subject_ref)
        .execute(&app.pool)
        .await
        .unwrap();
    }
    let r = app
        .api(Method::GET, "/api/v1/notifications", &vera, None)
        .await;
    assert!(
        r.text().contains(&m),
        "rows without a subject: {}",
        r.text()
    );
    sqlx::raw_sql(include_str!("../migrations/0027_notification_subjects.sql"))
        .execute(&app.pool)
        .await
        .expect("migration 0027");
    let r = app
        .api(Method::GET, "/api/v1/notifications", &vera, None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert!(!r.text().contains(&m), "{}", r.text());
    assert_eq!(r.json()["unread"], 0, "{}", r.text());
    app.teardown().await;
}

/// Records the e-mails instead of sending them.
#[derive(Default)]
struct Mails(std::sync::Mutex<Vec<String>>);

impl teamcraft::delivery::MailSender for Mails {
    fn send(
        &self,
        message: lettre::Message,
    ) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        let text = String::from_utf8_lossy(&message.formatted()).to_string();
        self.0.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

/// Fourth review round: the order of readiness placeholders, numbering
/// schemes that run into a hidden group's numbers (see the third round),
/// deleted items leaving requests, the assistant's outsider for users in no
/// restricted group and the page context, old notifications naming a second
/// object, the out-of-date flag of hidden simulation bases, memberships
/// from the identity provider, and e-mails to deactivated users.
#[tokio::test]
async fn fourth_review_round_leaks_are_closed() {
    use teamcraft::assistant::{Action, McpTools, Question, Tools};
    use teamcraft::services::users::{upsert_oidc_user, OidcIdentity};

    let app = app_or_skip!();
    let m = format!("zf{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mu = m.to_ascii_uppercase();
    let (_, mona) = app.user_in("mona", Role::Editor, &["ROVER"]).await;
    let (_, otto) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let (_, bob) = app.user_in("bob", Role::Editor, &["ROVER"]).await;
    let (admin_actor, _) = app.user("root", Role::Admin).await;
    let code = format!("S{mu}");
    post_ok(
        &app,
        "/api/v1/projects",
        &mona,
        json!({ "code": code, "name": "Secret group" }),
    )
    .await;
    post_ok(
        &app,
        &format!("/api/v1/projects/{code}/members"),
        &mona,
        json!({ "username": "bob" }),
    )
    .await;
    set_visibility(&app, &mona, &code, "restricted").await;
    let group_id = app.project_id(&code).await;
    let mona_actor = app.actor("mona").await;
    let otto_actor = app.actor("otto").await;

    // --- readiness: placeholders last, whatever the hidden number -----------
    // (the shared default scheme: the hidden part's number lies between)
    let p10 = new_item(&app, &mona, "part", "ROVER").await;
    let hs = new_item(&app, &mona, "part", &code).await;
    let p99 = new_item(&app, &mona, "part", "ROVER").await;
    assert!(p10 < hs && hs < p99, "{p10} {hs} {p99}");
    let asm = new_item(&app, &mona, "assembly", "ROVER").await;
    for child in [&p10, &hs, &p99] {
        post_ok(
            &app,
            &format!("{}/bom", rev_of(&asm)),
            &mona,
            json!({ "child": child, "quantity": 1.0 }),
        )
        .await;
    }
    checkin(&app, &mona, &asm).await;
    let chg = number_of(
        &post_ok(
            &app,
            "/api/v1/changes",
            &otto,
            json!({ "title": "Release" }),
        )
        .await,
        "change",
    );
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg}/items"),
        &otto,
        json!({ "item": asm }),
    )
    .await;
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}/readiness"),
            &otto,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let order: Vec<String> = r.json()["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["item_number"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        order,
        vec![p10.clone(), p99.clone(), "restricted item".to_string()],
        "{}",
        r.text()
    );
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg}/reviewers"),
        &otto,
        json!({ "username": "mona" }),
    )
    .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &otto,
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let text = r.text();
    let (a, b) = (
        text.find(&p99).expect("p99 named"),
        text.find("a restricted dependency").expect("placeholder"),
    );
    assert!(a < b, "placeholder last in the summary: {text}");

    // --- a deleted public item leaves its requests with a trace -------------
    let pub_d = new_item(&app, &otto, "part", "ROVER").await;
    let pub_d_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &pub_d).await;
    let ecr = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &mona,
            json!({ "title": "Variant",
                    "items": [{ "item": hs }, { "item": pub_d, "note": format!("replace {m}") }] }),
        )
        .await,
        "change_request",
    );
    let ecr_id = id_of(
        &app,
        "SELECT id FROM change_requests WHERE number = $1",
        &ecr,
    )
    .await;
    teamcraft::services::items::delete(&app.pool, &otto_actor, pub_d_id)
        .await
        .expect("delete the public item");
    let history = teamcraft::services::audit::for_entities(&app.pool, &mona_actor, &[ecr_id], 50)
        .await
        .unwrap();
    let entry = history
        .iter()
        .find(|e| e.action == "change_request.remove_item")
        .expect("the request records the removal");
    assert!(entry.summary.contains(&pub_d), "{}", entry.summary);
    assert_eq!(entry.details["note"], format!("replace {m}"));
    let told: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND title LIKE '%' || $2 || '%'",
    )
    .bind(mona_actor.id)
    .bind(format!("deleted {pub_d}"))
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(told, 1, "the requester is told");
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert!(
        !r.text().contains(&ecr),
        "the deleter learns nothing: {}",
        r.text()
    );

    // --- the assistant: identity reads of a user in no restricted group -----
    let hb = new_item(&app, &bob, "part", &code).await;
    checkin(&app, &bob, &hb).await;
    let chg_b = number_of(
        &post_ok(
            &app,
            "/api/v1/changes",
            &bob,
            json!({ "title": format!("Bob {m}") }),
        )
        .await,
        "change",
    );
    post_ok(
        &app,
        &format!("/api/v1/changes/{chg_b}/items"),
        &bob,
        json!({ "item": hb }),
    )
    .await;
    let bob_id = app.actor("bob").await.id;
    teamcraft::services::projects::remove_member(&app.pool, &mona_actor, group_id, bob_id)
        .await
        .expect("remove bob");
    let bob_actor = app.actor("bob").await;
    assert!(bob_actor.hidden_projects.contains(&group_id));
    let pub_q = new_item(&app, &otto, "part", "ROVER").await;
    let ecr_pub = number_of(
        &post_ok(
            &app,
            "/api/v1/change-requests",
            &otto,
            json!({ "title": "Public", "items": [{ "item": pub_q }] }),
        )
        .await,
        "change_request",
    );
    let comment = json!({ "request": ecr_pub, "body": "copied" });
    let comments = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM change_request_comments")
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let before = comments().await;
    let tools = McpTools::new(&app.state, &bob_actor);
    let read = tools.call("get_change", &json!({ "change": chg_b })).await;
    assert_ne!(read["isError"], true, "the author reads it: {read}");
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_eq!(wrote["isError"], true, "identity reads taint: {wrote}");
    assert_eq!(comments().await, before);

    // --- the assistant: the page the question was asked on ------------------
    let question = |item: &str| Question {
        action: Action::Ask,
        text: "Answer the open question".into(),
        item: Some(item.to_string()),
        revision: Some("A".into()),
        change: None,
        allow_drafts: true,
    };
    let tools = McpTools::new(&app.state, &mona_actor);
    tools.note_context(&question(&hs)).await;
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_eq!(wrote["isError"], true, "a restricted page taints: {wrote}");
    assert_eq!(comments().await, before);
    let tools = McpTools::new(&app.state, &mona_actor);
    tools.note_context(&question(&p10)).await;
    let wrote = tools.call("comment_change_request", &comment).await;
    assert_ne!(wrote["isError"], true, "a public page does not: {wrote}");
    assert_eq!(comments().await, before + 1);

    // --- notifications written before 0025 that name a second object --------
    let asm_id = id_of(&app, "SELECT id FROM items WHERE number = $1", &asm).await;
    sqlx::query(
        "INSERT INTO notifications (id, user_id, kind, title, body, link, subject_type,
                                    subject_ref, subject_kind, subject_id)
         VALUES ($1, $2, 'impact', $3, $4, '/', 'revision', $5, 'item', $6)",
    )
    .bind(Uuid::new_v4())
    .bind(otto_actor.id)
    .bind(format!("Check needed: {asm} – {hs}/B was released."))
    .bind(format!("old {m} (find no. 30)"))
    .bind(format!("{asm}/A"))
    .bind(asm_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert!(r.text().contains(&m), "stored as it was: {}", r.text());
    sqlx::raw_sql(include_str!("../migrations/0028_notification_mentions.sql"))
        .execute(&app.pool)
        .await
        .expect("migration 0028");
    let r = app
        .api(Method::GET, "/api/v1/notifications", &otto, None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert!(!r.text().contains(&m), "{}", r.text());

    // --- the out-of-date flag of a hidden simulation basis ------------------
    let r = app
        .api_upload(
            &format!("{}/files", rev_of(&hs)),
            &mona,
            "secret.step",
            b"ISO-10303-21; v1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sim = new_item(&app, &mona, "simulation", "ROVER").await;
    let r = app
        .api(
            Method::POST,
            &format!("{}/simulation/bases", rev_of(&sim)),
            &mona,
            Some(json!({ "item": hs, "revision": "A", "file": "secret.step" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_upload(
            &format!("{}/files", rev_of(&hs)),
            &mona,
            "secret.step",
            b"ISO-10303-21; v2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let bases_uri = format!("{}/simulation/bases", rev_of(&sim));
    let r = app.api(Method::GET, &bases_uri, &mona, None).await;
    assert!(r.json()[0]["out_of_date"].is_string(), "{}", r.text());
    let r = app.api(Method::GET, &bases_uri, &otto, None).await;
    let placeholder = r.json()[0].clone();
    assert_eq!(placeholder["restricted"], true, "{placeholder}");
    assert_eq!(placeholder["out_of_date"], Value::Null, "{placeholder}");
    let r = app
        .api(
            Method::GET,
            &format!("{}/simulation", rev_of(&sim)),
            &otto,
            None,
        )
        .await;
    assert_eq!(r.json()["out_of_date"], false, "{}", r.text());

    // --- memberships from the identity provider stay synchronised -----------
    let rocket = app.project_id("ROCKET").await;
    let identity = |groups: &[&str]| OidcIdentity {
        subject: format!("idp-{m}"),
        username_hint: "sam".into(),
        display_name: "Sam".into(),
        email: None,
        role: Role::Editor,
        groups: groups.iter().map(|g| g.to_string()).collect(),
    };
    upsert_oidc_user(&app.pool, identity(&["teamcraft-rocket"]))
        .await
        .expect("provision");
    let sam_id = app.actor("sam").await.id;
    let membership = || async {
        sqlx::query_as::<_, (String, bool)>(
            "SELECT source, manager FROM project_members WHERE project_id = $1 AND user_id = $2",
        )
        .bind(rocket)
        .bind(sam_id)
        .fetch_optional(&app.pool)
        .await
        .unwrap()
    };
    assert_eq!(membership().await, Some(("oidc".to_string(), false)));
    teamcraft::services::projects::add_member(&app.pool, &admin_actor, rocket, "sam", true)
        .await
        .expect("make sam a manager");
    assert_eq!(
        membership().await,
        Some(("oidc".to_string(), true)),
        "promoting keeps the membership synchronised"
    );
    let removed =
        teamcraft::services::projects::remove_member(&app.pool, &admin_actor, rocket, sam_id).await;
    assert!(
        matches!(removed, Err(teamcraft::error::AppError::Conflict(_))),
        "an IdP membership would come back: {removed:?}"
    );
    upsert_oidc_user(&app.pool, identity(&[]))
        .await
        .expect("login again");
    assert_eq!(membership().await, None, "the IdP dropped sam");

    // --- no e-mail to a user deactivated after the notification was queued --
    sqlx::query("UPDATE users SET email = 'mona@example.test' WHERE id = $1")
        .bind(mona_actor.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let queued = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO notifications (id, user_id, kind, title, email_state)
         VALUES ($1, $2, 'item_update', $3, 'pending')",
    )
    .bind(queued)
    .bind(mona_actor.id)
    .bind(format!("Mail {m}"))
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE users SET active = false WHERE id = $1")
        .bind(mona_actor.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let mails = std::sync::Arc::new(Mails::default());
    let delivery = teamcraft::delivery::Delivery::with_senders(
        "https://plm.example.test",
        "TeamCraft",
        Some((mails.clone(), "TeamCraft <teamcraft@example.test>")),
        None,
    )
    .unwrap();
    delivery.run_once(&app.pool).await.unwrap();
    assert!(
        mails.0.lock().unwrap().iter().all(|t| !t.contains(&m)),
        "nothing sent to the deactivated user"
    );
    let state: String = sqlx::query_scalar("SELECT email_state FROM notifications WHERE id = $1")
        .bind(queued)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(state, "skipped");
    app.teardown().await;
}
