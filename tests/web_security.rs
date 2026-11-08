//! Web hardening pinned by tests (threat model F4, F5, F10, F3, F1): a form post
//! needs `Origin` or `Referer`, HSTS follows the public URL, downloads are
//! attachments with `nosniff` and the CSP, the security headers of every
//! answer, sign-in events in the audit log, and the sign-in throttle.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    ($extra:expr) => {
        match common::setup_with($extra).await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn a_form_post_without_origin_and_referer_is_refused() {
    let app = app_or_skip!(&[]);
    let r = app
        .post_form(
            "/login",
            None,
            None,
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .post_form(
            "/login",
            None,
            Some("https://evil.example"),
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .post_form(
            "/login",
            None,
            Some("http://localhost:8080"),
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_ne!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn hsts_follows_the_public_url() {
    let app = app_or_skip!(&[("TEAMCRAFT_PUBLIC_URL", "https://plm.example.org")]);
    let r = app.get_page("/healthz", None).await;
    assert!(r
        .header("strict-transport-security")
        .is_some_and(|v| v.starts_with("max-age=")));
    app.teardown().await;

    let app = app_or_skip!(&[]);
    let r = app.get_page("/healthz", None).await;
    assert_eq!(r.header("strict-transport-security"), None);
}

#[tokio::test]
async fn downloads_are_attachments_with_nosniff_and_csp() {
    let app = app_or_skip!(&[]);
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Frame", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            &token,
            "evil.html",
            b"<script>alert(1)</script>",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()[0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{id}/content"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r
        .header("content-disposition")
        .is_some_and(|v| v.starts_with("attachment")));
    assert_eq!(
        r.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert!(r.header("content-security-policy").is_some());
}

// ---------------------------------------------------------------------------
// More cases of F4 and F5; F3 and F1
// ---------------------------------------------------------------------------

const SAME_ORIGIN: Option<&str> = Some("http://localhost:8080");

async fn tokens(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// F4 with a session: a forged state-changing request (neither `Origin` nor
/// `Referer`, or a foreign one) writes nothing and does not sign out; the
/// page's own request works.
#[tokio::test]
async fn cookie_requests_without_origin_or_referer_are_refused() {
    let app = app_or_skip!(&[]);
    app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("erika").await;
    let before = tokens(&app).await;

    // no Origin, no Referer, but the session cookie: refused, nothing written
    let r = app
        .post_form("/account/tokens", Some(&cookie), None, &[("name", "ci")])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("Cross-site request blocked"));
    assert_eq!(tokens(&app).await, before);
    // sign-out is state-changing too: the session survives the forged request
    let r = app.post_form("/logout", Some(&cookie), None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.get_page("/", Some(&cookie)).await.status,
        StatusCode::OK
    );

    // a foreign origin: refused as before
    let r = app
        .post_form(
            "/account/tokens",
            Some(&cookie),
            Some("https://evil.example"),
            &[("name", "ci")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(tokens(&app).await, before);

    // the same request from the page itself works
    let r = app
        .post_form(
            "/account/tokens",
            Some(&cookie),
            SAME_ORIGIN,
            &[("name", "ci")],
        )
        .await;
    assert!(
        r.status.is_success() || r.status.is_redirection(),
        "{}",
        r.status
    );
    assert_eq!(tokens(&app).await, before + 1);

    app.teardown().await;
}

fn header(r: &common::Response, name: &str) -> Option<String> {
    r.header(name)
}

/// F5: the security headers of every page, and HSTS exactly when the public
/// URL is https.
#[tokio::test]
async fn security_headers_and_hsts_for_https() {
    let app = app_or_skip!(&[]);
    let r = app.get_page("/login", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        header(&r, "x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert_eq!(header(&r, "x-frame-options").as_deref(), Some("DENY"));
    assert_eq!(
        header(&r, "referrer-policy").as_deref(),
        Some("same-origin")
    );
    assert_eq!(
        header(&r, "content-security-policy").as_deref(),
        Some(teamcraft::CONTENT_SECURITY_POLICY)
    );
    let csp = teamcraft::CONTENT_SECURITY_POLICY;
    for directive in [
        "default-src 'self'",
        "script-src 'self'",
        "object-src 'none'",
        "frame-ancestors 'none'",
        "form-action 'self'",
    ] {
        assert!(csp.contains(directive), "{directive} in {csp}");
    }
    assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"));
    assert!(header(&r, "permissions-policy")
        .unwrap()
        .contains("camera=()"));
    assert_eq!(
        header(&r, "strict-transport-security"),
        None,
        "no HSTS for an http public URL"
    );
    // the API answers carry them as well
    let r = app.get_page("/api/v1/items", None).await;
    assert_eq!(
        header(&r, "x-content-type-options").as_deref(),
        Some("nosniff")
    );
    app.teardown().await;

    let Some(app) =
        common::setup_with(&[("TEAMCRAFT_PUBLIC_URL", "https://plm.example.org")]).await
    else {
        return;
    };
    for uri in ["/login", "/api/v1/items", "/healthz"] {
        let r = app.get_page(uri, None).await;
        assert_eq!(
            header(&r, "strict-transport-security").as_deref(),
            Some("max-age=31536000"),
            "{uri}"
        );
    }
    app.teardown().await;
}

/// The sign-in entries of the audit log: action, user, entity, summary, details.
async fn sign_in_entries(
    app: &common::TestApp,
) -> Vec<(String, String, Option<uuid::Uuid>, String, String)> {
    sqlx::query_as(
        "SELECT action, username, entity_id, summary, details::text FROM audit_log
         WHERE action LIKE 'user.sign%' ORDER BY at, id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap()
}

/// F3: sign-in, failed sign-in and sign-out are in the audit log – never a
/// password, a session token or an unknown typed username.
#[tokio::test]
async fn sign_in_and_sign_out_are_audited() {
    let app = app_or_skip!(&[]);
    let (erika, _) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("erika").await;
    let wrong = "wrong-password-4711";
    for (username, password) in [("erika", wrong), ("Typed-Secret-0815", wrong)] {
        let r = app
            .post_form(
                "/login",
                None,
                SAME_ORIGIN,
                &[("username", username), ("password", password)],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = app
        .post_form("/logout", Some(&cookie), SAME_ORIGIN, &[])
        .await;
    assert!(r.status.is_redirection());

    let entries = sign_in_entries(&app).await;
    let actions: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(
        actions,
        [
            "user.sign_in",
            "user.sign_in_failed",
            "user.sign_in_failed",
            "user.sign_out"
        ],
        "{entries:?}"
    );
    let (_, by, entity, summary, details) = &entries[0];
    assert_eq!((by.as_str(), *entity), ("erika", Some(erika.id)));
    assert!(summary.contains("password") && details.contains("\"password\""));
    // a failed sign-in names an existing account …
    assert_eq!(entries[1].2, Some(erika.id));
    assert!(entries[1].3.contains("erika"));
    // … but keeps nothing of an unknown typed name
    assert_eq!(entries[2].2, None);
    assert_eq!(
        (entries[3].1.as_str(), entries[3].2),
        ("erika", Some(erika.id))
    );

    let everything: Vec<String> =
        sqlx::query_scalar("SELECT summary || ' ' || details::text FROM audit_log")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    for text in &everything {
        assert!(!text.contains(wrong), "no password: {text}");
        assert!(!text.contains(common::PASSWORD), "no password: {text}");
        assert!(!text.to_lowercase().contains("typed-secret"), "{text}");
        assert!(!text.contains("tc_sess_"), "no session token: {text}");
    }
    app.teardown().await;
}

/// A password sign-in from `client` (as the reverse proxy reports it).
async fn sign_in_from(
    app: &common::TestApp,
    username: &str,
    password: &str,
    client: &str,
) -> common::Response {
    use axum::{
        body::Body,
        http::{header, Method, Request},
    };
    let body = format!(
        "username={}&password={}",
        teamcraft::util::pct_encode(username),
        teamcraft::util::pct_encode(password)
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("/login")
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-forwarded-for", client)
        .body(Body::from(body))
        .unwrap();
    app.send(request).await
}

async fn audited(app: &common::TestApp, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// F1: failed sign-ins per username and per client address end in 429 with
/// `Retry-After`, without checking the password; other names and addresses
/// are not affected.
#[tokio::test]
async fn failed_sign_ins_are_throttled_per_name_and_address() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_SIGNIN_MAX_FAILURES", "2")]).await else {
        return;
    };
    app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let office = "198.51.100.20";

    // per username: two failures, then even the right password is refused
    for _ in 0..2 {
        let r = sign_in_from(&app, "erika", "wrong-password", office).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = sign_in_from(&app, "Erika", common::PASSWORD, office).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text());
    assert!(r.text().contains("Too many failed sign-ins"));
    let wait: u64 = r.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=15 * 60).contains(&wait), "{wait}");
    assert!(r
        .header("set-cookie")
        .is_none_or(|c| !c.starts_with("tc_session=")));
    assert_eq!(audited(&app, "user.sign_in").await, 0, "nobody signed in");
    // from another address the name is still blocked; another name is not
    let r = sign_in_from(&app, "erika", common::PASSWORD, "203.0.113.5").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = sign_in_from(&app, "max", common::PASSWORD, office).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());

    // per address: 2 × 5 = 10 failures, spread over names
    for i in 0..8 {
        let r = sign_in_from(&app, &format!("guess{i}"), "x", office).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "guess{i}");
    }
    let r = sign_in_from(&app, "max", common::PASSWORD, office).await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the address is full (2 + 8 failures)"
    );
    let r = sign_in_from(&app, "max", common::PASSWORD, "203.0.113.5").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "another address is not");

    // refused attempts are not audited as failed sign-ins (no flood)
    assert_eq!(audited(&app, "user.sign_in_failed").await, 10);
    app.teardown().await;
}
