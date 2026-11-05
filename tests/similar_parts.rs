//! Similar part search: shape signatures of uploaded meshes, ranking,
//! visibility (the item search's: every signed-in user, no items of
//! restricted groups for non-members, one hit per item, its latest
//! revision), REST endpoint, MCP tool and revision page.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
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

/// Binary STL of an axis-aligned box, optionally turned about z.
fn box_stl(sx: f32, sy: f32, sz: f32, turn: f32) -> Vec<u8> {
    let corner = |i: usize| -> [f32; 3] {
        let p = [
            if i & 1 == 0 { 0.0 } else { sx },
            if i & 2 == 0 { 0.0 } else { sy },
            if i & 4 == 0 { 0.0 } else { sz },
        ];
        let (s, c) = turn.sin_cos();
        [c * p[0] - s * p[1], s * p[0] + c * p[1], p[2]]
    };
    let quads = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let mut out = vec![0u8; 80];
    out.extend_from_slice(&12u32.to_le_bytes());
    for q in quads {
        for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            out.extend_from_slice(&[0u8; 12]); // normal
            for v in tri {
                for x in corner(v) {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            }
            out.extend_from_slice(&[0, 0]);
        }
    }
    out
}

async fn part(app: &common::TestApp, token: &str, project: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": name, "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn upload(app: &common::TestApp, token: &str, number: &str, filename: &str, data: &[u8]) {
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            token,
            filename,
            data,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn similar(app: &common::TestApp, token: &str, number: &str, query: &str) -> Value {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/revisions/latest/similar?wait=60{query}"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn numbers(report: &Value) -> Vec<String> {
    report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["item_number"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn similar_parts_by_shape() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;

    let plate = part(&app, &max, "ROVER", "Plate").await;
    upload(
        &app,
        &max,
        &plate,
        "plate.stl",
        &box_stl(120.0, 40.0, 8.0, 0.0),
    )
    .await;
    // nearly the same plate, turned, in another project group
    let near = part(&app, &erika, "ROCKET", "Old plate").await;
    upload(
        &app,
        &erika,
        &near,
        "old-plate.stl",
        &box_stl(118.0, 40.0, 8.0, 0.6),
    )
    .await;
    // the identical file in a third item
    let copy = part(&app, &erika, "ROCKET", "Copied plate").await;
    upload(
        &app,
        &erika,
        &copy,
        "copy.stl",
        &box_stl(120.0, 40.0, 8.0, 0.0),
    )
    .await;
    // twice the size: only found when scale is ignored
    let big = part(&app, &max, "ROVER", "Big plate").await;
    upload(
        &app,
        &max,
        &big,
        "big.stl",
        &box_stl(240.0, 80.0, 16.0, 0.0),
    )
    .await;
    // a cube: other proportions, never similar
    let cube = part(&app, &max, "ROVER", "Cube").await;
    upload(
        &app,
        &max,
        &cube,
        "cube.stl",
        &box_stl(50.0, 50.0, 50.0, 0.0),
    )
    .await;
    // an item with two plate files is listed once
    let twice = part(&app, &max, "ROVER", "Two plates").await;
    upload(&app, &max, &twice, "a.stl", &box_stl(121.0, 40.0, 8.0, 0.0)).await;
    upload(&app, &max, &twice, "b.stl", &box_stl(119.0, 41.0, 8.0, 0.0)).await;
    // no 3D file at all
    let doc = part(&app, &max, "ROVER", "Notes only").await;
    upload(&app, &max, &doc, "notes.txt", b"just text").await;

    // Make sure every signature exists (computed after the uploads in the
    // background; the endpoint waits for the one it needs).
    for number in [&near, &copy, &big, &cube, &twice] {
        let r = similar(&app, &max, number, "").await;
        assert_eq!(r["status"], "ready", "{number}: {r}");
    }
    let mut twice_files = Vec::new();
    for name in ["a.stl", "b.stl"] {
        let r = similar(&app, &max, &twice, &format!("&file={name}")).await;
        assert_eq!(r["source"]["filename"], name);
        twice_files.push(r);
    }

    let r = similar(&app, &max, &plate, "").await;
    assert_eq!(r["status"], "ready", "{r}");
    assert_eq!(r["item"], plate.as_str());
    assert_eq!(r["source"]["filename"], "plate.stl");
    let extents = r["signature"]["extents"].as_array().unwrap();
    assert!((extents[0].as_f64().unwrap() - 120.0).abs() < 0.01, "{r}");
    assert_eq!(r["signature"]["closed"], true);
    let found = numbers(&r);
    assert_eq!(found[0], copy, "the identical copy first: {r}");
    assert_eq!(r["candidates"][0]["identical"], true);
    assert_eq!(r["candidates"][0]["score"], 100);
    assert!(found.contains(&near) && found.contains(&twice), "{r}");
    assert!(!found.contains(&plate), "never the part itself");
    assert!(!found.contains(&cube) && !found.contains(&big), "{r}");
    assert_eq!(
        found.iter().filter(|n| **n == twice).count(),
        1,
        "one hit per item"
    );
    let near_hit = r["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["item_number"] == near.as_str())
        .unwrap();
    assert!(near_hit["score"].as_u64().unwrap() >= 90, "{near_hit}");
    assert_eq!(near_hit["project_code"], "ROCKET");
    let why = near_hit["explanation"].as_str().unwrap();
    assert!(
        why.starts_with("extents 118 × 40 × 8 vs 120 × 40 × 8") && why.contains("% similar"),
        "{why}"
    );

    // Visibility as in the search: a viewer outside every project group
    // gets the same hits.
    let as_viewer = similar(&app, &vera, &plate, "").await;
    assert_eq!(numbers(&as_viewer), found);

    // Ignoring scale finds the big plate.
    let r = similar(&app, &max, &plate, "&ignore_scale=true").await;
    assert_eq!(r["ignore_scale"], true);
    assert!(numbers(&r).contains(&big), "{r}");
    assert!(!numbers(&r).contains(&cube), "{r}");

    // limit
    let r = similar(&app, &max, &plate, "&limit=1").await;
    assert_eq!(numbers(&r), vec![copy.clone()]);

    // Only the latest revision of an item counts: a revision B of the near
    // plate whose file is a cube hides its plate revision A.
    let item_id: Uuid = sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(&near)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let cube_file: (String, Uuid) = sqlx::query_as(
        "SELECT f.sha256, f.uploaded_by FROM files f JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&cube)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE revisions SET state = 'released', checked_out_by = NULL, checked_out_at = NULL WHERE item_id = $1")
        .bind(item_id)
        .execute(&app.pool)
        .await
        .unwrap();
    let rev_b = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO revisions (id, item_id, seq, code, state, name, created_by)
         VALUES ($1, $2, 2, 'B', 'in_work', 'Old plate, now a cube', $3)",
    )
    .bind(rev_b)
    .bind(item_id)
    .bind(cube_file.1)
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by, format)
         VALUES ($1, $2, 'cube.stl', 'model/stl', 684, $3, $4, 'stl')",
    )
    .bind(Uuid::new_v4())
    .bind(rev_b)
    .bind(&cube_file.0)
    .bind(cube_file.1)
    .execute(&app.pool)
    .await
    .unwrap();
    let r = similar(&app, &max, &plate, "").await;
    assert!(
        !numbers(&r).contains(&near),
        "older revisions are not candidates: {r}"
    );

    // No 3D file: a status, not an error.
    let r = similar(&app, &max, &doc, "").await;
    assert_eq!(r["status"], "no_shape_file", "{r}");
    assert!(r["message"].as_str().unwrap().contains("STEP"));
    assert_eq!(r["candidates"], json!([]));
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{plate}/revisions/A/similar?file=missing.stl"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{plate}/revisions/A/similar"),
            "tc_pat_invalid",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // MCP tool (read-only) with an item/revision reference.
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            &vera,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "find_similar_parts", "arguments": { "item": format!("{plate}/A"), "limit": 5 } } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let result = &r.json()["result"];
    assert_ne!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["status"], "ready", "{v}");
    assert_eq!(v["source"], format!("{plate}/A"));
    assert_eq!(v["similar"][0]["item"], copy.as_str());
    assert!(v["similar"][0]["explanation"].is_string());

    // The revision page lists them.
    let cookie = app.login("max").await;
    let rev_id: Uuid = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&plate)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let page = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("id=\"similar\""), "section shown");
    assert!(html.contains("Similar existing parts"), "new part wording");
    assert!(html.contains(&format!("{copy}/A")), "hit listed");
    assert!(html.contains("identical file content"));

    app.teardown().await;
}

