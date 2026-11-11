//! Single sign-on (OIDC authorization code flow with PKCE) against a fake
//! identity provider in the test process, and the life of sessions and
//! personal access tokens.
//!
//! The fake provider serves discovery, a token endpoint that checks the PKCE
//! verifier and hands out each code once, and userinfo. A test asks the
//! server for the login URL (state, nonce, challenge), lets the provider issue
//! a code – optionally with a broken ID token – and calls the callback.
//!
//! Failure paths: a missing, unknown or expired `state`; the same callback
//! twice (replay); a wrong `nonce`; a PKCE verifier that does not match; an
//! expired ID token; a wrong `aud` or `iss`; an ID token without subject; an
//! error from the provider. Nothing of these signs anybody in.
//!
//! ID token signatures are not verified by design: the token comes straight
//! from the token endpoint (OpenID Connect Core 1.0, 3.1.3.7 step 6, see
//! `src/oidc.rs`); a test pins that behaviour.

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{Method, StatusCode},
    routing::{get, post},
    Form, Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use teamcraft::domain::Role;

const CLIENT_ID: &str = "teamcraft-test";

/// How the provider breaks the ID token of one code.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tweak {
    None,
    WrongNonce,
    Expired,
    OtherAudience,
    OtherIssuer,
    NoSubject,
    BadSignature,
    /// A fresh sign-in: `auth_time` = now (asked for with `max_age=0`).
    Fresh,
}

struct Grant {
    nonce: String,
    challenge: String,
    tweak: Tweak,
}

struct Provider {
    base: String,
    codes: Mutex<HashMap<String, Grant>>,
    next_code: Mutex<u32>,
}

impl Provider {
    async fn start() -> Arc<Provider> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = Arc::new(Provider {
            base,
            codes: Mutex::new(HashMap::new()),
            next_code: Mutex::new(0),
        });
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .route("/userinfo", get(userinfo))
            .with_state(provider.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        provider
    }

    /// What the provider does when the user signs in at the authorization
    /// endpoint: remember nonce and challenge for a new code.
    fn authorize(&self, nonce: &str, challenge: &str, tweak: Tweak) -> String {
        let mut n = self.next_code.lock().unwrap();
        *n += 1;
        let code = format!("code-{n}");
        self.codes.lock().unwrap().insert(
            code.clone(),
            Grant {
                nonce: nonce.to_string(),
                challenge: challenge.to_string(),
                tweak,
            },
        );
        code
    }
}

async fn discovery(State(p): State<Arc<Provider>>) -> Json<Value> {
    Json(json!({
        "issuer": p.base,
        "authorization_endpoint": format!("{}/authorize", p.base),
        "token_endpoint": format!("{}/token", p.base),
        "userinfo_endpoint": format!("{}/userinfo", p.base),
    }))
}

fn b64(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

async fn token(
    State(p): State<Arc<Provider>>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let refuse = |what: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant", "error_description": what })),
        )
    };
    if form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || form.get("client_secret").map(String::as_str) != Some("secret")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_client" })),
        );
    }
    let code = form.get("code").cloned().unwrap_or_default();
    // every code works once
    let Some(grant) = p.codes.lock().unwrap().remove(&code) else {
        return refuse("unknown or used code");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != grant.challenge {
        return refuse("PKCE verification failed");
    }
    let now = chrono::Utc::now().timestamp();
    let mut claims = json!({
        "iss": p.base, "aud": CLIENT_ID, "sub": "u-4711", "iat": now, "exp": now + 300,
        "nonce": grant.nonce, "preferred_username": "sso-erika", "email": "erika@example.org",
        "name": "Erika SSO", "groups": ["teamcraft-editors"],
    });
    match grant.tweak {
        Tweak::None | Tweak::BadSignature => {}
        Tweak::WrongNonce => claims["nonce"] = json!("not-the-nonce"),
        Tweak::Expired => claims["exp"] = json!(now - 3600),
        Tweak::OtherAudience => claims["aud"] = json!("another-app"),
        Tweak::OtherIssuer => claims["iss"] = json!("https://evil.example.org"),
        Tweak::NoSubject => claims["sub"] = json!(""),
        Tweak::Fresh => claims["auth_time"] = json!(now),
    }
    let signature = if grant.tweak == Tweak::BadSignature {
        "bm90IGEgc2lnbmF0dXJl".to_string()
    } else {
        URL_SAFE_NO_PAD.encode(b"signature-the-client-does-not-check")
    };
    let id_token = format!(
        "{}.{}.{signature}",
        b64(&json!({ "alg": "RS256", "typ": "JWT" })),
        b64(&claims)
    );
    (
        StatusCode::OK,
        Json(json!({ "id_token": id_token, "access_token": "at-1", "token_type": "Bearer" })),
    )
}

