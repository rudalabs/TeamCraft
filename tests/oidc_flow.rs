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
//!
//! Offboarding driven by the identity provider (issue #173, step 9): the
//! provider signs back-channel logout tokens with an ES256 key it publishes at
//! `jwks_uri`; a forged, unsigned or broken token ends no session. With
//! `TEAMCRAFT_OIDC_TOKEN_IDLE_DAYS` the tokens of SSO users get an idle
//! lifetime, and SSO users who stopped signing in are reported to the
//! administrators.

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
use ring::signature::{EcdsaKeyPair, KeyPair};
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
    /// The key the provider signs back-channel logout tokens with; its public
    /// half is served at `jwks_uri`.
    key: EcdsaKeyPair,
}

/// The key id of the provider's signing key.
const KEY_ID: &str = "provider-key-1";

/// A new ES256 (P-256) key pair.
fn es256_key_pair() -> EcdsaKeyPair {
    let rng = ring::rand::SystemRandom::new();
    let alg = &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING;
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
    EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap()
}

/// A JWS (compact form) of `claims`, signed with `key` under [`KEY_ID`].
fn sign_es256(key: &EcdsaKeyPair, claims: &Value) -> String {
    let header = json!({ "alg": "ES256", "kid": KEY_ID, "typ": "logout+jwt" });
    let message = format!("{}.{}", b64(&header), b64(claims));
    let rng = ring::rand::SystemRandom::new();
    let signature = key.sign(&rng, message.as_bytes()).unwrap();
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
}

/// The claims of a back-channel logout token of `issuer` for `sub`.
fn logout_claims(issuer: &str, sub: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": issuer, "aud": CLIENT_ID, "iat": now, "jti": format!("logout-{sub}-{now}"),
        "sub": sub, "events": { "http://schemas.openid.net/event/backchannel-logout": {} },
    })
}

impl Provider {
    async fn start() -> Arc<Provider> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = Arc::new(Provider {
            base,
            codes: Mutex::new(HashMap::new()),
            next_code: Mutex::new(0),
            key: es256_key_pair(),
        });
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .route("/userinfo", get(userinfo))
            .route("/jwks", get(jwks))
            .with_state(provider.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        provider
    }

    /// A back-channel logout token for `sub`, signed by the provider – what
    /// it posts when the user's session ends or the user is removed there.
    fn logout_token(&self, sub: &str) -> String {
        sign_es256(&self.key, &logout_claims(&self.base, sub))
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
        "jwks_uri": format!("{}/jwks", p.base),
    }))
}

