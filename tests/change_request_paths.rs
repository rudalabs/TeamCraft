//! Characterisation of change requests (ECR) through the REST API, failure
//! paths first: invalid input, refused permissions, wrong states (409),
//! unknown references, attachments that are not allowed – and the happy
//! path in between. Every step's status code, error code and message and a
//! projection of the reply (ids and times left out) are compared with the
//! snapshot in `tests/fixtures/change_requests/paths.json`.
//!
//! The snapshot pins today's behaviour, e.g. while `services::requests` is
//! split into modules. After an intended change, rewrite it with
//! `TEAMCRAFT_UPDATE_SNAPSHOTS=1 cargo test --test change_request_paths` and
//! review the diff.

mod common;

use std::path::PathBuf;

use axum::http::Method;
use regex::Regex;
use serde_json::{json, Map, Value};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/change_requests/paths.json")
}

/// Keys whose values differ from run to run (or list audit rows written in
/// the same transaction, whose order is not fixed).
const VOLATILE: &[&str] = &["history", "sha256", "storage_key"];

/// The reply without ids and times: UUIDs and timestamps become
/// placeholders, volatile keys are left out.
fn stable(v: &Value) -> Value {
    let uuid = Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
    let time = Regex::new(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}").unwrap();
    fn walk(v: &Value, uuid: &Regex, time: &Regex) -> Value {
        match v {
            Value::String(s) if time.is_match(s) => json!("<time>"),
            Value::String(s) => json!(uuid.replace_all(s, "<id>")),
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, uuid, time)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .filter(|(k, _)| !VOLATILE.contains(&k.as_str()))
                    .map(|(k, x)| (k.clone(), walk(x, uuid, time)))
                    .collect::<Map<_, _>>(),
            ),
            other => other.clone(),
        }
    }
    walk(v, &uuid, &time)
}

struct Script<'a> {
    app: &'a common::TestApp,
    steps: Vec<Value>,
}

impl Script<'_> {
    fn record(&mut self, step: &str, r: &common::Response) -> Value {
        let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
        self.steps.push(json!({
            "step": step,
            "status": r.status.as_u16(),
            "reply": stable(&body),
        }));
        body
    }

    async fn call(
        &mut self,
        step: &str,
        token: &str,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> Value {
        let r = self.app.api(method, uri, token, body).await;
        self.record(step, &r)
    }

    async fn post(&mut self, step: &str, token: &str, uri: &str, body: Value) -> Value {
        self.call(step, token, Method::POST, uri, Some(body)).await
    }

    async fn upload(&mut self, step: &str, token: &str, ecr: &str, name: &str, bytes: &[u8]) {
        let uri = format!("/api/v1/change-requests/{ecr}/attachments");
        let r = self.app.api_upload(&uri, token, name, bytes).await;
        self.record(step, &r);
    }
}

/// `/api/v1/change-requests/<number>` of a created request.
fn at(reply: &Value) -> String {
    let number = reply["change_request"]["number"]
        .as_str()
        .unwrap_or_else(|| panic!("no request number in {reply}"));
    format!("/api/v1/change-requests/{number}")
}

