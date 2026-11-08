//! Likely duplicate items by name (`GET /api/v1/items/duplicates`): similar
//! names are found, other names and hidden groups are not.

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
async fn similar_names_are_offered() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    for name in ["Bracket left M8", "Gearbox housing", "Motor mount plate"] {
        app.create_item(
            &max,
            json!({ "item_type": "part", "name": name, "project": "ROVER" }),
        )
        .await;
    }
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/duplicates?name=M8%20bracket%20left&type=part",
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let found = r.json();
    let names: Vec<&str> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Bracket left M8"]);
    assert_eq!(found[0]["score"], 1.0);

    // another type, or a name without a similar one: nothing
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/duplicates?name=Bracket%20left%20M8&type=document",
            &max,
            None,
        )
        .await;
    assert_eq!(r.json(), json!([]));
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/duplicates?name=Conveyor%20belt",
            &max,
            None,
        )
        .await;
    assert_eq!(r.json(), json!([]));
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/duplicates?name=x&type=spaceship",
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    app.teardown().await;
}
