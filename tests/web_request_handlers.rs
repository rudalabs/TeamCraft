//! Handler tests of `src/web/requests.rs` (engineering change requests in
//! the web UI) through the real router: the whole lifecycle through the
//! forms – who may triage, assign, decide, reopen, withdraw, edit and
//! implement –, validation errors, conflicts, affected items, attachments,
//! the cross-site guard, and requests about a restricted group.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::json;
use teamcraft::domain::Role;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const ORIGIN: Option<&str> = Some("http://localhost:8080");

/// The flash message a form action set: (`success`/`info`/`error`, message).
fn flash(r: &common::Response) -> Option<(String, String)> {
    r.headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.strip_prefix("tc_flash="))
        .map(|c| c.split(';').next().unwrap_or_default())
        .filter(|c| !c.is_empty())
        .filter_map(|hex| {
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .ok()?;
            let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            Some((
                v["kind"].as_str()?.to_string(),
                v["message"].as_str()?.to_string(),
            ))
        })
        .next()
}

fn refused(r: &common::Response, what: &str) -> String {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{what}: {}", r.text());
    let (kind, message) = flash(r).unwrap_or_else(|| panic!("{what}: no flash message"));
    assert_eq!(kind, "error", "{what}: {message}");
    message
}

fn accepted(r: &common::Response, what: &str) -> String {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{what}: {}", r.text());
    let (kind, message) = flash(r).unwrap_or_else(|| panic!("{what}: no flash message"));
    assert_ne!(kind, "error", "{what}: {message}");
    message
}

async fn multipart(
    app: &common::TestApp,
    uri: &str,
    cookie: &str,
    file: Option<(&str, &str, &[u8])>,
) -> common::Response {
    let boundary = "teamcraft-request-boundary";
    let mut body = Vec::new();
    match file {
        Some((name, content_type, bytes)) => {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {content_type}\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        None => body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\"\r\n\r\n\r\n")
                .as_bytes(),
        ),
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    app.send(request).await
}

