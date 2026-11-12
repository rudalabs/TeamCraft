//! BOM import at scale and under contention, on a real database: a table of
//! 10 000 lines in one transaction, simultaneous imports into one assembly,
//! refusals (no edit rights, released revision, wrong lock), the staged web
//! flow (used, foreign and expired tokens), awkward CSV files and units.
//! Complements `import_atomicity.rs` and `import_proofing.rs`.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{
    bomtable,
    domain::Role,
    error::AppError,
    services::{
        imports::{self, ImportOptions, NumberSource},
        revisions,
    },
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

/// Row counts of every table an import writes to.
async fn snapshot(pool: &sqlx::PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for sql in [
        "SELECT COUNT(*) FROM items",
        "SELECT COUNT(*) FROM revisions",
        "SELECT COUNT(*) FROM bom_lines",
        "SELECT COUNT(*) FROM audit_log",
        "SELECT COALESCE(SUM(value), 0)::BIGINT FROM counters",
        "SELECT COUNT(*) FROM staged_imports",
    ] {
        out.push(sqlx::query_scalar(sql).fetch_one(pool).await.unwrap());
    }
    out
}

fn csv_of(lines: usize) -> String {
    csv_with("SC", "Part", lines)
}

/// A flat BOM table of `lines` parts numbered `<prefix>-00001` … and named
/// `<name> 1` ….
fn csv_with(prefix: &str, name: &str, lines: usize) -> String {
    let mut s = String::from("Item,Part Number,Name,QTY\n");
    for i in 1..=lines {
        s.push_str(&format!("{i},{prefix}-{i:05},{name} {i},{}\n", i % 7 + 1));
    }
    s
}

fn table(csv: &str) -> bomtable::BomTable {
    bomtable::read("t.csv", csv.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{csv}"))
}

/// Creates an assembly and returns its number and the id of its revision A,
/// checked out by `actor`.
async fn checked_out_target(
    app: &common::TestApp,
    token: &str,
    actor: &teamcraft::auth::Actor,
    name: &str,
) -> (String, Uuid) {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "assembly", "name": name, "project": "ROCKET" }),
        )
        .await;
    let (id, locked): (Uuid, Option<Uuid>) = sqlx::query_as(
        "SELECT r.id, r.checked_out_by FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1",
    )
    .bind(&number)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    if locked != Some(actor.id) {
        revisions::checkout(&app.pool, actor, id)
            .await
            .expect("check out");
    }
    (number, id)
}

fn into_target(target: Uuid) -> ImportOptions {
    ImportOptions {
        target: Some(target),
        ..Default::default()
    }
}

/// How much longer than a tenth of the table the whole table may take: 10
/// for an import that grows linearly with the lines, 100 for one that grows
/// with their square. Today it is about 25 (3.9 s and 101 s on a developer
/// machine, 2026-11-12): the import already grows faster than linearly
/// (docs/OPEN_WORK.md). The margin above that absorbs a busy CI runner during
/// one of the two imports; a step towards quadratic still fails.
const MAX_TEN_FOLD_GROWTH: u32 = 60;

#[tokio::test]
async fn ten_thousand_lines_import_in_one_transaction() {
    let app = app_or_skip!();
    let (actor, _) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let project_id = Some(app.project_id("ROCKET").await);

    // A tenth of the table first: the time of the full table is compared with
    // it, not with a fixed limit of 120 s, which measured the runner: the
    // import took about 100 s on a developer machine and up to 126 s on
    // GitHub's runners next to the other tests. What this test guards is that
    // the import time does not grow much faster than the number of lines.
    let small = table(&csv_with("SM", "Small part", 1_000));
    let small_options = ImportOptions {
        project_id,
        name: "Small".into(),
        ..Default::default()
    };
    let started = Instant::now();
    imports::run(&app.pool, &actor, &small, "small.csv", &small_options, None)
        .await
        .expect("import of 1 000 lines");
    let took_small = started.elapsed();

    let csv = csv_of(10_000);
    let t = table(&csv);
    assert_eq!(t.rows.len(), 10_000);
    let before = snapshot(&app.pool).await;
    let options = ImportOptions {
        project_id,
        name: "Big".into(),
        ..Default::default()
    };
    let started = Instant::now();
    let result = imports::run(&app.pool, &actor, &t, "big.csv", &options, None)
        .await
        .expect("import");
    let took = started.elapsed();
    eprintln!("1 000 lines: {took_small:?}, 10 000 lines: {took:?}");
    // a floor of 1 s for the small import, so that a very fast machine does
    // not turn the fixed costs of the big one into a failure
    let budget = took_small.max(Duration::from_secs(1)) * MAX_TEN_FOLD_GROWTH;
    assert!(
        took < budget,
        "10 000 lines took {took:?}, 1 000 lines {took_small:?}: the import grows faster than \
         the number of lines (limit {budget:?})"
    );
    assert_eq!(result.lines, 10_000);
    assert_eq!(result.created.len(), 10_000);

    let after = snapshot(&app.pool).await;
    assert_eq!(after[0] - before[0], 10_001, "parts and the assembly");
    assert_eq!(after[2] - before[2], 10_000, "one BOM line per row");
    let imports_logged: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'import.bom'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(imports_logged, 2, "the tenth and the whole table");
    // all rows of one transaction share one commit time
    let distinct: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT created_at) FROM items WHERE number LIKE 'SC-%'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(distinct, 1, "one transaction");

    // the same table again: the plan matches every part, nothing new
    let again = imports::run(&app.pool, &actor, &t, "big.csv", &options, None)
        .await
        .unwrap();
    assert_eq!((again.created.len(), again.existing_items), (0, 10_000));
    app.teardown().await;
}

