//! The own password change – `POST /api/v1/me/password` (desktop app) and the
//! web form `/account/password`: the current password is required, the new
//! one must pass the rules, wrong current passwords count like failed
//! sign-ins (429 with `Retry-After`, audited), and the web sessions end while
//! tokens stay.

mod common;

use axum::http::{header, Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

const ORIGIN: &str = "http://localhost:8080";

fn body(current: &str, new: &str) -> Option<serde_json::Value> {
    Some(json!({ "current_password": current, "new_password": new }))
}

async fn audited(app: &common::TestApp, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(&app.pool)
        .await
        .unwrap()
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

/// The web form `/account/password` with the given current password.
async fn change_on_web(
    app: &common::TestApp,
    cookie: &str,
    current: &str,
    new: &str,
) -> common::Response {
    app.post_form(
        "/account/password",
        Some(cookie),
        Some(ORIGIN),
        &[
            ("current_password", current),
            ("new_password", new),
            ("confirm_password", new),
        ],
    )
    .await
}

#[tokio::test]
async fn changing_the_own_password_over_rest() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, token) = app.user("ed", Role::Editor).await;
    let cookie = app.login("ed").await;
    let me = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(me.json()["auth_source"], "local", "{}", me.text());

    // wrong current password, too short a new one
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body("nope", "a-long-new-password"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "short"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // success: 204, the old password no longer works, the token does, the session ended
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "a-long-new-password"),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        app.api(Method::GET, "/api/v1/me", &token, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_ne!(
        app.get_page("/items", Some(&cookie)).await.status,
        StatusCode::OK,
        "the web session ended"
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "another-long-password"),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "the old password is gone"
    );
    assert_eq!(audited(&app, "user.password").await, 1, "one audit entry");
    assert_eq!(
        audited(&app, "user.password_failed").await,
        2,
        "each wrong current password is audited"
    );
    let details: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'user.password_failed'")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    for d in &details {
        let text = d.to_string();
        assert!(
            !text.contains("nope") && !text.contains(common::PASSWORD),
            "no password in the audit log: {text}"
        );
    }

    // no token, no change
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            "tc_pat_invalid",
            body("x", "y"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.teardown().await;
}

/// F1 (rest): wrong current passwords count on the per-username sign-in
/// counter – over REST and in the web form – and a full counter refuses even
/// the right password (429 with `Retry-After`, a flash message on the web);
/// a right current password resets the counter.
#[tokio::test]
async fn wrong_current_passwords_are_throttled() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_SIGNIN_MAX_FAILURES", "2")]).await else {
        return;
    };
    let (_, token) = app.user("ed", Role::Editor).await;
    let cookie = app.login("ed").await;
    let post = |current: &'static str, new: &'static str| {
        app.api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(current, new),
        )
    };

    // one wrong, then the right current password (with a too short new
    // one): the counter starts again, so two more wrong ones are still 400
    assert_eq!(
        post("wrong-1", "a-long-new-password").await.status,
        StatusCode::BAD_REQUEST
    );
    let r = post(common::PASSWORD, "short").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    for guess in ["wrong-2", "wrong-3"] {
        let r = post(guess, "a-long-new-password").await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{guess}: {}", r.text());
    }

    // the counter is full: even the right password is refused, unchecked
    let r = post(common::PASSWORD, "a-long-new-password").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "too_many_requests");
    assert!(
        r.text().contains("Too many failed attempts"),
        "{}",
        r.text()
    );
    let wait: u64 = r.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=15 * 60).contains(&wait), "{wait}");
    assert_eq!(audited(&app, "user.password").await, 0, "nothing changed");
    assert_eq!(
        audited(&app, "user.password_failed").await,
        3,
        "refused attempts are not audited (no flood)"
    );

    // the web form of the same user is blocked too (a flash, not an error page)
    let r = change_on_web(&app, &cookie, common::PASSWORD, "a-long-new-password").await;
    let (to, message) = flash(&r);
    assert_eq!(to, "/account");
    assert!(
        message.starts_with("error: Too many failed attempts"),
        "{message}"
    );
    // the counter is the sign-in's: the web sign-in waits as well
    let r = app
        .post_form(
            "/login",
            None,
            Some(ORIGIN),
            &[("username", "ed"), ("password", common::PASSWORD)],
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);

    // the web form counts wrong current passwords as well
    let (_, max_token) = app.user("max", Role::Editor).await;
    let max = app.login("max").await;
    for _ in 0..2 {
        let (_, message) = flash(&change_on_web(&app, &max, "wrong", "a-long-new-password").await);
        assert_eq!(message, "error: The current password is wrong.");
    }
    let (_, message) =
        flash(&change_on_web(&app, &max, common::PASSWORD, "a-long-new-password").await);
    assert!(
        message.starts_with("error: Too many failed attempts"),
        "{message}"
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &max_token,
            body(common::PASSWORD, "a-long-new-password"),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "one counter");
    assert_eq!(audited(&app, "user.password_failed").await, 5);
    app.teardown().await;
}

/// A single-sign-on account has no password: a clear message, not "wrong",
/// and no count towards the throttle.
#[tokio::test]
async fn single_sign_on_accounts_have_no_password_to_change() {
    use teamcraft::services::users::{self, OidcIdentity};
    let Some(app) = common::setup_with(&[("TEAMCRAFT_SIGNIN_MAX_FAILURES", "2")]).await else {
        return;
    };
    users::upsert_oidc_user(
        &app.pool,
        OidcIdentity {
            subject: "idp-uid-7".into(),
            username_hint: "sso.user".into(),
            display_name: "SSO User".into(),
            email: None,
            role: Role::Editor,
            groups: Vec::new(),
        },
    )
    .await
    .expect("provision");
    let actor = app.actor("sso.user").await;
    let (_, token) = users::create_token(&app.pool, &actor, "desktop", None)
        .await
        .expect("token");
    let me = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(me.json()["auth_source"], "oidc", "{}", me.text());
    for _ in 0..3 {
        let r = app
            .api(
                Method::POST,
                "/api/v1/me/password",
                &token,
                body("anything", "a-long-new-password"),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        assert!(r.text().contains("single sign-on"), "{}", r.text());
    }
    assert_eq!(audited(&app, "user.password_failed").await, 0);
    app.teardown().await;
}
