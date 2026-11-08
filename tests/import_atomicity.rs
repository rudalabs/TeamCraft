//! BOM import is all-or-nothing and safe to repeat: a refused or failing
//! import leaves no item, revision, BOM line, counter step or audit entry
//! behind, and importing the same table again does not duplicate parts.

mod common;

use axum::http::{Method, StatusCode};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Row counts of every table an import writes to.
async fn snapshot(pool: &sqlx::PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for sql in [
        "SELECT COUNT(*) FROM items",
        "SELECT COUNT(*) FROM revisions",
        "SELECT COUNT(*) FROM bom_lines",
        "SELECT COUNT(*) FROM audit_log",
        "SELECT COUNT(*) FROM files",
        "SELECT COALESCE(SUM(value), 0)::BIGINT FROM counters",
    ] {
        out.push(sqlx::query_scalar(sql).fetch_one(pool).await.unwrap());
    }
    out
}

const TABLE: &str =
    "Item,Part Number,Name,QTY\n1,AT-100,Housing,1\n2,AT-200,Seal,2\n3,AT-300,Cap,4\n";

#[tokio::test]
async fn a_refused_import_writes_nothing() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, outsider) = app.user_in("outsider", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("viewer", Role::Viewer, &["ROCKET"]).await;
    let before = snapshot(&app.pool).await;
    let files: &[(&str, &[u8])] = &[("a.csv", TABLE.as_bytes())];

    // an editor of another group, and a viewer
    for token in [&outsider, &viewer] {
        let r = app
            .api_multipart(
                "/api/v1/bom-imports",
                token,
                files,
                &[("project", "ROCKET")],
            )
            .await;
        assert!(
            matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
            "{} {}",
            r.status,
            r.text()
        );
    }
    // no token
    let r = app
        .api_multipart("/api/v1/bom-imports", "tc_pat_invalid", files, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // unreadable and empty tables, an unknown group
    for (name, data) in [
        ("empty.csv", &b""[..]),
        ("head.csv", b"Part Number,Name,QTY\n"),
        ("junk.xlsx", b"PK\x03\x04not a workbook"),
        ("bin.csv", &[0xFF, 0xFE, 0x00, 0x01][..]),
    ] {
        let r = app
            .api_multipart(
                "/api/v1/bom-imports",
                &eng,
                &[(name, data)],
                &[("project", "ROCKET")],
            )
            .await;
        assert!(
            r.status.is_client_error(),
            "{name}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "NO-SUCH-GROUP")],
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.status);

    assert_eq!(snapshot(&app.pool).await, before, "nothing was written");
    app.teardown().await;
}

/// The database fails while the third part is written (an injected
/// failure): the whole import is rolled back – no part, no top-level
/// assembly, no BOM line, no counter step, no audit entry – and the same
/// import succeeds once the failure is gone.
#[tokio::test]
async fn an_error_in_a_later_row_rolls_back_the_earlier_rows() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let before = snapshot(&app.pool).await;
    for sql in [
        "CREATE FUNCTION fail_on_cap() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.number = 'AT-300' THEN RAISE EXCEPTION 'injected failure'; END IF;
           RETURN NEW;
         END $$",
        "CREATE TRIGGER fail_on_cap BEFORE INSERT ON items
         FOR EACH ROW EXECUTE FUNCTION fail_on_cap()",
    ] {
        sqlx::query(sql).execute(&app.pool).await.unwrap();
    }

    let files: &[(&str, &[u8])] = &[("a.csv", TABLE.as_bytes())];
    let fields = [("project", "ROCKET"), ("numbers", "file")];
    let r = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &fields)
        .await;
    assert!(
        r.status.is_server_error(),
        "the failure must surface: {} {}",
        r.status,
        r.text()
    );
    assert_eq!(snapshot(&app.pool).await, before, "all or nothing");
    for number in ["AT-100", "AT-200", "AT-300"] {
        let r = app
            .api(Method::GET, &format!("/api/v1/items/{number}"), &eng, None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{number} was left behind");
    }

    sqlx::query("DROP TRIGGER fail_on_cap ON items")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &fields)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["created"].as_array().unwrap().len(), 3);
    app.teardown().await;
}

#[tokio::test]
async fn importing_the_same_table_again_does_not_duplicate_parts() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let files: &[(&str, &[u8])] = &[("a.csv", TABLE.as_bytes())];
    let fields = [("project", "ROCKET"), ("name", "Unit")];

    let first = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &fields)
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text());
    assert_eq!(first.json()["created"].as_array().unwrap().len(), 3);
    let parts = |app: &common::TestApp| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM items WHERE number IN ('AT-100','AT-200','AT-300')",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(parts(&app).await, 3);

    // the dry run of the second import knows all three
    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROCKET"), ("dry_run", "true")],
        )
        .await
        .json();
    assert_eq!(
        (plan["new_items"].as_i64(), plan["existing_items"].as_i64()),
        (Some(0), Some(3))
    );

    let second = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &fields)
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text());
    let second = second.json();
    assert_eq!(second["created"].as_array().unwrap().len(), 0, "{second}");
    assert_eq!(second["existing_items"], 3);
    assert_eq!(parts(&app).await, 3, "no duplicate part");
    assert_ne!(
        first.json()["root"],
        second["root"],
        "a new top-level assembly"
    );

    // every BOM line of an assembly points at one of the three parts
    let lines: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM bom_lines b JOIN items i ON i.id = b.child_item_id
         WHERE i.number IN ('AT-100','AT-200','AT-300')",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(lines, 6, "three lines per imported assembly");
    app.teardown().await;
}

/// The plan of a dry run is what the import writes.
#[tokio::test]
async fn the_dry_run_plan_matches_the_import() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let files: &[(&str, &[u8])] = &[("a.csv", TABLE.as_bytes())];
    let before = snapshot(&app.pool).await;
    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROCKET"), ("dry_run", "true")],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    assert_eq!(
        snapshot(&app.pool).await,
        before,
        "a dry run writes nothing"
    );
    let plan = plan.json();

    let done = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &[("project", "ROCKET")])
        .await
        .json();
    let after = snapshot(&app.pool).await;
    let planned_items = plan["new_items"].as_i64().unwrap();
    let planned_lines = plan["lines"].as_array().unwrap().len() as i64;
    assert_eq!(
        done["created"].as_array().unwrap().len() as i64,
        planned_items
    );
    assert_eq!(done["lines"].as_i64().unwrap(), planned_lines);
    // new parts plus the new top-level assembly, one BOM line per planned line
    assert_eq!(after[0] - before[0], planned_items + 1, "items");
    assert_eq!(after[2] - before[2], planned_lines, "bom lines");
    app.teardown().await;
}
