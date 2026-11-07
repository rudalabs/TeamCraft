//! Solver summary of a file version (`solversummary`, migration 0045): made
//! at check-in from solver output, served with the file list, kept when a
//! version is restored, absent for other files.

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

const TRN: &[u8] = b"Welcome to ANSYS Fluent 2023 R1\nWarning: low quality cells\n  310  1.2e-04 3.4e-05 1.0e-03   0:00:12  190\n! 311 solution is converged\n";
const F06: &[u8] = b"1    Simcenter Nastran 2212  JAN 1, 2023  PAGE 1\n*** USER FATAL MESSAGE 9050\n * * * END OF JOB * * *\n";

#[tokio::test]
async fn solver_output_gets_a_summary_other_files_do_not() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let sim = app
        .create_item(
            &erika,
            json!({ "item_type": "simulation", "name": "Duct flow", "project": "ROCKET" }),
        )
        .await;
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{sim}/revisions/A/checkin"),
            &erika,
            &[
                ("run.trn", TRN),
                ("run.f06", F06),
                ("notes.txt", b"nothing about a solver here\n"),
            ],
            &[("comment", "solver output")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{sim}/revisions/A/files"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let files = r.json();
    let by = |name: &str| {
        files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .unwrap_or_else(|| panic!("no {name}: {files}"))
            .clone()
    };
    let trn = by("run.trn");
    assert_eq!(trn["solver_summary"]["solver"], "Fluent");
    assert_eq!(trn["solver_summary"]["version"], "2023 R1");
    assert_eq!(trn["solver_summary"]["status"], "converged");
    assert_eq!(trn["solver_summary"]["warning"], 1);
    assert!(trn["solver_line"].as_str().unwrap().contains("converged"));
    let f06 = by("run.f06");
    assert_eq!(f06["solver_summary"]["status"], "fatal");
    assert_eq!(f06["solver_summary"]["fatal"], 1);
    assert!(by("notes.txt").get("solver_summary").is_none());

    app.teardown().await;
}
