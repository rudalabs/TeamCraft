//! "Fill with AI…" on the Results card of a simulation (issue #177): the box
//! posts what to extract and which report files to read, the assistant (here a
//! fake OpenAI-compatible server on localhost – no real LLM is ever called)
//! proposes values as drafts, and nothing becomes a result without a human.
//! The button and the route follow the rights of the user, the need-to-know
//! `assistant` switch of the group and the assistant being configured.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    domain::Role,
    services::project_policy::{self, AccessUpdate, Interface},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const ORIGIN: &str = "http://localhost:8080";

/// A fake chat-completions server: with `tool_args` set, the first reply of a
/// run calls `propose_result_values`, the next (after the tool result) is the
/// plain answer. Every request body is recorded.
struct FakeLlm {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    tool_args: Arc<Mutex<Option<Value>>>,
}

impl FakeLlm {
    async fn start() -> FakeLlm {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let tool_args = Arc::new(Mutex::new(None::<Value>));
        let (reqs, args) = (requests.clone(), tool_args.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (reqs, args) = (reqs.clone(), args.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let body = loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(pos) = text.find("\r\n\r\n") {
                            let head = text[..pos].to_ascii_lowercase();
                            let len = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if buf.len() >= pos + 4 + len {
                                break String::from_utf8_lossy(&buf[pos + 4..pos + 4 + len])
                                    .to_string();
                            }
                        }
                    };
                    let tool_result_seen = body.contains("\"role\":\"tool\"");
                    reqs.lock().unwrap().push(body);
                    let call = args.lock().unwrap().clone();
                    let message = match (call, tool_result_seen) {
                        (Some(a), false) => json!({
                            "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
                                "role": "assistant", "content": null,
                                "tool_calls": [{ "id": "c1", "type": "function", "function": {
                                    "name": "propose_result_values", "arguments": a.to_string() } }] } }],
                            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
                        }),
                        _ => json!({
                            "choices": [{ "index": 0, "finish_reason": "stop", "message": {
                                "role": "assistant",
                                "content": "Proposed 1 value. Not found: safety factor." } }],
                            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
                        }),
                    };
                    let payload = message.to_string();
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        FakeLlm {
            base_url,
            requests,
            tool_args,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// An app whose assistant talks to `llm`.
async fn app_with(llm: &FakeLlm) -> Option<common::TestApp> {
    let url: &'static str = Box::leak(llm.base_url.clone().into_boxed_str());
    let extra: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
        ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
        ("TEAMCRAFT_ASSISTANT_BASE_URL", url),
        ("TEAMCRAFT_ASSISTANT_MODEL", "fake-model"),
    ]));
    common::setup_with(extra).await
}

/// A simulation in `project` with a text report, created by `token`; returns
/// the item number and the revision id.
async fn simulation(app: &common::TestApp, token: &str, project: &str) -> (String, uuid::Uuid) {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "simulation", "name": "Bracket FEA", "project": project }),
        )
        .await;
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            token,
            "report.txt",
            b"Bracket FEA report\nMaximum von Mises stress: 182.5 MPa\nMax displacement: 0.42 mm\n",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&number)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    (number, id)
}

async fn runs(app: &common::TestApp) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'assistant.run'")
        .fetch_all(&app.pool)
        .await
        .unwrap()
}

async fn pending(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM result_proposals WHERE status = 'pending'")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn results(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM simulation_results")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn fill(
    app: &common::TestApp,
    cookie: &str,
    rev: uuid::Uuid,
    form: &[(&str, &str)],
) -> common::Response {
    app.post_form(
        &format!("/revisions/{rev}/simulation/fill-with-ai"),
        Some(cookie),
        Some(ORIGIN),
        form,
    )
    .await
}

fn flash_of(r: &common::Response) -> String {
    r.headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.contains("flash"))
        .map(|c| {
            let hex = c.split(';').next().unwrap().split('=').nth(1).unwrap();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            String::from_utf8_lossy(&bytes).to_string()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn without_an_assistant_there_is_no_button_and_no_route() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (_, rev) = simulation(&app, &eng, "ROVER").await;
    let cookie = app.login("eng").await;
    let page = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.text().contains("Fill with AI"));
    let r = fill(&app, &cookie, rev, &[("wanted", "max stress")]).await;
    assert_refused(&r);
    assert!(runs(&app).await.is_empty());
    app.teardown().await;
}

#[tokio::test]
async fn the_button_follows_the_rights_of_the_user() {
    let llm = FakeLlm::start().await;
    let Some(app) = app_with(&llm).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = simulation(&app, &eng, "ROVER").await;
    let uri = format!("/revisions/{rev}");

    let page = app.get_page(&uri, Some(&app.login("eng").await)).await;
    let text = page.text();
    assert!(text.contains("Fill with AI"), "editor sees the button");
    assert!(
        text.contains("name=\"file_0\" value=\"report.txt\" checked"),
        "report files are ticked by default"
    );
    assert!(text.contains("nothing becomes a result until you accept it"));

    for user in ["vera", "otto"] {
        let cookie = app.login(user).await;
        let page = app.get_page(&uri, Some(&cookie)).await;
        assert!(!page.text().contains("Fill with AI"), "{user}");
        let r = fill(&app, &cookie, rev, &[("wanted", "max stress")]).await;
        // refused: no run, no draft
        assert!(
            r.status == StatusCode::FORBIDDEN || r.status == StatusCode::SEE_OTHER,
            "{user}: {}",
            r.status
        );
        if r.status == StatusCode::SEE_OTHER {
            assert!(flash_of(&r).contains("error"), "{user}: {}", flash_of(&r));
        }
    }
    assert!(runs(&app).await.is_empty());
    assert!(llm.requests().is_empty());
    assert_eq!(pending(&app).await, 0);
    app.teardown().await;
}

