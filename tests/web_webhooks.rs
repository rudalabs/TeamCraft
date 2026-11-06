//! The web pages of the webhook administration (`/admin/webhooks`,
//! `web/webhooks.rs`): administrators only, cross-site posts blocked, the
//! form validated (including the target rules against internal
//! addresses), and test delivery, retry, secret rotation and deletion with
//! their messages – the REST side is in `tests/webhooks.rs`.

mod common;

use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc, Mutex,
};

use axum::{
    extract::State,
    http::{header, StatusCode},
    routing::post,
    Router,
};
use teamcraft::domain::Role;

const ORIGIN: Option<&str> = Some("http://localhost:8080");

/// A receiver on a local port that counts requests and answers `status`.
struct Receiver {
    status: AtomicU16,
    hits: Mutex<usize>,
}

async fn receiver() -> (Arc<Receiver>, String) {
    async fn hook(State(r): State<Arc<Receiver>>) -> StatusCode {
        *r.hits.lock().unwrap() += 1;
        StatusCode::from_u16(r.status.load(Ordering::SeqCst)).unwrap()
    }
    let r = Arc::new(Receiver {
        status: AtomicU16::new(200),
        hits: Mutex::new(0),
    });
    let router = Router::new()
        .route("/hook", post(hook))
        .with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (r, format!("http://{addr}/hook"))
}

/// The redirect target of a form post and its flash message as
/// `"<kind>: <message>"` (the cookie holds the message as hex-encoded JSON).
fn flash(r: &common::Response) -> (String, String) {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let location = r.header("location").expect("redirect target");
    let value = r
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix("tc_flash="))
        .map(|c| c.split(';').next().unwrap().to_string())
        .expect("a flash message");
    let json: serde_json::Value =
        serde_json::from_slice(&hex::decode(value).expect("hex")).expect("json");
    (
        location,
        format!(
            "{}: {}",
            json["kind"].as_str().unwrap(),
            json["message"].as_str().unwrap()
        ),
    )
}

/// The message of an error flash (empty for any other kind).
fn flash_error(text: &str) -> String {
    text.strip_prefix("error: ").unwrap_or_default().to_string()
}

