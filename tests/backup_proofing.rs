//! Backup and restore beyond the success path of `tests/backup_restore.rs`:
//!
//! * a corrupt or a missing vault object refuses the backup, and nothing that
//!   looks like a backup is left;
//! * a backup of an older schema (v0.1.0 with data, the fixture of
//!   `tests/migrations.rs`) restores into an empty database and then migrates;
//! * a failing `restore --force` leaves the database **and** the vault as they
//!   were;
//! * credentials from a percent-encoded URL and from a `_FILE` reach the
//!   PostgreSQL client tools;
//! * large objects travel with the dump, the preview cache stays out of the
//!   backup and a target's preview cache is left alone.
//!
//! Needs `pg_dump` and `pg_restore` on `PATH` with a major version at least
//! that of the server (`TEAMCRAFT_REQUIRE_PG_TOOLS=1` makes their absence fail).

mod common;

use std::{borrow::Cow, path::Path, process::Command};

use axum::http::{Method, StatusCode};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use teamcraft::{
    backup,
    config::Config,
    db::{self, MIGRATOR},
    domain::Role,
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn tool_major(tool: &str) -> Option<u32> {
    let out = Command::new(tool).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find_map(|w| w.split('.').next()?.parse::<u32>().ok())
}

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

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tc-backup-proof-{name}-{}", uuid::Uuid::new_v4()))
}

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn object_path(vault: &Path, hash: &str) -> std::path::PathBuf {
    vault
        .join("objects")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(hash)
}

/// Every file below `dir` with its content (for "untouched" checks).
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// An item with one uploaded file; returns the token and the content hash.
async fn with_a_file(app: &common::TestApp, content: &[u8]) -> (String, String) {
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            &token,
            "bracket.step",
            content,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    (token, sha(content))
}

#[tokio::test]
async fn a_corrupt_or_missing_vault_object_refuses_the_backup() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        return app.teardown().await;
    }
    let content = b"ISO-10303-21; bracket";
    let (_, hash) = with_a_file(&app, content).await;
    let source = app.client_config(&app.db_name, &app.vault_dir);
    let object = object_path(&app.vault_dir, &hash);
    assert!(object.is_file());

    // corrupt: the object's content no longer matches its name
    std::fs::write(&object, b"bit rot").unwrap();
    let dir = scratch("corrupt");
    let err = backup::backup(&source, &dir).await.unwrap_err();
    assert!(format!("{err:#}").contains("corrupt"), "{err:#}");
    assert!(!dir.exists(), "nothing that looks like a backup is left");

    // missing: the database refers to a content the vault does not have
    std::fs::remove_file(&object).unwrap();
    let dir = scratch("missing");
    let err = backup::backup(&source, &dir).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("missing from the vault"),
        "{err:#}"
    );
    assert!(!dir.exists());

    // repaired: the backup is written
    std::fs::write(&object, content).unwrap();
    let dir = scratch("ok");
    let summary = backup::backup(&source, &dir).await.expect("backup");
    assert_eq!(summary.vault_objects, 1);
    assert!(dir.join("manifest.json").is_file());
    let _ = std::fs::remove_dir_all(&dir);
    app.teardown().await;
}

/// Migrations of the pre-release v0.1.0 (see `tests/migrations.rs`).
fn in_v0_1_0(version: i64) -> bool {
    matches!(version, 1..=14 | 16..=20 | 22)
}

