//! End-to-end tests of engineering change requests (ECR): raised by anybody,
//! triaged and decided by CAD editors of the affected project groups,
//! implemented by a change order (ECO).

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

async fn create_part(app: &common::TestApp, token: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": name, "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn raise(app: &common::TestApp, token: &str, body: Value) -> Value {
    let r = app
        .api(Method::POST, "/api/v1/change-requests", token, Some(body))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn notification_kinds(app: &common::TestApp, token: &str) -> Vec<String> {
    let r = app
        .api(Method::GET, "/api/v1/notifications", token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            format!(
                "{} {}",
                n["kind"].as_str().unwrap(),
                n["subject_ref"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn viewer_raises_editor_accepts_release_implements() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let part = create_part(&app, &eng, "Bracket").await;

    // a viewer (shop floor) raises the request on revision A
    let created = raise(
        &app,
        &viewer,
        json!({ "title": "Weld seam cracks", "description": "Seen on 3 of 10 parts.\nProposal: 4 mm fillet.",
                "reason": "manufacturing", "priority": "high", "due_date": "2026-12-01",
                "items": [{ "item": part, "revision": "A", "note": "seam at the flange" }] }),
    )
    .await;
    let ecr = created["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(ecr, "ECR-000001");
    assert_eq!(created["change_request"]["status"], "new");
    assert_eq!(created["change_request"]["reason"], "manufacturing");
    assert_eq!(created["change_request"]["due_date"], "2026-12-01");
    assert_eq!(created["items"][0]["item_number"], part.as_str());
    assert_eq!(created["items"][0]["revision_code"], "A");
    assert_eq!(created["watching"], true, "the requester watches it");
    assert_eq!(created["can"]["withdraw"], true);
    assert_eq!(created["can"]["decide"], false, "viewers do not decide");
    // the CAD editors of ROCKET are told, the ROVER editor is not
    assert!(notification_kinds(&app, &eng)
        .await
        .contains(&format!("change_request {ecr}")));
    assert!(!notification_kinds(&app, &outsider)
        .await
        .iter()
        .any(|k| k.starts_with("change_request")));

    // anybody discusses
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/comments"),
            &outsider,
            Some(json!({ "body": "Same on the rover bracket?" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // viewers and editors of other groups neither triage nor decide
    for token in [&viewer, &outsider] {
        for (path, body) in [
            ("triage", json!({})),
            ("decide", json!({ "decision": "accept" })),
        ] {
            let r = app
                .api(
                    Method::POST,
                    &format!("/api/v1/change-requests/{ecr}/{path}"),
                    token,
                    Some(body),
                )
                .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text());
        }
    }

    // the editor triages (and is assigned) ...
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/triage"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change_request"]["status"], "in_triage");
    assert_eq!(r.json()["change_request"]["assignee_username"], "erika");
    // ... rejecting needs a reason ...
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/decide"),
            &eng,
            Some(json!({ "decision": "reject" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // ... and accepts
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/decide"),
            &eng,
            Some(json!({ "decision": "accept", "comment": "4 mm fillet it is" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change_request"]["status"], "accepted");
    assert_eq!(
        r.json()["change_request"]["decided_by_name"],
        "erika (test)"
    );
    assert_eq!(r.json()["can"]["implement"], true);
    // the requester is told; withdrawing is over now
    assert!(notification_kinds(&app, &viewer)
        .await
        .contains(&format!("change_request {ecr}")));
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/withdraw"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // accepted → a draft change order with the in-work revision, linked
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/create-change"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let created = r.json();
    let chg = created["change"]["number"].as_str().unwrap().to_string();
    assert_eq!(created["change"]["status"], "draft");
    assert_eq!(created["added"], json!([format!("{part}/A")]));
    assert_eq!(created["skipped"], json!([]));
    assert!(created["change"]["title"]
        .as_str()
        .unwrap()
        .starts_with("ECR-000001: "));
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr}"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.json()["changes"][0]["number"], chg.as_str());
    let body = r.json();
    let history: Vec<&str> = body["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["action"].as_str().unwrap())
        .collect();
    for action in [
        "change_request.create",
        "change_request.comment",
        "change_request.triage",
        "change_request.accepted",
        "change_request.create_change",
    ] {
        assert!(
            history.contains(&action),
            "history has {action}: {history:?}"
        );
    }

    // the change order is reviewed and released → the request is implemented
    for (path, token, body) in [
        ("reviewers", &eng, Some(json!({ "username": "rolf" }))),
        ("submit", &eng, None),
        ("review", &reviewer, Some(json!({ "decision": "approve" }))),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/{path}"),
                token,
                body,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr}"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.json()["change_request"]["status"], "implemented");
    assert!(r.json()["change_request"]["closed_at"].is_string());
    assert_eq!(r.json()["changes"][0]["status"], "released");
    let r = app
        .api(
            Method::GET,
            "/api/v1/change-requests?status=open",
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.json()["total"], 0, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests?scope=mine&item={part}"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.json()["total"], 1, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn withdraw_link_and_attachments() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let (_, other) = app.user("olga", Role::Viewer).await;
    let part = create_part(&app, &eng, "Nozzle").await;

    // the requester withdraws while it is undecided; nobody else may
    let first = raise(
        &app,
        &viewer,
        json!({ "title": "Wrong thread", "items": [{ "item": part }] }),
    )
    .await;
    let ecr = first["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/withdraw"),
            &other,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/withdraw"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change_request"]["status"], "withdrawn");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr}/decide"),
            &eng,
            Some(json!({ "decision": "accept" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "final: {}", r.text());

    // defer → take up again → accept → link an existing draft change
    let second = raise(
        &app,
        &viewer,
        json!({ "title": "Lighter nozzle", "reason": "cost", "items": [{ "item": part }] }),
    )
    .await;
    let ecr2 = second["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/decide"),
            &eng,
            Some(json!({ "decision": "defer", "comment": "next model year" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change_request"]["status"], "deferred");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/reopen"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json()["change_request"]["status"], "in_triage");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/decide"),
            &eng,
            Some(json!({ "decision": "accept" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &eng,
            Some(json!({ "title": "Nozzle rework" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/link-change"),
            &viewer,
            Some(json!({ "change": chg })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/link-change"),
            &eng,
            Some(json!({ "change": chg })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changes"][0]["number"], chg.as_str());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/change-requests/{ecr2}/link-change"),
            &eng,
            Some(json!({ "change": chg })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "linked twice: {}", r.text());

    // attachments: images and PDF only, sniffed from the content
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    let r = app
        .api_upload(
            &format!("/api/v1/change-requests/{ecr2}/attachments"),
            &viewer,
            "crack.png",
            png,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["content_type"], "image/png");
    let attachment = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api_upload(
            &format!("/api/v1/change-requests/{ecr2}/attachments"),
            &viewer,
            "evil.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-request-attachments/{attachment}/content"),
            &other,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("image/png"));
    assert_eq!(r.body, png);

    // worklist: requests raised by me
    let r = app
        .api(Method::GET, "/api/v1/worklist", &viewer, None)
        .await;
    assert_eq!(r.json()["my_requests"][0]["number"], ecr2.as_str());
    // the revision of the item shows its open requests
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{part}/revisions/A"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.json()["open_requests"][0]["number"], ecr2.as_str());
    app.teardown().await;
}

#[tokio::test]
async fn web_ui_request_pages() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.user("vera", Role::Viewer).await;
    let part = create_part(&app, &eng, "Housing").await;
    let rev_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&part)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let cookie = app.login("vera").await;
    let origin = Some("http://localhost:8080");

    // the revision page offers "Request change…" to viewers
    let r = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r
        .text()
        .contains(&format!("/requests/new?revision={rev_id}")));
    let r = app
        .get_page(
            &format!("/requests/new?revision={rev_id}&file=housing.step"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("File: housing.step"));

    let r = app
        .post_form(
            "/requests",
            Some(&cookie),
            origin,
            &[
                ("title", "Burr on the edge"),
                ("description", "Sharp edge after milling"),
                ("reason", "safety"),
                ("priority", "normal"),
                ("revision_id", &rev_id.to_string()),
                ("note", "File: housing.step"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let location = r.header("location").unwrap();
    assert!(location.starts_with("/requests/"), "{location}");
    let r = app.get_page(&location, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    let page = r.text();
    assert!(page.contains("ECR-000001") && page.contains("Burr on the edge"));
    assert!(page.contains("Withdraw"), "the requester may withdraw");
    assert!(!page.contains("name=\"decision\""), "viewers do not decide");

    // the editor's dashboard lists nothing assigned yet, the list shows it
    let editor_cookie = app.login("erika").await;
    let r = app.get_page("/requests", Some(&editor_cookie)).await;
    assert!(r.text().contains("Burr on the edge"));
    let r = app.get_page(&location, Some(&editor_cookie)).await;
    assert!(r.text().contains("name=\"decision\""), "editors decide");
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(r.text().contains("My open requests") && r.text().contains("ECR-000001"));

    // a viewer's attempt to decide is refused with a flash, not an error page
    let r = app
        .post_form(
            &format!("{location}/decide"),
            Some(&cookie),
            origin,
            &[("decision", "accept")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app
        .api(
            Method::GET,
            "/api/v1/change-requests/ECR-000001",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json()["change_request"]["status"], "new");
    app.teardown().await;
}
