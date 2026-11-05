//! Functional tests of the item list: filters by type and state (of the
//! latest revision), paging with totals, LIKE wildcards in the query taken
//! literally, and refused unknown filter values.

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

fn numbers(r: &common::Response) -> Vec<String> {
    let mut out: Vec<String> = r.json()["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", r.text()))
        .iter()
        .map(|i| i["number"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn filters_paging_and_literal_wildcards() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["SHOP"]).await;
    let (_, reviewer) = app.user("rolf", Role::Editor).await;
    let mut parts = Vec::new();
    for name in ["Clamp 100% steel", "Clamp 10x steel", "Clamp_a", "Clampa"] {
        parts.push(
            app.create_item(
                &token,
                json!({ "item_type": "part", "name": name, "project": "SHOP" }),
            )
            .await,
        );
    }
    let tool = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Clamp tool", "project": "SHOP" }),
        )
        .await;
    app.release(&token, ("rolf", &reviewer), &[&parts[0]]).await;

    // by type
    let r = app
        .api(Method::GET, "/api/v1/items?type=assembly", &token, None)
        .await;
    assert_eq!(numbers(&r), std::slice::from_ref(&tool));
    let r = app
        .api(Method::GET, "/api/v1/items?type=part&q=clamp", &token, None)
        .await;
    assert_eq!(r.json()["total"], 4);

    // by the state of the latest revision
    let r = app
        .api(Method::GET, "/api/v1/items?state=released", &token, None)
        .await;
    assert_eq!(numbers(&r), [parts[0].clone()]);
    let r = app
        .api(Method::GET, "/api/v1/items?state=in_work", &token, None)
        .await;
    assert_eq!(r.json()["total"], 4);
    // after a new revision the item is in work again
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{}/revisions/A/revise", parts[0]),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/items?state=released", &token, None)
        .await;
    assert_eq!(r.json()["total"], 0, "{}", r.text());

    // % and _ are no wildcards in a search
    let r = app
        .api(Method::GET, "/api/v1/items?q=100%25", &token, None)
        .await;
    assert_eq!(numbers(&r), [parts[0].clone()], "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/items?q=clamp_a", &token, None)
        .await;
    assert_eq!(numbers(&r), [parts[2].clone()], "{}", r.text());

    // paging keeps the total and never repeats an item
    let mut seen = Vec::new();
    for offset in [0, 2, 4] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/items?q=clamp&limit=2&offset={offset}"),
                &token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["total"], 5);
        assert_eq!(r.json()["limit"], 2);
        assert_eq!(r.json()["offset"], offset);
        seen.extend(numbers(&r));
    }
    seen.sort();
    let mut all = parts.clone();
    all.push(tool.clone());
    all.sort();
    assert_eq!(seen, all);
    let r = app
        .api(
            Method::GET,
            "/api/v1/items?limit=100000&offset=-5",
            &token,
            None,
        )
        .await;
    assert_eq!(r.json()["limit"], 500, "the limit is capped");
    assert_eq!(r.json()["offset"], 0, "negative offsets start at the top");

    // unknown filter values are refused, not ignored
    for query in ["type=gizmo", "state=frozen", "system=nonsense"] {
        let r = app
            .api(Method::GET, &format!("/api/v1/items?{query}"), &token, None)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}: {}", r.text());
    }
    let r = app
        .api(Method::GET, "/api/v1/items?project=NOPE", &token, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    app.teardown().await;
}
