//! Need-to-know project groups, step 2 (issue #151): conditions on
//! membership – allowed networks, identity-provider claims and a maximum
//! sign-in age. A reader who does not meet them is treated as a non-member
//! ("not found"), administrators included; a member refused is audited once
//! per session; managers cannot lock themselves out; administrators limited
//! by the policy may tighten but not loosen.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::json;
use teamcraft::{
    domain::Role,
    services::{
        membership_conditions::{self, ClaimCondition, ConditionsUpdate, Origin},
        users::{self, OidcIdentity},
    },
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

struct Fixture {
    erika: String,
    vera: String,
    item: String,
}

/// A restricted group `NTK` with manager `erika`, member `vera` and an item.
async fn group(app: &common::TestApp) -> Fixture {
    let (_, erika) = app.user("erika", Role::Editor).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let (_, vera) = app.user_in("vera", Role::Editor, &["NTK"]).await;
    let item = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Secret part", "project": "NTK" }),
        )
        .await;
    // signed in just now (the sign-in age counts from here)
    sqlx::query("UPDATE users SET last_login_at = now()")
        .execute(&app.pool)
        .await
        .unwrap();
    Fixture { erika, vera, item }
}

fn from(ip: &str) -> Origin {
    Origin {
        client: ip.parse().ok(),
        ..Default::default()
    }
}

/// GET with a token from a client address (behind the test's reverse proxy:
/// no peer, so the `X-Forwarded-For` entry is the client).
async fn get_from(app: &common::TestApp, token: &str, uri: &str, ip: Option<&str>) -> StatusCode {
    let mut b = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(ip) = ip {
        b = b.header("x-forwarded-for", ip);
    }
    app.send(b.body(Body::empty()).unwrap()).await.status
}

async fn set(
    app: &common::TestApp,
    who: &str,
    update: ConditionsUpdate,
    origin: &Origin,
) -> Result<(), teamcraft::error::AppError> {
    let actor = app.actor(who).await;
    let id = app.project_id("NTK").await;
    membership_conditions::set(&app.pool, &actor, id, update, origin)
        .await
        .map(|_| ())
}

fn networks(list: &[&str]) -> ConditionsUpdate {
    ConditionsUpdate {
        allowed_networks: Some(list.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    }
}

#[tokio::test]
async fn without_conditions_nothing_changes() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let uri = format!("/api/v1/items/{}", f.item);
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::OK);
    assert_eq!(
        get_from(&app, &f.vera, &uri, Some("198.51.100.7")).await,
        StatusCode::OK
    );
    let r = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &f.erika, None)
        .await;
    let c = &r.json()["conditions"];
    assert_eq!(c["required_claims"], json!([]));
    assert_eq!(c["allowed_networks"], json!([]));
    assert!(c["max_signin_age_hours"].is_null());
    app.teardown().await;
}

#[tokio::test]
async fn the_group_is_only_visible_from_the_allowed_networks() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let uri = format!("/api/v1/items/{}", f.item);
    set(&app, "erika", networks(&["10.20.0.0/16"]), &from("10.20.1.1"))
        .await
        .expect("set networks");

    assert_eq!(
        get_from(&app, &f.vera, &uri, Some("10.20.9.9")).await,
        StatusCode::OK
    );
    assert_eq!(
        get_from(&app, &f.vera, &uri, Some("192.0.2.5")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_from(&app, &f.vera, &uri, None).await,
        StatusCode::NOT_FOUND,
        "an unknown address is outside"
    );
    // the group list hides it too
    let mut b = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/projects")
        .header(header::AUTHORIZATION, format!("Bearer {}", f.vera));
    b = b.header("x-forwarded-for", "192.0.2.5");
    let list = app.send(b.body(Body::empty()).unwrap()).await.text();
    assert!(!list.contains("\"NTK\""), "{list}");

    // the refusal is audited once per token, not per request
    let _ = get_from(&app, &f.vera, &uri, Some("192.0.2.6")).await;
    let id = app.project_id("NTK").await;
    let refused: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'project.access_refused' AND entity_id = $1",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(refused, 1);

    // the browser session follows the same rule
    let cookie = app.login("vera").await;
    let page = |ip: &'static str| {
        let cookie = cookie.clone();
        let app = &app;
        let uri = format!("/i/{}", f.item);
        async move {
            let req = Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap();
            app.send(req).await.status
        }
    };
    assert!(page("10.20.0.1").await.is_redirection(), "inside");
    assert_eq!(page("203.0.113.1").await, StatusCode::NOT_FOUND);
    app.teardown().await;
}

