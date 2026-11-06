//! Handler tests of the web modules `src/web/admin.rs` (project groups,
//! users, audit log, extraction templates) and `src/web/attributes.rs`
//! through the real router: who may see and change what, the cross-site
//! guard, validation errors as flash messages, conflicts, and that every
//! page renders.

mod common;

use axum::http::StatusCode;
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

/// The flash message a form action set: (`success`/`error`, message).
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

/// A form action that was refused: a redirect with an error message.
fn refused(r: &common::Response, what: &str) -> String {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{what}: {}", r.text());
    let (kind, message) = flash(r).unwrap_or_else(|| panic!("{what}: no flash message"));
    assert_eq!(kind, "error", "{what}: {message}");
    message
}

fn accepted(r: &common::Response, what: &str) -> String {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{what}: {}", r.text());
    let (kind, message) = flash(r).unwrap_or_else(|| panic!("{what}: no flash message"));
    assert_eq!(kind, "success", "{what}: {message}");
    message
}

async fn id_of(app: &common::TestApp, sql: &str, arg: &str) -> Uuid {
    sqlx::query_scalar(sql)
        .bind(arg)
        .fetch_one(&app.pool)
        .await
        .unwrap_or_else(|e| panic!("{sql} ({arg}): {e}"))
}

async fn count(app: &common::TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&app.pool).await.unwrap()
}

#[tokio::test]
async fn admin_pages_and_actions_need_an_administrator() {
    let app = app_or_skip!();
    app.user("root", Role::Admin).await;
    app.user("eddi", Role::Editor).await;
    app.user("vera", Role::Viewer).await;
    let admin = app.login("root").await;
    let vera_id = id_of(&app, "SELECT id FROM users WHERE username = $1", "vera").await;

    // an attribute to look at
    let r = app
        .post_form(
            "/admin/attributes",
            Some(&admin),
            ORIGIN,
            &[
                ("label", "Vendor"),
                ("key", "vendor"),
                ("data_type", "text"),
            ],
        )
        .await;
    accepted(&r, "create attribute");
    let attr = id_of(
        &app,
        "SELECT id FROM attribute_defs WHERE key = $1",
        "vendor",
    )
    .await;

    let pages = [
        "/admin/users".to_string(),
        format!("/admin/users/{vera_id}"),
        "/admin/audit?entity_type=user&username=root&action=user.create&page=2".to_string(),
        "/admin/attributes".to_string(),
        format!("/admin/attributes/{attr}"),
    ];
    for page in &pages {
        let r = app.get_page(page, Some(&admin)).await;
        assert_eq!(r.status, StatusCode::OK, "admin {page}: {}", r.text());
        let r = app.get_page(page, None).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "anonymous {page}");
        assert!(
            r.header("location").unwrap().starts_with("/login"),
            "anonymous {page}"
        );
    }
    for who in ["eddi", "vera"] {
        let cookie = app.login(who).await;
        for page in &pages {
            let r = app.get_page(page, Some(&cookie)).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} {page}");
        }
        // form actions: refused with a message, nothing changes
        for (uri, form) in [
            (
                "/admin/users".to_string(),
                vec![
                    ("username", "mallory"),
                    ("display_name", "M"),
                    ("role", "admin"),
                    ("password", "a-long-password-1"),
                ],
            ),
            (
                format!("/admin/users/{vera_id}"),
                vec![
                    ("display_name", "Vera"),
                    ("role", "admin"),
                    ("active", "on"),
                ],
            ),
            (
                format!("/admin/users/{vera_id}/password"),
                vec![("password", "another-password-1")],
            ),
            (
                "/admin/attributes".to_string(),
                vec![("label", "Cost"), ("key", "cost"), ("data_type", "number")],
            ),
            (
                format!("/admin/attributes/{attr}"),
                vec![("label", "Supplier"), ("active", "on")],
            ),
        ] {
            let r = app.post_form(&uri, Some(&cookie), ORIGIN, &form).await;
            refused(&r, &format!("{who} POST {uri}"));
        }
    }
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM users WHERE username = 'mallory'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM users WHERE role = 'admin'").await,
        1
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM attribute_defs").await, 1);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM attribute_defs WHERE label = 'Vendor'"
        )
        .await,
        1
    );
    // vera's password is unchanged: she still signs in with the old one
    app.login("vera").await;

    // cross-site posts never reach the handlers
    for uri in [
        "/admin/users",
        "/admin/attributes",
        &format!("/admin/users/{vera_id}/password"),
    ] {
        let r = app
            .post_form(
                uri,
                Some(&admin),
                Some("https://evil.example"),
                &[("label", "x"), ("key", "x")],
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
    }
    app.teardown().await;
}

