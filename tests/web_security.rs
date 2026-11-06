//! Security fixes from the threat model (`docs/src/dev/threat-model.md`):
//! cross-site requests (F4), security headers (F5), sign-in audit (F3).

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

fn header(r: &common::Response, name: &str) -> Option<String> {
    r.header(name)
}

/// F5: the security headers of every page, and HSTS exactly when the public
/// URL is https.
#[tokio::test]
async fn security_headers_and_hsts_for_https() {
    let app = app_or_skip!();
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
            Some(teamcraft::STRICT_TRANSPORT_SECURITY),
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
    let app = app_or_skip!();
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
