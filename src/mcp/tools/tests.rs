//! Unit tests of the MCP tools (no database).

use super::prompts::PROMPTS;
use super::*;

#[test]
fn the_agent_tools_are_read_only_and_described() {
    let defs = definitions();
    for name in ["read_file_text", "impact_summary"] {
        let d = defs
            .iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        assert_eq!(d["annotations"]["readOnlyHint"], true, "{name}");
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        assert!(d["description"].as_str().unwrap().len() > 80, "{name}");
        assert!(exists(name));
    }
    let schema =
        |name: &str| defs.iter().find(|d| d["name"] == name).unwrap()["inputSchema"].clone();
    let s = schema("read_file_text");
    assert_eq!(s["required"], json!(["item", "file"]));
    assert_eq!(
        s["properties"]["limit"]["maximum"],
        files::MAX_TEXT_PAGE_CHARS
    );
    assert_eq!(
        schema("impact_summary")["properties"]["levels"]["maximum"],
        MAX_IMPACT_LEVELS
    );
}

#[test]
fn counts_reject_negative_and_huge_values() {
    let args = json!({ "a": 5, "b": "7", "c": -1, "d": 1e12, "e": "x", "f": 2.5 });
    let a = Args::new(&args);
    assert_eq!(a.count("a", 0, 10).unwrap(), 5);
    assert_eq!(a.count("b", 0, 10).unwrap(), 7, "numeric strings");
    assert_eq!(a.count("missing", 3, 10).unwrap(), 3);
    for key in ["c", "d", "e", "f"] {
        assert!(a.count(key, 0, 10).is_err(), "{key}");
    }
    assert!(a.count("a", 0, 4).is_err(), "above the maximum");
}

fn file(name: &str, sha: &str, version: i32) -> FileState {
    FileState {
        filename: name.into(),
        sha256: sha.into(),
        version,
        size_bytes: 10,
    }
}

fn line(child: &str, find: i32, qty: f64) -> LineState {
    LineState {
        child: child.into(),
        name: format!("{child} name"),
        find_number: find,
        quantity: qty,
    }
}

fn snap(label: &str, mass: Option<f64>, files: Vec<FileState>, bom: Vec<LineState>) -> Snapshot {
    Snapshot {
        label: label.into(),
        attributes: vec![("name", json!("Bracket")), ("mass_kg", json!(mass))],
        files,
        bom,
    }
}

#[test]
fn compare_finds_attribute_file_and_bom_changes() {
    let a = snap(
        "ASM-1/A",
        Some(1.0),
        vec![
            file("top.SLDASM", "aa", 3),
            file("old.pdf", "bb", 1),
            file("same.step", "cc", 1),
        ],
        vec![
            line("PRT-1", 10, 2.0),
            line("PRT-2", 20, 1.0),
            line("PRT-3", 30, 4.0),
        ],
    );
    let b = snap(
        "ASM-1/B",
        Some(1.5),
        vec![
            file("Top.sldasm", "dd", 4),
            file("same.step", "cc", 1),
            file("new.dxf", "ee", 1),
        ],
        vec![
            line("PRT-1", 10, 1.0),
            line("PRT-1", 11, 2.0),
            line("PRT-3", 30, 4.0),
            line("PRT-4", 40, 1.0),
        ],
    );
    let c = compare(&a, &b);
    assert_eq!(c["identical"], false);
    assert_eq!(
        c["attributes"],
        json!([{ "field": "mass_kg", "from": 1.0, "to": 1.5 }])
    );
    assert_eq!(c["files"]["added"][0]["filename"], "new.dxf");
    assert_eq!(c["files"]["removed"][0]["filename"], "old.pdf");
    // matched case-insensitively, compared by content
    assert_eq!(c["files"]["new_content"][0]["from_version"], 3);
    assert_eq!(c["files"]["new_content"][0]["to_version"], 4);
    assert_eq!(c["files"]["unchanged"], 1);
    assert_eq!(c["bom"]["added"][0]["item"], "PRT-4");
    assert_eq!(c["bom"]["removed"][0]["item"], "PRT-2");
    // PRT-1: two lines summed up, a new find number
    let changed = c["bom"]["changed"].as_array().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["item"], "PRT-1");
    assert_eq!(changed[0]["from_quantity"], 2.0);
    assert_eq!(changed[0]["to_quantity"], 3.0);
    assert_eq!(changed[0]["to_find_numbers"], json!([10, 11]));
}

