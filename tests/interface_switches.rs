//! Interface guard for the need-to-know interface switches (issue #151,
//! step 3), read from the sources (no database): every interface that can
//! carry data out of a restricted group has a switch, and the code of that
//! interface applies it. A new outgoing interface without a policy check, or
//! a switch nobody enforces, fails here.
//!
//! The behaviour itself (a switched-off interface treats the group as
//! non-existent) is tested in `tests/access_log.rs` against a database.

use std::{
    fs,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let mut paths = Vec::new();
    rust_files(&root().join("src"), &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&p).unwrap();
            (rel, text)
        })
        .collect()
}

/// The interface names, read from `Interface::ALL` in the source.
fn interfaces() -> Vec<(String, String)> {
    let text = read("src/services/project_policy.rs");
    let all = text
        .split("pub const ALL: [Interface;")
        .nth(1)
        .and_then(|t| t.split("];").next())
        .expect("Interface::ALL");
    let variants: Vec<String> = all
        .split("Interface::")
        .skip(1)
        .map(|v| {
            v.chars()
                .take_while(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .collect();
    assert!(variants.len() >= 5, "{variants:?}");
    variants
        .into_iter()
        .map(|v| {
            // `Interface::X => "name"` in as_str
            let marker = format!("Interface::{v} => \"");
            let name = text
                .split(&marker)
                .nth(1)
                .and_then(|t| t.split('"').next())
                .unwrap_or_else(|| panic!("no name for {v}"))
                .to_string();
            (v, name)
        })
        .collect()
}

#[test]
fn every_interface_has_a_column_defaulting_to_allowed() {
    let migrations: String = {
        let mut names: Vec<_> = fs::read_dir(root().join("migrations"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        names
            .iter()
            .map(|p| fs::read_to_string(p).unwrap())
            .collect()
    };
    for (_, name) in interfaces() {
        assert!(
            migrations.contains(&format!("ADD COLUMN {name} BOOLEAN NOT NULL DEFAULT TRUE")),
            "interface '{name}' has no project_policies column that defaults to allowed"
        );
    }
}

#[test]
fn every_interface_is_enforced_somewhere_outside_its_definition() {
    let sources = sources();
    for (variant, name) in interfaces() {
        // webhooks are enforced in SQL by column name, the others by variant
        let used = sources.iter().any(|(rel, text)| {
            rel != "src/services/project_policy.rs"
                && (text.contains(&format!("Interface::{variant}"))
                    || text.contains(&format!("NOT wp.{name}")))
        });
        assert!(used, "interface {variant} is never applied by any code");
    }
}

#[test]
fn the_personal_access_token_extractor_applies_the_api_switch() {
    let users = read("src/services/users.rs");
    let body = users
        .split("pub async fn actor_for_api_token")
        .nth(1)
        .and_then(|t| t.split("\n}\n").next())
        .expect("actor_for_api_token");
    assert!(body.contains("Interface::ApiTokens"), "{body}");
}

#[test]
fn mcp_and_the_assistant_apply_their_switches() {
    assert!(read("src/mcp/mod.rs").contains("Interface::Mcp"));
    assert!(read("src/services/assistant.rs").contains("Interface::Assistant"));
    // the tool loop reloads the user for every call: it narrows there too
    let assistant = read("src/assistant.rs");
    assert!(assistant.contains("assistant::switches("));
}

#[test]
fn every_webhook_outbox_insert_applies_the_switch() {
    for (rel, text) in sources() {
        for statement in text.split("INSERT INTO webhook_deliveries").skip(1) {
            let statement = statement.split("\",").next().unwrap_or("");
            // the manual test delivery (`ping`) is no event of a group
            if statement.contains("'ping'") {
                continue;
            }
            assert!(
                statement.contains("NOT wp.webhooks"),
                "{rel}: an outbox insert without the webhooks switch:\n{statement}"
            );
        }
    }
}

#[test]
fn every_package_and_bom_download_applies_the_packages_switch() {
    for (rel, text) in sources() {
        if rel.starts_with("src/services/bom.rs") || rel.contains("tests") {
            continue;
        }
        // every function that produces a download applies the switch itself
        // (package::build narrows the actor on its own)
        for body in text.split("async fn ").skip(1) {
            let body = body.split("\n}\n").next().unwrap_or(body);
            let produces = body.contains("bom::to_csv(")
                || body.contains("bom::to_xlsx(")
                || body.contains("bom_compare::to_xlsx(");
            if produces {
                let name = body.split('(').next().unwrap_or("?");
                assert!(
                    body.contains("package_actor(")
                        || body.contains("comparison(&st, &ctx.me, id, &q, true)")
                        || body.contains("compare_at(&st, &me, &number, &code, &q, true)"),
                    "{rel}: {name} builds a BOM download without the packages switch"
                );
            }
        }
    }
}

#[test]
fn file_downloads_write_the_read_log() {
    let text = read("src/web/revisions.rs");
    for name in ["file_response", "version_response"] {
        let body = text
            .split(&format!("async fn {name}("))
            .nth(1)
            .and_then(|t| t.split("\n}\n").next())
            .unwrap_or_else(|| panic!("{name}"));
        assert!(
            body.contains("log_file("),
            "{name} does not log the download"
        );
    }
}

#[test]
fn the_assistant_runs_under_the_external_llm_switch_too() {
    // the run and the per-tool-call reload use the same switch list
    let service = read("src/services/assistant.rs");
    assert!(service.contains("Interface::ExternalLlm"));
    assert!(service.contains("switches(st.config.assistant.as_ref())"));
    assert!(read("src/assistant.rs").contains("assistant::switches("));
}

#[test]
fn the_desktop_get_applies_the_desktop_switch() {
    let text = read("src/api/bom_api.rs");
    let body = text
        .split("async fn get_workset")
        .nth(1)
        .and_then(|t| t.split("\n}\n").next())
        .expect("get_workset");
    assert!(body.contains("Interface::DesktopSync"), "{body}");
    // every other caller of the working set must narrow as well
    for (rel, text) in sources() {
        if rel == "src/services/workset.rs" || !text.contains("workset::resolve(") {
            continue;
        }
        assert!(
            text.contains("Interface::DesktopSync"),
            "{rel} resolves a working set without the desktop switch"
        );
    }
}

#[test]
fn notification_delivery_applies_the_content_switch() {
    let delivery = read("src/delivery.rs");
    assert!(delivery.contains("generic_text_sql("));
    assert!(delivery.contains("row.generic"));
    // e-mail and chat are sent from delivery.rs only: nobody else may send
    // notification text around the switch
    for (rel, text) in sources() {
        if rel == "src/delivery.rs" {
            continue;
        }
        assert!(
            !text.contains("MailSender") && !text.contains("ChatSender"),
            "{rel} sends mail or chat outside src/delivery.rs"
        );
    }
}

/// A new interface must be listed here with the place that enforces it; one
/// nobody listed, or whose place lost its check, fails.
#[test]
fn every_interface_has_a_named_enforcement_point() {
    let points: &[(&str, &str)] = &[
        ("api_tokens", "src/services/users.rs"),
        ("mcp", "src/mcp/mod.rs"),
        ("assistant", "src/services/assistant.rs"),
        ("webhooks", "src/services/webhooks.rs"),
        ("packages", "src/services/package.rs"),
        ("notification_content", "src/delivery.rs"),
        ("external_llm", "src/services/assistant.rs"),
        ("desktop_sync", "src/api/bom_api.rs"),
    ];
    for (variant, name) in interfaces() {
        let (_, file) = points
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("interface '{name}' has no entry in the enforcement table"));
        let text = read(file);
        assert!(
            text.contains(&format!("Interface::{variant}"))
                || text.contains(&format!("NOT wp.{name}"))
                || (name == "packages" && text.contains("package_actor("))
                || text.contains(&format!("{name}_sql("))
                || (name == "notification_content" && text.contains("generic_text_sql(")),
            "{file} does not apply the '{name}' switch"
        );
    }
    assert_eq!(points.len(), interfaces().len(), "stale enforcement table");
}
