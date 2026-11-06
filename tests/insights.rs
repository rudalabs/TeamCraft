//! Read-only analyses (`services::insights`): the BOM health check finds
//! every kind of issue once per line, says nothing about lines in groups
//! the reader may not see, handles an empty BOM and caps long reports; the
//! duplicate file search lists only copies the reader may see.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use serde_json::{json, Value};
use teamcraft::{
    domain::{RevisionRule, Role},
    error::AppError,
    services::insights::{self, Issue},
};
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

async fn new_item(app: &common::TestApp, token: &str, body: Value) -> String {
    app.create_item(token, body).await
}

/// Checks in `files` on revision A (new items are checked out by their author).
async fn checkin(app: &common::TestApp, token: &str, number: &str, files: &[(&str, &[u8])]) {
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            token,
            files,
            &[("comment", "insights")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{number}: {}", r.text());
}

async fn rev_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = 'A'",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn item_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(number)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn sql(app: &common::TestApp, query: &str, numbers: &[&str]) {
    let mut q = sqlx::query(query);
    for n in numbers {
        q = q.bind(n.to_string());
    }
    q.execute(&app.pool).await.unwrap();
}

/// A BOM line `parent` → `child` with find number `find`, written directly
/// (also where the API would refuse it: hidden children, cycles, repeats).
async fn line(app: &common::TestApp, parent: &str, child: &str, find: i32) {
    sqlx::query(
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity)
         VALUES (gen_random_uuid(), $1, $2, $3, 1)",
    )
    .bind(rev_id(app, parent).await)
    .bind(item_id(app, child).await)
    .bind(find)
    .execute(&app.pool)
    .await
    .unwrap();
}

/// The fixture: an assembly in ROCKET with one child for every issue, and a
/// child in the restricted group ARM. Returns the numbers by role.
struct Fixture {
    top: String,
    released: String,
    bare_part: String,
    document: String,
    software: String,
    checked_out: String,
    obsolete: String,
    hidden: String,
}

async fn fixture(app: &common::TestApp) -> Fixture {
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let part = |name: &str, mass: Option<f64>| json!({ "item_type": "part", "name": name, "mass_kg": mass, "project": "ROCKET" });
    let top = new_item(
        app,
        &erika,
        json!({ "item_type": "assembly", "name": "Avionics bay", "project": "ROCKET" }),
    )
    .await;
    let released = new_item(app, &erika, part("Released bracket", Some(0.2))).await;
    let bare_part = new_item(app, &erika, part("Bare spacer", None)).await;
    let document = new_item(
        app,
        &erika,
        json!({ "item_type": "document", "name": "Wiring plan", "project": "ROCKET" }),
    )
    .await;
    let software = new_item(
        app,
        &erika,
        json!({ "item_type": "software", "name": "Flight code", "project": "ROCKET" }),
    )
    .await;
    let checked_out = new_item(app, &erika, part("Busy plate", Some(0.1))).await;
    let obsolete = new_item(app, &erika, part("Old clamp", Some(0.3))).await;
    let hidden = new_item(
        app,
        &root,
        json!({ "item_type": "part", "name": "Secret gimbal", "project": "ARM" }),
    )
    .await;
    // the same content in the released and the obsolete part (and hidden)
    for n in [&released, &obsolete] {
        checkin(app, &erika, n, &[("bracket.step", b"STEP shared content")]).await;
    }
    checkin(app, &erika, &checked_out, &[("plate.step", b"STEP plate")]).await;
    checkin(
        app,
        &root,
        &hidden,
        &[("gimbal.step", b"STEP shared content")],
    )
    .await;
    for (i, child) in [
        &released,
        &bare_part,
        &document,
        &software,
        &checked_out,
        &obsolete,
        &hidden,
    ]
    .into_iter()
    .enumerate()
    {
        line(app, &top, child, i as i32 + 1).await;
    }
    // states: nothing checked out but the busy plate, one released, one
    // obsolete, the ARM group restricted, an open check on the released part
    sqlx::query("UPDATE revisions SET checked_out_by = NULL, checked_out_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    sql(
        app,
        "UPDATE revisions SET checked_out_by = (SELECT id FROM users WHERE username = 'erika'),
                checked_out_at = now()
         WHERE item_id = (SELECT id FROM items WHERE number = $1)",
        &[&checked_out],
    )
    .await;
    sql(
        app,
        "UPDATE revisions SET state = 'released', released_at = now()
         WHERE item_id = (SELECT id FROM items WHERE number = $1)",
        &[&released],
    )
    .await;
    sql(
        app,
        "UPDATE revisions SET state = 'obsolete', released_at = now()
         WHERE item_id = (SELECT id FROM items WHERE number = $1)",
        &[&obsolete],
    )
    .await;
    sql(
        app,
        "UPDATE projects SET visibility = 'restricted' WHERE code = 'ARM'",
        &[],
    )
    .await;
    sqlx::query(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id,
                              event, relation)
         VALUES (gen_random_uuid(), $1, $2, $3, $4, 'released', 'bom')",
    )
    .bind(item_id(app, &released).await)
    .bind(rev_id(app, &released).await)
    .bind(item_id(app, &bare_part).await)
    .bind(rev_id(app, &bare_part).await)
    .execute(&app.pool)
    .await
    .unwrap();
    Fixture {
        top,
        released,
        bare_part,
        document,
        software,
        checked_out,
        obsolete,
        hidden,
    }
}

