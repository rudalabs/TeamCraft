//! Preferred CAD program (account setting): stored per user, exposed on
//! `GET /api/v1/me`, and on the revision page and in the vault explorer the
//! native files of other CAD programs are listed in a collapsed `<details>`
//! – only when the revision has a file of the preferred program. Display
//! only: the API file list stays complete.

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

const ORIGIN: Option<&str> = Some("http://localhost:8080");
const STEP: &[u8] = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";

/// A part with files of the given names; returns its number and revision id.
async fn part_with_files(
    app: &common::TestApp,
    token: &str,
    name: &str,
    files: &[(&str, &[u8])],
) -> (String, String) {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "part", "name": name, "project": "ROVER" }),
        )
        .await;
    let rev = format!("/api/v1/items/{number}/revisions/A");
    for (filename, content) in files {
        let r = app
            .api_upload(&format!("{rev}/files"), token, filename, content)
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{filename}: {}", r.text());
    }
    let id = app.api(Method::GET, &rev, token, None).await.json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    (number, id)
}

async fn set_preference(app: &common::TestApp, cookie: &str, family: &str) -> common::Response {
    app.post_form(
        "/account/preferred-cad",
        Some(cookie),
        ORIGIN,
        &[("preferred_cad", family)],
    )
    .await
}

#[tokio::test]
async fn the_setting_is_stored_per_user_and_exposed_on_me() {
    let app = app_or_skip!();
    let (_, max_token) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, eva_token) = app.user_in("eva", Role::Editor, &["ROVER"]).await;
    let max = app.login("max").await;
    let me = |token: String| {
        let app = &app;
        async move {
            app.api(Method::GET, "/api/v1/me", &token, None)
                .await
                .json()
        }
    };
    assert_eq!(
        me(max_token.clone()).await["preferred_cad"],
        "",
        "default: all"
    );

    let r = set_preference(&app, &max, "nx").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some("/account#cad-preference")
    );
    assert_eq!(app.actor("max").await.preferred_cad, "nx");
    assert_eq!(me(max_token.clone()).await["preferred_cad"], "nx");
    // another user's setting stays separate
    assert_eq!(app.actor("eva").await.preferred_cad, "");
    assert_eq!(me(eva_token).await["preferred_cad"], "");

    // the account page shows the choice
    let html = app.get_page("/account", Some(&max)).await.text();
    assert!(html.contains("id=\"cad-preference\""));
    assert!(
        html.contains("<option value=\"nx\" selected>"),
        "current choice"
    );

    // neutral groups and unknown ids are refused, the setting is unchanged
    for bad in ["exchange", "teamcenter", "nonsense"] {
        let r = set_preference(&app, &max, bad).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{bad}: back with an error");
        assert_eq!(app.actor("max").await.preferred_cad, "nx", "{bad}");
    }
    // back to "show all"
    set_preference(&app, &max, "").await;
    assert_eq!(app.actor("max").await.preferred_cad, "");
    // a stored id that is no longer registered counts as "show all"
    sqlx::query("UPDATE users SET preferred_cad = 'gone' WHERE username = 'eva'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(app.actor("eva").await.preferred_cad, "");
    app.teardown().await;
}

#[tokio::test]
async fn the_revision_page_collapses_other_cad_programs() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let cookie = app.login("max").await;
    let (number, mixed) = part_with_files(
        &app,
        &token,
        "Bracket",
        &[
            ("bracket.prt", b"NX part content"),
            ("bracket.sldprt", b"SolidWorks part content"),
            ("bracket.step", STEP),
            ("bracket.ipt", b"Inventor part content"),
        ],
    )
    .await;
    let (_, without_nx) = part_with_files(
        &app,
        &token,
        "Cover",
        &[
            ("cover.sldprt", b"SolidWorks part content"),
            ("cover.step", STEP),
        ],
    )
    .await;
    let page = |rev: String| {
        let (app, cookie) = (&app, cookie.clone());
        async move {
            app.get_page(&format!("/revisions/{rev}"), Some(&cookie))
                .await
                .text()
        }
    };
    let link = |name: &str| format!(">{name}</a>");

    // without a preference: one list, no collapsed group
    let html = page(mixed.clone()).await;
    assert!(!html.contains("other-formats"));
    for name in [
        "bracket.prt",
        "bracket.sldprt",
        "bracket.step",
        "bracket.ipt",
    ] {
        assert!(html.contains(&link(name)), "{name}");
    }

    // NX preferred: NX and STEP outside, SolidWorks and Inventor inside <details>
    assert_eq!(
        set_preference(&app, &cookie, "nx").await.status,
        StatusCode::SEE_OTHER
    );
    let html = page(mixed.clone()).await;
    let (outside, inside) = html
        .split_once("<details class=\"other-formats\"")
        .expect("collapsed group");
    assert!(outside.contains(&link("bracket.prt")));
    assert!(
        outside.contains(&link("bracket.step")),
        "neutral files stay visible"
    );
    // in the order of the file list (by name)
    assert!(inside.contains("2 files in other CAD formats (Inventor, SolidWorks)"));
    assert!(inside.contains(&link("bracket.sldprt")) && inside.contains(&link("bracket.ipt")));
    for name in ["bracket.sldprt", "bracket.ipt"] {
        assert!(
            !outside.contains(&link(name)),
            "{name} only in the collapsed group"
        );
        assert_eq!(html.matches(&link(name)).count(), 1, "{name} listed once");
    }

    // no NX file in the revision: everything as today
    let html = page(without_nx).await;
    assert!(!html.contains("other-formats"));
    assert!(html.contains(&link("cover.sldprt")) && html.contains(&link("cover.step")));

    // the vault explorer's detail pane groups the same way
    let html = app
        .get_page(&format!("/vault?folder=mine&rev={mixed}"), Some(&cookie))
        .await
        .text();
    let (outside, inside) = html
        .split_once("<details class=\"other-formats\"")
        .expect("collapsed group in the explorer");
    assert!(outside.contains(">bracket.prt</a>") && outside.contains(">bracket.step</a>"));
    assert!(inside.contains("2 files in other CAD formats (Inventor, SolidWorks)"));
    assert!(inside.contains(">bracket.sldprt</a>"));

    // display only: the API lists every file, with its family for the desktop
    let files = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/revisions/A/files"),
            &token,
            None,
        )
        .await
        .json();
    let families: Vec<(String, serde_json::Value)> = files
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["filename"].as_str().unwrap().to_string(),
                f["format_info"]["family"].clone(),
            )
        })
        .collect();
    assert_eq!(families.len(), 4, "nothing removed from the API");
    for (name, family) in families {
        let expected = match name.as_str() {
            "bracket.prt" => json!("nx"),
            "bracket.sldprt" => json!("solidworks"),
            "bracket.ipt" => json!("inventor"),
            _ => json!(null),
        };
        assert_eq!(family, expected, "{name}");
    }
    app.teardown().await;
}
