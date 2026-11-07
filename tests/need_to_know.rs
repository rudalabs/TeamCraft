//! Need-to-know project groups, step 1 (issue #151): how administrators read
//! a restricted group. `implicit` (no policy row) keeps today's behaviour;
//! `with_reason` hides the group from an administrator who is not a member
//! until they open a time-limited, audited elevation; `none` hides it for
//! good unless they join as a member. Managers are notified, and every
//! change of the policy is audited.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{domain::Role, services::notifications};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// A restricted group `NTK` owned by `erika` (member and manager) with one
/// item; returns the item number.
async fn group_with_item(app: &common::TestApp, owner_token: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            owner_token,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    app.create_item(
        owner_token,
        json!({ "item_type": "part", "name": "Secret part", "project": "NTK" }),
    )
    .await
}

async fn status(app: &common::TestApp, method: Method, uri: &str, token: &str) -> StatusCode {
    app.api(method, uri, token, None).await.status
}

async fn set_policy(app: &common::TestApp, token: &str, mode: &str) -> common::Response {
    app.api(
        Method::PUT,
        "/api/v1/projects/NTK/policy",
        token,
        Some(json!({ "admin_read": mode })),
    )
    .await
}

async fn alerts(app: &common::TestApp, user: &str) -> Vec<String> {
    let actor = app.actor(user).await;
    notifications::list(&app.pool, &actor, false, 50)
        .await
        .expect("list notifications")
        .into_iter()
        .filter(|n| n.kind == "access_alert")
        .map(|n| format!("{} | {}", n.title, n.body))
        .collect()
}

