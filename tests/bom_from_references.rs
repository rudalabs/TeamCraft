//! A BOM from the CAD references the server found (#180): the plan, the
//! quantities from the instance counts of NX and SOLIDWORKS assemblies,
//! applying the chosen lines (one transaction, check-out required, audit),
//! per-line refusals, hidden children and the web preview.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
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

/// The NIST fixtures are several MiB each: a larger upload limit than the
/// test default.
macro_rules! big_or_skip {
    () => {
        match common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "64")]).await {
            Some(app) => app,
            None => return,
        }
    };
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A SOLIDWORKS assembly file (old format: file names as UTF-16 strings)
/// that references `names`.
fn sldasm(names: &[&str]) -> Vec<u8> {
    let mut out = b"\x00\x01".to_vec();
    for name in names {
        out.extend(utf16(&format!("C:\\Lib\\{name}")));
        out.extend([0, 0, 7, 0]);
    }
    out
}

async fn item(app: &common::TestApp, token: &str, kind: &str, project: &str) -> String {
    app.create_item(
        token,
        json!({ "item_type": kind, "name": format!("{kind} in {project}"), "project": project }),
    )
    .await
}

async fn upload(app: &common::TestApp, token: &str, number: &str, filename: &str, bytes: &[u8]) {
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            token,
            &[(filename, bytes)],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{filename}: {}", r.text());
}

async fn plan(app: &common::TestApp, token: &str, number: &str) -> common::Response {
    app.api(
        Method::GET,
        &format!("/api/v1/items/{number}/revisions/A/bom/from-references"),
        token,
        None,
    )
    .await
}

async fn apply(app: &common::TestApp, token: &str, number: &str, body: Value) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/items/{number}/revisions/A/bom/from-references"),
        token,
        Some(body),
    )
    .await
}

async fn checkout(app: &common::TestApp, token: &str, number: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{number}/revisions/A/checkout"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn bom(app: &common::TestApp, token: &str, number: &str) -> Vec<Value> {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/revisions/A/bom"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json().as_array().unwrap().clone()
}

async fn revision_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = 'A'",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

fn child<'a>(plan: &'a Value, number: &str) -> &'a Value {
    plan["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["item_number"] == number)
        .unwrap_or_else(|| panic!("{number} missing in {plan}"))
}

