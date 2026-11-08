//! Multi-row mutations are all-or-nothing, and concurrent runs of the same
//! mutation have one winner.
//!
//! **Injected failures.** A trigger in the test database makes one step in the
//! middle of a mutation fail – either with an error (a failing statement) or
//! by ending the database connection (`pg_terminate_backend` on its own
//! backend, a connection lost between two steps). Afterwards nothing of the
//! mutation may be left: no rows, no counter steps, no audit entries,
//! notifications or webhook deliveries, and the state it started from (a
//! check-out, a review decision, a visibility) is unchanged. Once the fault is
//! gone, the same request succeeds and writes everything exactly once.
//!
//! **Concurrency.** The same mutation sent twice at the same time ends with one
//! winner and a clean refusal (409), never with duplicates or a lost update.
//!
//! Covered: check-in, change order release, *Replace in assemblies*, Save As
//! with files and BOM, chunked upload `finish` (with check-in), the
//! restricted-group visibility switch, and numbering. The folder import is
//! covered by the import tests.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------
// Fault injection
// ---------------------------------------------------------------------------

/// How the injected step fails.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The statement raises an error.
    Error,
    /// The database connection ends in the middle of the transaction.
    Disconnect,
}

/// Makes every `event` (`INSERT` or `UPDATE`) on `table` whose row matches
/// `when` fail the way `fault` says.
async fn inject(pool: &PgPool, table: &str, event: &str, when: &str, fault: Fault) {
    let kill = match fault {
        Fault::Error => "",
        Fault::Disconnect => "PERFORM pg_terminate_backend(pg_backend_pid());",
    };
    sqlx::raw_sql(&format!(
        "CREATE OR REPLACE FUNCTION tc_test_fault() RETURNS trigger LANGUAGE plpgsql AS $f$
         BEGIN {kill} RAISE EXCEPTION 'injected fault'; END $f$;
         DROP TRIGGER IF EXISTS tc_test_fault ON {table};
         CREATE TRIGGER tc_test_fault BEFORE {event} ON {table}
         FOR EACH ROW WHEN ({when}) EXECUTE FUNCTION tc_test_fault();"
    ))
    .execute(pool)
    .await
    .expect("inject the fault");
}

async fn heal(pool: &PgPool, table: &str) {
    sqlx::raw_sql(&format!("DROP TRIGGER IF EXISTS tc_test_fault ON {table}"))
        .execute(pool)
        .await
        .expect("remove the fault");
}

/// Everything a mutation could leave behind: row counts, the sum of the
/// counters, and the state of every revision, change, review and group.
async fn snapshot(pool: &PgPool) -> Vec<String> {
    let mut out = Vec::new();
    for table in [
        "items",
        "revisions",
        "files",
        "file_versions",
        "bom_lines",
        "audit_log",
        "notifications",
        "impacts",
        "webhook_deliveries",
        "upload_sessions",
        "change_requests",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap();
        out.push(format!("{table}={n}"));
    }
    for (label, sql) in [
        ("counters", "SELECT COALESCE(SUM(value), 0)::text FROM counters"),
        (
            "revisions",
            "SELECT COALESCE(string_agg(id::text || ':' || state || ':' || COALESCE(checked_out_by::text, '-')
                                        || ':' || name, ',' ORDER BY id), '') FROM revisions",
        ),
        (
            "changes",
            "SELECT COALESCE(string_agg(number || ':' || status, ',' ORDER BY number), '') FROM changes",
        ),
        (
            "reviews",
            "SELECT COALESCE(string_agg(decision, ',' ORDER BY change_id, reviewer_id), '') FROM change_reviews",
        ),
        (
            "groups",
            "SELECT COALESCE(string_agg(code || ':' || visibility, ',' ORDER BY code), '') FROM projects",
        ),
        (
            "lines",
            "SELECT COALESCE(string_agg(id::text || ':' || child_item_id::text, ',' ORDER BY id), '') FROM bom_lines",
        ),
    ] {
        let v: String = sqlx::query_scalar(sql).fetch_one(pool).await.unwrap();
        out.push(format!("{label}={v}"));
    }
    out
}