async fn create_part(app: &common::TestApp, token: &str, project: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": name, "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn status_of(app: &common::TestApp, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM change_requests WHERE id = $1")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// Raises a request through the form; returns its id.
async fn raise(app: &common::TestApp, cookie: &str, title: &str, item: &str) -> Uuid {
    let r = app
        .post_form(
            "/requests",
            Some(cookie),
            ORIGIN,
            &[
                ("title", title),
                ("description", "The bore is 0.2 mm too small."),
                ("reason", "defect"),
                ("priority", "high"),
                ("item_number", item),
                ("note", "File: bracket.step"),
            ],
        )
        .await;
    assert!(accepted(&r, "raise").contains("Raised change request"));
    let location = r.header("location").unwrap();
    location.trim_start_matches("/requests/").parse().unwrap()
}

#[tokio::test]
async fn the_request_lifecycle_through_the_forms() {
    let app = app_or_skip!();
    let (_, eddi_token) = app.user_in("eddi", Role::Editor, &["ROVER"]).await;
    app.user_in("otto", Role::Editor, &["PUMP"]).await;
    app.user("vera", Role::Viewer).await;
    let part = create_part(&app, &eddi_token, "ROVER", "Bracket").await;
    let vera = app.login("vera").await;
    let eddi = app.login("eddi").await;
    let otto = app.login("otto").await;

    // the form, prefilled from an item and a file name
    let r = app
        .get_page(
            &format!("/requests/new?item={part}&file=bracket.step"),
            Some(&vera),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("File: bracket.step"));
    let r = app
        .get_page("/requests/new?revision=not-a-uuid", Some(&vera))
        .await;
    assert_eq!(r.status, StatusCode::OK, "a broken revision id is ignored");
    // bad input comes back in the form, with the message and the text kept
    for (field, value, expected) in [
        ("reason", "boredom", "Unknown reason"),
        ("priority", "asap", "Unknown priority"),
        ("due_date", "31.02.2027", "date"),
        ("item_number", "NOPE-404", "not found"),
        ("title", " ", "itle"),
    ] {
        let mut form = vec![
            ("title", "Bore too small"),
            ("description", "Kept <text>"),
            ("reason", "defect"),
            ("priority", "normal"),
            ("item_number", part.as_str()),
        ];
        form.retain(|(k, _)| *k != field);
        form.push((field, value));
        let r = app.post_form("/requests", Some(&vera), ORIGIN, &form).await;
        let html = r.text();
        assert!(html.contains(expected), "{field}={value}: {html}");
        assert!(
            html.contains("Kept &lt;text&gt;"),
            "{field}: the form keeps the input"
        );
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM change_requests")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);

    let id = raise(&app, &vera, "Bore too small", &part).await;
    let page = format!("/requests/{id}");
    for cookie in [&vera, &eddi, &otto] {
        let r = app.get_page(&page, Some(cookie)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    // triage: CAD editors of the affected group only
    for (cookie, who) in [(&vera, "viewer"), (&otto, "editor of another group")] {
        let r = app
            .post_form(&format!("{page}/triage"), Some(cookie), ORIGIN, &[])
            .await;
        assert!(refused(&r, who).contains("CAD editors"), "{who}");
    }
    let r = app
        .post_form(&format!("{page}/triage"), Some(&eddi), ORIGIN, &[])
        .await;
    accepted(&r, "triage");
    assert_eq!(status_of(&app, id).await, "in_triage");
    let r = app
        .post_form(&format!("{page}/triage"), Some(&eddi), ORIGIN, &[])
        .await;
    refused(&r, "triage twice");

    // assign
    let r = app
        .post_form(
            &format!("{page}/assign"),
            Some(&eddi),
            ORIGIN,
            &[("username", "nobody")],
        )
        .await;
    refused(&r, "unknown assignee");
    let r = app
        .post_form(
            &format!("{page}/assign"),
            Some(&eddi),
            ORIGIN,
            &[("username", "eddi")],
        )
        .await;
    assert!(accepted(&r, "assign").contains("assigned to"));
    let r = app
        .post_form(
            &format!("{page}/assign"),
            Some(&vera),
            ORIGIN,
            &[("username", "vera")],
        )
        .await;
    refused(&r, "the requester does not assign");

    // edit: the requester while undecided, bad input refused
    let r = app
        .post_form(
            &page,
            Some(&vera),
            ORIGIN,
            &[
                ("title", "Bore 0.2 mm too small"),
                ("description", "x"),
                ("reason", "defect"),
                ("priority", "urgent!"),
            ],
        )
        .await;
    assert!(refused(&r, "bad priority").contains("Unknown priority"));
    let r = app
        .post_form(
            &page,
            Some(&vera),
            ORIGIN,
            &[
                ("title", "Bore 0.2 mm too small"),
                ("description", "x"),
                ("reason", "defect"),
                ("priority", "normal"),
            ],
        )
        .await;
    accepted(&r, "requester edits");

    // decide: defer, reopen, accept
    let r = app
        .post_form(
            &format!("{page}/decide"),
            Some(&eddi),
            ORIGIN,
            &[("decision", "maybe")],
        )
        .await;
    assert!(refused(&r, "decision").contains("accept, reject or defer"));
    let r = app
        .post_form(
            &format!("{page}/decide"),
            Some(&vera),
            ORIGIN,
            &[("decision", "accept")],
        )
        .await;
    refused(&r, "the requester does not decide");
    let r = app
        .post_form(
            &format!("{page}/decide"),
            Some(&eddi),
            ORIGIN,
            &[("decision", "defer"), ("comment", "Next batch")],
        )
        .await;
    accepted(&r, "defer");
    assert_eq!(status_of(&app, id).await, "deferred");
    let r = app
        .post_form(&format!("{page}/reopen"), Some(&vera), ORIGIN, &[])
        .await;
    refused(&r, "the requester does not reopen");
    let r = app
        .post_form(&format!("{page}/reopen"), Some(&eddi), ORIGIN, &[])
        .await;
    accepted(&r, "reopen");
    let r = app
        .post_form(&format!("{page}/reopen"), Some(&eddi), ORIGIN, &[])
        .await;
    refused(&r, "reopen twice");
    let r = app
        .post_form(
            &format!("{page}/decide"),
            Some(&eddi),
            ORIGIN,
            &[("decision", "accept")],
        )
        .await;
    assert!(accepted(&r, "accept").contains("accepted"));

    // decided: no withdrawal, no edits by the requester
    let r = app
        .post_form(&format!("{page}/withdraw"), Some(&vera), ORIGIN, &[])
        .await;
    refused(&r, "withdraw after the decision");
    let r = app
        .post_form(
            &page,
            Some(&vera),
            ORIGIN,
            &[
                ("title", "Changed"),
                ("reason", "defect"),
                ("priority", "normal"),
            ],
        )
        .await;
    refused(&r, "edit after the decision");

    // comments: anybody, but not empty
    let r = app
        .post_form(
            &format!("{page}/comments"),
            Some(&vera),
            ORIGIN,
            &[("body", "  ")],
        )
        .await;
    refused(&r, "empty comment");
    let r = app
        .post_form(
            &format!("{page}/comments"),
            Some(&otto),
            ORIGIN,
            &[("body", "Same on PUMP-1")],
        )
        .await;
    accepted(&r, "comment");
    assert!(r.header("location").unwrap().ends_with("#discussion"));

    // implementation: link a change order or create one
    for (value, expected) in [("", "Choose a change order"), ("CHG-999999", "not found")] {
        let r = app
            .post_form(
                &format!("{page}/link-change"),
                Some(&eddi),
                ORIGIN,
                &[("change", value)],
            )
            .await;
        assert!(refused(&r, value).contains(expected), "{value}");
    }
    let r = app
        .post_form(
            &format!("{page}/create-change"),
            Some(&vera),
            ORIGIN,
            &[("title", "Fix the bore")],
        )
        .await;
    refused(&r, "the requester does not implement");
    let r = app
        .post_form(
            &format!("{page}/create-change"),
            Some(&eddi),
            ORIGIN,
            &[("title", "Fix the bore")],
        )
        .await;
    accepted(&r, "create change");
    assert!(r.header("location").unwrap().starts_with("/changes/"));
    let r = app
        .post_form(
            &format!("{page}/create-change"),
            Some(&eddi),
            ORIGIN,
            &[("title", "Again")],
        )
        .await;
    assert!(r.status == StatusCode::SEE_OTHER, "{}", r.text());

    // the list with every filter, and a withdrawal of a second request
    for q in [
        "",
        "?status=all",
        "?status=accepted&scope=mine",
        "?scope=assigned",
        &format!("?item={part}"),
        "?item=NOPE-404",
        "?status=bogus&page=0",
    ] {
        let r = app.get_page(&format!("/requests{q}"), Some(&vera)).await;
        assert_eq!(r.status, StatusCode::OK, "/requests{q}: {}", r.text());
    }
    let second = raise(&app, &vera, "Paint flakes", &part).await;
    let r = app
        .post_form(
            &format!("/requests/{second}/withdraw"),
            Some(&otto),
            ORIGIN,
            &[],
        )
        .await;
    refused(&r, "others do not withdraw");
    let r = app
        .post_form(
            &format!("/requests/{second}/withdraw"),
            Some(&vera),
            ORIGIN,
            &[],
        )
        .await;
    accepted(&r, "withdraw");
    assert_eq!(status_of(&app, second).await, "withdrawn");

    // unknown requests and cross-site posts
    let nobody = Uuid::new_v4();
    let r = app
        .get_page(&format!("/requests/{nobody}"), Some(&vera))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/requests/{nobody}/comments"),
            Some(&vera),
            ORIGIN,
            &[("body", "Hello")],
        )
        .await;
    refused(&r, "comment on an unknown request");
    let r = app
        .post_form(
            &format!("{page}/comments"),
            Some(&vera),
            Some("https://evil.example"),
            &[("body", "CSRF")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

#[tokio::test]
async fn affected_items_and_attachments() {
    let app = app_or_skip!();
    let (_, eddi_token) = app.user_in("eddi", Role::Editor, &["ROVER"]).await;
    app.user("vera", Role::Viewer).await;
    app.user("walt", Role::Viewer).await;
    let part = create_part(&app, &eddi_token, "ROVER", "Bracket").await;
    let other = create_part(&app, &eddi_token, "ROVER", "Plate").await;
    let vera = app.login("vera").await;
    let walt = app.login("walt").await;
    let id = raise(&app, &vera, "Bore too small", &part).await;
    let page = format!("/requests/{id}");

    let r = app
        .post_form(
            &format!("{page}/items"),
            Some(&vera),
            ORIGIN,
            &[("item_number", "NOPE-404")],
        )
        .await;
    refused(&r, "unknown item");
    let r = app
        .post_form(
            &format!("{page}/items"),
            Some(&walt),
            ORIGIN,
            &[("item_number", other.as_str())],
        )
        .await;
    refused(&r, "only the requester and the CAD editors add items");
    let r = app
        .post_form(
            &format!("{page}/items"),
            Some(&vera),
            ORIGIN,
            &[("item_number", other.as_str()), ("note", "too")],
        )
        .await;
    accepted(&r, "add item");
    let other_id: Uuid = sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(&other)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let r = app
        .post_form(
            &format!("{page}/items/{other_id}/remove"),
            Some(&vera),
            ORIGIN,
            &[],
        )
        .await;
    accepted(&r, "remove item");

    // attachments: a file is required, only the uploader (or an admin) removes it
    let r = multipart(&app, &format!("{page}/attachments"), &vera, None).await;
    assert!(refused(&r, "no file").contains("Choose a file"));
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0\x90wS\xde";
    let r = multipart(
        &app,
        &format!("{page}/attachments"),
        &vera,
        Some(("shot.png", "image/png", png)),
    )
    .await;
    accepted(&r, "attach");
    assert!(r.header("location").unwrap().ends_with("#attachments"));
    let att: Uuid =
        sqlx::query_scalar("SELECT id FROM change_request_attachments WHERE request_id = $1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let r = app
        .get_page(&format!("/request-attachments/{att}"), Some(&walt))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r
        .header("content-disposition")
        .unwrap()
        .starts_with("attachment"));
    assert!(r
        .header("content-security-policy")
        .unwrap()
        .contains("sandbox"));
    let r = app
        .get_page(&format!("/request-attachments/{att}?inline=1"), Some(&walt))
        .await;
    assert!(
        r.header("content-disposition")
            .unwrap()
            .starts_with("inline"),
        "images inline on request"
    );
    let r = app
        .post_form(
            &format!("/request-attachments/{att}/delete"),
            Some(&walt),
            ORIGIN,
            &[],
        )
        .await;
    refused(&r, "others do not remove an attachment");
    assert_eq!(r.header("location").as_deref(), Some(page.as_str()));
    let r = app
        .post_form(
            &format!("/request-attachments/{att}/delete"),
            Some(&vera),
            ORIGIN,
            &[],
        )
        .await;
    accepted(&r, "remove attachment");
    let r = app
        .get_page(&format!("/request-attachments/{att}"), Some(&vera))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/request-attachments/{att}/delete"),
            Some(&vera),
            ORIGIN,
            &[],
        )
        .await;
    refused(&r, "remove twice");
    assert_eq!(
        r.header("location").as_deref(),
        Some("/requests"),
        "unknown: back to the list"
    );
    app.teardown().await;
}

#[tokio::test]
async fn requests_about_a_restricted_group_stay_hidden() {
    let app = app_or_skip!();
    let (_, mona_token) = app.user("mona", Role::Editor).await;
    app.user_in("eddi", Role::Editor, &["ROVER"]).await;
    // mona's own group, restricted to its members
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &mona_token,
            Some(json!({ "code": "LAB", "name": "Lab", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let secret = create_part(&app, &mona_token, "LAB", "Secret part").await;
    let mona = app.login("mona").await;
    let eddi = app.login("eddi").await;
    let id = raise(&app, &mona, "Secret problem", &secret).await;
    let page = format!("/requests/{id}");

    let r = app.get_page(&page, Some(&eddi)).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "an outsider does not see it"
    );
    let html = app
        .get_page("/requests?status=all", Some(&eddi))
        .await
        .text();
    assert!(!html.contains("Secret problem"), "nor in the list");
    for (uri, form) in [
        (format!("{page}/comments"), vec![("body", "Hello")]),
        (format!("{page}/triage"), vec![]),
        (format!("{page}/decide"), vec![("decision", "reject")]),
        (
            format!("{page}/items"),
            vec![("item_number", secret.as_str())],
        ),
    ] {
        let r = app.post_form(&uri, Some(&eddi), ORIGIN, &form).await;
        assert!(
            r.status == StatusCode::NOT_FOUND || flash(&r).is_some_and(|(k, _)| k == "error"),
            "outsider POST {uri}: {} {:?}",
            r.status,
            flash(&r)
        );
        if let Some((_, message)) = flash(&r) {
            assert!(!message.contains("Secret"), "{uri}: {message}");
        }
    }
    assert_eq!(status_of(&app, id).await, "new");
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM change_request_comments WHERE request_id = $1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(n, 0);
    // a new request naming the hidden item is refused without telling it exists
    let r = app
        .post_form(
            "/requests",
            Some(&eddi),
            ORIGIN,
            &[
                ("title", "Probe"),
                ("reason", "defect"),
                ("priority", "normal"),
                ("item_number", secret.as_str()),
            ],
        )
        .await;
    assert!(r.text().contains("not found"), "{}", r.text());
    app.teardown().await;
}