async fn webhook_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM webhooks")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// Name, URL, events, active and secret hash of the one webhook.
async fn state(app: &common::TestApp, id: &str) -> (String, String, Vec<String>, bool, String) {
    sqlx::query_as(
        "SELECT name, url, events, active, secret FROM webhooks WHERE id = $1::text::uuid",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

/// Creates a webhook through the form; returns its id.
async fn create(app: &common::TestApp, cookie: &str, name: &str, url: &str) -> String {
    let r = app
        .post_form(
            "/admin/webhooks",
            Some(cookie),
            ORIGIN,
            &[
                ("name", name),
                ("url", url),
                ("event", "change.released"),
                ("active", "on"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("tc_whsec_"), "the secret is shown once");
    sqlx::query_scalar("SELECT id::text FROM webhooks WHERE name = $1")
        .bind(name)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn only_administrators_and_only_from_the_same_site() {
    let Some(app) = common::setup().await else {
        return;
    };
    app.user("ada", Role::Admin).await;
    app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let admin = app.login("ada").await;
    let id = create(
        &app,
        &admin,
        "ERP",
        "https://erp.example.org/events?token=t0p",
    )
    .await;
    let before = state(&app, &id).await;
    let delivery: String = {
        let r = app
            .post_form(
                &format!("/admin/webhooks/{id}/test"),
                Some(&admin),
                ORIGIN,
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
        sqlx::query_scalar("SELECT id::text FROM webhook_deliveries")
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let audit = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE entity_type = 'webhook'")
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let audit_before = audit().await;

    // a CAD editor: the pages answer 403, the actions change nothing
    let erika = app.login("erika").await;
    for uri in [
        "/admin/webhooks".to_string(),
        format!("/admin/webhooks/{id}"),
    ] {
        let r = app.get_page(&uri, Some(&erika)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert!(!r.text().contains("erp.example.org"), "{uri}");
    }
    let actions = [
        format!("/admin/webhooks/{id}"),
        format!("/admin/webhooks/{id}/delete"),
        format!("/admin/webhooks/{id}/rotate-secret"),
        format!("/admin/webhooks/{id}/test"),
        format!("/admin/webhooks/{id}/deliveries/{delivery}/retry"),
        "/admin/webhooks".to_string(),
    ];
    let form = [
        ("name", "Hijacked"),
        ("url", "https://evil.example.net/x"),
        ("event", "change.released"),
        ("active", "on"),
    ];
    for uri in &actions {
        let r = app.post_form(uri, Some(&erika), ORIGIN, &form).await;
        assert_ne!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(!r.text().contains("tc_whsec_"), "{uri}: no secret");
        if r.status == StatusCode::SEE_OTHER {
            let (_, message) = flash(&r);
            assert!(
                flash_error(&message).contains("requires the 'Admin' role"),
                "{uri}: {message}"
            );
        } else {
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        }
    }
    // an administrator's session, but a post from another site
    for uri in &actions {
        let r = app
            .post_form(uri, Some(&admin), Some("https://evil.example.net"), &form)
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert!(r.text().contains("Cross-site request blocked"), "{uri}");
    }
    let r = app
        .post_form("/admin/webhooks", Some(&admin), Some("null"), &form)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "an opaque origin");

    assert_eq!(state(&app, &id).await, before, "nothing changed");
    assert_eq!(webhook_count(&app).await, 1);
    assert_eq!(audit().await, audit_before);
    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(deliveries, 1);

    // the list names the host only: the path may hold a token
    let page = app.get_page("/admin/webhooks", Some(&admin)).await.text();
    assert!(page.contains("erp.example.org"));
    assert!(!page.contains("t0p"), "{page}");
    // an unknown webhook
    let r = app
        .get_page(
            "/admin/webhooks/00000000-0000-0000-0000-000000000000",
            Some(&admin),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.teardown().await;
}

#[tokio::test]
async fn the_form_is_validated_and_internal_targets_refused() {
    let Some(app) = common::setup().await else {
        return;
    };
    app.user("ada", Role::Admin).await;
    let admin = app.login("ada").await;
    let good = [
        ("name", "ERP"),
        ("url", "https://erp.example.org/events"),
        ("event", "change.released"),
    ];
    let with = |key: &'static str, value: &'static str| {
        let mut f: Vec<(&str, &str)> = good.iter().filter(|(k, _)| *k != key).copied().collect();
        if !value.is_empty() || key == "name" {
            f.push((key, value));
        }
        f
    };
    let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (with("name", ""), "name"),
        (with("url", "ftp://erp.example.org/x"), "http"),
        (with("url", "not a url"), "URL"),
        (
            with("url", "http://127.0.0.1:8080/x"),
            "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
        ),
        // (a host name such as localhost passes here: it is checked when the
        // connection is made, see tests/webhook_ssrf.rs)
        (
            with("url", "https://ci:hunter2@erp.example.org/x"),
            "passwords in the URL are not supported",
        ),
        (
            with("url", "http://10.1.2.3/x"),
            "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
        ),
        (
            with("url", "http://169.254.169.254/latest/meta-data"),
            "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
        ),
        (
            with("url", "http://[::1]/x"),
            "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
        ),
        (with("event", ""), "event"),
        (with("event", "item.deleted"), "item.deleted"),
        (
            {
                let mut f = good.to_vec();
                f.push(("project", "not-a-uuid"));
                f
            },
            "Unknown project group",
        ),
        (
            {
                let mut f = good.to_vec();
                f.push(("project", "00000000-0000-0000-0000-000000000001"));
                f
            },
            "",
        ),
    ];
    for (form, message) in &cases {
        let r = app
            .post_form("/admin/webhooks", Some(&admin), ORIGIN, form)
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{form:?} was accepted");
        let (location, page) = flash(&r);
        assert_eq!(location, "/admin/webhooks", "{form:?}");
        let error = flash_error(&page);
        assert!(!error.is_empty(), "{form:?}: no error shown");
        assert!(
            error.to_lowercase().contains(&message.to_lowercase()),
            "{form:?}: '{error}' lacks '{message}'"
        );
    }
    assert_eq!(webhook_count(&app).await, 0, "nothing stored");

    // the same rules on an existing webhook: refused, and it stays as it was
    let id = create(&app, &admin, "ERP", "https://erp.example.org/events").await;
    let before = state(&app, &id).await;
    for (form, message) in cases.iter().take(9) {
        let r = app
            .post_form(&format!("/admin/webhooks/{id}"), Some(&admin), ORIGIN, form)
            .await;
        let (location, page) = flash(&r);
        assert_eq!(location, format!("/admin/webhooks/{id}"));
        let error = flash_error(&page);
        assert!(
            !error.is_empty() && error.to_lowercase().contains(&message.to_lowercase()),
            "{form:?}: '{error}'"
        );
    }
    assert_eq!(state(&app, &id).await, before);

    // a valid change: saved, limited to a group, inactive without the box
    let rocket = app.project_id("ROCKET").await.to_string();
    let r = app
        .post_form(
            &format!("/admin/webhooks/{id}"),
            Some(&admin),
            ORIGIN,
            &[
                ("name", "ERP rocket"),
                ("url", "https://erp.example.org/rocket"),
                ("event", "revision.released"),
                ("event", "change.released"),
                ("project", &rocket),
            ],
        )
        .await;
    let (_, page) = flash(&r);
    assert!(page.contains("Saved webhook ERP rocket"), "{page}");
    let (name, url, events, active, _) = state(&app, &id).await;
    assert_eq!(name, "ERP rocket");
    assert_eq!(url, "https://erp.example.org/rocket");
    assert_eq!(events, ["revision.released", "change.released"]);
    assert!(!active);
    let project: Option<String> =
        sqlx::query_scalar("SELECT project_id::text FROM webhooks WHERE id = $1::text::uuid")
            .bind(&id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(project.as_deref(), Some(rocket.as_str()));
    app.teardown().await;
}

#[tokio::test]
async fn test_delivery_retry_rotation_and_deletion_from_the_page() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE", "true")]).await else {
        return;
    };
    app.user("ada", Role::Admin).await;
    let admin = app.login("ada").await;
    let (rx, url) = receiver().await;
    let id = create(&app, &admin, "CI", &url).await;
    let page = format!("/admin/webhooks/{id}");

    // a test delivery that arrives
    let r = app
        .post_form(&format!("{page}/test"), Some(&admin), ORIGIN, &[])
        .await;
    let (location, html) = flash(&r);
    assert_eq!(location, page);
    assert!(
        html.contains("Test delivery delivered (HTTP 200)"),
        "{html}"
    );
    assert_eq!(*rx.hits.lock().unwrap(), 1);
    let sent: String =
        sqlx::query_scalar("SELECT id::text FROM webhook_deliveries WHERE status = 'sent'")
            .fetch_one(&app.pool)
            .await
            .unwrap();

    // one that fails is kept for an automatic retry
    rx.status.store(503, Ordering::SeqCst);
    let r = app
        .post_form(&format!("{page}/test"), Some(&admin), ORIGIN, &[])
        .await;
    let (_, html) = flash(&r);
    assert!(html.contains("Test delivery failed"), "{html}");
    assert!(html.contains("It is retried automatically."), "{html}");
    let failed: String =
        sqlx::query_scalar("SELECT id::text FROM webhook_deliveries WHERE status = 'pending'")
            .fetch_one(&app.pool)
            .await
            .unwrap();

    // retried by hand once the receiver is back
    rx.status.store(200, Ordering::SeqCst);
    let r = app
        .post_form(
            &format!("{page}/deliveries/{failed}/retry"),
            Some(&admin),
            ORIGIN,
            &[],
        )
        .await;
    let (_, html) = flash(&r);
    assert!(html.contains("Delivery delivered (HTTP 200)"), "{html}");
    assert_eq!(*rx.hits.lock().unwrap(), 3);

    // a sent delivery is not sent again (409)
    let r = app
        .post_form(
            &format!("{page}/deliveries/{sent}/retry"),
            Some(&admin),
            ORIGIN,
            &[],
        )
        .await;
    let (_, html) = flash(&r);
    assert!(html.contains("already sent successfully"), "{html}");
    assert_eq!(*rx.hits.lock().unwrap(), 3);

    // nor anything of an inactive webhook
    sqlx::query("UPDATE webhook_deliveries SET status = 'failed' WHERE id = $1::text::uuid")
        .bind(&failed)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE webhooks SET active = FALSE")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .post_form(
            &format!("{page}/deliveries/{failed}/retry"),
            Some(&admin),
            ORIGIN,
            &[],
        )
        .await;
    let (_, html) = flash(&r);
    assert!(html.contains("is inactive"), "{html}");
    assert_eq!(*rx.hits.lock().unwrap(), 3);

    // a new secret is shown once, on the page itself
    let (_, _, _, _, old_secret) = state(&app, &id).await;
    let r = app
        .post_form(&format!("{page}/rotate-secret"), Some(&admin), ORIGIN, &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (_, _, _, _, new_secret) = state(&app, &id).await;
    assert_ne!(old_secret, new_secret);
    assert!(r.text().contains(&new_secret), "shown after rotating");
    let later = app.get_page(&page, Some(&admin)).await.text();
    assert!(!later.contains(&new_secret), "and never again");
    assert!(
        later.contains(&new_secret[new_secret.len() - 4..]),
        "the hint"
    );

    // deleted: back to the list, with the deliveries
    let r = app
        .post_form(&format!("{page}/delete"), Some(&admin), ORIGIN, &[])
        .await;
    let (location, html) = flash(&r);
    assert_eq!(location, "/admin/webhooks");
    assert!(html.contains("Deleted webhook CI"), "{html}");
    assert_eq!(webhook_count(&app).await, 0);
    // the page of a deleted webhook is gone
    let r = app.get_page(&page, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(&format!("{page}/test"), Some(&admin), ORIGIN, &[])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    app.teardown().await;
}