#[tokio::test]
async fn a_table_over_the_row_limit_is_refused_before_any_write() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let csv = csv_of(bomtable::MAX_ROWS + 1);
    let before = snapshot(&app.pool).await;
    assert!(bomtable::read("t.csv", csv.as_bytes()).is_err());
    assert!(imports::read_table("big.csv", csv.as_bytes()).is_err());
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &token,
            &[("big.csv", csv.as_bytes())],
            &[("project", "ROCKET")],
        )
        .await;
    assert!(r.status.is_client_error(), "{} {}", r.status, r.text());
    assert_eq!(snapshot(&app.pool).await, before);
    app.teardown().await;
}

#[tokio::test]
async fn simultaneous_imports_into_one_assembly_are_never_mixed() {
    let app = app_or_skip!();
    let (actor, token) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, target) = checked_out_target(&app, &token, &actor, "Target").await;
    let a = table("Part Number,Name,QTY\nCA-1,Alpha one,1\nCA-2,Alpha two,2\nCA-3,Alpha three,3\n");
    let b = table("Part Number,Name,QTY\nCB-1,Beta one,1\nCB-2,Beta two,2\nCB-3,Beta three,3\n");
    let options = into_target(target);
    let (ra, rb) = tokio::join!(
        imports::run(&app.pool, &actor, &a, "a.csv", &options, None),
        imports::run(&app.pool, &actor, &b, "b.csv", &options, None),
    );
    assert!(ra.is_ok() || rb.is_ok(), "{ra:?} {rb:?}");
    let numbers: Vec<String> = sqlx::query_scalar(
        "SELECT c.number FROM bom_lines l JOIN items c ON c.id = l.child_item_id
         WHERE l.parent_revision_id = $1 ORDER BY c.number",
    )
    .bind(target)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    let mut expected = Vec::new();
    if ra.is_ok() {
        expected.extend(["CA-1", "CA-2", "CA-3"]);
    }
    if rb.is_ok() {
        expected.extend(["CB-1", "CB-2", "CB-3"]);
    }
    assert_eq!(
        numbers, expected,
        "all lines of a successful import, none of a failed one"
    );
    let created: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE number ~ '^C[AB]-'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        created as usize,
        expected.len(),
        "no part of a refused import"
    );
    app.teardown().await;
}

#[tokio::test]
async fn the_same_table_imported_twice_at_once_creates_each_part_once() {
    let app = app_or_skip!();
    let (actor, token) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, target) = checked_out_target(&app, &token, &actor, "Target").await;
    let t = table("Part Number,Name,QTY\nDD-1,One,1\nDD-2,Two,2\n");
    let options = into_target(target);
    let (r1, r2) = tokio::join!(
        imports::run(&app.pool, &actor, &t, "a.csv", &options, None),
        imports::run(&app.pool, &actor, &t, "a.csv", &options, None),
    );
    assert!(r1.is_ok() || r2.is_ok(), "{r1:?} {r2:?}");
    let parts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE number LIKE 'DD-%'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(parts, 2, "no duplicate part");
    let lines: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM bom_lines WHERE parent_revision_id = $1")
            .bind(target)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let succeeded = [r1.is_ok(), r2.is_ok()].iter().filter(|x| **x).count() as i64;
    assert!(
        lines == 2 * succeeded || lines == 2,
        "lines {lines} for {succeeded} successful imports"
    );
    app.teardown().await;
}