fn assert_failed(r: &common::Response, what: &str) {
    assert!(
        r.status.is_server_error(),
        "{what}: the injected fault must fail the request, got {} {}",
        r.status,
        r.text()
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn new_item(app: &common::TestApp, token: &str, item_type: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn ok(app: &common::TestApp, method: Method, uri: &str, token: &str, body: Value) -> Value {
    let r = app.api(method, uri, token, Some(body)).await;
    assert!(r.status.is_success(), "{uri}: {} {}", r.status, r.text());
    r.json()
}

async fn checkin(
    app: &common::TestApp,
    token: &str,
    number: &str,
    files: &[(&str, &[u8])],
) -> common::Response {
    app.api_multipart(
        &format!("/api/v1/items/{number}/revisions/A/checkin"),
        token,
        files,
        &[("comment", "atomic")],
    )
    .await
}

async fn checkout(app: &common::TestApp, token: &str, number: &str) {
    ok(
        app,
        Method::POST,
        &format!("/api/v1/items/{number}/revisions/A/checkout"),
        token,
        json!({}),
    )
    .await;
}

/// A change with `items`, `reviewer` assigned and submitted; returns its number.
async fn submitted_change(
    app: &common::TestApp,
    author: &str,
    reviewer: &str,
    items: &[&str],
) -> String {
    let change = ok(
        app,
        Method::POST,
        "/api/v1/changes",
        author,
        json!({ "title": "Release" }),
    )
    .await;
    let number = change["change"]["number"]
        .as_str()
        .or_else(|| change["number"].as_str())
        .unwrap()
        .to_string();
    for item in items {
        ok(
            app,
            Method::POST,
            &format!("/api/v1/changes/{number}/items"),
            author,
            json!({ "item": item }),
        )
        .await;
    }
    ok(
        app,
        Method::POST,
        &format!("/api/v1/changes/{number}/reviewers"),
        author,
        json!({ "username": reviewer }),
    )
    .await;
    ok(
        app,
        Method::POST,
        &format!("/api/v1/changes/{number}/submit"),
        author,
        json!({}),
    )
    .await;
    number
}

async fn approve(app: &common::TestApp, token: &str, change: &str) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{change}/review"),
        token,
        Some(json!({ "decision": "approved" })),
    )
    .await
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn item_id(pool: &PgPool, number: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(number)
        .fetch_one(pool)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// All or nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn check_in_is_all_or_nothing() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = new_item(&app, &erika, "part", "Bracket").await;
    for (fault, table, event, when) in [
        // between storing the versions and releasing the lock: the lock
        // release fails (connection lost)
        (
            Fault::Disconnect,
            "revisions",
            "UPDATE",
            "OLD.checked_out_by IS NOT NULL AND NEW.checked_out_by IS NULL",
        ),
        // the last step, the audit entry, fails
        (
            Fault::Error,
            "audit_log",
            "INSERT",
            "NEW.action = 'revision.checkin'",
        ),
    ] {
        let before = snapshot(&app.pool).await;
        inject(&app.pool, table, event, when, fault).await;
        let r = checkin(
            &app,
            &erika,
            &part,
            &[("bracket.step", b"STEP one"), ("notes.txt", b"n")],
        )
        .await;
        assert_failed(&r, &format!("check-in with {fault:?}"));
        heal(&app.pool, table).await;
        assert_eq!(
            snapshot(&app.pool).await,
            before,
            "check-in with {fault:?} left something behind"
        );
    }
    // without the fault: both versions and the released lock, once
    let r = checkin(
        &app,
        &erika,
        &part,
        &[("bracket.step", b"STEP one"), ("notes.txt", b"n")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        count(&app.pool, "SELECT COUNT(*) FROM file_versions").await,
        2
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions WHERE checked_out_by IS NOT NULL"
        )
        .await,
        0
    );
    app.teardown().await;
}

#[tokio::test]
async fn change_release_is_all_or_nothing() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let a = new_item(&app, &erika, "part", "Bracket").await;
    let b = new_item(&app, &erika, "part", "Bolt").await;
    for n in [&a, &b] {
        checkin(&app, &erika, n, &[("x.step", n.as_bytes())]).await;
    }
    // a webhook for every event, and a watcher: the release writes both
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, secret_hint, events, active)
         VALUES (gen_random_uuid(), 'all', 'https://hooks.example.org/x', repeat('s', 40), '…ssss',
                 ARRAY['revision.released', 'change.approved', 'change.released'], TRUE)",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    ok(
        &app,
        Method::POST,
        &format!("/api/v1/items/{a}/watch"),
        &erika,
        json!({}),
    )
    .await;
    let change = submitted_change(&app, &erika, "ada", &[&a, &b]).await;

    // (a trigger condition cannot hold a subquery: the id goes in as a literal)
    let b_id = item_id(&app.pool, &b).await;
    let second_rev = format!("NEW.state = 'released' AND NEW.item_id = '{b_id}'");
    for (fault, table, event, when) in [
        // the second of the two revisions cannot be released
        (
            Fault::Disconnect,
            "revisions",
            "UPDATE",
            second_rev.as_str(),
        ),
        // the notifications fail
        (Fault::Error, "notifications", "INSERT", "TRUE"),
        // the webhook deliveries, the very last step, fail
        (Fault::Error, "webhook_deliveries", "INSERT", "TRUE"),
    ] {
        let before = snapshot(&app.pool).await;
        inject(&app.pool, table, event, when, fault).await;
        let r = approve(&app, &ada, &change).await;
        assert_failed(&r, &format!("release with {fault:?} on {table}"));
        heal(&app.pool, table).await;
        assert_eq!(
            snapshot(&app.pool).await,
            before,
            "release with {fault:?} on {table} left something behind"
        );
    }
    // without the fault: everything released, flagged and announced once
    let r = approve(&app, &ada, &change).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions WHERE state = 'released'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'change.released'"
        )
        .await,
        1
    );
    assert!(count(&app.pool, "SELECT COUNT(*) FROM webhook_deliveries").await >= 3);
    assert!(count(&app.pool, "SELECT COUNT(*) FROM notifications").await >= 1);
    app.teardown().await;
}