#[tokio::test]
async fn administrators_are_held_to_the_conditions_too() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, root) = app.user("root", Role::Admin).await;
    sqlx::query("UPDATE users SET last_login_at = now()")
        .execute(&app.pool)
        .await
        .unwrap();
    let uri = format!("/api/v1/items/{}", f.item);
    assert_eq!(get_from(&app, &root, &uri, None).await, StatusCode::OK);
    set(&app, "erika", networks(&["10.0.0.0/8"]), &from("10.0.0.1"))
        .await
        .unwrap();
    assert_eq!(
        get_from(&app, &root, &uri, Some("192.0.2.1")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_from(&app, &root, &uri, Some("10.1.1.1")).await,
        StatusCode::OK
    );
    // but they still manage the group
    let mut b = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/projects/NTK/policy")
        .header(header::AUTHORIZATION, format!("Bearer {root}"));
    b = b.header("x-forwarded-for", "192.0.2.1");
    assert_eq!(
        app.send(b.body(Body::empty()).unwrap()).await.status,
        StatusCode::OK
    );
    app.teardown().await;
}

#[tokio::test]
async fn claims_of_the_last_single_sign_on_decide() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let _ = root;
    let uri = format!("/api/v1/items/{}", f.item);
    // erika has no claims: she cannot set a condition she would fail …
    let site = ConditionsUpdate {
        required_claims: Some(vec![ClaimCondition {
            claim: "site".into(),
            values: vec!["Main-Office".into(), "partner-workshop".into()],
        }]),
        ..Default::default()
    };
    let err = set(&app, "erika", site.clone(), &Origin::default())
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT, "{err}");
    // … an administrator who is no member may (tightening)
    set(&app, "root", site, &Origin::default()).await.unwrap();
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::NOT_FOUND);

    // vera signs in with single sign-on from the main office
    let sign_in = |site: &'static str| {
        let pool = app.pool.clone();
        async move {
            sqlx::query("UPDATE users SET oidc_subject = 'sub-vera', auth_source = 'oidc' WHERE username = 'vera'")
                .execute(&pool)
                .await
                .unwrap();
            users::upsert_oidc_user(
                &pool,
                OidcIdentity {
                    subject: "sub-vera".into(),
                    username_hint: "vera".into(),
                    display_name: "Vera".into(),
                    email: None,
                    role: Role::Editor,
                    groups: vec![],
                    claims: [
                        ("site".to_string(), vec![site.to_string()]),
                        ("email_verified".to_string(), vec!["true".to_string()]),
                    ]
                    .into_iter()
                    .collect(),
                },
            )
            .await
            .unwrap();
        }
    };
    sign_in("MAIN-OFFICE").await;
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::OK);
    // only the claims a condition names are kept
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT idp_claims FROM users WHERE username = 'vera'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(stored, json!({ "site": ["main-office"] }));
    // working from home: hidden again after the next sign-in
    sign_in("home").await;
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::NOT_FOUND);
    app.teardown().await;
}

