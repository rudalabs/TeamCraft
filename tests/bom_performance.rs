//! Performance of the dynamic BOM: a large synthetic structure (4094 lines,
//! 11 levels, shared parts) is expanded with a bounded number of queries –
//! one per BOM level – and the results match the expected roll-ups for every
//! revision rule. Also covers the multi-level where-used report.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use teamcraft::{
    domain::RevisionRule,
    domain::Role,
    services::{
        bom,
        workset::{self, Via},
    },
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

/// Assemblies of the synthetic tree, numbered like a binary heap: assembly
/// `n` holds `2n` (find no. 10) and `2n + 1` (find no. 20); the 1024 leaf
/// assemblies (depth 10) hold 4 × PRT-BOLT and 2 × PRT-NUT.
const ASSEMBLIES: i32 = 2047;
const FIRST_LEAF: i32 = 1024;

/// Builds the structure with set-based SQL (creating 2000 items through the
/// services would test the services, not the expansion). Returns the root
/// revision id.
///
/// PRT-BOLT has A (released, 0.01 kg), B (released, 0.02 kg) and C (in work,
/// 0.03 kg); every bolt line is frozen to A as if the leaves had been
/// released with it. PRT-NUT has A (released, 0.005 kg).
async fn build_large_tree(app: &common::TestApp, user: Uuid) -> Uuid {
    let pool = &app.pool;
    let statements = [
        "INSERT INTO items (id, number, item_type, created_by)
         SELECT gen_random_uuid(), 'ASM-T' || lpad(n::text, 5, '0'), 'assembly', $1
         FROM generate_series(1, 2047) n",
        "INSERT INTO items (id, number, item_type, created_by) VALUES
         (gen_random_uuid(), 'PRT-BOLT', 'part', $1), (gen_random_uuid(), 'PRT-NUT', 'part', $1)",
        "INSERT INTO revisions (id, item_id, seq, code, state, name, mass_kg, unit_cost, created_by, released_at)
         SELECT gen_random_uuid(), i.id, 1, 'A', 'released', 'Synthetic ' || i.number,
                CASE i.number WHEN 'PRT-BOLT' THEN 0.01 WHEN 'PRT-NUT' THEN 0.005 ELSE 0 END,
                CASE i.number WHEN 'PRT-BOLT' THEN 0.1 WHEN 'PRT-NUT' THEN 0.05 ELSE 1 END,
                $1, now()
         FROM items i",
        "INSERT INTO revisions (id, item_id, seq, code, state, name, mass_kg, unit_cost, created_by, released_at)
         SELECT gen_random_uuid(), i.id, v.seq, v.code, v.state, 'Bolt', v.mass, 0.1, $1,
                CASE v.state WHEN 'released' THEN now() END
         FROM items i, (VALUES (2, 'B', 'released', 0.02), (3, 'C', 'in_work', 0.03)) v(seq, code, state, mass)
         WHERE i.number = 'PRT-BOLT'",
        // assembly lines: child n below parent n / 2
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity)
         SELECT gen_random_uuid(), pr.id, ci.id, 10 + 10 * (c.n % 2), 1
         FROM generate_series(2, 2047) c(n)
         JOIN items ci ON ci.number = 'ASM-T' || lpad(c.n::text, 5, '0')
         JOIN items pi ON pi.number = 'ASM-T' || lpad((c.n / 2)::text, 5, '0')
         JOIN revisions pr ON pr.item_id = pi.id",
        // parts in every leaf; bolts frozen to revision A
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity,
                                released_child_revision_id)
         SELECT gen_random_uuid(), pr.id, p.id, p.find, p.qty,
                CASE p.number WHEN 'PRT-BOLT' THEN
                    (SELECT r.id FROM revisions r WHERE r.item_id = p.id AND r.code = 'A')
                END
         FROM generate_series(1024, 2047) l(n)
         JOIN items li ON li.number = 'ASM-T' || lpad(l.n::text, 5, '0')
         JOIN revisions pr ON pr.item_id = li.id
         CROSS JOIN (SELECT i.id, i.number,
                            CASE i.number WHEN 'PRT-BOLT' THEN 10 ELSE 20 END AS find,
                            CASE i.number WHEN 'PRT-BOLT' THEN 4.0 ELSE 2.0 END AS qty
                     FROM items i WHERE i.number IN ('PRT-BOLT', 'PRT-NUT')) p",
    ];
    for sql in statements {
        let query = sqlx::query(sql);
        let query = if sql.contains("$1") {
            query.bind(user)
        } else {
            query
        };
        query
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{e}: {sql}"));
    }
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = 'ASM-T00001'",
    )
    .fetch_one(pool)
    .await
    .expect("root revision")
}