#[tokio::test]
async fn without_a_policy_administrators_read_restricted_groups_as_before() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    let uri = format!("/api/v1/items/{item}");
    assert_eq!(status(&app, Method::GET, &uri, &root).await, StatusCode::OK);
    let r = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &root, None)
        .await;
    assert_eq!(r.json()["admin_read"], "implicit", "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn none_hides_the_group_from_administrators_but_not_the_management() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let (_, vera) = app.user("vera", Role::Editor).await;
    let item = group_with_item(&app, &erika).await;
    let uri = format!("/api/v1/items/{item}");

    // an outsider cannot touch the policy and learns nothing
    assert_eq!(
        set_policy(&app, &vera, "none").await.status,
        StatusCode::NOT_FOUND
    );
    // a manager sets it; the same value again changes nothing
    let r = set_policy(&app, &erika, "none").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::OK
    );

    // the administrator no longer reads the group ...
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );
    let groups = app
        .api(Method::GET, "/api/v1/projects", &root, None)
        .await
        .text();
    assert!(!groups.contains("\"NTK\""), "{groups}");
    // ... the member still does ...
    assert_eq!(
        status(&app, Method::GET, &uri, &erika).await,
        StatusCode::OK
    );
    // ... and the administrator still manages members and the policy
    assert_eq!(
        status(&app, Method::GET, "/api/v1/projects/NTK/members", &root).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, Method::GET, "/api/v1/projects/NTK/policy", &root).await,
        StatusCode::OK
    );
    // no elevation under `none`
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/elevations",
            &root,
            Some(json!({ "reason": "I would like to read this please" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // the audit log does not show the administrator what happened to the item
    let admin = app.actor("root").await;
    let recent = teamcraft::services::audit::recent(&app.pool, &admin, 200)
        .await
        .unwrap();
    assert!(
        recent.iter().all(|e| !e.summary.contains(&item)),
        "an entry about the hidden item reached the administrator"
    );

    // the policy change is audited with the previous value, once
    let id = app.project_id("NTK").await;
    let actions = app.audit_actions(id).await;
    assert_eq!(
        actions.iter().filter(|a| *a == "project.policy").count(),
        1,
        "{actions:?}"
    );
    app.teardown().await;
}

#[tokio::test]
async fn an_administrator_cannot_add_themselves_to_a_none_group_a_manager_adds_them() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::OK
    );
    // self-add is refused: the managers decide
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/members",
            &root,
            Some(json!({ "username": "root" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(
        status(&app, Method::GET, &format!("/api/v1/items/{item}"), &root).await,
        StatusCode::NOT_FOUND
    );
    // a manager adds the administrator
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/members",
            &erika,
            Some(json!({ "username": "root" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        status(&app, Method::GET, &format!("/api/v1/items/{item}"), &root).await,
        StatusCode::OK
    );
    // and as a member the administrator cannot promote themselves
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/members",
            &root,
            Some(json!({ "username": "root", "manager": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn with_reason_opens_a_limited_audited_elevation() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    let uri = format!("/api/v1/items/{item}");
    let elevations = "/api/v1/projects/NTK/elevations";
    assert_eq!(
        set_policy(&app, &root, "with_reason").await.status,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );

    // a reason is required, and only administrators elevate
    let short = app
        .api(
            Method::POST,
            elevations,
            &root,
            Some(json!({ "reason": "please" })),
        )
        .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST, "{}", short.text());
    let r = app
        .api(
            Method::POST,
            elevations,
            &erika,
            Some(json!({ "reason": "I am a manager, not an admin" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // with a reason the group is readable
    let r = app
        .api(
            Method::POST,
            elevations,
            &root,
            Some(json!({ "reason": "Customer asked for a data export" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let first = r.json()["id"].clone();
    assert_eq!(status(&app, Method::GET, &uri, &root).await, StatusCode::OK);
    // a second request returns the running one
    let again = app
        .api(
            Method::POST,
            elevations,
            &root,
            Some(json!({ "reason": "Customer asked for a data export" })),
        )
        .await;
    assert_eq!(again.json()["id"], first);

    // audited with the reason, the manager is told, the history lists it
    let id = app.project_id("NTK").await;
    let actions = app.audit_actions(id).await;
    assert_eq!(
        actions.iter().filter(|a| *a == "project.elevate").count(),
        1,
        "{actions:?}"
    );
    let detail: String = sqlx::query_scalar(
        "SELECT details::text FROM audit_log WHERE entity_id = $1 AND action = 'project.elevate'",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(
        detail.contains("Customer asked for a data export"),
        "{detail}"
    );
    let alerts = alerts(&app, "erika").await;
    assert!(
        alerts
            .iter()
            .any(|a| a.contains("Customer asked for a data export")),
        "{alerts:?}"
    );
    let policy = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &erika, None)
        .await
        .json();
    assert_eq!(policy["elevations"].as_array().unwrap().len(), 1);
    assert_eq!(policy["elevations"][0]["active"], true);

    // an expired elevation hides the group again
    sqlx::query("UPDATE project_elevations SET expires_at = now() - interval '1 minute'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );

    // a new one can be ended early
    let r = app
        .api(
            Method::POST,
            elevations,
            &root,
            Some(json!({ "reason": "Second look for the audit" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_ne!(r.json()["id"], first);
    assert_eq!(status(&app, Method::GET, &uri, &root).await, StatusCode::OK);
    assert_eq!(
        status(&app, Method::DELETE, elevations, &root).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(&app, Method::DELETE, elevations, &root).await,
        StatusCode::NOT_FOUND
    );
    app.teardown().await;
}

#[tokio::test]
async fn the_policy_needs_a_restricted_group_with_a_manager() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    // internal group: the policy builds on `restricted`
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika,
            Some(json!({ "code": "NTK", "name": "Open" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::CONFLICT
    );
    // an elevation on a group nobody restricted is pointless
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/elevations",
            &root,
            Some(json!({ "reason": "No need to ask at all" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // a restricted group without any manager cannot lock the administrators out
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &root,
            Some(json!({ "code": "NOMGR", "name": "No manager", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NOMGR/policy",
            &root,
            Some(json!({ "admin_read": "none" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // an unknown mode is a bad request
    assert_eq!(
        set_policy(&app, &erika, "sometimes").await.status,
        StatusCode::BAD_REQUEST
    );
    app.teardown().await;
}

#[tokio::test]
async fn opening_the_group_makes_the_policy_inert() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    let uri = format!("/api/v1/items/{item}");
    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );
    // the manager opens the group to all users: everybody reads it again
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/visibility",
            &erika,
            Some(json!({ "visibility": "internal" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status(&app, Method::GET, &uri, &root).await, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn the_project_page_of_a_hidden_group_shows_management_only() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    assert_eq!(
        set_policy(&app, &erika, "with_reason").await.status,
        StatusCode::OK
    );
    let cookie = app.login("root").await;
    let id = app.project_id("NTK").await;
    let page = app
        .get_page(&format!("/projects/{id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(
        html.contains("Open administrator access"),
        "no elevation form"
    );
    assert!(!html.contains(&item), "the page names an item of the group");

    // the form opens the access; the page then offers to end it
    let r = app
        .post_form(
            &format!("/projects/{id}/elevate"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("reason", "Reading the group for the audit")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = app
        .get_page(&format!("/projects/{id}"), Some(&cookie))
        .await;
    assert!(page.text().contains("End my administrator access"));
    app.teardown().await;
}

#[tokio::test]
async fn a_shut_out_administrator_cannot_undo_the_policy() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::OK
    );
    let uri = format!("/api/v1/items/{item}");

    // loosening the policy, opening the group, linking an IdP group, adding
    // oneself: all refused for an administrator who is no manager member
    assert_eq!(
        set_policy(&app, &root, "implicit").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        set_policy(&app, &root, "with_reason").await.status,
        StatusCode::FORBIDDEN
    );
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/visibility",
            &root,
            Some(json!({ "visibility": "internal" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let admin = app.actor("root").await;
    let pid = app.project_id("NTK").await;
    let err = teamcraft::services::projects::update(
        &app.pool,
        &admin,
        pid,
        teamcraft::services::projects::ProjectUpdate {
            name: "Need to know",
            description: "",
            active: true,
            oidc_group: Some("everybody"),
        },
    )
    .await
    .expect_err("linking an IdP group is refused");
    assert!(
        matches!(err, teamcraft::error::AppError::Forbidden(_)),
        "{err:?}"
    );
    assert_eq!(
        status(&app, Method::GET, &uri, &root).await,
        StatusCode::NOT_FOUND
    );

    // the managers can still loosen it
    assert_eq!(
        set_policy(&app, &erika, "with_reason").await.status,
        StatusCode::OK
    );
    // tightening is open to the administrator
    assert_eq!(set_policy(&app, &root, "none").await.status, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn a_shut_out_administrator_sees_neither_audit_page_search_nor_tools_of_the_group() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    let item = group_with_item(&app, &erika).await;
    let admin = app.actor("root").await;
    let page = || teamcraft::services::Page::from_page_number(Some(1), 500);
    let filter = teamcraft::services::audit::AuditFilter::default();
    let (before, _) = teamcraft::services::audit::list(&app.pool, &admin, &filter, page())
        .await
        .unwrap();
    assert!(before.iter().any(|e| e.summary.contains(&item)));

    assert_eq!(
        set_policy(&app, &erika, "none").await.status,
        StatusCode::OK
    );
    let admin = app.actor("root").await;
    let (entries, total) = teamcraft::services::audit::list(&app.pool, &admin, &filter, page())
        .await
        .unwrap();
    assert!(
        entries.iter().all(|e| !e.summary.contains(&item)),
        "audit::list names the hidden item"
    );
    assert_eq!(total as usize, entries.len(), "the count follows the rule");

    // search and the MCP endpoint answer nothing about the group
    let found = app
        .api(Method::GET, "/api/v1/items?q=Secret", &root, None)
        .await
        .text();
    assert!(!found.contains(&item), "{found}");
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            &root,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": "search_items",
                                     "arguments": { "q": "Secret" } } })),
        )
        .await;
    assert!(!r.text().contains(&item), "{}", r.text());

    // a webhook for the hidden group cannot be created
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &root,
            Some(json!({ "name": "Spy", "url": "https://example.org/hook",
                         "events": ["item.created"], "project": "NTK" })),
        )
        .await;
    assert!(
        r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::NOT_FOUND,
        "{} {}",
        r.status,
        r.text()
    );
    app.teardown().await;
}
