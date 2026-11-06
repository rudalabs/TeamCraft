//! The name a STEP file was exported under (`FILE_NAME` in its header) is a
//! second name of the file: a file that references that name finds the
//! STEP file – but only where no file has the referenced name itself, and
//! never across a group the reader may not see. The export name is never a
//! reference of the STEP file itself.

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

/// A small STEP file exported under `export` that references `refs` in its
/// DATA section.
fn step(export: &str, refs: &[&str]) -> Vec<u8> {
    let mut data = String::new();
    for (i, name) in refs.iter().enumerate() {
        data.push_str(&format!(
            "#{}=DOCUMENT_FILE('{name}','',$,#1,'','');\n",
            i + 10
        ));
    }
    format!(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
         FILE_NAME({export},'2026-11-06T10:00:00',(''),(''),'','','');\n\
         FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{data}ENDSEC;\nEND-ISO-10303-21;\n"
    )
    .into_bytes()
}

/// A new part in `project` with one file; returns its number.
async fn part_with(
    app: &common::TestApp,
    token: &str,
    project: &str,
    filename: &str,
    content: &[u8],
) -> String {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "part", "name": filename, "project": project }),
        )
        .await;
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            token,
            &[(filename, content)],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{filename}: {}", r.text());
    number
}

async fn references(app: &common::TestApp, token: &str, number: &str) -> Value {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/revisions/A/references"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// The matches of `name` among the CAD file references: (item, file, by
/// export name).
fn matches_of(refs: &Value, name: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = refs["files"][0]["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .unwrap_or_else(|| panic!("{name} not referenced: {refs}"))["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["item_number"].as_str().unwrap().to_string(),
                m["filename"].as_str().unwrap().to_string(),
                m["export_name"].as_bool().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

async fn export_name(app: &common::TestApp, filename: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT fv.export_name FROM file_versions fv JOIN files f ON f.id = fv.file_id
         WHERE f.filename = $1 ORDER BY fv.version DESC LIMIT 1",
    )
    .bind(filename)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn files_are_found_by_the_name_they_were_exported_under() {
    let app = app_or_skip!();
    let (_, erika) = app
        .user_in("erika", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let (_, root) = app.user("root", Role::Admin).await;
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ARM'")
        .execute(&app.pool)
        .await
        .unwrap();

    // a part renamed after the export
    let bracket = part_with(
        &app,
        &erika,
        "ROCKET",
        "bracket_v2.stp",
        &step("'bracket.stp'", &[]),
    )
    .await;
    assert_eq!(
        export_name(&app, "bracket_v2.stp").await.as_deref(),
        Some("bracket.stp")
    );
    // a real file of that name wins over an export name
    let nut = part_with(&app, &erika, "ROVER", "nut.stp", &step("'nut.stp'", &[])).await;
    let old_nut = part_with(
        &app,
        &erika,
        "ROVER",
        "nut_old.stp",
        &step("'nut.stp'", &[]),
    )
    .await;
    assert_eq!(export_name(&app, "nut.stp").await, None, "the same name");
    // two files exported under the same name
    let bolt_a = part_with(
        &app,
        &erika,
        "ROCKET",
        "bolt_a.stp",
        &step("'C:\\Export\\bolt.stp'", &[]),
    )
    .await;
    let bolt_b = part_with(
        &app,
        &erika,
        "ROVER",
        "bolt_b.stp",
        &step("'/out/BOLT.stp'", &[]),
    )
    .await;
    // a broken and an empty header name: no second name
    part_with(&app, &erika, "ROCKET", "plain.stp", &step("''", &[])).await;
    part_with(&app, &erika, "ROCKET", "broken.stp", &step("$", &[])).await;
    assert_eq!(export_name(&app, "plain.stp").await, None);
    assert_eq!(export_name(&app, "broken.stp").await, None);
    // a part in a group erika may not see
    let secret = part_with(
        &app,
        &root,
        "ARM",
        "gimbal_x.stp",
        &step("'gimbal.stp'", &[]),
    )
    .await;

    // the assembly references all of them by the exported names
    let stage = part_with(
        &app,
        &erika,
        "ROCKET",
        "stage.stp",
        &step(
            "'stage.stp'",
            &[
                "bracket.stp",
                "nut.stp",
                "bolt.stp",
                "gimbal.stp",
                "missing.stp",
            ],
        ),
    )
    .await;
    let refs = references(&app, &erika, &stage).await;
    assert_eq!(
        matches_of(&refs, "bracket.stp"),
        [(bracket.clone(), "bracket_v2.stp".into(), true)]
    );
    assert_eq!(
        matches_of(&refs, "nut.stp"),
        [(nut.clone(), "nut.stp".into(), false)],
        "not also the file exported as nut.stp"
    );
    assert_eq!(
        matches_of(&refs, "bolt.stp"),
        [
            (bolt_a.clone(), "bolt_a.stp".into(), true),
            (bolt_b.clone(), "bolt_b.stp".into(), true)
        ],
        "both, the reader decides"
    );
    assert!(matches_of(&refs, "gimbal.stp").is_empty(), "hidden: {refs}");
    assert!(!refs.to_string().contains("gimbal_x"));
    assert!(matches_of(&refs, "missing.stp").is_empty());
    let refs = references(&app, &root, &stage).await;
    assert_eq!(
        matches_of(&refs, "gimbal.stp"),
        [(secret.clone(), "gimbal_x.stp".into(), true)],
        "the administrator sees it"
    );

    // the other direction: who references the renamed part
    let refs = references(&app, &erika, &bracket).await;
    let referrers = refs["file_referrers"].as_array().unwrap();
    assert_eq!(referrers.len(), 1, "{refs}");
    assert_eq!(referrers[0]["item_number"], stage.as_str());
    assert_eq!(referrers[0]["filename"], "stage.stp");
    assert_eq!(referrers[0]["referenced"], "bracket_v2.stp");
    assert_eq!(referrers[0]["export_name"], true);
    // the export name of the file itself is no reference of its own
    assert!(refs["files"].as_array().unwrap().is_empty(), "{refs}");
    // a file whose export name another file has as its real name
    let refs = references(&app, &erika, &old_nut).await;
    assert!(
        refs["file_referrers"].as_array().unwrap().is_empty(),
        "{refs}"
    );
    let refs = references(&app, &erika, &nut).await;
    assert_eq!(refs["file_referrers"][0]["export_name"], false);
    for bolt in [&bolt_a, &bolt_b] {
        let refs = references(&app, &erika, bolt).await;
        assert_eq!(
            refs["file_referrers"].as_array().unwrap().len(),
            1,
            "{bolt}"
        );
    }

    // the revision page says how the file was found
    let cookie = app.login("erika").await;
    let rev: String = sqlx::query_scalar(
        "SELECT r.id::text FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&stage)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let html = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await
        .text();
    assert!(html.contains("as bracket_v2.stp (export name)"), "{html}");

    // an existing STEP file without the second name gets it from the
    // startup scan (migration 0036 adds them unscanned); the stored
    // references stay as they are
    sqlx::query(
        "UPDATE file_versions SET export_name = NULL, export_name_scanned = FALSE
         WHERE file_id = (SELECT id FROM files WHERE filename = 'bracket_v2.stp')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let vault = app.vault().await;
    teamcraft::services::files::scan_unscanned_references(&app.pool, &vault)
        .await
        .unwrap();
    assert_eq!(
        export_name(&app, "bracket_v2.stp").await.as_deref(),
        Some("bracket.stp")
    );
    // `teamcraft rescan-references --format step` as well
    sqlx::query("UPDATE file_versions SET export_name = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    teamcraft::services::files::rescan_references(&app.pool, &vault, &["step".into()])
        .await
        .unwrap();
    assert_eq!(
        export_name(&app, "bracket_v2.stp").await.as_deref(),
        Some("bracket.stp")
    );
    assert_eq!(
        export_name(&app, "bolt_b.stp").await.as_deref(),
        Some("BOLT.stp")
    );
    app.teardown().await;
}

#[tokio::test]
async fn the_export_name_comes_along_with_the_file() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let part = part_with(
        &app,
        &erika,
        "ROCKET",
        "fin_v3.stp",
        &step("'fin.stp'", &[]),
    )
    .await;
    // a new revision takes the file over with its export name
    app.release(&erika, ("rolf", &rolf), &[&part]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/revise"),
            &erika,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let names: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT fv.export_name FROM file_versions fv JOIN files f ON f.id = fv.file_id
         JOIN revisions r ON r.id = f.revision_id WHERE f.filename = 'fin_v3.stp' ORDER BY r.seq",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(names, [Some("fin.stp".into()), Some("fin.stp".into())]);
    // a new version exported under another name replaces it
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{part}/revisions/B/checkin"),
            &erika,
            &[("fin_v3.stp", &step("'fin_v3.stp'", &[]))],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(export_name(&app, "fin_v3.stp").await, None);
    app.teardown().await;
}