#[tokio::test]
async fn user_administration_reports_bad_input_and_keeps_an_administrator() {
    let app = app_or_skip!();
    app.user("root", Role::Admin).await;
    let admin = app.login("root").await;
    let root_id = id_of(&app, "SELECT id FROM users WHERE username = $1", "root").await;

    for (form, expected) in [
        (
            vec![
                ("username", "x"),
                ("display_name", "X"),
                ("role", "editor"),
                ("password", "a-long-password-1"),
            ],
            "username",
        ),
        (
            vec![
                ("username", "newbie"),
                ("display_name", "N"),
                ("role", "wizard"),
                ("password", "a-long-password-1"),
            ],
            "Unknown role",
        ),
        (
            vec![
                ("username", "newbie"),
                ("display_name", "N"),
                ("role", "editor"),
                ("password", "short"),
            ],
            "assword",
        ),
        (
            vec![
                ("username", "root"),
                ("display_name", "R"),
                ("role", "editor"),
                ("password", "a-long-password-1"),
            ],
            "root",
        ),
    ] {
        let r = app
            .post_form("/admin/users", Some(&admin), ORIGIN, &form)
            .await;
        let message = refused(&r, &format!("{form:?}"));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
    }
    let r = app
        .post_form(
            "/admin/users",
            Some(&admin),
            ORIGIN,
            &[
                ("username", "newbie"),
                ("display_name", "Newbie"),
                ("email", "n@example.org"),
                ("role", "editor"),
                ("password", "a-long-password-1"),
            ],
        )
        .await;
    assert!(accepted(&r, "create").contains("newbie"));
    let newbie = id_of(&app, "SELECT id FROM users WHERE username = $1", "newbie").await;

    // the last administrator can neither demote nor deactivate themselves
    for form in [
        vec![
            ("display_name", "Root"),
            ("role", "editor"),
            ("active", "on"),
        ],
        vec![("display_name", "Root"), ("role", "admin")],
    ] {
        let r = app
            .post_form(
                &format!("/admin/users/{root_id}"),
                Some(&admin),
                ORIGIN,
                &form,
            )
            .await;
        assert!(refused(&r, "last admin").contains("administrator must remain"));
    }
    let r = app
        .post_form(
            &format!("/admin/users/{newbie}"),
            Some(&admin),
            ORIGIN,
            &[
                ("display_name", "Newbie"),
                ("role", "nobody"),
                ("active", "on"),
            ],
        )
        .await;
    assert!(refused(&r, "unknown role").contains("Unknown role"));
    let r = app
        .post_form(
            &format!("/admin/users/{newbie}"),
            Some(&admin),
            ORIGIN,
            &[("display_name", " "), ("role", "viewer"), ("active", "on")],
        )
        .await;
    refused(&r, "empty display name");
    let r = app
        .post_form(
            &format!("/admin/users/{newbie}"),
            Some(&admin),
            ORIGIN,
            &[
                ("display_name", "New Bie"),
                ("role", "viewer"),
                ("active", "on"),
            ],
        )
        .await;
    accepted(&r, "update");
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM users WHERE username = 'newbie' AND role = 'viewer' AND display_name = 'New Bie'").await,
        1
    );
    let r = app
        .post_form(
            &format!("/admin/users/{newbie}/password"),
            Some(&admin),
            ORIGIN,
            &[("password", "x")],
        )
        .await;
    refused(&r, "short password");
    let r = app
        .post_form(
            &format!("/admin/users/{newbie}/password"),
            Some(&admin),
            ORIGIN,
            &[("password", "a-new-long-password-2")],
        )
        .await;
    assert!(accepted(&r, "reset").contains("signed out"));

    // unknown users
    let nobody = Uuid::new_v4();
    let r = app
        .get_page(&format!("/admin/users/{nobody}"), Some(&admin))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/admin/users/{nobody}"),
            Some(&admin),
            ORIGIN,
            &[("display_name", "X"), ("role", "viewer")],
        )
        .await;
    refused(&r, "unknown user");
    app.teardown().await;
}

