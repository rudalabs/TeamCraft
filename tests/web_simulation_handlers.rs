//! Handler tests of `src/web/simulations.rs` through the real router: basis
//! links and result values of a simulation item – who may change them,
//! validation errors as flash messages, the CSV export and import, the
//! comparison page, and that a `back` target never leaves the site.

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

async fn upload(
    app: &common::TestApp,
    uri: &str,
    cookie: &str,
    file: Option<(&str, &[u8])>,
    fields: &[(&str, &str)],
) -> common::Response {
    let boundary = "teamcraft-sim-boundary";
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    if let Some((name, bytes)) = file {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/csv\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
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

async fn create(app: &common::TestApp, token: &str, item_type: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn revision_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1 ORDER BY r.seq DESC LIMIT 1",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn count(app: &common::TestApp, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn simulation_bases_and_results_through_the_forms() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("eddi", Role::Editor, &["ROVER"]).await;
    app.user_in("otto", Role::Editor, &["PUMP"]).await;
    app.user("vera", Role::Viewer).await;
    let part = create(&app, &token, "part", "Duct").await;
    let sim = create(&app, &token, "simulation", "Duct flow").await;
    let rev = revision_id(&app, &sim).await;
    let eddi = app.login("eddi").await;
    let otto = app.login("otto").await;
    let vera = app.login("vera").await;
    let bases = format!("/revisions/{rev}/simulation/bases");
    let results = format!("/revisions/{rev}/simulation/results");

    // basis links: the CAD editors of the group, a known role and item
    for (cookie, form, expected) in [
        (
            &eddi,
            vec![("item", part.as_str()), ("role", "colour")],
            "role of the basis",
        ),
        (
            &eddi,
            vec![("item", "NOPE-404"), ("role", "geometry")],
            "not found",
        ),
        (
            &vera,
            vec![("item", part.as_str()), ("role", "geometry")],
            "",
        ),
        (
            &otto,
            vec![("item", part.as_str()), ("role", "geometry")],
            "",
        ),
    ] {
        let r = app.post_form(&bases, Some(cookie), ORIGIN, &form).await;
        let message = refused(&r, &format!("{form:?}"));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
        assert!(r.header("location").unwrap().ends_with("#simulation-basis"));
    }
    let r = app
        .post_form(
            &bases,
            Some(&eddi),
            ORIGIN,
            &[
                ("item", part.as_str()),
                ("role", "geometry"),
                ("note", "CAD model"),
            ],
        )
        .await;
    assert!(accepted(&r, "basis").contains(&part));
    let basis: Uuid = sqlx::query_scalar("SELECT id FROM simulation_bases WHERE revision_id = $1")
        .bind(rev)
        .fetch_one(&app.pool)
        .await
        .unwrap();

    // result values: a number, limits that are numbers, editors only
    for (cookie, form, expected) in [
        (
            &eddi,
            vec![("name", "Pressure drop"), ("value", "")],
            "Enter the value",
        ),
        (
            &eddi,
            vec![("name", "Pressure drop"), ("value", "low")],
            "not a number",
        ),
        (
            &eddi,
            vec![
                ("name", "Pressure drop"),
                ("value", "12,5"),
                ("upper_limit", "high"),
            ],
            "Upper limit",
        ),
        (&eddi, vec![("name", " "), ("value", "1")], "ame"),
        (&vera, vec![("name", "Pressure drop"), ("value", "12")], ""),
        (&otto, vec![("name", "Pressure drop"), ("value", "12")], ""),
    ] {
        let r = app.post_form(&results, Some(cookie), ORIGIN, &form).await;
        let message = refused(&r, &format!("{form:?}"));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
    }
    let r = app
        .post_form(
            &results,
            Some(&eddi),
            ORIGIN,
            &[
                ("name", "Pressure drop"),
                ("value", "12,5"),
                ("unit", "Pa"),
                ("upper_limit", "20"),
                ("load_case", "LC1"),
            ],
        )
        .await;
    assert!(accepted(&r, "result").contains("Pressure drop"));
    let result: Uuid =
        sqlx::query_scalar("SELECT id FROM simulation_results WHERE revision_id = $1")
            .bind(rev)
            .fetch_one(&app.pool)
            .await
            .unwrap();

    // update: bad input refused; a foreign back target falls back to the site
    let update = format!("/simulation-results/{result}");
    let r = app
        .post_form(
            &update,
            Some(&eddi),
            ORIGIN,
            &[
                ("name", "Pressure drop"),
                ("value", "x"),
                ("back", "/somewhere"),
            ],
        )
        .await;
    refused(&r, "bad update");
    assert_eq!(r.header("location").as_deref(), Some("/somewhere"));
    for evil in ["https://evil.example/", "//evil.example", "/\\evil.example"] {
        let r = app
            .post_form(
                &update,
                Some(&eddi),
                ORIGIN,
                &[
                    ("name", "Pressure drop"),
                    ("value", "13"),
                    ("unit", "Pa"),
                    ("back", evil),
                ],
            )
            .await;
        accepted(&r, "update");
        assert_eq!(r.header("location").as_deref(), Some("/"), "back={evil}");
    }
    let r = app
        .post_form(
            &update,
            Some(&vera),
            ORIGIN,
            &[("name", "Hacked"), ("value", "0")],
        )
        .await;
    refused(&r, "viewer update");

    // CSV export and import
    let r = app
        .get_page(
            &format!("/revisions/{rev}/simulation/results.csv"),
            Some(&vera),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-type").unwrap().starts_with("text/csv"));
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains(&format!("{sim}_A_results.csv")));
    assert!(r.text().contains("Pressure drop"));
    let import = format!("{results}/import");
    let r = upload(&app, &import, &eddi, None, &[]).await;
    assert!(refused(&r, "no file").contains("Choose a CSV or Excel file"));
    let r = upload(
        &app,
        &import,
        &eddi,
        Some(("bad.csv", b"name,value\nVelocity,fast\n")),
        &[],
    )
    .await;
    refused(&r, "bad value");
    let r = upload(
        &app,
        &import,
        &vera,
        Some(("ok.csv", b"name,value,unit\nVelocity,3.2,m/s\n")),
        &[],
    )
    .await;
    refused(&r, "viewer import");
    let r = upload(
        &app,
        &import,
        &eddi,
        Some((
            "ok.csv",
            b"name,value,unit,limit_low,limit_high,load_case\nVelocity,3.2,m/s,,5,LC1\n",
        )),
        &[("replace", "1")],
    )
    .await;
    assert!(accepted(&r, "import").contains("Imported result values"));
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM simulation_results WHERE revision_id = $1",
            rev
        )
        .await,
        1,
        "replace: only the imported value is left"
    );

    // the comparison needs a second revision
    let r = app
        .get_page(&format!("/revisions/{rev}/simulation/compare"), Some(&eddi))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("only one revision"));
    let r = app
        .get_page(
            &format!("/revisions/{rev}/simulation/compare?with=not-a-uuid"),
            Some(&eddi),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .get_page(
            &format!("/revisions/{rev}/simulation/compare?with={rev}"),
            Some(&eddi),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "comparing with itself renders: {}",
        r.text()
    );

    // removing: editors only
    let remaining: Uuid =
        sqlx::query_scalar("SELECT id FROM simulation_results WHERE revision_id = $1")
            .bind(rev)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    for (uri, what) in [
        (format!("/simulation-results/{remaining}/delete"), "result"),
        (format!("/simulation-bases/{basis}/delete"), "basis"),
    ] {
        let r = app
            .post_form(&uri, Some(&vera), ORIGIN, &[("back", "/items")])
            .await;
        refused(&r, &format!("viewer removes the {what}"));
        let r = app
            .post_form(&uri, Some(&eddi), ORIGIN, &[("back", "/items")])
            .await;
        accepted(&r, &format!("remove the {what}"));
        assert_eq!(r.header("location").as_deref(), Some("/items"));
        let r = app.post_form(&uri, Some(&eddi), ORIGIN, &[]).await;
        refused(&r, &format!("remove the {what} twice"));
    }

    // proposals that do not exist
    for action in ["accept", "reject"] {
        let r = app
            .post_form(
                &format!("/simulation-proposals/{}/{action}", Uuid::new_v4()),
                Some(&eddi),
                ORIGIN,
                &[],
            )
            .await;
        refused(&r, action);
    }
    // cross-site posts are blocked
    let r = app
        .post_form(
            &results,
            Some(&eddi),
            Some("https://evil.example"),
            &[("name", "CSRF"), ("value", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // the revision page with all of it renders
    let r = app
        .get_page(&format!("/revisions/{rev}"), Some(&vera))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    app.teardown().await;
}
