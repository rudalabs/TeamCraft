//! Result values proposed by the assistant from a report: the PDF text
//! reaches `read_file_text`, `propose_result_values` stores drafts with the
//! quoted text, only a CAD editor with the revision checked out accepts them,
//! everything is audited, and the assistant never writes a final value.
//! (`tests/read_permissions.rs` calls the tool and the routes as outsiders.)

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

/// A one-page PDF with the given lines of text (Helvetica).
fn sample_pdf(lines: &[&str]) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 14 TL 50 740 Td\n");
    for line in lines {
        content.push_str(&format!("({line}) Tj T*\n"));
    }
    content.push_str("ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

async fn tool(app: &common::TestApp, token: &str, name: &str, args: Value) -> Value {
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            token,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": name, "arguments": args } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["result"].clone()
}

/// A simulation in ROVER with a PDF report, checked out to the token's user.
async fn simulation_with_report(app: &common::TestApp, token: &str) -> (String, String) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "simulation", "name": "Duct flow", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    let rev = format!("/api/v1/items/{number}/revisions/A");
    let pdf = sample_pdf(&[
        "Duct flow report",
        "Pressure drop at nominal flow: 388.25 Pa",
        "Maximum von Mises stress: 182.5 MPa",
    ]);
    let r = app
        .api_upload(&format!("{rev}/files"), token, "report.pdf", &pdf)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    (number, rev)
}

