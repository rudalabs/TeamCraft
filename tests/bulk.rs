//! Bulk lifecycle operations (`POST /api/v1/items/bulk`): each item goes
//! through the single operation, refusals do not stop the others.

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

#[tokio::test]
async fn revise_and_obsolete_many_items() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, eva) = app.user_in("eva", Role::Editor, &["ROVER"]).await;
    let mut numbers = Vec::new();
    for name in ["Bracket", "Plate", "Shaft"] {
        numbers.push(
            app.create_item(
                &max,
                json!({ "item_type": "part", "name": name, "project": "ROVER" }),
            )
            .await,
        );
    }
    // the first two are released, the third stays in work
    for n in &numbers[..2] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{n}/revisions/A/checkin"),
                &max,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let released: Vec<&str> = numbers[..2].iter().map(String::as_str).collect();
    app.release(&max, ("eva", &eva), &released).await;

    let mut items: Vec<String> = numbers.clone();
    items.push("PRT-999999".into());
    let r = app
        .api(
            Method::POST,
            "/api/v1/items/bulk",
            &max,
            Some(json!({ "action": "revise", "items": items })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(
        (body["ok"].as_u64(), body["failed"].as_u64()),
        (Some(2), Some(2))
    );
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["code"], "B");
    assert_eq!(results[1]["code"], "B");
    assert_eq!(results[2]["ok"], false, "still in work: {}", results[2]);
    assert_eq!(results[3]["ok"], false, "unknown: {}", results[3]);

    // obsolete: the latest revisions are now in work, so it is refused per item
    let r = app
        .api(
            Method::POST,
            "/api/v1/items/bulk",
            &max,
            Some(json!({ "action": "obsolete", "items": [numbers[0]] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["failed"], 1);

    // bad requests
    for body in [
        json!({ "action": "release", "items": [numbers[0]] }),
        json!({ "action": "revise", "items": [] }),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/items/bulk", &max, Some(body))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    }
    app.teardown().await;
}