#[test]
fn compare_of_equal_snapshots_is_identical() {
    let a = snap(
        "P/A",
        None,
        vec![file("a.prt", "x", 1)],
        vec![line("C", 1, 1.0)],
    );
    let b = snap(
        "P/B",
        None,
        vec![file("a.prt", "x", 1)],
        vec![line("C", 1, 1.0)],
    );
    let c = compare(&a, &b);
    assert_eq!(c["identical"], true);
    assert_eq!(c["files"]["unchanged"], 1);
}

#[test]
fn item_references_split_number_and_revision() {
    assert_eq!(parse_item_ref("PRT-000012"), ("PRT-000012".into(), None));
    assert_eq!(
        parse_item_ref(" PRT-000012/B "),
        ("PRT-000012".into(), Some("B".into()))
    );
    assert_eq!(parse_item_ref("PRT-000012/"), ("PRT-000012".into(), None));
}

#[test]
fn arguments_are_typed_and_bounded() {
    let v = json!({
        "q": "  bolt ", "empty": " ", "n": 500, "s": "7", "f": 2.0, "bad": "x",
        "yes": "true", "list": ["A", " ", "B"], "one": "C", "null": null,
    });
    let a = Args::new(&v);
    assert_eq!(a.opt_str("q").unwrap().as_deref(), Some("bolt"));
    assert_eq!(a.opt_str("empty").unwrap(), None);
    assert_eq!(a.opt_str("null").unwrap(), None);
    assert!(a.opt_str("n").is_err());
    assert!(a.req_str("missing").is_err());
    assert_eq!(a.limit("n", 20, 100).unwrap(), 100);
    assert_eq!(a.limit("s", 20, 100).unwrap(), 7);
    assert_eq!(a.limit("f", 20, 100).unwrap(), 2);
    assert_eq!(a.limit("missing", 20, 100).unwrap(), 20);
    assert!(a.limit("bad", 20, 100).is_err());
    assert!(a.flag("yes", false).unwrap());
    assert!(a.flag("missing", true).unwrap());
    assert!(a.flag("q", true).is_err());
    assert_eq!(a.list("list", 5).unwrap(), vec!["A", "B"]);
    assert_eq!(a.list("one", 5).unwrap(), vec!["C"]);
    assert!(a.list("list", 1).is_err());
    assert!(a.list("n", 5).is_err());
    // a non-object counts as no arguments
    let none = json!("nope");
    assert_eq!(Args::new(&none).limit("limit", 3, 9).unwrap(), 3);
}

#[test]
fn results_are_wrapped_and_cut() {
    let ok = success(json!({ "a": 1 }));
    assert_eq!(ok["isError"], false);
    assert_eq!(ok["structuredContent"]["a"], 1);
    assert_eq!(ok["content"][0]["text"], "{\"a\":1}");
    let list = success(json!([1, 2]));
    assert_eq!(list["structuredContent"]["result"], json!([1, 2]));

    let big = success(json!({ "s": "ä".repeat(MAX_TEXT_BYTES) }));
    let text = big["content"][0]["text"].as_str().unwrap();
    assert!(text.len() < MAX_TEXT_BYTES + 200);
    assert!(text.contains("output cut"));
    assert!(big.get("structuredContent").is_none());

    let err = failure("nope");
    assert_eq!(err["isError"], true);
    assert_eq!(err["content"][0]["text"], "nope");
    assert_eq!(cut("aä", 2), "a");
    let (v, t) = capped(vec![1, 2, 3], 2);
    assert_eq!((v, t), (vec![1, 2], true));
}

#[test]
fn internal_errors_stay_hidden() {
    let r = from_error("x", AppError::Other(anyhow::anyhow!("secret detail")));
    assert_eq!(r["isError"], true);
    assert!(!r["content"][0]["text"].as_str().unwrap().contains("secret"));
    let r = from_error("x", AppError::Conflict("checked out by Erika".into()));
    assert_eq!(r["content"][0]["text"], "checked out by Erika");
}

