//! Chunked, resumable uploads (`/api/v1/uploads`): end to end, resume after
//! an interruption, offset and size checks, privacy, verification, the
//! check-in with chunked files and the clean-up of expired sessions.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use teamcraft::domain::Role;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const REV: &str = "/api/v1/items/PRT-000001/revisions/A";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Creates PRT-000001 (revision A, checked out to its author).
async fn item(app: &common::TestApp, token: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": "Simulation deck", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn start(app: &common::TestApp, token: &str, body: Value) -> common::Response {
    app.api(Method::POST, "/api/v1/uploads", token, Some(body))
        .await
}

async fn patch(
    app: &common::TestApp,
    token: &str,
    id: &str,
    offset: u64,
    chunk: &[u8],
) -> common::Response {
    let request = Request::builder()
        .method(Method::PATCH)
        .uri(format!("/api/v1/uploads/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/offset+octet-stream")
        .header("Upload-Offset", offset.to_string())
        .body(Body::from(chunk.to_vec()))
        .unwrap();
    app.send(request).await
}

async fn head(app: &common::TestApp, token: &str, id: &str) -> common::Response {
    let request = Request::builder()
        .method(Method::HEAD)
        .uri(format!("/api/v1/uploads/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    app.send(request).await
}

#[tokio::test]
async fn chunked_upload_end_to_end() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let content = b"0123456789abcdefghij";
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "results.op2",
                "size": content.len(), "sha256": sha(content), "base": 0 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let session = r.json();
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(session["offset"], 0);
    assert_eq!(session["size"], 20);
    assert!(session["chunk_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        r.header("location").as_deref(),
        Some(format!("/api/v1/uploads/{id}").as_str())
    );

    for (offset, chunk) in [
        (0u64, &content[..8]),
        (8, &content[8..16]),
        (16, &content[16..]),
    ] {
        let r = patch(&app, &max, &id, offset, chunk).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(
            r.header("upload-offset"),
            Some((offset + chunk.len() as u64).to_string())
        );
    }
    let r = head(&app, &max, &id).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("upload-offset").as_deref(), Some("20"));
    assert_eq!(r.header("upload-length").as_deref(), Some("20"));
    let r = app
        .api(Method::GET, &format!("/api/v1/uploads/{id}"), &max, None)
        .await;
    assert_eq!(r.json()["complete"], true);

    // finish = check-in: version 1 is stored and the lock released
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{id}/finish"),
            &max,
            Some(json!({ "comment": "first run", "check_in": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let done = r.json();
    assert_eq!(done["files"][0]["filename"], "results.op2");
    assert_eq!(done["files"][0]["version"], 1);
    assert!(done["revision"]["checked_out_by"].is_null());

    let r = app
        .api(Method::GET, &format!("{REV}/files"), &max, None)
        .await;
    let files = r.json();
    assert_eq!(files[0]["sha256"], sha(content));
    let file_id = files[0]["id"].as_str().unwrap();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/content"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.body, content);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/versions"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.json()[0]["comment"], "first run");

    // the session is gone, and the audit log has the upload
    let r = app
        .api(Method::GET, &format!("/api/v1/uploads/{id}"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('file.upload', 'revision.checkin')",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(audited >= 2);
    app.teardown().await;
}

#[tokio::test]
async fn resumes_after_an_interruption() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let content: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "mesh.bdf",
                "size": content.len(), "sha256": sha(&content) }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let r = patch(&app, &max, &id, 0, &content[..100]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // The connection broke while the second chunk was sent: part of it
    // reached the disk, but the server never confirmed it.
    let vault = app.vault().await;
    let staging = vault
        .uploads_dir()
        .join(Uuid::parse_str(&id).unwrap().simple().to_string());
    let mut partial = std::fs::read(&staging).unwrap();
    partial.extend_from_slice(b"garbage of the broken chunk");
    std::fs::write(&staging, partial).unwrap();

    // The client does not know what arrived: sending the chunk again at the
    // wrong offset is refused with 409 and the server's offset …
    let r = patch(&app, &max, &id, 0, &content[..100]).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.header("upload-offset").as_deref(), Some("100"));
    // … which HEAD tells as well; the upload resumes from there.
    let r = head(&app, &max, &id).await;
    assert_eq!(r.header("upload-offset").as_deref(), Some("100"));
    let r = patch(&app, &max, &id, 100, &content[100..250]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // beyond the announced size
    let r = patch(&app, &max, &id, 250, &content[200..]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // finishing early is refused
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{id}/finish"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = patch(&app, &max, &id, 250, &content[250..]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a chunk after the end is refused
    let r = patch(&app, &max, &id, 300, b"x").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // finish without check-in: the version is stored, the lock stays
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{id}/finish"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["files"][0]["version"], 1);
    assert!(r.json()["revision"].is_null());
    let r = app.api(Method::GET, REV, &max, None).await;
    assert!(!r.json()["revision"]["checked_out_by"].is_null());
    let r = app
        .api(Method::GET, &format!("{REV}/files"), &max, None)
        .await;
    assert_eq!(r.json()[0]["sha256"], sha(&content));
    assert!(!staging.exists(), "the staging file moved into the vault");
    app.teardown().await;
}

#[tokio::test]
async fn sessions_are_private_and_checked() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    item(&app, &max).await;
    let new = |size: u64| json!({ "item": "PRT-000001", "revision": "A", "filename": "part.prt", "size": size });
    // larger than TEAMCRAFT_MAX_UPLOAD_MB (1 MiB in the tests)
    let r = start(&app, &max, new(2 * 1024 * 1024)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // only the holder of the check-out can upload
    let r = start(&app, &erika, new(4)).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A",
                                       "filename": "part.prt", "size": 4, "sha256": "nope" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    let r = start(&app, &max, new(4)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    // nobody else can see, extend, finish or cancel it – not even an admin
    for token in [&erika, &admin] {
        assert_eq!(
            patch(&app, token, &id, 0, b"evil").await.status,
            StatusCode::NOT_FOUND
        );
        let r = app
            .api(Method::GET, &format!("/api/v1/uploads/{id}"), token, None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/uploads/{id}/finish"),
                token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = app
            .api(
                Method::DELETE,
                &format!("/api/v1/uploads/{id}"),
                token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let r = patch(&app, &max, &id, 0, b"mine").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // missing offset header
    let request = Request::builder()
        .method(Method::PATCH)
        .uri(format!("/api/v1/uploads/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {max}"))
        .body(Body::from("x"))
        .unwrap();
    assert_eq!(app.send(request).await.status, StatusCode::BAD_REQUEST);
    // the owner cancels: the session and its data are gone
    let r = app
        .api(Method::DELETE, &format!("/api/v1/uploads/{id}"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(head(&app, &max, &id).await.status, StatusCode::NOT_FOUND);
    let staging = app
        .vault()
        .await
        .uploads_dir()
        .join(Uuid::parse_str(&id).unwrap().simple().to_string());
    assert!(!staging.exists());
    let r = app
        .api(Method::GET, &format!("{REV}/files"), &max, None)
        .await;
    assert_eq!(r.json(), json!([]), "nothing was stored");
    app.teardown().await;
}

#[tokio::test]
async fn content_is_verified_before_it_is_stored() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "a.step",
                "size": 5, "sha256": sha(b"right") }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &id, 0, b"wrong").await.status,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{id}/finish"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("SHA-256"), "{}", r.text());
    // the session was reset: upload again from offset 0
    assert_eq!(
        head(&app, &max, &id)
            .await
            .header("upload-offset")
            .as_deref(),
        Some("0")
    );
    assert_eq!(
        patch(&app, &max, &id, 0, b"right").await.status,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{id}/finish"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn check_in_with_chunked_files_keeps_the_base_guard() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    item(&app, &max).await;
    let r = app
        .api_multipart(&format!("{REV}/checkin"), &max, &[("hub.step", b"v1")], &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Max checks out again and uploads a large file in chunks …
    app.api(Method::POST, &format!("{REV}/checkout"), &max, None)
        .await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "hub.step",
                "size": 9, "base": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &id, 0, b"v2 by max").await.status,
        StatusCode::OK
    );
    // … and checks it in together with a small file, in one transaction
    let r = app
        .api_multipart(
            &format!("{REV}/checkin"),
            &max,
            &[("notes.txt", b"small")],
            &[
                ("uploads", &format!("[\"{id}\"]")),
                ("comment", "big and small"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut stored: Vec<(String, i64)> = r.json()["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["filename"].as_str().unwrap().to_string(),
                f["version"].as_i64().unwrap(),
            )
        })
        .collect();
    stored.sort();
    assert_eq!(
        stored,
        [("hub.step".to_string(), 2), ("notes.txt".to_string(), 1)]
    );
    assert!(r.json()["revision"]["checked_out_by"].is_null());
    assert_eq!(
        app.api(Method::GET, &format!("/api/v1/uploads/{id}"), &max, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Erika saves v3 after an admin broke Max's next lock: Max's upload
    // based on v2 is refused before any byte is sent …
    app.api(Method::POST, &format!("{REV}/checkout"), &max, None)
        .await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "hub.step",
                "size": 6, "base": 2 }),
    )
    .await;
    let late = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &late, 0, b"v3 max").await.status,
        StatusCode::OK
    );
    app.api(
        Method::POST,
        &format!("{REV}/cancel-checkout"),
        &admin,
        None,
    )
    .await;
    app.api(Method::POST, &format!("{REV}/checkout"), &erika, None)
        .await;
    let r = app
        .api_multipart(
            &format!("{REV}/checkin"),
            &erika,
            &[("hub.step", b"v3 by erika")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.api(Method::POST, &format!("{REV}/checkout"), &max, None)
        .await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "hub.step",
                "size": 6, "base": 2 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // … and the one started before the conflict is refused when stored
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{late}/finish"),
            &max,
            Some(json!({ "check_in": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("changed on the server"), "{}", r.text());
    let r = app.api(Method::GET, REV, &max, None).await;
    assert!(
        !r.json()["revision"]["checked_out_by"].is_null(),
        "the lock stays"
    );
    // the verified content is kept: after merging, the client can retry
    // without sending the bytes again (here: it cancels instead)
    let r = app
        .api(Method::GET, &format!("/api/v1/uploads/{late}"), &max, None)
        .await;
    assert_eq!(r.json()["complete"], true);
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/uploads/{late}"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    app.teardown().await;
}

/// The folder of a file in its project group (files.folder, the desktop
/// working folder) comes with the session or with the multipart fields.
#[tokio::test]
async fn chunked_uploads_keep_the_working_folder() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let folder_of = |files: &Value, name: &str| -> String {
        files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .map(|f| f["folder"].as_str().unwrap_or("").to_string())
            .unwrap_or_else(|| panic!("{name} missing: {files}"))
    };

    // an invalid folder is refused before any byte is sent
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "frame.asm",
                "size": 5, "folder": "../outside" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // the session's own folder (normalised) …
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "frame.asm",
                "size": 5, "base": 0, "folder": "chassis\\frames/" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["folder"], "chassis/frames");
    let own = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &own, 0, b"frame").await.status,
        StatusCode::OK
    );
    // … and one without a folder that takes it from the `folders` field
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "wheel.prt",
                "size": 5 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let joined = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &joined, 0, b"wheel").await.status,
        StatusCode::OK
    );
    let r = app
        .api_multipart(
            &format!("{REV}/checkin"),
            &max,
            &[("notes.txt", b"small")],
            &[
                ("uploads", &format!("{own},{joined}")),
                (
                    "folders",
                    r#"{"wheel.prt": "chassis/wheels", "frame.asm": "ignored", "notes.txt": "docs"}"#,
                ),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let files = app
        .api(Method::GET, &format!("{REV}/files"), &max, None)
        .await
        .json();
    assert_eq!(folder_of(&files, "frame.asm"), "chassis/frames");
    assert_eq!(folder_of(&files, "wheel.prt"), "chassis/wheels");
    assert_eq!(folder_of(&files, "notes.txt"), "docs");

    // a new version without a folder keeps the file where it is; finish
    // applies the session's base guard (SHA-256 of version 1)
    app.api(Method::POST, &format!("{REV}/checkout"), &max, None)
        .await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "frame.asm",
                "size": 6, "base": sha(b"frame") }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let next = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &next, 0, b"frame2").await.status,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/uploads/{next}/finish"),
            &max,
            Some(json!({ "check_in": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let files = app
        .api(Method::GET, &format!("{REV}/files"), &max, None)
        .await
        .json();
    assert_eq!(folder_of(&files, "frame.asm"), "chassis/frames");
    app.teardown().await;
}

#[tokio::test]
async fn concurrent_chunks_at_one_offset_are_serialised() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "twice.bin",
                "size": 8, "sha256": sha(b"aaaabbbb") }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    // Two clients (or a retry racing its original) send a chunk at offset 0
    // at the same time: the row lock lets exactly one of them through.
    let (a, b) = tokio::join!(
        patch(&app, &max, &id, 0, b"aaaa"),
        patch(&app, &max, &id, 0, b"xxxx")
    );
    let mut statuses = [a.status, b.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let winner: &[u8] = if a.status == StatusCode::OK {
        b"aaaa"
    } else {
        b"xxxx"
    };
    assert_eq!(
        head(&app, &max, &id)
            .await
            .header("upload-offset")
            .as_deref(),
        Some("4")
    );
    assert_eq!(
        patch(&app, &max, &id, 4, b"bbbb").await.status,
        StatusCode::OK
    );
    let staging = app
        .vault()
        .await
        .uploads_dir()
        .join(Uuid::parse_str(&id).unwrap().simple().to_string());
    assert_eq!(std::fs::read(&staging).unwrap(), [winner, b"bbbb"].concat());
    app.teardown().await;
}

#[tokio::test]
async fn a_lost_staging_file_is_recovered_or_reset() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let finish = |id: String| {
        let app = &app;
        let max = &max;
        async move {
            app.api(
                Method::POST,
                &format!("/api/v1/uploads/{id}/finish"),
                max,
                None,
            )
            .await
        }
    };
    let uploads_dir = app.vault().await.uploads_dir();
    let staging_of = |id: &str| uploads_dir.join(Uuid::parse_str(id).unwrap().simple().to_string());
    // The content is in the vault already (e.g. an earlier finish moved the
    // staging file there, then its transaction failed).
    let r = app
        .api_multipart(
            &format!("{REV}/files"),
            &max,
            &[("other.bin", b"payload!")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // With the announced SHA-256 the content is recovered from the vault.
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "lost.bin",
                "size": 8, "sha256": sha(b"payload!") }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &id, 0, b"payload!").await.status,
        StatusCode::OK
    );
    std::fs::remove_file(staging_of(&id)).unwrap();
    let r = finish(id.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Without it the upload is reset to offset 0 (409), not wedged with 500s.
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "gone.bin", "size": 4 }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &id, 0, b"data").await.status,
        StatusCode::OK
    );
    std::fs::remove_file(staging_of(&id)).unwrap();
    let r = finish(id.clone()).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(
        head(&app, &max, &id)
            .await
            .header("upload-offset")
            .as_deref(),
        Some("0")
    );
    assert_eq!(
        patch(&app, &max, &id, 0, b"data").await.status,
        StatusCode::OK
    );
    let r = finish(id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn expired_sessions_are_removed() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "old.bin", "size": 8 }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        patch(&app, &max, &id, 0, b"half").await.status,
        StatusCode::OK
    );
    let r = start(
        &app,
        &max,
        json!({ "item": "PRT-000001", "revision": "A", "filename": "fresh.bin", "size": 8 }),
    )
    .await;
    let fresh = r.json()["id"].as_str().unwrap().to_string();

    let uuid = Uuid::parse_str(&id).unwrap();
    sqlx::query("UPDATE upload_sessions SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(uuid)
        .execute(&app.pool)
        .await
        .unwrap();
    // an expired session is unusable at once …
    assert_eq!(head(&app, &max, &id).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        patch(&app, &max, &id, 4, b"rest").await.status,
        StatusCode::NOT_FOUND
    );
    // … and the clean-up removes it with its data, but not the fresh one
    let vault = app.vault().await;
    let removed = teamcraft::services::uploads::purge_expired(&app.pool, &vault)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM upload_sessions")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert_eq!(left, [Uuid::parse_str(&fresh).unwrap()]);
    assert!(!vault.uploads_dir().join(uuid.simple().to_string()).exists());
    assert_eq!(head(&app, &max, &fresh).await.status, StatusCode::OK);
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// The routes of the web UI (`/uploads`, session cookie, used by static/app.js)
// ---------------------------------------------------------------------------

/// A browser request: session cookie and same-origin headers.
fn web_request(method: Method, uri: &str, cookie: Option<&str>) -> axum::http::request::Builder {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header("X-TeamCraft-Script", "1");
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    builder
}

async fn web_get(app: &common::TestApp, cookie: &str, id: &str) -> common::Response {
    let request = web_request(Method::GET, &format!("/uploads/{id}"), Some(cookie))
        .body(Body::empty())
        .unwrap();
    app.send(request).await
}

async fn web_start(app: &common::TestApp, cookie: &str, body: Value) -> common::Response {
    let request = web_request(Method::POST, "/uploads", Some(cookie))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    app.send(request).await
}

async fn web_patch(
    app: &common::TestApp,
    cookie: &str,
    id: &str,
    offset: u64,
    chunk: &[u8],
) -> common::Response {
    let request = web_request(Method::PATCH, &format!("/uploads/{id}"), Some(cookie))
        .header(header::CONTENT_TYPE, "application/offset+octet-stream")
        .header("Upload-Offset", offset.to_string())
        .body(Body::from(chunk.to_vec()))
        .unwrap();
    app.send(request).await
}

/// The attach or check-in form as app.js sends it: text fields and small files.
async fn web_form(
    app: &common::TestApp,
    uri: &str,
    cookie: &str,
    files: &[(&str, &[u8])],
    fields: &[(&str, &str)],
) -> common::Response {
    let boundary = "teamcraft-chunked-boundary";
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    for (filename, content) in files {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let request = web_request(Method::POST, uri, Some(cookie))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    app.send(request).await
}

async fn file_named(app: &common::TestApp, token: &str, name: &str) -> Value {
    let files = app
        .api(Method::GET, &format!("{REV}/files"), token, None)
        .await
        .json();
    files
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["filename"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn chunked_uploads_from_the_web_ui() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    app.user_in("erika", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let rev = app.api(Method::GET, REV, &max, None).await.json();
    let rev_id = rev["revision"]["id"].as_str().unwrap().to_string();
    let cookie = app.login("max").await;
    let content = b"0123456789abcdefghij";

    // The revision page marks the attach and check-in forms for app.js.
    let page = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await
        .text();
    assert_eq!(
        page.matches(&format!("data-chunked=\"{rev_id}\"")).count(),
        2,
        "attach and check-in form"
    );

    // Signed out: a JSON 401, not a redirect to the login page.
    let start_body = json!({ "revision_id": rev_id, "filename": "a.bin", "size": 1 }).to_string();
    let request = web_request(Method::POST, "/uploads", None)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(start_body.clone()))
        .unwrap();
    let r = app.send(request).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"]["code"], "unauthorized");
    // Another site cannot start uploads with the cookie.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/uploads")
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(start_body))
        .unwrap();
    assert_eq!(app.send(request).await.status, StatusCode::FORBIDDEN);
    // An unknown revision is a JSON 404.
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": Uuid::new_v4(), "filename": "a.bin", "size": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.json()["error"]["message"].is_string());

    // Check-in with one chunked file and one small file in the form.
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": rev_id, "filename": "results.op2", "size": content.len(),
                "content_type": "application/octet-stream" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let session = r.json();
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(session["revision_id"], rev_id.as_str());
    assert!(session["chunk_bytes"].as_u64().unwrap() > 0);
    let r = web_patch(&app, &cookie, &id, 0, &content[..12]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("upload-offset").as_deref(), Some("12"));
    // A repeated chunk (its answer was lost): 409 with the offset to go on from.
    let r = web_patch(&app, &cookie, &id, 0, &content[..12]).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.header("upload-offset").as_deref(), Some("12"));
    // Resume: the state says where to continue.
    let r = web_get(&app, &cookie, &id).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["offset"], 12);
    let r = web_patch(&app, &cookie, &id, 12, &content[12..]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["complete"], true);

    // The session is private to its user.
    let erika = app.login("erika").await;
    assert_eq!(
        web_get(&app, &erika, &id).await.status,
        StatusCode::NOT_FOUND
    );

    let uploads_field = json!([id]).to_string();
    let r = web_form(
        &app,
        &format!("/revisions/{rev_id}/checkin"),
        &cookie,
        &[("notes.txt", b"small file")],
        &[("comment", "first run"), ("uploads", &uploads_field)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let target = r.json()["location"].as_str().unwrap().to_string();
    assert!(target.starts_with("/revisions/"), "{target}");
    let stored = file_named(&app, &max, "results.op2").await;
    assert_eq!(stored["sha256"], sha(content));
    assert!(file_named(&app, &max, "notes.txt").await.is_object());
    let rev = app.api(Method::GET, REV, &max, None).await.json();
    assert!(
        rev["revision"]["checked_out_by"].is_null(),
        "the check-in released the lock"
    );
    // The finished session is gone (app.js then forgets its id).
    assert_eq!(
        web_get(&app, &cookie, &id).await.status,
        StatusCode::NOT_FOUND
    );

    // Not checked out: no new session.
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": rev_id, "filename": "results.op2", "size": 4 }),
    )
    .await;
    assert!(r.status.is_client_error(), "{}", r.status);

    // Attach (the files form) with only a chunked file: version 2.
    let r = app
        .api(Method::POST, &format!("{REV}/checkout"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let next = b"second run";
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": rev_id, "filename": "results.op2", "size": next.len() }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        web_patch(&app, &cookie, &id, 0, next).await.status,
        StatusCode::OK
    );
    let uploads_field = json!([id]).to_string();
    let r = web_form(
        &app,
        &format!("/revisions/{rev_id}/files"),
        &cookie,
        &[],
        &[("comment", "second run"), ("uploads", &uploads_field)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let stored = file_named(&app, &max, "results.op2").await;
    assert_eq!(stored["version"], 2);
    assert_eq!(stored["sha256"], sha(next));

    // Cancel: the session and its staged chunks are removed.
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": rev_id, "filename": "big.bin", "size": 8 }),
    )
    .await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    web_patch(&app, &cookie, &id, 0, b"1234").await;
    let request = web_request(Method::DELETE, &format!("/uploads/{id}"), Some(&cookie))
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.send(request).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
        web_get(&app, &cookie, &id).await.status,
        StatusCode::NOT_FOUND
    );
    let vault = app.vault().await;
    let uuid = Uuid::parse_str(&id).unwrap();
    assert!(!vault.uploads_dir().join(uuid.simple().to_string()).exists());
    app.teardown().await;
}

/// The web routes check like the check-in: other sites, viewers, users
/// without the check-out, restricted groups and foreign session ids.
#[tokio::test]
async fn web_uploads_check_origin_rights_and_ownership() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.user_in("vic", Role::Viewer, &["ROVER"]).await;
    app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    item(&app, &max).await;
    let rev = app.api(Method::GET, REV, &max, None).await.json();
    let rev_id = rev["revision"]["id"].as_str().unwrap().to_string();
    let cookie = app.login("max").await;
    let start = json!({ "revision_id": rev_id, "filename": "big.bin", "size": 8 });

    let r = web_start(&app, &cookie, start.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();

    // Another site cannot append to or cancel the session with the cookie.
    for method in [Method::PATCH, Method::DELETE] {
        let request = Request::builder()
            .method(method.clone())
            .uri(format!("/uploads/{id}"))
            .header(header::HOST, "localhost:8080")
            .header(header::ORIGIN, "https://evil.example")
            .header(header::COOKIE, &cookie)
            .header("Upload-Offset", "0")
            .body(Body::from(b"evil".to_vec()))
            .unwrap();
        assert_eq!(
            app.send(request).await.status,
            StatusCode::FORBIDDEN,
            "cross-site {method}"
        );
    }
    let r = web_get(&app, &cookie, &id).await;
    assert_eq!(r.json()["offset"], 0, "nothing was appended");

    // A viewer and an editor without the check-out cannot start uploads.
    for who in ["vic", "erika"] {
        let c = app.login(who).await;
        let r = web_start(&app, &c, start.clone()).await;
        assert!(
            r.status == StatusCode::FORBIDDEN || r.status == StatusCode::CONFLICT,
            "{who}: {} {}",
            r.status,
            r.text()
        );
    }

    // A foreign session id does nothing: not readable, not appendable, not
    // cancellable, and it cannot join another revision's check-in.
    let erika = app.login("erika").await;
    assert_eq!(
        web_get(&app, &erika, &id).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        web_patch(&app, &erika, &id, 0, b"1234").await.status,
        StatusCode::NOT_FOUND
    );
    let request = web_request(Method::DELETE, &format!("/uploads/{id}"), Some(&erika))
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.send(request).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        web_patch(&app, &cookie, &id, 0, b"12345678").await.status,
        StatusCode::OK,
        "the owner goes on"
    );
    let (_, erika_token) = app.user_in("erika2", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &erika_token,
            Some(json!({ "item_type": "part", "name": "Other part", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let other = r.json();
    let other_number = other["item_number"].as_str().unwrap().to_string();
    let other_rev_id = other["id"].as_str().unwrap().to_string();
    let erika2 = app.login("erika2").await;
    let uploads_field = json!([id]).to_string();
    let r = web_form(
        &app,
        &format!("/revisions/{other_rev_id}/checkin"),
        &erika2,
        &[],
        &[("uploads", &uploads_field)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let files = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{other_number}/revisions/A/files"),
            &erika_token,
            None,
        )
        .await
        .json();
    assert_eq!(files.as_array().unwrap().len(), 0, "{files}");
    let rev = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{other_number}/revisions/A"),
            &erika_token,
            None,
        )
        .await
        .json();
    assert!(
        !rev["revision"]["checked_out_by"].is_null(),
        "the failed check-in changed nothing"
    );
    let r = web_get(&app, &cookie, &id).await;
    assert_eq!(r.status, StatusCode::OK, "max's session is untouched");
    assert_eq!(r.json()["complete"], true);

    // A restricted group: outsiders get 404, as on the revision page.
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let otto = app.login("otto").await;
    let r = web_start(&app, &otto, start.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert_eq!(
        web_get(&app, &otto, &id).await.status,
        StatusCode::NOT_FOUND
    );
    // The member still can.
    let r = web_start(
        &app,
        &cookie,
        json!({ "revision_id": rev_id, "filename": "more.bin", "size": 4 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    app.teardown().await;
}
