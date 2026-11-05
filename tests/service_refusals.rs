//! Refusal paths and races of the mutating services: a refused request
//! leaves no row, no counter step and no audit entry behind, and two
//! requests racing for the same thing give one winner and a clean refusal.

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

/// Row counts of the tables a mutation writes to, plus the counters.
async fn snapshot(pool: &sqlx::PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for sql in [
        "SELECT COUNT(*) FROM items",
        "SELECT COUNT(*) FROM revisions",
        "SELECT COUNT(*) FROM bom_lines",
        "SELECT COUNT(*) FROM audit_log",
        "SELECT COALESCE(SUM(value), 0)::BIGINT FROM counters",
    ] {
        out.push(sqlx::query_scalar(sql).fetch_one(pool).await.unwrap());
    }
    out
}

async fn create(app: &common::TestApp, token: &str, kind: &str, name: &str, number: &str) -> Value {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": kind, "name": name, "project": "ROCKET", "number": number })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn refused_item_creation_leaves_nothing_behind() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user_in("viewer", Role::Viewer, &["ROCKET"]).await;
    let (_, outsider) = app.user_in("outsider", Role::Editor, &["ROVER"]).await;
    create(&app, &eng, "part", "Bracket", "BRK-1").await;
    let before = snapshot(&app.pool).await;

    let post = |token: String, body: Value| {
        let app = &app;
        async move {
            app.api(Method::POST, "/api/v1/items", &token, Some(body))
                .await
        }
    };
    // duplicate number: a conflict
    let r = post(
        eng.clone(),
        json!({ "item_type": "part", "name": "Again", "project": "ROCKET", "number": "BRK-1" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // read-only role, editor of another group, unknown type, unknown group,
    // empty and oversized names
    let bodies = [
        (
            &viewer,
            json!({ "item_type": "part", "name": "X", "project": "ROCKET" }),
        ),
        (
            &outsider,
            json!({ "item_type": "part", "name": "X", "project": "ROCKET" }),
        ),
        (
            &eng,
            json!({ "item_type": "gadget", "name": "X", "project": "ROCKET" }),
        ),
        (
            &eng,
            json!({ "item_type": "part", "name": "X", "project": "NO-SUCH" }),
        ),
        (
            &eng,
            json!({ "item_type": "part", "name": "", "project": "ROCKET" }),
        ),
        (
            &eng,
            json!({ "item_type": "part", "name": "x".repeat(10_000), "project": "ROCKET" }),
        ),
    ];
    for (token, body) in bodies {
        let r = post(token.clone(), body.clone()).await;
        assert!(
            r.status.is_client_error(),
            "{body}: {} {}",
            r.status,
            r.text()
        );
    }
    assert_eq!(snapshot(&app.pool).await, before, "no row, no audit entry");
    app.teardown().await;
}

#[tokio::test]
async fn refused_bom_edits_leave_nothing_behind() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    create(&app, &eng, "assembly", "Top", "ASM-T").await;
    create(&app, &eng, "assembly", "Sub", "ASM-S").await;
    create(&app, &eng, "part", "Leaf", "PRT-L").await;
    let add = |parent: &'static str, body: Value| {
        let (app, eng) = (&app, eng.clone());
        async move {
            app.api(
                Method::POST,
                &format!("/api/v1/items/{parent}/revisions/latest/bom"),
                &eng,
                Some(body),
            )
            .await
        }
    };
    let ok = add("ASM-T", json!({ "child": "ASM-S", "quantity": 1 })).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text());
    let before = snapshot(&app.pool).await;

    let refused = [
        // a cycle through the sub-assembly, and the direct one
        ("ASM-S", json!({ "child": "ASM-T", "quantity": 1 })),
        ("ASM-S", json!({ "child": "ASM-S", "quantity": 1 })),
        // quantities that are not positive numbers
        ("ASM-T", json!({ "child": "PRT-L", "quantity": 0 })),
        ("ASM-T", json!({ "child": "PRT-L", "quantity": -2 })),
        // a child that does not exist
        ("ASM-T", json!({ "child": "NOPE-1", "quantity": 1 })),
        // a part cannot have a BOM
        ("PRT-L", json!({ "child": "ASM-S", "quantity": 1 })),
    ];
    for (parent, body) in refused {
        let r = add(parent, body.clone()).await;
        assert!(
            r.status.is_client_error(),
            "{parent} {body}: {} {}",
            r.status,
            r.text()
        );
    }
    assert_eq!(snapshot(&app.pool).await, before, "nothing changed");

    // the structure that was built is intact
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-T/revisions/A/bom",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 1);
    app.teardown().await;
}

