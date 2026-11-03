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
    "items/list.html",
    "items/new.html",
    "revisions/show.html",
    "revisions/edit.html",
    "revisions/bom.html",
    "changes/list.html",
    "changes/new.html",
    "changes/show.html",
    "projects/list.html",
    "projects/show.html",
    "admin/users.html",
    "admin/user_edit.html",
    "admin/audit.html",
    "account.html",
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
    env.add_global("instance_name", instance_name.to_string());
    env.add_global("version", env!("CARGO_PKG_VERSION"));
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
            "unit": "each", "project_id": null, "project_code": "NIXUS", "created_by": ME,
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

    fn audit_entry() -> Value {
        json!({ "id": 1, "at": TS, "user_id": ME, "username": "max", "action": "item.create",
                "entity_type": "item", "entity_id": "44444444-4444-4444-8444-444444444444",
                "summary": "Created Part PRT-000001", "details": {} })
    }

    fn user() -> Value {
        json!({ "id": OTHER, "username": "erika", "display_name": "Erika", "email": null,
                "auth_source": "local", "role": "editor", "active": true, "created_at": TS, "last_login_at": null,
                "skin": "warr" })
    }

    const NIXUS: &str = "66666666-6666-4666-8666-666666666666";

    fn project() -> Value {
        json!({ "id": NIXUS, "code": "NIXUS", "name": "WARR Rocketry Project NIXUS",
                "description": "Rocketry", "active": true, "created_at": TS,
                "oidc_group": "teamcraft-nixus", "member_count": 3 })
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
        let file = json!({ "id": OTHER, "revision_id": ME, "filename": "drawing.pdf", "content_type": "application/pdf",
                           "size_bytes": 123456, "sha256": "ab".repeat(32), "uploaded_by": ME,
                           "uploaded_by_name": "Max", "uploaded_at": TS, "version": 2,
                           "versions": [version(2, "wall 2.5 mm"), version(1, "")] });
        let can_all = json!({ "edit": true, "checkout": true, "checkin": true, "cancel_checkout": true,
                              "revise": true, "obsolete": true, "delete": true, "master": true,
                              "add_to_change": true });

        let html = render(
            &env,
            "dashboard.html",
            with_common(json!({
                "stats": { "items": 3, "revisions_in_work": 1, "revisions_in_review": 0, "revisions_released": 2,
                           "open_changes": 1, "files": 4, "stored_bytes": 2048 },
                "checkouts": [revision("in_work", true)], "reviews": [change("in_review")],
                "my_changes": [change("draft")], "activity": [audit_entry()],
            })),
        );
        assert!(html.contains("Saved &lt;ok&gt;"), "flash is escaped");
        assert!(html.contains("2 KiB"));

        render(
            &env,
            "login.html",
            json!({ "next": "/", "next_encoded": "%2F", "error": "bad", "oidc_name": "WARR SSO",
                                           "username": "max", "local_login": true, "flash": null }),
        );
        render(
            &env,
            "error.html",
            json!({ "status": 404, "reason": "Not Found", "message": "gone", "flash": null, "local_login": true }),
        );

        render(
            &env,
            "items/list.html",
            with_common(json!({
                "items": [{ "id": ME, "number": "PRT-000001", "item_type": "part", "unit": "each", "project_code": null,
                            "created_at": TS, "latest_revision_id": OTHER, "latest_code": "A", "latest_state": "in_work",
                            "name": "Bracket" }],
                "pagination": page(), "base_query": "q=a%20b",
                "filter": { "q": "a b", "type": "part", "project": "", "state": "" },
                "projects": [project()], "types": [{ "value": "part", "label": "Part" }],
                "states": [{ "value": "in_work", "label": "In Work" }],
            })),
        );
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
                    "is_latest": false, "latest_id": OTHER, "files": [file.clone()], "lines": [bom_line.clone()],
                    "where_used": [{ "line_id": ME, "parent_revision_id": OTHER, "parent_item_id": ME,
                                     "parent_number": "ASM-000001", "parent_code": "A", "parent_name": "Assembly",
                                     "parent_state": "released", "find_number": 10, "quantity": 2.0 }],
                    "changes": [change("draft")], "open_change": change("draft"), "drafts": [change("draft")],
                    "history": [audit_entry()], "projects": [project()], "units": units(),
                    "can": can_all.clone(), "max_upload_mb": 512,
                })),
            );
            assert!(html.contains("Bracket &lt;b&gt;"), "names are escaped");
            assert!(
                html.contains("/files/") && html.contains("/restore"),
                "rollback offered"
            );
            assert!(html.contains("Check in and release"), "check-in form");
        }
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
        render(
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
            render(
                &env,
                "changes/show.html",
                with_common(json!({
                    "change": change(status), "status_label": "Draft",
                    "items": [{ "revision_id": OTHER, "item_id": ME, "item_number": "PRT-000001", "code": "A",
                                "name": "Bracket", "state": "in_work", "checked_out_by_name": "Erika" }],
                    "reviews": [{ "reviewer_id": OTHER, "reviewer_username": "erika", "reviewer_name": "Erika",
                                  "decision": "approved", "comment": "ok", "decided_at": TS }],
                    "candidates": [user()], "history": [audit_entry()], "priorities": ["low", "normal"],
                    "can": { "modify": true, "edit_text": true, "submit": true, "withdraw": true, "reopen": true,
                             "cancel": true, "review": true },
                })),
            );
        }

        render(
            &env,
            "projects/list.html",
            with_common(json!({ "projects": [project()], "my_projects": [NIXUS] })),
        );
        let html = render(
            &env,
            "projects/show.html",
            with_common(json!({
                "project": project(),
                "members": [{ "user_id": OTHER, "username": "erika", "display_name": "Erika", "role": "editor",
                              "active": true, "source": "oidc", "added_at": TS }],
                "candidates": [user()], "is_member": true, "can_edit": true, "oidc_enabled": true,
            })),
        );
        assert!(html.contains("CAD editor"));
        assert!(html.contains("via IdP group"));
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
        let html = render(
            &env,
            "account.html",
            with_common(json!({
                "user": user(), "tokens": [], "new_token": { "name": "ci", "value": "tc_pat_secret" },
                "api_base": "http://localhost:8080/api/v1",
                "skins": crate::domain::SKINS,
            })),
        );
        assert!(html.contains("tc_pat_secret"));
        assert!(html.contains("<html lang=\"en\" data-skin=\"arch\">"));
        assert!(
            html.contains("data-skin=\"ubuntu\""),
            "skin picker lists every skin"
        );
        assert!(
            html.contains("skin-card selected"),
            "current skin is marked"
        );
    }

    fn units() -> Value {
        json!(crate::domain::COMMON_UNITS)
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
