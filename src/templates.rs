//! HTML templates (MiniJinja), embedded into the binary at compile time.

use minijinja::Environment;

use crate::util;

macro_rules! embed {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../templates/", $name)))),*]
    };
}

/// All templates, keyed by their path below `templates/`.
pub const TEMPLATES: &[(&str, &str)] = embed![
    "base.html",
    "macros.html",
    "login.html",
    "error.html",
    "dashboard.html",
    "explorer.html",
    "items/list.html",
    "items/new.html",
    "revisions/show.html",
    "revisions/edit.html",
    "revisions/bom.html",
    "revisions/bom_compare.html",
    "revisions/save_as.html",
    "revisions/simulation_compare.html",
    "changes/list.html",
    "changes/new.html",
    "changes/show.html",
    "requests/list.html",
    "requests/new.html",
    "requests/show.html",
    "projects/list.html",
    "projects/show.html",
    "files/preview.html",
    "admin/users.html",
    "admin/user_edit.html",
    "admin/audit.html",
    "admin/webhooks.html",
    "admin/webhook_edit.html",
    "desktop/authorize.html",
    "formats.html",
    "imports/upload.html",
    "imports/preview.html",
    "notifications.html",
    "account.html",
    "assistant.html",
];

pub fn environment(instance_name: &str) -> Result<Environment<'static>, minijinja::Error> {
    let mut env = Environment::new();
    for (name, source) in TEMPLATES {
        env.add_template(name, source)?;
    }
    env.add_filter("dt", filter_dt);
    env.add_filter("num", filter_num);
    env.add_filter("filesize", filter_filesize);
    env.add_filter("label", filter_label);
    env.add_filter("urlencode", |value: String| util::pct_encode(&value));
    env.add_filter("age", filter_age);
    env.add_global("instance_name", instance_name.to_string());
    env.add_global("version", env!("CARGO_PKG_VERSION"));
    // cache busting of the embedded stylesheets, scripts and logos
    env.add_global("assets", crate::assets::version());
    Ok(env)
}

/// RFC 3339 timestamp → `2026-11-03 14:05 UTC`. (The browser script
/// rewrites `<time>` elements into the viewer's local time zone.)
fn filter_dt(value: Option<String>) -> String {
    match value {
        Some(v) => match chrono::DateTime::parse_from_rfc3339(&v) {
            Ok(t) => t
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string(),
            Err(_) => v,
        },
        None => "—".to_string(),
    }
}

