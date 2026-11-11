//! Mass properties end to end: the material library and who may change it,
//! the roll-up of a small vehicle built from STL boxes generated here (a
//! cube and an offset bar in the common frame), a measured override set
//! through the web form, a missing density, a restricted item, the REST
//! endpoint, the MCP tool, the page with its exports, and the settings
//! carried over by *revise*.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    domain::{RevisionRule, Role},
    massprops::{self, Body},
    services::mass_properties::{self as mass, MaterialInput, Source},
};
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Binary STL of the axis-aligned box from `lo` with edges `e` (mm),
/// normals outward.
fn box_stl(lo: [f32; 3], e: [f32; 3]) -> Vec<u8> {
    let corner = |i: usize| -> [f32; 3] {
        [
            lo[0] + if i & 1 == 0 { 0.0 } else { e[0] },
            lo[1] + if i & 2 == 0 { 0.0 } else { e[1] },
            lo[2] + if i & 4 == 0 { 0.0 } else { e[2] },
        ]
    };
    let quads = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let mut out = vec![0u8; 80];
    out.extend_from_slice(&12u32.to_le_bytes());
    for q in quads {
        for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            out.extend_from_slice(&[0u8; 12]);
            for v in tri {
                for x in corner(v) {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            }
            out.extend_from_slice(&[0, 0]);
        }
    }
    out
}

/// A box body in SI from mm corner and edges at `density`.
fn box_body(lo: [f64; 3], e: [f64; 3], density: f64) -> Body {
    let s = 1e-3;
    let (a, b, c) = (e[0] * s, e[1] * s, e[2] * s);
    let m = density * a * b * c;
    Body {
        mass: m,
        cg: [
            (lo[0] + e[0] / 2.0) * s,
            (lo[1] + e[1] / 2.0) * s,
            (lo[2] + e[2] / 2.0) * s,
        ],
        inertia: [
            m * (b * b + c * c) / 12.0,
            m * (a * a + c * c) / 12.0,
            m * (a * a + b * b) / 12.0,
            0.0,
            0.0,
            0.0,
        ],
    }
}

