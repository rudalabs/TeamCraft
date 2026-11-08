//! The *Files* card of a revision page: how to add files in every state
//! (free, locked by me, locked by someone else, in review, released,
//! viewer, outsider), *Check out and add files* and the drop zone.

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

const ORIGIN: Option<&str> = Some("http://localhost:8080");

async fn new_item(app: &common::TestApp, token: &str, item_type: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": "Hint test", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn files_card_shows_the_way_in_for_every_state() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("leo", Role::Editor, &["ROVER"]).await;
    app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    app.user("olli", Role::Editor).await;
    app.user("root", Role::Admin).await;
    let rev = new_item(&app, &token, "part").await;
    let leo = app.login("leo").await;
    let erika = app.login("erika").await;
    let vera = app.login("vera").await;
    let olli = app.login("olli").await;
    let root = app.login("root").await;
    let page = format!("/revisions/{rev}");

    // new items are checked out to the author: upload form, drop zone, part hint
    let html = app.get_page(&page, Some(&leo)).await.text();
    assert!(html.contains("data-dropzone") && html.contains("name=\"file\""));
    assert!(html.contains("files-type-hint") && html.contains("STEP export"));
    assert!(!html.contains("Check out and add files"));

    let r = app
        .post_form(
            &format!("/revisions/{rev}/cancel-checkout"),
            Some(&leo),
            ORIGIN,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    // free: editors get the check-out button, no upload form
    let html = app.get_page(&page, Some(&erika)).await.text();
    assert!(html.contains("To add or replace files, check out this revision."));
    assert!(html.contains("Check out and add files"));
    assert!(html.contains("%23files"), "returns to the Files card");
    assert!(!html.contains("data-dropzone"));
    // viewers and users outside the group see no add path
    for cookie in [&vera, &olli] {
        let html = app.get_page(&page, Some(cookie)).await.text();
        assert!(!html.contains("Check out and add files"), "{html}");
        assert!(!html.contains("data-dropzone"));
        assert!(!html.contains("files-hint"));
    }

    // Check out and add files: checks out and lands on #files with the form
    let r = app
        .post_form(
            &format!("/revisions/{rev}/checkout?back=%2Frevisions%2F{rev}%23files"),
            Some(&erika),
            ORIGIN,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some(format!("/revisions/{rev}#files").as_str())
    );
    let html = app.get_page(&page, Some(&erika)).await.text();
    assert!(html.contains("data-dropzone") && html.contains("data-chunked"));

    // locked by erika: leo sees the lock and no way in; admins are pointed to force-unlock
    let html = app.get_page(&page, Some(&leo)).await.text();
    assert!(html.contains("files can be added after check-in"));
    assert!(html.contains("erika (test)"));
    assert!(!html.contains("Check out and add files") && !html.contains("data-dropzone"));
    assert!(!html.contains("force-unlock it from the"));
    let html = app.get_page(&page, Some(&root)).await.text();
    assert!(html.contains("force-unlock it from the <em>More</em> menu"));
    let html = app.get_page(&page, Some(&vera)).await.text();
    assert!(html.contains("files can be added after check-in"));
    assert!(!html.contains("data-dropzone"));

    // released: frozen hint, Revise for editors only
    sqlx::query(
        "UPDATE revisions SET state = 'released', checked_out_by = NULL, checked_out_at = NULL
         WHERE id = $1::uuid",
    )
    .bind(&rev)
    .execute(&app.pool)
    .await
    .unwrap();
    let revise = format!("/revisions/{rev}/revise");
    let html = app.get_page(&page, Some(&leo)).await.text();
    assert!(html.contains("revisions are frozen – revise to add files"));
    assert!(html.contains(&revise));
    assert!(!html.contains("data-dropzone"));
    let html = app.get_page(&page, Some(&vera)).await.text();
    assert!(html.contains("revisions are frozen"));
    assert!(!html.contains(&revise), "viewers cannot revise");

    // in review
    sqlx::query("UPDATE revisions SET state = 'in_review' WHERE id = $1::uuid")
        .bind(&rev)
        .execute(&app.pool)
        .await
        .unwrap();
    let html = app.get_page(&page, Some(&leo)).await.text();
    assert!(html.contains("in review and frozen"));
    assert!(!html.contains("data-dropzone"));
    app.teardown().await;
}

#[tokio::test]
async fn type_hint_for_simulations_and_none_for_documents() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("leo", Role::Editor, &["ROVER"]).await;
    let leo = app.login("leo").await;
    let sim = new_item(&app, &token, "simulation").await;
    let html = app
        .get_page(&format!("/revisions/{sim}"), Some(&leo))
        .await
        .text();
    assert!(html.contains("Solver input, models, result files and reports"));
    let doc = new_item(&app, &token, "document").await;
    let html = app
        .get_page(&format!("/revisions/{doc}"), Some(&leo))
        .await
        .text();
    assert!(!html.contains("files-type-hint"));
    app.teardown().await;
}

#[test]
fn drop_zone_uses_the_chunked_upload_script() {
    // static/app.js: the drop handler fills the form's file input and
    // submits the form, so the chunked/resumable path of the form is used.
    let js = include_str!("../static/app.js");
    let start = js.find("form[data-dropzone]").expect("drop zone code");
    let zone = &js[start..];
    assert!(zone.contains("\"drop\"") && zone.contains("dataTransfer.files"));
    assert!(zone.contains("requestSubmit"));
    let submit = js.find("form[enctype='multipart/form-data']").unwrap();
    assert!(start < submit);
    assert!(js.contains("data-chunked"));
}