#[tokio::test]
async fn large_bom_expands_level_by_level() {
    let app = app_or_skip!();
    let (admin, token) = app.user("root", Role::Admin).await;
    let root = build_large_tree(&app, admin.id).await;
    let leaves = f64::from(ASSEMBLIES - FIRST_LEAF + 1);
    let lines = (ASSEMBLIES - 1) as usize + 2 * 1024;

    for (rule, bolt_mass) in [
        (RevisionRule::Latest, 0.03),
        (RevisionRule::Released, 0.02),
        (RevisionRule::AsReleased, 0.01),
    ] {
        let started = Instant::now();
        let tree = bom::tree(&app.pool, &teamcraft::auth::Actor::system(), root, rule)
            .await
            .expect("expand");
        let elapsed = started.elapsed();
        eprintln!(
            "{}: {} lines in {elapsed:?} with {} queries",
            rule.as_str(),
            tree.line_count,
            tree.queries
        );
        assert_eq!(tree.line_count, lines);
        assert_eq!(tree.unresolved_count, 0);
        // root + one query per level (11 levels of lines, one for the parts' empty BOMs)
        assert!(tree.queries <= 13, "{} queries", tree.queries);
        let expected_mass = leaves * (4.0 * bolt_mass + 2.0 * 0.005);
        assert!(
            (tree.total_mass_kg - expected_mass).abs() < 1e-6,
            "{}: {} kg, expected {expected_mass}",
            rule.as_str(),
            tree.total_mass_kg
        );
        assert!(tree.mass_complete && tree.cost_complete);
        // every assembly costs 1, every leaf adds 4 × 0.1 + 2 × 0.05
        let expected_cost = f64::from(ASSEMBLIES) + leaves * 0.5;
        assert!((tree.total_cost - expected_cost).abs() < 1e-6);
        // loose bound: catches a regression to per-line queries on slow CI runners
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");

        let rows = bom::flatten(&tree.children);
        assert_eq!(rows.len(), lines);
        assert_eq!(rows.iter().map(|r| r.level).max(), Some(11));
        // depth first, by find number: T00002 (10) before T00003 (20)
        assert_eq!(rows[0].node.item_number, "ASM-T00002");
        assert_eq!(rows[1].node.item_number, "ASM-T00004");
        let bolt = rows
            .iter()
            .find(|r| r.node.item_number == "PRT-BOLT")
            .unwrap();
        assert_eq!(bolt.level, 11);
        let code = &bolt.node.revision.as_ref().unwrap().code;
        let expected_code = match rule {
            RevisionRule::Latest => "C",
            RevisionRule::Released => "B",
            RevisionRule::AsReleased => "A",
        };
        assert_eq!(code, expected_code);
    }

    // The JSON API serves the same tree.
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-T00001/revisions/A/bom?expand=true&rule=released",
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["line_count"], lines);

    // Multi-level where-used: a leaf assembly reaches the root on level 10.
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-T01024/where-used?levels=25",
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0]["level"], 1);
    assert_eq!(rows[0]["parent_number"], "ASM-T00512");
    assert_eq!(rows[0]["child_number"], "ASM-T01024");
    assert_eq!(rows[9]["level"], 10);
    assert_eq!(rows[9]["parent_number"], "ASM-T00001");
    assert_eq!(rows[9]["child_number"], "ASM-T00002");
    // without `levels` the single-level list keeps its shape
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-T01024/where-used",
            &token,
            None,
        )
        .await;
    let rows = r.json();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert!(rows[0].get("level").is_none());
    // the shared bolt: 1024 uses on level 1, 1024 on level 2 – capped
    let used = teamcraft::services::items::where_used_levels(
        &app.pool,
        &teamcraft::auth::Actor::system(),
        sqlx::query_scalar("SELECT id FROM items WHERE number = 'PRT-BOLT'")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        used.len() as i64,
        teamcraft::services::items::MAX_WHERE_USED_ROWS
    );
    assert!(used[..1024].iter().all(|u| u.level == 1));
    assert!(used[1024..].iter().all(|u| u.level == 2));

    app.teardown().await;
}

