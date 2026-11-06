//! The MCP contract as an assistant sees it: tool names, titles,
//! descriptions, argument schemas and hints (`tools/list`), the prompts
//! (`prompts/list`) and the text of every prompt (`prompts/get`), compared
//! with the snapshot in `tests/fixtures/mcp/contract.json`.
//!
//! The snapshot guards against unintended changes, e.g. when the tool
//! modules are moved or split. After an intended change, rewrite it with
//! `TEAMCRAFT_UPDATE_SNAPSHOTS=1 cargo test --test mcp_contract` and review
//! the diff. No database needed.

use std::path::PathBuf;

use serde_json::{json, Value};
use teamcraft::mcp::tools;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp/contract.json")
}

/// Every prompt with all arguments, with only the required ones and with
/// none (the error message), plus an unknown prompt.
fn prompt_texts() -> Value {
    let full = json!({ "item": "ASM-000003", "revision": "B", "change": "ECO-000004" });
    let mut out = serde_json::Map::new();
    for p in tools::prompts() {
        let name = p["name"].as_str().expect("prompt name").to_string();
        let required: serde_json::Map<String, Value> = p["arguments"]
            .as_array()
            .expect("prompt arguments")
            .iter()
            .filter(|a| a["required"] == true)
            .map(|a| {
                let key = a["name"].as_str().expect("argument name").to_string();
                let value = full[&key].clone();
                (key, value)
            })
            .collect();
        let outcome = |args: &Value| match tools::prompt(&name, args) {
            Ok(v) => json!({ "ok": v }),
            Err(e) => json!({ "err": e }),
        };
        out.insert(
            name.clone(),
            json!({
                "all_arguments": outcome(&full),
                "required_arguments": outcome(&Value::Object(required)),
                "no_arguments": outcome(&json!({})),
            }),
        );
    }
    out.insert(
        "(unknown)".into(),
        match tools::prompt("no_such_prompt", &json!({})) {
            Ok(v) => json!({ "ok": v }),
            Err(e) => json!({ "err": e }),
        },
    );
    Value::Object(out)
}

fn contract() -> Value {
    json!({
        "tools": tools::definitions(),
        "prompts": tools::prompts(),
        "prompt_texts": prompt_texts(),
        "failure": tools::failure("A user error."),
        "success": tools::success(json!({ "item": "PRT-000001" })),
        "success_non_object": tools::success(json!(["a", "b"])),
    })
}

#[test]
fn the_mcp_contract_matches_the_snapshot() {
    let now = contract();
    let path = snapshot_path();
    if std::env::var_os("TEAMCRAFT_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&now).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (write it with TEAMCRAFT_UPDATE_SNAPSHOTS=1)",
            path.display()
        )
    }))
    .expect("snapshot is JSON");

    // Name the first difference instead of dumping both documents.
    for key in ["tools", "prompts"] {
        let by_name = |v: &Value| -> Vec<(String, Value)> {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| (t["name"].as_str().unwrap().to_string(), t.clone()))
                .collect()
        };
        let (old, new) = (by_name(&saved), by_name(&now));
        let names = |l: &[(String, Value)]| l.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&old), names(&new), "{key}: names or order changed");
        for ((name, o), (_, n)) in old.iter().zip(&new) {
            assert_eq!(o, n, "{key}: `{name}` changed");
        }
    }
    for (name, o) in saved["prompt_texts"].as_object().unwrap() {
        assert_eq!(
            o, &now["prompt_texts"][name],
            "prompt text of `{name}` changed"
        );
    }
    assert_eq!(saved, now, "the MCP contract changed");
}

#[test]
fn every_tool_is_registered_once() {
    let defs = tools::definitions();
    let mut names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(names.iter().all(|n| tools::exists(n)));
    assert!(!tools::exists("no_such_tool"));
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "a tool name is registered twice");
}