#[tokio::test]
async fn a_backup_of_an_older_schema_restores_and_then_migrates() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        return app.teardown().await;
    }
    // a v0.1.0 installation with data
    let old_db = app.create_empty_database("v010src").await;
    let old_vault = scratch("v010vault");
    let old = app.open(&old_db, old_vault.clone()).await;
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|m| in_v0_1_0(m.version))
                .cloned()
                .collect(),
        ),
        ignore_missing: MIGRATOR.ignore_missing,
        locking: MIGRATOR.locking,
        no_tx: MIGRATOR.no_tx,
    }
    .run(&old.pool)
    .await
    .expect("v0.1.0 migrations");
    let seed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations/v0.1.0-seed.sql"),
    )
    .unwrap();
    sqlx::raw_sql(&seed).execute(&old.pool).await.expect("seed");
    // the fixture names invented hashes: give every referenced content a
    // real object in the old vault
    let referenced = teamcraft::services::files::all_hashes(&old.pool)
        .await
        .unwrap();
    let mut contents = Vec::new();
    for (i, fake) in referenced.iter().enumerate() {
        let content = format!("content {i} of v0.1.0").into_bytes();
        let real = sha(&content);
        for table in ["files", "file_versions", "change_request_attachments"] {
            sqlx::query(&format!("UPDATE {table} SET sha256 = $2 WHERE sha256 = $1"))
                .bind(fake)
                .bind(&real)
                .execute(&old.pool)
                .await
                .unwrap();
        }
        let path = object_path(&old_vault, &real);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &content).unwrap();
        contents.push((real, content));
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM revisions")
        .fetch_one(&old.pool)
        .await
        .unwrap();

    let dir = scratch("v010backup");
    let summary = backup::backup(&app.client_config(&old_db, &old_vault), &dir)
        .await
        .expect("backup of the v0.1.0 database");
    assert_eq!(summary.vault_objects, contents.len());

    // restore into an empty database and vault, then start the current server
    let new_db = app.create_empty_database("v010dst").await;
    let new_vault = scratch("v010restored");
    backup::restore(&app.client_config(&new_db, &new_vault), &dir, false)
        .await
        .expect("restore of the v0.1.0 backup");
    let restored = app.open(&new_db, new_vault.clone()).await;
    db::migrate(&restored.pool)
        .await
        .expect("migrate after restore");
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&restored.pool)
        .await
        .unwrap();
    assert_eq!(applied as usize, MIGRATOR.iter().count());
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM revisions")
        .fetch_one(&restored.pool)
        .await
        .unwrap();
    assert_eq!(after, rows);
    // the current API serves the old data and its file contents
    let actor = restored.actor("erika").await;
    let (_, token) = teamcraft::services::users::create_token(
        &restored.pool,
        &actor,
        "t",
        teamcraft::services::users::TokenLifetime::Unlimited,
        teamcraft::domain::TokenScope::Full,
    )
    .await
    .unwrap();
    let file_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM files WHERE filename = 'bracket.step'")
            .fetch_one(&restored.pool)
            .await
            .unwrap();
    let r = restored
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/content"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        contents.iter().any(|(_, c)| *c == r.body),
        "the restored content"
    );

    let _ = std::fs::remove_dir_all(&dir);
    restored.teardown().await;
    old.teardown().await;
    app.teardown().await;
}

#[tokio::test]
async fn a_failing_forced_restore_leaves_database_and_vault_untouched() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        return app.teardown().await;
    }
    with_a_file(&app, b"STEP of the backup").await;
    let dir = scratch("force");
    backup::backup(&app.client_config(&app.db_name, &app.vault_dir), &dir)
        .await
        .expect("backup");

    // a target with data of its own: a newer table that depends on a table
    // the restore would replace, and a vault with other objects and previews
    let target_db = app.create_empty_database("forcedst").await;
    let target_vault = scratch("forcevault");
    let target = app.open(&target_db, target_vault.clone()).await;
    db::migrate(&target.pool).await.unwrap();
    target.user_in("max", Role::Editor, &[]).await;
    let own = b"an object of the target";
    let own_path = object_path(&target_vault, &sha(own));
    std::fs::create_dir_all(own_path.parent().unwrap()).unwrap();
    std::fs::write(&own_path, own).unwrap();
    sqlx::query("CREATE TABLE zz_newer (user_id UUID REFERENCES users (id))")
        .execute(&target.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO zz_newer SELECT id FROM users")
        .execute(&target.pool)
        .await
        .unwrap();
    std::fs::create_dir_all(target_vault.join("previews")).unwrap();
    std::fs::write(target_vault.join("previews").join("x.tcmesh"), b"cached").unwrap();
    let vault_before = tree(&target_vault);
    let users =
        "SELECT username || ':' || (SELECT COUNT(*) FROM zz_newer)::text FROM users ORDER BY 1";
    let users_before: Vec<String> = sqlx::query_scalar(users)
        .fetch_all(&target.pool)
        .await
        .unwrap();

    let err = backup::restore(&app.client_config(&target_db, &target_vault), &dir, true)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("unchanged"), "{err:#}");
    assert_eq!(tree(&target_vault), vault_before, "the vault is untouched");
    let users_after: Vec<String> = sqlx::query_scalar(users)
        .fetch_all(&target.pool)
        .await
        .unwrap();
    assert_eq!(users_after, users_before, "the database is untouched");

    let _ = std::fs::remove_dir_all(&dir);
    target.teardown().await;
    app.teardown().await;
}

/// Percent-encodes every byte (valid in a URL, and it exercises the decoding).
fn encode_all(s: &str) -> String {
    s.bytes().map(|b| format!("%{b:02X}")).collect()
}