#[tokio::test]
async fn simultaneous_new_assemblies_with_the_same_part_numbers_leave_no_half_import() {
    let app = app_or_skip!();
    let (actor, _) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let t = table("Part Number,Name,QTY\nEE-1,One,1\nEE-2,Two,2\nEE-3,Three,3\n");
    let options = ImportOptions {
        project_id: Some(app.project_id("ROCKET").await),
        ..Default::default()
    };
    let (r1, r2) = tokio::join!(
        imports::run(&app.pool, &actor, &t, "a.csv", &options, None),
        imports::run(&app.pool, &actor, &t, "b.csv", &options, None),
    );
    let ok = [r1.is_ok(), r2.is_ok()].iter().filter(|x| **x).count() as i64;
    assert!(ok >= 1, "{r1:?} {r2:?}");
    let parts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE number LIKE 'EE-%'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(parts, 3, "each part exactly once");
    // every assembly that exists has all of its lines
    let lines: Vec<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM bom_lines l JOIN items c ON c.id = l.child_item_id
         WHERE c.number LIKE 'EE-%' GROUP BY l.parent_revision_id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(lines.len() as i64, ok);
    assert!(lines.iter().all(|n| *n == 3), "{lines:?}");
    app.teardown().await;
}

#[tokio::test]
async fn an_import_without_edit_rights_is_refused_and_writes_nothing() {
    let app = app_or_skip!();
    let (owner, token) = app.user_in("owner", Role::Editor, &["ROCKET"]).await;
    let (target_number, target) = checked_out_target(&app, &token, &owner, "Target").await;
    let (viewer, _) = app.user_in("viewer", Role::Viewer, &["ROCKET"]).await;
    let (outsider, _) = app.user_in("outsider", Role::Editor, &["ROVER"]).await;
    let (other, _) = app.user_in("other", Role::Editor, &["ROCKET"]).await;
    let t = table("Part Number,Name,QTY\nFF-1,One,1\n");
    let before = snapshot(&app.pool).await;

    // a viewer (also for a new assembly)
    for options in [
        into_target(target),
        ImportOptions {
            project_id: Some(app.project_id("ROCKET").await),
            ..Default::default()
        },
    ] {
        let e = imports::run(&app.pool, &viewer, &t, "a.csv", &options, None)
            .await
            .unwrap_err();
        assert!(matches!(e, AppError::Forbidden(_)), "{e:?}");
    }
    // an editor of another group cannot see the target
    let e = imports::run(
        &app.pool,
        &outsider,
        &t,
        "a.csv",
        &into_target(target),
        None,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(e, AppError::NotFound(_) | AppError::Forbidden(_)),
        "{e:?}"
    );
    // a member who did not check the revision out
    let e = imports::run(&app.pool, &other, &t, "a.csv", &into_target(target), None)
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Conflict(_) | AppError::Forbidden(_)),
        "{e:?}"
    );
    assert_eq!(snapshot(&app.pool).await, before, "nothing was written");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{target_number}/revisions/A"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.json()["bom"].as_array().unwrap().len(), 0);
    app.teardown().await;
}

#[tokio::test]
async fn an_import_into_a_released_revision_is_refused() {
    let app = app_or_skip!();
    let (actor, token) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (number, target) = checked_out_target(&app, &token, &actor, "Target").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.release(&token, ("rolf", &rolf), &[&number]).await;
    let before = snapshot(&app.pool).await;
    let t = table("Part Number,Name,QTY\nGG-1,One,1\n");
    let e = imports::run(&app.pool, &actor, &t, "a.csv", &into_target(target), None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            AppError::Conflict(_) | AppError::BadRequest(_) | AppError::Forbidden(_)
        ),
        "{e:?}"
    );
    // the preview refuses as well
    let mut conn = app.pool.acquire().await.unwrap();
    assert!(
        imports::plan(&mut conn, &actor, &t, "a.csv", &into_target(target))
            .await
            .is_err()
    );
    drop(conn);
    assert_eq!(snapshot(&app.pool).await, before, "nothing was written");
    app.teardown().await;
}

