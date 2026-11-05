//! End-to-end tests of outgoing event webhooks: events written in the
//! transaction of the business event, signed delivery to a local HTTP
//! receiver, retries with backoff, `SKIP LOCKED` claims and permissions.

mod common;

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Router,
};
use serde_json::{json, Value};
use teamcraft::{
    config::WebhookConfig,
    domain::{Role, WebhookEvent},
    services::webhooks as hooks,
    webhooks::{self, Dispatcher},
};
use uuid::Uuid;

const PUBLIC_URL: &str = "https://plm.example.org";

macro_rules! app_or_skip {
    () => {
        match common::setup_with(&[("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE", "true")]).await {
            Some(app) => app,
            None => return,
        }
    };
}

/// A local HTTP server that records every request and answers with
/// `status`.
struct Receiver {
    status: AtomicU16,
    hits: Mutex<Vec<(HeaderMap, Bytes)>>,
}

impl Receiver {
    async fn start() -> (Arc<Receiver>, String) {
        let receiver = Arc::new(Receiver {
            status: AtomicU16::new(200),
            hits: Mutex::new(Vec::new()),
        });
        async fn hook(
            State(r): State<Arc<Receiver>>,
            headers: HeaderMap,
            body: Bytes,
        ) -> StatusCode {
            r.hits.lock().unwrap().push((headers, body));
            StatusCode::from_u16(r.status.load(Ordering::SeqCst)).unwrap()
        }
        let router = Router::new()
            .route("/hook", post(hook))
            .with_state(receiver.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (receiver, format!("http://{addr}/hook"))
    }

    fn hits(&self) -> Vec<(HeaderMap, Bytes)> {
        self.hits.lock().unwrap().clone()
    }

    fn set_status(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }
}

fn dispatcher() -> Dispatcher {
    Dispatcher::new(
        PUBLIC_URL,
        &WebhookConfig {
            allow_private: true,
            ..WebhookConfig::default()
        },
    )
    .unwrap()
}

async fn create_webhook(
    app: &common::TestApp,
    admin: &str,
    url: &str,
    events: &[&str],
    project: Option<&str>,
) -> (Uuid, String) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            admin,
            Some(json!({ "name": "ERP", "url": url, "events": events, "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body = r.json();
    assert!(
        body["webhook"].get("secret").is_none(),
        "the webhook object never carries the secret"
    );
    (
        Uuid::parse_str(body["webhook"]["id"].as_str().unwrap()).unwrap(),
        body["secret"].as_str().unwrap().to_string(),
    )
}

async fn create_part(app: &common::TestApp, token: &str, project: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": "Bracket", "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

/// Releases revision A of `item` through a change; returns the change number.
async fn release(
    app: &common::TestApp,
    author: &str,
    reviewer: &str,
    reviewer_name: &str,
    item: &str,
) -> String {
    let call = |method: Method, uri: String, token: &str, body: Option<Value>| {
        let token = token.to_string();
        async move { app.api(method, &uri, &token, body).await }
    };
    let r = call(
        Method::POST,
        "/api/v1/changes".into(),
        author,
        Some(json!({ "title": "Release" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (uri, body) in [
        (
            format!("/api/v1/changes/{chg}/items"),
            json!({ "item": item, "revision": "A" }),
        ),
        (
            format!("/api/v1/changes/{chg}/reviewers"),
            json!({ "username": reviewer_name }),
        ),
    ] {
        let r = call(Method::POST, uri, author, Some(body)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg}/submit"),
        author,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg}/review"),
        reviewer,
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
    chg
}

#[derive(sqlx::FromRow, Debug)]
struct Row {
    id: Uuid,
    event: String,
    payload: Value,
    status: String,
    attempts: i32,
    last_status_code: Option<i32>,
}

async fn deliveries(app: &common::TestApp) -> Vec<Row> {
    sqlx::query_as::<_, Row>(
        "SELECT id, event, payload, status, attempts, last_status_code
         FROM webhook_deliveries ORDER BY created_at, event",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

#[tokio::test]
async fn release_events_are_signed_and_delivered() {
    let app = app_or_skip!();
    let (_, admin) = app.user("ada", Role::Admin).await;
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rev_token) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (receiver, url) = Receiver::start().await;
    let (_, secret) = create_webhook(
        &app,
        &admin,
        &url,
        &[
            "change.submitted",
            "change.approved",
            "revision.released",
            "change.released",
            "revision.obsoleted",
        ],
        None,
    )
    .await;
    // a webhook of another project group and an unsubscribed one get nothing
    let (rover_hook, _) =
        create_webhook(&app, &admin, &url, &["revision.released"], Some("ROVER")).await;
    let (other_hook, _) =
        create_webhook(&app, &admin, &url, &["change_request.created"], None).await;

    let part = create_part(&app, &eng, "ROCKET").await;
    let chg = release(&app, &eng, &rev_token, "rolf", &part).await;

    let rows = deliveries(&app).await;
    let events: Vec<&str> = rows.iter().map(|r| r.event.as_str()).collect();
    for e in [
        "change.submitted",
        "change.approved",
        "revision.released",
        "change.released",
    ] {
        assert_eq!(
            events.iter().filter(|x| **x == e).count(),
            1,
            "{e} once: {events:?}"
        );
    }
    assert_eq!(
        rows.len(),
        4,
        "nothing for the ROVER or the request webhook"
    );
    let count = |id: Uuid| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE webhook_id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(count(rover_hook).await + count(other_hook).await, 0);

    // payload shape
    let released = &rows
        .iter()
        .find(|r| r.event == "revision.released")
        .unwrap()
        .payload;
    assert_eq!(released["event"], "revision.released");
    assert!(Uuid::parse_str(released["id"].as_str().unwrap()).is_ok());
    assert!(
        chrono::DateTime::parse_from_rfc3339(released["occurred_at"].as_str().unwrap()).is_ok()
    );
    assert_eq!(released["actor"], json!({ "username": "rolf" }));
    assert_eq!(released["revision"]["item_number"], part.as_str());
    assert_eq!(released["revision"]["code"], "A");
    assert_eq!(released["revision"]["state"], "released");
    assert_eq!(released["revision"]["project"], "ROCKET");
    assert_eq!(
        released["revision"]["api_url"],
        format!("/api/v1/items/{part}/revisions/A")
    );
    assert!(released["revision"]["web_url"]
        .as_str()
        .unwrap()
        .starts_with("/revisions/"));
    assert_eq!(released["change"]["number"], chg.as_str());
    assert_eq!(released["change"]["status"], "released");
    let submitted = &rows
        .iter()
        .find(|r| r.event == "change.submitted")
        .unwrap()
        .payload;
    assert_eq!(submitted["actor"]["username"], "erika");
    assert_eq!(submitted["change"]["status"], "in_review");
    assert_eq!(submitted["revisions"][0]["state"], "in_review");
    let approved = &rows
        .iter()
        .find(|r| r.event == "change.approved")
        .unwrap()
        .payload;
    assert_eq!(approved["review"]["reviewer"], "rolf");
    assert_eq!(approved["review"]["change_status"], "released");
    let change_released = &rows
        .iter()
        .find(|r| r.event == "change.released")
        .unwrap()
        .payload;
    assert_eq!(change_released["revisions"][0]["state"], "released");
    assert_eq!(
        change_released["change"]["api_url"],
        format!("/api/v1/changes/{chg}")
    );
    for r in &rows {
        let text = r.payload.to_string();
        assert!(
            !text.contains("tc_whsec_") && !text.contains("tc_pat_"),
            "nothing secret"
        );
    }

    // delivery: signed, one request per row, absolute URLs
    let report = dispatcher().run_once(&app.pool).await.unwrap();
    assert_eq!(report.sent, 4, "{report:?}");
    let hits = receiver.hits();
    assert_eq!(hits.len(), 4);
    let ids: HashSet<Uuid> = rows.iter().map(|r| r.id).collect();
    for (headers, body) in &hits {
        assert!(
            webhooks::verify_signature(&secret, body, header(headers, "x-teamcraft-signature")),
            "valid signature"
        );
        assert_eq!(header(headers, "content-type"), "application/json");
        let payload: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            header(headers, "x-teamcraft-event"),
            payload["event"].as_str().unwrap()
        );
        let delivery = Uuid::parse_str(header(headers, "x-teamcraft-delivery")).unwrap();
        assert!(ids.contains(&delivery));
        if let Some(rev) = payload.get("revision") {
            assert!(rev["web_url"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{PUBLIC_URL}/revisions/")));
            assert!(rev["api_url"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{PUBLIC_URL}/api/v1/")));
        }
    }
    assert!(deliveries(&app)
        .await
        .iter()
        .all(|r| r.status == "sent" && r.attempts == 1 && r.last_status_code == Some(200)));
    assert_eq!(
        dispatcher().run_once(&app.pool).await.unwrap().sent,
        0,
        "sent once"
    );

    // obsolete: one more event
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/obsolete"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = deliveries(&app).await;
    let obsoleted = rows
        .iter()
        .find(|r| r.event == "revision.obsoleted")
        .unwrap();
    assert_eq!(obsoleted.payload["revision"]["state"], "obsolete");
    assert_eq!(obsoleted.status, "pending");
    app.teardown().await;
}

#[tokio::test]
async fn requests_and_simulations_emit_events() {
    let app = app_or_skip!();
    let (_, admin) = app.user("ada", Role::Admin).await;
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    create_webhook(
        &app,
        &admin,
        "http://127.0.0.1:9/hook",
        &[
            "change_request.created",
            "change_request.decided",
            "simulation.result_added",
        ],
        Some("ROCKET"),
    )
    .await;
    let part = create_part(&app, &eng, "ROCKET").await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/change-requests",
            &viewer,
            Some(json!({ "title": "Seam cracks", "reason": "manufacturing",
                         "items": [{ "item": part, "revision": "A" }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ecr = r.json()["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();
    for (uri, body) in [
        (format!("/api/v1/change-requests/{ecr}/triage"), None),
        (
            format!("/api/v1/change-requests/{ecr}/decide"),
            Some(json!({ "decision": "reject", "comment": "Within tolerance." })),
        ),
    ] {
        let r = app.api(Method::POST, &uri, &eng, body).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let rows = deliveries(&app).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let created = &rows
        .iter()
        .find(|r| r.event == "change_request.created")
        .unwrap()
        .payload;
    assert_eq!(created["actor"]["username"], "vera");
    assert_eq!(created["change_request"]["number"], ecr.as_str());
    assert_eq!(created["change_request"]["status"], "new");
    assert_eq!(created["items"][0], format!("{part}/A"));
    let decided = &rows
        .iter()
        .find(|r| r.event == "change_request.decided")
        .unwrap()
        .payload;
    assert_eq!(decided["decision"]["decision"], "rejected");
    assert_eq!(decided["decision"]["comment"], "Within tolerance.");
    assert_eq!(decided["change_request"]["status"], "rejected");
    assert_eq!(
        decided["change_request"]["api_url"],
        format!("/api/v1/change-requests/{ecr}")
    );

    // simulation results: one value, then a result table from a solver script
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "simulation", "name": "Bracket – static", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let sim = r.json()["item_number"].as_str().unwrap().to_string();
    let results = format!("/api/v1/items/{sim}/revisions/A/simulation/results");
    let r = app
        .api(
            Method::POST,
            &results,
            &eng,
            Some(json!({ "name": "Max. stress", "value": 182.5, "unit": "MPa", "upper_limit": 250.0 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            &results,
            &eng,
            Some(json!({ "results": [
                { "name": "Max. stress", "value": 190.0, "unit": "MPa", "upper_limit": 250.0 },
                { "name": "Safety factor", "value": 1.3, "lower_limit": 1.5 }
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = deliveries(&app).await;
    let sims: Vec<&Value> = rows
        .iter()
        .filter(|r| r.event == "simulation.result_added")
        .map(|r| &r.payload)
        .collect();
    assert_eq!(sims.len(), 2, "{rows:?}");
    let single = sims.iter().find(|p| p.get("result").is_some()).unwrap();
    assert_eq!(single["result"]["name"], "Max. stress");
    assert_eq!(single["result"]["status"], "pass");
    assert_eq!(single["revision"]["item_number"], sim.as_str());
    let import = sims.iter().find(|p| p.get("import").is_some()).unwrap();
    assert_eq!(import["import"]["added"], 1);
    assert_eq!(import["import"]["updated"], 1);
    app.teardown().await;
}

/// The outbox row is part of the caller's transaction: a rollback leaves no
/// delivery, a failed business action queues nothing.
#[tokio::test]
async fn events_roll_back_with_their_transaction() {
    let app = app_or_skip!();
    let (admin_actor, admin) = app.user("ada", Role::Admin).await;
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    create_webhook(
        &app,
        &admin,
        "http://127.0.0.1:9/hook",
        &["change.released", "change.submitted"],
        None,
    )
    .await;

    let mut tx = app.pool.begin().await.unwrap();
    let queued = hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(queued, 1);
    tx.rollback().await.unwrap();
    assert!(deliveries(&app).await.is_empty(), "rolled back");

    let mut tx = app.pool.begin().await.unwrap();
    hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(deliveries(&app).await.len(), 1, "committed");

    // a submit that fails (no reviewer) queues nothing
    let part = create_part(&app, &eng, "ROCKET").await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &eng,
            Some(json!({ "title": "X" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/items"),
            &eng,
            Some(json!({ "item": part, "revision": "A" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(deliveries(&app).await.len(), 1, "no change.submitted");
    app.teardown().await;
}

#[tokio::test]
async fn failed_deliveries_back_off_and_give_up() {
    let app = app_or_skip!();
    let (admin_actor, admin) = app.user("ada", Role::Admin).await;
    let (receiver, url) = Receiver::start().await;
    let (hook, _) = create_webhook(&app, &admin, &url, &["change.released"], None).await;
    let mut tx = app.pool.begin().await.unwrap();
    hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let d = dispatcher();

    receiver.set_status(503);
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!(report.retrying, 1, "{report:?}");
    let row = &deliveries(&app).await[0];
    assert_eq!(
        (row.status.as_str(), row.attempts, row.last_status_code),
        ("pending", 1, Some(503))
    );
    let wait: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_attempt_at - now())::float8 FROM webhook_deliveries",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let expected = webhooks::retry_delay(row.id, 1).as_secs_f64();
    assert!(
        (wait - expected).abs() < 5.0,
        "next attempt in {wait} s, expected {expected} s"
    );
    assert!((60.0..=76.0).contains(&expected));
    assert_eq!(
        d.run_once(&app.pool).await.unwrap(),
        Default::default(),
        "not due yet"
    );

    // due again: the receiver is back
    receiver.set_status(204);
    sqlx::query("UPDATE webhook_deliveries SET next_attempt_at = now()")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(d.run_once(&app.pool).await.unwrap().sent, 1);
    let row = &deliveries(&app).await[0];
    assert_eq!(
        (row.status.as_str(), row.attempts, row.last_status_code),
        ("sent", 2, Some(204))
    );
    let hits = receiver.hits();
    assert_eq!(
        header(&hits[0].0, "x-teamcraft-delivery"),
        header(&hits[1].0, "x-teamcraft-delivery"),
        "the delivery id stays the same over retries"
    );

    // the last attempt fails: given up
    receiver.set_status(500);
    sqlx::query(
        "UPDATE webhook_deliveries SET status = 'pending', attempts = $1 - 1, next_attempt_at = now()",
    )
    .bind(webhooks::MAX_ATTEMPTS)
    .execute(&app.pool)
    .await
    .unwrap();
    assert_eq!(d.run_once(&app.pool).await.unwrap().failed, 1);
    let row = &deliveries(&app).await[0];
    assert_eq!(
        (row.status.as_str(), row.attempts),
        ("failed", webhooks::MAX_ATTEMPTS)
    );

    // an unreachable receiver is an error without a status code
    sqlx::query("UPDATE webhooks SET url = 'http://127.0.0.1:9/closed'")
        .execute(&app.pool)
        .await
        .unwrap();
    receiver.set_status(200);
    // "Retry" through the API: fresh attempts, tried at once
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/webhook-deliveries/{}/retry", row.id),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(
        (body["status"].as_str(), body["attempts"].as_i64()),
        (Some("pending"), Some(1))
    );
    assert!(body["last_status_code"].is_null());
    assert!(!body["last_error"].as_str().unwrap().is_empty());
    assert!(
        !body["last_error"].as_str().unwrap().contains("/closed"),
        "no URL in errors"
    );

    // deactivating gives up what is waiting
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/webhooks/{hook}"),
            &admin,
            Some(json!({ "active": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["active"], false);
    let row = &deliveries(&app).await[0];
    assert_eq!(row.status, "failed");
    let mut tx = app.pool.begin().await.unwrap();
    let queued = hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(queued, 0, "inactive webhooks receive nothing");
    app.teardown().await;
}

/// A webhook limited to a project group sees only that group's revisions of
/// a change that spans several groups; unlimited webhooks see all.
#[tokio::test]
async fn project_webhooks_see_only_their_revisions() {
    let app = app_or_skip!();
    let (admin_actor, admin) = app.user("ada", Role::Admin).await;
    let (rover_hook, _) = create_webhook(
        &app,
        &admin,
        "http://127.0.0.1:9/rover",
        &["change.submitted"],
        Some("ROVER"),
    )
    .await;
    let (all_hook, _) = create_webhook(
        &app,
        &admin,
        "http://127.0.0.1:9/all",
        &["change.submitted"],
        None,
    )
    .await;
    let ids: Vec<(String, Uuid)> =
        sqlx::query_as("SELECT code, id FROM projects WHERE code IN ('ROVER', 'ROCKET')")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    let id_of = |code: &str| ids.iter().find(|(c, _)| c == code).map(|(_, id)| *id);
    let mut tx = app.pool.begin().await.unwrap();
    let queued = hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeSubmitted,
        &[id_of("ROCKET"), id_of("ROVER"), None],
        json!({
            "change": { "number": "CHG-000001" },
            "revisions": [
                { "label": "PRT-1/A", "project": "ROCKET" },
                { "label": "PRT-2/A", "project": "ROVER" },
                { "label": "PRT-3/A", "project": null },
                { "label": "PRT-4/A", "project": "ROVER" },
            ],
        }),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(queued, 2);
    let payload_of = |hook: Uuid| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT payload FROM webhook_deliveries WHERE webhook_id = $1",
            )
            .bind(hook)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let labels = |p: &Value| -> Vec<String> {
        p["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["label"].as_str().unwrap().to_string())
            .collect()
    };
    let rover = payload_of(rover_hook).await;
    assert_eq!(
        labels(&rover),
        ["PRT-2/A", "PRT-4/A"],
        "own group, in order"
    );
    assert_eq!(rover["change"]["number"], "CHG-000001");
    assert_eq!(labels(&payload_of(all_hook).await).len(), 4);
    app.teardown().await;
}

/// A leased delivery (another sender's attempt may still be running) is not
/// sent again at once; "Retry" makes it due; the cleanup gives up rows that
/// are still pending for an inactive webhook.
#[tokio::test]
async fn leased_deliveries_are_not_sent_twice() {
    let app = app_or_skip!();
    let (admin_actor, admin) = app.user("ada", Role::Admin).await;
    let (receiver, url) = Receiver::start().await;
    create_webhook(&app, &admin, &url, &["change.released"], None).await;
    let mut tx = app.pool.begin().await.unwrap();
    hooks::emit(
        &mut tx,
        Some(&admin_actor),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM webhook_deliveries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    // another sender claimed it (attempt 1, leased) and is still waiting for
    // the answer
    sqlx::query(
        "UPDATE webhook_deliveries
         SET attempts = 1, next_attempt_at = now() + interval '2 minutes'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let d = dispatcher();
    assert_eq!(
        d.deliver_now(&app.pool, id).await.unwrap(),
        None,
        "a leased delivery is not sent twice"
    );
    assert!(receiver.hits().is_empty());

    // "Retry" resets it; the new attempt is again number 1
    receiver.set_status(503);
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/webhook-deliveries/{id}/retry"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["attempts"], 1);
    assert_eq!(r.json()["last_status_code"], 503);
    assert_eq!(receiver.hits().len(), 1);

    // pending rows of a deactivated webhook are given up by the cleanup
    sqlx::query("UPDATE webhooks SET active = FALSE")
        .execute(&app.pool)
        .await
        .unwrap();
    webhooks::purge_finished(&app.pool, 30).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_deliveries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    app.teardown().await;
}

/// Several senders never send the same delivery: rows are claimed with
/// `FOR UPDATE SKIP LOCKED`.
#[tokio::test]
async fn claims_skip_locked_rows() {
    let app = app_or_skip!();
    let (_, admin) = app.user("ada", Role::Admin).await;
    let (receiver, url) = Receiver::start().await;
    let (hook, _) = create_webhook(&app, &admin, &url, &["change.released"], None).await;
    sqlx::query(
        "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event, payload)
         SELECT gen_random_uuid(), $1, gen_random_uuid(), 'ping', '{\"event\": \"ping\"}'
         FROM generate_series(1, 60)",
    )
    .bind(hook)
    .execute(&app.pool)
    .await
    .unwrap();

    // another server holds five rows
    let mut other = app.pool.begin().await.unwrap();
    let held: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM webhook_deliveries ORDER BY id LIMIT 5 FOR UPDATE")
            .fetch_all(&mut *other)
            .await
            .unwrap();

    // two senders at the same time
    let (a, b) = (dispatcher(), dispatcher());
    loop {
        let (ra, rb) = tokio::join!(a.run_once(&app.pool), b.run_once(&app.pool));
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        if ra.sent + rb.sent == 0 {
            break;
        }
    }
    let sent: HashSet<String> = receiver
        .hits()
        .iter()
        .map(|(h, _)| header(h, "x-teamcraft-delivery").to_string())
        .collect();
    assert_eq!(receiver.hits().len(), 55, "every free row once");
    assert_eq!(sent.len(), 55, "no row twice");
    for id in &held {
        assert!(!sent.contains(&id.to_string()), "locked rows are skipped");
    }
    other.commit().await.unwrap();
    assert_eq!(a.run_once(&app.pool).await.unwrap().sent, 5);
    assert_eq!(receiver.hits().len(), 60);
    app.teardown().await;
}

#[tokio::test]
async fn administration_and_permissions() {
    let app = match common::setup().await {
        Some(app) => app,
        None => return,
    };
    let (_, admin) = app.user("ada", Role::Admin).await;
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;

    // only administrators
    for (method, uri) in [
        (Method::GET, "/api/v1/webhooks"),
        (Method::POST, "/api/v1/webhooks"),
    ] {
        let body = (method == Method::POST).then(|| {
            json!({ "name": "x", "url": "https://ci.example.org/x", "events": ["change.released"] })
        });
        let r = app.api(method, uri, &eng, body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}: {}", r.text());
    }
    let cookie = app.login("erika").await;
    let r = app.get_page("/admin/webhooks", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // the default target guard refuses internal receivers
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &admin,
            Some(json!({ "name": "x", "url": "http://127.0.0.1:8080/x", "events": ["change.released"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS"));
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &admin,
            Some(json!({ "name": "x", "url": "https://ci.example.org/x", "events": ["item.deleted"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // the web UI: create shows the secret once, the page later only the hint
    let admin_cookie = app.login("ada").await;
    let r = app
        .post_form(
            "/admin/webhooks",
            Some(&admin_cookie),
            Some("http://localhost:8080"),
            &[
                ("name", "CAM job"),
                ("url", "https://cam.example.org/jobs?token=abc"),
                ("event", "revision.released"),
                ("event", "change.released"),
                ("active", "on"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let html = r.text();
    let start = html.find("tc_whsec_").expect("the secret is shown");
    let secret = &html[start..start + "tc_whsec_".len() + 64];
    let id: String = sqlx::query_scalar("SELECT id::text FROM webhooks WHERE name = 'CAM job'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let page = app
        .get_page(&format!("/admin/webhooks/{id}"), Some(&admin_cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.text().contains(secret), "never shown again");
    assert!(
        page.text().contains(&secret[secret.len() - 4..]),
        "the hint"
    );
    let list = app
        .api(Method::GET, "/api/v1/webhooks", &admin, None)
        .await
        .json();
    assert_eq!(
        list["webhooks"][0]["events"],
        json!(["revision.released", "change.released"])
    );
    assert!(!list.to_string().contains(secret));

    // rotate, test (the receiver is unreachable: an error, retried later), delete
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/webhooks/{id}/rotate-secret"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_ne!(r.json()["secret"].as_str().unwrap(), secret);
    let r = app
        .post_form(
            &format!("/admin/webhooks/{id}/test"),
            Some(&admin_cookie),
            Some("http://localhost:8080"),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/webhooks/{id}/deliveries"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.json()[0]["event"], "ping");
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/webhooks/{id}"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/webhooks/{id}"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "deliveries go with the webhook");

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE entity_type = 'webhook' ORDER BY at, id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "webhook.create",
            "webhook.rotate_secret",
            "webhook.test",
            "webhook.delete"
        ]
    );
    let details: Vec<String> = sqlx::query_scalar(
        "SELECT summary || details::text FROM audit_log WHERE entity_type = 'webhook'",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert!(
        details
            .iter()
            .all(|d| !d.contains("token=abc") && !d.contains(secret)),
        "audit records hold neither the URL's token nor the secret: {details:?}"
    );
    app.teardown().await;
}
