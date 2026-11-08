//! Header name and subtitle: administrators rename what the top left shows;
//! the software identity (footer, API version, user agent) stays TeamCraft.

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

const ORIGIN: &str = "http://localhost:8080";

async fn save(app: &common::TestApp, cookie: &str, name: &str, subtitle: &str) -> common::Response {
    app.post_form(
        "/admin/branding/name",
        Some(cookie),
        Some(ORIGIN),
        &[("display_name", name), ("subtitle", subtitle)],
    )
    .await
}

fn flash_kind(r: &common::Response) -> String {
    use axum::http::header;
    let value = r
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix("tc_flash="))
        .map(|c| c.split(';').next().unwrap().to_string())
        .expect("a flash message");
    let json: serde_json::Value = serde_json::from_slice(&hex::decode(value).unwrap()).unwrap();
    json["kind"].as_str().unwrap().to_string()
}

async fn audit_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'branding.update'")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_header_name_is_renamed_and_reset_by_administrators() {
    let app = app_or_skip!();
    let (_, token) = app.user("root", Role::Admin).await;
    let cookie = app.login("root").await;

    // defaults: the configured instance name and "PLM"
    let page = app.get_page("/items", Some(&cookie)).await.text();
    assert!(page.contains("<span class=\"brand-name\">TeamCraft</span>"));
    assert!(page.contains("<span class=\"brand-sub\">PLM</span>"));
    let info = app
        .api(Method::GET, "/api/v1/branding", &token, None)
        .await
        .json();
    assert_eq!(info["display_name"], "TeamCraft");
    assert_eq!(info["subtitle"], "PLM");

    // rename; markup characters in the name are refused, '&' is escaped
    let r = save(&app, &cookie, "Rocket Team & Co", "PDM").await;
    assert_eq!(flash_kind(&r), "success");
    let page = app.get_page("/items", Some(&cookie)).await.text();
    assert!(page.contains("<span class=\"brand-name\">Rocket Team &amp; Co</span>"));
    assert!(page.contains("<span class=\"brand-sub\">PDM</span>"));
    assert!(
        page.contains("<title>Items · Rocket Team &amp; Co</title>"),
        "{page}"
    );
    // the software identity stays: footer with product name and version
    let footer = page.split("<footer").nth(1).expect("a footer");
    assert!(footer.contains(&format!("TeamCraft {}", env!("CARGO_PKG_VERSION"))));
    assert!(!footer.contains("Rocket Team"));
    // the sign-in page of anonymous visitors and the admin form show it too
    let admin = app.get_page("/admin/branding", Some(&cookie)).await.text();
    assert!(admin.contains("value=\"Rocket Team &amp; Co\""), "{admin}");
    let info = app
        .api(Method::GET, "/api/v1/branding", &token, None)
        .await
        .json();
    assert_eq!(info["display_name"], "Rocket Team & Co");
    assert_eq!(info["subtitle"], "PDM");
    // version info keeps the product name
    let version = app.get_page("/healthz", None).await;
    assert!(!version.text().contains("Rocket"));

    // empty subtitle: none shown
    assert_eq!(
        flash_kind(&save(&app, &cookie, "ACME", "").await),
        "success"
    );
    let page = app.get_page("/items", Some(&cookie)).await.text();
    assert!(page.contains("<span class=\"brand-name\">ACME</span>"));
    assert!(!page.contains("brand-sub"));
    let info = app
        .api(Method::GET, "/api/v1/branding", &token, None)
        .await
        .json();
    assert_eq!(info["subtitle"], "");

    // a wide logo wins over the text, as before
    // (checked in tests/branding.rs); here: empty name = default again
    assert_eq!(
        flash_kind(&save(&app, &cookie, "  ", "PLM").await),
        "success"
    );
    let info = app
        .api(Method::GET, "/api/v1/branding", &token, None)
        .await
        .json();
    assert_eq!(info["display_name"], "TeamCraft");
    assert_eq!(info["subtitle"], "PLM");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branding_text")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    // every successful save is audited
    assert_eq!(audit_count(&app).await, 3);
    app.teardown().await;
}

