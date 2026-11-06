//! The BOM page of a multi-level structure: levels (`?depth=`), project
//! group column and filter (`?group=`), roll-up over the whole structure, and
//! restricted lines that never reveal their group.

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

struct Structure {
    top_rev: String,
    sub: String,
    wheel: String,
    engine: String,
    bolt: String,
    rocket_id: String,
}

/// TOP (ROVER, 10 kg)
/// ├ SUB (ROVER, 1 kg) x2
/// │ ├ WHEEL (ROVER, 0.5 kg) x4
/// │ └ ENGINE (ROCKET, 3 kg) x1
/// └ BOLT (ROCKET, 0.25 kg) x8
///
/// Whole structure: SUB = 1 + 4 x 0.5 + 3 = 6 kg, TOP = 10 + 2 x 6 + 8 x 0.25 = 24 kg.
/// Without the ROCKET lines: SUB = 3 kg, TOP = 16 kg.
async fn build(app: &common::TestApp, token: &str) -> Structure {
    let make = |kind: &'static str, name: &'static str, mass: f64, project: &'static str| {
        let body = json!({ "item_type": kind, "name": name, "mass_kg": mass, "project": project });
        async move { app.create_item(token, body).await }
    };
    let top = make("assembly", "Rover top", 10.0, "ROVER").await;
    let sub = make("assembly", "Axle module", 1.0, "ROVER").await;
    let wheel = make("part", "Wheel hub", 0.5, "ROVER").await;
    let engine = make("part", "Thruster body", 3.0, "ROCKET").await;
    let bolt = make("part", "Flange bolt", 0.25, "ROCKET").await;
    for (parent, child, quantity) in [
        (&sub, &wheel, 4.0),
        (&sub, &engine, 1.0),
        (&top, &sub, 2.0),
        (&top, &bolt, 8.0),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{parent}/revisions/A/bom"),
                token,
                Some(json!({ "child": child, "quantity": quantity })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let top_rev: String = sqlx::query_scalar(
        "SELECT r.id::text FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&top)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let rocket_id: String =
        sqlx::query_scalar("SELECT id::text FROM projects WHERE code = 'ROCKET'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    Structure {
        top_rev,
        sub,
        wheel,
        engine,
        bolt,
        rocket_id,
    }
}

fn rows_in(page: &str) -> usize {
    page.matches("<tr data-level=").count()
}

#[tokio::test]
async fn depth_and_group_views_keep_the_roll_up_of_the_whole_structure() {
    let app = app_or_skip!();
    let (_, token) = app
        .user_in("tilly", Role::Editor, &["ROVER", "ROCKET"])
        .await;
    let s = build(&app, &token).await;
    let cookie = app.login("tilly").await;
    let page = |query: &str| {
        let uri = format!("/revisions/{}/bom{query}", s.top_rev);
        let cookie = cookie.clone();
        let app = &app;
        async move {
            let r = app.get_page(&uri, Some(&cookie)).await;
            assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
            r.text()
        }
    };

    // all levels: five lines, owner groups shown and linked, totals
    let all = page("").await;
    assert_eq!(rows_in(&all), 5, "{all}");
    assert!(all.contains("24.000"), "total mass");
    assert!(all.contains(">ROCKET</a>"));
    assert!(all.contains(&format!("href=\"/projects/{}\"", s.rocket_id)));
    assert!(all.contains("data-bom-tree"));

    // depth: fewer rows, the roll-up of every line and the total unchanged
    let one = page("?depth=1").await;
    assert_eq!(rows_in(&one), 2, "SUB and BOLT: {one}");
    assert!(one.contains(&s.sub) && one.contains(&s.bolt));
    assert!(!one.contains(&s.wheel) && !one.contains(&s.engine));
    assert!(one.contains("24.000"), "total over the whole structure");
    assert!(
        one.contains("12.000"),
        "SUB x2 rolls up its hidden children"
    );
    assert!(one.contains("+2"), "two lines hidden below SUB");
    assert!(one.contains("CSV and Excel exports always contain every line"));
    let two = page("?depth=2&rule=latest").await;
    assert_eq!(rows_in(&two), 5);
    assert!(!two.contains("bom-hidden"));
    for odd in ["?depth=all", "?depth=", "?depth=0", "?depth=-3", "?depth=x"] {
        assert_eq!(rows_in(&page(odd).await), 5, "{odd} shows everything");
    }

    // group filter: matching lines and the path to them
    let rocket = page("?group=ROCKET").await;
    assert_eq!(rows_in(&rocket), 3, "SUB (path), ENGINE, BOLT: {rocket}");
    assert!(rocket.contains(&s.engine) && rocket.contains(&s.bolt) && rocket.contains(&s.sub));
    assert!(!rocket.contains(&s.wheel));
    assert!(rocket.contains("group-match"));
    assert!(rocket.contains("24.000"));
    // lower-case code and combination with depth
    let both = page("?group=rocket&depth=1").await;
    assert_eq!(rows_in(&both), 2, "SUB (+1) and BOLT");
    assert!(!both.contains(&s.engine));
    // a group without lines in the structure
    let none = page("?group=PUMP").await;
    assert_eq!(rows_in(&none), 0);
    assert!(none.contains("No line of group PUMP"));
    // an unknown code is an error, not everything
    let r = app
        .get_page(
            &format!("/revisions/{}/bom?group=NOPE", s.top_rev),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // the exports are not cut
    let r = app
        .get_page(
            &format!("/revisions/{}/bom.csv?depth=1&group=ROCKET", s.top_rev),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.text().lines().count(), 6, "header and five lines");
    app.teardown().await;
}

#[tokio::test]
async fn restricted_lines_do_not_reveal_their_group() {
    let app = app_or_skip!();
    let (_, token) = app
        .user_in("tilly", Role::Editor, &["ROVER", "ROCKET"])
        .await;
    let s = build(&app, &token).await;
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ROCKET'")
        .execute(&app.pool)
        .await
        .unwrap();
    app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let cookie = app.login("otto").await;
    let page = |query: &str| {
        let uri = format!("/revisions/{}/bom{query}", s.top_rev);
        let cookie = cookie.clone();
        let app = &app;
        async move {
            let r = app.get_page(&uri, Some(&cookie)).await;
            (r.status, r.text())
        }
    };
    for query in ["", "?depth=1", "?depth=2", "?depth=all&rule=released"] {
        let (status, body) = page(query).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        for secret in [
            "ROCKET",
            "Sounding rocket",
            &s.engine,
            &s.bolt,
            "Thruster body",
            "Flange bolt",
            &s.rocket_id,
        ] {
            assert!(!body.contains(secret), "{query} leaks {secret}: {body}");
        }
        if query.is_empty() {
            // placeholders stay, the roll-up leaves them out: 10 + 2 x 3
            assert!(body.contains("16.000"), "{body}");
            assert!(body.contains(&s.wheel));
            assert_eq!(rows_in(&body), 4, "SUB, WHEEL and two placeholders");
            assert!(body.contains("2 restricted lines"));
        }
    }
    // the group cannot be probed through the filter either
    let (status, body) = page("?group=ROCKET").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains(&s.rocket_id));
    let (_, unknown) = page("?group=NOPE").await;
    let (_, hidden) = page("?group=ROCKET").await;
    assert_eq!(
        unknown.contains("not found"),
        hidden.contains("not found"),
        "a hidden group answers like an unknown one"
    );
    // a member of ROVER's own group still filters by it
    let (status, body) = page("?group=ROVER").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows_in(&body), 2, "SUB and WHEEL: {body}");
    assert!(!body.contains("ROCKET"));
    app.teardown().await;
}