#[test]
fn tool_definitions_are_complete() {
    let defs = definitions();
    assert_eq!(defs.len(), TOOLS.len());
    let mut names = HashSet::new();
    for d in &defs {
        assert!(names.insert(d["name"].as_str().unwrap().to_string()));
        assert_eq!(d["inputSchema"]["type"], "object");
        assert_eq!(d["annotations"]["destructiveHint"], false);
        for req in d["inputSchema"]["required"].as_array().unwrap() {
            let key = req.as_str().unwrap();
            assert!(d["inputSchema"]["properties"].get(key).is_some(), "{key}");
        }
    }
    for name in ["submit_change", "approve_change", "release", "obsolete"] {
        assert!(!exists(name));
    }
    let writes: Vec<&str> = defs
        .iter()
        .filter(|d| d["annotations"]["readOnlyHint"] == false)
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        writes,
        vec![
            "create_change_draft",
            "add_to_change",
            "create_change_request",
            "propose_result_values",
            "comment_change_request"
        ]
    );
    // change requests are raised and discussed, never decided
    for name in [
        "decide_change_request",
        "accept_change_request",
        "triage_change_request",
        "reject_change_request",
    ] {
        assert!(!exists(name));
    }
    let def = |name: &str| defs.iter().find(|d| d["name"] == name).unwrap().clone();
    // the shape sibling of find_duplicate_files: read-only, by item
    let similar = def("find_similar_parts");
    assert_eq!(similar["annotations"]["readOnlyHint"], true);
    assert_eq!(similar["inputSchema"]["required"], json!(["item"]));
    assert!(similar["description"]
        .as_str()
        .unwrap()
        .contains("create_change_draft"));
    // reviewers are suggested, never assigned
    let suggest = def("suggest_reviewers");
    assert_eq!(suggest["annotations"]["readOnlyHint"], true);
    assert!(suggest["description"]
        .as_str()
        .unwrap()
        .contains("Never assigns"));
    assert!(!exists("add_reviewer") && !exists("assign_reviewers"));
    let create = def("create_change_request");
    assert_eq!(create["annotations"]["idempotentHint"], false);
    assert_eq!(
        create["inputSchema"]["properties"]["items"]["maxItems"],
        requests::MAX_ITEMS
    );
    assert_eq!(
        create["inputSchema"]["properties"]["reason"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        RequestReason::ALL.len()
    );
    let list = def("list_change_requests");
    assert_eq!(
        list["inputSchema"]["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        RequestStatus::ALL.len() + 2
    );
    assert_eq!(
        def("list_simulations")["inputSchema"]["properties"]["limit"]["maximum"],
        MAX_SIMULATIONS
    );
    for read in [
        "get_change_request",
        "get_simulation",
        "compare_simulation_results",
    ] {
        assert_eq!(def(read)["annotations"]["readOnlyHint"], true, "{read}");
    }
}

#[test]
fn request_arguments_are_checked() {
    assert_eq!(request_status_filter(None).unwrap(), (None, false));
    assert_eq!(request_status_filter(Some("all")).unwrap(), (None, false));
    assert_eq!(request_status_filter(Some(" open ")).unwrap(), (None, true));
    assert_eq!(
        request_status_filter(Some("in_triage")).unwrap(),
        (Some(RequestStatus::InTriage), false)
    );
    assert!(request_status_filter(Some("decided")).is_err());
    // every listed filter parses
    for s in REQUEST_STATUS_FILTERS {
        assert!(request_status_filter(Some(s)).is_ok(), "{s}");
    }

    let me = uuid::Uuid::new_v4();
    assert_eq!(request_scope(None, me).unwrap(), (None, None));
    assert_eq!(request_scope(Some("mine"), me).unwrap(), (Some(me), None));
    assert_eq!(
        request_scope(Some("assigned"), me).unwrap(),
        (None, Some(me))
    );
    assert!(request_scope(Some("everyone"), me).is_err());

    assert_eq!(request_reason(None).unwrap(), RequestReason::Other);
    assert_eq!(request_reason(Some(" ")).unwrap(), RequestReason::Other);
    assert_eq!(
        request_reason(Some("Defect")).unwrap(),
        RequestReason::Defect
    );
    assert!(request_reason(Some("boredom")).is_err());
    for r in REQUEST_REASONS {
        assert!(request_reason(Some(r)).is_ok(), "{r}");
    }

    // the newest comments are kept, in their order
    assert_eq!(newest(vec![1, 2, 3, 4], 2), (vec![3, 4], true));
    assert_eq!(newest(vec![1, 2], 5), (vec![1, 2], false));

    // the item list of a new request is bounded like the service
    let many: Vec<String> = (0..=requests::MAX_ITEMS)
        .map(|i| format!("PRT-{i}"))
        .collect();
    let v = json!({ "items": many });
    assert!(Args::new(&v)
        .list("items", requests::MAX_ITEMS as usize)
        .is_err());
}

fn sim_result(name: &str, value: f64, upper: Option<f64>) -> simulations::SimulationResult {
    simulations::SimulationResult {
        id: uuid::Uuid::new_v4(),
        revision_id: uuid::Uuid::nil(),
        position: 0,
        name: name.into(),
        value,
        unit: "MPa".into(),
        lower_limit: None,
        upper_limit: upper,
        load_case: "LC1".into(),
        solver: String::new(),
        solver_version: String::new(),
        note: String::new(),
        source: String::new(),
        updated_at: chrono::Utc::now(),
        status: Some(crate::domain::result_status(value, None, upper)),
    }
}

#[test]
fn result_diffs_are_compact() {
    let before = [sim_result("Stress", 200.0, Some(250.0))];
    let after = [
        sim_result("Stress", 260.0, Some(250.0)),
        sim_result("Mass", 1.0, None),
    ];
    let diffs = simulations::compare_results(&before, &after);
    let views: Vec<Value> = diffs.iter().map(diff_brief).collect();
    let stress = views.iter().find(|v| v["name"] == "Stress").unwrap();
    assert_eq!(stress["change"], "changed");
    assert_eq!(stress["before"]["status"], "pass");
    assert_eq!(stress["after"]["status"], "fail");
    assert_eq!(stress["delta"], 60.0);
    assert_eq!(stress["delta_percent"], 30.0);
    let mass = views.iter().find(|v| v["name"] == "Mass").unwrap();
    assert_eq!(mass["change"], "added");
    assert_eq!(mass["before"], Value::Null);
    assert_eq!(mass["after"]["status"], "none");
}

fn based(sim: &str, basis: &str, out_of_date: Option<&str>) -> simulations::BasedSimulation {
    simulations::BasedSimulation {
        simulation_item_id: uuid::Uuid::nil(),
        simulation_number: sim.into(),
        simulation_code: "A".into(),
        simulation_state: "released".into(),
        simulation_name: format!("{sim} static"),
        result_count: 3,
        failed_count: 1,
        open_impact_id: out_of_date.map(|_| uuid::Uuid::nil()),
        basis: simulations::SimulationBasis {
            id: uuid::Uuid::new_v4(),
            revision_id: uuid::Uuid::nil(),
            role: "geometry".into(),
            note: String::new(),
            basis_item_id: uuid::Uuid::nil(),
            basis_number: basis.into(),
            basis_type: "part".into(),
            basis_revision_id: uuid::Uuid::nil(),
            basis_code: "A".into(),
            basis_seq: 1,
            basis_state: "released".into(),
            basis_name: "Bracket".into(),
            file_version_id: None,
            file_id: None,
            file_name: String::new(),
            file_version: None,
            file_current_version: None,
            released_revision_id: None,
            released_code: Some("A".into()),
            released_seq: Some(1),
            created_at: chrono::Utc::now(),
            out_of_date: out_of_date.map(str::to_string),
            basis_project_id: None,
            restricted: false,
        },
    }
}

#[test]
fn based_simulations_are_grouped() {
    let g = group_based(&[
        based("SIM-1", "PRT-1", None),
        based(
            "SIM-1",
            "PRT-2",
            Some("PRT-2/B was released (simulated: A)"),
        ),
        based("SIM-2", "PRT-1", None),
    ]);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["simulation"], "SIM-1");
    assert_eq!(g[0]["bases"].as_array().unwrap().len(), 2);
    assert_eq!(g[0]["out_of_date"], true);
    assert_eq!(g[0]["check_open"], true);
    assert_eq!(g[0]["bases"][0]["file"], Value::Null);
    assert_eq!(g[1]["out_of_date"], false);
    assert_eq!(g[1]["failed_count"], 1);
}

