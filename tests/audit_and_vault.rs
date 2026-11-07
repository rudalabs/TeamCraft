//! Functional tests of the audit log (every modification with user, time
//! and details) and of the content-addressed vault (identical content is
//! stored once, at `objects/ab/cd/<sha256>`).

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

/// Every step in the life of an item leaves an audit entry on the item or
/// the revision it changed; the history endpoint lists them newest first.
#[tokio::test]
async fn every_modification_of_an_item_is_audited() {
    let app = app_or_skip!();
    let (erika, token) = app.user_in("erika", Role::Editor, &["DRONE"]).await;
    let (_, reviewer) = app.user("rolf", Role::Editor).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Propeller", "project": "DRONE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    let item_id: uuid::Uuid = r.json()["item_id"].as_str().unwrap().parse().unwrap();
    let rev_a: uuid::Uuid = r.json()["id"].as_str().unwrap().parse().unwrap();

    let base = format!("/api/v1/items/{number}/revisions/A");
    // A new item is checked out to its author; checking out again changes
    // nothing and is not audited.
    assert_eq!(r.json()["checked_out_by"], json!(erika.id));
    assert_eq!(
        app.api(Method::POST, &format!("{base}/checkout"), &token, None)
            .await
            .status,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::PATCH,
            &base,
            &token,
            Some(json!({ "material": "PA12" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for content in [&b"blade v1"[..], &b"blade v2"[..]] {
        let r = app
            .api_upload(&format!("{base}/files"), &token, "blade.stl", content)
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    assert_eq!(
        app.api(Method::POST, &format!("{base}/checkin"), &token, None)
            .await
            .status,
        StatusCode::OK
    );
    let chg = app.release(&token, ("rolf", &reviewer), &[&number]).await;
    let r = app
        .api(Method::POST, &format!("{base}/revise"), &token, None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev_b: uuid::Uuid = r.json()["id"].as_str().unwrap().parse().unwrap();
    let r = app
        .api(Method::POST, &format!("{base}/obsolete"), &token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    assert_eq!(app.audit_actions(item_id).await, ["item.create"]);
    assert_eq!(
        app.audit_actions(rev_a).await,
        [
            "revision.update",
            "file.upload",
            "file.version",
            "revision.checkin",
            "revision.obsolete",
        ]
    );
    assert_eq!(app.audit_actions(rev_b).await, ["revision.revise"]);

    let change_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&chg)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        app.audit_actions(change_id).await,
        [
            "change.create",
            "change.add_item",
            "change.add_reviewer",
            "change.submit",
            "change.approve",
            "change.released",
        ]
    );
    let (by_erika, by_rolf): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE user_id = $2 AND username = 'erika'),
                COUNT(*) FILTER (WHERE username = 'rolf')
         FROM audit_log WHERE entity_id = $1",
    )
    .bind(change_id)
    .bind(erika.id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!((by_erika, by_rolf), (4, 2), "who did what is recorded");

    // The details carry the stored version and its content hash
    let details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE entity_id = $1 AND action = 'file.version'",
    )
    .bind(rev_a)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(details["version"], 2);
    assert_eq!(details["sha256"].as_str().map(str::len), Some(64));

    // History of the item: everything about the item and its revisions,
    // newest first, limited on request
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/history"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let history = r.json().as_array().unwrap().clone();
    assert_eq!(history.len(), 7, "{history:?}");
    assert_eq!(history[0]["action"], "revision.obsolete");
    assert_eq!(history.last().unwrap()["action"], "item.create");
    assert!(history.iter().all(|e| e["username"] == "erika"));
    let times: Vec<&str> = history.iter().map(|e| e["at"].as_str().unwrap()).collect();
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(times, sorted, "newest first");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/history?limit=2"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 2);
    app.teardown().await;
}

/// Refused modifications leave no audit entry behind.
#[tokio::test]
async fn refused_modifications_are_not_audited() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["DRONE"]).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["DRONE"]).await;
    let number = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Arm", "project": "DRONE" }),
        )
        .await;
    let base = format!("/api/v1/items/{number}/revisions/A");
    // A new item is checked out to its author: release the lock first so
    // that the author's edit below is refused.
    let r = app
        .api(
            Method::POST,
            &format!("{base}/cancel-checkout"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    for (method, uri, body) in [
        (Method::POST, format!("{base}/checkout"), None),
        (Method::PATCH, base.clone(), Some(json!({ "name": "x" }))),
        (Method::POST, format!("{base}/revise"), None),
        (Method::POST, format!("{base}/obsolete"), None),
    ] {
        let r = app.api(method, &uri, &viewer, body).await;
        assert!(r.status.is_client_error(), "{uri}: {}", r.status);
    }
    // Edits without a check-out are refused for the author as well
    let r = app
        .api(Method::PATCH, &base, &token, Some(json!({ "name": "x" })))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    app.teardown().await;
}

fn count_files(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                count_files(&path)
            } else {
                1
            }
        })
        .sum()
}