#[tokio::test]
async fn replace_in_assemblies_is_all_or_nothing() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let old = new_item(&app, &erika, "part", "Old bolt").await;
    let new = new_item(&app, &erika, "part", "New bolt").await;
    let mut parents = Vec::new();
    for name in ["Mount", "Frame", "Stage"] {
        let p = new_item(&app, &erika, "assembly", name).await;
        ok(
            &app,
            Method::POST,
            &format!("/api/v1/items/{p}/revisions/A/bom"),
            &erika,
            json!({ "child": old, "quantity": 2 }),
        )
        .await;
        parents.push(p);
    }
    let old_id = item_id(&app.pool, &old).await;
    let third_rev: uuid::Uuid = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&parents[2])
    .fetch_one(&app.pool)
    .await
    .unwrap();
    // the third line cannot be changed / its audit entry fails
    let third_line =
        format!("OLD.child_item_id = '{old_id}' AND NEW.parent_revision_id = '{third_rev}'");
    for (fault, table, event, when) in [
        (
            Fault::Disconnect,
            "bom_lines",
            "UPDATE",
            third_line.as_str(),
        ),
        (
            Fault::Error,
            "audit_log",
            "INSERT",
            &*format!(
                "NEW.action = 'bom.replace' AND NEW.summary LIKE '%{}%'",
                parents[1]
            ),
        ),
    ] {
        let before = snapshot(&app.pool).await;
        inject(&app.pool, table, event, when, fault).await;
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{old}/replace"),
                &erika,
                Some(json!({ "with": new })),
            )
            .await;
        assert_failed(&r, &format!("replace with {fault:?}"));
        heal(&app.pool, table).await;
        assert_eq!(
            snapshot(&app.pool).await,
            before,
            "replace with {fault:?} left something behind"
        );
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{old}/replace"),
            &erika,
            Some(json!({ "with": new })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["replaced"], 3);
    app.teardown().await;
}

