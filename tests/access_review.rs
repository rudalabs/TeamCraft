//! Need-to-know groups, step 5 (issue #151): the access review of a
//! restricted group – the report (members, source, last sign-in, policy,
//! elevations), the dated and audited confirmation and the CSV evidence.

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

async fn restricted_group(app: &common::TestApp, token: &str, visibility: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            token,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": visibility })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn report_confirmation_and_evidence() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let (_, _outsider) = app.user("outsider", Role::Editor).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    restricted_group(&app, &erika, "restricted").await;
    sqlx::query(
        "UPDATE users SET last_login_at = now() - interval '200 days' WHERE username = 'erika'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &erika,
            Some(json!({ "admin_read": "with_reason" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // the report
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-review",
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["admin_read"], "with_reason");
    let m = &j["members"][0];
    assert_eq!(m["username"], "erika");
    assert_eq!(m["manager"], true);
    assert_eq!(m["source"], "manual");
    assert_eq!(m["stale"], true);
    assert_eq!(j["administrators_outside"], 1);
    assert!(j["reviews"].as_array().unwrap().is_empty());

    // not for non-managers; a viewer or outsider does not even see the group
    for token in [&viewer, &_outsider] {
        let s = app
            .api(
                Method::GET,
                "/api/v1/projects/NTK/access-review",
                token,
                None,
            )
            .await
            .status;
        assert!(
            s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
            "{s}"
        );
        let s = app
            .api(
                Method::POST,
                "/api/v1/projects/NTK/access-review",
                token,
                Some(json!({})),
            )
            .await
            .status;
        assert!(
            s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
            "{s}"
        );
    }

    // confirm: dated, audited, snapshot kept
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/access-review",
            &erika,
            Some(json!({ "note": "quarterly review" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let review = r.json();
    assert_eq!(review["username"], "erika");
    assert_eq!(review["member_count"], 1);
    assert!(review["reviewed_at"].is_string());
    let pid = app.project_id("NTK").await;
    assert!(app
        .audit_actions(pid)
        .await
        .contains(&"project.access_review".to_string()));
    // a later change does not alter the snapshot
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/members",
            &erika,
            Some(json!({ "username": "vera" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let id = review["id"].as_str().unwrap();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/projects/NTK/access-review/{id}"),
            &root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json().as_array().unwrap().len(), 1);

    // too long a note
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/access-review",
            &root,
            Some(json!({ "note": "x".repeat(1001) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // the evidence
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-review?format=csv",
            &root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let csv = r.text();
    assert!(csv.starts_with("record,group,username"), "{csv}");
    assert!(csv.contains("member,NTK,erika,"), "{csv}");
    assert!(csv.contains("member,NTK,vera,"), "{csv}");
    assert!(csv.contains("review,NTK,erika,"), "{csv}");
    assert!(csv.contains("quarterly review"), "{csv}");
    app.teardown().await;
}

#[tokio::test]
async fn open_groups_have_no_review_and_the_web_pages_work() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, _root) = app.user("root", Role::Admin).await;
    restricted_group(&app, &erika, "internal").await;
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-review",
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/access-review",
            &erika,
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/visibility",
            &erika,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let pid = app.project_id("NTK").await;
    let cookie = app.login("erika").await;
    let page = app
        .get_page(&format!("/projects/{pid}/access-review"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Access review: NTK"));
    let csv = app
        .get_page(&format!("/projects/{pid}/access-review.csv"), Some(&cookie))
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert!(csv.text().contains("member,NTK,erika,"));
    let r = app
        .post_form(
            &format!("/projects/{pid}/access-review"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("note", "web")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = app
        .get_page(&format!("/projects/{pid}/access-review"), Some(&cookie))
        .await;
    assert!(page.text().contains("web"));
    app.teardown().await;
}
