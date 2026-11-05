//! Result values from files at check-in: the exchange file
//! (`teamcraft-results.json|csv`) and the extraction templates of a project
//! group. A malformed file refuses the check-in as a whole, templates are
//! isolated per group, every automatic write is audited.

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

/// A simulation in `project`, checked out to the token's user.
async fn simulation(app: &common::TestApp, token: &str, project: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "simulation", "name": "Duct flow", "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    format!(
        "/api/v1/items/{}/revisions/A",
        r.json()["item_number"].as_str().unwrap()
    )
}

async fn results(app: &common::TestApp, token: &str, rev: &str) -> Vec<Value> {
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/results"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["results"].as_array().unwrap().clone()
}

async fn rev_id(app: &common::TestApp, rev: &str) -> uuid::Uuid {
    let number = rev.split('/').nth(4).unwrap().to_string();
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_exchange_file_sets_values_at_check_in_and_audits_it() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let rev = simulation(&app, &eng, "ROVER").await;
    let json_file = br#"{"results":[
        {"name":"Pressure drop","value":388.25,"unit":"Pa","limit_max":500,"load_case":"nominal"},
        {"name":"Max velocity","value":12.5,"unit":"m/s"}]}"#;
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[("teamcraft-results.json", json_file), ("run.log", b"x")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let values = results(&app, &eng, &rev).await;
    assert_eq!(values.len(), 2);
    let dp = values
        .iter()
        .find(|v| v["name"] == "Pressure drop")
        .unwrap();
    assert_eq!(dp["value"], 388.25);
    assert_eq!(dp["status"], "pass");
    assert_eq!(dp["source"], "teamcraft-results.json:1");
    let audit = app.audit_actions(rev_id(&app, &rev).await).await;
    assert!(
        audit.contains(&"simulation.result.exchange".to_string()),
        "{audit:?}"
    );

    // the next check-in updates by name and load case (same file name, new
    // content = new version) and adds the new one
    let r = app
        .api(Method::POST, &format!("{rev}/checkout"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let csv = "name,value,unit,limit_min,limit_max,load_case\nPressure drop,401.5,Pa,,500,nominal\nMass,2.5,kg,,,\n";
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[("teamcraft-results.csv", csv.as_bytes())],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let values = results(&app, &eng, &rev).await;
    assert_eq!(values.len(), 3);
    let dp = values
        .iter()
        .find(|v| v["name"] == "Pressure drop")
        .unwrap();
    assert_eq!(dp["value"], 401.5, "updated, not duplicated");
}

#[tokio::test]
async fn a_malformed_exchange_file_refuses_the_whole_check_in() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let rev = simulation(&app, &eng, "ROVER").await;
    let before = results(&app, &eng, &rev).await.len();
    for (name, body, expect) in [
        (
            "teamcraft-results.json",
            &br#"[{"name":"a","value":1},{"name":"b","value":"high"}]"#[..],
            "results[1].value must be a number",
        ),
        (
            "teamcraft-results.json",
            &b"{\"results\": [\n  {\"name\": "[..],
            "line 2",
        ),
        (
            "teamcraft-results.csv",
            &b"name,value\nStress,abc\n"[..],
            "Row 2",
        ),
        (
            "teamcraft-results.json",
            &br#"[{"name":"a","value":1},{"name":"A","value":2}]"#[..],
            "listed twice",
        ),
    ] {
        let r = app
            .api_multipart(
                &format!("{rev}/checkin"),
                &eng,
                &[("notes.txt", b"first file"), (name, body)],
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{name}: {}", r.text());
        assert!(r.text().contains(expect), "{expect}: {}", r.text());
    }
    // nothing half-written: no values, no stored file, still checked out
    assert_eq!(results(&app, &eng, &rev).await.len(), before);
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE revision_id = $1")
        .bind(rev_id(&app, &rev).await)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(files, 0, "the check-in was rolled back");
    let r = app.api(Method::GET, &rev, &eng, None).await;
    assert!(
        !r.json()["checked_out_by"].is_null(),
        "still checked out: {}",
        r.text()
    );
    // too large
    let big = vec![b' '; 1024 * 1024 + 1];
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[("teamcraft-results.json", &big)],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("larger than 1 MB"), "{}", r.text());
    // both formats at once
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[
                ("teamcraft-results.json", br#"[{"name":"a","value":1}]"#),
                ("teamcraft-results.csv", b"name,value\na,1\n"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn templates_extract_values_per_group_and_show_their_source() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER", "ROCKET"]).await;
    let (_, boss) = app.user_in("boss", Role::Editor, &["ROVER"]).await;
    sqlx::query(
        "UPDATE project_members SET manager = true WHERE user_id = (SELECT id FROM users WHERE username = 'boss')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let new_template = json!({
        "name": "Pressure drop", "file_pattern": "*.log", "method": "regex",
        "pattern": "Pressure drop:\\s*([-+0-9.eE]+)\\s*Pa", "unit": "Pa", "upper_limit": 500,
        "solver": "OpenFOAM"
    });
    // a plain member may read but not change the templates
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/ROVER/result-templates",
            &eng,
            Some(new_template.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/ROVER/result-templates",
            &boss,
            Some(new_template.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let template_id = r.json()["id"].as_str().unwrap().to_string();
    // a regex without capture group is refused precisely
    let mut bad = new_template.clone();
    bad["name"] = json!("Other");
    bad["pattern"] = json!("Pressure drop: [0-9]+");
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/ROVER/result-templates",
            &boss,
            Some(bad),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.text().contains("exactly one capture group"),
        "{}",
        r.text()
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROVER/result-templates",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json()["templates"].as_array().unwrap().len(), 1);
    let project_id = app.project_id("ROVER").await;
    let audit = app.audit_actions(project_id).await;
    assert!(
        audit.contains(&"result_template.create".to_string()),
        "{audit:?}"
    );

    // ROVER simulation: the log matches, a non-matching file name does not
    let rev = simulation(&app, &eng, "ROVER").await;
    let log = "iter 1\nPressure drop: 410.5 Pa\niter 2\nPressure drop: 388.25 Pa\n";
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[
                ("solver.log", log.as_bytes()),
                ("notes.txt", log.as_bytes()),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let values = results(&app, &eng, &rev).await;
    assert_eq!(values.len(), 1, "only *.log matches");
    assert_eq!(values[0]["value"], 388.25, "the last match");
    assert_eq!(values[0]["source"], "solver.log:4");
    assert_eq!(values[0]["status"], "pass");
    assert_eq!(values[0]["solver"], "OpenFOAM");
    let audit = app.audit_actions(rev_id(&app, &rev).await).await;
    assert!(
        audit.contains(&"simulation.result.template".to_string()),
        "{audit:?}"
    );

    // another group has no templates: the same log gives nothing
    let other = simulation(&app, &eng, "ROCKET").await;
    let r = app
        .api_multipart(
            &format!("{other}/checkin"),
            &eng,
            &[("solver.log", log.as_bytes())],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        results(&app, &eng, &other).await.is_empty(),
        "per-group isolation"
    );

    // a file that does not contain the number: nothing
    let third = simulation(&app, &eng, "ROVER").await;
    let r = app
        .api_multipart(
            &format!("{third}/checkin"),
            &eng,
            &[("solver.log", b"no numbers")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(results(&app, &eng, &third).await.is_empty());

    // disabled and removed templates do nothing
    let mut off = new_template.clone();
    off["enabled"] = json!(false);
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/result-templates/{template_id}"),
            &boss,
            Some(off),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let fourth = simulation(&app, &eng, "ROVER").await;
    app.api_multipart(
        &format!("{fourth}/checkin"),
        &eng,
        &[("solver.log", log.as_bytes())],
        &[],
    )
    .await;
    assert!(results(&app, &eng, &fourth).await.is_empty());
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/result-templates/{template_id}"),
            &boss,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let audit = app.audit_actions(project_id).await;
    assert!(
        audit.contains(&"result_template.delete".to_string()),
        "{audit:?}"
    );
}

#[tokio::test]
async fn csv_templates_and_the_scan_size_cap() {
    let app = app_or_skip!();
    let (_, boss) = app.user_in("boss", Role::Editor, &["ROVER"]).await;
    sqlx::query("UPDATE project_members SET manager = true")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/ROVER/result-templates",
            &boss,
            Some(json!({
                "name": "Mass flow", "file_pattern": "summary.csv", "method": "csv",
                "csv_column": "mdot", "csv_key_column": "case", "csv_key_value": "B", "unit": "kg/s"
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = simulation(&app, &boss, "ROVER").await;
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &boss,
            &[("summary.csv", b"case,mdot\nA,1.5\nB,2.25\n")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let values = results(&app, &boss, &rev).await;
    assert_eq!(values[0]["value"], 2.25);
    assert_eq!(values[0]["source"], "summary.csv:3");

    // a number after the first 2 MB of a file is never looked at
    let tmpl = json!({
        "name": "Late", "file_pattern": "huge.log", "method": "regex", "pattern": "late=([0-9]+)"
    });
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/ROVER/result-templates",
            &boss,
            Some(tmpl),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = simulation(&app, &boss, "ROVER").await;
    let mut huge = vec![b'.'; 3 * 1024 * 1024];
    huge.extend_from_slice(b"\nlate=7\n");
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &boss,
            &[("huge.log", &huge)],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(results(&app, &boss, &rev).await.is_empty());
}
