//! Resumable downloads: `Range` / `If-Range` on the file and version content
//! endpoints of the REST API and the web UI (the desktop Get resumes a large
//! file half-way with them).

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
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

async fn get(
    app: &common::TestApp,
    uri: &str,
    token: &str,
    extra: &[(&str, &str)],
) -> common::Response {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    app.send(builder.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn downloads_resume_with_range_and_if_range() {
    let app = app_or_skip!();
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
    let content: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            &token,
            "frame.step",
            &content,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file_id = r.json()[0]["id"].as_str().unwrap().to_string();
    let sha = r.json()[0]["sha256"].as_str().unwrap().to_string();
    let etag = format!("\"{sha}\"");
    let uri = format!("/api/v1/files/{file_id}/content");

    // the whole file names the ETag and that ranges are possible
    let r = get(&app, &uri, &token, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, content);
    assert_eq!(r.header("accept-ranges").as_deref(), Some("bytes"));
    assert_eq!(r.header("etag").as_deref(), Some(etag.as_str()));

    // resume from byte 40000 of this content
    let r = get(
        &app,
        &uri,
        &token,
        &[("range", "bytes=40000-"), ("if-range", &etag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, content[40_000..]);
    assert_eq!(
        r.header("content-range").as_deref(),
        Some("bytes 40000-99999/100000")
    );
    assert_eq!(r.header("content-length").as_deref(), Some("60000"));

    // a closed range and a suffix range
    let r = get(&app, &uri, &token, &[("range", "bytes=10-19")]).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, content[10..20]);
    let r = get(&app, &uri, &token, &[("range", "bytes=-5")]).await;
    assert_eq!(r.body, content[99_995..]);

    // the content changed (another ETag): the whole file, start over
    let r = get(
        &app,
        &uri,
        &token,
        &[("range", "bytes=40000-"), ("if-range", "\"0000\"")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, content);

    // past the end
    let r = get(&app, &uri, &token, &[("range", "bytes=100000-")]).await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.header("content-range").as_deref(), Some("bytes */100000"));
    assert!(r.body.is_empty());

    // a version endpoint resumes the same way
    let versions = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/versions"),
            &token,
            None,
        )
        .await;
    assert_eq!(versions.status, StatusCode::OK, "{}", versions.text());
    let version_id = versions.json()[0]["id"].as_str().unwrap().to_string();
    let r = get(
        &app,
        &format!("/api/v1/file-versions/{version_id}/content"),
        &token,
        &[("range", "bytes=99990-"), ("if-range", &etag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, content[99_990..]);

    // the web UI's download too (browsers resume with Range)
    let cookie = app.login("erika").await;
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(format!("/files/{file_id}/download"))
        .header(header::COOKIE, cookie)
        .header("range", "bytes=99000-");
    builder = builder.header("if-range", etag.as_str());
    let r = app.send(builder.body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, content[99_000..]);
    app.teardown().await;
}
