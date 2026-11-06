//! Proofing of the BOM import (`services::imports`, `bomtable`, `stepbom`):
//!
//! * the dry-run plan equals what the import writes, over generated tables
//!   (repeated parts, parts that exist from an earlier import, invalid
//!   numbers, duplicate lines below one parent, reference rows, decimal
//!   commas, units),
//! * cycles and self-references never reach the database,
//! * quantity, unit and position edge cases of `bomtable`,
//! * a corpus of malformed STEP files never panics.
//!
//! Atomicity and repeated imports: `tests/import_atomicity.rs`; random byte
//! damage of tables and STEP files: `tests/parser_robustness.rs`.

mod common;

use std::collections::{BTreeMap, HashMap};

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{bomtable, domain::Role, stepbom};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Small deterministic generator (xorshift), so a failing seed can be rerun.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// A random structured BOM table (CSV, levels 1…4) that uses a pool of part
/// numbers shared between seeds, so later tables meet items created by
/// earlier imports.
fn generated_table(seed: u64) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut csv = String::from("Level;Part Number;Name;Qty;Unit;BOM Structure\n");
    let rows = 3 + rng.below(14);
    let mut level = 1;
    for _ in 0..rows {
        // down at most one level, up any number of levels
        level = match rng.below(3) {
            0 if level < 4 => level + 1,
            1 => 1 + rng.below(level),
            _ => level,
        };
        let number = match rng.below(10) {
            // an invalid item number: imported with a generated number
            0 => format!("M{} x {}", 3 + rng.below(4), 10 + 5 * rng.below(4)),
            _ => format!("PP-{:03}", rng.below(40)),
        };
        let qty = match rng.below(6) {
            0 => "2,5".to_string(),
            1 => format!("{} Stk", 1 + rng.below(9)),
            2 => String::new(),
            _ => format!("{}", 1 + rng.below(12)),
        };
        let unit = if qty == "2,5" { "m" } else { "" };
        let structure = match rng.below(12) {
            0 => "Reference",
            1 => "Purchased",
            _ => "Normal",
        };
        csv.push_str(&format!(
            "{level};{number};Name of {number};{qty};{unit};{structure}\n"
        ));
        if rng.chance(8) {
            csv.push_str(";;;;;\n"); // empty separator row
        }
    }
    csv
}

/// BOM lines below the given parents as (parent, child) → (quantity, find).
async fn lines_of(
    pool: &sqlx::PgPool,
    parents: &[String],
) -> BTreeMap<(String, String), (f64, i32)> {
    sqlx::query_as::<_, (String, String, f64, i32)>(
        "SELECT pi.number, ci.number, bl.quantity, bl.find_number
         FROM bom_lines bl
         JOIN revisions pr ON pr.id = bl.parent_revision_id
         JOIN items pi ON pi.id = pr.item_id
         JOIN items ci ON ci.id = bl.child_item_id
         WHERE pi.number = ANY($1)",
    )
    .bind(parents)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(p, c, q, f)| ((p, c), (q, f)))
    .collect()
}