async fn userinfo() -> Json<Value> {
    Json(json!({ "sub": "u-4711", "groups": ["teamcraft-editors"] }))
}

/// A test app whose single sign-on points at `provider`.
async fn app_with(provider: &Provider) -> Option<common::TestApp> {
    app_with_secret(provider, "secret").await
}

async fn app_with_secret(
    provider: &Provider,
    client_secret: &'static str,
) -> Option<common::TestApp> {
    let issuer: &'static str = Box::leak(provider.base.clone().into_boxed_str());
    let extra: &'static [(&'static str, &'static str)] = Box::leak(
        vec![
            ("TEAMCRAFT_OIDC_ISSUER_URL", issuer),
            ("TEAMCRAFT_OIDC_CLIENT_ID", CLIENT_ID),
            ("TEAMCRAFT_OIDC_CLIENT_SECRET", client_secret),
        ]
        .into_boxed_slice(),
    );
    common::setup_with(extra).await
}

/// The parameters of the redirect to the provider.
struct Login {
    state: String,
    nonce: String,
    challenge: String,
}

async fn start_login(app: &common::TestApp) -> Login {
    let r = app.get_page("/auth/oidc/login?next=/items", None).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.text());
    let location = r.header("location").expect("redirect to the provider");
    let url = reqwest::Url::parse(&location).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["code_challenge_method"], "S256");
    Login {
        state: q["state"].clone(),
        nonce: q["nonce"].clone(),
        challenge: q["code_challenge"].clone(),
    }
}

async fn callback(app: &common::TestApp, code: &str, state: &str) -> common::Response {
    app.get_page(
        &format!(
            "/auth/oidc/callback?code={}&state={}",
            teamcraft::util::pct_encode(code),
            teamcraft::util::pct_encode(state)
        ),
        None,
    )
    .await
}

fn session_cookie(r: &common::Response) -> Option<String> {
    r.headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("tc_session=") && !v.starts_with("tc_session=;"))
        .map(|v| v.split(';').next().unwrap().to_string())
}

async fn sessions(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// A failed callback: refused with a user-facing message, nobody signed in.
async fn refused(app: &common::TestApp, r: &common::Response, status: StatusCode, needle: &str) {
    assert_eq!(r.status, status, "{}", r.text());
    assert!(r.text().contains(needle), "'{needle}' in {}", r.text());
    assert!(session_cookie(r).is_none(), "no session cookie");
    assert_eq!(sessions(app).await, 0, "no session");
}

#[tokio::test]
async fn a_good_login_signs_in_and_maps_the_groups() {
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::None);
    let r = callback(&app, &code, &login.state).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.text());
    assert_eq!(
        r.header("location").as_deref(),
        Some("/items"),
        "back to `next`"
    );
    let cookie = session_cookie(&r).expect("session cookie");
    let (role, source): (String, String) =
        sqlx::query_as("SELECT role, auth_source FROM users WHERE oidc_subject = 'u-4711'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!((role.as_str(), source.as_str()), ("editor", "oidc"));
    let page = app.get_page("/", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK, "the session works");

    // replay: the same callback again – the state was used up
    let r = callback(&app, &code, &login.state).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("expired or is invalid"));
    assert_eq!(sessions(&app).await, 1, "still the one session");
    app.teardown().await;
}

