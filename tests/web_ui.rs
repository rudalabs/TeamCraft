//! End-to-end tests of the HTML user interface (login, pages, CSRF guard).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn health_endpoints() {
    let app = app_or_skip!();
    let r = app.get_page("/healthz", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.text(), "ok");
    let r = app.get_page("/readyz", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["database"], "ok");
    assert_eq!(r.json()["vault"], "ok");
    app.teardown().await;
}

#[tokio::test]
async fn anonymous_users_are_sent_to_login() {
    let app = app_or_skip!();
    let r = app.get_page("/items?q=x", None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some("/login?next=%2Fitems%3Fq%3Dx")
    );
    let r = app.get_page("/login", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("Sign in"));
    assert!(r.text().contains("data-skin=\"warr\""), "default skin");
    // the remembered skin also styles the login page
    let r = app.get_page("/login", Some("tc_skin=opensuse")).await;
    assert!(r.text().contains("data-skin=\"opensuse\""));
    let r = app.get_page("/login", Some("tc_skin=<script>")).await;
    assert!(
        r.text().contains("data-skin=\"warr\""),
        "unknown skins fall back"
    );
    // embedded media
    let r = app
        .get_page("/static/media/logos/TeamCraft_Icon.svg", None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("image/svg+xml"));
    let r = app
        .get_page("/static/media/icons/Green_tick.svg", None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = app.get_page("/static/media/../Cargo.toml", None).await;
    assert_ne!(r.status, StatusCode::OK);
    let r = app.get_page("/favicon.ico", None).await;
    assert_eq!(r.status, StatusCode::OK);
    // static assets and security headers
    let r = app.get_page("/static/style.css", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert!(r
        .header("content-security-policy")
        .unwrap()
        .contains("default-src 'self'"));
    app.teardown().await;
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let app = app_or_skip!();
    app.user("max", Role::Engineer).await;
    let r = app
        .post_form(
            "/login",
            None,
            None,
            &[("username", "max"), ("password", "wrong-password")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.text().contains("Invalid username or password"));
    app.teardown().await;
}

#[tokio::test]
async fn cross_site_posts_are_blocked() {
    let app = app_or_skip!();
    app.user("max", Role::Engineer).await;
    let cookie = app.login("max").await;
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            Some("https://evil.example"),
            &[("item_type", "part"), ("name", "CSRF")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.text().contains("Cross-site request blocked"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.teardown().await;
}

#[tokio::test]
async fn every_page_renders_for_an_admin() {
    let app = app_or_skip!();
    let (_, token) = app.user("root", Role::Admin).await;
    app.user("rolf", Role::Engineer).await;
    let cookie = app.login("root").await;
    let origin = Some("http://localhost:8080");

    // Create data through the forms.
    let r = app
        .post_form(
            "/projects",
            Some(&cookie),
            origin,
            &[
                ("code", "EXP"),
                ("name", "Experimental"),
                ("description", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            origin,
            &[
                ("item_type", "part"),
                ("name", "Nozzle"),
                ("mass_kg", "1,25"),
                ("unit", "each"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let rev_url = r.header("location").unwrap();
    assert!(rev_url.starts_with("/revisions/"));

    // Validation errors re-render the form
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            origin,
            &[("item_type", "part"), ("name", ""), ("mass_kg", "abc")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("is not a number") || r.text().contains("must not be empty"));

    // Flash message after redirect, then the page
    let r = app.get_page(&rev_url, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Nozzle"));
    assert!(r.text().contains("1.25 kg"));

    let rev_id = rev_url.trim_start_matches("/revisions/").to_string();
    let r = app
        .post_form(
            &format!("/revisions/{rev_id}/bom/lines"),
            Some(&cookie),
            origin,
            &[("child", "PRT-000001"), ("quantity", "1")],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "self reference redirects back with an error"
    );

    // A change via the form, then the change page
    let r = app
        .post_form(
            "/changes",
            Some(&cookie),
            origin,
            &[
                ("title", "Release nozzle"),
                ("priority", "normal"),
                ("revision_id", &rev_id),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let change_url = r.header("location").unwrap();

    // Token-based API data for the remaining pages
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "assembly", "name": "Engine" })),
        )
        .await;
    let asm_rev = r.json()["id"].as_str().unwrap().to_string();
    app.api(
        Method::POST,
        "/api/v1/items/ASM-000001/revisions/A/bom",
        &token,
        Some(json!({ "child": "PRT-000001", "quantity": 2 })),
    )
    .await;

    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'rolf'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let pages = [
        "/".to_string(),
        "/items".to_string(),
        "/items?q=nozzle&type=part&state=in_work".to_string(),
        "/items/new".to_string(),
        rev_url.clone(),
        format!("/revisions/{rev_id}/edit"),
        format!("/revisions/{asm_rev}"),
        format!("/revisions/{asm_rev}/bom"),
        format!("/revisions/{asm_rev}/bom?rule=released"),
        format!("/revisions/{asm_rev}/bom.csv"),
        "/changes".to_string(),
        "/changes?status=draft".to_string(),
        "/changes/new".to_string(),
        change_url.clone(),
        "/projects".to_string(),
        "/admin/users".to_string(),
        format!("/admin/users/{user_id}"),
        "/admin/audit".to_string(),
        "/account".to_string(),
    ];
    for page in &pages {
        let r = app.get_page(page, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::OK, "GET {page}: {}", r.text());
    }

    // Short links and 404 pages
    let r = app.get_page("/i/prt-000001", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app.get_page("/nope", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.text().contains("404"));

    // Non-admins cannot open the admin pages
    app.user("vera", Role::Viewer).await;
    let viewer_cookie = app.login("vera").await;
    let r = app.get_page("/admin/users", Some(&viewer_cookie)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Skins: switching stores the choice and remembers it in a cookie
    let r = app
        .post_form(
            "/account/skin",
            Some(&cookie),
            origin,
            &[("skin", "cachyos")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r
        .headers
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").starts_with("tc_skin=cachyos")));
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(r.text().contains("data-skin=\"cachyos\""));
    let r = app
        .post_form(
            "/account/skin",
            Some(&cookie),
            origin,
            &[("skin", "windows")],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "unknown skins redirect back with an error"
    );
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(r.text().contains("data-skin=\"cachyos\""), "unchanged");

    // Logout ends the session
    let r = app.post_form("/logout", Some(&cookie), origin, &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app.get_page("/", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    app.teardown().await;
}