#[tokio::test]
async fn the_dry_run_plan_equals_the_import_over_generated_tables() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (mut imported, mut existing_seen, mut lines_seen) = (0, 0, 0);
    for seed in 1..=24u64 {
        let csv = generated_table(seed);
        let files: &[(&str, &[u8])] = &[("gen.csv", csv.as_bytes())];
        let plan = app
            .api_multipart(
                "/api/v1/bom-imports",
                &eng,
                files,
                &[("project", "ROCKET"), ("dry_run", "true")],
            )
            .await;
        if plan.status == StatusCode::BAD_REQUEST {
            // e.g. every row below a reference: the import must refuse too
            let r = app
                .api_multipart("/api/v1/bom-imports", &eng, files, &[("project", "ROCKET")])
                .await;
            assert_eq!(
                r.status,
                StatusCode::BAD_REQUEST,
                "seed {seed}: {}",
                r.text()
            );
            continue;
        }
        assert_eq!(
            plan.status,
            StatusCode::OK,
            "seed {seed}: {}\n{csv}",
            plan.text()
        );
        let plan = plan.json();
        let done = app
            .api_multipart("/api/v1/bom-imports", &eng, files, &[("project", "ROCKET")])
            .await;
        assert_eq!(
            done.status,
            StatusCode::CREATED,
            "seed {seed}: {}\n{csv}",
            done.text()
        );
        let done = done.json();

        // the planned items and their numbers after the import
        let created: HashMap<String, String> = done["created"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["file_number"].as_str().unwrap().to_uppercase(),
                    c["number"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let items = plan["items"].as_array().unwrap();
        assert_eq!(
            created.len() as i64,
            plan["new_items"].as_i64().unwrap(),
            "seed {seed}: created items"
        );
        assert_eq!(
            done["existing_items"], plan["existing_items"],
            "seed {seed}"
        );
        let number_of = |i: &Value| -> String {
            match i["existing"].as_str() {
                Some(_) => i["number"].as_str().unwrap().to_string(),
                None => created[&i["file_number"].as_str().unwrap().to_uppercase()].clone(),
            }
        };
        let root = done["root"]
            .as_str()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_string();

        // every planned line is in the database with quantity and find number
        let mut expected: BTreeMap<(String, String), (f64, i32)> = BTreeMap::new();
        for l in plan["lines"].as_array().unwrap() {
            let parent = match l["parent"].as_u64() {
                Some(p) => number_of(&items[p as usize]),
                None => root.clone(),
            };
            let child = number_of(&items[l["child"].as_u64().unwrap() as usize]);
            let previous = expected.insert(
                (parent.clone(), child.clone()),
                (
                    l["quantity"].as_f64().unwrap(),
                    l["find_number"].as_i64().unwrap() as i32,
                ),
            );
            assert!(
                previous.is_none(),
                "seed {seed}: one line per parent and child ({parent} → {child})"
            );
        }
        assert_eq!(
            done["lines"].as_u64().unwrap() as usize,
            expected.len(),
            "seed {seed}"
        );
        let parents: Vec<String> = expected.keys().map(|(p, _)| p.clone()).collect();
        let written = lines_of(&app.pool, &parents).await;
        // parents that existed before keep their own lines – compare only
        // the parents this import filled
        let written: BTreeMap<_, _> = written
            .into_iter()
            .filter(|((p, _), _)| {
                p == &root
                    || items
                        .iter()
                        .any(|i| i["existing"].is_null() && &number_of(i) == p)
            })
            .collect();
        for ((p, c), (q, f)) in &expected {
            let got = written.get(&(p.clone(), c.clone()));
            assert!(
                got.is_some_and(|(gq, gf)| (gq - q).abs() < 1e-9 && gf == f),
                "seed {seed}: line {p} → {c} qty {q} find {f}, written {got:?}\n{csv}"
            );
        }
        assert_eq!(
            written.len(),
            expected.len(),
            "seed {seed}: no extra lines\n{csv}"
        );
        imported += 1;
        existing_seen += plan["existing_items"].as_i64().unwrap();
        lines_seen += expected.len();
    }
    // the generator really exercises the import: most tables import, later
    // ones meet items of earlier imports
    assert!(imported >= 18, "only {imported} of 24 tables imported");
    assert!(
        existing_seen > 10 && lines_seen > 100,
        "{existing_seen} existing, {lines_seen} lines"
    );
    app.teardown().await;
}

#[tokio::test]
async fn cycles_and_self_references_never_reach_the_database() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let create = |number: &str, item_type: &str| json!({ "item_type": item_type, "name": number, "number": number, "project": "ROCKET" });
    for (n, t) in [("TOP-1", "assembly"), ("MID-1", "assembly")] {
        let r = app
            .api(Method::POST, "/api/v1/items", &eng, Some(create(n, t)))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    // TOP-1 contains MID-1
    let r = app
        .api(
            Method::POST,
            "/api/v1/items/TOP-1/revisions/A/bom",
            &eng,
            Some(json!({ "child": "MID-1", "quantity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // Importing into MID-1 a table that lists TOP-1 (its parent) and MID-1
    // (itself): both rows are skipped with a warning, the rest is imported.
    let csv = "Level;Part Number;Name;Qty\n1;TOP-1;Top;1\n1;MID-1;Mid;1\n1;LEAF-1;Leaf;2\n";
    let url = "/api/v1/items/MID-1/revisions/A/bom/import";
    let plan = app
        .api_multipart(
            url,
            &eng,
            &[("mid.csv", csv.as_bytes())],
            &[("dry_run", "true")],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    let warnings = plan.json()["warnings"].to_string();
    assert!(warnings.contains("circular"), "{warnings}");
    let r = app
        .api_multipart(url, &eng, &[("mid.csv", csv.as_bytes())], &[])
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let lines = lines_of(&app.pool, &["MID-1".into(), "TOP-1".into()]).await;
    let mid: Vec<&str> = lines
        .keys()
        .filter(|(p, _)| p == "MID-1")
        .map(|(_, c)| c.as_str())
        .collect();
    assert_eq!(
        mid,
        ["LEAF-1"],
        "no line to itself or to its parent: {lines:?}"
    );

    // A table whose rows nest a part inside itself (A > B > A): the inner
    // occurrence is skipped, A keeps one line to B.
    let csv = "Level;Part Number;Name;Qty\n1;LOOP-A;A;1\n2;LOOP-B;B;1\n3;LOOP-A;A again;1\n";
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("loop.csv", csv.as_bytes())],
            &[("project", "ROCKET")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert!(
        r.json()["warnings"].to_string().contains("LOOP-A"),
        "{}",
        r.text()
    );
    let lines = lines_of(&app.pool, &["LOOP-A".into(), "LOOP-B".into()]).await;
    assert_eq!(
        lines.keys().cloned().collect::<Vec<_>>(),
        [("LOOP-A".to_string(), "LOOP-B".to_string())],
        "{lines:?}"
    );

    // No BOM in the database contains a cycle.
    let cycles: i64 = sqlx::query_scalar(
        "WITH RECURSIVE walk(start, item, depth) AS (
             SELECT pr.item_id, bl.child_item_id, 1
             FROM bom_lines bl JOIN revisions pr ON pr.id = bl.parent_revision_id
           UNION ALL
             SELECT w.start, bl.child_item_id, w.depth + 1
             FROM walk w
             JOIN revisions r ON r.item_id = w.item
             JOIN bom_lines bl ON bl.parent_revision_id = r.id
             WHERE w.depth < 30 AND w.item <> w.start)
         SELECT COUNT(*) FROM walk WHERE item = start",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(cycles, 0);
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// bomtable: quantities, units, positions, levels
// ---------------------------------------------------------------------------

fn table(csv: &str) -> bomtable::BomTable {
    bomtable::read("t.csv", csv.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{csv}"))
}

#[test]
fn quantity_and_unit_edge_cases() {
    let cases: &[(&str, f64, &str, bool)] = &[
        // (QTY cell, quantity, unit, warning expected)
        ("2,5", 2.5, "", false),
        ("1.234,5", 1234.5, "", false),
        ("1,234.5", 1234.5, "", false),
        ("4 Stk", 4.0, "each", false),
        ("2.5 m", 2.5, "m", false),
        ("3 Satz", 3.0, "set", false),
        ("+3", 3.0, "", false),
        ("", 1.0, "", false),
        ("0", 1.0, "", true),
        ("-2", 1.0, "", true),
        ("abc", 1.0, "", true),
        ("1e9", 1.0, "", false), // the number stops at 'e' → 1, unit "e9"
        ("999999999999", 1.0, "", true),
    ];
    for (cell, quantity, unit, warned) in cases {
        let t = table(&format!("Part Number;Name;Qty\nP-1;Part;\"{cell}\"\n"));
        let row = &t.rows[0];
        assert_eq!(row.quantity, *quantity, "{cell:?}");
        if *cell != "1e9" {
            assert_eq!(row.unit, *unit, "{cell:?}");
        }
        assert_eq!(
            t.warnings.iter().any(|w| w.contains("quantity")),
            *warned,
            "{cell:?}: {:?}",
            t.warnings
        );
        assert!(row.quantity.is_finite() && row.quantity > 0.0);
    }
    // an explicit unit column wins over the unit in the quantity cell
    let t = table("Part Number;Name;Qty;Unit\nP-1;Part;4 Stk;m\n");
    assert_eq!((t.rows[0].quantity, t.rows[0].unit.as_str()), (4.0, "m"));
}

#[test]
fn dotted_positions_with_gaps_and_empty_levels() {
    // 1, 1.1, 1.1.3 (find number gap), 1.3.1 (its parent 1.3 is missing), 2
    let t =
        table("Pos;Part Number;Name;Qty\n1;A;A;1\n1.1;B;B;1\n1.1.3;C;C;1\n1.3.1;D;D;1\n2;E;E;1\n");
    let shape: Vec<(&str, usize, Option<i32>)> = t
        .rows
        .iter()
        .map(|r| (r.number.as_str(), r.level, r.find_number))
        .collect();
    assert_eq!(shape[0], ("A", 1, Some(1)));
    assert_eq!(shape[1], ("B", 2, Some(1)));
    assert_eq!(shape[2], ("C", 3, Some(3)), "the find number keeps the gap");
    // D's parent 1.3 does not exist: D must not end up below C (1.1.3) or B
    // (1.1) silently – it hangs below A, the nearest existing ancestor, with a
    // warning.
    assert_eq!(shape[3].0, "D");
    assert_eq!(shape[3].1, 2, "D below A, not below B: {shape:?}");
    assert!(
        t.warnings.iter().any(|w| w.contains("1.3.1")),
        "{:?}",
        t.warnings
    );
    assert_eq!(shape[4], ("E", 1, Some(2)));
    // a child below a skipped reference stays skipped, even without its parent row
    let t = table("Pos;Part Number;Name;Qty;BOM Structure\n1;A;A;1;Normal\n2;R;R;1;Reference\n2.1;X;X;1;Normal\n2.1.1;Y;Y;1;Normal\n3;F;F;1;Normal\n");
    let numbers: Vec<&str> = t.rows.iter().map(|r| r.number.as_str()).collect();
    assert_eq!(numbers, ["A", "F"]);
    // a position out of order (1.2 after 2) is reported
    let t = table("Pos;Part Number;Name;Qty\n1;A;A;1\n1.1;B;B;1\n2;C;C;1\n1.2;D;D;1\n");
    assert!(
        t.warnings
            .iter()
            .any(|w| w.contains("1.2") && w.contains("comes after")),
        "{:?}",
        t.warnings
    );

    // empty and invalid level cells: used 1 with a warning, never 0 or panic
    let t = table("Level;Part Number;Name;Qty\n1;A;A;1\n;B;B;1\nx;C;C;1\n..2;D;D;1\n");
    assert!(t.rows.iter().all(|r| r.level >= 1));
    assert_eq!(
        t.warnings.iter().filter(|w| w.contains("level")).count(),
        2,
        "{:?}",
        t.warnings
    );
    assert_eq!(t.rows[3].level, 2, "..2 is level 2 below the row above");
}

// ---------------------------------------------------------------------------
// stepbom: malformed files
// ---------------------------------------------------------------------------

/// A small assembly: TOP with 2 × P-1 and SUB, SUB with 3 × P-2.
fn assembly_step() -> String {
    let product = |id: u32, number: &str| {
        format!(
            "#{a}=PRODUCT('{number}','{number} name','',(#2));\n\
             #{b}=PRODUCT_DEFINITION_FORMATION('','',#{a});\n\
             #{c}=PRODUCT_DEFINITION('design','',#{b},#3);\n",
            a = id,
            b = id + 1,
            c = id + 2
        )
    };
    let mut s = String::from(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
         FILE_NAME('a.stp','2026-11-06',(''),(''),'','','');\n\
         FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n\
         #2=PRODUCT_CONTEXT('',#1,'mechanical');\n#3=PRODUCT_DEFINITION_CONTEXT('',#1,'design');\n",
    );
    s += &product(10, "TOP");
    s += &product(20, "P-1");
    s += &product(30, "SUB");
    s += &product(40, "P-2");
    for (i, (parent, child)) in [(12, 22), (12, 22), (12, 32), (32, 42), (32, 42), (32, 42)]
        .iter()
        .enumerate()
    {
        s += &format!(
            "#{}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{}','','',#{parent},#{child},$);\n",
            100 + i,
            i + 1
        );
    }
    s += "ENDSEC;\nEND-ISO-10303-21;\n";
    s
}

/// What a parsed table may never contain, whatever the input.
fn check_invariants(t: &bomtable::BomTable, what: &str) {
    assert!(t.rows.len() <= bomtable::MAX_ROWS, "{what}");
    for r in &t.rows {
        assert!(r.level >= 1 && r.level <= 64, "{what}: level {}", r.level);
        assert!(r.quantity.is_finite() && r.quantity >= 1.0, "{what}");
    }
}

#[test]
fn the_sample_assembly_is_read() {
    let t = stepbom::read(assembly_step().as_bytes()).unwrap();
    assert_eq!(t.root.as_ref().unwrap().number, "TOP");
    let rows: Vec<(&str, usize, f64)> = t
        .rows
        .iter()
        .map(|r| (r.number.as_str(), r.level, r.quantity))
        .collect();
    assert_eq!(rows, [("P-1", 1, 2.0), ("SUB", 1, 1.0), ("P-2", 2, 3.0)]);
}

#[test]
fn truncated_step_files_never_panic() {
    let full = assembly_step();
    let bytes = full.as_bytes();
    for end in 0..bytes.len() {
        if let Ok(t) = stepbom::read(&bytes[..end]) {
            check_invariants(&t, &format!("cut at {end}"));
        }
    }
}

#[test]
fn unusual_step_content_never_panics() {
    let base = assembly_step();
    let data_end = base.find("ENDSEC;\nEND-ISO").unwrap();
    let with = |extra: &str| format!("{}{extra}{}", &base[..data_end], &base[data_end..]);
    let cases: Vec<(&str, String)> = vec![
        (
            "unknown entities",
            with("#500=SOMETHING_NEW('x',#12,(1.,2.),.T.);\n#501=FOO();\n"),
        ),
        (
            "quantified usage",
            with(
                "#502=QUANTIFIED_ASSEMBLY_COMPONENT_USAGE('9','','',#12,#42,$,#503);\n\
                  #503=MEASURE_REPRESENTATION_ITEM('',POSITIVE_RATIO_MEASURE(4.),#504);\n",
            ),
        ),
        (
            "complex instance",
            with("#505=(NAMED_UNIT(*)SI_UNIT($,.METRE.)LENGTH_UNIT());\n"),
        ),
        (
            "strings with separators",
            with("#506=PRODUCT('X;Y','it''s \\X2\\00E4\\X0\\ (#1)','',(#2));\n"),
        ),
        (
            "comments",
            with("/* #507=PRODUCT('C','c','',(#2)); */\n#508=FOO(/* ; */1);\n"),
        ),
        (
            "deep lists",
            with(&format!(
                "#509=FOO({}{});\n",
                "(".repeat(5000),
                ")".repeat(5000)
            )),
        ),
        (
            "dangling references",
            with("#510=NEXT_ASSEMBLY_USAGE_OCCURRENCE('x','','',#99999,#12,$);\n"),
        ),
        (
            "self usage",
            with("#511=NEXT_ASSEMBLY_USAGE_OCCURRENCE('x','','',#12,#12,$);\n"),
        ),
        (
            "huge ids",
            with("#99999999999999999999=PRODUCT('Z','z','',(#2));\n"),
        ),
        (
            "unterminated string",
            with("#512=PRODUCT('open,'x','',(#2));\n"),
        ),
        ("no data section", base.replace("DATA;", "")),
        ("no header", base.replacen("ISO-10303-21;", "", 1)),
        ("only the magic", "ISO-10303-21;".to_string()),
        (
            "binary after the header",
            format!("{}\u{0}\u{1}\u{ff}", &base[..data_end]),
        ),
    ];
    for (what, text) in &cases {
        if let Ok(t) = stepbom::read(text.as_bytes()) {
            check_invariants(&t, what);
        }
    }
    // the quantified usage is not read, the rest of the structure still is
    let t = stepbom::read(cases[1].1.as_bytes()).unwrap();
    assert_eq!(t.rows.len(), 3);
}