/// Two users check out the same revision at the same time: one wins, the
/// other gets a conflict, and the lock belongs to the winner.
#[tokio::test]
async fn concurrent_check_outs_have_one_winner() {
    let app = app_or_skip!();
    let (_, a) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (_, b) = app.user_in("bert", Role::Editor, &["ROCKET"]).await;
    let (_, c) = app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    create(&app, &a, "part", "Shaft", "SHAFT-1").await;
    // released items are revised first; here the creator's lock is lifted
    let r = app
        .api(
            Method::POST,
            "/api/v1/items/SHAFT-1/revisions/A/checkin",
            &a,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let uri = "/api/v1/items/SHAFT-1/revisions/A/checkout";
    let (r1, r2, r3) = tokio::join!(
        app.api(Method::POST, uri, &a, None),
        app.api(Method::POST, uri, &b, None),
        app.api(Method::POST, uri, &c, None),
    );
    let statuses = [r1.status, r2.status, r3.status];
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        2,
        "{statuses:?}"
    );
    let holders: Vec<String> = sqlx::query_scalar(
        "SELECT u.username FROM revisions r JOIN users u ON u.id = r.checked_out_by
         JOIN items i ON i.id = r.item_id WHERE i.number = 'SHAFT-1'",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(holders.len(), 1, "{holders:?}");
    let winner =
        ["anna", "bert", "carl"][statuses.iter().position(|s| *s == StatusCode::OK).unwrap()];
    assert_eq!(holders[0], winner);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action LIKE '%check%out%' AND action NOT LIKE '%cancel%'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(audited >= 1);
    app.teardown().await;
}

/// The same explicit number requested twice at once: one item, one conflict.
#[tokio::test]
async fn concurrent_creation_with_one_number_has_one_winner() {
    let app = app_or_skip!();
    let (_, a) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (_, b) = app.user_in("bert", Role::Editor, &["ROCKET"]).await;
    let body = |name: &str| {
        Some(json!({ "item_type": "part", "name": name, "project": "ROCKET", "number": "RACE-1" }))
    };
    let (r1, r2) = tokio::join!(
        app.api(Method::POST, "/api/v1/items", &a, body("One")),
        app.api(Method::POST, "/api/v1/items", &b, body("Two")),
    );
    let mut statuses = [r1.status, r2.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::CREATED, StatusCode::CONFLICT],
        "{} / {}",
        r1.text(),
        r2.text()
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE number = 'RACE-1'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let revisions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = 'RACE-1'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(revisions, 1, "the loser left no revision behind");
    app.teardown().await;
}

/// Generated numbers never repeat under concurrent requests.
#[tokio::test]
async fn concurrent_numbering_gives_distinct_numbers() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let one = || {
        app.api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Auto", "project": "ROCKET" })),
        )
    };
    let (r1, r2, r3, r4, r5, r6) = tokio::join!(one(), one(), one(), one(), one(), one());
    let results = [r1, r2, r3, r4, r5, r6];
    let mut numbers = Vec::new();
    for r in &results {
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        numbers.push(r.json()["item_number"].as_str().unwrap().to_string());
    }
    let unique: std::collections::BTreeSet<_> = numbers.iter().collect();
    assert_eq!(unique.len(), numbers.len(), "{numbers:?}");
    app.teardown().await;
}

/// A BOM line added twice with the same position at once: one wins.
#[tokio::test]
async fn concurrent_bom_lines_with_one_position_have_one_winner() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    create(&app, &eng, "assembly", "Top", "ASM-T").await;
    create(&app, &eng, "part", "One", "PRT-1").await;
    create(&app, &eng, "part", "Two", "PRT-2").await;
    let uri = "/api/v1/items/ASM-T/revisions/A/bom";
    let (r1, r2) = tokio::join!(
        app.api(
            Method::POST,
            uri,
            &eng,
            Some(json!({ "child": "PRT-1", "quantity": 1, "find_number": 10 }))
        ),
        app.api(
            Method::POST,
            uri,
            &eng,
            Some(json!({ "child": "PRT-2", "quantity": 1, "find_number": 10 }))
        ),
    );
    let mut statuses = [r1.status, r2.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::CREATED, StatusCode::CONFLICT],
        "{} / {}",
        r1.text(),
        r2.text()
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bom_lines WHERE find_number = 10")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    app.teardown().await;
}