/// The provider's public signing key (RFC 7517).
async fn jwks(State(p): State<Arc<Provider>>) -> Json<Value> {
    // SEC 1 uncompressed point: 0x04 ‖ x ‖ y
    let point = p.key.public_key().as_ref().to_vec();
    Json(json!({ "keys": [{
        "kty": "EC", "crv": "P-256", "kid": KEY_ID, "alg": "ES256", "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
        "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
    }] }))
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

/// The SSO user signs in through the fake provider; returns the session cookie.
async fn sso_sign_in(app: &common::TestApp, provider: &Provider) -> String {
    let login = start_login(app).await;
    let code = provider.authorize(&login.nonce, &login.challenge, Tweak::None);
    session_cookie(&callback(app, &code, &login.state).await).expect("signed in")
}

/// What the identity provider does: a server-to-server form post without a
/// cookie or an `Origin` header.
async fn backchannel_logout(app: &common::TestApp, token: &str) -> common::Response {
    app.post_form(
        "/auth/oidc/backchannel-logout",
        None,
        None,
        &[("logout_token", token)],
    )
    .await
}

/// OpenID Connect Back-Channel Logout (#173 step 9): a token signed by the
/// provider ends every browser session of that user and nothing else; a
/// forged, unsigned or broken token is refused with 400 and ends nothing.
#[tokio::test]
async fn the_identity_provider_ends_the_sessions_over_the_back_channel() {
    let provider = Provider::start().await;
    let Some(app) = app_with(&provider).await else {
        return;
    };
    // the SSO user in two browsers, a local user in one
    let first = sso_sign_in(&app, &provider).await;
    let second = sso_sign_in(&app, &provider).await;
    app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let max = app.login("max").await;
    assert_eq!(sessions(&app).await, 3);

    // signed with another key under the provider's key id, unsigned, garbage
    let claims = logout_claims(&provider.base, "u-4711");
    let forged = sign_es256(&es256_key_pair(), &claims);
    let parts: Vec<&str> = forged.split('.').collect();
    let none = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"provider-key-1"}"#);
    let unsigned = format!("{none}.{}.", parts[1]);
    // the provider's own signature, but another issuer
    let evil = logout_claims("https://evil.example.org", "u-4711");
    let other_issuer = sign_es256(&provider.key, &evil);
    let refused = [
        forged.as_str(),
        unsigned.as_str(),
        other_issuer.as_str(),
        "not.a.token",
        "",
    ];
    for token in refused {
        let r = backchannel_logout(&app, token).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{token}: {}", r.text());
        assert_eq!(r.header("cache-control").as_deref(), Some("no-store"));
    }
    assert_eq!(sessions(&app).await, 3, "no session ended");

    // a subject TeamCraft does not know: accepted, nothing happens
    let r = backchannel_logout(&app, &provider.logout_token("u-unknown")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(sessions(&app).await, 3);

    // the provider logs the SSO user out: both sessions end, the local user stays
    let r = backchannel_logout(&app, &provider.logout_token("u-4711")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(sessions(&app).await, 1);
    for cookie in [&first, &second] {
        let page = app.get_page("/", Some(cookie)).await;
        assert!(page.status.is_redirection(), "signed out: {}", page.status);
    }
    assert_eq!(app.get_page("/", Some(&max)).await.status, StatusCode::OK);

    // noted and audited; the account itself stays active (deactivating is the
    // administrator's offboarding step)
    let (logged_out, active): (Option<chrono::DateTime<chrono::Utc>>, bool) =
        sqlx::query_as("SELECT idp_logout_at, active FROM users WHERE oidc_subject = 'u-4711'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(logged_out.is_some(), "the time of the logout is noted");
    assert!(active);
    let details: Value =
        sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'user.idp_logout'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(details["sessions"], 2, "{details}");
    app.teardown().await;
}

/// Without single sign-on the endpoint does not exist.
#[tokio::test]
async fn backchannel_logout_needs_single_sign_on() {
    let app = app_or_skip!();
    let r = backchannel_logout(&app, "a.b.c").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    app.teardown().await;
}

/// `TEAMCRAFT_OIDC_TOKEN_IDLE_DAYS` caps the tokens SSO users create, and an
/// SSO user without a sign-in for 90 days who still holds tokens shows up
/// on the administrators' *Users* page (#173 step 9).
#[tokio::test]
async fn sso_tokens_get_an_idle_lifetime_and_idle_sso_users_are_reported() {
    let provider = Provider::start().await;
    let issuer: &'static str = Box::leak(provider.base.clone().into_boxed_str());
    let extra: &'static [(&'static str, &'static str)] = Box::leak(
        vec![
            ("TEAMCRAFT_OIDC_ISSUER_URL", issuer),
            ("TEAMCRAFT_OIDC_CLIENT_ID", CLIENT_ID),
            ("TEAMCRAFT_OIDC_CLIENT_SECRET", "secret"),
            ("TEAMCRAFT_OIDC_TOKEN_IDLE_DAYS", "30"),
        ]
        .into_boxed_slice(),
    );
    let Some(app) = common::setup_with(extra).await else {
        return;
    };
    let sso = sso_sign_in(&app, &provider).await;
    app.user("root", Role::Admin).await;
    let create = |cookie: String, name: &'static str, days: &'static str| {
        let app = &app;
        async move {
            let r = app
                .post_form(
                    "/account/tokens",
                    Some(&cookie),
                    Some("http://localhost:8080"),
                    &[
                        ("name", name),
                        ("expires_days", days),
                        ("scope", "read_only"),
                    ],
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            sqlx::query_as::<_, (Option<i32>, bool)>(
                "SELECT idle_days, expires_at IS NULL FROM api_tokens WHERE name = $1",
            )
            .bind(name)
            .fetch_one(&app.pool)
            .await
            .unwrap()
        }
    };

    // an SSO user's "never expires" and "1 year" become 30 idle days,
    // a shorter fixed lifetime stays
    assert_eq!(
        create(sso.clone(), "sso-never", "never").await,
        (Some(30), false)
    );
    assert_eq!(
        create(sso.clone(), "sso-year", "365").await,
        (Some(30), false)
    );
    assert_eq!(create(sso.clone(), "sso-week", "7").await.0, None);
    let account = app.get_page("/account", Some(&sso)).await;
    assert!(
        account.text().contains("30 days without use"),
        "{}",
        account.text()
    );
    // a local account is not capped
    let root = app.login("root").await;
    assert_eq!(
        create(root.clone(), "local-never", "never").await,
        (None, true)
    );

    // nobody is idle yet
    let page = app.get_page("/admin/users", Some(&root)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.text().contains("SSO users without sign-in"));

    // 100 days later the SSO user has neither signed in nor used a token
    sqlx::query(
        "UPDATE users SET last_login_at = now() - interval '100 days',
                          created_at = now() - interval '100 days'
         WHERE oidc_subject = 'u-4711'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let page = app.get_page("/admin/users", Some(&root)).await;
    let text = page.text();
    assert!(text.contains("SSO users without sign-in"), "{text}");
    assert!(
        text.contains("sso-erika") && text.contains("/holdings\""),
        "{text}"
    );
    let admin = app.actor("root").await;
    let idle = teamcraft::services::offboarding::idle_sso_users(&app.pool, &admin)
        .await
        .unwrap();
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].username, "sso-erika");
    assert_eq!(idle[0].tokens, 3, "{:?}", idle[0]);

    // only administrators see it; a token used recently counts as activity
    let erika = app.actor("sso-erika").await;
    let err = teamcraft::services::offboarding::idle_sso_users(&app.pool, &erika)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE name = 'sso-week'")
        .execute(&app.pool)
        .await
        .unwrap();
    let idle = teamcraft::services::offboarding::idle_sso_users(&app.pool, &admin)
        .await
        .unwrap();
    assert!(idle.is_empty(), "{idle:?}");
    app.teardown().await;
}