#[tokio::test]
async fn the_instance_name_variable_is_only_the_default() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_INSTANCE_NAME", "Env Name")]).await else {
        return;
    };
    let (_, token) = app.user("root", Role::Admin).await;
    let cookie = app.login("root").await;
    let info = app
        .api(Method::GET, "/api/v1/branding", &token, None)
        .await
        .json();
    assert_eq!(info["display_name"], "Env Name");
    assert!(app
        .get_page("/items", Some(&cookie))
        .await
        .text()
        .contains("<span class=\"brand-name\">Env Name</span>"));
    assert_eq!(
        flash_kind(&save(&app, &cookie, "Custom", "PLM").await),
        "success"
    );
    assert!(app
        .get_page("/items", Some(&cookie))
        .await
        .text()
        .contains("<span class=\"brand-name\">Custom</span>"));
    assert_eq!(flash_kind(&save(&app, &cookie, "", "PLM").await), "success");
    assert!(app
        .get_page("/items", Some(&cookie))
        .await
        .text()
        .contains("<span class=\"brand-name\">Env Name</span>"));
    app.teardown().await;
}

#[tokio::test]
async fn invalid_names_and_other_users_are_refused() {
    let app = app_or_skip!();
    let (_, token) = app.user("root", Role::Admin).await;
    let (_, editor_token) = app.user("ed", Role::Editor).await;
    let cookie = app.login("root").await;
    let editor = app.login("ed").await;

    let long_name = "x".repeat(41);
    let long_sub = "y".repeat(21);
    for (what, name, sub) in [
        ("too long name", long_name.as_str(), "PLM"),
        ("too long subtitle", "ACME", long_sub.as_str()),
        ("markup", "<b>ACME</b>", "PLM"),
        ("markup in subtitle", "ACME", "<i>"),
        ("line break", "ACME\nPDM", "PLM"),
        ("control character", "ACME\u{7}", "PLM"),
    ] {
        let r = save(&app, &cookie, name, sub).await;
        assert_eq!(flash_kind(&r), "error", "{what}");
    }
    // exactly at the limits is fine
    assert_eq!(
        flash_kind(&save(&app, &cookie, &"x".repeat(40), &"y".repeat(20)).await),
        "success"
    );
    assert_eq!(audit_count(&app).await, 1);

    // not an administrator: web and API
    let before = audit_count(&app).await;
    let r = save(&app, &editor, "Hacked", "X").await;
    assert!(r.status == StatusCode::FORBIDDEN || flash_kind(&r) == "error");
    let r = app
        .api(
            Method::PUT,
            "/api/v1/branding/name",
            &editor_token,
            Some(json!({ "display_name": "Hacked", "subtitle": "X" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // anonymous
    let r = app
        .send(
            axum::http::Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/branding/name")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(audit_count(&app).await, before);
    let info = app
        .api(Method::GET, "/api/v1/branding", &editor_token, None)
        .await
        .json();
    assert_ne!(info["display_name"], "Hacked");

    // invalid API input: 400 and nothing stored
    let r = app
        .api(
            Method::PUT,
            "/api/v1/branding/name",
            &token,
            Some(json!({ "display_name": "<script>", "subtitle": "" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // valid API change: the answer is the new branding, audited
    let r = app
        .api(
            Method::PUT,
            "/api/v1/branding/name",
            &token,
            Some(json!({ "display_name": "API Name", "subtitle": "" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["display_name"], "API Name");
    assert_eq!(r.json()["subtitle"], "");
    assert_eq!(audit_count(&app).await, before + 1);

    // cross-site form posts are blocked
    let r = app
        .post_form(
            "/admin/branding/name",
            Some(&cookie),
            Some("https://evil.example"),
            &[("display_name", "Evil")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

#[test]
fn the_software_identity_never_uses_the_instance_name() {
    // Guard: footer, About, settings, user agent and server info stay literal.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let base = std::fs::read_to_string(root.join("templates/base.html")).unwrap();
    let footer = base.split("<footer").nth(1).expect("footer in base.html");
    let footer = footer.split("</footer>").next().unwrap();
    assert!(footer.contains("TeamCraft {{ version }}"));
    assert!(!footer.contains("instance_name") && !footer.contains("display_name"));
    for file in [
        "desktop/ui/js/settings.js",
        "desktop/ui/index.html",
        "src/mcp/tools.rs",
    ] {
        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        assert!(!text.contains("instance_name"), "{file}");
    }
    // About and settings texts in the desktop UI do not read the branding
    let js = std::fs::read_to_string(root.join("desktop/ui/js/settings.js")).unwrap();
    assert!(!js.contains("brandTexts") && !js.contains("/branding"));
    // the title bar is the one place that reads it
    let shell = std::fs::read_to_string(root.join("desktop/ui/js/shell.js")).unwrap();
    assert!(shell.contains("brandTexts(b)"));
}