#[tokio::test]
async fn the_group_is_hidden_after_the_maximum_sign_in_age() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let uri = format!("/api/v1/items/{}", f.item);
    set(
        &app,
        "erika",
        ConditionsUpdate {
            max_signin_age_hours: Some(Some(8)),
            ..Default::default()
        },
        &Origin::default(),
    )
    .await
    .unwrap();
    // tokens: the user's last sign-in
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::OK);
    sqlx::query("UPDATE users SET last_login_at = now() - interval '9 hours' WHERE username = 'vera'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(get_from(&app, &f.vera, &uri, None).await, StatusCode::NOT_FOUND);

    // the browser: the session's own start
    let cookie = app.login("vera").await; // a new sign-in
    let page = app.get_page(&format!("/i/{}", f.item), Some(&cookie)).await;
    assert!(page.status.is_redirection(), "{}", page.status);
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '9 hours'
         WHERE user_id = (SELECT id FROM users WHERE username = 'vera')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let page = app.get_page(&format!("/i/{}", f.item), Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    app.teardown().await;
}

#[tokio::test]
async fn managers_cannot_lock_themselves_out_and_admins_cannot_loosen() {
    let app = app_or_skip!();
    let _f = group(&app).await;
    let (_, _root) = app.user("root", Role::Admin).await;
    // erika's own address is outside: refused
    let err = set(&app, "erika", networks(&["10.0.0.0/8"]), &from("192.0.2.1"))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT, "{err}");
    // bad input
    for bad in [networks(&["10.0.0.1/8"]), networks(&["nonsense"])] {
        let err = set(&app, "erika", bad, &from("10.0.0.1")).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{err}");
    }
    set(&app, "erika", networks(&["10.0.0.0/8"]), &from("10.0.0.1"))
        .await
        .unwrap();
    // limit the administrators (through the service: erika's requests in
    // this test have no address, so they are outside the range)
    let actor = app.actor("erika").await;
    let id = app.project_id("NTK").await;
    teamcraft::services::project_policy::set_admin_read(
        &app.pool,
        &actor,
        id,
        teamcraft::domain::AdminRead::None,
    )
    .await
    .unwrap();
    // the administrator narrows the range, but cannot widen or drop it
    set(&app, "root", networks(&["10.1.0.0/16"]), &Origin::default())
        .await
        .expect("tightening");
    for looser in [networks(&["10.0.0.0/8"]), networks(&[])] {
        let err = set(&app, "root", looser, &Origin::default())
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN, "{err}");
    }
    // the manager (inside the range) widens it again
    set(&app, "erika", networks(&["10.0.0.0/8"]), &from("10.1.0.1"))
        .await
        .expect("loosening by the manager");
    let actions = app.audit_actions(id).await;
    assert!(actions.contains(&"project.conditions".to_string()));
    app.teardown().await;
}

#[tokio::test]
async fn the_api_sets_and_shows_the_conditions() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let put = |body: serde_json::Value| {
        let app = &app;
        let token = f.erika.clone();
        async move {
            let req = Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/projects/NTK/policy")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", "10.9.9.9")
                .body(Body::from(body.to_string()))
                .unwrap();
            app.send(req).await
        }
    };
    let r = put(json!({ "allowed_networks": ["10.0.0.0/8"], "max_signin_age_hours": 12 })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["conditions"]["allowed_networks"], json!(["10.0.0.0/8"]));
    assert_eq!(r.json()["conditions"]["max_signin_age_hours"], 12);
    let r = put(json!({ "max_signin_age_hours": 0 })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["conditions"]["max_signin_age_hours"].is_null());
    let r = put(json!({ "required_claims": [{ "claim": "bad name", "values": ["x"] }] })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // the access review shows whether each member met the conditions
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/projects/NTK/access-review")
        .header(header::AUTHORIZATION, format!("Bearer {}", f.erika))
        .header("x-forwarded-for", "10.9.9.9")
        .body(Body::empty())
        .unwrap();
    let r = app.send(req).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["conditions"]["allowed_networks"], json!(["10.0.0.0/8"]));
    app.teardown().await;
}
