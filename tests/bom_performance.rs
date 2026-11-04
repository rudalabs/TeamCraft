//! Performance of the dynamic BOM: a large synthetic structure (4094 lines,
//! 11 levels, shared parts) is expanded with a bounded number of queries –
//! one per BOM level – and the results match the expected roll-ups for every
//! revision rule. Also covers the multi-level where-used report.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use teamcraft::{domain::RevisionRule, domain::Role, services::bom};
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
        let tree = bom::tree(&app.pool, root, rule).await.expect("expand");
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