async fn upload(app: &common::TestApp, token: &str, number: &str, filename: &str, data: &[u8]) {
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            token,
            filename,
            data,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn add_line(app: &common::TestApp, token: &str, parent: &str, child: &str, qty: f64) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            token,
            Some(json!({ "child": child, "quantity": qty })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn revision_id(app: &common::TestApp, number: &str, code: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = $2",
    )
    .bind(number)
    .bind(code)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

#[tokio::test]
async fn only_admins_change_the_material_library() {
    let app = app_or_skip!();
    let (admin, _) = app.user("ada", Role::Admin).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("max").await;

    let input = |name: &str, density: f64| MaterialInput {
        name: name.into(),
        density_kg_m3: density,
        notes: String::new(),
    };
    // an editor may not, neither through the service nor the form
    let e = mass::create_material(&app.pool, &max, input("Steel", 7850.0))
        .await
        .unwrap_err();
    assert_eq!(e.status(), StatusCode::FORBIDDEN);
    let r = app
        .post_form(
            "/materials",
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("name", "Steel"), ("density", "7850")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(mass::materials(&app.pool).await.unwrap().is_empty());

    // the admin adds, renames and removes; names are unique ignoring case
    let steel = mass::create_material(&app.pool, &admin, input("Steel", 7850.0))
        .await
        .unwrap();
    let e = mass::create_material(&app.pool, &admin, input("STEEL", 7800.0))
        .await
        .unwrap_err();
    assert_eq!(e.status(), StatusCode::CONFLICT);
    for bad in [0.0, -1.0, f64::NAN, 1e6] {
        assert!(mass::create_material(&app.pool, &admin, input("X", bad))
            .await
            .is_err());
    }
    let e = mass::update_material(&app.pool, &max, steel.id, input("Steel", 1.0))
        .await
        .unwrap_err();
    assert_eq!(e.status(), StatusCode::FORBIDDEN);
    let s = mass::update_material(&app.pool, &admin, steel.id, input("Steel S355", 7850.0))
        .await
        .unwrap();
    assert_eq!(s.name, "Steel S355");
    let e = mass::delete_material(&app.pool, &max, steel.id)
        .await
        .unwrap_err();
    assert_eq!(e.status(), StatusCode::FORBIDDEN);
    mass::delete_material(&app.pool, &admin, steel.id)
        .await
        .unwrap();
    let actions = app.audit_actions(steel.id).await;
    assert_eq!(
        actions,
        ["material.create", "material.update", "material.delete"]
    );
    // everybody reads the library
    let r = app.get_page("/materials", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text().contains("Add material"));
}

#[tokio::test]
async fn a_vehicle_rolls_up_from_its_meshes_and_measured_parts() {
    let app = app_or_skip!();
    let (admin, admin_token) = app.user("ada", Role::Admin).await;
    let (max_actor, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let cookie = app.login("max").await;
    for (name, density) in [("Aluminium 6061", 2700.0), ("Steel", 7850.0)] {
        mass::create_material(
            &app.pool,
            &admin,
            MaterialInput {
                name: name.into(),
                density_kg_m3: density,
                notes: String::new(),
            },
        )
        .await
        .unwrap();
    }

    let item = |kind: &str, name: &str, material: &str, project: &str| json!({ "item_type": kind, "name": name, "material": material, "project": project });
    let vehicle = app
        .create_item(&max, item("assembly", "Vehicle", "", "ROCKET"))
        .await;
    // the material is matched ignoring case
    let cube = app
        .create_item(&max, item("part", "Bulkhead", "aluminium 6061", "ROCKET"))
        .await;
    let bar = app
        .create_item(&max, item("part", "Rail", "Steel", "ROCKET"))
        .await;
    let motor = app
        .create_item(&max, item("part", "Motor (bought)", "", "ROCKET"))
        .await;
    let odd = app
        .create_item(&max, item("part", "Mystery part", "Unobtainium", "ROCKET"))
        .await;
    let secret = app
        .create_item(
            &admin_token,
            json!({ "item_type": "part", "name": "Payload", "project": "ROVER", "mass_kg": 5.0 }),
        )
        .await;
    upload(
        &app,
        &max,
        &cube,
        "bulkhead.stl",
        &box_stl([0.0; 3], [100.0; 3]),
    )
    .await;
    upload(
        &app,
        &max,
        &bar,
        "rail.stl",
        &box_stl([300.0, 0.0, 0.0], [200.0, 50.0, 20.0]),
    )
    .await;
    upload(
        &app,
        &max,
        &odd,
        "mystery.stl",
        &box_stl([0.0; 3], [10.0; 3]),
    )
    .await;
    for child in [&cube, &bar, &motor, &odd, &secret] {
        add_line(&app, &max, &vehicle, child, 1.0).await;
    }

    // the motor is weighed: mass and CG through the settings form
    let motor_rev = revision_id(&app, &motor, "A").await;
    let r = app
        .post_form(
            &format!("/revisions/{motor_rev}/mass"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[
                ("geometry_file", "auto"),
                ("measured_mass", "1.0"),
                ("cg_x", "1.0"),
                ("cg_y", "0"),
                ("cg_z", "0"),
                ("note", "scale 2026-11-01"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let stored = mass::revision_settings(&app.pool, &max_actor, motor_rev)
        .await
        .unwrap();
    assert_eq!(stored.settings.measured_mass_kg, Some(1.0));
    assert_eq!(stored.settings.measured_note, "scale 2026-11-01");
    assert!(app
        .audit_actions(motor_rev)
        .await
        .contains(&"revision.mass".to_string()));
    // somebody without the check-out may not
    let rolf_actor = app.actor("rolf").await;
    let e = mass::update_settings(
        &app.pool,
        &rolf_actor,
        motor_rev,
        mass::MassSettingsUpdate::default(),
    )
    .await
    .unwrap_err();
    assert!(e.status().is_client_error(), "{e:?}");

    // the payload's group becomes restricted: max no longer sees it
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ROVER'")
        .execute(&app.pool)
        .await
        .unwrap();
    let max_actor = app.actor("max").await;

    let vehicle_rev = revision_id(&app, &vehicle, "A").await;
    let reference = [2.0, 0.0, 0.0];
    let report = mass::rollup(
        &app.state,
        &max_actor,
        vehicle_rev,
        RevisionRule::Latest,
        reference,
    )
    .await
    .unwrap();
    let cube_body = box_body([0.0; 3], [100.0; 3], 2700.0);
    let bar_body = box_body([300.0, 0.0, 0.0], [200.0, 50.0, 20.0], 7850.0);
    let motor_body = Body::point(1.0, [1.0, 0.0, 0.0]);
    let want = massprops::combine([&cube_body, &bar_body, &motor_body]);
    assert!(
        close(report.mass_kg, 2.7 + 1.57 + 1.0),
        "{}",
        report.mass_kg
    );
    let cg = report.cg_m.unwrap();
    for k in 0..3 {
        assert!(close(cg[k], want.cg[k]), "{cg:?} vs {:?}", want.cg);
    }
    let inertia = report.inertia_cg.unwrap();
    let about = report.inertia_ref.unwrap();
    let want_about = want.inertia_about(reference);
    for k in 0..6 {
        assert!(close(inertia[k], want.inertia[k]), "{inertia:?}");
        assert!(close(about[k], want_about[k]), "{about:?}");
    }
    let source = |item: &str| {
        report
            .lines
            .iter()
            .find(|l| l.item_number == item)
            .unwrap_or_else(|| panic!("{item}"))
    };
    assert_eq!(source(&cube).source, Source::Computed);
    assert!(source(&cube).detail.contains("bulkhead.stl (mm)"));
    assert!(source(&cube).detail.contains("Aluminium 6061"));
    assert_eq!(source(&bar).source, Source::Computed);
    assert_eq!(source(&motor).source, Source::Measured);
    let mystery = source(&odd);
    assert_eq!(mystery.source, Source::Missing);
    assert!(mystery.warnings.iter().any(|w| w.contains("Unobtainium")));
    // the restricted payload is left out and said so
    assert_eq!(report.restricted_lines, 1);
    assert!(report.lines.iter().all(|l| l.item_number != secret));
    assert!(!report.complete);
    assert!(report.warnings.iter().any(|w| w.contains("restricted")));

    // an admin sees the payload: its entered mass counts, without position
    let full = mass::rollup(
        &app.state,
        &admin,
        vehicle_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    assert_eq!(full.restricted_lines, 0);
    assert!(close(full.unplaced_mass_kg, 5.0));
    assert!(close(full.mass_kg, report.mass_kg + 5.0));
    for (a, b) in full.cg_m.unwrap().iter().zip(report.cg_m.unwrap()) {
        assert!(close(*a, b));
    }

    // the density override wins over the library
    let mut tx_settings = mass::MassSettingsUpdate {
        density_kg_m3: Some(1000.0),
        ..Default::default()
    };
    let odd_rev = revision_id(&app, &odd, "A").await;
    mass::update_settings(&app.pool, &max_actor, odd_rev, tx_settings.clone())
        .await
        .unwrap();
    // a unit: the mystery file is in inches
    tx_settings.length_unit = Some("in".into());
    let s = mass::update_settings(&app.pool, &max_actor, odd_rev, tx_settings)
        .await
        .unwrap();
    assert_eq!(s.files[0].length_unit.as_deref(), Some("in"));
    let r2 = mass::rollup(
        &app.state,
        &max_actor,
        vehicle_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    let line = r2.lines.iter().find(|l| l.item_number == odd).unwrap();
    assert_eq!(line.source, Source::Computed);
    let inch_cube = 1000.0 * (10.0 * 0.0254f64).powi(3);
    assert!(close(line.total_mass_kg.unwrap(), inch_cube), "{line:?}");
    assert!(line.detail.contains("(in)") && line.detail.contains("override"));

    // --- REST, MCP, page and exports --------------------------------------
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{vehicle}/revisions/A/mass-properties?ref=2,0,0"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert!(close(body["mass_kg"].as_f64().unwrap(), r2.mass_kg));
    assert_eq!(body["restricted_lines"], 1);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{vehicle}/revisions/A/mass-properties?ref=nope"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app.api(Method::GET, "/api/v1/materials", &max, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json().as_array().unwrap().len(), 2);

    let result = teamcraft::mcp::tools::call(
        &app.state,
        &max_actor,
        "get_mass_properties",
        &json!({ "item": vehicle, "reference": "0,0,0" }),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let s: &Value = &result["structuredContent"];
    assert!(close(s["mass_kg"].as_f64().unwrap(), r2.mass_kg));
    assert!(s["inertia_about_cg_kg_m2"]["ixx"].as_f64().unwrap() > 0.0);
    assert!(teamcraft::mcp::tools::is_read_only("get_mass_properties"));

    let r = app
        .get_page(
            &format!("/revisions/{vehicle_rev}/mass?ref=2,0,0"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let html = r.text();
    assert!(html.contains("Mass properties") && html.contains("bulkhead.stl"));
    assert!(html.contains("Unobtainium") || html.contains("override"));
    let r = app
        .get_page(&format!("/revisions/{vehicle_rev}/mass.csv"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let csv = r.text();
    assert!(csv.starts_with("level,") && csv.contains("TOTAL") && csv.contains(&cube));
    assert!(!csv.contains(&secret));
    let r = app
        .get_page(
            &format!("/revisions/{vehicle_rev}/mass.json"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(close(r.json()["mass_kg"].as_f64().unwrap(), r2.mass_kg));

    // --- revise keeps the settings -------------------------------------------
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{motor}/revisions/A/checkin"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.release(&max, ("rolf", &rolf), &[&motor]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{motor}/revisions/A/revise"),
            &max,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let b = revision_id(&app, &motor, "B").await;
    let s = mass::revision_settings(&app.pool, &max_actor, b)
        .await
        .unwrap();
    assert_eq!(s.settings.measured_mass_kg, Some(1.0));
    assert_eq!(s.settings.measured_cg_m, Some(vec![1.0, 0.0, 0.0]));
}

#[tokio::test]
async fn an_open_mesh_is_computed_but_flagged() {
    let app = app_or_skip!();
    let (admin, _) = app.user("ada", Role::Admin).await;
    let (max_actor, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    mass::create_material(
        &app.pool,
        &admin,
        MaterialInput {
            name: "PLA".into(),
            density_kg_m3: 1240.0,
            notes: "100 % infill".into(),
        },
    )
    .await
    .unwrap();
    let part = app
        .create_item(
            &max,
            json!({ "item_type": "part", "name": "Fin", "material": "PLA", "project": "ROCKET" }),
        )
        .await;
    // one triangle short: an open box
    let mut stl = box_stl([0.0; 3], [50.0; 3]);
    stl.truncate(84 + 11 * 50);
    stl[80..84].copy_from_slice(&11u32.to_le_bytes());
    upload(&app, &max, &part, "fin.stl", &stl).await;
    let rev = revision_id(&app, &part, "A").await;
    let r = mass::rollup(&app.state, &max_actor, rev, RevisionRule::Latest, [0.0; 3])
        .await
        .unwrap();
    assert_eq!(r.root_source, Source::Computed);
    assert!(
        r.warnings.iter().any(|w| w.contains("open mesh")),
        "{:?}",
        r.warnings
    );
    assert!(!r.complete);
}

#[tokio::test]
async fn files_of_several_parts_are_not_guessed() {
    let app = app_or_skip!();
    let (admin, _) = app.user("ada", Role::Admin).await;
    let (max_actor, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("max").await;
    mass::create_material(
        &app.pool,
        &admin,
        MaterialInput {
            name: "Steel".into(),
            density_kg_m3: 7850.0,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let item = |kind: &str, name: &str| json!({ "item_type": kind, "name": name, "material": "Steel", "project": "ROCKET" });

    // one model in two revisions of the file: picked automatically, and said so
    let single = app.create_item(&max, item("part", "Bracket")).await;
    upload(
        &app,
        &max,
        &single,
        "bracket.stl",
        &box_stl([0.0; 3], [10.0; 3]),
    )
    .await;
    upload(
        &app,
        &max,
        &single,
        "Bracket_rev-B.stl",
        &box_stl([0.0; 3], [10.0; 3]),
    )
    .await;
    let single_rev = revision_id(&app, &single, "A").await;
    let r = mass::rollup(
        &app.state,
        &max_actor,
        single_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    assert_eq!(r.root_source, Source::Computed, "{:?}", r.warnings);
    let g = r.root_geometry.as_ref().unwrap();
    assert_eq!((g.choice, g.other_formats), ("automatic", 1));
    assert!(r
        .root_detail
        .contains("chosen automatically; 1 other format of the same model"));

    // files of two parts: nothing is computed, the totals are incomplete
    let two = app.create_item(&max, item("part", "Harness")).await;
    upload(&app, &max, &two, "fin.stl", &box_stl([0.0; 3], [10.0; 3])).await;
    upload(
        &app,
        &max,
        &two,
        "body.stl",
        &box_stl([20.0, 0.0, 0.0], [10.0; 3]),
    )
    .await;
    let two_rev = revision_id(&app, &two, "A").await;
    let r = mass::rollup(
        &app.state,
        &max_actor,
        two_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    assert_eq!(r.root_source, Source::GeometryAmbiguous);
    assert_eq!(r.mass_kg, 0.0);
    assert!(!r.complete && r.cg_m.is_none());
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("2 files of 2 different parts")));
    let g = r.root_geometry.as_ref().unwrap();
    assert_eq!((g.choice, g.models, g.files), ("ambiguous", 2, 2));
    assert!(g.file.is_none() && g.candidates.len() == 2);

    // inside an assembly it contributes nothing, the good line still counts
    let asm = app.create_item(&max, item("assembly", "Rig")).await;
    add_line(&app, &max, &asm, &single, 1.0).await;
    add_line(&app, &max, &asm, &two, 1.0).await;
    let asm_rev = revision_id(&app, &asm, "A").await;
    let r = mass::rollup(
        &app.state,
        &max_actor,
        asm_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    assert!(close(r.mass_kg, 0.00785), "{}", r.mass_kg);
    assert!(!r.complete);
    let line = r.lines.iter().find(|l| l.item_number == two).unwrap();
    assert_eq!(line.source, Source::GeometryAmbiguous);
    assert_eq!(line.total_mass_kg, None);
    assert_eq!(r.missing_lines, 1);

    // REST, MCP, CSV and JSON carry the source and the candidates
    let body = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/mass-properties"),
            &max,
            None,
        )
        .await
        .json();
    let lines = body["lines"].as_array().unwrap();
    let l = lines
        .iter()
        .find(|l| l["item_number"] == two.as_str())
        .unwrap();
    assert_eq!(l["source"], "geometry_ambiguous");
    assert_eq!(l["geometry"]["choice"], "ambiguous");
    assert_eq!(l["geometry"]["candidates"].as_array().unwrap().len(), 2);
    let l = lines
        .iter()
        .find(|l| l["item_number"] == single.as_str())
        .unwrap();
    assert_eq!(l["geometry"]["choice"], "automatic");
    let result = teamcraft::mcp::tools::call(
        &app.state,
        &max_actor,
        "get_mass_properties",
        &json!({ "item": two }),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let s = &result["structuredContent"];
    assert_eq!(s["root_source"], "geometry_ambiguous");
    assert_eq!(s["root_geometry"]["choice"], "ambiguous");
    let csv = app
        .get_page(&format!("/revisions/{asm_rev}/mass.csv"), Some(&cookie))
        .await
        .text();
    assert!(csv
        .lines()
        .next()
        .unwrap()
        .ends_with("geometry_file,geometry_choice"));
    assert!(csv.contains("geometry ambiguous") && csv.contains(",automatic"));
    let html = app
        .get_page(&format!("/revisions/{two_rev}/mass"), Some(&cookie))
        .await
        .text();
    assert!(html.contains("Geometry ambiguous") && html.contains("<optgroup label=\"fin\">"));

    // choosing a file by hand wins; automatic can be restored
    let fin: Uuid =
        sqlx::query_scalar("SELECT id FROM files WHERE revision_id = $1 AND filename = 'fin.stl'")
            .bind(two_rev)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let r = app
        .post_form(
            &format!("/revisions/{two_rev}/mass/geometry"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("geometry_file", &fin.to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = mass::rollup(
        &app.state,
        &max_actor,
        two_rev,
        RevisionRule::Latest,
        [0.0; 3],
    )
    .await
    .unwrap();
    assert_eq!(r.root_source, Source::Computed, "{:?}", r.warnings);
    assert!(close(r.mass_kg, 0.00785));
    assert_eq!(r.root_geometry.as_ref().unwrap().choice, "chosen");
    let r = app
        .post_form(
            &format!("/revisions/{two_rev}/mass/geometry"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("geometry_file", "auto")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let stored = mass::revision_settings(&app.pool, &max_actor, two_rev)
        .await
        .unwrap();
    assert!(stored.settings.geometry_file_id.is_none());
    assert_eq!(stored.geometry.choice, "ambiguous");
    // a file of another revision is refused
    let other: Uuid = sqlx::query_scalar("SELECT id FROM files WHERE revision_id = $1 LIMIT 1")
        .bind(single_rev)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(
        mass::choose_geometry(&app.pool, &max_actor, two_rev, Some(other))
            .await
            .is_err()
    );
}

/// The NIST MTC assembly (`tests/fixtures/cad`) attached to one revision
/// without BOM lines, as in the bug report: the mass of one hex nut must not
/// stand for the assembly.
#[tokio::test]
async fn the_nist_assembly_in_one_revision_is_ambiguous() {
    let app = app_or_skip!();
    let (max_actor, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &max,
            json!({ "item_type": "assembly", "name": "NIST mtc crada assembly", "project": "ROCKET" }),
        )
        .await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly");
    let mut names = Vec::new();
    // The test server accepts 1 MB per upload: the fixture files that fit
    // (the 1.6 MB JT of the assembly is replaced by a stand-in with its
    // name; nothing is converted while the files are ambiguous).
    for sub in ["nx", "solidworks"] {
        for entry in std::fs::read_dir(dir.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            let ext = path.extension().unwrap().to_string_lossy().to_lowercase();
            let data = std::fs::read(&path).unwrap();
            if ["prt", "sldprt"].contains(&ext.as_str()) && data.len() < 1_000_000 {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                upload(&app, &max, &part, &name, &data).await;
                names.push(name);
            }
        }
    }
    upload(&app, &max, &part, "NIST mtc crada assembly.jt", b"stand-in").await;
    names.push("NIST mtc crada assembly.jt".into());
    assert!(names.len() >= 12, "{names:?}");
    let rev = revision_id(&app, &part, "A").await;
    let r = mass::rollup(&app.state, &max_actor, rev, RevisionRule::Latest, [0.0; 3])
        .await
        .unwrap();
    assert_eq!(r.root_source, Source::GeometryAmbiguous, "{:?}", r.warnings);
    assert_eq!(r.mass_kg, 0.0);
    assert!(!r.complete);
    let g = r.root_geometry.as_ref().unwrap();
    assert_eq!(g.choice, "ambiguous");
    assert!(g.file.is_none());
    assert_eq!(g.likely_file.as_deref(), Some("NIST mtc crada assembly.jt"));
    assert_eq!(g.candidates[0].model, "nist mtc crada assembly");
    assert!(g.candidates[0].files.iter().any(|c| c.likely));
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("different parts") && w.contains("assembly.jt")));
    // every file is a candidate; the assembly .prt and .jt are one model
    assert_eq!(g.files, names.len());
    assert_eq!(g.candidates[0].files.len(), 2);
}