/// The working set ("Get" of the desktop app) of the large tree: the same
/// revisions in the same order as the expanded BOM it was derived from
/// before, with one query per BOM level instead of one per line.
#[tokio::test]
async fn large_workset_follows_the_expanded_bom() {
    let app = app_or_skip!();
    let (admin, _token) = app.user("root", Role::Admin).await;
    let root = build_large_tree(&app, admin.id).await;

    for rule in [
        RevisionRule::Latest,
        RevisionRule::Released,
        RevisionRule::AsReleased,
    ] {
        let name = rule.as_str();
        let started = Instant::now();
        let ws = workset::resolve(&app.pool, &admin, root, rule, true)
            .await
            .expect("workset");
        let elapsed = started.elapsed();
        eprintln!(
            "workset {name}: {} revisions in {elapsed:?} with {} queries",
            ws.revisions.len(),
            ws.queries
        );
        // the former derivation: every revision of the flattened tree once
        let tree = bom::tree(&app.pool, &teamcraft::auth::Actor::system(), root, rule)
            .await
            .expect("expand");
        let mut expected = vec![(tree.root.id, Via::Root)];
        for row in bom::flatten(&tree.children) {
            let id = row.node.revision.as_ref().unwrap().id;
            if !expected.iter().any(|(e, _)| *e == id) {
                expected.push((id, Via::Bom));
            }
        }
        let got: Vec<(Uuid, Via)> = ws
            .revisions
            .iter()
            .map(|r| (r.revision_id, r.via))
            .collect();
        assert_eq!(got, expected, "{name}");
        // 2047 assemblies + bolt + nut (one bolt revision per rule)
        assert_eq!(ws.revisions.len(), 2049, "{name}");
        assert!(ws.unresolved.is_empty() && ws.missing_references.is_empty());
        // root + 12 levels + one reference round (no files) + revisions + files
        assert!(ws.queries <= 16, "{name}: {} queries", ws.queries);
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    app.teardown().await;
}

/// Inserts a file (version 1) into a revision whose content references `refs`.
async fn file_with_references(
    pool: &sqlx::PgPool,
    user: Uuid,
    revision: Uuid,
    filename: &str,
    refs: &[&str],
) {
    let file = Uuid::new_v4();
    let version = Uuid::new_v4();
    let sha = format!("{:0>64}", file.simple());
    sqlx::query(
        "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by)
         VALUES ($1, $2, $3, 'application/octet-stream', 10, $4, $5)",
    )
    .bind(file)
    .bind(revision)
    .bind(filename)
    .bind(&sha)
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type,
                                    uploaded_by, references_scanned)
         VALUES ($1, $2, 1, $3, 10, 'application/octet-stream', $4, TRUE)",
    )
    .bind(version)
    .bind(file)
    .bind(&sha)
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
    for name in refs {
        sqlx::query("INSERT INTO file_references (file_version_id, name) VALUES ($1, $2)")
            .bind(version)
            .bind(*name)
            .execute(pool)
            .await
            .unwrap();
    }
}

