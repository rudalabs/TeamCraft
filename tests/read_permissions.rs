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
            ("{revision_id}", _) => h.revision_id.to_string(),
            ("{item_id}", _) => h.item_id.to_string(),
            ("{user_id}", _) => h.caller.to_string(),
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

fn assert_clean(who: &str, what: &str, r: &common::Response, markers: &[String], sent: &[String]) {
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

const COUNTED: [&str; 20] = [
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
        json!({ "child": hid, "quantity": 3.0, "find_number": 30, "notes": "the hidden line" }),
    )
    .await;
    post_ok(
        &app,
        &format!("{asm_rev}/references"),
        &out_editor,
        json!({ "item": hid, "kind": "drawing" }),
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
        caller: Uuid::nil(),
    };
    let markers: Vec<String> = vec![
        m.clone(),
        hidden_base.group_id.to_string(),
        hidden_base.item_id.to_string(),
        hidden_base.revision_id.to_string(),
        file_id.to_string(),
        hidden_base.version_id.to_string(),
        hidden_base.change_id.to_string(),
        hidden_base.request_id.to_string(),
        attachment_id.to_string(),
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
        assert_eq!(line["notes"], "the hidden line");
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
            let r = call(&app, method, &uri, token, &cookie).await;
            let what = format!("{method} {path}");
            assert_clean(who, &what, &r, &markers, &sent_of(&uri));
            if method == "GET" {
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
    // routes without a path parameter: lists, searches, forms, creation
    for (who, token) in [
        ("outsider_editor", &out_editor),
        ("outsider_viewer", &out_viewer),
    ] {
        let cookie = app.login(who).await;
        for (method, path) in &routes {
            if skipped(method, path) || path.contains('{') {
                continue;
            }
            let r = call(&app, method, path, token, &cookie).await;
            assert_clean(who, &format!("{method} {path}"), &r, &markers, &[]);
        }
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
            assert_clean(who, &uri, &r, &markers, &sent_of(&uri));
        }
    }

    // --- MCP: every tool and prompt --------------------------------------------
    let args = json!({
        "item": hid, "revision": "A", "with": "A", "change": chg, "request": ecr,
        "number": ecr, "query": "hidden", "items": [hid], "title": "probe", "body": "probe",
        "comment": "probe", "levels": 3,
    });
    let sent = vec![hid.clone(), chg.clone(), ecr.clone()];
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
        let r = app
            .api(
                Method::POST,
                "/api/v1/mcp",
                token,
                Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": { "name": "search_items", "arguments": { "query": m } } })),
            )
            .await;
        assert!(!r.text().contains(&hid), "{who}: MCP search finds nothing");
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
            assert_clean(who, "MCP prompt", &r, &markers, &sent);
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
        caller: h.caller,
    }
}

/// One call: the REST API with the token, the web UI with the session.
async fn call(
    app: &common::TestApp,
    method: &str,
    uri: &str,
    token: &str,
    cookie: &str,
) -> common::Response {
    let method = Method::from_bytes(method.as_bytes()).unwrap();
    if uri.starts_with("/api/") {
        let body = (method != Method::GET).then(|| json!({}));
        return app.api(method, uri, token, body).await;
    }
    if method == Method::GET {
        app.get_page(uri, Some(cookie)).await
    } else {
        app.post_form(uri, Some(cookie), Some("http://localhost:8080"), &[])
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
    let moved =
        teamcraft::services::items::update_master(&app.pool, &item, item_id, Some(rover), "each")
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