#[tokio::test]
async fn attribute_forms_report_bad_input() {
    let app = app_or_skip!();
    app.user("root", Role::Admin).await;
    let admin = app.login("root").await;
    for (form, expected) in [
        (
            vec![
                ("label", "Vendor"),
                ("key", "9vendor"),
                ("data_type", "text"),
            ],
            "key",
        ),
        (
            vec![
                ("label", "Finish"),
                ("key", "finish"),
                ("data_type", "choice"),
            ],
            "at least one choice",
        ),
        (
            vec![
                ("label", "Finish"),
                ("key", "finish"),
                ("data_type", "text"),
                ("choices", "a"),
            ],
            "Only choice lists",
        ),
        (
            vec![
                ("label", "Finish"),
                ("key", "finish"),
                ("data_type", "colour"),
            ],
            "Data type",
        ),
        (
            vec![
                ("label", "Finish"),
                ("key", "finish"),
                ("data_type", "text"),
                ("position", "first"),
            ],
            "whole number",
        ),
        (
            vec![
                ("label", "Finish"),
                ("key", "finish"),
                ("data_type", "text"),
                ("item_type", "widget"),
            ],
            "item type",
        ),
        (
            vec![("label", " "), ("key", "finish"), ("data_type", "text")],
            "Label",
        ),
    ] {
        let r = app
            .post_form("/admin/attributes", Some(&admin), ORIGIN, &form)
            .await;
        let message = refused(&r, &format!("{form:?}"));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
    }
    assert_eq!(count(&app, "SELECT COUNT(*) FROM attribute_defs").await, 0);
    let r = app
        .post_form(
            "/admin/attributes",
            Some(&admin),
            ORIGIN,
            &[
                ("label", "Surface"),
                ("key", "surface"),
                ("data_type", "choice"),
                ("choices", "anodized\npainted"),
                ("item_type", "part"),
                ("item_type", "assembly"),
            ],
        )
        .await;
    accepted(&r, "create");
    let r = app
        .post_form(
            "/admin/attributes",
            Some(&admin),
            ORIGIN,
            &[
                ("label", "Surface again"),
                ("key", "Surface"),
                ("data_type", "text"),
            ],
        )
        .await;
    assert!(refused(&r, "duplicate key").contains("already in use"));
    let id = id_of(
        &app,
        "SELECT id FROM attribute_defs WHERE key = $1",
        "surface",
    )
    .await;
    let r = app
        .post_form(
            &format!("/admin/attributes/{id}"),
            Some(&admin),
            ORIGIN,
            &[("label", "Surface"), ("position", "5")],
        )
        .await;
    assert!(refused(&r, "choices removed").contains("at least one choice"));
    let r = app
        .post_form(
            &format!("/admin/attributes/{id}"),
            Some(&admin),
            ORIGIN,
            &[
                ("label", "Surface finish"),
                ("choices", "painted"),
                ("position", "5"),
            ],
        )
        .await;
    accepted(&r, "update without the active box");
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM attribute_defs WHERE NOT active AND label = 'Surface finish' AND item_types = '{}'").await,
        1,
        "unticked boxes: inactive, all item types"
    );
    let html = app
        .get_page(&format!("/admin/attributes/{id}"), Some(&admin))
        .await
        .text();
    assert!(html.contains("Surface finish") && html.contains("painted"));

    let nobody = Uuid::new_v4();
    let r = app
        .get_page(&format!("/admin/attributes/{nobody}"), Some(&admin))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/admin/attributes/{nobody}"),
            Some(&admin),
            ORIGIN,
            &[("label", "X")],
        )
        .await;
    refused(&r, "unknown attribute");
    app.teardown().await;
}

