//! Offboarding (issue #173, step 1): the report of what a user still holds.
//!
//! It is filtered by the reader's rights, never the leaver's: a check-out in
//! a group the leaver was removed from is still listed. Administrators see
//! every group they can read and only a count for a group that hides its
//! content from them; a manager sees the groups they manage and no tokens;
//! everybody else is refused.

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

/// Checks the newest revision of `item` out to `username` (as the database
/// would hold it after a check-out).
async fn hold(app: &common::TestApp, item: &str, username: &str) {
    let n = sqlx::query(
        "UPDATE revisions SET checked_out_by = (SELECT id FROM users WHERE username = $2),
                checked_out_at = now() - interval '40 days'
         WHERE item_id = (SELECT id FROM items WHERE number = $1)",
    )
    .bind(item)
    .bind(username)
    .execute(&app.pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(n, 1, "one revision of {item}");
}

async fn holdings(app: &common::TestApp, token: &str, user: uuid::Uuid) -> common::Response {
    app.api(
        Method::GET,
        &format!("/api/v1/users/{user}/holdings"),
        token,
        None,
    )
    .await
}

async fn part(app: &common::TestApp, token: &str, name: &str, project: &str) -> String {
    app.create_item(
        token,
        json!({ "item_type": "part", "name": name, "project": project }),
    )
    .await
}

#[tokio::test]
async fn an_administrator_sees_one_of_each_kind() {
    let app = app_or_skip!();
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (_, tom) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let upright = part(&app, &lena_token, "Upright", "ROCKET").await;
    let hub = part(&app, &lena_token, "Hub", "ROCKET").await;
    hold(&app, &upright, "lena").await;

    // a draft change order of hers, and one of tom's that waits for her
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &lena_token,
            Some(json!({ "title": "Stiffer upright" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let hers = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &tom,
            Some(json!({ "title": "Lighter hub" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let his = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{his}/reviewers"),
            &tom,
            Some(json!({ "username": "lena" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let r = holdings(&app, &root, lena.id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h = r.json();
    assert_eq!(h["username"], "lena");
    assert_eq!(h["scope"], "all");
    // a new item's first revision is checked out to its creator: both parts,
    // the older lock first
    assert_eq!(h["checkouts"]["total"], 2, "{h}");
    assert_eq!(h["checkouts"]["shown"][0]["item_number"], upright.as_str());
    assert_eq!(h["checkouts"]["shown"][1]["item_number"], hub.as_str());
    assert_eq!(h["checkouts"]["shown"][0]["project_code"], "ROCKET");
    assert!(h["checkouts"]["shown"][0]["checked_out_at"].is_string());
    assert_eq!(h["authored_changes"]["total"], 1, "{h}");
    assert_eq!(h["authored_changes"]["shown"][0]["number"], hers.as_str());
    assert_eq!(h["pending_reviews"]["total"], 1, "{h}");
    assert_eq!(h["pending_reviews"]["shown"][0]["number"], his.as_str());
    // she created (and so owns) both parts
    assert_eq!(h["owned_items"]["total"], 2, "{h}");
    let owned: Vec<&str> = h["owned_items"]["shown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_str().unwrap())
        .collect();
    assert!(owned.contains(&hub.as_str()));
    assert_eq!(h["memberships"][0]["project_code"], "ROCKET");
    assert_eq!(h["memberships"][0]["only_active_manager"], false);
    // the token the fixture made for her: name and scope, never the value
    let tokens = h["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(!r.text().contains(&lena_token));

    // an unknown user
    let r = holdings(&app, &root, uuid::Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // the page renders for the administrator
    let cookie = app.login("root").await;
    let page = app
        .get_page(&format!("/users/{}/holdings", lena.id), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(page.text().contains(&upright) && page.text().contains("Stiffer upright"));
    app.teardown().await;
}

#[tokio::test]
async fn a_lock_the_leaver_can_no_longer_see_is_still_listed() {
    let app = app_or_skip!();
    let (lena, lena_token) = app.user("lena", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &lena_token,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let secret = part(&app, &lena_token, "Secret part", "NTK").await;
    hold(&app, &secret, "lena").await;
    // sole manager of the group: flagged
    let h = holdings(&app, &root, lena.id).await.json();
    assert_eq!(h["memberships"][0]["project_code"], "NTK");
    assert_eq!(h["memberships"][0]["manager"], true);
    assert_eq!(h["memberships"][0]["only_active_manager"], true, "{h}");

    // she is removed from the group: the lock is orphaned, she cannot see it
    sqlx::query("DELETE FROM project_members WHERE user_id = $1")
        .bind(lena.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        app.api(
            Method::GET,
            &format!("/api/v1/items/{secret}"),
            &lena_token,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let h = holdings(&app, &root, lena.id).await.json();
    assert_eq!(h["checkouts"]["total"], 1, "{h}");
    assert_eq!(h["checkouts"]["shown"][0]["item_number"], secret.as_str());
    assert_eq!(h["memberships"], json!([]));
    app.teardown().await;
}

#[tokio::test]
async fn a_manager_sees_only_the_groups_they_manage() {
    let app = app_or_skip!();
    let (lena, lena_token) = app
        .user_in("lena", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let (tom, tom_token) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (_, plain) = app.user_in("paula", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let rocket = part(&app, &lena_token, "Fin", "ROCKET").await;
    let rover = part(&app, &lena_token, "Wheel", "ROVER").await;
    hold(&app, &rocket, "lena").await;
    hold(&app, &rover, "lena").await;
    sqlx::query(
        "UPDATE project_members SET manager = TRUE
         WHERE user_id = $1 AND project_id = (SELECT id FROM projects WHERE code = 'ROCKET')",
    )
    .bind(tom.id)
    .execute(&app.pool)
    .await
    .unwrap();

    let r = holdings(&app, &tom_token, lena.id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h = r.json();
    assert_eq!(h["scope"], "managed");
    assert_eq!(h["checkouts"]["total"], 1, "{h}");
    assert_eq!(h["checkouts"]["shown"][0]["item_number"], rocket.as_str());
    assert!(!r.text().contains(&rover), "the other group stays out");
    assert_eq!(h["owned_items"]["total"], 1);
    let groups: Vec<&str> = h["memberships"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["project_code"].as_str().unwrap())
        .collect();
    assert_eq!(groups, ["ROCKET"]);
    assert!(h["tokens"].is_null(), "tokens are for administrators");

    // a member who manages nothing, and a viewer
    for token in [&plain, &viewer] {
        assert_eq!(
            holdings(&app, token, lena.id).await.status,
            StatusCode::FORBIDDEN
        );
    }
    // the manager's page
    let cookie = app.login("tom").await;
    let page = app
        .get_page(&format!("/users/{}/holdings", lena.id), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(page.text().contains(&rocket) && !page.text().contains(&rover));
    app.teardown().await;
}

#[tokio::test]
async fn a_group_closed_to_administrators_gives_only_a_count() {
    let app = app_or_skip!();
    let (erika, erika_token) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika_token,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let secret = part(&app, &erika_token, "Secret part", "NTK").await;
    hold(&app, &secret, "erika").await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &erika_token,
            Some(json!({ "admin_read": "none" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let r = holdings(&app, &root, erika.id).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let h: Value = r.json();
    assert_eq!(h["checkouts"]["total"], 0, "{h}");
    assert_eq!(
        h["hidden_checkouts"],
        json!([{ "project_code": "NTK", "count": 1 }])
    );
    assert_eq!(h["owned_items"]["total"], 0);
    assert!(
        !r.text().contains(&secret) && !r.text().contains("Secret part"),
        "no item number or name of the closed group: {}",
        r.text()
    );
    // the manager of the group reads it in full
    let h = holdings(&app, &erika_token, erika.id).await.json();
    assert_eq!(h["checkouts"]["shown"][0]["item_number"], secret.as_str());
    app.teardown().await;
}
