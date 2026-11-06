//! Security fixes from the threat model (`docs/src/dev/threat-model.md`):
//! cross-site requests (F4).

mod common;

use axum::http::StatusCode;
use teamcraft::domain::Role;

const SAME_ORIGIN: Option<&str> = Some("http://localhost:8080");

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

async fn tokens(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// F4: a state-changing request with a session cookie but neither `Origin`
/// nor `Referer` is refused; one without a session cookie (sign-in) is not.
#[tokio::test]
async fn cookie_requests_without_origin_or_referer_are_refused() {
    let app = app_or_skip!();
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

    // without a session cookie the request acts for nobody: sign-in works
    // without the headers (scripts, privacy tools)
    let r = app
        .post_form(
            "/login",
            None,
            None,
            &[("username", "erika"), ("password", common::PASSWORD)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    app.teardown().await;
}