#[tokio::test]
async fn save_as_with_files_and_bom_is_all_or_nothing() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let bolt = new_item(&app, &erika, "part", "Bolt").await;
    let asm = new_item(&app, &erika, "assembly", "Mount").await;
    ok(
        &app,
        Method::POST,
        &format!("/api/v1/items/{asm}/revisions/A/bom"),
        &erika,
        json!({ "child": bolt, "quantity": 4 }),
    )
    .await;
    checkin(
        &app,
        &erika,
        &asm,
        &[("mount.step", b"STEP mount"), ("mount.pdf", b"%PDF")],
    )
    .await;
    let save_as = |token: String| {
        let app = &app;
        let asm = asm.clone();
        async move {
            app.api(
                Method::POST,
                &format!("/api/v1/items/{asm}/revisions/A/save-as"),
                &token,
                Some(json!({ "name": "Mount copy", "files": true, "bom": true })),
            )
            .await
        }
    };
    for (fault, table, event, when) in [
        // the BOM is copied after the files: the line cannot be inserted
        (Fault::Disconnect, "bom_lines", "INSERT", "TRUE"),
        (
            Fault::Error,
            "audit_log",
            "INSERT",
            "NEW.action = 'item.save_as'",
        ),
    ] {
        let before = snapshot(&app.pool).await;
        inject(&app.pool, table, event, when, fault).await;
        let r = save_as(erika.clone()).await;
        assert_failed(&r, &format!("save as with {fault:?}"));
        heal(&app.pool, table).await;
        assert_eq!(
            snapshot(&app.pool).await,
            before,
            "save as with {fault:?} left something behind (incl. the number counter)"
        );
    }
    let r = save_as(erika.clone()).await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    assert_eq!(count(&app.pool, "SELECT COUNT(*) FROM items").await, 3);
    assert_eq!(count(&app.pool, "SELECT COUNT(*) FROM bom_lines").await, 2);
    assert_eq!(count(&app.pool, "SELECT COUNT(*) FROM files").await, 4);
    app.teardown().await;
}

#[tokio::test]
async fn upload_finish_with_check_in_is_all_or_nothing_and_can_be_repeated() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = new_item(&app, &erika, "part", "Housing").await;
    let content = vec![7u8; 100_000];
    let sha = hex::encode(Sha256::digest(&content));
    let session = ok(
        &app,
        Method::POST,
        "/api/v1/uploads",
        &erika,
        json!({ "item": part, "revision": "A", "filename": "housing.step", "size": content.len(), "sha256": sha }),
    )
    .await;
    let id = session["id"].as_str().unwrap().to_string();
    let request = axum::http::Request::builder()
        .method(Method::PATCH)
        .uri(format!("/api/v1/uploads/{id}"))
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {erika}"))
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/offset+octet-stream",
        )
        .header("Upload-Offset", "0")
        .body(axum::body::Body::from(content.clone()))
        .unwrap();
    let r = app.send(request).await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    let finish = || {
        let app = &app;
        let (id, erika) = (id.clone(), erika.clone());
        async move {
            app.api(
                Method::POST,
                &format!("/api/v1/uploads/{id}/finish"),
                &erika,
                Some(json!({ "comment": "big", "check_in": true })),
            )
            .await
        }
    };
    for (fault, table, event, when) in [
        (Fault::Disconnect, "file_versions", "INSERT", "TRUE"),
        (
            Fault::Error,
            "audit_log",
            "INSERT",
            "NEW.action = 'revision.checkin'",
        ),
    ] {
        let versions = count(&app.pool, "SELECT COUNT(*) FROM file_versions").await;
        inject(&app.pool, table, event, when, fault).await;
        let r = finish().await;
        assert_failed(&r, &format!("finish with {fault:?}"));
        heal(&app.pool, table).await;
        assert_eq!(
            count(&app.pool, "SELECT COUNT(*) FROM file_versions").await,
            versions
        );
        assert_eq!(
            count(
                &app.pool,
                "SELECT COUNT(*) FROM revisions WHERE checked_out_by IS NOT NULL"
            )
            .await,
            1,
            "still checked out after finish with {fault:?}"
        );
        // the verified upload is kept, so finish can be repeated
        let r = app
            .api(Method::GET, &format!("/api/v1/uploads/{id}"), &erika, None)
            .await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "session kept after {fault:?}: {}",
            r.text()
        );
    }
    let r = finish().await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["files"][0]["version"], 1);
    assert_eq!(
        count(&app.pool, "SELECT COUNT(*) FROM file_versions").await,
        1
    );
    assert_eq!(
        count(&app.pool, "SELECT COUNT(*) FROM upload_sessions").await,
        0
    );
    app.teardown().await;
}