async fn pending(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM result_proposals WHERE status = 'pending'")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn rev_id(app: &common::TestApp, number: &str) -> uuid::Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_text_of_a_pdf_report_reaches_the_agent_and_proposals_need_a_decision() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    let (_, other) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let (number, rev) = simulation_with_report(&app, &eng).await;

    // the PDF's text layer is extracted at upload and read by the agent
    let r = tool(
        &app,
        &viewer,
        "read_file_text",
        json!({ "item": number, "file": "report.pdf" }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let text = r["structuredContent"]["text"].as_str().unwrap();
    assert!(
        text.contains("388.25") && text.contains("von Mises"),
        "{text}"
    );

    let values = json!([
        { "name": "Pressure drop", "value": 388.25, "unit": "Pa", "limit_max": 500,
          "load_case": "nominal", "file": "report.pdf",
          "quote": "Pressure drop at nominal flow: 388.25 Pa" },
        { "name": "Max von Mises stress", "value": 182.5, "unit": "MPa", "file": "REPORT.pdf",
          "quote": "maximum von mises stress: 182.5 MPa" }
    ]);

    // not allowed: a viewer, an editor of another group
    for (who, token) in [("viewer", &viewer), ("other group", &other)] {
        let r = tool(
            &app,
            token,
            "propose_result_values",
            json!({ "item": number, "values": values }),
        )
        .await;
        assert_eq!(r["isError"], true, "{who}: {r}");
    }
    // a quote that is not in the report, a file that is not in the revision
    for (what, bad) in [
        (
            "invented quote",
            json!([{ "name": "x", "value": 1, "file": "report.pdf", "quote": "Pressure drop: 500 Pa" }]),
        ),
        (
            "unknown file",
            json!([{ "name": "x", "value": 1, "file": "other.pdf", "quote": "Duct flow report" }]),
        ),
        (
            "no number",
            json!([{ "name": "x", "value": "high", "file": "report.pdf", "quote": "Duct flow report" }]),
        ),
        (
            "twice",
            json!([{ "name": "x", "value": 1, "file": "report.pdf", "quote": "Duct flow report" },
                   { "name": "X", "value": 2, "file": "report.pdf", "quote": "Duct flow report" }]),
        ),
        ("empty", json!([])),
    ] {
        let r = tool(
            &app,
            &eng,
            "propose_result_values",
            json!({ "item": number, "values": bad }),
        )
        .await;
        assert_eq!(r["isError"], true, "{what}: {r}");
    }
    assert_eq!(pending(&app).await, 0, "nothing stored by refused calls");

    // the editor's agent proposes: drafts only
    let r = tool(
        &app,
        &eng,
        "propose_result_values",
        json!({ "item": number, "values": values }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["proposed"].as_array().unwrap().len(),
        2
    );
    assert_eq!(pending(&app).await, 2);
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/results"),
            &eng,
            None,
        )
        .await;
    assert!(
        r.json()["results"].as_array().unwrap().is_empty(),
        "no final value was written"
    );
    let rid = rev_id(&app, &number).await;
    let audit = app.audit_actions(rid).await;
    assert_eq!(
        audit
            .iter()
            .filter(|a| *a == "simulation.proposal.create")
            .count(),
        2,
        "{audit:?}"
    );

    // a newer proposal for the same value replaces the pending one
    let r = tool(
        &app,
        &eng,
        "propose_result_values",
        json!({ "item": number, "values": [values[0].clone()] }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(pending(&app).await, 2);

    // everybody who sees the revision reads the proposals; a viewer decides nothing
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/proposals"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let list = r.json()["proposals"].as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    let stress = list
        .iter()
        .find(|p| p["name"] == "Max von Mises stress")
        .unwrap();
    assert_eq!(stress["source_file"], "report.pdf");
    assert_eq!(stress["source_version"], 1);
    let stress_id = stress["id"].as_str().unwrap().to_string();
    let drop_id = list.iter().find(|p| p["name"] == "Pressure drop").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (who, token) in [("viewer", &viewer), ("other group", &other)] {
        for action in ["accept", "reject"] {
            let r = app
                .api(
                    Method::POST,
                    &format!("/api/v1/simulation-proposals/{stress_id}/{action}"),
                    token,
                    None,
                )
                .await;
            assert!(r.status.is_client_error(), "{who} {action}: {}", r.text());
        }
    }
    assert_eq!(pending(&app).await, 2);

    // accept: the revision is checked out to the editor -> a result value
    // with its source; the second accept and a stranger's reject are refused
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/simulation-proposals/{drop_id}/accept"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "accepted");
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/results"),
            &eng,
            None,
        )
        .await;
    let results = r.json()["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["name"], "Pressure drop");
    assert_eq!(results[0]["value"], 388.25);
    assert_eq!(results[0]["status"], "pass");
    assert!(
        results[0]["source"]
            .as_str()
            .unwrap()
            .starts_with("report.pdf v1 (proposed by"),
        "{}",
        results[0]["source"]
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/simulation-proposals/{drop_id}/accept"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // reject: no value, the decision is audited
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/simulation-proposals/{stress_id}/reject"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "rejected");
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/results"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json()["results"].as_array().unwrap().len(), 1);
    assert_eq!(pending(&app).await, 0);
    let audit = app.audit_actions(rid).await;
    for action in ["simulation.proposal.accept", "simulation.proposal.reject"] {
        assert!(audit.contains(&action.to_string()), "{action}: {audit:?}");
    }
    let r = app
        .api(
            Method::GET,
            &format!("{rev}/simulation/proposals?status=all"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json()["proposals"].as_array().unwrap().len(), 2);

    // accepting needs the check-out: after the check-in a new proposal waits
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[("notes.txt", b"done")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = tool(
        &app,
        &eng,
        "propose_result_values",
        json!({ "item": number, "values": [values[1].clone()] }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let id: String =
        sqlx::query_scalar("SELECT id::text FROM result_proposals WHERE status = 'pending'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/simulation-proposals/{id}/accept"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("Check out"), "{}", r.text());
    assert_eq!(pending(&app).await, 1, "still a draft");
}

#[tokio::test]
async fn the_web_page_shows_the_drafts_with_their_quote() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (number, _) = simulation_with_report(&app, &eng).await;
    let r = tool(
        &app,
        &eng,
        "propose_result_values",
        json!({ "item": number, "values": [{
            "name": "Pressure drop", "value": 388.25, "unit": "Pa", "file": "report.pdf",
            "quote": "Pressure drop at nominal flow: 388.25 Pa" }] }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let cookie = app.login("eng").await;
    let id = rev_id(&app, &number).await;
    let page = app
        .get_page(&format!("/revisions/{id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("Proposed from reports"), "{html}");
    assert!(html.contains("Pressure drop at nominal flow: 388.25 Pa"));
    assert!(html.contains("/simulation-proposals/"));
}

#[tokio::test]
async fn the_assistant_offers_proposals_only_when_drafts_are_allowed() {
    let names = |drafts| -> Vec<String> {
        teamcraft::assistant::tool_specs(drafts)
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert!(!names(false).contains(&"propose_result_values".to_string()));
    assert!(names(true).contains(&"propose_result_values".to_string()));
    // the quick action exists and needs a revision
    let action = teamcraft::assistant::Action::parse("result_values").unwrap();
    assert_eq!(action.needs(), "revision");
    assert!(action.label().contains("result values"));
}