#[test]
fn prompts_check_their_arguments() {
    assert_eq!(prompts().len(), PROMPTS.len());
    assert!(prompt("review_change", &json!({})).is_err());
    assert!(prompt("nope", &json!({})).is_err());
    let p = prompt("review_change", &json!({ "change": "ECO-000004" })).unwrap();
    let text = p["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("get_change with change=ECO-000004"));
    assert!(text.contains("suggest_reviewers with change=ECO-000004"));
    let p = prompt("release_notes", &json!({ "item": "ASM-1" })).unwrap();
    let text = p["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("to=latest"));
    // every tool a prompt mentions exists
    for p in PROMPTS {
        let args = json!({ "item": "X", "change": "Y" });
        let text = prompt(p.name, &args).unwrap()["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if word.contains('_') && word.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                let known = exists(word) || ["in_work", "as_released"].contains(&word);
                assert!(known, "prompt {} mentions unknown tool {word}", p.name);
            }
        }
    }
}

#[test]
fn duplicates_are_grouped_by_content() {
    let row = |sha: &str, item: &str| insights::DuplicateFile {
        sha256: sha.into(),
        filename: "a.step".into(),
        size_bytes: 5,
        item_number: item.into(),
        revision_code: "A".into(),
        revision_state: "released".into(),
    };
    let g = group_duplicates(&[
        row("zz", "P-1"),
        row("zz", "P-2"),
        row("aa", "P-3"),
        row("aa", "P-4"),
    ]);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["sha256"], "zz");
    assert_eq!(g[0]["copies"].as_array().unwrap().len(), 2);
}

