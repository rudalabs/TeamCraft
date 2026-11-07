//! Instance logos: administrators replace the wide and the square logo,
//! everybody sees them (web pages, login page, REST API for the desktop app),
//! bad files are refused, and every change is in the audit log.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v
}

/// Browser-style multipart upload with a session cookie.
async fn upload(app: &common::TestApp, cookie: &str, kind: &str, file: &[u8]) -> common::Response {
    let boundary = "logo-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"logo.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    app.send(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/admin/branding/{kind}"))
            .header(header::HOST, "localhost:8080")
            .header(header::ORIGIN, "http://localhost:8080")
            .header(header::COOKIE, cookie)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

fn flash(r: &common::Response) -> String {
    let value = r
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix("tc_flash="))
        .map(|c| c.split(';').next().unwrap().to_string())
        .expect("a flash message");
    let json: serde_json::Value = serde_json::from_slice(&hex::decode(value).unwrap()).unwrap();
    format!(
        "{}: {}",
        json["kind"].as_str().unwrap(),
        json["message"].as_str().unwrap()
    )
}

async fn audit_count(app: &common::TestApp, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn administrators_replace_and_reset_the_logos() {
    let app = app_or_skip!();
    let (_, api_admin) = app.user("root", Role::Admin).await;
    let admin = app.login("root").await;
    let logo = png(600, 120);

    // built-in logos: no image, the pages show the TeamCraft files
    assert_eq!(
        app.get_page("/branding/wide", None).await.status,
        StatusCode::NOT_FOUND
    );
    let page = app.get_page("/admin/branding", Some(&admin)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(page.text().contains("Built-in logo in use."));
    let home = app.get_page("/items", Some(&admin)).await.text();
    assert!(home.contains("TeamCraft_Icon.svg") && !home.contains("/branding/"));
    let info = app
        .api(Method::GET, "/api/v1/branding", &api_admin, None)
        .await;
    assert_eq!(info.status, StatusCode::OK);
    assert!(info.json()["wide"].is_null() && info.json()["square"].is_null());

    // upload the wide logo
    let r = upload(&app, &admin, "wide", &logo).await;
    assert!(flash(&r).starts_with("success:"), "{}", flash(&r));
    let served = app.get_page("/branding/wide", None).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.body, logo);
    assert_eq!(served.header("content-type").as_deref(), Some("image/png"));
    assert!(served
        .header("content-security-policy")
        .unwrap()
        .contains("sandbox"));
    // conditional request
    let etag = served.header("etag").expect("etag");
    let again = app
        .send(
            Request::builder()
                .uri("/branding/wide")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);

    // header, login page and the REST API (desktop) show it; the square slot is untouched
    let items = app.get_page("/items", Some(&admin)).await.text();
    assert!(items.contains("class=\"brand-wide\""), "{items}");
    assert!(app
        .get_page("/login", None)
        .await
        .text()
        .contains("/branding/wide?v="));
    let info = app
        .api(Method::GET, "/api/v1/branding", &api_admin, None)
        .await
        .json();
    assert!(info["wide"].is_string() && info["square"].is_null());
    let bytes = app
        .api(Method::GET, "/api/v1/branding/wide", &api_admin, None)
        .await;
    assert_eq!(bytes.status, StatusCode::OK);
    assert_eq!(bytes.body, logo);
    assert_eq!(
        app.api(Method::GET, "/api/v1/branding/square", &api_admin, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // the square logo replaces the footer mark and the tab icon
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#036"/></svg>"##;
    assert!(flash(&upload(&app, &admin, "square", svg).await).starts_with("success:"));
    let items = app.get_page("/items", Some(&admin)).await.text();
    assert!(items.contains("rel=\"icon\" href=\"/branding/square?v="));
    let svg_response = app.get_page("/branding/square", None).await;
    assert_eq!(
        svg_response.header("content-type").as_deref(),
        Some("image/svg+xml")
    );

    // replacing changes the version token, so browsers reload the image
    let before = app
        .api(Method::GET, "/api/v1/branding", &api_admin, None)
        .await
        .json()["wide"]
        .clone();
    assert!(flash(&upload(&app, &admin, "wide", &png(800, 160)).await).starts_with("success:"));
    let after = app
        .api(Method::GET, "/api/v1/branding", &api_admin, None)
        .await
        .json()["wide"]
        .clone();
    assert_ne!(before, after);

    // reset
    let r = app
        .post_form(
            "/admin/branding/wide/reset",
            Some(&admin),
            Some("http://localhost:8080"),
            &[],
        )
        .await;
    assert!(flash(&r).starts_with("success:"));
    assert_eq!(
        app.get_page("/branding/wide", None).await.status,
        StatusCode::NOT_FOUND
    );
    // resetting an empty slot writes no audit entry
    let r = app
        .post_form(
            "/admin/branding/wide/reset",
            Some(&admin),
            Some("http://localhost:8080"),
            &[],
        )
        .await;
    assert!(flash(&r).starts_with("success:"));
    assert_eq!(audit_count(&app, "branding.set").await, 3);
    assert_eq!(audit_count(&app, "branding.reset").await, 1);
    app.teardown().await;
}

#[tokio::test]
async fn bad_logos_and_other_users_are_refused() {
    let app = app_or_skip!();
    let (_, _) = app.user("root", Role::Admin).await;
    let (_, _) = app.user("ed", Role::Editor).await;
    let admin = app.login("root").await;
    let editor = app.login("ed").await;

    // not an administrator
    let r = upload(&app, &editor, "wide", &png(10, 10)).await;
    assert!(
        r.status == StatusCode::FORBIDDEN || flash(&r).starts_with("error:"),
        "{}",
        r.status
    );
    assert_eq!(
        app.get_page("/admin/branding", Some(&editor)).await.status,
        StatusCode::FORBIDDEN
    );
    // nothing was stored
    assert_eq!(
        app.get_page("/branding/wide", None).await.status,
        StatusCode::NOT_FOUND
    );

    // bad content
    for (what, file) in [
        ("text", b"hello".to_vec()),
        ("gif", b"GIF89a\x01\0\x01\0".to_vec()),
        (
            "script svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg"><script>1</script></svg>"#.to_vec(),
        ),
        ("huge pixels", png(5000, 10)),
        ("too big", {
            let mut v = png(10, 10);
            v.resize(600 * 1024, 0);
            v
        }),
    ] {
        let r = upload(&app, &admin, "wide", &file).await;
        assert!(
            flash(&r).starts_with("error:") || r.status == StatusCode::PAYLOAD_TOO_LARGE,
            "{what}: {} {}",
            r.status,
            r.text()
        );
    }
    // unknown slot, missing file
    let r = upload(&app, &admin, "banner", &png(10, 10)).await;
    assert!(r.status == StatusCode::NOT_FOUND || flash(&r).starts_with("error:"));
    assert_eq!(
        app.get_page("/branding/banner", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get_page("/branding/wide", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(audit_count(&app, "branding.set").await, 0);

    // cross-site form posts are blocked
    let r = app
        .post_form(
            "/admin/branding/wide/reset",
            Some(&admin),
            Some("https://evil.example"),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}
