//! Suggested reviewers: ranked from the history of the affected items, with
//! reasons; the author, deactivated users, viewers and assigned reviewers
//! are never suggested; editors of the items are listed as "cannot review". REST, MCP tool and the change page.

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

async fn item(app: &common::TestApp, token: &str, item_type: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn revision_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

/// Seeds `n` edits of a revision by a user in the audit log.
async fn edits(app: &common::TestApp, user: Uuid, revision: Uuid, n: usize, days_ago: i32) {
    for _ in 0..n {
        sqlx::query(
            "INSERT INTO audit_log (at, user_id, username, action, entity_type, entity_id, summary)
             VALUES (now() - make_interval(days => $3), $1, 'x', 'revision.checkin', 'revision', $2, 'seeded')",
        )
        .bind(user)
        .bind(revision)
        .bind(days_ago)
        .execute(&app.pool)
        .await
        .unwrap();
    }
}

fn usernames(v: &Value) -> Vec<String> {
    v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["username"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn reviewers_are_suggested_from_history() {
    let app = app_or_skip!();
    let (erika, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &[]).await;
    let (rolf, _) = app.user_in("rolf", Role::Editor, &[]).await;
    let (anna, _) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (_, olga_token) = app.user_in("olga", Role::Editor, &["ROCKET"]).await;
    let (carl, _) = app.user_in("carl", Role::Admin, &[]).await;
    let (vera, _) = app.user_in("vera", Role::Viewer, &[]).await;
    let (ida, _) = app.user_in("ida", Role::Editor, &[]).await;

    let part = item(&app, &author, "part", "Fin bracket").await;
    let part_rev = revision_id(&app, &part).await;
    // olga owns an assembly that uses the part (a dependent)
    let asm = item(&app, &olga_token, "assembly", "Fin can").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &olga_token,
            Some(json!({ "child": part, "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // history: max edited the part 3 times recently (and once long ago) and
    // so cannot review it (separation of duties); the viewer and the
    // deactivated user edited it more, the author too
    edits(&app, max.id, part_rev, 3, 5).await;
    edits(&app, max.id, part_rev, 1, 400).await;
    edits(&app, vera.id, part_rev, 6, 1).await;
    edits(&app, ida.id, part_rev, 6, 1).await;
    edits(&app, erika.id, part_rev, 9, 1).await;
    sqlx::query("UPDATE users SET active = false WHERE id = $1")
        .bind(ida.id)
        .execute(&app.pool)
        .await
        .unwrap();
    // anna manages ROCKET
    sqlx::query("UPDATE project_members SET manager = true WHERE user_id = $1")
        .bind(anna.id)
        .execute(&app.pool)
        .await
        .unwrap();
    // rolf approved an earlier change with the same part; ida (inactive) too
    let earlier = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO changes (id, number, title, status, created_by, closed_at)
         VALUES ($1, 'ECO-900001', 'Earlier', 'released', $2, now())",
    )
    .bind(earlier)
    .bind(erika.id)
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO change_items (change_id, revision_id) VALUES ($1, $2)")
        .bind(earlier)
        .bind(part_rev)
        .execute(&app.pool)
        .await
        .unwrap();
    for user in [rolf.id, ida.id] {
        sqlx::query(
            "INSERT INTO change_reviews (change_id, reviewer_id, decision, decided_at)
             VALUES ($1, $2, 'approved', now())",
        )
        .bind(earlier)
        .bind(user)
        .execute(&app.pool)
        .await
        .unwrap();
    }

    // the change under review
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(json!({ "title": "Stiffer fin bracket" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    let chg_id = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/items"),
            &author,
            Some(json!({ "item": part })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // carl raised the change request this change implements
    let request = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_requests (id, number, title, status, requested_by)
         VALUES ($1, 'ECR-900001', 'Fin flutter', 'accepted', $2)",
    )
    .bind(request)
    .bind(carl.id)
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO change_request_changes (request_id, change_id, linked_by)
         VALUES ($1, $2, $3)",
    )
    .bind(request)
    .bind(Uuid::parse_str(&chg_id).unwrap())
    .bind(erika.id)
    .execute(&app.pool)
    .await
    .unwrap();

    let get = |limit: Option<u32>| {
        let uri = match limit {
            Some(l) => format!("/api/v1/changes/{chg}/suggested-reviewers?limit={l}"),
            None => format!("/api/v1/changes/{chg}/suggested-reviewers"),
        };
        let app = &app;
        let author = author.clone();
        async move {
            let r = app.api(Method::GET, &uri, &author, None).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let v = get(None).await;
    assert_eq!(v["change"], chg.as_str());
    // ranked: earlier review (4) > manager (3) > dependent (2) > request (1);
    // max edited the affected revision: listed last, flagged, not to be added
    assert_eq!(usernames(&v), ["rolf", "anna", "olga", "carl", "max"]);
    assert_eq!(v["suggestions"][4]["cannot_review"], true);
    assert_eq!(v["suggestions"][0]["cannot_review"], false);
    let reasons = |i: usize| v["suggestions"][i]["reasons"].clone();
    assert_eq!(reasons(0), json!(["reviewed ECO-900001 (same items)"]));
    assert_eq!(reasons(1), json!(["manager of ROCKET"]));
    assert_eq!(
        reasons(2),
        json!([format!("owns {asm}, which uses {part}")])
    );
    assert_eq!(
        reasons(3),
        json!(["raised ECR-900001, implemented by this change"])
    );
    assert_eq!(v["suggestions"][0]["score"], 4);

    // limit, and a hostile limit is clamped
    assert_eq!(usernames(&get(Some(2)).await), ["rolf", "anna"]);
    assert_eq!(
        get(Some(4_000_000_000)).await["suggestions"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/changes/ECO-999999/suggested-reviewers",
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // the change page offers them as one-click Add buttons with the reason
    let cookie = app.login("erika").await;
    let page = app
        .get_page(&format!("/changes/{chg_id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("Suggested reviewers"), "{html}");
    assert!(html.contains("name=\"username\" value=\"rolf\""));
    assert!(html.contains("manager of ROCKET"));

    // MCP: read-only tool with the same ranking
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            &author,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": "suggest_reviewers", "arguments": { "change": chg, "limit": 3 } } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let result = r.json()["result"]["structuredContent"].clone();
    assert_eq!(usernames(&result), ["rolf", "anna", "olga"]);
    assert_eq!(result["suggestions"][0]["name"], "rolf (test)");
    assert_eq!(result["suggestions"][0]["cannot_review"], false);

    // someone who edited the revision cannot be assigned
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            &author,
            Some(json!({ "username": "max" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // an assigned reviewer drops out of the suggestions
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            &author,
            Some(json!({ "username": "rolf" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(usernames(&get(None).await), ["anna", "olga", "carl", "max"]);

    app.teardown().await;
}
