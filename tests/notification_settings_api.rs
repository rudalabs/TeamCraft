//! The notification settings over REST – what the desktop app's settings
//! dialog uses: the form description (kinds, channels) comes with the values,
//! and a saved change is read back.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

#[tokio::test]
async fn settings_describe_the_form_and_round_trip() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, token) = app.user("ed", Role::Editor).await;

    let r = app
        .api(Method::GET, "/api/v1/notifications/settings", &token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s = r.json();
    let kinds = s["kinds"].as_array().expect("kinds");
    assert!(!kinds.is_empty());
    assert!(
        kinds.iter().all(|k| k["on"] == json!(true)),
        "nothing muted yet"
    );
    assert_eq!(s["email_enabled"], json!(false));
    assert_eq!(s["mattermost_enabled"], json!(false));
    // e-mail notifications are on for a new user (users.notify_email DEFAULT TRUE)
    assert_eq!(s["email"], json!(true));

    let first = kinds[0]["value"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::PUT,
            "/api/v1/notifications/settings",
            &token,
            Some(json!({ "email": false, "mattermost": false,
                         "mattermost_username": "ed.m", "muted": [first] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let s = app
        .api(Method::GET, "/api/v1/notifications/settings", &token, None)
        .await
        .json();
    assert_eq!(s["email"], json!(false), "e-mail switched off by the PUT");
    assert_eq!(s["mattermost"], json!(false));
    assert_eq!(s["mattermost_username"], json!("ed.m"));
    let k = s["kinds"].as_array().unwrap();
    assert_eq!(k[0]["on"], json!(false));
    assert!(k[1..].iter().all(|k| k["on"] == json!(true)));

    // a kind that cannot be muted is refused
    let r = app
        .api(
            Method::PUT,
            "/api/v1/notifications/settings",
            &token,
            Some(json!({ "email": false, "mattermost": false, "muted": ["no-such-kind"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    app.teardown().await;
}