/// The working set with CAD references under `latest` and `released`:
/// BOM revisions first, then the items whose files are referenced (resolved
/// with the rule in one query per round), unresolved items of the BOM and of
/// references, and names found nowhere.
#[tokio::test]
async fn workset_resolves_references_in_batches() {
    let app = app_or_skip!();
    let (admin, _token) = app.user("root", Role::Admin).await;
    let pool = &app.pool;
    let (_, root) = item_with_revisions(
        pool,
        admin.id,
        "ASM-R",
        "assembly",
        &[("A", "released", 1.0)],
    )
    .await;
    let (sub_item, sub) = item_with_revisions(
        pool,
        admin.id,
        "ASM-S",
        "assembly",
        &[("A", "released", 1.0), ("B", "in_work", 1.0)],
    )
    .await;
    let (wip_item, _) =
        item_with_revisions(pool, admin.id, "PRT-X", "part", &[("A", "in_work", 1.0)]).await;
    let (_, p) = item_with_revisions(
        pool,
        admin.id,
        "PRT-P",
        "part",
        &[("A", "released", 1.0), ("B", "in_work", 1.0)],
    )
    .await;
    let (_, q) =
        item_with_revisions(pool, admin.id, "PRT-Q", "part", &[("A", "in_work", 1.0)]).await;
    let root = root[0];
    bom_line(pool, root, sub_item, 10, None).await;
    bom_line(pool, root, wip_item, 20, None).await;
    file_with_references(pool, admin.id, root, "r.asm", &["P.prt", "missing.prt"]).await;
    file_with_references(pool, admin.id, sub[0], "s.asm", &["p.prt"]).await;
    for rev in &p {
        file_with_references(pool, admin.id, *rev, "p.prt", &["q.prt"]).await;
    }
    file_with_references(pool, admin.id, q[0], "q.prt", &[]).await;

    let codes = |ws: &workset::Workset| -> Vec<(String, String, Via)> {
        ws.revisions
            .iter()
            .map(|r| (r.item_number.clone(), r.code.clone(), r.via))
            .collect()
    };
    let own = |n: &str, c: &str, v: Via| (n.to_string(), c.to_string(), v);

    let ws = workset::resolve(pool, &admin, root, RevisionRule::Latest, true)
        .await
        .unwrap();
    assert_eq!(
        codes(&ws),
        [
            own("ASM-R", "A", Via::Root),
            own("ASM-S", "B", Via::Bom),
            own("PRT-X", "A", Via::Bom),
            own("PRT-P", "B", Via::Reference),
            own("PRT-Q", "A", Via::Reference),
        ]
    );
    assert!(ws.unresolved.is_empty(), "{:?}", ws.unresolved);
    assert_eq!(ws.missing_references, ["missing.prt"]);
    // root + 2 BOM levels; 2 rounds × (names, holders, revisions) + last names; revisions + files
    assert!(ws.queries <= 12, "{} queries", ws.queries);
    assert_eq!(ws.file_count, 3);

    let ws = workset::resolve(pool, &admin, root, RevisionRule::Released, true)
        .await
        .unwrap();
    assert_eq!(
        codes(&ws),
        [
            own("ASM-R", "A", Via::Root),
            own("ASM-S", "A", Via::Bom),
            own("PRT-P", "A", Via::Reference),
        ]
    );
    assert_eq!(ws.unresolved, ["PRT-X", "PRT-Q"]);
    assert_eq!(ws.missing_references, ["missing.prt"]);
    assert_eq!(ws.file_count, 3);

    // without references only the BOM
    let ws = workset::resolve(pool, &admin, root, RevisionRule::Released, false)
        .await
        .unwrap();
    assert_eq!(ws.revisions.len(), 2);
    assert_eq!(ws.unresolved, ["PRT-X"]);
    assert!(ws.missing_references.is_empty());

    app.teardown().await;
}

/// Inserts an item with its revisions `(code, state, mass)` (seq in order)
/// and returns the item id and the revision ids.
async fn item_with_revisions(
    pool: &sqlx::PgPool,
    user: Uuid,
    number: &str,
    item_type: &str,
    revisions: &[(&str, &str, f64)],
) -> (Uuid, Vec<Uuid>) {
    let item = Uuid::new_v4();
    sqlx::query("INSERT INTO items (id, number, item_type, created_by) VALUES ($1, $2, $3, $4)")
        .bind(item)
        .bind(number)
        .bind(item_type)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (seq, (code, state, mass)) in revisions.iter().enumerate() {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO revisions (id, item_id, seq, code, state, name, mass_kg, unit_cost,
                                    created_by, released_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8,
                     CASE WHEN $5 = 'released' THEN now() END)",
        )
        .bind(id)
        .bind(item)
        .bind(seq as i32 + 1)
        .bind(*code)
        .bind(*state)
        .bind(format!("{number} {code}"))
        .bind(*mass)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    (item, ids)
}