#[tokio::test]
async fn change_request_paths_match_the_snapshot() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, eng2) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let (_, other) = app.user("olga", Role::Viewer).await;
    let (_, admin) = app.user("adam", Role::Admin).await;
    let part = app
        .create_item(
            &eng,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" }),
        )
        .await;
    let rover_part = app
        .create_item(
            &outsider,
            json!({ "item_type": "part", "name": "Wheel", "project": "ROVER" }),
        )
        .await;
    let mut s = Script {
        app: &app,
        steps: Vec::new(),
    };
    let list = "/api/v1/change-requests";
    let none = json!({});

    // --- raising: invalid input, nothing is created ------------------------
    s.post(
        "create: empty title",
        &viewer,
        list,
        json!({ "title": "  " }),
    )
    .await;
    s.post(
        "create: title too long",
        &viewer,
        list,
        json!({ "title": "x".repeat(201) }),
    )
    .await;
    s.post(
        "create: description too long",
        &viewer,
        list,
        json!({ "title": "Long", "description": "x".repeat(20_001) }),
    )
    .await;
    s.post(
        "create: unknown reason",
        &viewer,
        list,
        json!({ "title": "T", "reason": "nonsense" }),
    )
    .await;
    s.post(
        "create: unknown priority",
        &viewer,
        list,
        json!({ "title": "T", "priority": "asap" }),
    )
    .await;
    s.post(
        "create: bad due date",
        &viewer,
        list,
        json!({ "title": "T", "due_date": "31.12.2026" }),
    )
    .await;
    s.post(
        "create: unknown item",
        &viewer,
        list,
        json!({ "title": "T", "items": [{ "item": "PRT-999999" }] }),
    )
    .await;
    s.post(
        "create: unknown revision",
        &viewer,
        list,
        json!({ "title": "T", "items": [{ "item": part, "revision": "Z" }] }),
    )
    .await;
    s.post(
        "create: body is a string",
        &viewer,
        list,
        json!("just a string"),
    )
    .await;
    s.call(
        "list: unknown status",
        &viewer,
        Method::GET,
        &format!("{list}?status=bogus"),
        None,
    )
    .await;
    s.call(
        "list: unknown scope",
        &viewer,
        Method::GET,
        &format!("{list}?scope=bogus"),
        None,
    )
    .await;
    s.call("list: nothing yet", &viewer, Method::GET, list, None)
        .await;

    // --- W: the same item 101 times (listed once), withdrawn ---------------
    let many: Vec<Value> = (0..101).map(|_| json!({ "item": part })).collect();
    let w = at(&s
        .post(
            "create W: same item 101 times",
            &viewer,
            list,
            json!({ "title": "Duplicate items", "items": many }),
        )
        .await);
    s.post(
        "withdraw W: not the requester",
        &other,
        &format!("{w}/withdraw"),
        none.clone(),
    )
    .await;
    s.post(
        "withdraw W: requester",
        &viewer,
        &format!("{w}/withdraw"),
        none.clone(),
    )
    .await;
    s.post(
        "withdraw W: again",
        &viewer,
        &format!("{w}/withdraw"),
        none.clone(),
    )
    .await;
    s.post(
        "triage W: withdrawn",
        &eng,
        &format!("{w}/triage"),
        none.clone(),
    )
    .await;
    s.post(
        "decide W: withdrawn",
        &eng,
        &format!("{w}/decide"),
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "comment W: withdrawn",
        &eng,
        &format!("{w}/comments"),
        json!({ "body": "Ok" }),
    )
    .await;
    s.call(
        "update W: withdrawn",
        &viewer,
        Method::PATCH,
        &w,
        Some(json!({ "title": "Again" })),
    )
    .await;

    // --- A: raised by a viewer on a ROCKET part, deferred, rejected ---------
    let a = at(&s
        .post(
            "create A: viewer, item twice",
            &viewer,
            list,
            json!({ "title": "Weld seam cracks", "description": "Seen on 3 of 10 parts.",
                "reason": "manufacturing", "priority": "high", "due_date": "2026-12-01",
                "items": [{ "item": part, "revision": "A", "note": "flange" }, { "item": part }] }),
        )
        .await);
    s.call(
        "get: unknown number",
        &viewer,
        Method::GET,
        &format!("{list}/ECR-999999"),
        None,
    )
    .await;
    s.call("get A: other viewer", &other, Method::GET, &a, None)
        .await;
    s.call(
        "get A: editor of another group",
        &outsider,
        Method::GET,
        &a,
        None,
    )
    .await;
    s.call(
        "update A: not the requester",
        &other,
        Method::PATCH,
        &a,
        Some(json!({ "title": "Hijacked" })),
    )
    .await;
    s.call(
        "update A: empty title",
        &viewer,
        Method::PATCH,
        &a,
        Some(json!({ "title": "" })),
    )
    .await;
    s.call(
        "update A: bad due date",
        &viewer,
        Method::PATCH,
        &a,
        Some(json!({ "due_date": "tomorrow" })),
    )
    .await;
    s.call(
        "update A: requester",
        &viewer,
        Method::PATCH,
        &a,
        Some(json!({ "title": "Weld seam cracks at the flange", "due_date": null })),
    )
    .await;
    let comments = format!("{a}/comments");
    s.post(
        "comment A: empty",
        &other,
        &comments,
        json!({ "body": " " }),
    )
    .await;
    s.post(
        "comment A: too long",
        &other,
        &comments,
        json!({ "body": "x".repeat(10_001) }),
    )
    .await;
    s.post(
        "comment A: other viewer",
        &other,
        &comments,
        json!({ "body": "Same here." }),
    )
    .await;
    let items = format!("{a}/items");
    s.post(
        "add item A: listed already",
        &viewer,
        &items,
        json!({ "item": part }),
    )
    .await;
    s.post(
        "add item A: unknown",
        &viewer,
        &items,
        json!({ "item": "PRT-999999" }),
    )
    .await;
    s.post(
        "add item A: not the requester",
        &other,
        &items,
        json!({ "item": part }),
    )
    .await;
    let triage = format!("{a}/triage");
    s.post("triage A: viewer", &viewer, &triage, none.clone())
        .await;
    s.post(
        "triage A: editor of another group",
        &outsider,
        &triage,
        none.clone(),
    )
    .await;
    s.post("triage A: editor of the group", &eng, &triage, none.clone())
        .await;
    s.post("triage A: again", &eng, &triage, none.clone()).await;
    let assign = format!("{a}/assign");
    s.post(
        "assign A: unknown user",
        &eng,
        &assign,
        json!({ "username": "nobody" }),
    )
    .await;
    s.post(
        "assign A: a viewer",
        &eng,
        &assign,
        json!({ "username": "vera" }),
    )
    .await;
    s.post(
        "assign A: editor of another group",
        &eng,
        &assign,
        json!({ "username": "otto" }),
    )
    .await;
    s.post(
        "assign A: by a viewer",
        &viewer,
        &assign,
        json!({ "username": "rolf" }),
    )
    .await;
    s.post(
        "assign A: editor of the group",
        &eng,
        &assign,
        json!({ "username": "rolf" }),
    )
    .await;
    s.post(
        "assign A: nobody",
        &eng2,
        &assign,
        json!({ "username": null }),
    )
    .await;
    let decide = format!("{a}/decide");
    s.post(
        "decide A: unknown decision",
        &eng,
        &decide,
        json!({ "decision": "maybe" }),
    )
    .await;
    s.post(
        "decide A: viewer",
        &viewer,
        &decide,
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "decide A: editor of another group",
        &outsider,
        &decide,
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "decide A: defer without comment",
        &eng,
        &decide,
        json!({ "decision": "defer" }),
    )
    .await;
    s.post(
        "decide A: defer",
        &eng,
        &decide,
        json!({ "decision": "defer", "comment": "After the test campaign." }),
    )
    .await;
    s.post(
        "decide A: deferred again",
        &eng,
        &decide,
        json!({ "decision": "defer", "comment": "Still." }),
    )
    .await;
    let reopen = format!("{a}/reopen");
    s.post("reopen A: viewer", &viewer, &reopen, none.clone())
        .await;
    s.post("reopen A: editor", &eng, &reopen, none.clone())
        .await;
    s.post("reopen A: not deferred", &eng, &reopen, none.clone())
        .await;
    s.post(
        "decide A: reject without comment",
        &eng,
        &decide,
        json!({ "decision": "reject" }),
    )
    .await;
    s.post(
        "decide A: reject",
        &eng,
        &decide,
        json!({ "decision": "reject", "comment": "Seam is within tolerance." }),
    )
    .await;
    s.post(
        "decide A: decided already",
        &eng,
        &decide,
        json!({ "decision": "accept" }),
    )
    .await;
    s.post("reopen A: rejected", &eng, &reopen, none.clone())
        .await;
    s.post(
        "withdraw A: decided",
        &viewer,
        &format!("{a}/withdraw"),
        none.clone(),
    )
    .await;
    s.post(
        "comment A: after the decision",
        &other,
        &comments,
        json!({ "body": "Ok." }),
    )
    .await;
    s.post(
        "add item A: after rejection",
        &eng,
        &items,
        json!({ "item": rover_part }),
    )
    .await;
    s.call(
        "update A: after rejection, requester",
        &viewer,
        Method::PATCH,
        &a,
        Some(json!({ "title": "Again" })),
    )
    .await;
    s.post(
        "create change A: rejected",
        &eng,
        &format!("{a}/create-change"),
        none.clone(),
    )
    .await;

    // --- B: accepted from new, implemented by change orders ----------------
    let draft = s
        .post(
            "change order: a draft to link",
            &eng,
            "/api/v1/changes",
            json!({ "title": "Thread rework" }),
        )
        .await;
    let draft = draft["number"]
        .as_str()
        .expect("draft change number")
        .to_string();
    let b = at(&s
        .post(
            "create B: viewer",
            &viewer,
            list,
            json!({ "title": "Wrong thread", "reason": "defect", "items": [{ "item": part }] }),
        )
        .await);
    s.post(
        "create change B: not accepted",
        &eng,
        &format!("{b}/create-change"),
        none.clone(),
    )
    .await;
    s.post(
        "link change B: not accepted",
        &eng,
        &format!("{b}/link-change"),
        json!({ "change": draft }),
    )
    .await;
    s.post(
        "decide B: accept from new",
        &eng,
        &format!("{b}/decide"),
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "link change B: unknown change",
        &eng,
        &format!("{b}/link-change"),
        json!({ "change": "ECO-999999" }),
    )
    .await;
    s.post(
        "link change B: viewer",
        &viewer,
        &format!("{b}/link-change"),
        json!({ "change": draft }),
    )
    .await;
    s.post(
        "link change B: draft",
        &eng,
        &format!("{b}/link-change"),
        json!({ "change": draft }),
    )
    .await;
    s.post(
        "link change B: linked already",
        &eng,
        &format!("{b}/link-change"),
        json!({ "change": draft }),
    )
    .await;
    s.post(
        "create change B: viewer",
        &viewer,
        &format!("{b}/create-change"),
        none.clone(),
    )
    .await;
    s.post(
        "create change B: editor of another group",
        &outsider,
        &format!("{b}/create-change"),
        none.clone(),
    )
    .await;
    s.post(
        "create change B: bad priority",
        &eng,
        &format!("{b}/create-change"),
        json!({ "priority": "asap" }),
    )
    .await;
    s.post(
        "create change B: editor",
        &eng,
        &format!("{b}/create-change"),
        json!({ "title": "Fix the thread", "priority": "high" }),
    )
    .await;
    s.call("get B: requester", &viewer, Method::GET, &b, None)
        .await;
    s.call("get B: admin", &admin, Method::GET, &b, None).await;

    // attachments
    let number = b.rsplit('/').next().unwrap().to_string();
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    s.upload("attach B: empty", &viewer, &number, "empty.png", b"")
        .await;
    s.upload(
        "attach B: svg",
        &viewer,
        &number,
        "evil.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
    )
    .await;
    s.upload(
        "attach B: text",
        &viewer,
        &number,
        "notes.txt",
        b"plain text",
    )
    .await;
    s.upload(
        "attach B: png with a path in the name",
        &viewer,
        &number,
        "../crack .png",
        png,
    )
    .await;
    s.upload(
        "attach B: pdf by another viewer",
        &other,
        &number,
        "report.pdf",
        b"%PDF-1.7\n%\xe2\xe3\n",
    )
    .await;
    s.upload(
        "attach: unknown request",
        &viewer,
        "ECR-999999",
        "a.png",
        png,
    )
    .await;

    // --- C: no items first, then an item of another group -------------------
    let c = at(&s
        .post(
            "create C: viewer, no items",
            &viewer,
            list,
            json!({ "title": "Lighter wheel" }),
        )
        .await);
    s.post(
        "add item C: other group's item, requester",
        &viewer,
        &format!("{c}/items"),
        json!({ "item": rover_part }),
    )
    .await;
    s.post(
        "decide C: editor of another group",
        &eng,
        &format!("{c}/decide"),
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "decide C: editor of the group",
        &outsider,
        &format!("{c}/decide"),
        json!({ "decision": "accept" }),
    )
    .await;
    s.post(
        "add item C: foreign item after decision",
        &outsider,
        &format!("{c}/items"),
        json!({ "item": part }),
    )
    .await;
    s.post(
        "add item C: member of the item's group",
        &eng,
        &format!("{c}/items"),
        json!({ "item": part }),
    )
    .await;

    // --- D: no items, any CAD editor decides --------------------------------
    let d = at(&s
        .post(
            "create D: editor of another group, no items",
            &outsider,
            list,
            json!({ "title": "Lighter bracket" }),
        )
        .await);
    s.post(
        "decide D: any CAD editor",
        &eng,
        &format!("{d}/decide"),
        json!({ "decision": "accept" }),
    )
    .await;

    // --- lists ------------------------------------------------------------
    s.call(
        "list: open",
        &viewer,
        Method::GET,
        &format!("{list}?status=open"),
        None,
    )
    .await;
    s.call(
        "list: mine",
        &viewer,
        Method::GET,
        &format!("{list}?scope=mine&status=all"),
        None,
    )
    .await;
    s.call(
        "list: assigned to rolf",
        &eng2,
        Method::GET,
        &format!("{list}?scope=assigned"),
        None,
    )
    .await;
    s.call(
        "list: by item",
        &other,
        Method::GET,
        &format!("{list}?item={part}"),
        None,
    )
    .await;
    s.call(
        "list: unknown item",
        &other,
        Method::GET,
        &format!("{list}?item=PRT-999999"),
        None,
    )
    .await;

    let now = Value::Array(std::mem::take(&mut s.steps));
    app.teardown().await;

    let path = snapshot_path();
    if std::env::var_os("TEAMCRAFT_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&now).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (write it with TEAMCRAFT_UPDATE_SNAPSHOTS=1)",
            path.display()
        )
    }))
    .expect("snapshot is JSON");
    let (saved, now) = (saved.as_array().unwrap(), now.as_array().unwrap());
    for (old, new) in saved.iter().zip(now) {
        assert_eq!(
            old,
            new,
            "step `{}` changed",
            old["step"].as_str().unwrap_or("?")
        );
    }
    assert_eq!(saved.len(), now.len(), "number of steps changed");
}
