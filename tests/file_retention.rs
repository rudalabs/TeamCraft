//! Retention of old file versions: the per-file setting (`all` / `latest`),
//! the removal of superseded versions after a grace period by
//! `retention::purge_superseded_versions`, and what it never touches (the
//! newest version, files that keep everything, revisions that are not in
//! work, content other files still use).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{domain::Role, services::retention};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const REV: &str = "/api/v1/items/PRT-000001/revisions/A";

async fn part(app: &common::TestApp, token: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": "Rotor", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn checkout(app: &common::TestApp, token: &str) {
    // already checked out by the same user is fine too
    app.api(Method::POST, &format!("{REV}/checkout"), token, None)
        .await;
}

/// Uploads `content` as the next version of `name` (check out, check in).
async fn version(app: &common::TestApp, token: &str, name: &str, content: &[u8]) {
    checkout(app, token).await;
    let r = app
        .api_multipart(&format!("{REV}/checkin"), token, &[(name, content)], &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn file_id(app: &common::TestApp, token: &str, name: &str) -> String {
    let r = app
        .api(Method::GET, &format!("{REV}/files"), token, None)
        .await;
    r.json()
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["filename"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn versions_of(app: &common::TestApp, id: &str) -> Vec<i32> {
    sqlx::query_scalar("SELECT version FROM file_versions WHERE file_id = $1::uuid ORDER BY 1")
        .bind(id)
        .fetch_all(&app.pool)
        .await
        .unwrap()
}

/// Pretends every version was uploaded `days` days ago.
async fn age(app: &common::TestApp, days: i32) {
    sqlx::query("UPDATE file_versions SET uploaded_at = uploaded_at - make_interval(days => $1)")
        .bind(days)
        .execute(&app.pool)
        .await
        .unwrap();
}

async fn set_retention(
    app: &common::TestApp,
    token: &str,
    id: &str,
    value: &str,
) -> common::Response {
    app.api(
        Method::PATCH,
        &format!("/api/v1/files/{id}"),
        token,
        Some(json!({ "retention": value })),
    )
    .await
}

#[tokio::test]
async fn the_setting_needs_a_checkout_and_is_audited() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    part(&app, &max).await;
    let r = app
        .api_multipart(&format!("{REV}/files"), &max, &[("run.csv", b"1")], &[])
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = file_id(&app, &max, "run.csv").await;
    checkout(&app, &max).await;
    let r = app
        .api(Method::GET, &format!("/api/v1/files/{id}"), &max, None)
        .await;
    assert_eq!(
        r.json()["retention"],
        "all",
        "every version is kept by default"
    );

    assert_eq!(
        set_retention(&app, &max, &id, "sometimes").await.status,
        StatusCode::BAD_REQUEST
    );
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/files/{id}"),
            &max,
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        !set_retention(&app, &viewer, &id, "latest")
            .await
            .status
            .is_success(),
        "a viewer cannot change it"
    );
    let r = set_retention(&app, &max, &id, "latest").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["retention"], "latest");
    // setting it again records nothing
    set_retention(&app, &max, &id, "latest").await;

    // checked in: the revision is no longer checked out by Max
    let r = app
        .api_multipart(&format!("{REV}/checkin"), &max, &[("run.csv", b"2")], &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        set_retention(&app, &max, &id, "all").await.status,
        StatusCode::CONFLICT
    );
    let rev: uuid::Uuid = sqlx::query_scalar("SELECT revision_id FROM files WHERE id = $1::uuid")
        .bind(&id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let n = app
        .audit_actions(rev)
        .await
        .iter()
        .filter(|a| *a == "file.retention")
        .count();
    assert_eq!(n, 1);
    app.teardown().await;
}

#[tokio::test]
async fn superseded_versions_go_after_the_grace_period() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    part(&app, &max).await;
    let r = app
        .api_multipart(
            &format!("{REV}/files"),
            &max,
            &[("run.csv", b"result one"), ("keep.csv", b"keep one")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let run = file_id(&app, &max, "run.csv").await;
    let keep = file_id(&app, &max, "keep.csv").await;
    checkout(&app, &max).await;
    assert_eq!(
        set_retention(&app, &max, &run, "latest").await.status,
        StatusCode::OK
    );
    version(&app, &max, "run.csv", b"result two").await;
    version(&app, &max, "run.csv", b"result three").await;
    version(&app, &max, "keep.csv", b"keep two").await;
    let vault = app.vault().await;
    let sha = |content: &[u8]| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(content))
    };
    assert_eq!(versions_of(&app, &run).await, [1, 2, 3]);

    // Nothing is old enough yet.
    let none = retention::purge_superseded_versions(&app.pool, &vault, 7, true)
        .await
        .unwrap();
    assert!(none.files.is_empty());
    // A grace period of 0 days is refused.
    assert!(
        retention::purge_superseded_versions(&app.pool, &vault, 0, true)
            .await
            .is_err()
    );

    age(&app, 10).await;
    // A dry run reports and changes nothing.
    let dry = retention::purge_superseded_versions(&app.pool, &vault, 7, false)
        .await
        .unwrap();
    assert_eq!(dry.files.len(), 1);
    assert_eq!(dry.files[0].filename, "run.csv");
    assert_eq!(dry.files[0].versions, [1, 2]);
    assert_eq!(versions_of(&app, &run).await, [1, 2, 3]);

    let done = retention::purge_superseded_versions(&app.pool, &vault, 7, true)
        .await
        .unwrap();
    assert_eq!(done.versions(), 2);
    assert_eq!(versions_of(&app, &run).await, [3]);
    // the file that keeps everything is untouched
    assert_eq!(versions_of(&app, &keep).await, [1, 2]);
    assert!(vault.exists(&sha(b"keep one")).await);
    assert!(vault.exists(&sha(b"keep two")).await);
    // the newest version stays; the removed content went with the versions
    // (the vault keeps content younger than an hour, so only the rows are
    // asserted for it) and the file still downloads
    assert!(vault.exists(&sha(b"result three")).await);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{run}/content"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, b"result three");
    // an old version is gone for good
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/files/{run}/restore"),
            &max,
            Some(json!({ "version": 1 })),
        )
        .await;
    assert!(!r.status.is_success());

    // audited, with the versions named
    let rev: uuid::Uuid = sqlx::query_scalar("SELECT revision_id FROM files WHERE id = $1::uuid")
        .bind(&run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(app
        .audit_actions(rev)
        .await
        .contains(&"file.purge_versions".to_string()));
    let details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'file.purge_versions' AND entity_id = $1",
    )
    .bind(rev)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(details["versions"], json!([1, 2]));
    assert_eq!(details["kept_version"], 3);

    // a second run finds nothing
    let again = retention::purge_superseded_versions(&app.pool, &vault, 7, true)
        .await
        .unwrap();
    assert!(again.files.is_empty());
    app.teardown().await;
}

