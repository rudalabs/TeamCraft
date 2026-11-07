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

/// Pretends the blob was stored two hours ago, past the vault's in-flight grace.
fn old_blob(vault: &teamcraft::vault::Vault, content: &[u8]) {
    let path = vault.object_path(&sha(content)).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(past)
        .unwrap();
}

fn sha(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(content))
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

    old_blob(&vault, b"result one");
    old_blob(&vault, b"result two");
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
    assert_eq!(done.blobs_removed, 2);
    assert!(!vault.exists(&sha(b"result one")).await);
    assert!(!vault.exists(&sha(b"result two")).await);
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
    old_blob(&vault, b"shared content");
    let report = retention::purge_superseded_versions(&app.pool, &vault, 7, true)
        .await
        .unwrap();
    assert_eq!(report.versions(), 1);
    assert_eq!(report.blobs_removed, 0, "copy.csv still uses the content");
    assert!(vault.exists(&sha(b"shared content")).await);
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

#[tokio::test]
async fn revise_and_save_as_keep_the_retention_setting() {
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
    app.release(&max, ("rolf", &reviewer), &["PRT-000001"])
        .await;
    let r = app
        .api(Method::POST, &format!("{REV}/revise"), &max, None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let kept: Vec<String> = sqlx::query_scalar(
        "SELECT f.retention FROM files f JOIN revisions r ON r.id = f.revision_id
         WHERE r.code = 'B'",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(kept, ["latest"]);
    app.teardown().await;
}

#[tokio::test]
async fn audit_purge_keeps_what_separation_of_duties_needs() {
    let app = app_or_skip!();
    let (max, token) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    part(&app, &token).await;
    let rev: uuid::Uuid = sqlx::query_scalar("SELECT id FROM revisions LIMIT 1")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    for (action, entity) in [
        ("revision.update", "revision"),
        ("login", "session"),
        ("revision.update", "item"),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (at, user_id, username, action, entity_type, entity_id, summary)
             VALUES (now() - interval '100 days', $1, 'max', $2, $3, $4, 'seeded')",
        )
        .bind(max.id)
        .bind(action)
        .bind(entity)
        .bind(rev)
        .execute(&app.pool)
        .await
        .unwrap();
    }
    let count = |app: &common::TestApp| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_log WHERE summary = 'seeded' AND entity_type = 'revision'",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    // the old edit of an in-work revision survives; the other old entries go
    teamcraft::services::audit::purge_older_than(&app.pool, 30)
        .await
        .unwrap();
    assert_eq!(count(&app).await, 1);
    let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE summary = 'seeded'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(others, 1);
    // once the revision is released it is no longer needed
    sqlx::query("UPDATE revisions SET state = 'released' WHERE id = $1")
        .bind(rev)
        .execute(&app.pool)
        .await
        .unwrap();
    teamcraft::services::audit::purge_older_than(&app.pool, 30)
        .await
        .unwrap();
    assert_eq!(count(&app).await, 0);
    app.teardown().await;
}