#[tokio::test]
async fn the_staged_flow_refuses_used_foreign_and_expired_tokens() {
    let app = app_or_skip!();
    let (actor, _) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (other, _) = app.user_in("other", Role::Editor, &["ROCKET"]).await;
    let vault = app.vault().await;
    let options = ImportOptions {
        project_id: Some(app.project_id("ROCKET").await),
        numbers: NumberSource::File,
        ..Default::default()
    };
    let csv = b"Part Number,Name,QTY\nHH-1,One,1\nHH-2,Two,2\n";
    let id = imports::stage(
        &app.pool,
        &vault,
        &actor,
        "a.csv",
        csv.to_vec(),
        options.clone(),
    )
    .await
    .unwrap();

    // another user's token finds nothing
    let e = imports::run_staged(&app.pool, &vault, &other, id)
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::NotFound(_)), "{e:?}");
    let e = imports::preview(&app.pool, &vault, &other, id).await.err();
    assert!(matches!(e, Some(AppError::NotFound(_))), "{e:?}");
    assert_eq!(snapshot(&app.pool).await[5], 1, "still staged");

    // the first run works, the second finds nothing and writes nothing
    let done = imports::run_staged(&app.pool, &vault, &actor, id)
        .await
        .unwrap();
    assert_eq!(done.created.len(), 2);
    let after = snapshot(&app.pool).await;
    assert_eq!(after[5], 0, "the staged upload is gone");
    let e = imports::run_staged(&app.pool, &vault, &actor, id)
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::NotFound(_)), "{e:?}");
    assert_eq!(snapshot(&app.pool).await, after);

    // an expired upload is purged and cannot be run
    let id = imports::stage(
        &app.pool,
        &vault,
        &actor,
        "b.csv",
        csv.to_vec(),
        options.clone(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE staged_imports SET created_at = now() - interval '25 hours' WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(imports::purge_stale(&app.pool, &vault).await.unwrap(), 1);
    let before = snapshot(&app.pool).await;
    let e = imports::run_staged(&app.pool, &vault, &actor, id)
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::NotFound(_)), "{e:?}");
    assert_eq!(snapshot(&app.pool).await, before);
    // a random token
    let e = imports::run_staged(&app.pool, &vault, &actor, Uuid::new_v4())
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::NotFound(_)), "{e:?}");

    // a failing staged import keeps the upload (the rollback includes the removal)
    let id = imports::stage(
        &app.pool,
        &vault,
        &actor,
        "c.csv",
        csv.to_vec(),
        ImportOptions {
            project_id: Some(Uuid::new_v4()),
            ..options
        },
    )
    .await
    .unwrap();
    let before = snapshot(&app.pool).await;
    assert!(imports::run_staged(&app.pool, &vault, &actor, id)
        .await
        .is_err());
    assert_eq!(snapshot(&app.pool).await, before, "upload still staged");
    app.teardown().await;
}

#[tokio::test]
async fn two_clicks_at_once_on_a_staged_import_run_it_once() {
    let app = app_or_skip!();
    let (actor, _) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let vault = app.vault().await;
    let options = ImportOptions {
        project_id: Some(app.project_id("ROCKET").await),
        ..Default::default()
    };
    let csv = b"Part Number,Name,QTY\nII-1,One,1\nII-2,Two,2\n";
    let id = imports::stage(&app.pool, &vault, &actor, "a.csv", csv.to_vec(), options)
        .await
        .unwrap();
    let (r1, r2) = tokio::join!(
        imports::run_staged(&app.pool, &vault, &actor, id),
        imports::run_staged(&app.pool, &vault, &actor, id),
    );
    assert_eq!(
        [r1.is_ok(), r2.is_ok()].iter().filter(|x| **x).count(),
        1,
        "{r1:?} {r2:?}"
    );
    let logged: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'import.bom'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(logged, 1);
    app.teardown().await;
}

