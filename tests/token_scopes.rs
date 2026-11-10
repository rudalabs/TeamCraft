//! Scoped personal access tokens (threat model F2): a `read_only` token
//! only reads (GET/HEAD and the read-only MCP tools), an `mcp_only` token
//! only reaches `POST /api/v1/mcp`; every refusal writes nothing. Tokens
//! get their scope on the account page, through `POST /api/v1/tokens` and
//! the CLI (`tests/cli.rs`); a sliding lifetime renews on use (the desktop
//! app's token, `tests/web_ui.rs`).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    auth::Actor,
    domain::{Role, TokenScope},
    services::users::{self, TokenLifetime},
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const SAME_ORIGIN: Option<&str> = Some("http://localhost:8080");

async fn token(app: &common::TestApp, actor: &Actor, scope: TokenScope) -> String {
    users::create_token(&app.pool, actor, "scoped", TokenLifetime::Days(30), scope)
        .await
        .expect("create token")
        .1
}

async fn count(app: &common::TestApp, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn rpc(app: &common::TestApp, token: &str, method: &str, params: Value) -> Value {
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            token,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{method}: {}", r.text());
    let v = r.json();
    assert!(v.get("error").is_none(), "{v}");
    v["result"].clone()
}

async fn tool(app: &common::TestApp, token: &str, name: &str, args: Value) -> Value {
    rpc(
        app,
        token,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await
}

fn assert_refused(r: &common::Response, what: &str, message: &str) {
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{what}: {}", r.text());
    assert_eq!(r.json()["error"]["code"], "forbidden", "{what}");
    assert!(r.text().contains(message), "{what}: {}", r.text());
}

#[tokio::test]
async fn read_only_tokens_only_read() {
    let app = app_or_skip!();
    let (erika, full) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &full,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let read = token(&app, &erika, TokenScope::ReadOnly).await;

    // reads work, with the user's rights
    let r = app.api(Method::GET, "/api/v1/items", &read, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::GET, &format!("/api/v1/items/{part}"), &read, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.api(Method::GET, "/api/v1/me", &read, None).await;
    assert_eq!(r.json()["token_scope"], "read_only");
    let r = app.api(Method::GET, "/api/v1/tokens", &read, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // every other method is refused before the handler runs: nothing written
    let (items, tokens, changes) = (
        count(&app, "items").await,
        count(&app, "api_tokens").await,
        count(&app, "changes").await,
    );
    let message = "This token is read-only";
    for (method, uri, body) in [
        (
            Method::POST,
            "/api/v1/items".to_string(),
            Some(json!({ "item_type": "part", "name": "Not allowed", "project": "ROCKET" })),
        ),
        (
            Method::PATCH,
            format!("/api/v1/items/{part}"),
            Some(json!({ "unit": "kg" })),
        ),
        (
            Method::POST,
            format!("/api/v1/items/{part}/revisions/latest/checkin"),
            Some(json!({})),
        ),
        (
            Method::POST,
            "/api/v1/tokens".to_string(),
            Some(json!({ "name": "escalate", "scope": "full" })),
        ),
        (
            Method::POST,
            "/api/v1/assistant".to_string(),
            Some(json!({ "question": "Release everything" })),
        ),
        (Method::DELETE, "/api/v1/assistant/thread".to_string(), None),
        (
            Method::PUT,
            "/api/v1/projects/ROCKET/visibility".to_string(),
            Some(json!({ "visibility": "restricted" })),
        ),
    ] {
        let r = app.api(method.clone(), &uri, &read, body).await;
        assert_refused(&r, &format!("{method} {uri}"), message);
    }
    // revoking is a write too (with a full token or on the account page)
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM api_tokens WHERE scope = 'read_only'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(Method::DELETE, &format!("/api/v1/tokens/{id}"), &read, None)
        .await;
    assert_refused(&r, "DELETE /api/v1/tokens/{id}", message);
    assert_eq!(count(&app, "items").await, items);
    assert_eq!(count(&app, "api_tokens").await, tokens);

    // MCP: only the read-only tools are listed and callable
    let init = rpc(&app, &read, "initialize", json!({})).await;
    assert!(
        init["instructions"].as_str().unwrap().contains("read-only"),
        "{init}"
    );
    let listed = rpc(&app, &read, "tools/list", json!({})).await;
    let listed = listed["tools"].as_array().unwrap();
    assert!(!listed.is_empty());
    assert!(
        listed
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true),
        "{listed:?}"
    );
    assert!(!listed.iter().any(|t| t["name"] == "create_change_draft"));
    let res = tool(&app, &read, "search_items", json!({ "q": "Fin" })).await;
    assert_eq!(res["isError"], false, "{res}");
    for (name, args) in [
        (
            "create_change_draft",
            json!({ "title": "Not allowed", "items": [part] }),
        ),
        (
            "create_change_request",
            json!({ "title": "Not allowed", "description": "x", "items": [part] }),
        ),
    ] {
        let res = tool(&app, &read, name, args).await;
        assert_eq!(res["isError"], true, "{name}: {res}");
        assert!(res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(message));
    }
    assert_eq!(count(&app, "changes").await, changes);
    assert_eq!(count(&app, "change_requests").await, 0);

    // the same user's full token still writes
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{part}"),
            &full,
            Some(json!({ "unit": "kg" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    app.teardown().await;
}

#[tokio::test]
async fn mcp_only_tokens_only_reach_the_mcp_endpoint() {
    let app = app_or_skip!();
    let (erika, full) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &full,
            json!({ "item_type": "part", "name": "Nose cone", "project": "ROCKET" }),
        )
        .await;
    let mcp = token(&app, &erika, TokenScope::McpOnly).await;

    let message = "This token may only call the MCP endpoint";
    let items = count(&app, "items").await;
    for (method, uri, body) in [
        (Method::GET, "/api/v1/me".to_string(), None),
        (Method::GET, "/api/v1/items".to_string(), None),
        (Method::GET, format!("/api/v1/items/{part}"), None),
        (Method::GET, "/api/v1/tokens".to_string(), None),
        (Method::GET, "/api/v1/assistant".to_string(), None),
        (
            Method::POST,
            "/api/v1/items".to_string(),
            Some(json!({ "item_type": "part", "name": "Not allowed", "project": "ROCKET" })),
        ),
        (
            Method::POST,
            "/api/v1/tokens".to_string(),
            Some(json!({ "name": "escalate" })),
        ),
        (
            Method::POST,
            "/api/v1/assistant".to_string(),
            Some(json!({ "question": "hi" })),
        ),
    ] {
        let r = app.api(method.clone(), &uri, &mcp, body).await;
        assert_refused(&r, &format!("{method} {uri}"), message);
    }
    assert_eq!(count(&app, "items").await, items);

    // the MCP endpoint itself: every tool, with the user's rights
    let listed = rpc(&app, &mcp, "tools/list", json!({})).await;
    assert!(listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "create_change_draft"));
    let res = tool(&app, &mcp, "get_item", json!({ "item": part })).await;
    assert_eq!(res["isError"], false, "{res}");
    let res = tool(
        &app,
        &mcp,
        "create_change_draft",
        json!({ "title": "Nose cone release", "items": [part] }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");
    assert_eq!(count(&app, "changes").await, 1);

    app.teardown().await;
}

#[tokio::test]
async fn tokens_are_managed_through_the_api_with_a_scope() {
    let app = app_or_skip!();
    let (erika, full) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, max) = app.user("max", Role::Editor).await;

    let r = app
        .api(
            Method::POST,
            "/api/v1/tokens",
            &full,
            Some(json!({ "name": "report bot", "scope": "read_only", "expires_in_days": 30 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let created = r.json();
    let value = created["token"].as_str().unwrap().to_string();
    assert!(value.starts_with("tc_pat_"));
    assert_eq!(created["scope"], "read_only");
    assert!(!created["expires_at"].is_null());
    let id = created["id"].as_str().unwrap().to_string();
    // the new token is read-only indeed
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &value,
            Some(json!({ "item_type": "part", "name": "x", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // audited with the scope, never the value
    let (summary, details): (String, Value) = sqlx::query_as(
        "SELECT summary, details FROM audit_log
         WHERE action = 'token.create' AND details->>'token_id' = $1",
    )
    .bind(&id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(details["scope"], "read_only");
    assert!(summary.contains("read-only"), "{summary}");
    assert!(!details.to_string().contains(&value));

    // the default scope is full; no expiry = never
    let r = app
        .api(
            Method::POST,
            "/api/v1/tokens",
            &full,
            Some(json!({ "name": "ci" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["scope"], "full");
    assert!(r.json()["expires_at"].is_null());

    // refusals: unknown scope, lifetime out of range, no name – nothing created
    let before = count(&app, "api_tokens").await;
    for body in [
        json!({ "name": "x", "scope": "admin" }),
        json!({ "name": "x", "scope": "read" }),
        json!({ "name": "x", "expires_in_days": 0 }),
        json!({ "name": "x", "expires_in_days": 3651 }),
        json!({ "name": "  " }),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/tokens", &full, Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text());
    }
    assert_eq!(count(&app, "api_tokens").await, before);

    // the list shows scopes, never a value or a hash
    let r = app.api(Method::GET, "/api/v1/tokens", &full, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let text = r.text();
    assert!(!text.contains(&value) && !text.contains("token_hash"));
    let list = r.json();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == id.as_str() && t["scope"] == "read_only"));

    // another user cannot revoke it (not found), the owner can
    let r = app
        .api(Method::DELETE, &format!("/api/v1/tokens/{id}"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = app
        .api(Method::DELETE, &format!("/api/v1/tokens/{id}"), &full, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = app.api(Method::GET, "/api/v1/items", &value, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(app
        .audit_actions(erika.id)
        .await
        .contains(&"token.revoke".to_string()));

    app.teardown().await;
}

/// An admin revoking somebody else's token is recorded against the token's
/// owner (with the admin as actor), so it shows in the owner's history.
#[tokio::test]
async fn an_admin_revoke_is_recorded_on_the_owner() {
    let app = app_or_skip!();
    let (admin, admin_token) = app.user("root", Role::Admin).await;
    let (leaver, _) = app.user_in("lena", Role::Editor, &["ROVER"]).await;
    let value = token(&app, &leaver, TokenScope::Full).await;
    let id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM api_tokens WHERE user_id = $1 AND name = 'scoped'")
            .bind(leaver.id)
            .fetch_one(&app.pool)
            .await
            .unwrap();

    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/tokens/{id}"),
            &admin_token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = app.api(Method::GET, "/api/v1/items", &value, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "the token is gone");

    let (entity, actor, summary): (uuid::Uuid, uuid::Uuid, String) = sqlx::query_as(
        "SELECT entity_id, user_id, summary FROM audit_log WHERE action = 'token.revoke'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(entity, leaver.id, "on the owner");
    assert_eq!(actor, admin.id, "by the admin");
    assert!(summary.contains("of lena"), "{summary}");
    assert!(!app
        .audit_actions(admin.id)
        .await
        .contains(&"token.revoke".to_string()));

    app.teardown().await;
}

#[tokio::test]
async fn the_account_page_creates_scoped_tokens() {
    let app = app_or_skip!();
    app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("erika").await;

    let r = app
        .post_form(
            "/account/tokens",
            Some(&cookie),
            SAME_ORIGIN,
            &[
                ("name", "assistant"),
                ("expires_days", "30"),
                ("scope", "mcp_only"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("(MCP only)"), "{}", r.text());
    let scope: String = sqlx::query_scalar("SELECT scope FROM api_tokens WHERE name = 'assistant'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(scope, "mcp_only");

    // an unknown scope is refused with a message, nothing created
    let before = count(&app, "api_tokens").await;
    let r = app
        .post_form(
            "/account/tokens",
            Some(&cookie),
            SAME_ORIGIN,
            &[("name", "bad"), ("expires_days", "30"), ("scope", "root")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(count(&app, "api_tokens").await, before);

    // the page lists the scope
    let page = app.get_page("/account", Some(&cookie)).await;
    assert!(page.text().contains("MCP only"));

    app.teardown().await;
}

#[tokio::test]
async fn sliding_tokens_are_renewed_on_use_and_die_unused() {
    let app = app_or_skip!();
    let (erika, _) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (sliding_record, sliding) = users::create_token(
        &app.pool,
        &erika,
        "desktop",
        TokenLifetime::IdleDays(30),
        TokenScope::Full,
    )
    .await
    .unwrap();
    assert_eq!(sliding_record.idle_days, Some(30));
    let (fixed_record, fixed) = users::create_token(
        &app.pool,
        &erika,
        "fixed",
        TokenLifetime::Days(30),
        TokenScope::Full,
    )
    .await
    .unwrap();
    assert_eq!(fixed_record.idle_days, None);

    // both close to their expiry, unused for a while
    sqlx::query(
        "UPDATE api_tokens SET expires_at = now() + interval '1 day',
                               last_used_at = now() - interval '29 days'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let expiry = |id: uuid::Uuid| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, f64>(
                "SELECT EXTRACT(EPOCH FROM expires_at - now())::float8 / 86400 FROM api_tokens WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    for t in [&sliding, &fixed] {
        let r = app.api(Method::GET, "/api/v1/me", t, None).await;
        assert_eq!(r.status, StatusCode::OK);
    }
    // a use renews the sliding token for another 30 days, not the fixed one
    let days = expiry(sliding_record.id).await;
    assert!((29.9..=30.1).contains(&days), "{days}");
    let days = expiry(fixed_record.id).await;
    assert!(days < 1.1, "{days}");

    // unused past its idle time: refused like any expired token
    sqlx::query("UPDATE api_tokens SET expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(sliding_record.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app.api(Method::GET, "/api/v1/me", &sliding, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    app.teardown().await;
}

/// A `write` token does everything but approve: it is refused (403, before
/// the route runs) on the review of a change, the decision on a change
/// request and making a revision obsolete, and it cannot mint a `full` token.
#[tokio::test]
async fn write_tokens_cannot_approve_or_mint_full_tokens() {
    let app = app_or_skip!();
    let (erika, _) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let write = token(&app, &erika, TokenScope::Write).await;
    let full = token(&app, &erika, TokenScope::Full).await;

    // reads and writes work
    let r = app.api(Method::GET, "/api/v1/me", &write, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["token_scope"], "write");
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &write,
            Some(json!({ "item_type": "part", "name": "Nose cone", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let items = count(&app, "items").await;

    // approving routes are refused for the write token, whatever exists
    for path in [
        "/api/v1/changes/ECO-000001/review",
        "/api/v1/change-requests/CR-000001/decide",
        "/api/v1/items/PRT-000001/revisions/A/obsolete",
    ] {
        let r = app
            .api(
                Method::POST,
                path,
                &write,
                Some(json!({ "decision": "approve" })),
            )
            .await;
        assert_refused(&r, path, "may not approve");
        // the full token passes the scope check (the object does not exist)
        let r = app
            .api(
                Method::POST,
                path,
                &full,
                Some(json!({ "decision": "approve" })),
            )
            .await;
        assert_ne!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text());
    }

    // no escalation: a write token creates write (or narrower) tokens only
    let r = app
        .api(
            Method::POST,
            "/api/v1/tokens",
            &write,
            Some(json!({ "name": "esc", "scope": "full" })),
        )
        .await;
    assert_refused(&r, "mint full", "cannot create a token that may approve");
    let r = app
        .api(
            Method::POST,
            "/api/v1/tokens",
            &write,
            Some(json!({ "name": "ok", "scope": "write" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["scope"], "write");
    assert_eq!(count(&app, "items").await, items);

    app.teardown().await;
}

/// Tokens that exist before the migration keep their rights: the column
/// default is `full` (read, write and approve).
#[tokio::test]
async fn tokens_without_a_choice_are_full() {
    let app = app_or_skip!();
    let (erika, _) = app.user("erika", Role::Editor).await;
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES (gen_random_uuid(), $1, 'old', 'hash-old', 'tc_pat_old')",
    )
    .bind(erika.id)
    .execute(&app.pool)
    .await
    .unwrap();
    let scope: String = sqlx::query_scalar("SELECT scope FROM api_tokens WHERE name = 'old'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(scope, "full");
    app.teardown().await;
}