#[tokio::test]
async fn the_visibility_switch_is_all_or_nothing() {
    let app = app_or_skip!();
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let put = |visibility: &'static str| {
        let (app, ada) = (&app, ada.clone());
        async move {
            app.api(
                Method::PUT,
                "/api/v1/projects/ROCKET/visibility",
                &ada,
                Some(json!({ "visibility": visibility })),
            )
            .await
        }
    };
    // a scheme of its own: a restricted group then counts in private counters
    ok(
        &app,
        Method::PUT,
        "/api/v1/projects/ROCKET/numbering",
        &ada,
        json!({ "scheme": "{PROJECT}-{TYPE}-{SEQ:4}" }),
    )
    .await;
    let r = put("restricted").await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    // numbers drawn while restricted live in the group's own counters
    new_item(&app, &erika, "part", "Secret bracket").await;
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM counters WHERE name LIKE 'num@%'"
        )
        .await,
        1,
        "a private counter"
    );
    for (fault, table, event, when) in [
        // opening the group moves its counters back: that step fails
        (Fault::Disconnect, "counters", "INSERT", "TRUE"),
        (
            Fault::Error,
            "audit_log",
            "INSERT",
            "NEW.action = 'project.visibility'",
        ),
    ] {
        let before = snapshot(&app.pool).await;
        let counters: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, value FROM counters ORDER BY name")
                .fetch_all(&app.pool)
                .await
                .unwrap();
        inject(&app.pool, table, event, when, fault).await;
        let r = put("internal").await;
        assert_failed(&r, &format!("visibility with {fault:?}"));
        heal(&app.pool, table).await;
        assert_eq!(snapshot(&app.pool).await, before);
        let after: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, value FROM counters ORDER BY name")
                .fetch_all(&app.pool)
                .await
                .unwrap();
        assert_eq!(after, counters, "the group's counters with {fault:?}");
    }
    let r = put("internal").await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM projects WHERE code = 'ROCKET' AND visibility = 'internal'"
        )
        .await,
        1
    );
    // the next number continues after the one drawn while restricted
    let next = new_item(&app, &erika, "part", "Open bracket").await;
    let numbers: Vec<String> = sqlx::query_scalar("SELECT number FROM items ORDER BY created_at")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert_eq!(numbers.len(), 2);
    assert_ne!(numbers[0], next, "no number twice");
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Concurrency: one winner, a clean 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_check_ins_at_once_have_one_winner() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = new_item(&app, &erika, "part", "Bracket").await;
    for round in 0..3 {
        if round > 0 {
            checkout(&app, &erika, &part).await;
        }
        let (one, two) = (format!("one {round}"), format!("two {round}"));
        let first = [("bracket.step", one.as_bytes())];
        let second = [("bracket.step", two.as_bytes())];
        let (r1, r2) = tokio::join!(
            checkin(&app, &erika, &part, &first),
            checkin(&app, &erika, &part, &second),
        );
        let mut statuses = [r1.status.as_u16(), r2.status.as_u16()];
        statuses.sort_unstable();
        assert_eq!(
            statuses,
            [200, 409],
            "round {round}: {} / {}",
            r1.text(),
            r2.text()
        );
        assert_eq!(
            count(&app.pool, "SELECT COUNT(*) FROM file_versions").await,
            round + 1,
            "one new version per round"
        );
    }
    app.teardown().await;
}