#[tokio::test]
async fn pressing_the_button_proposes_drafts_and_never_a_result() {
    let llm = FakeLlm::start().await;
    let Some(app) = app_with(&llm).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (number, rev) = simulation(&app, &eng, "ROVER").await;
    *llm.tool_args.lock().unwrap() = Some(json!({
        "item": number,
        "values": [{
            "name": "Max. von Mises stress", "value": 182.5, "unit": "MPa", "limit_max": 250,
            "file": "report.txt", "quote": "Maximum von Mises stress: 182.5 MPa"
        }]
    }));
    let cookie = app.login("eng").await;
    let r = fill(
        &app,
        &cookie,
        rev,
        &[
            ("wanted", "max von Mises stress, safety factor"),
            ("limits", "upper limit 250 MPa for stress"),
            ("file_0", "report.txt"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let location = r.headers["location"].to_str().unwrap().to_string();
    assert!(location.ends_with("#result-proposals"), "{location}");
    let flash = flash_of(&r);
    assert!(flash.contains("proposed 1 value"), "{flash}");
    assert!(flash.contains("Not found: safety factor"), "{flash}");

    // a draft with its quote, not a result
    assert_eq!(pending(&app).await, 1);
    assert_eq!(results(&app).await, 0);
    let page = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await;
    assert!(page.text().contains("Maximum von Mises stress: 182.5 MPa"));

    // the user's text reached the model as the request, with the file choice
    let sent = llm.requests().join("\n");
    assert!(sent.contains("Extract only these values: max von Mises stress, safety factor"));
    assert!(sent.contains("upper limit 250 MPa for stress"));
    assert!(sent.contains("report.txt"));

    // audited as any run: no text, no answer; drafts were allowed
    let runs = runs(&app).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["action"], "result_values");
    assert_eq!(runs[0]["allow_drafts"], true);
    assert!(runs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "propose_result_values"));
    let text = runs[0].to_string();
    assert!(!text.contains("von Mises stress, safety"), "{text}");
    assert!(!text.contains("Not found"), "{text}");
    app.teardown().await;
}

#[tokio::test]
async fn invalid_requests_are_refused_before_the_assistant_runs() {
    let llm = FakeLlm::start().await;
    let Some(app) = app_with(&llm).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (_, rev) = simulation(&app, &eng, "ROVER").await;
    let part = app
        .create_item(
            &eng,
            json!({ "item_type": "part", "name": "P", "project": "ROVER" }),
        )
        .await;
    let part_rev: uuid::Uuid = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&part)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let cookie = app.login("eng").await;

    // a file that is not a report of this revision
    let r = fill(&app, &cookie, rev, &[("file_0", "other.pdf")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        flash_of(&r).contains("not a report file"),
        "{}",
        flash_of(&r)
    );
    // too long
    let long = "x".repeat(4_001);
    let r = fill(&app, &cookie, rev, &[("wanted", &long)]).await;
    assert!(flash_of(&r).contains("too long"), "{}", flash_of(&r));
    let long = "x".repeat(501);
    let r = fill(&app, &cookie, rev, &[("limits", &long)]).await;
    assert!(flash_of(&r).contains("too long"), "{}", flash_of(&r));
    // not a simulation
    let r = fill(&app, &cookie, part_rev, &[]).await;
    assert!(flash_of(&r).contains("Only simulation"), "{}", flash_of(&r));
    // an unknown revision
    let r = fill(&app, &cookie, uuid::Uuid::new_v4(), &[]).await;
    assert_refused(&r);
    // cross-site posts are refused by the origin check
    let r = app
        .post_form(
            &format!("/revisions/{rev}/simulation/fill-with-ai"),
            Some(&cookie),
            Some("https://evil.example"),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(llm.requests().is_empty());
    assert!(runs(&app).await.is_empty());
    app.teardown().await;
}

#[tokio::test]
async fn a_group_that_switched_the_assistant_off_gets_no_button_and_a_404() {
    let llm = FakeLlm::start().await;
    let Some(app) = app_with(&llm).await else {
        return;
    };
    let (_, owner) = app.user("erika", Role::Editor).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &owner,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let (_, rev) = simulation(&app, &owner, "NTK").await;
    app.user("outsider", Role::Editor).await;
    let uri = format!("/revisions/{rev}");
    let cookie = app.login("erika").await;

    // switches default to allowed: the button is there
    let page = app.get_page(&uri, Some(&cookie)).await;
    assert!(page.text().contains("Fill with AI"));

    let actor = app.actor("erika").await;
    let id = app.project_id("NTK").await;
    project_policy::set_access(
        &app.pool,
        &actor,
        id,
        AccessUpdate {
            interfaces: vec![(Interface::Assistant, false)],
            ..AccessUpdate::default()
        },
    )
    .await
    .expect("set_access");

    let page = app.get_page(&uri, Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK, "the web UI keeps working");
    assert!(!page.text().contains("Fill with AI"));
    let r = fill(&app, &cookie, rev, &[("wanted", "stress")]).await;
    assert_refused(&r);

    // a user outside the restricted group sees it as missing, either way
    let other = app.login("outsider").await;
    let r = fill(&app, &other, rev, &[("wanted", "stress")]).await;
    assert_refused(&r);
    assert!(llm.requests().is_empty());
    assert!(runs(&app).await.is_empty());
    app.teardown().await;
}

/// A refused request goes back to the page with an error message (the web
/// UI's post/redirect/get); nothing is run.
fn assert_refused(r: &common::Response) {
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let flash = flash_of(r);
    assert!(flash.contains("\"error\""), "{flash}");
}