/// An item of a restricted group is never a similar part for non-members –
/// not even with identical file content – in REST, MCP and the revision
/// page; its members still find it.
#[tokio::test]
async fn restricted_groups_stay_out_of_similar_parts() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, member) = app.user_in("mira", Role::Editor, &["ROVER"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;

    let shape = box_stl(60.0, 30.0, 5.0, 0.0);
    let visible = part(&app, &outsider, "ROCKET", "Bracket").await;
    upload(&app, &outsider, &visible, "bracket.stl", &shape).await;
    let hidden = part(&app, &member, "ROVER", "Secret bracket").await;
    upload(&app, &member, &hidden, "secret-bracket.stl", &shape).await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // the member sees the identical copy in the restricted group
    let report = similar(&app, &member, &visible, "").await;
    assert_eq!(report["status"], "ready", "{report}");
    assert!(numbers(&report).contains(&hidden), "member: {report}");

    // the outsider does not: neither the item nor its file
    let report = similar(&app, &outsider, &visible, "").await;
    assert_eq!(report["status"], "ready", "{report}");
    let text = report.to_string();
    for marker in [hidden.as_str(), "Secret bracket", "secret-bracket.stl"] {
        assert!(!text.contains(marker), "REST shows {marker}: {text}");
    }
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            &outsider,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": "find_similar_parts", "arguments": { "item": visible } } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for marker in [hidden.as_str(), "Secret bracket", "secret-bracket.stl"] {
        assert!(
            !r.text().contains(marker),
            "MCP shows {marker}: {}",
            r.text()
        );
    }
    let cookie = app.login("otto").await;
    let rev_id: Uuid = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&visible)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let page = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    for marker in [hidden.as_str(), "Secret bracket", "secret-bracket.stl"] {
        assert!(
            !page.text().contains(marker),
            "revision page shows {marker}"
        );
    }
    // and the hidden item itself is not found
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{hidden}/revisions/latest/similar?wait=0"),
            &outsider,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());

    app.teardown().await;
}