#[tokio::test]
async fn two_releases_at_once_release_once() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let a = new_item(&app, &erika, "part", "Bracket").await;
    checkin(&app, &erika, &a, &[("x.step", b"x")]).await;
    let change = submitted_change(&app, &erika, "ada", &[&a]).await;
    let (r1, r2) = tokio::join!(approve(&app, &ada, &change), approve(&app, &ada, &change));
    let mut statuses = [r1.status.as_u16(), r2.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "{} / {}", r1.text(), r2.text());
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'change.released'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions WHERE state = 'released'"
        )
        .await,
        1
    );
    app.teardown().await;
}

#[tokio::test]
async fn concurrent_numbering_never_hands_out_a_number_twice() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let creates = (0..12).map(|i| {
        let (app, erika) = (&app, erika.clone());
        async move {
            app.api(
                Method::POST,
                "/api/v1/items",
                &erika,
                Some(json!({ "item_type": "part", "name": format!("Part {i}"), "project": "ROCKET" })),
            )
            .await
        }
    });
    let results = futures_join_all(creates).await;
    let mut numbers: Vec<String> = results
        .iter()
        .map(|r| {
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r.json()["item_number"].as_str().unwrap().to_string()
        })
        .collect();
    numbers.sort();
    numbers.dedup();
    assert_eq!(numbers.len(), 12, "every number once: {numbers:?}");
    // the same explicit number twice at once: one item, one 409
    let explicit =
        |n: u8| {
            let (app, erika) = (&app, erika.clone());
            async move {
                app.api(
                Method::POST,
                "/api/v1/items",
                &erika,
                Some(json!({ "item_type": "part", "number": "AT-777", "name": format!("Twin {n}"),
                             "project": "ROCKET" })),
            )
            .await
            }
        };
    let (r1, r2) = tokio::join!(explicit(1), explicit(2));
    let mut statuses = [r1.status.as_u16(), r2.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409], "{} / {}", r1.text(), r2.text());
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM items WHERE number = 'AT-777'"
        )
        .await,
        1
    );
    app.teardown().await;
}

#[tokio::test]
async fn two_replacements_at_once_replace_once() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let old = new_item(&app, &erika, "part", "Old").await;
    let new = new_item(&app, &erika, "part", "New").await;
    for name in ["Mount", "Frame"] {
        let p = new_item(&app, &erika, "assembly", name).await;
        ok(
            &app,
            Method::POST,
            &format!("/api/v1/items/{p}/revisions/A/bom"),
            &erika,
            json!({ "child": old, "quantity": 1 }),
        )
        .await;
    }
    let replace = || {
        let (app, erika, old, new) = (&app, erika.clone(), old.clone(), new.clone());
        async move {
            app.api(
                Method::POST,
                &format!("/api/v1/items/{old}/replace"),
                &erika,
                Some(json!({ "with": new })),
            )
            .await
        }
    };
    let (r1, r2) = tokio::join!(replace(), replace());
    let mut statuses = [r1.status.as_u16(), r2.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "{} / {}", r1.text(), r2.text());
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'bom.replace'"
        )
        .await,
        2,
        "each line replaced once"
    );
    app.teardown().await;
}

/// `join_all` without the futures crate: polls every future together.
async fn futures_join_all<F: std::future::Future>(
    futures: impl Iterator<Item = F>,
) -> Vec<F::Output> {
    let mut pinned: Vec<std::pin::Pin<Box<F>>> = futures.map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in pinned.iter_mut().enumerate() {
            if outputs[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => outputs[i] = Some(v),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().map(Option::unwrap).collect()
}