/// The same content uploaded under different names and items is stored
/// once; the blob lives at `objects/<2>/<2>/<sha256>` and new content adds
/// a new blob while the old version stays downloadable.
#[tokio::test]
async fn identical_content_is_stored_once() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["SHOP"]).await;
    let mut hashes = Vec::new();
    let mut file_ids = Vec::new();
    let mut numbers = Vec::new();
    for (name, filename) in [("Jig", "jig.txt"), ("Fixture", "copy-of-jig.txt")] {
        let number = app
            .create_item(
                &token,
                json!({ "item_type": "part", "name": name, "project": "SHOP" }),
            )
            .await;
        numbers.push(number.clone());
        let base = format!("/api/v1/items/{number}/revisions/A");
        app.api(Method::POST, &format!("{base}/checkout"), &token, None)
            .await;
        let r = app
            .api_upload(
                &format!("{base}/files"),
                &token,
                filename,
                b"ISO-10303-21; same bytes",
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        hashes.push(r.json()[0]["sha256"].as_str().unwrap().to_string());
        file_ids.push(r.json()[0]["id"].as_str().unwrap().to_string());
    }
    assert_eq!(hashes[0], hashes[1]);
    let vault = app.vault().await;
    let path = vault.object_path(&hashes[0]).unwrap();
    assert!(path.is_file(), "{path:?}");
    let rel = path.strip_prefix(vault.root()).unwrap();
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        parts,
        [
            "objects".to_string(),
            hashes[0][0..2].to_string(),
            hashes[0][2..4].to_string(),
            hashes[0].clone()
        ]
    );
    let objects = vault.root().join("objects");
    assert_eq!(count_files(&objects), 1, "one blob for both files");
    assert!(vault.verify(&hashes[0]).await.unwrap());

    // A new version of one file adds a blob; the first version stays
    let r = app
        .api_upload(
            &format!("/api/v1/items/{}/revisions/A/files", numbers[0]),
            &token,
            "jig.txt",
            b"ISO-10303-21; changed bytes",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()[0]["version"], 2);
    assert_ne!(r.json()[0]["sha256"], hashes[0].as_str());
    assert_eq!(count_files(&objects), 2);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{}/versions", file_ids[0]),
            &token,
            None,
        )
        .await;
    let versions = r.json().as_array().unwrap().clone();
    assert_eq!(versions.len(), 2, "{versions:?}");
    let v1 = versions
        .iter()
        .find(|v| v["version"] == 1)
        .expect("version 1");
    let r = app
        .api(
            Method::GET,
            &format!(
                "/api/v1/file-versions/{}/content",
                v1["id"].as_str().unwrap()
            ),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, b"ISO-10303-21; same bytes");
    // the other item's file still points at the shared blob
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{}/content", file_ids[1]),
            &token,
            None,
        )
        .await;
    assert_eq!(r.body, b"ISO-10303-21; same bytes");
    app.teardown().await;
}
