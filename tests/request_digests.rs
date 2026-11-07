//! E-mail digest of the change requests waiting for triage
//! (`services::request_digests`): recipients, opt-out, periods without
//! duplicates, triaged requests, restricted project groups – delivered
//! through the regular outbox to a recording mailer.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{delivery::Delivery, domain::Role, services::request_digests};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[derive(Default)]
struct RecordingMailer(Mutex<Vec<String>>);

impl teamcraft::delivery::MailSender for RecordingMailer {
    fn send(
        &self,
        message: lettre::Message,
    ) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        let text = String::from_utf8_lossy(&message.formatted()).to_string();
        self.0.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

impl RecordingMailer {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// The digests among the recorded e-mails: (recipient, whole message).
fn digests(mails: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = mails
        .iter()
        .filter(|m| m.contains("for triage"))
        .map(|m| {
            let to = m
                .lines()
                .find_map(|l| l.strip_prefix("To: "))
                .unwrap_or_default()
                .to_string();
            // quoted-printable: soft line breaks and the escaped `=`
            (to, m.replace("=\r\n", "").replace("=3D", "="))
        })
        .collect();
    out.sort();
    out
}

async fn raise(app: &common::TestApp, token: &str, title: &str, items: &[&str]) -> String {
    let items: Vec<Value> = items.iter().map(|i| json!({ "item": i })).collect();
    let r = app
        .api(
            Method::POST,
            "/api/v1/change-requests",
            token,
            Some(json!({ "title": title, "items": items })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn part(app: &common::TestApp, token: &str, project: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": format!("Part of {project}"), "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

/// As if a day had passed: the digest periods and the requests raised so
/// far move 25 hours into the past.
async fn a_day_later(app: &common::TestApp) {
    for sql in [
        "UPDATE request_digests SET covered_until = covered_until - interval '25 hours'",
        "UPDATE change_requests SET created_at = created_at - interval '25 hours'",
    ] {
        sqlx::query(sql).execute(&app.pool).await.unwrap();
    }
}

#[tokio::test]
async fn digest_of_requests_waiting_for_triage() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROVER"]).await;
    app.user_in("ivan", Role::Editor, &["ROVER"]).await; // no e-mail address
    let (_, otto) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let (_, vera) = app
        .user_in("vera", Role::Viewer, &["ROVER", "ROCKET"])
        .await;
    let (_, admin) = app.user("root", Role::Admin).await;
    for user in ["erika", "rolf", "otto", "vera", "root"] {
        sqlx::query("UPDATE users SET email = $1 || '@example.test' WHERE username = $1")
            .bind(user)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    // rolf does not want the digest (but the other notifications).
    let r = app
        .api(
            Method::PUT,
            "/api/v1/notifications/settings",
            &rolf,
            Some(json!({ "email": true, "mattermost": false, "muted": ["request_digest"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["muted"], json!(["request_digest"]));

    let rover = part(&app, &erika, "ROVER").await;
    let rocket = part(&app, &otto, "ROCKET").await;
    let cr1 = raise(&app, &vera, "Wheel rubs on the frame", &[&rover]).await;
    let cr2 = raise(&app, &vera, "Paint flakes off", &[&rover]).await;
    let cr3 = raise(&app, &vera, "Nozzle too heavy", &[&rocket]).await;

    let mailer = Arc::new(RecordingMailer::default());
    let delivery = Delivery::with_senders(
        "http://localhost:8080",
        "TeamCraft",
        Some((
            mailer.clone() as Arc<dyn teamcraft::delivery::MailSender>,
            "TeamCraft <teamcraft@example.test>",
        )),
        None,
    )
    .unwrap();
    // The immediate notifications of the new requests are not digests.
    delivery.run_once(&app.pool).await.unwrap();
    let immediate = mailer.take();
    assert!(!immediate.is_empty());
    assert!(digests(&immediate).is_empty());

    // --- the first digest: erika (ROVER) and otto (ROCKET) -------------------
    assert_eq!(request_digests::run_once(&app.pool, 24).await.unwrap(), 2);
    delivery.run_once(&app.pool).await.unwrap();
    let sent = digests(&mailer.take());
    assert_eq!(sent.len(), 2, "{sent:#?}");
    let (to, erika_mail) = &sent[0];
    assert!(to.contains("erika@example.test"), "{to}");
    assert!(
        erika_mail.contains("Subject: [TeamCraft] 2 new change requests in ROVER wait for triage")
    );
    assert!(erika_mail.contains(&cr1) && erika_mail.contains(&cr2));
    assert!(!erika_mail.contains(&cr3), "only the requests of the group");
    assert!(erika_mail.contains("raised by vera"));
    assert!(erika_mail.contains("http://localhost:8080/requests?status=new"));
    let (to, otto_mail) = &sent[1];
    assert!(to.contains("otto@example.test"), "{to}");
    assert!(otto_mail.contains("1 new change request in ROCKET waits for triage"));
    assert!(otto_mail.contains(&cr3));
    // In the app the digest is stored as read: it does not ring the bell.
    let list = app
        .api(Method::GET, "/api/v1/notifications", &erika, None)
        .await
        .json();
    let digest = list["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "request_digest")
        .cloned()
        .unwrap();
    assert!(!digest["read_at"].is_null());
    assert_eq!(digest["email_state"], "sent");
    assert_eq!(digest["chat_state"], "none", "e-mail only");

    // --- no duplicates: again right away (or after a restart) -----------------
    assert_eq!(request_digests::run_once(&app.pool, 24).await.unwrap(), 0);
    delivery.run_once(&app.pool).await.unwrap();
    assert!(digests(&mailer.take()).is_empty());

    // --- a day later without new requests: no digest -------------------------
    a_day_later(&app).await;
    assert_eq!(request_digests::run_once(&app.pool, 24).await.unwrap(), 0);
    let fresh: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM request_digests WHERE covered_until > now() - interval '1 hour'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(fresh, 2, "the quiet period is covered too");

    // --- triaged requests leave, new ones come in -----------------------------
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{cr1}/triage"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cr4 = raise(&app, &vera, "Bolt too short", &[&rover]).await;
    delivery.run_once(&app.pool).await.unwrap();
    mailer.take();
    a_day_later(&app).await;
    assert_eq!(request_digests::run_once(&app.pool, 24).await.unwrap(), 1);
    delivery.run_once(&app.pool).await.unwrap();
    let sent = digests(&mailer.take());
    assert_eq!(sent.len(), 1, "{sent:#?}");
    let mail = &sent[0].1;
    assert!(mail.contains("1 new change request in ROVER waits for triage"));
    assert!(mail.contains(&cr4));
    assert!(mail.contains(&format!("1 older request still waits for triage: {cr2}.")));
    assert!(!mail.contains(&cr1), "triaged");

    // --- restricted groups ----------------------------------------------------
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // A request on a ROVER and a ROCKET item: otto may not read it (it
    // touches the restricted group), so his ROCKET digest leaves it out.
    let cr5 = raise(
        &app,
        &vera,
        "Interface between rover and rocket",
        &[&rover, &rocket],
    )
    .await;
    delivery.run_once(&app.pool).await.unwrap();
    mailer.take();
    a_day_later(&app).await;
    assert_eq!(
        request_digests::run_once(&app.pool, 24).await.unwrap(),
        1,
        "erika only: nothing otto may read is new"
    );
    // erika leaves the group before the digest is sent: it is not delivered.
    sqlx::query(
        "DELETE FROM project_members WHERE user_id = (SELECT id FROM users WHERE username = 'erika')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    delivery.run_once(&app.pool).await.unwrap();
    let sent = digests(&mailer.take());
    assert!(sent.is_empty(), "{sent:#?}");
    let state: String = sqlx::query_scalar(
        "SELECT email_state FROM notifications WHERE kind = 'request_digest'
         ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(state, "skipped");
    let body: String = sqlx::query_scalar(
        "SELECT body FROM notifications WHERE kind = 'request_digest'
         ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(body.contains(&cr5), "the member's digest named it");

    // --- switched off ----------------------------------------------------------
    a_day_later(&app).await;
    assert_eq!(request_digests::run_once(&app.pool, 0).await.unwrap(), 0);
    app.teardown().await;
}
