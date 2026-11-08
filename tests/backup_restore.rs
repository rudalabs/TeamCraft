//! End-to-end test of `teamcraft backup` / `teamcraft restore` with the real
//! PostgreSQL client tools: data written through the API survives a backup
//! and a restore into a fresh database and vault.
//!
//! Needs `pg_dump` and `pg_restore` on `PATH` with a major version at least
//! that of the server; otherwise the test prints a notice and passes.
//! `TEAMCRAFT_REQUIRE_PG_TOOLS=1` turns that into a failure.

mod common;

use std::process::Command;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{backup, domain::Role};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Major version of a PostgreSQL client tool, `None` if it does not run.
fn tool_major(tool: &str) -> Option<u32> {
    let out = Command::new(tool).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "pg_dump (PostgreSQL) 17.11" or "pg_dump (PostgreSQL) 16.4 (Ubuntu …)"
    text.split_whitespace()
        .nth(2)?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Whether the client tools can dump this server; prints why not.
async fn tools_usable(app: &common::TestApp) -> bool {
    let required = std::env::var("TEAMCRAFT_REQUIRE_PG_TOOLS").is_ok();
    let server: i64 = sqlx::query_scalar::<_, String>("SHOW server_version_num")
        .fetch_one(&app.pool)
        .await
        .unwrap()
        .parse()
        .unwrap();
    let server = (server / 10000) as u32;
    let reason = match (tool_major("pg_dump"), tool_major("pg_restore")) {
        (Some(d), Some(r)) if d >= server && r >= server => return true,
        (Some(d), Some(_)) => format!("pg_dump {d} is older than the server ({server})"),
        _ => "pg_dump/pg_restore are not on PATH".to_string(),
    };
    if required {
        panic!("TEAMCRAFT_REQUIRE_PG_TOOLS is set, but {reason}");
    }
    eprintln!("skipping backup test: {reason}");
    false
}

/// Row counts of the tables a backup must carry over.
async fn counts(pool: &sqlx::PgPool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for table in [
        "users",
        "api_tokens",
        "projects",
        "project_members",
        "items",
        "revisions",
        "files",
        "file_versions",
        "bom_lines",
        "counters",
        "audit_log",
        "_sqlx_migrations",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        out.push((table.to_string(), n));
    }
    out
}

#[tokio::test]
async fn backup_and_restore_with_the_postgresql_client_tools() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        app.teardown().await;
        return;
    }
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;

    // some data: an assembly with a STEP file and a BOM line to a part
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": "Chassis", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let asm = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Wheel", "project": "ROVER" })),
        )
        .await;
    let wheel = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &eng,
            Some(json!({ "child": wheel, "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let step = include_bytes!("fixtures/cube.step");
    let r = app
        .api_upload(
            &format!("/api/v1/items/{asm}/revisions/A/files"),
            &eng,
            "chassis.step",
            step,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let uploaded = r.json();
    let file_id = uploaded[0]["id"]
        .as_str()
        .or_else(|| uploaded["id"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("file id in {uploaded}"));

    // backup
    let dir = std::env::temp_dir().join(format!("teamcraft-backup-e2e-{}", uuid::Uuid::new_v4()));
    let source = app.client_config(&app.db_name, &app.vault_dir);
    let summary = backup::backup(&source, &dir).await.expect("backup");
    assert_eq!(summary.vault_objects, 1);
    assert!(summary.db_dump_bytes > 0);
    let err = backup::backup(&source, &dir).await.unwrap_err().to_string();
    assert!(err.contains("not empty"), "{err}");

    // restore into a fresh database and vault
    let database = app.create_empty_database("restored").await;
    let vault =
        std::env::temp_dir().join(format!("teamcraft-restored-vault-{}", uuid::Uuid::new_v4()));
    let target = app.client_config(&database, &vault);
    let restored = backup::restore(&target, &dir, false)
        .await
        .expect("restore");
    assert_eq!(
        (restored.vault_objects, restored.copied, restored.skipped),
        (1, 1, 0)
    );
    let copy = app.open(&database, vault).await;
    assert_eq!(counts(&copy.pool).await, counts(&app.pool).await);

    // the restored instance serves the same data with the same token
    let r = copy
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&wheel), "{}", r.text());
    let r = copy
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/content"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.body, step.to_vec());
    // and keeps working: new items continue the counters
    let r = copy
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Axle", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_ne!(r.json()["item_number"], wheel.as_str());

    // a second restore needs --force and then replaces the data again
    let err = backup::restore(&target, &dir, false).await.unwrap_err();
    assert!(format!("{err:#}").contains("--force"), "{err:#}");
    let again = backup::restore(&target, &dir, true)
        .await
        .expect("restore --force");
    assert_eq!((again.copied, again.skipped), (0, 1));
    assert_eq!(counts(&copy.pool).await, counts(&app.pool).await);

    // A backup older than the database (a table of a newer version depends
    // on a restored one): the restore fails as a whole and changes nothing.
    sqlx::query("CREATE TABLE zz_newer (user_id UUID REFERENCES users (id))")
        .execute(&copy.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO zz_newer SELECT id FROM users")
        .execute(&copy.pool)
        .await
        .unwrap();
    let before = counts(&copy.pool).await;
    let err = backup::restore(&target, &dir, true).await.unwrap_err();
    assert!(format!("{err:#}").contains("unchanged"), "{err:#}");
    assert_eq!(counts(&copy.pool).await, before);
    let newer: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM zz_newer")
        .fetch_one(&copy.pool)
        .await
        .unwrap();
    assert!(newer > 0, "the newer table and its rows are still there");

    let _ = std::fs::remove_dir_all(&dir);
    copy.teardown().await;
    app.teardown().await;
}