#[tokio::test]
async fn broken_state_never_signs_in() {
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };

    // missing state (and code)
    let r = app.get_page("/auth/oidc/callback?code=x", None).await;
    refused(&app, &r, StatusCode::BAD_REQUEST, "Missing code or state").await;

    // a state the server never issued
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::None);
    let r = callback(&app, &code, "forged-state").await;
    refused(&app, &r, StatusCode::BAD_REQUEST, "expired or is invalid").await;

    // the real state, but older than 15 minutes
    sqlx::query("UPDATE oidc_states SET created_at = now() - interval '16 minutes'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = callback(&app, &code, &login.state).await;
    refused(&app, &r, StatusCode::BAD_REQUEST, "expired or is invalid").await;

    // an error from the provider (e.g. the user denied access)
    let r = app
        .get_page(
            "/auth/oidc/callback?error=access_denied&error_description=Denied%20by%20policy",
            None,
        )
        .await;
    refused(&app, &r, StatusCode::FORBIDDEN, "Denied by policy").await;
    app.teardown().await;
}

#[tokio::test]
async fn broken_id_tokens_never_sign_in() {
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };
    for (tweak, needle) in [
        (Tweak::WrongNonce, "nonce mismatch"),
        (Tweak::Expired, "token expired"),
        (Tweak::OtherAudience, "issued for another application"),
        (Tweak::OtherIssuer, "unexpected token issuer"),
        (Tweak::NoSubject, "no subject"),
    ] {
        let login = start_login(&app).await;
        let code = provider.authorize(&login.nonce, &login.challenge, tweak);
        let r = callback(&app, &code, &login.state).await;
        refused(&app, &r, StatusCode::BAD_REQUEST, needle).await;
        // and the state is used up, so the same callback cannot be retried
        let again = callback(&app, &code, &login.state).await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST, "{tweak:?}");
    }
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE auth_source = 'oidc'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(users, 0, "no account was provisioned");
    app.teardown().await;
}

#[tokio::test]
async fn a_pkce_mismatch_or_a_used_code_is_refused_by_the_provider() {
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };

    // the code was issued for another challenge (an intercepted code used
    // with this browser's state): the provider refuses the verifier
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, "a-different-challenge", Tweak::None);
    let r = callback(&app, &code, &login.state).await;
    refused(&app, &r, StatusCode::BAD_REQUEST, "refused the login").await;

    // a code that was already used, with a fresh state
    let first = start_login(&app).await;
    let code = provider.authorize(&first.nonce, &first.challenge, Tweak::None);
    let r = callback(&app, &code, &first.state).await;
    assert!(r.status.is_redirection(), "{}", r.text());
    sqlx::query("DELETE FROM sessions")
        .execute(&app.pool)
        .await
        .unwrap();
    let second = start_login(&app).await;
    let r = callback(&app, &code, &second.state).await;
    refused(&app, &r, StatusCode::BAD_REQUEST, "refused the login").await;
    app.teardown().await;
}

#[tokio::test]
async fn a_misconfigured_client_stays_a_server_error() {
    // `invalid_client` is the administrator's to fix, not the user's: it
    // stays an internal error (logged and reported), and nobody signs in
    let provider = Provider::start().await;
    let Some(app) = app_with_secret(&provider, "not-the-secret").await else {
        return;
    };
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::None);
    let r = callback(&app, &code, &login.state).await;
    refused(
        &app,
        &r,
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal server error",
    )
    .await;
    app.teardown().await;
}

#[tokio::test]
async fn id_token_signatures_are_not_checked_by_design() {
    // OpenID Connect Core 1.0, 3.1.3.7 step 6: an ID token received directly
    // from the token endpoint may be trusted without checking its
    // signature; iss, aud, exp and nonce still are (see src/oidc.rs). This
    // test pins that decision – a change of it has to change the test.
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::BadSignature);
    let r = callback(&app, &code, &login.state).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.text());
    assert!(session_cookie(&r).is_some());
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Sessions and personal access tokens
// ---------------------------------------------------------------------------

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn sessions_expire_end_with_sign_out_and_with_deactivation() {
    let app = app_or_skip!();
    app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("erika").await;
    assert_eq!(
        app.get_page("/", Some(&cookie)).await.status,
        StatusCode::OK
    );

    // expired
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(
        r.status.is_redirection(),
        "an expired session signs out: {}",
        r.status
    );
    assert!(r.header("location").unwrap_or_default().contains("/login"));

    // signed out
    let cookie = app.login("erika").await;
    let r = app
        .post_form("/logout", Some(&cookie), Some("http://localhost:8080"), &[])
        .await;
    assert!(r.status.is_redirection());
    assert!(app
        .get_page("/", Some(&cookie))
        .await
        .status
        .is_redirection());

    // deactivated: the session stops working at once
    let cookie = app.login("erika").await;
    sqlx::query("UPDATE users SET active = false WHERE username = 'erika'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(app
        .get_page("/", Some(&cookie))
        .await
        .status
        .is_redirection());
    app.teardown().await;
}

