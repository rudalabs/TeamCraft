//! Custom attributes and the classification of items (feature 5): admin
//! definitions, values per revision, search, Save as, and the web pages.

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

/// Item numbers of a search.
fn numbers(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn custom_attributes_and_classification() {
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;

    // --- definitions: administrators only ---------------------------------
    let def = |key: &str, data_type: &str, extra: Value| {
        let mut body = json!({ "key": key, "data_type": data_type, "label": key.to_uppercase() });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let r = app
        .api(
            Method::POST,
            "/api/v1/attributes",
            &eng,
            Some(def("vendor", "text", json!({}))),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    for body in [
        def(
            "Vendor",
            "text",
            json!({ "label": "Vendor", "item_types": ["part"] }),
        ),
        def(
            "length_mm",
            "number",
            json!({ "label": "Length", "unit": "mm", "position": 5 }),
        ),
        def("rohs", "boolean", json!({ "label": "RoHS" })),
        def(
            "surface",
            "choice",
            json!({ "label": "Surface", "choices": ["anodized", "painted"] }),
        ),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/attributes", &admin, Some(body))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    for (body, status) in [
        (def("vendor", "text", json!({})), StatusCode::CONFLICT),
        (def("finish", "choice", json!({})), StatusCode::BAD_REQUEST),
        (def("finish", "colour", json!({})), StatusCode::BAD_REQUEST),
        (def("9lives", "text", json!({})), StatusCode::BAD_REQUEST),
        (
            def("finish", "text", json!({ "item_types": ["widget"] })),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/attributes", &admin, Some(body))
            .await;
        assert_eq!(r.status, status, "{}", r.text());
    }
    let r = app.api(Method::GET, "/api/v1/attributes", &eng, None).await;
    let defs = r.json();
    let keys: Vec<&str> = defs
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["rohs", "surface", "vendor", "length_mm"],
        "order: position (0, 0, 0, 5), then label"
    );
    assert_eq!(defs[2]["label"], "Vendor");
    assert_eq!(defs[2]["item_types"], json!(["part"]));

    // --- values when creating and editing ----------------------------------
    let create = |item_type: &str, name: &str, extra: Value| {
        let mut body = json!({ "item_type": item_type, "name": name, "project": "ROVER" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create("part", "Linear guide", json!({
                "classification": "purchased",
                "attributes": { "vendor": "Bosch Rexroth", "length_mm": "120,5", "rohs": true, "surface": "Anodized" },
            }))),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let guide = r.json()["item_number"].as_str().unwrap().to_string();
    assert_eq!(r.json()["classification"], "purchased");
    assert_eq!(
        r.json()["attributes"],
        json!({ "vendor": "Bosch Rexroth", "length_mm": 120.5, "rohs": true, "surface": "anodized" }),
        "numbers parsed, choices keep their spelling"
    );
    for (attrs, why) in [
        (json!({ "vendor": "X" }), "vendor is for parts only"),
        (json!({ "length_mm": "long" }), "not a number"),
        (json!({ "surface": "chrome" }), "not a choice"),
        (json!({ "colour": "red" }), "unknown attribute"),
        (json!({ "rohs": [true] }), "not a scalar"),
    ] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(create("assembly", "Frame", json!({ "attributes": attrs }))),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{why}: {}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create(
                "assembly",
                "Frame",
                json!({ "attributes": { "length_mm": 800 } }),
            )),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let frame = r.json()["item_number"].as_str().unwrap().to_string();
    assert_eq!(r.json()["classification"], "make", "the default");

    let rev = format!("/api/v1/items/{guide}/revisions/A");
    let r = app
        .api(
            Method::PATCH,
            &rev,
            &eng,
            Some(json!({ "attributes": { "vendor": null, "length_mm": 250 } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["attributes"],
        json!({ "length_mm": 250.0, "rohs": true, "surface": "anodized" }),
        "null removes, unlisted keys stay"
    );
    let r = app
        .api(
            Method::PATCH,
            &rev,
            &eng,
            Some(json!({ "attributes": { "length_mm": "x" } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::PATCH,
            &rev,
            &eng,
            Some(json!({ "attributes": { "vendor": "SKF" } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // --- search -----------------------------------------------------------------
    let search = |query: &str| format!("/api/v1/items?project=ROVER&{query}");
    for (query, expected) in [
        ("attr=length_mm&value=100..300", vec![guide.as_str()]),
        ("attr=length_mm&value=%3E%3D500", vec![frame.as_str()]),
        ("attr=length_mm&value=250", vec![guide.as_str()]),
        ("attr=length_mm", vec![frame.as_str(), guide.as_str()]),
        ("attr=rohs&value=yes", vec![guide.as_str()]),
        ("attr=rohs&value=no", vec![]),
        ("attr=surface&value=ANOD", vec![guide.as_str()]),
        ("attr=vendor&value=sk", vec![guide.as_str()]),
        ("class=purchased", vec![guide.as_str()]),
        ("class=make", vec![frame.as_str()]),
        ("q=SKF", vec![guide.as_str()]),
        ("q=anodized", vec![guide.as_str()]),
    ] {
        let r = app.api(Method::GET, &search(query), &eng, None).await;
        assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.text());
        let mut found = numbers(&r.json());
        found.sort();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(found, expected, "{query}");
    }
    for query in [
        "attr=colour&value=x",
        "attr=length_mm&value=10-20",
        "attr=rohs&value=maybe",
        "class=phantom",
    ] {
        let r = app.api(Method::GET, &search(query), &eng, None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}: {}", r.text());
    }

    // --- classification as master data, Save as copies both --------------------
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{frame}"),
            &eng,
            Some(json!({ "classification": "standard" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["classification"], "standard");
    assert_eq!(
        r.json()["project_code"],
        "ROVER",
        "other master data unchanged"
    );
    let r = app
        .api(
            Method::POST,
            &format!("{rev}/save-as"),
            &eng,
            Some(json!({ "name": "Linear guide, long" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["classification"], "purchased");
    assert_eq!(
        r.json()["attributes"],
        json!({ "vendor": "SKF", "length_mm": 250.0, "rohs": true, "surface": "anodized" })
    );

    // --- deactivating keeps values; unchanged values pass -----------------------
    let r = app
        .api(
            Method::PATCH,
            "/api/v1/attributes/rohs",
            &admin,
            Some(json!({ "active": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["active"], false);
    assert_eq!(r.json()["label"], "RoHS", "unlisted settings stay");
    let r = app.api(Method::GET, "/api/v1/attributes", &eng, None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 3);
    let r = app
        .api(Method::GET, "/api/v1/attributes?all=true", &eng, None)
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 4);
    let r = app
        .api(
            Method::PATCH,
            &rev,
            &eng,
            Some(json!({ "attributes": { "rohs": false } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "inactive: {}", r.text());
    let r = app
        .api(
            Method::PATCH,
            &rev,
            &eng,
            Some(json!({ "attributes": { "rohs": "yes", "vendor": "SKF" } })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "unchanged values pass: {}",
        r.text()
    );
    assert_eq!(r.json()["attributes"]["rohs"], true);
    let r = app
        .api(
            Method::PATCH,
            "/api/v1/attributes/surface",
            &admin,
            Some(json!({ "choices": ["painted"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // --- web pages ------------------------------------------------------------------
    app.user("rolf", Role::Admin).await;
    let cookie = app.login("rolf").await;
    let origin = Some("http://localhost:8080");
    let html = app
        .get_page("/admin/attributes", Some(&cookie))
        .await
        .text();
    assert!(
        html.contains(">Length</a>") && html.contains("inactive"),
        "{html}"
    );
    assert!(
        html.contains(">Position</th>")
            && html.contains("Position in forms")
            && !html.contains("<label>Order"),
        "position label: {html}"
    );
    // The edit page uses the same wording.
    let end = html.find(">Length</a>").unwrap();
    let start = html[..end].rfind("/admin/attributes/").unwrap();
    let edit_url = &html[start..end];
    let edit_url = &edit_url[..edit_url.find('"').unwrap()];
    let edit = app.get_page(edit_url, Some(&cookie)).await.text();
    assert!(
        edit.contains("Position in forms")
            && edit.contains("lower numbers appear first")
            && !edit.contains("<label>Order"),
        "edit page: {edit}"
    );
    let rev_id = app.api(Method::GET, &rev, &eng, None).await.json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let html = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await
        .text();
    assert!(html.contains("<dd>250 mm</dd>"), "values with unit");
    assert!(
        html.contains("no longer in use"),
        "the inactive RoHS value stays visible"
    );
    assert!(html.contains("<dd>Purchased</dd>"), "classification");
    let r = app
        .api(Method::POST, &format!("{rev}/checkout"), &eng, None)
        .await;
    // eng created the item, so it is still checked out to eng
    assert!(
        r.status.is_success() || r.status == StatusCode::CONFLICT,
        "{}",
        r.text()
    );
    let eng_cookie = app.login("eng").await;
    let html = app
        .get_page(&format!("/revisions/{rev_id}/edit"), Some(&eng_cookie))
        .await
        .text();
    assert!(
        html.contains("name=\"attr.length_mm\" value=\"250\""),
        "{html}"
    );
    assert!(
        html.contains("anodized (no longer a choice)"),
        "removed choice still selectable"
    );
    assert!(
        !html.contains("attr.rohs"),
        "inactive attributes are not offered"
    );
    let r = app
        .post_form(
            &format!("/revisions/{rev_id}/edit"),
            Some(&eng_cookie),
            origin,
            &[
                ("name", "Linear guide"),
                ("attr.length_mm", "300"),
                ("attr.surface", "anodized"),
                ("attr.vendor", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = app.api(Method::GET, &rev, &eng, None).await;
    assert_eq!(
        r.json()["revision"]["attributes"],
        json!({ "length_mm": 300.0, "rohs": true, "surface": "anodized" }),
        "form saves; the old choice stays; empty removes"
    );
    let r = app
        .post_form(
            &format!("/revisions/{rev_id}/edit"),
            Some(&eng_cookie),
            origin,
            &[("name", "Linear guide"), ("attr.length_mm", "abc")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.text().contains("value=\"abc\""),
        "the form keeps the input"
    );
    let html = app
        .get_page("/items?attr=length_mm&value=abc", Some(&cookie))
        .await
        .text();
    assert!(
        html.contains("is not a number"),
        "filter errors on the page, not an error page"
    );
    let html = app
        .get_page(
            "/items?class=purchased&attr=length_mm&value=200..400",
            Some(&cookie),
        )
        .await
        .text();
    assert!(html.contains(&guide) && !html.contains(&frame));

    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action IN ('attribute.create', 'attribute.update')",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audits, 6, "4 created, 2 updated");
    app.teardown().await;
}

#[tokio::test]
async fn bom_import_fills_classification_and_attributes() {
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    for body in [
        json!({ "key": "vendor", "data_type": "text", "label": "Vendor", "item_types": ["part"] }),
        json!({ "key": "length_mm", "data_type": "number", "label": "Length", "unit": "mm" }),
        json!({ "key": "surface", "data_type": "choice", "label": "Surface", "choices": ["anodized", "painted"] }),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/attributes", &admin, Some(body))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    // Inventor-like structured export with extra columns: "Vendor" = label,
    // "length_mm" = key, "Oberfläche" and "Cost Center" match nothing.
    let csv =
        "Item,Part Number,BOM Structure,QTY,Description,Vendor,length_mm,Surface,Cost Center\n\
               1,FRAME-1,Normal,1,Frame,Item GmbH,800,painted,K100\n\
               1.1,GUIDE-1,Kaufteil,2,Linear guide,Bosch Rexroth,\"120,5\",anodized,K100\n\
               1.2,ISO 4762 M6 x 20,Content Center,8,Screw,Würth,20,chrome,K100\n\
               2,MOTOR-1,Purchased,1,Motor,Maxon,abc,,K200\n\
               3,PLATE-1,Phantom,1,Plate,,,,\n\
               4,ODD-1,Superpart,1,Odd one,,,,\n";
    let files: &[(&str, &[u8])] = &[("rover.csv", csv.as_bytes())];

    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROVER"), ("dry_run", "true")],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    let plan = plan.json();
    let headers: Vec<&str> = plan["attribute_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["header"].as_str().unwrap())
        .collect();
    assert_eq!(headers, ["Vendor", "length_mm", "Surface"], "{plan}");
    assert_eq!(plan["unused_columns"], json!(["Cost Center"]));
    let warnings = plan["warnings"].to_string();
    assert!(
        warnings.contains("Vendor does not apply to items of type assembly"),
        "{warnings}"
    );
    assert!(warnings.contains("'chrome' is not one of"), "{warnings}");
    assert!(warnings.contains("'abc' is not a number"), "{warnings}");
    assert!(
        warnings.contains("BOM structure 'Superpart' is not understood"),
        "{warnings}"
    );

    let done = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROVER"), ("name", "Rover")],
        )
        .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
    let created = done.json()["created"].clone();
    let number_of = |file_number: &str| {
        created
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["file_number"] == file_number)
            .unwrap_or_else(|| panic!("{file_number} in {created}"))["number"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let rev = |number: String| {
        let app = &app;
        let eng = &eng;
        async move {
            app.api(
                Method::GET,
                &format!("/api/v1/items/{number}/revisions/latest"),
                eng,
                None,
            )
            .await
            .json()["revision"]
                .clone()
        }
    };
    let frame = rev(number_of("FRAME-1")).await;
    assert_eq!(frame["item_type"], "assembly");
    assert_eq!(frame["classification"], "make");
    assert_eq!(
        frame["attributes"],
        json!({ "length_mm": 800.0, "surface": "painted" }),
        "vendor is for parts only"
    );
    let guide = rev(number_of("GUIDE-1")).await;
    assert_eq!(guide["classification"], "purchased", "Kaufteil");
    assert_eq!(
        guide["attributes"],
        json!({ "vendor": "Bosch Rexroth", "length_mm": 120.5, "surface": "anodized" })
    );
    let screw = rev(number_of("ISO 4762 M6 x 20")).await;
    assert_eq!(screw["classification"], "standard", "Content Center");
    assert_eq!(
        screw["attributes"],
        json!({ "vendor": "Würth", "length_mm": 20.0 }),
        "the invalid choice is left out"
    );
    let motor = rev(number_of("MOTOR-1")).await;
    assert_eq!(motor["classification"], "purchased");
    assert_eq!(motor["attributes"], json!({ "vendor": "Maxon" }));
    assert_eq!(
        rev(number_of("PLATE-1")).await["classification"],
        "make",
        "Phantom"
    );
    assert_eq!(rev(number_of("ODD-1")).await["classification"], "make");

    // the search finds the imported values
    let r = app
        .api(
            Method::GET,
            "/api/v1/items?class=purchased&attr=vendor&value=bosch",
            &eng,
            None,
        )
        .await;
    let hits = r.json();
    let found: Vec<&str> = hits["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(found, ["Linear guide"]);

    // An existing item is not changed by a second import.
    let csv2 = "Part Number,QTY,BOM Structure,Vendor\nGUIDE-1,1,Normal,Other Ltd\n";
    let guide_number = number_of("GUIDE-1");
    let csv2 = csv2.replace("GUIDE-1", &guide_number);
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("again.csv", csv2.as_bytes())],
            &[("project", "ROVER")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let guide = rev(guide_number).await;
    assert_eq!(guide["classification"], "purchased");
    assert_eq!(guide["attributes"]["vendor"], "Bosch Rexroth");
    app.teardown().await;
}
