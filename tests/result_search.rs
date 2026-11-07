//! Result values in the item search (`result`, `result_name`) and the
//! dashboard card of simulations to check: filters on the latest revision's
//! values against their limits, and the read permissions of restricted
//! project groups.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// A simulation in `project` with these values `(name, value, lower, upper)`.
async fn simulation(
    app: &common::TestApp,
    token: &str,
    project: &str,
    name: &str,
    values: &[(&str, f64, Option<f64>, Option<f64>)],
) -> String {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "simulation", "name": name, "project": project }),
        )
        .await;
    for (value_name, value, lower, upper) in values {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{number}/revisions/A/simulation/results"),
                token,
                Some(json!({ "name": value_name, "value": value,
                             "lower_limit": lower, "upper_limit": upper })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    number
}

async fn search(app: &common::TestApp, token: &str, query: &str) -> (Vec<String>, i64) {
    let r = app
        .api(Method::GET, &format!("/api/v1/items?{query}"), token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.text());
    let body: Value = r.json();
    let mut numbers: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_str().unwrap().to_string())
        .collect();
    numbers.sort();
    (numbers, body["total"].as_i64().unwrap())
}

#[tokio::test]
async fn the_item_search_filters_by_result_values() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let over = simulation(
        &app,
        &eng,
        "ROVER",
        "Over",
        &[("Max stress", 300.0, None, Some(250.0))],
    )
    .await;
    let ok = simulation(
        &app,
        &eng,
        "ROVER",
        "Fine",
        &[("Max stress", 100.0, None, Some(250.0))],
    )
    .await;
    let under = simulation(
        &app,
        &eng,
        "ROVER",
        "Under",
        &[("Safety factor", 1.2, Some(1.5), None)],
    )
    .await;
    let free = simulation(&app, &eng, "ROVER", "Free", &[("Mass", 12.0, None, None)]).await;
    app.create_item(
        &eng,
        json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
    )
    .await;

    macro_rules! q {
        ($s:expr) => {
            search(&app, &eng, $s)
        };
    }
    assert_eq!(q!("result=failed").await.0, [over.clone(), under.clone()]);
    assert_eq!(q!("result=failed").await.1, 2);
    assert_eq!(q!("result=passed").await.0, std::slice::from_ref(&ok));
    assert_eq!(
        q!("result=above_limit").await.0,
        std::slice::from_ref(&over)
    );
    assert_eq!(
        q!("result=below_limit").await.0,
        std::slice::from_ref(&under)
    );
    // by name, with and without a state; LIKE wildcards are literal
    let mut stress = vec![over.clone(), ok.clone()];
    stress.sort();
    assert_eq!(q!("result_name=stress").await.0, stress);
    assert_eq!(
        q!("result_name=stress&result=failed").await.0,
        std::slice::from_ref(&over)
    );
    assert_eq!(q!("result_name=mass").await.0, std::slice::from_ref(&free));
    assert!(q!("result_name=%25").await.0.is_empty());
    // combined with other filters
    assert!(q!("result=failed&type=part").await.0.is_empty());
    assert_eq!(
        q!("result=failed&q=Over").await.0,
        std::slice::from_ref(&over)
    );
    // a page behind the last one still reports the total
    let (found, total) = search(&app, &eng, "result=failed&limit=1&offset=5").await;
    assert!(found.is_empty());
    assert_eq!(total, 2);
    // unknown filter
    let r = app
        .api(Method::GET, "/api/v1/items?result=nope", &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    app.teardown().await;
}

#[tokio::test]
async fn restricted_groups_stay_out_of_result_search_and_dashboard() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, member) = app.user_in("mira", Role::Editor, &["ROVER"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let secret = simulation(
        &app,
        &member,
        "ROVER",
        "Secret duct",
        &[("Pressure drop", 900.0, None, Some(500.0))],
    )
    .await;
    let open = simulation(
        &app,
        &outsider,
        "ROCKET",
        "Open duct",
        &[("Pressure drop", 900.0, None, Some(500.0))],
    )
    .await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    for query in [
        "result=failed",
        "result_name=pressure",
        "result=above_limit&result_name=pressure",
    ] {
        let (found, total) = search(&app, &outsider, query).await;
        assert_eq!(found, std::slice::from_ref(&open), "outsider {query}");
        assert_eq!(total, 1, "the total must not count the restricted group");
        let (found, total) = search(&app, &outsider, &format!("{query}&limit=1&offset=5")).await;
        assert!(found.is_empty());
        assert_eq!(total, 1, "the count query must skip the restricted group");
        let (found, _) = search(&app, &member, query).await;
        let mut both = vec![open.clone(), secret.clone()];
        both.sort();
        assert_eq!(found, both, "member {query}");
    }

    // the dashboard card lists what the user may read
    let cookie = app.login("otto").await;
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(page.contains(&open) && page.contains("Simulations to check"));
    assert!(!page.contains(&secret) && !page.contains("Secret duct"));
    let cookie = app.login("mira").await;
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(page.contains(&open) && page.contains(&secret));
    app.teardown().await;
}

#[tokio::test]
async fn the_dashboard_card_lists_failed_and_outdated_simulations() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let cookie = app.login("eng").await;
    let fine = simulation(
        &app,
        &eng,
        "ROVER",
        "Fine duct",
        &[("Pressure drop", 100.0, None, Some(500.0))],
    )
    .await;
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(!page.contains("Simulations to check"), "nothing to check");
    let failing = simulation(
        &app,
        &eng,
        "ROVER",
        "Failing duct",
        &[("Pressure drop", 900.0, None, Some(500.0))],
    )
    .await;
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(page.contains("Simulations to check") && page.contains(&failing));
    assert!(page.contains("1 of 1 failed"));
    // other dashboard cards (recent items) may list the fine simulation; look at the card only
    let card = page
        .split("id=\"simulations\"")
        .nth(1)
        .and_then(|rest| rest.split("</section>").next())
        .expect("the simulations card");
    assert!(card.contains(&failing) && !card.contains(&fine));
    app.teardown().await;
}