#[tokio::test]
async fn credentials_from_a_percent_encoded_url_and_from_a_file_work() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        return app.teardown().await;
    }
    let base = app.client_config(&app.db_name, &app.vault_dir);
    let o = &base.database;
    let (host, port, user) = (
        o.get_host().to_string(),
        o.get_port(),
        o.get_username().to_string(),
    );
    let password = base.database_password.clone();
    let vault = app.vault_dir.to_string_lossy().to_string();

    // the password in the URL, every byte percent-encoded
    let url = match &password {
        Some(p) => format!(
            "postgres://{}:{}@{host}:{port}/{}",
            encode_all(&user),
            encode_all(p),
            app.db_name
        ),
        None => format!(
            "postgres://{}@{host}:{port}/{}",
            encode_all(&user),
            app.db_name
        ),
    };
    let (u, v) = (url.clone(), vault.clone());
    let from_url = Config::from_lookup(move |key| match key {
        "TEAMCRAFT_DATABASE_URL" => Some(u.clone()),
        "TEAMCRAFT_VAULT_DIR" => Some(v.clone()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(from_url.database_password, password, "decoded");
    let dir = scratch("url");
    backup::backup(&from_url, &dir)
        .await
        .expect("backup with the URL credentials");
    let _ = std::fs::remove_dir_all(&dir);

    // the password from TEAMCRAFT_DB_PASSWORD_FILE (with a trailing newline)
    let secret = scratch("secret");
    std::fs::write(
        &secret,
        format!("{}\n", password.clone().unwrap_or_default()),
    )
    .unwrap();
    let (s, v, db_name) = (
        secret.to_string_lossy().to_string(),
        vault.clone(),
        app.db_name.clone(),
    );
    let with_password = password.is_some();
    let from_file = Config::from_lookup(move |key| match key {
        "TEAMCRAFT_DB_HOST" => Some(host.clone()),
        "TEAMCRAFT_DB_PORT" => Some(port.to_string()),
        "TEAMCRAFT_DB_USER" => Some(user.clone()),
        "TEAMCRAFT_DB_NAME" => Some(db_name.clone()),
        "TEAMCRAFT_DB_PASSWORD_FILE" if with_password => Some(s.clone()),
        "TEAMCRAFT_VAULT_DIR" => Some(v.clone()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(from_file.database_password, password, "read from the file");
    let dir = scratch("file");
    backup::backup(&from_file, &dir)
        .await
        .expect("backup with the password file");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&secret);
    app.teardown().await;
}

#[tokio::test]
async fn large_objects_travel_and_the_preview_cache_stays_out() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        return app.teardown().await;
    }
    with_a_file(&app, b"STEP with a preview").await;
    let blob: Vec<u8> = (0..70_000u32).map(|i| (i % 256) as u8).collect();
    let oid: sqlx::postgres::types::Oid = sqlx::query_scalar("SELECT lo_from_bytea(0, $1)")
        .bind(&blob)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    std::fs::create_dir_all(app.vault_dir.join("previews")).unwrap();
    std::fs::write(
        app.vault_dir.join("previews").join("source.tcmesh"),
        b"source cache",
    )
    .unwrap();

    let dir = scratch("lo");
    backup::backup(&app.client_config(&app.db_name, &app.vault_dir), &dir)
        .await
        .expect("backup");
    assert!(
        !dir.join("vault").join("previews").exists(),
        "no preview cache in the backup"
    );
    assert!(
        tree(&dir).iter().all(|(p, _)| !p.contains("tcmesh")),
        "no cached preview anywhere in the backup"
    );

    let db = app.create_empty_database("lodst").await;
    let vault = scratch("lovault");
    std::fs::create_dir_all(vault.join("previews")).unwrap();
    std::fs::write(
        vault.join("previews").join("target.tcmesh"),
        b"target cache",
    )
    .unwrap();
    backup::restore(&app.client_config(&db, &vault), &dir, false)
        .await
        .expect("restore");
    let copy = app.open(&db, vault.clone()).await;
    let restored: Vec<u8> = sqlx::query_scalar("SELECT lo_get($1)")
        .bind(oid)
        .fetch_one(&copy.pool)
        .await
        .expect("the large object is restored");
    assert_eq!(restored, blob);
    assert_eq!(
        std::fs::read(vault.join("previews").join("target.tcmesh")).unwrap(),
        b"target cache",
        "the target's preview cache is left alone"
    );
    assert!(!vault.join("previews").join("source.tcmesh").exists());

    let _ = std::fs::remove_dir_all(&dir);
    copy.teardown().await;
    app.teardown().await;
}