/// RFC 3339 timestamp → its age now (`3 days`, `5 h`, `just now`), e.g.
/// how long a lock has been held; `—` for none.
fn filter_age(value: Option<String>) -> String {
    match value
        .as_deref()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
    {
        Some(t) => crate::domain::format_age(
            (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds(),
        ),
        None => "—".to_string(),
    }
}

/// Number with up to `decimals` (default 3) decimals; `—` for none.
fn filter_num(value: Option<f64>, decimals: Option<usize>) -> String {
    match value {
        Some(v) => util::format_number(v, decimals.unwrap_or(3)),
        None => "—".to_string(),
    }
}

fn filter_filesize(value: i64) -> String {
    util::format_bytes(value)
}

/// `in_review` → `In Review`; a few codes have fixed labels (`in_work` → `WIP`).
fn filter_label(value: Option<String>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value.as_str() {
        "in_work" => return "WIP".into(),
        "editor" => return "CAD editor".into(),
        _ => {}
    }
    value
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const ME: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER: &str = "22222222-2222-4222-8222-222222222222";
    const TS: &str = "2026-11-03T14:05:59.123456Z";

    fn me() -> Value {
        json!({ "id": ME, "username": "max", "display_name": "Max Muster", "role": "admin", "skin": "arch", "is_admin": true, "can_edit": true })
    }

    fn revision(state: &str, checked_out: bool) -> Value {
        json!({
            "id": "33333333-3333-4333-8333-333333333333", "item_id": "44444444-4444-4444-8444-444444444444",
            "item_number": "PRT-000001", "item_type": "part", "unit": "each", "seq": 2, "code": "B",
            "state": state, "name": "Bracket <b>", "description": "line 1\nline 2", "material": "AlMg3",
            "mass_kg": 0.125, "unit_cost": null,
            "checked_out_by": if checked_out { json!(OTHER) } else { Value::Null },
            "checked_out_by_name": if checked_out { json!("Erika") } else { Value::Null },
            "checked_out_at": if checked_out { json!(TS) } else { Value::Null },
            "created_by": ME, "created_by_name": "Max Muster",
            "created_at": TS, "updated_at": TS, "released_at": null
        })
    }

    fn item() -> Value {
        json!({
            "id": "44444444-4444-4444-8444-444444444444", "number": "PRT-000001", "item_type": "part",
            "unit": "each", "project_id": null, "project_code": "ROCKET", "created_by": ME,
            "created_by_name": "Max Muster", "created_at": TS
        })
    }

    fn change(status: &str) -> Value {
        json!({
            "id": "55555555-5555-4555-8555-555555555555", "number": "CHG-000001", "title": "Release bracket",
            "description": "why", "priority": "high", "status": status, "created_by": ME,
            "created_by_name": "Max Muster", "created_at": TS, "updated_at": TS,
            "submitted_at": TS, "closed_at": null
        })
    }

    fn request(status: &str) -> Value {
        json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "number": "ECR-000001", "title": "Crack <at> the weld",
            "description": "Problem\nproposal", "reason": "manufacturing", "priority": "high", "status": status,
            "requested_by": OTHER, "requested_by_name": "Erika", "assignee": ME, "assignee_name": "Max Muster",
            "assignee_username": "max", "decision_comment": "not now", "decided_by": ME,
            "decided_by_name": "Max Muster", "decided_at": TS, "due_date": "2026-12-01", "created_at": TS,
            "updated_at": TS, "closed_at": null, "item_count": 1
        })
    }

    fn audit_entry() -> Value {
        json!({ "id": 1, "at": TS, "user_id": ME, "username": "max", "action": "item.create",
                "entity_type": "item", "entity_id": "44444444-4444-4444-8444-444444444444",
                "summary": "Created Part PRT-000001", "details": {} })
    }

    fn user() -> Value {
        json!({ "id": OTHER, "username": "erika", "display_name": "Erika", "email": null,
                "auth_source": "local", "role": "editor", "active": true, "created_at": TS, "last_login_at": null,
                "skin": "teamcraft" })
    }

    const ROCKET: &str = "66666666-6666-4666-8666-666666666666";

    fn project() -> Value {
        json!({ "id": ROCKET, "code": "ROCKET", "name": "Sounding rocket",
                "description": "Rocketry", "active": true, "created_at": TS,
                "oidc_group": "teamcraft-rocket", "member_count": 3, "created_by": ME })
    }

    fn roles() -> Value {
        json!([{ "value": "viewer", "label": "Viewer" }, { "value": "editor", "label": "CAD editor" },
               { "value": "admin", "label": "Admin" }])
    }

    fn page() -> Value {
        json!({ "page": 1, "pages": 2, "total": 60, "prev": null, "next": 2 })
    }

    fn with_common(mut data: Value) -> Value {
        let map = data.as_object_mut().unwrap();
        map.insert("me".into(), me());
        map.insert(
            "flash".into(),
            json!({ "kind": "success", "message": "Saved <ok>" }),
        );
        map.insert("local_login".into(), json!(true));
        map.insert("skin".into(), json!("arch"));
        map.insert("skin_light".into(), json!(false));
        data
    }

    fn render(env: &Environment<'static>, name: &str, data: Value) -> String {
        env.get_template(name)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"))
            .render(&data)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"))
    }

    #[test]
    fn every_template_renders() {
        let env = environment("TeamCraft Test").expect("templates compile");
        let bom_line = json!({
            "id": "77777777-7777-4777-8777-777777777777", "parent_revision_id": "33333333-3333-4333-8333-333333333333",
            "child_item_id": "88888888-8888-4888-8888-888888888888", "child_number": "PRT-000002",
            "child_type": "part", "child_unit": "each", "find_number": 10, "quantity": 4.0, "notes": "M6",
            "child_latest_revision_id": "99999999-9999-4999-8999-999999999999", "child_latest_code": "A",
            "child_latest_state": "released", "child_name": "Bolt"
        });
        let version = |n: i32, comment: &str| {
            json!({ "id": ME, "file_id": OTHER, "version": n, "sha256": "cd".repeat(32), "size_bytes": 1000,
                    "content_type": "application/pdf", "comment": comment, "uploaded_by": ME,
                    "uploaded_by_name": "Max", "uploaded_at": TS })
        };
        let file = json!({ "id": OTHER, "revision_id": ME, "filename": "nozzle.step", "content_type": "model/step",
                           "size_bytes": 123456, "sha256": "ab".repeat(32), "uploaded_by": ME,
                           "uploaded_by_name": "Max", "uploaded_at": TS, "version": 2, "preview": true, "folder": "rover/nozzle",
                           "format": "step", "format_info": crate::cadformat::info("step"),
                           "versions": [version(2, "wall 2.5 mm"), version(1, "")] });
        let native = json!({ "id": ME, "revision_id": ME, "filename": "nozzle.SLDPRT",
                             "content_type": "application/octet-stream", "size_bytes": 4096,
                             "sha256": "ef".repeat(32), "uploaded_by": ME, "uploaded_by_name": "Max",
                             "uploaded_at": TS, "version": 1, "preview": false, "format": "solidworks-part",
                             "format_info": crate::cadformat::info("solidworks-part"),
                             "versions": [version(1, "")] });
        let unknown = json!({ "id": OTHER, "revision_id": ME, "filename": "notes.xyz",
                              "content_type": "application/x-unknown", "size_bytes": 10,
                              "sha256": "12".repeat(32), "uploaded_by": ME, "uploaded_by_name": "Max",
                              "uploaded_at": TS, "version": 1, "preview": false, "format": "",
                              "format_info": null, "versions": [version(1, "")] });
        let impact = |resolution: Option<&str>| {
            json!({ "id": ME, "item_id": ME, "item_number": "ASM-000001", "project_id": ROCKET,
                    "project_code": "ROCKET", "revision_id": OTHER, "revision_code": "A",
                    "revision_state": "released", "item_name": "Stage <1>", "source_item_id": OTHER,
                    "source_number": "PRT-000001", "source_revision_id": ME, "source_code": "B",
                    "source_name": "Bracket", "event": "released", "relation": "bom",
                    "detail": "find no. 10", "change_id": ME, "change_number": "CHG-000002",
                    "created_at": TS, "resolved_at": resolution.map(|_| TS), "resolution": resolution,
                    "resolved_by_name": resolution.map(|_| "Erika"),
                    "note": if resolution.is_some() { "fit unchanged <ok>" } else { "" } })
        };
        let references = json!({
            "outgoing": [{ "id": ME, "from_revision_id": ME, "to_item_id": OTHER, "to_number": "PRT-000009",
                           "to_type": "part", "kind": "drawing", "note": "sheet 2", "to_latest_revision_id": OTHER,
                           "to_latest_code": "C", "to_latest_state": "released", "to_name": "Nozzle",
                           "released_to_revision_id": OTHER, "released_to_code": "B", "created_at": TS }],
            "incoming": [{ "reference_id": ME, "kind": "specification", "note": "", "from_revision_id": OTHER,
                           "from_item_id": OTHER, "from_number": "DOC-000003", "from_code": "A",
                           "from_name": "Welding spec", "from_state": "released", "released_to_code": "A" }],
            "files": [{ "file_id": ME, "filename": "Stage.SLDASM", "format_kind": "assembly", "references": [
                { "name": "bracket.sldprt", "matches": [{ "item_id": OTHER, "item_number": "PRT-000001",
                  "item_name": "Bracket", "revision_id": OTHER, "revision_code": "B", "revision_state": "released",
                  "file_id": OTHER, "filename": "Bracket.SLDPRT", "same_revision": false }] },
                { "name": "Bolt M6.SLDPRT", "matches": [] }] }],
            "file_referrers": [{ "item_id": OTHER, "item_number": "DOC-000004", "revision_id": OTHER,
                                 "revision_code": "A", "revision_state": "in_work", "item_name": "Stage drawing",
                                 "file_id": OTHER, "filename": "Stage.SLDDRW", "referenced": "Stage.SLDASM" }],
            "suggestions": {
                "bom": [{ "item_number": "PRT-000001", "item_name": "Bracket", "via": "Stage.SLDASM → Bracket.SLDPRT" }],
                "drawing_of": [{ "item_number": "PRT-000007", "item_name": "Hub", "via": "Hub.SLDDRW → Hub.SLDPRT" }] },
            "kinds": crate::domain::ReferenceKind::ALL.iter()
                .map(|k| json!({ "value": k.as_str(), "label": k.label() })).collect::<Vec<_>>(),
        });
        let can_all = json!({ "edit": true, "checkout": true, "checkin": true, "cancel_checkout": true,
                              "revise": true, "obsolete": true, "delete": true, "master": true,
                              "add_to_change": true, "acknowledge": true });

        let html = render(
            &env,
            "dashboard.html",
            with_common(json!({
                "stats": { "items": 3, "revisions_in_work": 1, "revisions_in_review": 0, "revisions_released": 2,
                           "open_changes": 1, "files": 4, "stored_bytes": 2048 },
                "checkouts": [revision("in_work", true)], "reviews": [change("in_review")],
                "my_changes": [change("draft")], "activity": [audit_entry()], "impacts": [impact(None)],
                "assigned_requests": [request("in_triage")], "my_requests": [request("new")],
            })),
        );
        assert!(
            html.contains("Requests assigned to me") && html.contains("due 2026-12-01"),
            "change requests on the dashboard"
        );
        assert!(html.contains("Saved &lt;ok&gt;"), "flash is escaped");
        assert!(html.contains("2 KiB"));
        assert!(
            html.contains("Dependents to check")
                && html.contains("uses it in its BOM")
                && html.contains("Stage &lt;1&gt;"),
            "open impacts on the dashboard"
        );

        let entry = json!({ "item_id": ME, "number": "PRT-000001", "item_type": "part", "project_id": ROCKET,
                            "project_code": "ROCKET", "revision_id": revision("in_work", true)["id"],
                            "code": "A", "name": "Bracket <b>",
                            "state": "in_work", "checked_out_by": ME, "checked_out_by_name": "Max Muster",
                            "checked_out_at": TS, "updated_at": TS, "file_count": 2 });
        let html = render(
            &env,
            "explorer.html",
            with_common(json!({
                "folders": [{ "id": ROCKET, "code": "ROCKET", "name": "Sounding rocket", "items": 3, "locked": 1 }],
                "folder": ROCKET, "entries": [entry], "limit": 500, "q": "",
                "selected": { "rev": revision("in_work", true), "files": [file.clone()],
                              "can": { "checkout": false, "checkin": true, "cancel_checkout": true } },
                "back": crate::util::pct_encode(&format!("/vault?folder={ROCKET}&rev={OTHER}")),
                "max_upload_mb": 512,
            })),
        );
        for expected in [
            "Sounding rocket",
            "Bracket &lt;b&gt;",
            "Check in and release",
            "/cancel-checkout?back=",
            "?back=%2Fvault%3Ffolder%3D",
            "enctype=\"multipart/form-data\"",
            "class=\"selected\"",
        ] {
            assert!(html.contains(expected), "vault explorer shows {expected}");
        }
        let html = render(
            &env,
            "explorer.html",
            with_common(
                json!({ "folders": [], "folder": "mine", "entries": [], "limit": 500, "q": "",
                                "selected": null, "back": "%2Fvault", "max_upload_mb": 512 }),
            ),
        );
        assert!(html.contains("You have nothing checked out"));

        // Save As and the BOM comparison
        let html = render(
            &env,
            "revisions/save_as.html",
            with_common(
                json!({ "rev": revision("released", false), "projects": [project()],
                                "file_count": 3, "line_count": 2 }),
            ),
        );
        assert!(
            html.contains("Copy the current files (3)") && html.contains("Copy the BOM (2 lines)")
        );
        assert!(html.contains("/save-as") && html.contains("ROCKET"));
        let diff = |level: usize, change: &str, before: Value, after: Value| {
            json!({ "level": level, "change": change, "child_item_id": OTHER, "child_number": "PRT-000002",
                    "child_name": "Bolt <M6>", "unit": "each", "before": before, "after": after })
        };
        let values = |find: i32, qty: f64, rev: Value| json!({ "find_number": find, "quantity": qty, "notes": "", "revision": rev });
        let diffs = json!([
            diff(1, "added", Value::Null, values(10, 2.0, Value::Null)),
            diff(1, "removed", values(20, 1.0, Value::Null), Value::Null),
            diff(
                1,
                "changed",
                values(30, 1.0, Value::Null),
                values(30, 4.0, Value::Null)
            )
        ]);
        let summary = json!({ "added": 1, "removed": 1, "changed": 1, "unchanged": 0 });
        let html = render(
            &env,
            "revisions/bom_compare.html",
            with_common(json!({
                "rev": revision("in_work", false), "before": revision("released", false),
                "after": revision("in_work", false), "revisions": [revision("in_work", false)],
                "depth": "1", "rule": "latest", "diffs": diffs, "summary": summary,
            })),
        );
        assert!(html.contains("1 added · 1 removed · 1 changed"));
        assert!(html.contains("bom-diff-removed") && html.contains("Bolt &lt;M6&gt;"));
        assert!(
            !html.contains("<th>Lvl</th>"),
            "levels only over all levels"
        );
        let deep = render(
            &env,
            "revisions/bom_compare.html",
            with_common(json!({
                "rev": revision("in_work", false), "before": revision("released", false),
                "after": revision("in_work", false), "revisions": [revision("in_work", false)],
                "depth": "all", "rule": "as_released", "summary": summary,
                "diffs": [diff(1, "changed", values(10, 1.0, json!("A")), values(10, 1.0, json!("B"))),
                          diff(2, "added", Value::Null, values(10, 2.0, json!("A"))),
                          diff(9, "removed", values(20, 1.0, Value::Null), Value::Null)],
            })),
        );
        assert!(
            deep.contains("<th>Lvl</th>")
                && deep.contains("indent-2")
                && deep.contains("indent-deep")
        );
        assert!(
            deep.contains("A → <strong>B</strong>"),
            "a new child revision"
        );
        assert!(deep.contains("value=\"all\" selected"));

        render(
            &env,
            "login.html",
            json!({ "next": "/", "next_encoded": "%2F", "error": "bad", "oidc_name": "Single Sign-On",
                                           "username": "max", "local_login": true, "flash": null }),
        );
        render(
            &env,
            "error.html",
            json!({ "status": 404, "reason": "Not Found", "message": "gone", "flash": null, "local_login": true }),
        );

        let html = render(
            &env,
            "items/list.html",
            with_common(json!({
                "items": [{ "id": ME, "number": "PRT-000001", "item_type": "part", "unit": "each", "project_code": null,
                            "created_at": TS, "latest_revision_id": OTHER, "latest_code": "A", "latest_state": "in_work",
                            "name": "Bracket", "matched_file": "bracket.step" }],
                "pagination": page(), "base_query": "q=a%20b",
                "filter": { "q": "a b", "type": "part", "project": "", "state": "", "system": "creo" },
                "projects": [project()], "types": [{ "value": "part", "label": "Part" }],
                "states": [{ "value": "in_work", "label": "In Work" }],
                "systems": crate::cadformat::SYSTEMS,
            })),
        );
        assert!(
            html.contains("<option value=\"creo\" selected>PTC Creo"),
            "CAD program filter"
        );

        let html = render(
            &env,
            "formats.html",
            with_common(
                json!({ "systems": crate::cadformat::catalogue(), "previews": [".stl", ".step"] }),
            ),
        );
        for expected in [
            "SolidWorks assembly",
            ".sldasm",
            "NX part",
            "Creo part",
            ".catproduct",
            "JT visualisation",
            ".x_t",
            "/items?system=inventor",
            "by content",
        ] {
            assert!(html.contains(expected), "formats page lists {expected}");
        }
        render(
            &env,
            "items/new.html",
            with_common(json!({
                "form": { "item_type": "part", "number": "", "name": "", "description": "", "project_id": "",
                          "unit": "each", "material": "", "mass_kg": "", "unit_cost": "" },
                "error": "Name must not be empty.", "projects": [project()],
                "types": [{ "value": "part", "label": "Part" }], "units": units(),
            })),
        );

        for (state, checked_out) in [("in_work", true), ("in_work", false), ("released", false)] {
            let html = render(
                &env,
                "revisions/show.html",
                with_common(json!({
                    "rev": revision(state, checked_out), "item": item(), "revisions": [revision(state, checked_out)],
                    "is_latest": false, "latest_id": OTHER, "files": [file.clone(), native.clone(), unknown.clone()], "native_without_preview": state == "released", "lines": [bom_line.clone()],
                    "where_used": [{ "line_id": ME, "parent_revision_id": OTHER, "parent_item_id": ME,
                                     "parent_number": "ASM-000001", "parent_code": "A", "parent_name": "Assembly",
                                     "parent_state": "released", "find_number": 10, "quantity": 2.0 }],
                    "changes": [change("draft")], "open_change": change("draft"), "drafts": [change("draft")],
                    "history": [audit_entry()], "projects": [project()], "units": units(),
                    "references": references.clone(), "open_impacts": [impact(None)],
                    "resolved_impacts": [impact(Some("acknowledged"))],
                    "open_requests": [request("in_triage")],
                    "can": can_all.clone(), "max_upload_mb": 512, "assistant_enabled": state == "released",
                    "lifecycle": {
                        "states": crate::domain::LifecycleState::ALL,
                        "next": crate::domain::rules_from(crate::domain::LifecycleState::parse(state).unwrap()),
                        "rules": crate::domain::LIFECYCLE_RULES,
                    },
                })),
            );
            assert!(
                html.contains("aria-current=\"step\""),
                "current lifecycle state is marked"
            );
            assert!(html.contains("four-eyes principle"), "rules are listed");
            assert!(html.contains("Bracket &lt;b&gt;"), "names are escaped");
            assert!(
                html.contains("/files/") && html.contains("/restore"),
                "rollback offered"
            );
            assert!(html.contains("Check in and release"), "check-in form");
            assert!(
                html.contains(&format!("/files/{OTHER}/preview"))
                    && html.contains(&format!("/file-versions/{ME}/preview")),
                "3D preview of the file and of its versions"
            );
            assert!(
                html.contains("STEP (ISO 10303)")
                    && html.contains("fmt fmt-exchange")
                    && html.contains("SolidWorks part")
                    && html.contains("fmt fmt-part"),
                "format labels"
            );
            for expected in [
                "Check needed:",
                "/acknowledge",
                "Drawing of",
                "PRT-000009",
                "released with",
                "Welding spec",
                "not in TeamCraft",
                "Add to BOM",
                "Hub.SLDDRW",
                "Stage.SLDDRW references Stage.SLDASM",
                "fit unchanged &lt;ok&gt;",
                "Request change…",
                "1 open change request on",
                "file=nozzle.step",
            ] {
                assert!(html.contains(expected), "revision page shows {expected}");
            }
            assert!(
                html.contains("x-unknown"),
                "unknown formats show the content type"
            );
            assert_eq!(
                html.contains("No 3D preview for native CAD files"),
                state == "released",
                "hint for native files without a neutral export"
            );
            assert_eq!(
                html.contains("Draft release notes")
                    && html.contains("Check before release")
                    && html.contains("name=\"item\" value=\"PRT-000001\"")
                    && html.contains("name=\"revision\" value=\"B\""),
                state == "released",
                "assistant quick actions only when enabled"
            );
        }

        // Simulations: basis links and results on the simulation, the
        // simulations based on a part on the part
        let basis = |out_of_date: Option<&str>| {
            json!({ "id": ME, "revision_id": OTHER, "role": "geometry", "note": "fillet <r2>",
                    "basis_item_id": OTHER, "basis_number": "PRT-000001", "basis_type": "part",
                    "basis_revision_id": OTHER, "basis_code": "A", "basis_state": "released",
                    "basis_name": "Bracket", "file_version_id": ME, "file_id": OTHER,
                    "file_name": "bracket.step", "file_version": 2, "file_current_version": 3,
                    "released_revision_id": ME, "released_code": "B", "created_at": TS,
                    "out_of_date": out_of_date })
        };
        let sim_result = |name: &str, status: &str, upper: Option<f64>| {
            json!({ "id": ME, "revision_id": OTHER, "position": 1, "name": name, "value": 182.5,
                    "unit": "MPa", "lower_limit": null, "upper_limit": upper, "load_case": "LC1",
                    "solver": "CalculiX", "solver_version": "2.21", "note": "", "updated_at": TS,
                    "status": status })
        };
        let basis_roles: Vec<_> = crate::domain::BasisRole::ALL
            .iter()
            .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
            .collect();
        for (state, checked_out) in [("in_work", false), ("released", false)] {
            let mut rev = revision(state, checked_out);
            rev["item_type"] = json!("simulation");
            rev["item_number"] = json!("SIM-000001");
            let mut sim_item = item();
            sim_item["item_type"] = json!("simulation");
            let editable = state == "in_work";
            let html = render(
                &env,
                "revisions/show.html",
                with_common(json!({
                    "rev": rev.clone(), "item": sim_item, "revisions": [rev.clone(), revision("released", false)],
                    "is_latest": true, "latest_id": OTHER, "files": [], "native_without_preview": false,
                    "lines": [], "where_used": [], "changes": [], "open_change": null, "drafts": [],
                    "history": [], "projects": [], "units": units(), "references": references.clone(),
                    "open_impacts": [], "resolved_impacts": [],
                    "simulation": {
                        "bases": [basis(Some("PRT-000001/B was released (simulated: A)")), basis(None)],
                        "results": [sim_result("Max. stress <vM>", "pass", Some(250.0)),
                                    sim_result("Safety factor", "fail", None),
                                    sim_result("Mass", "none", None)],
                        "summary": { "total": 3, "pass": 1, "fail": 1, "without_limits": 1 },
                        "out_of_date": 1, "roles": basis_roles.clone(),
                    },
                    "based_simulations": [],
                    "can": if editable { can_all.clone() } else { json!({}) }, "max_upload_mb": 512,
                    "lifecycle": {
                        "states": crate::domain::LifecycleState::ALL,
                        "next": crate::domain::rules_from(crate::domain::LifecycleState::parse(state).unwrap()),
                        "rules": crate::domain::LIFECYCLE_RULES,
                    },
                })),
            );
            for expected in [
                "Based on",
                "B was released (simulated: A)",
                "bracket.step",
                "fillet &lt;r2&gt;",
                "Out of date",
                "Up to date",
                "Max. stress &lt;vM&gt;",
                "badge badge-pass",
                "badge badge-fail",
                "1 failed",
                "no limit",
                "/simulation/compare",
                "/simulation/results.csv",
            ] {
                assert!(html.contains(expected), "simulation page shows {expected}");
            }
            assert!(!html.contains("id=\"bom\""), "no BOM card for simulations");
            assert!(!html.contains("id=\"simulations\""));
            assert_eq!(
                html.contains("/simulation/results/import") && html.contains("/simulation/bases"),
                editable,
                "editing only while checked out"
            );
        }
        let html = render(
            &env,
            "revisions/show.html",
            with_common(json!({
                "rev": revision("released", false), "item": item(), "revisions": [revision("released", false)],
                "is_latest": true, "latest_id": OTHER, "files": [], "native_without_preview": false,
                "lines": [], "where_used": [], "changes": [], "open_change": null, "drafts": [],
                "history": [], "projects": [], "units": units(), "references": references.clone(),
                "open_impacts": [], "resolved_impacts": [], "simulation": null,
                "based_simulations": [{
                    "simulation_item_id": OTHER, "simulation_number": "SIM-000001", "simulation_code": "A",
                    "simulation_state": "in_work", "simulation_name": "Bracket – static <LC1>",
                    "result_count": 3, "failed_count": 1, "open_impact_id": ME,
                    "basis": basis(Some("PRT-000001/B was released (simulated: A)")) }],
                "can": {}, "max_upload_mb": 512,
                "lifecycle": {
                    "states": crate::domain::LifecycleState::ALL,
                    "next": crate::domain::rules_from(crate::domain::LifecycleState::Released),
                    "rules": crate::domain::LIFECYCLE_RULES,
                },
            })),
        );
        for expected in [
            "id=\"simulations\"",
            "SIM-000001/A",
            "Bracket – static &lt;LC1&gt;",
            "1 of 3 failed",
            "check open",
            "Out of date",
        ] {
            assert!(html.contains(expected), "part page shows {expected}");
        }
        let diff = |change: &str, before: Value, after: Value, delta: Value| {
            json!({ "change": change, "name": "Stress <max>", "load_case": "LC1", "unit": "MPa",
                    "before": before, "after": after, "delta": delta,
                    "delta_percent": if delta.is_null() { Value::Null } else { json!(-10.0) } })
        };
        let html = render(
            &env,
            "revisions/simulation_compare.html",
            with_common(json!({
                "rev": revision("in_work", false), "before": revision("released", false),
                "after": revision("in_work", false), "revisions": [revision("released", false)],
                "diffs": [diff("changed", sim_result("Stress", "fail", Some(150.0)),
                               sim_result("Stress", "pass", Some(250.0)), json!(-20.0)),
                          diff("added", Value::Null, sim_result("Mass", "none", None), Value::Null),
                          diff("removed", sim_result("Old", "pass", Some(1e9)), Value::Null, Value::Null)],
                "summary": { "added": 1, "removed": 1, "changed": 1, "unchanged": 0 },
            })),
        );
        assert!(html.contains("1 added · 1 removed · 1 changed · 0 unchanged"));
        assert!(html.contains("Stress &lt;max&gt;") && html.contains("-10 %"));
        assert!(html.contains("badge badge-fail") && html.contains("badge badge-pass"));

        // 3D preview: the server delivers the mesh, the page only draws it
        let preview = |format: &str, is_current: bool| {
            let mut shown = version(1, "first <draft>");
            shown["id"] = json!(ME);
            with_common(json!({
                "file": file.clone(), "shown": shown, "is_current": is_current,
                "versions": [version(2, "wall 2.5 mm"), version(1, "first <draft>")],
                "rev": revision("released", false),
                "format": format, "is_brep": format == "step", "is_points": format == "las",
            }))
        };
        let html = render(&env, "files/preview.html", preview("stl", true));
        assert!(html.contains(&format!("data-src=\"/file-versions/{ME}/mesh\"")));
        assert!(!html.contains("viewer-stl"), "no STL export of an STL file");
        assert!(html.contains("/static/viewer.js?v="));
        assert!(html.contains("first &lt;draft&gt;"), "comments are escaped");
        let html = render(&env, "files/preview.html", preview("step", false));
        assert!(html.contains("older version"));
        assert!(html.contains("tessellated"), "STEP hint");
        assert!(
            html.contains("id=\"viewer-stl\"") && html.contains("data-name=\"nozzle.step\""),
            "STL export of STEP models"
        );
        let html = render(&env, "files/preview.html", preview("las", true));
        assert!(
            !html.contains("viewer-stl"),
            "no STL export of a point cloud"
        );
        render(
            &env,
            "revisions/edit.html",
            with_common(json!({ "rev": revision("in_work", false), "error": null })),
        );
        render(
            &env,
            "revisions/edit.html",
            with_common(json!({
                "rev": revision("in_work", false), "error": "Mass: 'x' is not a number.", "mass_raw": "x", "cost_raw": ""
            })),
        );

        let node = json!({
            "line_id": ME, "level": 2, "find_number": 10, "quantity": 2.0, "notes": "", "item_id": OTHER,
            "item_number": "PRT-000002", "item_type": "part", "unit": "each",
            "revision": { "id": OTHER, "code": "A", "state": "released", "name": "Bolt", "mass_kg": 0.01, "unit_cost": 0.2 },
            "unit_mass_kg": 0.01, "total_mass_kg": 0.02, "unit_cost_total": 0.2, "total_cost": 0.4,
            "mass_complete": true, "cost_complete": false, "cycle": false, "children": []
        });
        let mut unresolved = node.clone();
        unresolved["revision"] = Value::Null;
        unresolved["level"] = json!(9);
        let bom_html = render(
            &env,
            "revisions/bom.html",
            with_common(json!({
                "rev": revision("released", false), "rule": "released",
                "tree": { "total_mass_kg": 1.5, "total_cost": 10.0, "mass_complete": false, "cost_complete": true,
                          "line_count": 2, "unresolved_count": 1 },
                "rows": [
                    { "level": 2, "indent": "\u{2003}", "has_children": false, "node": node },
                    { "level": 9, "indent": "", "has_children": false, "node": unresolved }
                ],
            })),
        );
        assert!(
            bom_html.contains("/package.zip?rule=released")
                && bom_html.contains("Pack and Go (ZIP)"),
            "Pack and Go with the selected rule"
        );

        render(
            &env,
            "changes/list.html",
            with_common(json!({
                "changes": [change("in_review")], "status": "in_review",
                "statuses": [{ "value": "draft", "label": "Draft" }], "pagination": page(),
            })),
        );
        render(
            &env,
            "changes/new.html",
            with_common(json!({
                "form": { "title": "", "description": "", "priority": "normal", "revision_id": OTHER },
                "error": null, "priorities": ["low", "normal"], "revision": revision("in_work", false),
            })),
        );
        for status in ["draft", "in_review", "rejected", "released"] {
            let html = render(
                &env,
                "changes/show.html",
                with_common(json!({
                    "change": change(status), "status_label": "Draft",
                    "implements": [request("accepted")],
                    "items": [{ "revision_id": OTHER, "item_id": ME, "item_number": "PRT-000001", "code": "A",
                                "name": "Bracket", "state": "in_work", "checked_out_by_name": "Erika" }],
                    "reviews": [{ "reviewer_id": OTHER, "reviewer_username": "erika", "reviewer_name": "Erika",
                                  "decision": "approved", "comment": "ok", "decided_at": TS }],
                    "candidates": [user()], "history": [audit_entry()], "priorities": ["low", "normal"],
                    "impact": [{ "root_item_id": ME, "level": 1, "item_id": OTHER, "item_number": "ASM-000001",
                                 "item_name": "Stage", "revision_id": OTHER, "revision_code": "A",
                                 "revision_state": "released", "relation": "bom", "via_number": "PRT-000001",
                                 "in_change": false }],
                    "can": { "modify": true, "edit_text": true, "submit": true, "withdraw": true, "reopen": true,
                             "cancel": true, "review": true },
                })),
            );
            assert!(
                html.contains("Impact analysis") && html.contains("uses PRT-000001 in its BOM"),
                "impact analysis on the change page"
            );
            assert!(
                html.contains("Implements") && html.contains("ECR-000001"),
                "the change page names the request it implements"
            );
        }

        // change requests (ECR)
        let html = render(
            &env,
            "requests/list.html",
            with_common(json!({
                "requests": [request("new"), request("implemented")],
                "filter": { "status": "open", "scope": "mine", "item": "PRT-000001" },
                "statuses": crate::domain::RequestStatus::ALL.iter()
                    .map(|s| json!({ "value": s.as_str(), "label": s.label() })).collect::<Vec<_>>(),
                "base_query": "status=open&scope=mine&item=PRT-000001", "pagination": page(),
            })),
        );
        assert!(html.contains("ECR-000001") && html.contains("Crack &lt;at&gt; the weld"));
        assert!(html.contains("value=\"mine\" selected") && html.contains("In Triage"));
        let reasons = json!(crate::domain::RequestReason::ALL
            .iter()
            .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
            .collect::<Vec<_>>());
        let html = render(
            &env,
            "requests/new.html",
            with_common(json!({
                "form": { "title": "", "description": "Found in nozzle.step:\n\n", "reason": "defect",
                          "priority": "normal", "due_date": "", "item_number": "", "revision_id": OTHER,
                          "note": "File: nozzle.step" },
                "error": "Title must not be empty.", "priorities": ["low", "normal"], "reasons": reasons,
                "revision": revision("released", false),
            })),
        );
        assert!(html.contains("PRT-000001/B") && html.contains("File: nozzle.step"));
        assert!(html.contains("value=\"defect\" selected") && html.contains("Raise request"));
        let request_item = json!({
            "item_id": ME, "item_number": "PRT-000001", "item_type": "part", "project_id": ROCKET,
            "project_code": "ROCKET", "revision_id": OTHER, "revision_code": "B", "revision_state": "released",
            "name": "Bracket <b>", "latest_revision_id": OTHER, "latest_code": "C", "latest_state": "in_work",
            "note": "weld seam" });
        let attachment = |ct: &str, name: &str| {
            json!({ "id": OTHER, "request_id": ME, "filename": name, "content_type": ct, "size_bytes": 2048,
                    "sha256": "ab".repeat(32), "uploaded_by": ME, "uploaded_by_name": "Max", "uploaded_at": TS })
        };
        let can_all = json!({ "edit": true, "start_triage": true, "decide": true, "assign": true, "reopen": true,
                              "withdraw": true, "implement": true, "comment": true, "attach": true });
        for status in [
            "new",
            "in_triage",
            "accepted",
            "rejected",
            "deferred",
            "implemented",
            "withdrawn",
        ] {
            let html = render(
                &env,
                "requests/show.html",
                with_common(json!({
                    "request": request(status), "status_label": "New", "reason_label": "Defect / error",
                    "items": [request_item.clone()], "in_work": [request_item.clone()],
                    "comments": [{ "id": OTHER, "author_id": OTHER, "author_name": "Erika",
                                   "body": "Seen on <three> parts", "created_at": TS }],
                    "attachments": [attachment("image/png", "crack.png"), attachment("application/pdf", "report.pdf")],
                    "images": [attachment("image/png", "crack.png")],
                    "changes": [change("draft")], "open_changes": [change("draft")], "candidates": [user()],
                    "history": [audit_entry()], "watching": false, "can": can_all.clone(),
                    "priorities": ["low", "normal"], "reasons": reasons.clone(), "max_attachment_mb": 10,
                })),
            );
            for expected in [
                "Crack &lt;at&gt; the weld",
                "Seen on &lt;three&gt; parts",
                "Create change order",
                "PRT-000001/C",
                "Link an open change order",
                "name=\"decision\" value=\"accept\"",
                "/request-attachments/",
                "?inline=1",
                "enctype=\"multipart/form-data\"",
                "CHG-000001",
                "weld seam",
            ] {
                assert!(
                    html.contains(expected),
                    "request page ({status}) shows {expected}"
                );
            }
            // deferred → deferred is no transition: no Defer button then
            assert_eq!(
                html.contains("name=\"decision\" value=\"defer\""),
                status != "deferred",
                "request page ({status}): Defer button"
            );
        }

        render(
            &env,
            "projects/list.html",
            with_common(json!({ "projects": [project()], "my_projects": [ROCKET] })),
        );
        let html = render(
            &env,
            "projects/show.html",
            with_common(json!({
                "project": project(),
                "members": [{ "user_id": OTHER, "username": "erika", "display_name": "Erika", "role": "editor",
                              "active": true, "source": "oidc", "added_at": TS, "manager": true }],
                "candidates": [user()], "is_member": true, "can_edit": true, "oidc_enabled": true,
                "can_manage": true,
                "numbering": { "scheme": "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}", "is_default": false,
                               "next": [{ "item_type": "part", "label": "Part", "number": "ROCKET-PRT-2026-0001" }] },
                "scheme_input": "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}", "previewing": true,
                "scheme_error": "Invalid numbering scheme 'x': <bad>", "default_scheme": "{TYPE}-{SEQ:6}",
            })),
        );
        assert!(html.contains("CAD editor"));
        assert!(html.contains("via IdP group"));
        assert!(html.contains("manager"), "project managers are marked");
        assert!(html.contains("Add member"), "managers add members");
        assert!(
            html.contains("ROCKET-PRT-2026-0001"),
            "next numbers are shown"
        );
        assert!(html.contains("preview – not saved"));
        assert!(html.contains("value=\"{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}\""));
        assert!(html.contains("&lt;bad&gt;"), "scheme errors are escaped");
        assert!(html.contains("/numbering\""), "managers edit the scheme");
        render(
            &env,
            "admin/users.html",
            with_common(json!({
                "users": [user()], "roles": roles(),
                "oidc": { "admin_group": "a", "editor_group": "e", "viewer_group": null },
            })),
        );
        render(
            &env,
            "admin/user_edit.html",
            with_common(json!({
                "user": user(), "roles": roles(), "oidc_enabled": false, "memberships": [project()],
                "tokens": [{ "id": ME, "name": "ci", "token_prefix": "tc_pat_abc123", "created_at": TS,
                             "last_used_at": null, "expires_at": TS }],
            })),
        );
        render(
            &env,
            "admin/audit.html",
            with_common(json!({
                "entries": [audit_entry()], "filter": { "entity_type": "item", "username": null, "action": null },
                "base_query": "entity_type=item", "pagination": page(), "per_page": 50,
            })),
        );
        let webhook = json!({
            "id": ME, "name": "ERP <export>", "url": "https://erp.example.org/in?token=x",
            "secret_hint": "tc_whsec_…a1b2", "events": ["revision.released", "change.released"],
            "project_id": OTHER, "project_code": "ROVER", "active": true, "created_by": ME,
            "created_by_name": "Max", "created_at": TS, "updated_at": TS, "secret_rotated_at": TS,
            "pending": 2, "failed_recently": 1, "last_status": "failed", "last_delivery_at": TS,
            "host": "erp.example.org",
        });
        let events = json!(crate::domain::WebhookEvent::ALL
            .iter()
            .map(|e| json!({ "value": e.as_str(), "label": e.label(),
                             "on": *e == crate::domain::WebhookEvent::ChangeReleased }))
            .collect::<Vec<_>>());
        let projects = json!([{ "id": OTHER, "code": "ROVER", "name": "Planetary rover" }]);
        let html = render(
            &env,
            "admin/webhooks.html",
            with_common(json!({
                "webhooks": [webhook.clone()], "events": events.clone(), "projects": projects.clone(),
                "targets": { "allowed_hosts": [], "allow_private": false },
            })),
        );
        for expected in [
            "ERP &lt;export&gt;",
            "badge badge-failed",
            "1 failed in 7 days",
            "value=\"simulation.result_added\"",
            "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
        ] {
            assert!(html.contains(expected), "webhook list shows {expected}");
        }
        assert!(!html.contains("token=x"), "the list shows the host only");
        let delivery = |status: &str, attempts: i32, code: Option<i32>, error: &str| {
            json!({
                "id": OTHER, "webhook_id": ME, "event_id": ME, "event": "change.released",
                "payload": {}, "payload_text": "{\n  \"event\": \"change.released\" <x>\n}",
                "status": status, "attempts": attempts, "next_attempt_at": TS,
                "last_status_code": code, "last_error": error, "created_at": TS, "updated_at": TS,
                "delivered_at": if status == "sent" { json!(TS) } else { json!(null) },
            })
        };
        let html = render(
            &env,
            "admin/webhook_edit.html",
            with_common(json!({
                "webhook": webhook, "host": "erp.example.org", "secret": "tc_whsec_0123",
                "events": events, "projects": projects, "max_attempts": 8, "retention_days": 30,
                "targets": { "allowed_hosts": ["erp.example.org"], "allow_private": false },
                "deliveries": [
                    delivery("sent", 1, Some(200), ""),
                    delivery("pending", 2, Some(503), "HTTP 503"),
                    delivery("failed", 8, None, "connection failed: refused"),
                ],
            })),
        );
        for expected in [
            "tc_whsec_0123",
            "data-copy=\"webhook-secret\"",
            "tc_whsec_…a1b2",
            "badge badge-retrying",
            "badge badge-failed",
            "connection failed: refused",
            "&lt;x&gt;",
            "/deliveries/",
            "value=\"change.released\" checked",
            "selected>ROVER",
        ] {
            assert!(html.contains(expected), "webhook page shows {expected}");
        }
        assert!(
            !html.contains("HTTP 503 – HTTP 503"),
            "the status code once"
        );
        let html = render(
            &env,
            "desktop/authorize.html",
            with_common(json!({
                "req": { "port": 53682, "state": "abcdefghijklmnop", "challenge": "c".repeat(43),
                         "device": "Leo's <MacBook>" },
                "token_days": 90,
            })),
        );
        assert!(
            html.contains("Leo&#x27;s &lt;MacBook&gt;"),
            "device name is escaped"
        );
        assert!(html.contains("name=\"decision\" value=\"allow\""));
        let html = render(
            &env,
            "account.html",
            with_common(json!({
                "user": user(), "tokens": [], "new_token": { "name": "ci", "value": "tc_pat_secret" },
                "api_base": "http://localhost:8080/api/v1",
                "skins": crate::domain::SKINS,
                "notify": {
                    "settings": { "email": true, "email_address": "max@example.org", "mattermost": false,
                                  "mattermost_username": null, "muted": ["item_update"] },
                    "kinds": crate::domain::NotificationKind::MUTABLE.iter().map(|k| json!({
                        "value": k.as_str(), "label": k.label(), "on": *k != crate::domain::NotificationKind::ItemUpdate
                    })).collect::<Vec<_>>(),
                    "email_enabled": true, "mattermost_enabled": false, "mattermost_direct": false,
                },
                "unread_notifications": 3,
            })),
        );
        assert!(html.contains("tc_pat_secret"));
        assert!(
            html.contains("value=\"review_requested\" checked")
                && html.contains("value=\"item_update\" >")
                && html.contains("max@example.org")
                && html.contains("Mattermost is not configured"),
            "notification settings"
        );
        assert!(
            html.contains("class=\"bell has-unread\"")
                && html.contains("<span class=\"bell-count\">3</span>"),
            "bell with the unread count"
        );
        let notes = render(
            &env,
            "notifications.html",
            with_common(json!({
                "notifications": [
                    { "id": ME, "kind": "review_requested", "title": "Review requested: CHG-000004 <x>",
                      "body": "Erika asks you to review PRT-000003/B.", "link": "/changes/1", "subject_type": "change",
                      "subject_ref": "CHG-000004", "actor_name": "Erika", "created_at": TS, "read_at": null,
                      "email_state": "sent", "chat_state": "failed" },
                    { "id": OTHER, "kind": "impact", "title": "Check needed: ASM-000001", "body": "",
                      "link": "/revisions/1", "subject_type": "revision", "subject_ref": "ASM-000001/A",
                      "actor_name": "Rolf", "created_at": TS, "read_at": TS, "email_state": "none", "chat_state": "none" }],
                "watched": [{ "entity_type": "item", "entity_id": ME, "number": "PRT-000003", "name": "Bracket",
                              "state": "released", "link": "/revisions/1", "revision_code": "B" },
                            { "entity_type": "change", "entity_id": OTHER, "number": "CHG-000004", "name": "Wall",
                              "state": "in_review", "link": "/changes/1", "revision_code": null }],
                "email_enabled": true, "mattermost_enabled": true, "unread_notifications": 1,
            })),
        );
        for expected in [
            "Review requested: CHG-000004 &lt;x&gt;",
            "class=\"unread\"",
            "e-mailed",
            "Mattermost failed",
            &format!("/notifications/{ME}/open"),
            "Mark all as read",
            &format!("/items/{ME}/watch"),
            &format!("/changes/{OTHER}/watch"),
        ] {
            assert!(
                notes.contains(expected),
                "notifications page shows {expected}"
            );
        }

        let upload = render(
            &env,
            "imports/upload.html",
            with_common(json!({
                "target": null,
                "projects": [{ "id": ME, "code": "ROCKET", "name": "Rocket <stage>" }],
                "max_mib": 20,
            })),
        );
        assert!(
            upload.contains("enctype=\"multipart/form-data\"")
                && upload.contains("Rocket &lt;stage&gt;")
                && upload.contains("name=\"match_existing_sent\"")
                && upload.contains("name=\"project_id\""),
            "import upload form"
        );
        let into = render(
            &env,
            "imports/upload.html",
            with_common(json!({
                "target": { "id": ME, "item_number": "ASM-000001", "code": "A", "name": "Pump",
                            "project_code": "ROCKET" },
                "projects": [], "max_mib": 20,
            })),
        );
        assert!(
            into.contains(&format!("name=\"into\" value=\"{ME}\""))
                && !into.contains("name=\"project_id\""),
            "import into a checked-out revision"
        );
        let preview = render(
            &env,
            "imports/preview.html",
            with_common(json!({
                "id": OTHER,
                "plan": {
                    "filename": "pump <bom>.csv", "target": null, "root_label": "Pump",
                    "root_number": "ASM-PUMP", "root_name": "Pump", "project_code": "ROCKET",
                    "new_items": 2, "existing_items": 1, "lines": [{}, {}, {}],
                    "warnings": ["Row 4: <screw> appears again"],
                    "columns": [{ "header": "Part Number", "field": "number", "label": "Part number" }],
                },
                "rows": [
                    { "row": 2, "level": 1, "indent": "", "find_number": 1, "file_number": "HOUSING",
                      "number": "HOUSING", "existing": null, "type": "Assembly", "name": "Housing",
                      "quantity": 1.0, "unit": "each", "material": "AlMg3", "mass_kg": 0.8 },
                    { "row": 3, "level": 2, "indent": "indent-2", "find_number": 10,
                      "file_number": "ISO 4762 M6", "number": null, "existing": null, "type": "Part",
                      "name": "Screw", "quantity": 6.0, "unit": "each", "material": "", "mass_kg": null },
                    { "row": 5, "level": 1, "indent": "", "find_number": 2, "file_number": "PRT-000001",
                      "number": "PRT-000001", "existing": "PRT-000001/B", "type": "Part",
                      "name": "Bracket", "quantity": 2.0, "unit": "each", "material": "", "mass_kg": null },
                ],
            })),
        );
        for expected in [
            "pump &lt;bom&gt;.csv",
            "<span class=\"mono\">ASM-PUMP</span>",
            &format!("/import/{OTHER}/run"),
            &format!("/import/{OTHER}/discard"),
            "Row 4: &lt;screw&gt; appears again",
            "indent-2",
            "new assembly",
            "generated",
            "PRT-000001&#x2f;B",
            "badge-released\">existing",
            "<td>Part number</td>",
        ] {
            assert!(
                preview.contains(expected),
                "import preview shows {expected}"
            );
        }
        assert!(html.contains("<html lang=\"en\" data-skin=\"arch\">"));
        assert!(
            html.contains("data-skin=\"ubuntu\""),
            "skin picker lists every skin"
        );
        assert!(
            html.contains("skin-card selected"),
            "current skin is marked"
        );

        // the assistant: dashboard quick action, change briefing, answer page
        let dashboard = |enabled: bool| {
            let mut data = with_common(json!({
                "stats": { "items": 0, "revisions_in_work": 0, "revisions_in_review": 0, "revisions_released": 0,
                           "open_changes": 0, "files": 0, "stored_bytes": 0 },
                "checkouts": [], "reviews": [], "my_changes": [], "activity": [], "impacts": [],
            }));
            data["assistant_enabled"] = json!(enabled);
            render(&env, "dashboard.html", data)
        };
        let html = dashboard(true);
        assert!(html.contains("What needs my attention") && html.contains("action=\"/assistant\""));
        assert!(
            html.contains("Allow drafting change orders"),
            "editors may allow drafts"
        );
        assert!(
            !dashboard(false).contains("/assistant"),
            "hidden when disabled"
        );

        let mut data = with_common(json!({
            "change": change("in_review"), "status_label": "In review", "items": [], "reviews": [],
            "candidates": [], "history": [], "priorities": ["low"], "impact": [],
            "can": { "modify": false, "edit_text": false, "submit": false, "withdraw": false, "reopen": false,
                     "cancel": false, "review": true },
        }));
        data["assistant_enabled"] = json!(true);
        let html = render(&env, "changes/show.html", data);
        assert!(
            html.contains("Brief me for this review")
                && html.contains("name=\"change\" value=\"CHG-000001\"")
        );

        let answer = crate::assistant::render_markdown("## Scope\n- PRT-000001 <b>");
        let html = render(
            &env,
            "assistant.html",
            with_common(json!({
                "actions": [["review_brief", "Brief me for this review"]],
                "form": { "question": "Is it <ready>?", "item": "", "revision": "", "change": "CHG-000001" },
                "back": "/changes/x", "error": null,
                "run": { "label": "Brief me for this review", "html": answer, "text": "x", "truncated": true,
                         "tools": [{ "name": "get_change", "ok": true }, { "name": "bom_health", "ok": false }],
                         "steps": 3, "model": "claude-opus-5-5", "allow_drafts": false },
            })),
        );
        for expected in [
            "<h3>Scope</h3>",
            "<a href=\"/i/PRT-000001\">PRT-000001</a> &lt;b&gt;",
            "Is it &lt;ready&gt;?",
            "get_change",
            "badge badge-rejected",
            "3 steps",
            "read-only",
            "cut off",
            "changes&#x2f;x\" data-back>Back</a>",
        ] {
            assert!(html.contains(expected), "assistant page shows {expected}");
        }
        let html = render(
            &env,
            "assistant.html",
            with_common(json!({
                "actions": [["attention", "What needs my attention"]],
                "form": { "question": "", "item": "", "revision": "", "change": "" },
                "back": "/", "error": "Type a question first.", "run": null,
            })),
        );
        assert!(
            html.contains("Type a question first.") && html.contains("What needs my attention")
        );
    }

    fn units() -> Value {
        json!(crate::domain::COMMON_UNITS)
    }

    /// Every link to an embedded asset carries the asset version, so browsers
    /// never keep an outdated stylesheet, script or logo after an update.
    #[test]
    fn static_links_carry_the_asset_version() {
        for (name, source) in TEMPLATES {
            for (i, _) in source.match_indices("\"/static/") {
                let link = &source[i + 1..];
                let link = &link[..link.find('"').unwrap_or(link.len())];
                assert!(
                    link.contains("?v={{ assets }}"),
                    "{name}: {link} has no ?v={{{{ assets }}}}"
                );
            }
        }
    }

    #[test]
    fn filters() {
        assert_eq!(
            filter_dt(Some("2026-11-03T14:05:59.123Z".into())),
            "2026-11-03 14:05 UTC"
        );
        assert_eq!(filter_dt(None), "—");
        assert_eq!(filter_num(Some(1.23456), None), "1.235");
        assert_eq!(filter_num(Some(2.0), Some(2)), "2");
        assert_eq!(filter_num(None, None), "—");
        assert_eq!(filter_label(Some("in_work".into())), "WIP");
        assert_eq!(filter_label(Some("in_review".into())), "In Review");
        assert_eq!(filter_label(Some("released".into())), "Released");
    }
}