#[test]
fn token_scopes_narrow_the_tool_list() {
    let all = definitions();
    let names = |defs: &[Value]| -> Vec<String> {
        defs.iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&definitions_for(TokenScope::Full)), names(&all));
    assert_eq!(names(&definitions_for(TokenScope::McpOnly)), names(&all));
    let read = definitions_for(TokenScope::ReadOnly);
    assert!(!read.is_empty() && read.len() < all.len());
    for d in &all {
        let name = d["name"].as_str().unwrap();
        let read_only = d["annotations"]["readOnlyHint"] == true;
        assert_eq!(is_read_only(name), read_only, "{name}");
        assert_eq!(read.iter().any(|r| r["name"] == name), read_only, "{name}");
    }
    for write in [
        "create_change_draft",
        "add_to_change",
        "create_change_request",
        "comment_change_request",
        "propose_result_values",
    ] {
        assert!(!is_read_only(write), "{write}");
    }
    assert!(is_read_only("search_items"));
    assert!(!is_read_only("no_such_tool"));
}

#[test]
fn open_3d_preview_is_a_described_read_tool() {
    let defs = definitions();
    let d = defs
        .iter()
        .find(|d| d["name"] == "open_3d_preview")
        .expect("open_3d_preview is registered");
    assert_eq!(d["annotations"]["readOnlyHint"], true);
    assert!(is_read_only("open_3d_preview"));
    let schema = &d["inputSchema"];
    assert_eq!(schema["required"], json!([]));
    assert_eq!(schema["additionalProperties"], false);
    for key in ["item", "q", "project", "revision", "file"] {
        assert_eq!(schema["properties"][key]["type"], "string", "{key}");
    }
    let text = d["description"].as_str().unwrap();
    for word in ["choose", "no_3d_file", "never guess"] {
        assert!(text.contains(word), "{word}");
    }
}

#[test]
fn open_3d_preview_arguments_resolve_to_an_item_or_a_search() {
    use preview_tools::Target;
    let target = |v: Value| preview_tools::target(&Args::new(&v));
    assert_eq!(
        target(json!({ "item": " PRT-000012 " })).unwrap(),
        Target::Item {
            number: "PRT-000012".into(),
            revision: None
        }
    );
    assert_eq!(
        target(json!({ "item": "PRT-000012/B" })).unwrap(),
        Target::Item {
            number: "PRT-000012".into(),
            revision: Some("B".into())
        }
    );
    assert_eq!(
        target(json!({ "item": "PRT-000012", "revision": "C" })).unwrap(),
        Target::Item {
            number: "PRT-000012".into(),
            revision: Some("C".into())
        }
    );
    // the same code twice is fine, two different ones are not
    assert!(target(json!({ "item": "PRT-000012/b", "revision": "B" })).is_ok());
    assert!(target(json!({ "item": "PRT-000012/B", "revision": "C" })).is_err());
    // `item` wins over `q`
    assert!(matches!(
        target(json!({ "item": "ASM-000001", "q": "bracket" })).unwrap(),
        Target::Item { .. }
    ));
    assert_eq!(
        target(json!({ "q": "bracket", "revision": "latest" })).unwrap(),
        Target::Search {
            query: "bracket".into(),
            revision: Some("latest".into())
        }
    );
    for bad_args in [
        json!({}),
        json!({ "q": "   " }),
        json!({ "item": "/B" }),
        json!({ "item": 12 }),
        json!({ "q": ["bracket"] }),
    ] {
        assert!(target(bad_args.clone()).is_err(), "{bad_args}");
    }
}