/// Plan, apply, repeat, differing quantities and the audit trail.
#[tokio::test]
async fn plan_and_apply_lines_from_the_cad_references() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;
    let (_, root) = app.user("root", Role::Admin).await;
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ARM'")
        .execute(&app.pool)
        .await
        .unwrap();

    let bracket = item(&app, &eng, "part", "ROCKET").await;
    upload(&app, &eng, &bracket, "Bracket.SLDPRT", b"\x00bracket").await;
    // a part in a group the engineer may not see
    let bolt = item(&app, &root, "part", "ARM").await;
    upload(&app, &root, &bolt, "Bolt M6.SLDPRT", b"\x00bolt").await;
    let stage = item(&app, &eng, "assembly", "ROCKET").await;
    upload(
        &app,
        &eng,
        &stage,
        "Stage.SLDASM",
        &sldasm(&["bracket.sldprt", "Bolt M6.SLDPRT", "Ghost.SLDPRT"]),
    )
    .await;

    // the plan: the bracket, the ghost is not in TeamCraft, the bolt is hidden
    let r = plan(&app, &eng, &stage).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = r.json();
    assert_eq!(p["children"].as_array().unwrap().len(), 1, "{p}");
    let c = child(&p, &bracket);
    assert_eq!(c["status"], "new");
    assert_eq!(c["quantity"], 1.0);
    assert_eq!(c["quantity_source"], "assumed");
    assert_eq!(c["selected"], true);
    assert_eq!(p["hidden"], 1, "counted, not named: {p}");
    assert!(!p.to_string().contains(&bolt), "{p}");
    let unmatched: Vec<&str> = p["not_in_teamcraft"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["name"].as_str().unwrap())
        .collect();
    assert_eq!(unmatched, ["Ghost.SLDPRT"], "{p}");
    // the plan changes nothing
    assert!(bom(&app, &eng, &stage).await.is_empty());
    // a reader sees the plan; applying needs the check-out
    assert_eq!(plan(&app, &vera, &stage).await.status, StatusCode::OK);
    let r = apply(&app, &eng, &stage, json!({ "children": [bracket] })).await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "not checked out: {}",
        r.text()
    );
    checkout(&app, &eng, &stage).await;
    // a viewer cannot apply
    let r = apply(&app, &vera, &stage, json!({ "children": [bracket] })).await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::CONFLICT,
        "{:?}",
        r.status
    );
    // refusals leave the BOM alone
    let r = apply(&app, &eng, &stage, json!({ "children": [] })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = apply(
        &app,
        &eng,
        &stage,
        json!({ "children": [bracket, "PRT-999999"] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = apply(&app, &eng, &stage, json!({ "children": [bolt] })).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "hidden looks like unknown"
    );
    assert!(bom(&app, &eng, &stage).await.is_empty());

    // apply
    let r = apply(&app, &eng, &stage, json!({ "children": [bracket] })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["added"], json!([bracket]));
    let lines = bom(&app, &eng, &stage).await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["child_number"], json!(bracket));
    assert_eq!(lines[0]["quantity"], 1.0);
    assert!(lines[0]["notes"].as_str().unwrap().contains("check it"));
    let rev = revision_id(&app, &stage).await;
    let actions = app.audit_actions(rev).await;
    assert!(actions.contains(&"bom.add".to_string()), "{actions:?}");
    assert!(
        actions.contains(&"bom.from_references".to_string()),
        "{actions:?}"
    );

    // again: already in the BOM, nothing duplicated
    let p = plan(&app, &eng, &stage).await.json();
    assert_eq!(child(&p, &bracket)["status"], "already_in_bom");
    assert_eq!(child(&p, &bracket)["selected"], false);
    let r = apply(&app, &eng, &stage, json!({ "children": [bracket] })).await;
    assert_eq!(r.json()["added"], json!([]));
    assert_eq!(r.json()["skipped"], json!([bracket]));
    assert_eq!(bom(&app, &eng, &stage).await.len(), 1);

    // a different quantity is shown and changed only when asked for
    let line_id = lines[0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/bom-lines/{line_id}"),
            &eng,
            Some(json!({ "quantity": 3, "find_number": 10, "notes": "" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = plan(&app, &eng, &stage).await.json();
    let c = child(&p, &bracket);
    assert_eq!(c["status"], "differs");
    assert_eq!(c["bom_quantity"], 3.0);
    let r = apply(&app, &eng, &stage, json!({ "children": [bracket] })).await;
    assert_eq!(r.json()["skipped"], json!([bracket]));
    assert_eq!(bom(&app, &eng, &stage).await[0]["quantity"], 3.0);
    let r = apply(
        &app,
        &eng,
        &stage,
        json!({ "children": [bracket], "update_quantities": true }),
    )
    .await;
    assert_eq!(r.json()["updated"], json!([bracket]), "{}", r.text());
    assert_eq!(bom(&app, &eng, &stage).await[0]["quantity"], 1.0);
    assert!(app
        .audit_actions(rev)
        .await
        .contains(&"bom.update".to_string()));
}

/// Drawings and parts do not describe the structure; a part-only
/// revision has an empty plan.
#[tokio::test]
async fn drawings_do_not_count_as_structure() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let part = item(&app, &eng, "part", "ROCKET").await;
    upload(&app, &eng, &part, "Bracket.SLDPRT", b"\x00bracket").await;
    let doc = item(&app, &eng, "document", "ROCKET").await;
    upload(
        &app,
        &eng,
        &doc,
        "Bracket.SLDDRW",
        b"\x00\x03Bracket.SLDPRT\x00",
    )
    .await;
    let p = plan(&app, &eng, &doc).await.json();
    assert_eq!(p["children"], json!([]), "{p}");
    assert_eq!(p["not_in_teamcraft"], json!([]));
    assert_eq!(plan(&app, &eng, &part).await.json()["children"], json!([]));
}

/// A child that would make the BOM circular is refused per line; the other
/// lines are still added.
#[tokio::test]
async fn a_circular_child_is_refused_per_line() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let part = item(&app, &eng, "part", "ROCKET").await;
    upload(&app, &eng, &part, "Plate.SLDPRT", b"\x00plate").await;
    let top = item(&app, &eng, "assembly", "ROCKET").await;
    let sub = item(&app, &eng, "assembly", "ROCKET").await;
    upload(&app, &eng, &sub, "Sub.SLDASM", &sldasm(&["Plate.SLDPRT"])).await;
    // the sub-assembly already contains the top assembly
    checkout(&app, &eng, &sub).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{sub}/revisions/A/bom"),
            &eng,
            Some(json!({ "child": top, "quantity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    upload(
        &app,
        &eng,
        &top,
        "Top.SLDASM",
        &sldasm(&["Sub.SLDASM", "Plate.SLDPRT"]),
    )
    .await;

    let p = plan(&app, &eng, &top).await.json();
    assert_eq!(child(&p, &sub)["status"], "circular");
    assert_eq!(child(&p, &sub)["selected"], false);
    assert_eq!(child(&p, &part)["status"], "new");

    checkout(&app, &eng, &top).await;
    let r = apply(&app, &eng, &top, json!({ "children": [sub, part] })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(body["added"], json!([part]));
    assert_eq!(body["refused"][0]["child"], json!(sub));
    assert!(body["refused"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("circular"));
    let lines = bom(&app, &eng, &top).await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["child_number"], json!(part));
}

/// Reading the plan needs read access to the revision: a restricted group's
/// revision looks missing to outsiders, for the API and the web page.
#[tokio::test]
async fn outsiders_see_no_plan() {
    let app = app_or_skip!();
    let (_, root) = app.user("root", Role::Admin).await;
    let (_, outsider) = app.user_in("olga", Role::Editor, &["ROCKET"]).await;
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ARM'")
        .execute(&app.pool)
        .await
        .unwrap();
    let part = item(&app, &root, "part", "ARM").await;
    upload(&app, &root, &part, "Cog.SLDPRT", b"\x00cog").await;
    let asm = item(&app, &root, "assembly", "ARM").await;
    upload(&app, &root, &asm, "Gear.SLDASM", &sldasm(&["Cog.SLDPRT"])).await;
    assert_eq!(
        plan(&app, &root, &asm).await.json()["children"][0]["item_number"],
        json!(part)
    );
    assert_eq!(
        plan(&app, &outsider, &asm).await.status,
        StatusCode::NOT_FOUND
    );
    checkout(&app, &root, &asm).await;
    let r = apply(&app, &outsider, &asm, json!({ "children": [part] })).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(bom(&app, &root, &asm).await.is_empty());
    // the web page
    let rev = revision_id(&app, &asm).await;
    let cookie = app.login("olga").await;
    let r = app
        .get_page(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("child", &part)],
        )
        .await;
    // a user error: back to the page with a flash, nothing added
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(bom(&app, &root, &asm).await.is_empty());
    // not logged in: sent to the login page
    let r = app
        .get_page(&format!("/revisions/{rev}/bom/from-references"), None)
        .await;
    assert!(r.status.is_redirection() || r.status == StatusCode::UNAUTHORIZED);
}

/// The web preview and apply, including "check out and create".
#[tokio::test]
async fn the_web_preview_and_apply() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let part = item(&app, &eng, "part", "ROCKET").await;
    upload(&app, &eng, &part, "Bracket.SLDPRT", b"\x00bracket").await;
    let asm = item(&app, &eng, "assembly", "ROCKET").await;
    upload(
        &app,
        &eng,
        &asm,
        "Stage.SLDASM",
        &sldasm(&["Bracket.SLDPRT", "Ghost.SLDPRT"]),
    )
    .await;
    let rev = revision_id(&app, &asm).await;
    let cookie = app.login("eng").await;

    // the revision page offers the action
    let page = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Create BOM from CAD references"));

    let r = app
        .get_page(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let html = r.text();
    assert!(html.contains(&part) && html.contains("new line"), "{html}");
    assert!(html.contains("assumed 1") && html.contains("Not in TeamCraft"));
    assert!(html.contains("Check out and create"));

    // nothing ticked is refused with a flash, nothing changes
    async fn post(
        app: &common::TestApp,
        cookie: &str,
        rev: Uuid,
        fields: &[(&str, &str)],
    ) -> common::Response {
        app.post_form(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(cookie),
            Some("http://localhost:8080"),
            fields,
        )
        .await
    }
    let r = post(&app, &cookie, rev, &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(bom(&app, &eng, &asm).await.is_empty());
    // not checked out, no "checkout": refused
    let r = post(&app, &cookie, rev, &[("child", &part)]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(bom(&app, &eng, &asm).await.is_empty());
    // a cross-site POST is refused
    let r = app
        .post_form(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(&cookie),
            Some("http://evil.example"),
            &[("child", &part), ("checkout", "1")],
        )
        .await;
    assert!(r.status.is_client_error(), "{:?}", r.status);
    assert!(bom(&app, &eng, &asm).await.is_empty());
    // check out and create
    let r = post(&app, &cookie, rev, &[("child", &part), ("checkout", "1")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some(format!("/revisions/{rev}#bom").as_str())
    );
    assert_eq!(bom(&app, &eng, &asm).await.len(), 1);
    // the page shows it as already in the BOM
    let html = app
        .get_page(
            &format!("/revisions/{rev}/bom/from-references"),
            Some(&cookie),
        )
        .await
        .text();
    assert!(html.contains("already in the BOM"));
}

/// NX assemblies: the quantity is the number of placed instances.
#[tokio::test]
async fn nx_instance_counts_become_quantities() {
    let app = big_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly/nx");
    let asm_path = dir.join("NIST mtc crada assembly.prt");
    // what the file says, independent of the server
    let expected: std::collections::HashMap<String, u32> =
        match teamcraft::nxasm::kind_of(&asm_path).unwrap() {
            teamcraft::nxasm::Kind::Assembly(c) | teamcraft::nxasm::Kind::Part(c) => {
                let mut m = std::collections::HashMap::new();
                for comp in c {
                    *m.entry(format!("{}.prt", comp.name.to_lowercase()))
                        .or_default() += 1;
                }
                m
            }
            teamcraft::nxasm::Kind::Other => panic!("the fixture is an NX assembly"),
        };
    assert!(
        expected.values().any(|n| *n > 1),
        "the fixture places parts more than once: {expected:?}"
    );
    let mut numbers = std::collections::HashMap::new();
    for name in [
        "90591A141 HEX NUT.prt",
        "91274A118 SHCS.prt",
        "NIST mtc crada box.prt",
    ] {
        let n = item(&app, &eng, "part", "ROCKET").await;
        upload(
            &app,
            &eng,
            &n,
            name,
            &std::fs::read(dir.join(name)).unwrap(),
        )
        .await;
        numbers.insert(name.to_lowercase(), n);
    }
    let asm = item(&app, &eng, "assembly", "ROCKET").await;
    upload(
        &app,
        &eng,
        &asm,
        "NIST mtc crada assembly.prt",
        &std::fs::read(&asm_path).unwrap(),
    )
    .await;
    let p = plan(&app, &eng, &asm).await.json();
    assert_eq!(p["children"].as_array().unwrap().len(), 3, "{p}");
    for (file, number) in &numbers {
        let c = child(&p, number);
        assert_eq!(c["quantity_source"], "cad_instances", "{file}");
        assert_eq!(
            c["quantity"].as_f64().unwrap(),
            f64::from(expected[file]),
            "{file}: {expected:?}"
        );
    }
    // the other parts of the assembly are not in TeamCraft
    assert!(p["not_in_teamcraft"].as_array().unwrap().len() >= 4, "{p}");

    checkout(&app, &eng, &asm).await;
    let all: Vec<&String> = numbers.values().collect();
    let r = apply(&app, &eng, &asm, json!({ "children": all })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let lines = bom(&app, &eng, &asm).await;
    assert_eq!(lines.len(), 3);
    for (file, number) in &numbers {
        let line = lines.iter().find(|l| l["child_number"] == json!(number));
        assert_eq!(
            line.unwrap()["quantity"].as_f64().unwrap(),
            f64::from(expected[file])
        );
    }
}

/// SOLIDWORKS assemblies: the instances of the component tree.
#[tokio::test]
async fn solidworks_instance_counts_become_quantities() {
    let app = big_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly/solidworks");
    let asm_name = "nist_mtc_crada_assembly_rev-D.SLDASM";
    let asm_path = dir.join(asm_name);
    let counts = teamcraft::swpart::file_component_counts(
        std::io::BufReader::new(std::fs::File::open(&asm_path).unwrap()),
        32 * 1024 * 1024,
    )
    .unwrap()
    .expect("the fixture has a component tree");
    assert!(counts.iter().any(|(_, n)| *n > 1), "{counts:?}");
    let (path, expected) = counts.iter().find(|(_, n)| *n > 1).unwrap().clone();
    let part_name = path.rsplit(['\\', '/']).next().unwrap().to_string();
    let part = item(&app, &eng, "part", "ROCKET").await;
    upload(
        &app,
        &eng,
        &part,
        &part_name,
        &std::fs::read(dir.join(&part_name)).unwrap(),
    )
    .await;
    let asm = item(&app, &eng, "assembly", "ROCKET").await;
    upload(
        &app,
        &eng,
        &asm,
        asm_name,
        &std::fs::read(&asm_path).unwrap(),
    )
    .await;
    let p = plan(&app, &eng, &asm).await.json();
    let c = child(&p, &part);
    assert_eq!(c["quantity_source"], "cad_instances", "{p}");
    assert_eq!(c["quantity"].as_f64().unwrap(), f64::from(expected));
}