fn issues_of(report: &insights::BomHealth, number: &str) -> Vec<Issue> {
    let mut v: Vec<Issue> = report
        .findings
        .iter()
        .filter(|f| f.item_number == number)
        .map(|f| f.issue)
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn bom_health_finds_every_issue_and_keeps_hidden_lines_hidden() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let erika = app.actor("erika").await;
    let top = rev_id(&app, &f.top).await;

    let report = insights::bom_health(&app.pool, &erika, top, RevisionRule::Latest)
        .await
        .unwrap();
    assert_eq!(report.root, format!("{}/A", f.top));
    assert_eq!(report.line_count, 7);
    assert!(!report.truncated);
    assert_eq!(issues_of(&report, &f.released), [Issue::CheckNeeded]);
    assert_eq!(
        issues_of(&report, &f.bare_part),
        [Issue::NotReleased, Issue::MissingMass, Issue::NoFiles],
        "a part without mass and files"
    );
    assert_eq!(
        issues_of(&report, &f.document),
        [Issue::NotReleased, Issue::NoFiles],
        "documents carry no mass"
    );
    assert_eq!(
        issues_of(&report, &f.software),
        [Issue::NotReleased],
        "software needs neither mass nor files"
    );
    assert_eq!(
        issues_of(&report, &f.checked_out),
        [Issue::NotReleased, Issue::CheckedOut]
    );
    assert_eq!(issues_of(&report, &f.obsolete), [Issue::Obsolete]);
    let busy = report
        .findings
        .iter()
        .find(|x| x.issue == Issue::CheckedOut)
        .unwrap();
    assert!(
        busy.detail.contains("checked out by erika"),
        "{}",
        busy.detail
    );
    assert_eq!(busy.path, std::slice::from_ref(&f.checked_out));
    assert_eq!(busy.level, 1);
    assert_eq!(busy.revision.as_deref(), Some("A"));

    // the hidden line: one finding, nothing of it named
    assert!(issues_of(&report, &f.hidden).is_empty());
    let restricted: Vec<_> = report
        .findings
        .iter()
        .filter(|x| x.issue == Issue::Restricted)
        .collect();
    assert_eq!(restricted.len(), 1);
    assert_eq!(restricted[0].revision, None);
    let text = serde_json::to_string(&report).unwrap();
    for secret in [f.hidden.as_str(), "Secret gimbal", "gimbal.step"] {
        assert!(!text.contains(secret), "{secret} leaks: {text}");
    }
    let counts: BTreeMap<Issue, usize> = [
        (Issue::NotReleased, 4),
        (Issue::MissingMass, 1),
        (Issue::NoFiles, 2),
        (Issue::CheckedOut, 1),
        (Issue::Obsolete, 1),
        (Issue::CheckNeeded, 1),
        (Issue::Restricted, 1),
    ]
    .into();
    assert_eq!(report.counts, counts);

    // an administrator sees the hidden line and its issues
    let (root, _) = app.user("boss", Role::Admin).await;
    let report = insights::bom_health(&app.pool, &root, top, RevisionRule::Latest)
        .await
        .unwrap();
    assert_eq!(
        issues_of(&report, &f.hidden),
        [Issue::NotReleased, Issue::MissingMass]
    );
    assert!(!report.counts.contains_key(&Issue::Restricted));

    // with the released rule, what was never released does not resolve
    let report = insights::bom_health(&app.pool, &erika, top, RevisionRule::Released)
        .await
        .unwrap();
    for n in [&f.bare_part, &f.document, &f.software, &f.checked_out] {
        assert_eq!(issues_of(&report, n), [Issue::Unresolved], "{n}");
    }
    let unresolved = report
        .findings
        .iter()
        .find(|x| x.issue == Issue::Unresolved)
        .unwrap();
    assert!(
        unresolved.detail.contains("released"),
        "{}",
        unresolved.detail
    );
    assert_eq!(unresolved.revision, None);
    assert!(!report.counts.contains_key(&Issue::NotReleased));

    // a structure that loops back is reported, not followed
    line(&app, &f.bare_part, &f.top, 1).await;
    let report = insights::bom_health(&app.pool, &erika, top, RevisionRule::Latest)
        .await
        .unwrap();
    let cycle: Vec<_> = report
        .findings
        .iter()
        .filter(|x| x.issue == Issue::Cycle)
        .collect();
    assert_eq!(cycle.len(), 1, "{:?}", report.findings);
    assert_eq!(cycle[0].path, [f.bare_part.clone(), f.top.clone()]);
    assert_eq!(cycle[0].level, 2);

    // the root itself must be visible
    let err = insights::bom_health(
        &app.pool,
        &erika,
        rev_id(&app, &f.hidden).await,
        RevisionRule::Latest,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    app.teardown().await;
}

#[tokio::test]
async fn bom_health_of_an_empty_bom_and_a_long_report() {
    let app = app_or_skip!();
    let (erika_actor, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let empty = new_item(
        &app,
        &erika,
        json!({ "item_type": "assembly", "name": "Empty frame", "project": "ROCKET" }),
    )
    .await;
    let report = insights::bom_health(
        &app.pool,
        &erika_actor,
        rev_id(&app, &empty).await,
        RevisionRule::Latest,
    )
    .await
    .unwrap();
    assert_eq!(report.line_count, 0);
    assert!(report.findings.is_empty() && report.counts.is_empty());
    assert!(!report.truncated);

    // 170 lines with three issues each: the report keeps 500, counts all
    let bare = new_item(
        &app,
        &erika,
        json!({ "item_type": "part", "name": "Rivet", "project": "ROCKET" }),
    )
    .await;
    for find in 1..=170 {
        line(&app, &empty, &bare, find).await;
    }
    let report = insights::bom_health(
        &app.pool,
        &erika_actor,
        rev_id(&app, &empty).await,
        RevisionRule::Latest,
    )
    .await
    .unwrap();
    assert_eq!(report.line_count, 170);
    assert!(report.truncated);
    assert_eq!(report.findings.len(), 500);
    // NotReleased, MissingMass, NoFiles – and CheckedOut: new items are
    // checked out by their author
    assert_eq!(report.counts.values().sum::<usize>(), 170 * 4);
    app.teardown().await;
}

#[tokio::test]
async fn duplicate_files_show_only_what_the_reader_may_see() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let erika = app.actor("erika").await;
    let (root, _) = app.user("boss", Role::Admin).await;

    let copies = |rows: &[insights::DuplicateFile]| {
        let mut v: Vec<String> = rows.iter().map(|r| r.item_number.clone()).collect();
        v.sort();
        v
    };
    let rows = insights::duplicate_files(&app.pool, &erika, None, 100)
        .await
        .unwrap();
    assert_eq!(copies(&rows), [f.released.clone(), f.obsolete.clone()]);
    assert!(rows.iter().all(|r| r.filename == "bracket.step"));
    let states: Vec<&str> = rows.iter().map(|r| r.revision_state.as_str()).collect();
    assert!(states.contains(&"obsolete") && states.contains(&"released"));
    assert_eq!(rows[0].size_bytes, b"STEP shared content".len() as i64);

    // the administrator also sees the copy in the restricted group
    let rows = insights::duplicate_files(&app.pool, &root, None, 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.item_number == f.hidden));

    // scoped to one revision: only the contents of its files
    let rows = insights::duplicate_files(
        &app.pool,
        &erika,
        Some(rev_id(&app, &f.released).await),
        100,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let rows = insights::duplicate_files(
        &app.pool,
        &erika,
        Some(rev_id(&app, &f.checked_out).await),
        100,
    )
    .await
    .unwrap();
    assert!(rows.is_empty(), "plate.step exists once");

    // the limit is clamped to 1…1000
    let rows = insights::duplicate_files(&app.pool, &erika, None, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    // a revision in a group the reader may not see answers "not found" –
    // it must not tell which visible files share its content
    let err =
        insights::duplicate_files(&app.pool, &erika, Some(rev_id(&app, &f.hidden).await), 100)
            .await
            .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    app.teardown().await;
}