#[test]
fn open_3d_preview_picks_viewable_files_models_first() {
    use crate::preview::PreviewFormat;
    let fmt = preview_tools::preview_format;
    assert_eq!(fmt("step", "x.bin"), Some(PreviewFormat::Step));
    assert_eq!(fmt("", "bracket.STL"), Some(PreviewFormat::Stl), "by name");
    assert_eq!(fmt("", "bracket.iges"), None);
    assert_eq!(fmt("", "drawing.pdf"), None);
    assert_eq!(fmt("", "notes.txt"), None);
    let rank = preview_tools::preview_rank;
    assert!(rank(PreviewFormat::Step) < rank(PreviewFormat::Stl));
    assert_eq!(rank(PreviewFormat::Stl), rank(PreviewFormat::NxPart));
    assert!(rank(PreviewFormat::Stl) < rank(PreviewFormat::Vtu));
    assert!(rank(PreviewFormat::Vtu) < rank(PreviewFormat::Las));
}

#[test]
fn viewer_urls_are_checked_and_made_relative() {
    let id = "0b9c3a3e-5d1f-4c9a-9e8b-1234567890ab";
    let public = "https://plm.example.org";
    let ok = format!("/files/{id}/preview");
    assert_eq!(viewer_path(id.parse().unwrap()), ok);
    assert_eq!(
        checked_viewer_path(&ok, public).as_deref(),
        Some(ok.as_str())
    );
    assert_eq!(
        checked_viewer_path(&format!("{public}/files/{id}/preview"), public).as_deref(),
        Some(ok.as_str())
    );
    assert_eq!(
        checked_viewer_path(&format!("{public}{ok}"), "https://plm.example.org/").as_deref(),
        Some(ok.as_str()),
        "a trailing slash of the public URL"
    );
    // upper-case hex is canonicalised
    assert_eq!(
        checked_viewer_path(&format!("/files/{}/preview", id.to_uppercase()), public).as_deref(),
        Some(ok.as_str())
    );
    for bad in [
        format!("https://evil.example/files/{id}/preview"),
        format!("https://plm.example.org.evil.example/files/{id}/preview"),
        format!("https://plm.example.org@evil.example/files/{id}/preview"),
        format!("http://plm.example.org/files/{id}/preview"),
        format!("//evil.example/files/{id}/preview"),
        format!("/files/{id}/preview?x=1"),
        format!("/files/{id}/preview#x"),
        format!("/files/{id}/download"),
        format!("/files/{id}/preview/../../admin"),
        format!("/files/../admin/{id}/preview"),
        "/files/not-a-uuid-at-all-but-36-chars-x/preview".to_string(),
        "/files/0b9c3a3e%2d5d1f-4c9a-9e8b-1234567890ab/preview".to_string(),
        format!("javascript:alert(1)//files/{id}/preview"),
        format!("files/{id}/preview"),
        String::new(),
    ] {
        assert_eq!(checked_viewer_path(&bad, public), None, "{bad}");
    }
    // without a public URL only relative paths pass
    assert_eq!(checked_viewer_path(&ok, "").as_deref(), Some(ok.as_str()));
    assert_eq!(checked_viewer_path(&format!("{public}{ok}"), ""), None);
}

#[test]
fn the_open_hint_names_the_viewer() {
    let url = "https://plm.example.org/files/x/preview";
    assert_eq!(
        preview_action(url),
        json!({ "action": "open", "kind": "preview3d", "url": url })
    );
}