#[tokio::test]
async fn personal_access_tokens_expire_are_revoked_and_die_with_the_account() {
    let app = app_or_skip!();
    let (erika, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, other) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let ok = |token: String| {
        let app = &app;
        async move {
            app.api(Method::GET, "/api/v1/items", &token, None)
                .await
                .status
        }
    };
    assert_eq!(ok(token.clone()).await, StatusCode::OK);

    // expired
    sqlx::query(
        "UPDATE api_tokens SET expires_at = now() - interval '1 second' WHERE user_id = $1",
    )
    .bind(erika.id)
    .execute(&app.pool)
    .await
    .unwrap();
    assert_eq!(ok(token.clone()).await, StatusCode::UNAUTHORIZED);

    // revoked: only by its owner (or an administrator)
    let (record, fresh) = teamcraft::services::users::create_token(
        &app.pool,
        &erika,
        "ci",
        teamcraft::services::users::TokenLifetime::Days(30),
        teamcraft::domain::TokenScope::Full,
    )
    .await
    .unwrap();
    assert_eq!(ok(fresh.clone()).await, StatusCode::OK);
    let max = app.actor("max").await;
    let err = teamcraft::services::users::revoke_token(&app.pool, &max, record.id)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND, "another user's token");
    assert_eq!(ok(fresh.clone()).await, StatusCode::OK);
    teamcraft::services::users::revoke_token(&app.pool, &erika, record.id)
        .await
        .unwrap();
    assert_eq!(ok(fresh.clone()).await, StatusCode::UNAUTHORIZED);
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'token.revoke'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // a deactivated account: every token stops working
    let (_, third) = teamcraft::services::users::create_token(
        &app.pool,
        &erika,
        "ci2",
        teamcraft::services::users::TokenLifetime::Unlimited,
        teamcraft::domain::TokenScope::Full,
    )
    .await
    .unwrap();
    assert_eq!(ok(third.clone()).await, StatusCode::OK);
    sqlx::query("UPDATE users SET active = false WHERE username = 'erika'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(ok(third).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        ok(other).await,
        StatusCode::OK,
        "other accounts are not affected"
    );

    // a made-up token is refused like an expired one
    assert_eq!(
        ok("not-a-real-token".into()).await,
        StatusCode::UNAUTHORIZED
    );
    app.teardown().await;
}

#[tokio::test]
async fn changing_the_own_password_ends_the_other_sessions() {
    // A session stolen before the change must not outlive it; the session
    // that made the change stays, and so do personal access tokens.
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let here = app.login("erika").await;
    let elsewhere = app.login("erika").await;
    assert_eq!(
        app.get_page("/", Some(&elsewhere)).await.status,
        StatusCode::OK
    );

    // a wrong current password changes nothing
    let r = app
        .post_form(
            "/account/password",
            Some(&here),
            Some("http://localhost:8080"),
            &[
                ("current_password", "not-my-password"),
                ("new_password", "a-new-long-password"),
                ("confirm_password", "a-new-long-password"),
            ],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    assert_eq!(
        app.get_page("/", Some(&elsewhere)).await.status,
        StatusCode::OK
    );

    let r = app
        .post_form(
            "/account/password",
            Some(&here),
            Some("http://localhost:8080"),
            &[
                ("current_password", common::PASSWORD),
                ("new_password", "a-new-long-password"),
                ("confirm_password", "a-new-long-password"),
            ],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    let audited: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'user.password' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audited["other_sessions_ended"], 1, "{audited}");

    let gone = app.get_page("/", Some(&elsewhere)).await;
    assert!(
        gone.status.is_redirection(),
        "the other session ended: {}",
        gone.status
    );
    assert_eq!(
        app.get_page("/", Some(&here)).await.status,
        StatusCode::OK,
        "the session that changed the password stays"
    );
    assert_eq!(
        app.api(Method::GET, "/api/v1/items", &token, None)
            .await
            .status,
        StatusCode::OK,
        "tokens are revoked one by one, not with the password"
    );
    app.teardown().await;
}

/// The parameters of a redirect to the provider for a fresh sign-in.
async fn start_reauth(app: &common::TestApp, cookie: &str, next: &str) -> Login {
    let r = app
        .get_page(&format!("/auth/oidc/reauth?next={next}"), Some(cookie))
        .await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.text());
    let location = r.header("location").expect("redirect to the provider");
    let url = reqwest::Url::parse(&location).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(
        q["prompt"], "login",
        "the provider is asked to authenticate again"
    );
    assert_eq!(q["max_age"], "0");
    Login {
        state: q["state"].clone(),
        nonce: q["nonce"].clone(),
        challenge: q["code_challenge"].clone(),
    }
}