#[test]
fn csv_with_bom_semicolons_crlf_and_quoted_newlines() {
    let variants: Vec<(&str, Vec<u8>)> = vec![
        (
            "utf-8 BOM, CRLF",
            b"\xEF\xBB\xBFPart Number,Name,QTY\r\nP-1,Housing,2\r\nP-2,Seal,4\r\n".to_vec(),
        ),
        (
            "semicolons, decimal comma",
            b"Part Number;Name;QTY\nP-1;Housing;2,5\nP-2;Seal;4\n".to_vec(),
        ),
        (
            "BOM, semicolons, CRLF, no final newline",
            b"\xEF\xBB\xBFTeilenummer;Benennung;Menge\r\nP-1;Housing;2,5\r\nP-2;Seal;4".to_vec(),
        ),
        (
            "quoted newline and delimiter in a name",
            b"Part Number,Name,QTY\r\nP-1,\"Housing,\r\nleft half\",2.5\r\nP-2,Seal,4\r\n".to_vec(),
        ),
    ];
    for (what, bytes) in variants {
        let t = bomtable::read("t.csv", &bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(t.rows.len(), 2, "{what}: {:?}", t.rows);
        assert_eq!(
            t.rows[0].number, "P-1",
            "{what}: BOM must not stick to the header"
        );
        assert_eq!(t.rows[1].number, "P-2", "{what}");
        assert_eq!(t.rows[1].quantity, 4.0, "{what}");
        assert!(t.warnings.is_empty(), "{what}: {:?}", t.warnings);
        if what.starts_with("quoted") {
            assert!(t.rows[0].name.starts_with("Housing,"), "{}", t.rows[0].name);
            assert!(t.rows[0].name.contains("left half"), "{}", t.rows[0].name);
            assert_eq!(t.rows[0].quantity, 2.5);
        }
    }
    // a quote that never closes: no panic, no more rows than the file has
    let r = bomtable::read(
        "t.csv",
        b"Part Number,Name,QTY\nP-1,\"Housing,2\nP-2,Seal,4\n",
    );
    if let Ok(t) = r {
        assert!(t.rows.len() <= 1, "{:?}", t.rows);
    }
}

#[test]
fn unit_conversions_of_quantity_and_mass() {
    let t = table(
        "Part Number,Name,QTY,Mass\n\
         P-1,Plate,4 Stk,125 g\n\
         P-2,Rod,2.5 m,0.5 kg\n\
         P-3,Kit,3 Satz,1 lbmass\n\
         P-4,Foil,1.5 m²,12\n\
         P-5,Oil,\"2,5 Liter\",bogus\n",
    );
    let r = &t.rows;
    assert_eq!((r[0].quantity, r[0].unit.as_str()), (4.0, "each"));
    assert!((r[0].mass_kg.unwrap() - 0.125).abs() < 1e-12);
    assert_eq!((r[1].quantity, r[1].unit.as_str()), (2.5, "m"));
    assert_eq!(r[1].mass_kg, Some(0.5));
    assert_eq!((r[2].quantity, r[2].unit.as_str()), (3.0, "set"));
    assert!((r[2].mass_kg.unwrap() - 0.453_592_37).abs() < 1e-9);
    assert_eq!((r[3].quantity, r[3].unit.as_str()), (1.5, "m2"));
    assert_eq!(r[3].mass_kg, Some(12.0), "plain number: kg");
    assert_eq!((r[4].quantity, r[4].unit.as_str()), (2.5, "l"));
    assert_eq!(r[4].mass_kg, None, "unknown mass unit is not guessed");

    // the header decides when the value has no unit
    let t = table("Part Number,Name,QTY,Mass [g]\nP-1,Plate,1,250\n");
    assert!((t.rows[0].mass_kg.unwrap() - 0.25).abs() < 1e-12);
    assert_eq!(bomtable::normalize_unit("Stück"), "each");
    assert_eq!(bomtable::normalize_unit("SATZ"), "set");
    assert_eq!(bomtable::normalize_unit(&"x".repeat(40)).len(), 16);
}

#[tokio::test]
async fn units_and_quantities_reach_the_database() {
    let app = app_or_skip!();
    let (actor, _) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let t = table(
        "Part Number,Name,QTY,Mass\nUU-1,Plate,4 Stk,125 g\nUU-2,Rod,2.5 m,0.5 kg\nUU-3,Kit,3 Satz,\n",
    );
    let options = ImportOptions {
        project_id: Some(app.project_id("ROCKET").await),
        ..Default::default()
    };
    imports::run(&app.pool, &actor, &t, "u.csv", &options, None)
        .await
        .unwrap();
    let rows: Vec<(String, f64, String, Option<f64>)> = sqlx::query_as(
        "SELECT c.number, l.quantity::float8, c.unit, r.mass_kg::float8
         FROM bom_lines l JOIN items c ON c.id = l.child_item_id
         JOIN revisions r ON r.item_id = c.id
         WHERE c.number LIKE 'UU-%' ORDER BY c.number",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0].1, rows[0].2.as_str()), (4.0, "each"));
    assert!((rows[0].3.unwrap() - 0.125).abs() < 1e-9);
    assert_eq!((rows[1].1, rows[1].2.as_str()), (2.5, "m"));
    assert_eq!((rows[2].1, rows[2].2.as_str()), (3.0, "set"));
    app.teardown().await;
}