async fn bom_line(
    pool: &sqlx::PgPool,
    parent: Uuid,
    child_item: Uuid,
    find: i32,
    frozen: Option<Uuid>,
) {
    sqlx::query(
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity,
                                released_child_revision_id)
         VALUES ($1, $2, $3, $4, 1, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(parent)
    .bind(child_item)
    .bind(find)
    .bind(frozen)
    .execute(pool)
    .await
    .unwrap();
}

/// The level-wise expansion picks the same child revisions as the former
/// line-by-line resolution: frozen lines under *as released*, the newest
/// (released) revision otherwise, unresolved lines without a match, and a
/// cycle written past the input checks is stopped on the path.
#[tokio::test]
async fn expansion_resolves_rules_unresolved_lines_and_cycles() {
    let app = app_or_skip!();
    let (admin, _token) = app.user("root", Role::Admin).await;
    let pool = &app.pool;
    let (root_item, root) = item_with_revisions(
        pool,
        admin.id,
        "ASM-R",
        "assembly",
        &[("A", "released", 1.0)],
    )
    .await;
    let (sub_item, sub) = item_with_revisions(
        pool,
        admin.id,
        "ASM-S",
        "assembly",
        &[
            ("A", "released", 2.0),
            ("B", "released", 3.0),
            ("C", "in_work", 4.0),
        ],
    )
    .await;
    // only an in-work revision: unresolved under released / as released
    let (wip_item, _) =
        item_with_revisions(pool, admin.id, "PRT-W", "part", &[("A", "in_work", 5.0)]).await;
    let root = root[0];
    bom_line(pool, root, sub_item, 10, Some(sub[0])).await;
    bom_line(pool, root, wip_item, 20, None).await;
    // ASM-S A: back to the root (a cycle) and the in-work part
    bom_line(pool, sub[0], root_item, 10, None).await;
    bom_line(pool, sub[0], wip_item, 20, None).await;
    bom_line(pool, sub[1], wip_item, 10, None).await;
    bom_line(pool, sub[2], wip_item, 10, None).await;

    for (rule, sub_code, lines, unresolved, cycle) in [
        (RevisionRule::Latest, "C", 3, 0, false),
        (RevisionRule::Released, "B", 3, 2, false),
        (RevisionRule::AsReleased, "A", 4, 2, true),
    ] {
        let tree = bom::tree(pool, &teamcraft::auth::Actor::system(), root, rule)
            .await
            .expect("expand");
        let name = rule.as_str();
        assert_eq!(tree.line_count, lines, "{name}");
        assert_eq!(tree.unresolved_count, unresolved, "{name}");
        let s = &tree.children[0];
        assert_eq!(s.item_number, "ASM-S", "{name}");
        assert_eq!(s.revision.as_ref().unwrap().code, sub_code, "{name}");
        let w = &tree.children[1];
        assert_eq!(w.item_number, "PRT-W");
        assert_eq!(w.revision.is_some(), rule == RevisionRule::Latest, "{name}");
        let back = s.children.iter().find(|n| n.item_number == "ASM-R");
        assert_eq!(back.is_some(), cycle, "{name}");
        if let Some(back) = back {
            assert!(back.cycle && back.children.is_empty(), "{name}");
            assert!(!tree.mass_complete, "{name}");
        }
        if rule == RevisionRule::Latest {
            // 1 + (4 + 5) + 5
            assert!(
                (tree.total_mass_kg - 15.0).abs() < 1e-9,
                "{}",
                tree.total_mass_kg
            );
            assert!(tree.mass_complete);
        }
    }

    app.teardown().await;
}