/// With `TEAMCRAFT_APPROVAL_REAUTH` an SSO reviewer signs in again (fresh
/// `auth_time`) to approve; the existing session is marked, no new one is
/// opened, and the decision records `sso` (#152 step 4).
#[tokio::test]
async fn an_sso_reviewer_signs_in_again_to_approve() {
    let provider = Provider::start().await;
    let issuer: &'static str = Box::leak(provider.base.clone().into_boxed_str());
    let extra: &'static [(&'static str, &'static str)] = Box::leak(
        vec![
            ("TEAMCRAFT_OIDC_ISSUER_URL", issuer),
            ("TEAMCRAFT_OIDC_CLIENT_ID", CLIENT_ID),
            ("TEAMCRAFT_OIDC_CLIENT_SECRET", "secret"),
            ("TEAMCRAFT_APPROVAL_REAUTH", "release"),
        ]
        .into_boxed_slice(),
    );
    let Some(app) = common::setup_with(extra).await else {
        return;
    };
    // the SSO reviewer signs in once
    let login = start_login(&app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::None);
    let cookie = session_cookie(&callback(&app, &code, &login.state).await).expect("signed in");

    // a change in review with the SSO user as reviewer
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkin"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let number = app
        .submit_change(&erika, "Bracket", &[&part], &["sso-erika"])
        .await;
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&number)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let status = || async {
        sqlx::query_scalar::<_, String>("SELECT status FROM changes WHERE id = $1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    let review_uri = format!("/changes/{id}/review");
    let approve = || {
        app.post_form(
            &review_uri,
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("decision", "approve")],
        )
    };

    // the page offers "Sign in again", the session alone does not approve
    let page = app.get_page(&format!("/changes/{id}"), Some(&cookie)).await;
    assert!(page.text().contains("/auth/oidc/reauth"), "{}", page.text());
    approve().await;
    assert_eq!(status().await, "in_review");

    // a provider that does not confirm a fresh sign-in is refused
    let next = format!("/changes/{id}");
    let again = start_reauth(&app, &cookie, &next).await;
    let code = provider.authorize(&again.nonce, &again.challenge, Tweak::None);
    let r = callback(&app, &code, &again.state).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("fresh sign-in"), "{}", r.text());

    // a fresh sign-in marks this session and returns to the change
    let before = sessions(&app).await;
    let again = start_reauth(&app, &cookie, &next).await;
    let code = provider.authorize(&again.nonce, &again.challenge, Tweak::Fresh);
    let r = callback(&app, &code, &again.state).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.text());
    assert_eq!(r.header("location").as_deref(), Some(next.as_str()));
    assert!(session_cookie(&r).is_none(), "no new session");
    assert_eq!(sessions(&app).await, before);
    approve().await;
    assert_eq!(status().await, "released");
    let method: Option<String> =
        sqlx::query_scalar("SELECT sign_off_method FROM change_reviews WHERE change_id = $1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(method.as_deref(), Some("sso"));
    app.teardown().await;
}