#[tokio::test]
async fn released_revisions_are_never_purged() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["ROVER"]).await;
    part(&app, &max).await;
    let r = app
        .api_multipart(&format!("{REV}/files"), &max, &[("run.csv", b"one")], &[])
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = file_id(&app, &max, "run.csv").await;
    checkout(&app, &max).await;
    assert_eq!(
        set_retention(&app, &max, &id, "latest").await.status,
        StatusCode::OK
    );
    version(&app, &max, "run.csv", b"two").await;
    version(&app, &max, "run.csv", b"three").await;
    app.release(&max, ("rolf", &reviewer), &["PRT-000001"])
        .await;
    age(&app, 30).await;
    let vault = app.vault().await;
    let report = retention::purge_superseded_versions(&app.pool, &vault, 1, true)
        .await
        .unwrap();
    assert!(report.files.is_empty(), "{:?}", report.files);
    assert_eq!(versions_of(&app, &id).await, [1, 2, 3]);
    app.teardown().await;
}

#[tokio::test]
async fn shared_content_stays_in_the_vault() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    part(&app, &max).await;
    let r = app
        .api_multipart(
            &format!("{REV}/files"),
            &max,
            &[
                ("run.csv", b"shared content"),
                ("copy.csv", b"shared content"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = file_id(&app, &max, "run.csv").await;
    checkout(&app, &max).await;
    assert_eq!(
        set_retention(&app, &max, &id, "latest").await.status,
        StatusCode::OK
    );
    version(&app, &max, "run.csv", b"other content").await;
    age(&app, 10).await;
    let vault = app.vault().await;
    let report = retention::purge_superseded_versions(&app.pool, &vault, 7, true)
        .await
        .unwrap();
    assert_eq!(report.versions(), 1);
    assert_eq!(report.blobs_removed, 0, "copy.csv still uses the content");
    let r = app
        .api(
            Method::GET,
            &format!(
                "/api/v1/files/{}/content",
                file_id(&app, &max, "copy.csv").await
            ),
            &max,
            None,
        )
        .await;
    assert_eq!(r.body, b"shared content");
    app.teardown().await;
}
