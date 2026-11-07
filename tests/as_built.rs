//! As-built records: a unit built to a design revision, its as-built BOM,
//! deviations with a disposition, the release rule (no open deviation),
//! Revise, the design side and read permissions.

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

async fn checkin(app: &common::TestApp, token: &str, item: &str, code: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{item}/revisions/{code}/checkin"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "check in {item}: {}", r.text());
}

/// A released assembly with one bolt (4×) in its BOM; returns (assembly, bolt).
async fn released_design(app: &common::TestApp, token: &str, reviewer: &str) -> (String, String) {
    let asm = app
        .create_item(
            token,
            json!({ "item_type": "assembly", "name": "Gearbox", "project": "ROVER" }),
        )
        .await;
    let bolt = app
        .create_item(
            token,
            json!({ "item_type": "part", "name": "Bolt M8", "project": "ROVER" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            token,
            Some(json!({ "child": bolt, "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    checkin(app, token, &asm, "A").await;
    checkin(app, token, &bolt, "A").await;
    app.release(token, ("rolf", reviewer), &[&bolt, &asm]).await;
    (asm, bolt)
}

#[tokio::test]
async fn a_unit_records_its_design_bom_and_deviations_and_releases_only_when_decided() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let (asm, bolt) = released_design(&app, &eng, &rolf).await;

    // --- record a unit: as-built item with the design's BOM ------------------
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "SN-0001", "built_on": "2026-11-02" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let unit = r.json()["item_number"].as_str().unwrap().to_string();
    assert!(unit.starts_with("AB-"), "{unit}");
    assert_eq!(r.json()["item_type"], "as_built");
    assert_eq!(r.json()["name"], "Gearbox – SN SN-0001");
    let ab = format!("/api/v1/items/{unit}/revisions/A");
    let r = app
        .api(Method::GET, &format!("{ab}/as-built"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let state = r.json();
    assert_eq!(state["record"]["design_number"], asm.as_str());
    assert_eq!(state["record"]["design_code"], "A");
    assert_eq!(state["record"]["serial_number"], "SN-0001");
    assert_eq!(state["record"]["built_on"], "2026-11-02");
    assert_eq!(state["record"]["newer_design"], serde_json::Value::Null);
    let r = app.api(Method::GET, &format!("{ab}/bom"), &eng, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.text().contains(&bolt),
        "the design's BOM was copied: {}",
        r.text()
    );

    // the serial number is unique per design item
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "sn-0001" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains(&unit), "{}", r.text());

    // a unit is built to a part, assembly or software – not to itself
    let r = app
        .api(
            Method::PUT,
            &format!("{ab}/as-built/record"),
            &eng,
            Some(json!({ "design": unit, "serial_number": "SN-0001" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // --- deviations ----------------------------------------------------------
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(json!({ "kind": "dimension", "feature": "Bore Ø12 H7",
                         "designed": "12.000 +0.018/0", "built": "12.035" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["disposition"], "open");
    let bore = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(json!({ "kind": "colour", "feature": "x" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // an open deviation blocks the submit
    checkin(&app, &eng, &unit, "A").await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &eng,
            Some(json!({ "title": "Unit 1 accepted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (path, body) in [
        ("items", json!({ "item": unit })),
        ("reviewers", json!({ "username": "rolf" })),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/{path}"),
                &eng,
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("1 open deviation"), "{}", r.text());

    // deciding it (revision checked out again) lets the release through
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/as-built-deviations/{bore}"),
            &eng,
            Some(json!({ "disposition": "use_as_is" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "not checked out: {}",
        r.text()
    );
    let r = app
        .api(Method::POST, &format!("{ab}/checkout"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/as-built-deviations/{bore}"),
            &eng,
            Some(json!({ "disposition": "use-as-is", "note": "MRB 12: fit still OK" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["disposition"], "use_as_is");
    assert_eq!(r.json()["feature"], "Bore Ø12 H7");
    checkin(&app, &eng, &unit, "A").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &rolf, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());

    // released: frozen
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(json!({ "kind": "process", "feature": "Torque" })),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.text());

    // --- Revise (rework) copies the record and the deviations ----------------
    let r = app
        .api(Method::POST, &format!("{ab}/revise"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{unit}/revisions/B/as-built"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["record"]["serial_number"], "SN-0001");
    assert_eq!(r.json()["summary"]["total"], 1);
    assert_eq!(r.json()["summary"]["use_as_is"], 1);
    // the same unit keeps its serial number in the new revision
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{unit}/revisions/B/as-built/record"),
            &eng,
            Some(
                json!({ "design": asm, "design_revision": "A", "serial_number": "SN-0001",
                         "note": "flange reworked" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // --- the design side -----------------------------------------------------
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/built-units"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let units = r.json();
    assert_eq!(
        units.as_array().unwrap().len(),
        1,
        "latest revision only: {units}"
    );
    assert_eq!(units[0]["unit_number"], unit.as_str());
    assert_eq!(units[0]["unit_code"], "B");
    assert_eq!(units[0]["serial_number"], "SN-0001");
    assert_eq!(units[0]["open_count"], 0);
    assert_eq!(r.header("x-hidden-count").as_deref(), Some("0"));
    let page = app
        .get_page(
            &format!(
                "/revisions/{}",
                sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
                     WHERE i.number = $1 AND r.code = 'A'",
                )
                .bind(&asm)
                .fetch_one(&app.pool)
                .await
                .unwrap()
            ),
            Some(&app.login("erika").await),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Built units"), "design page");
    assert!(
        page.text().contains("SN-0001"),
        "design page lists the unit"
    );

    // a design that units were built to cannot be deleted (a fresh one with a unit)
    let proto = app
        .create_item(
            &eng,
            json!({ "item_type": "part", "name": "Prototype", "project": "ROVER" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{proto}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "P1", "with_bom": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let proto_id = sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM items WHERE number = $1")
        .bind(&proto)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let refused =
        teamcraft::services::items::delete(&app.pool, &app.actor("erika").await, proto_id)
            .await
            .expect_err("units were built to it");
    assert!(refused.to_string().contains("built"), "{refused}");

    let actions = app
        .audit_actions(
            sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
                 WHERE i.number = $1 AND r.code = 'A'",
            )
            .bind(&unit)
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        )
        .await;
    for action in [
        "as_built.record.set",
        "as_built.deviation.add",
        "as_built.deviation.update",
    ] {
        assert!(
            actions.iter().any(|a| a == action),
            "{action} in {actions:?}"
        );
    }
    app.teardown().await;
}

#[tokio::test]
async fn a_newer_design_release_marks_the_unit_and_hidden_designs_are_placeholders() {
    let app = app_or_skip!();
    let (_, eng) = app
        .user_in("erika", Role::Editor, &["ROVER", "ROCKET"])
        .await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let (asm, _) = released_design(&app, &eng, &rolf).await;

    // a unit in another group, built to A
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "SN-7", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let unit = r.json()["item_number"].as_str().unwrap().to_string();

    // the design moves on: B is released
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/revise"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    checkin(&app, &eng, &asm, "B").await;
    app.release(&eng, ("rolf", &rolf), &[&asm]).await;
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{unit}/revisions/A/as-built"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["record"]["newer_design"],
        format!("{asm}/B is released (built to: A)")
    );

    // the design's group becomes restricted: the outsider sees a placeholder
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ROVER'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{unit}/revisions/A/as-built"),
            &outsider,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let record = &r.json()["record"];
    assert_eq!(record["restricted"], true, "{record}");
    assert_eq!(record["design_number"], "restricted item");
    assert_eq!(record["newer_design"], serde_json::Value::Null);
    assert!(!r.text().contains(&asm), "{}", r.text());
    // and the units list of the hidden design is not found
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/built-units"),
            &outsider,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    app.teardown().await;
}

/// Id of revision `code` of item `number`.
async fn revision_id(app: &common::TestApp, number: &str, code: &str) -> uuid::Uuid {
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = $2",
    )
    .bind(number)
    .bind(code)
    .fetch_one(&app.pool)
    .await
    .expect("revision exists")
}

#[tokio::test]
async fn deviation_writes_need_the_checkout_and_hidden_deviations_are_not_found() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, colleague) = app.user_in("emil", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let design = app
        .create_item(
            &eng,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    // a unit of the in-work design (prototype); checked out by erika
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{design}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "B-1", "with_bom": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let unit = r.json()["item_number"].as_str().unwrap().to_string();
    let ab = format!("/api/v1/items/{unit}/revisions/A");
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(
                json!({ "kind": "material", "feature": "Sheet", "designed": "1.4301",
                         "built": "1.4404" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let deviation = format!("/api/v1/as-built-deviations/{id}");
    let add = json!({ "kind": "process", "feature": "Weld seam" });
    let patch = json!({ "disposition": "scrap" });

    // a viewer of the group and an editor of another group may read, not write
    for (who, token) in [("viewer", &viewer), ("non-member", &outsider)] {
        let r = app
            .api(Method::GET, &format!("{ab}/as-built"), token, None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{who} reads: {}", r.text());
        let r = app
            .api(
                Method::POST,
                &format!("{ab}/as-built/deviations"),
                token,
                Some(add.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} adds: {}", r.text());
        let r = app
            .api(Method::PATCH, &deviation, token, Some(patch.clone()))
            .await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{who} patches: {}",
            r.text()
        );
        let r = app.api(Method::DELETE, &deviation, token, None).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{who} deletes: {}",
            r.text()
        );
    }

    // a member while erika has the revision checked out
    let r = app
        .api(
            Method::PUT,
            &format!("{ab}/as-built/record"),
            &colleague,
            Some(json!({ "design": design, "serial_number": "B-2" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("checked out"), "{}", r.text());
    let r = app
        .api(Method::PATCH, &deviation, &colleague, Some(patch.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // nothing changed
    let r = app
        .api(Method::GET, &format!("{ab}/as-built"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["record"]["serial_number"], "B-1");
    assert_eq!(r.json()["deviations"][0]["disposition"], "open");
    assert_eq!(r.json()["summary"]["total"], 1);

    // the group becomes restricted: the outsider gets "not found", not 403
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ROVER'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(Method::PATCH, &deviation, &outsider, Some(patch.clone()))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = app.api(Method::DELETE, &deviation, &outsider, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert!(!r.text().contains(&unit), "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{ab}/as-built"), &outsider, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());

    // the owner still can
    let r = app
        .api(Method::PATCH, &deviation, &eng, Some(patch.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["disposition"], "scrap");
    assert_eq!(r.json()["built"], "1.4404", "unchanged fields are kept");
    app.teardown().await;
}

#[tokio::test]
async fn submit_needs_a_build_record_and_deviations_are_limited() {
    let app = app_or_skip!();
    let (erika, eng) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.user("rolf", Role::Editor).await;
    // an as-built item created directly: no build record yet
    let unit = app
        .create_item(
            &eng,
            json!({ "item_type": "as_built", "name": "Loose unit", "project": "ROVER" }),
        )
        .await;
    assert!(unit.starts_with("AB-"), "{unit}");
    let ab = format!("/api/v1/items/{unit}/revisions/A");

    // the deviation limit: fill up to one below it, then one more fits
    let limit = teamcraft::services::as_built::MAX_DEVIATIONS;
    sqlx::query(
        "INSERT INTO as_built_deviations (id, revision_id, position, kind, feature,
                                          disposition, created_by)
         SELECT gen_random_uuid(), $1, g, 'other', 'Feature ' || g, 'use_as_is', $2
         FROM generate_series(1, $3) g",
    )
    .bind(revision_id(&app, &unit, "A").await)
    .bind(erika.id)
    .bind(i32::try_from(limit - 1).unwrap())
    .execute(&app.pool)
    .await
    .unwrap();
    let one = json!({ "kind": "other", "feature": "Last one", "disposition": "use_as_is" });
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(one.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["position"], i64::try_from(limit).unwrap());
    let r = app
        .api(
            Method::POST,
            &format!("{ab}/as-built/deviations"),
            &eng,
            Some(one),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.text().contains(&format!("at most {limit} deviations")),
        "{}",
        r.text()
    );

    // every deviation is decided, but the build record is missing
    checkin(&app, &eng, &unit, "A").await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &eng,
            Some(json!({ "title": "Unit without record" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (path, body) in [
        ("items", json!({ "item": unit })),
        ("reviewers", json!({ "username": "rolf" })),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/{path}"),
                &eng,
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("no build record"), "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn obsolete_design_revisions_take_no_new_units() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let (asm, _) = released_design(&app, &eng, &rolf).await;
    // a unit recorded while A was current
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "OLD-1", "with_bom": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let unit = r.json()["item_number"].as_str().unwrap().to_string();

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/obsolete"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // no new unit against the obsolete revision
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/built-units"),
            &eng,
            Some(json!({ "serial_number": "NEW-1", "with_bom": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("obsolete"), "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/built-units"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json().as_array().map(Vec::len), Some(1), "{}", r.text());

    // the existing unit keeps its record and may still correct it
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{unit}/revisions/A/as-built/record"),
            &eng,
            Some(
                json!({ "design": asm, "design_revision": "A", "serial_number": "OLD-1",
                         "note": "built before A was withdrawn" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["note"], "built before A was withdrawn");
    app.teardown().await;
}

#[tokio::test]
async fn concurrent_units_with_one_serial_number_record_exactly_one() {
    let app = app_or_skip!();
    app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, eng) = app.user_in("emil", Role::Editor, &["ROVER"]).await;
    let design = app
        .create_item(
            &eng,
            json!({ "item_type": "part", "name": "Housing", "project": "ROVER" }),
        )
        .await;
    let design_rev = revision_id(&app, &design, "A").await;
    let erika = app.actor("erika").await;
    let emil = app.actor("emil").await;
    for round in 0..3 {
        let serial = format!("H-{round}");
        let unit = |serial: &str| teamcraft::services::as_built::NewUnit {
            serial_number: serial.to_string(),
            ..Default::default()
        };
        let (a, b) = tokio::join!(
            teamcraft::services::as_built::record_unit(
                &app.pool,
                &erika,
                design_rev,
                unit(&serial)
            ),
            teamcraft::services::as_built::record_unit(&app.pool, &emil, design_rev, unit(&serial)),
        );
        let ok = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
        assert_eq!(ok, 1, "round {round}: {a:?} / {b:?}");
        let refused = a.err().or(b.err()).unwrap();
        assert!(
            refused.to_string().contains("already recorded"),
            "round {round}: {refused}"
        );
        let stored = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM as_built_records rec
             JOIN items d ON d.id = rec.design_item_id
             WHERE d.number = $1 AND rec.serial_number = $2",
        )
        .bind(&design)
        .bind(&serial)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(stored, 1, "round {round}");
        // the refused transaction left no as-built item behind
        let units = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT i.id) FROM items i JOIN revisions r ON r.item_id = i.id
             WHERE i.item_type = 'as_built' AND r.name LIKE $1",
        )
        .bind(format!("% – SN {serial}"))
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(units, 1, "round {round}");
    }
    app.teardown().await;
}