#[tokio::test]
async fn project_group_forms_check_managers_and_input() {
    let app = app_or_skip!();
    app.user("root", Role::Admin).await;
    app.user("mona", Role::Editor).await; // creates the group: its manager
    app.user_in("eddi", Role::Editor, &["ROVER"]).await;
    app.user("vera", Role::Viewer).await;
    let mona = app.login("mona").await;
    let eddi = app.login("eddi").await;
    let vera = app.login("vera").await;

    for (form, expected) in [
        (vec![("code", "bad code!"), ("name", "X")], "code"),
        (vec![("code", "LAB"), ("name", " ")], "name"),
        (vec![("code", "ROVER"), ("name", "Twin")], "ROVER"),
    ] {
        let r = app.post_form("/projects", Some(&mona), ORIGIN, &form).await;
        let message = refused(&r, &format!("{form:?}"));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
    }
    let r = app
        .post_form(
            "/projects",
            Some(&vera),
            ORIGIN,
            &[("code", "VIEW"), ("name", "View")],
        )
        .await;
    refused(&r, "viewers create no groups");
    let r = app
        .post_form(
            "/projects",
            Some(&mona),
            ORIGIN,
            &[("code", "LAB"), ("name", "Lab"), ("restricted", "on")],
        )
        .await;
    accepted(&r, "create restricted group");
    let lab = id_of(&app, "SELECT id FROM projects WHERE code = $1", "LAB").await;
    let page = format!("/projects/{lab}");

    // outsiders do not see a restricted group, and cannot act on it
    for cookie in [&eddi, &vera] {
        let r = app.get_page(&page, Some(cookie)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    for (uri, form) in [
        (page.clone(), vec![("name", "Taken over"), ("active", "on")]),
        (format!("{page}/numbering"), vec![("scheme", "{SEQ:3}")]),
        (
            format!("{page}/visibility"),
            vec![("visibility", "internal")],
        ),
        (format!("{page}/members"), vec![("username", "eddi")]),
        (
            format!("{page}/result-templates"),
            vec![
                ("name", "T"),
                ("file_pattern", "*.log"),
                ("method", "regex"),
                ("pattern", "x=(\\d+)"),
            ],
        ),
    ] {
        let r = app.post_form(&uri, Some(&eddi), ORIGIN, &form).await;
        assert!(
            r.status == StatusCode::NOT_FOUND || flash(&r).is_some_and(|(k, _)| k == "error"),
            "outsider POST {uri}: {} {:?}",
            r.status,
            flash(&r)
        );
    }
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM projects WHERE name = 'Taken over' OR number_scheme IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM projects WHERE code = 'LAB' AND visibility = 'restricted'"
        )
        .await,
        1
    );

    // the manager: bad input is reported, good input saved
    let r = app
        .post_form(
            &format!("{page}/visibility"),
            Some(&mona),
            ORIGIN,
            &[("visibility", "secret")],
        )
        .await;
    assert!(refused(&r, "visibility").contains("internal or restricted"));
    let r = app
        .post_form(
            &format!("{page}/numbering"),
            Some(&mona),
            ORIGIN,
            &[("scheme", "{TYPE}")],
        )
        .await;
    refused(&r, "scheme without sequence");
    let r = app
        .post_form(
            &format!("{page}/members"),
            Some(&mona),
            ORIGIN,
            &[("username", "nobody")],
        )
        .await;
    refused(&r, "unknown member");
    let r = app
        .post_form(
            &page,
            Some(&mona),
            ORIGIN,
            &[("name", " "), ("active", "on")],
        )
        .await;
    refused(&r, "empty name");
    let r = app
        .post_form(
            &format!("{page}/result-templates"),
            Some(&mona),
            ORIGIN,
            &[
                ("name", "Max stress"),
                ("file_pattern", "*.log"),
                ("method", "regex"),
                ("pattern", "stress=([0-9.]+)"),
                ("lower_limit", "low"),
            ],
        )
        .await;
    assert!(refused(&r, "template limit").contains("Lower limit"));
    let r = app
        .post_form(
            &format!("{page}/members"),
            Some(&mona),
            ORIGIN,
            &[("username", "eddi")],
        )
        .await;
    accepted(&r, "add member");
    let r = app.get_page(&page, Some(&eddi)).await;
    assert_eq!(r.status, StatusCode::OK, "a member sees the group");
    // a member who is not a manager cannot change the group
    let r = app
        .post_form(
            &page,
            Some(&eddi),
            ORIGIN,
            &[("name", "Mine now"), ("active", "on")],
        )
        .await;
    assert!(refused(&r, "non-manager").contains("managers"));
    let eddi_id = id_of(&app, "SELECT id FROM users WHERE username = $1", "eddi").await;
    // eddi leaves: back to the list, the restricted page is gone for him
    let r = app
        .post_form(
            &format!("{page}/members/{eddi_id}/remove"),
            Some(&eddi),
            ORIGIN,
            &[],
        )
        .await;
    assert_eq!(r.header("location").as_deref(), Some("/projects"));
    accepted(&r, "leave");
    let r = app.get_page(&page, Some(&eddi)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // pages render: the list, the group with a preview, the admin's view
    let root = app.login("root").await;
    for (uri, cookie) in [
        ("/projects".to_string(), &mona),
        (format!("{page}?scheme=%7BPROJECT%7D-%7BSEQ:4%7D"), &mona),
        (format!("{page}?scheme=%7Bbroken"), &mona),
        (page.clone(), &root),
    ] {
        let r = app.get_page(&uri, Some(cookie)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    }
    let r = app
        .get_page(&format!("/projects/{}", Uuid::new_v4()), Some(&root))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/result-templates/{}/delete", Uuid::new_v4()),
            Some(&root),
            ORIGIN,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.teardown().await;
}
