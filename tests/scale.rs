//! Scale check: seeds a large synthetic vault and times the hot paths
//! through the real router (web pages, REST API, MCP).
//!
//! Not part of the normal test run (`#[ignore]`) and not a feature: it is
//! started by hand or by the `scale` workflow (.github/workflows/scale.yml):
//!
//! ```text
//! TEAMCRAFT_TEST_DATABASE_URL=postgres://teamcraft@localhost:5432/teamcraft \
//!   cargo test --release --test scale -- --ignored --nocapture
//! ```
//!
//! * The data lives in a database of its own (`teamcraft_scale`, or
//!   `TEAMCRAFT_SCALE_DB`) on the test server. It is created, migrated and
//!   seeded with `tests/scale/seed.sql` (~100k items, ~1M BOM lines, 2M audit
//!   rows, …) on the first run and kept, so later runs (e.g. after adding an
//!   index migration) only migrate and measure. `DROP DATABASE teamcraft_scale`
//!   starts over.
//! * Every step is called once to warm up and then `TEAMCRAFT_SCALE_RUNS`
//!   times (default 5); the report has p50 and max.
//! * One more call per step goes through a second pool whose connections load
//!   `auto_explain` (needs a superuser, as the CI service container and the
//!   dev container have): PostgreSQL sends the `EXPLAIN (ANALYZE, BUFFERS)`
//!   plan of every statement back as a notice, which is collected here. That
//!   gives the number of queries per request (N+1 loops show up), the slowest
//!   statement and the sequential scans on big tables.
//! * The report goes to `TEAMCRAFT_SCALE_REPORT_DIR` (default
//!   `target/scale-report`): `report.md` (table), `plans.md` (every plan),
//!   `results.json` (for a later comparison: set `TEAMCRAFT_SCALE_BASELINE`
//!   to an earlier `results.json` to get a before/after table).

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    Connection, Executor, PgConnection, PgPool,
};
use teamcraft::{
    config::{self, Config},
    db,
    domain::Role,
    services::users::{self, NewLocalUser},
    AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

const SEED: &str = include_str!("scale/seed.sql");
const BUSY_USER: &str = "scale.busy";
const PASSWORD: &str = "scale-check-password";
/// The page/API budget the scale check aims for (p50 on the CI runner).
const TARGET_MS: f64 = 200.0;

/// Tables whose sequential scans the report points out.
const BIG_TABLES: [&str; 16] = [
    "items",
    "revisions",
    "bom_lines",
    "files",
    "file_versions",
    "file_references",
    "item_references",
    "audit_log",
    "notifications",
    "changes",
    "change_items",
    "change_reviews",
    "impacts",
    "watches",
    "change_requests",
    "change_request_items",
];

// ------------------------------------------------------------ plan capture

/// Plans PostgreSQL sent as notices (auto_explain) since the last drain.
static NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct NoticeCapture;

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.0.is_empty() {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for NoticeCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "sqlx::postgres::notice" {
            return;
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        if visitor.0.contains("plan:") {
            NOTICES.lock().unwrap().push(visitor.0);
        }
    }
}

fn install_notice_capture() {
    use tracing_subscriber::{filter::Targets, layer::SubscriberExt, Layer};
    let filter = Targets::new().with_target("sqlx::postgres::notice", tracing::Level::INFO);
    let subscriber = tracing_subscriber::registry().with(NoticeCapture.with_filter(filter));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn drain_notices() -> Vec<String> {
    std::mem::take(&mut *NOTICES.lock().unwrap())
}

/// One statement of a request as auto_explain reported it.
struct Plan {
    duration_ms: f64,
    text: String,
}

impl Plan {
    fn parse(notice: &str) -> Plan {
        let duration_ms = notice
            .strip_prefix("duration: ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(0.0);
        Plan {
            duration_ms,
            text: notice.to_string(),
        }
    }

    /// Big tables read by a sequential scan, with the rows the scan returned
    /// (all loops).
    fn seq_scans(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for line in self.text.lines() {
            let Some(pos) = line.find("Seq Scan on ") else {
                continue;
            };
            let table = line[pos + 12..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !BIG_TABLES.contains(&table.as_str()) {
                continue;
            }
            let rows = actual_rows(line).unwrap_or(0);
            out.push((table, rows));
        }
        out
    }
}

/// `rows × loops` of an `(actual time=… rows=N loops=M)` node line.
fn actual_rows(line: &str) -> Option<u64> {
    let actual = &line[line.find("(actual")?..];
    // PostgreSQL 18 prints average rows per loop with decimals (rows=0.50)
    let rows: f64 = actual
        .split("rows=")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let loops: f64 = actual
        .split("loops=")
        .nth(1)?
        .trim_end_matches(')')
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((rows * loops).round() as u64)
}

// ------------------------------------------------------------ setup

fn admin_options() -> PgConnectOptions {
    config::load_local_env_file().expect("parse the local configuration file");
    if let Ok(url) = std::env::var("TEAMCRAFT_TEST_DATABASE_URL") {
        return url.parse().expect("valid TEAMCRAFT_TEST_DATABASE_URL");
    }
    Config::from_env()
        .expect("database configuration (TEAMCRAFT_TEST_DATABASE_URL or TEAMCRAFT_DATABASE_URL)")
        .database
}

fn report_dir() -> PathBuf {
    std::env::var_os("TEAMCRAFT_SCALE_REPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/scale-report"))
}

fn app_config() -> Config {
    let vault = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("scale-vault")
        .to_string_lossy()
        .to_string();
    Config::from_lookup(move |key| match key {
        "TEAMCRAFT_VAULT_DIR" => Some(vault.clone()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        _ => None,
    })
    .expect("scale configuration")
}

/// Statements sqlx runs on every new connection of the plan pool: report
/// the plan of every statement as a notice to the client.
const AUTO_EXPLAIN: &str = "LOAD 'auto_explain';
    SET auto_explain.log_min_duration = 0;
    SET auto_explain.log_analyze = on;
    SET auto_explain.log_buffers = on;
    SET auto_explain.log_timing = on;
    SET auto_explain.log_nested_statements = on;
    SET auto_explain.log_level = notice;
    SET client_min_messages = notice;";

struct Seeded {
    steps: Vec<(String, Duration)>,
    reused: bool,
}

/// Creates (if needed), migrates and seeds the scale database.
async fn prepare(admin: &PgConnectOptions, db_name: &str) -> (PgPool, Seeded) {
    let mut conn = PgConnection::connect_with(admin)
        .await
        .expect("connect to the database server");
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(db_name)
            .fetch_one(&mut conn)
            .await
            .expect("look up the scale database");
    if !exists {
        conn.execute(format!("CREATE DATABASE {db_name}").as_str())
            .await
            .expect("CREATE DATABASE (the role needs CREATEDB)");
    }
    conn.close().await.ok();

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(db::session_options(admin.clone().database(db_name)))
        .await
        .expect("connect to the scale database");
    let started = Instant::now();
    db::migrate(&pool).await.expect("migrations apply");
    let mut steps = vec![("migrations".to_string(), started.elapsed())];

    let (seeded, items): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM counters WHERE name = 'scale:seeded'),
                EXISTS(SELECT 1 FROM items)",
    )
    .fetch_one(&pool)
    .await
    .expect("check the seed");
    assert!(
        seeded || !items,
        "{db_name} has items but no complete seed (an earlier seed failed?): drop the database and run again"
    );
    if seeded {
        return (
            pool,
            Seeded {
                steps,
                reused: true,
            },
        );
    }
    if users::find_by_username(&pool, BUSY_USER)
        .await
        .expect("look up the busy user")
        .is_none()
    {
        users::create_local(
            &pool,
            None,
            NewLocalUser {
                username: BUSY_USER.into(),
                display_name: "Busy Engineer".into(),
                email: Some("busy@example.invalid".into()),
                role: Role::Editor,
                password: PASSWORD.into(),
            },
        )
        .await
        .expect("create the busy user");
    }
    let mut conn = pool.acquire().await.expect("seed connection");
    for chunk in SEED.split("\n-- step: ").skip(1) {
        let (name, sql) = chunk.split_once('\n').expect("step name");
        let started = Instant::now();
        sqlx::raw_sql(sql)
            .execute(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("seed step '{name}': {e}"));
        let elapsed = started.elapsed();
        eprintln!("seed: {name} in {elapsed:.1?}");
        steps.push((format!("seed: {name}"), elapsed));
    }
    drop(conn);
    let started = Instant::now();
    sqlx::raw_sql("VACUUM (ANALYZE)")
        .execute(&pool)
        .await
        .expect("VACUUM ANALYZE");
    steps.push(("VACUUM (ANALYZE)".into(), started.elapsed()));
    (
        pool,
        Seeded {
            steps,
            reused: false,
        },
    )
}

// ------------------------------------------------------------ requests

#[derive(Clone, Copy)]
enum Auth {
    /// Browser session cookie of the busy user.
    Web,
    /// Bearer token of the busy user.
    Api,
    None,
}

struct Step {
    name: String,
    method: Method,
    uri: String,
    auth: Auth,
    body: Option<Value>,
    expect: StatusCode,
}

fn get(name: &str, auth: Auth, uri: String) -> Step {
    Step {
        name: name.into(),
        method: Method::GET,
        uri,
        auth,
        body: None,
        expect: StatusCode::OK,
    }
}

fn mcp(name: &str, tool: &str, arguments: Value) -> Step {
    Step {
        name: name.into(),
        method: Method::POST,
        uri: "/api/v1/mcp".into(),
        auth: Auth::Api,
        body: Some(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        })),
        expect: StatusCode::OK,
    }
}

struct Session {
    cookie: String,
    token: String,
}

struct Outcome {
    status: StatusCode,
    bytes: usize,
    body: Vec<u8>,
}

async fn call(router: &Router, session: &Session, step: &Step) -> Outcome {
    let mut builder = Request::builder()
        .method(step.method.clone())
        .uri(&step.uri);
    builder = match step.auth {
        Auth::Web => builder.header(header::COOKIE, &session.cookie),
        Auth::Api => builder.header(header::AUTHORIZATION, format!("Bearer {}", session.token)),
        Auth::None => builder,
    };
    let body = match &step.body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(v).unwrap())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .expect("router call");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body")
        .to_vec();
    Outcome {
        status,
        bytes: body.len(),
        body,
    }
}

/// Why a response does not count as a success, if it does not.
fn failure(step: &Step, outcome: &Outcome) -> Option<String> {
    let snippet = || {
        String::from_utf8_lossy(&outcome.body)
            .chars()
            .take(300)
            .collect::<String>()
    };
    if outcome.status != step.expect {
        return Some(format!(
            "status {} (expected {}): {}",
            outcome.status,
            step.expect,
            snippet()
        ));
    }
    if step.uri == "/api/v1/mcp" {
        let v: Value = serde_json::from_slice(&outcome.body).unwrap_or(Value::Null);
        if v.get("error").is_some() || v["result"]["isError"] == json!(true) {
            return Some(format!("MCP error: {}", snippet()));
        }
    }
    None
}

async fn login(router: &Router) -> String {
    let body = format!("username={BUSY_USER}&password={PASSWORD}&next=%2F");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/login")
                .header(header::HOST, "localhost:8080")
                .header(header::ORIGIN, "http://localhost:8080")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("login");
    assert_eq!(response.status(), StatusCode::SEE_OTHER, "login");
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("tc_session="))
        .and_then(|c| c.split(';').next())
        .expect("session cookie")
        .to_string()
}

async fn uuid_of(pool: &PgPool, sql: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{e}: {sql}"))
}

/// The hot paths: what an engineer, the desktop app and an AI assistant do
/// all day, on the biggest objects of the vault.
async fn steps(pool: &PgPool) -> Vec<Step> {
    let big_item = uuid_of(pool, "SELECT id FROM items WHERE number = 'ASM-000001'").await;
    let big_latest = uuid_of(
        pool,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = 'ASM-000001' ORDER BY r.seq DESC LIMIT 1",
    )
    .await;
    let big_first = uuid_of(
        pool,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = 'ASM-000001' AND r.seq = 1",
    )
    .await;
    let bolt_latest = uuid_of(
        pool,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = 'STD-0001' ORDER BY r.seq DESC LIMIT 1",
    )
    .await;
    let part_latest = uuid_of(
        pool,
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = 'PRT-040000' ORDER BY r.seq DESC LIMIT 1",
    )
    .await;
    let big_change = uuid_of(pool, "SELECT id FROM changes WHERE number = 'ECO-000001'").await;
    let big_folder = uuid_of(pool, "SELECT id FROM projects WHERE code = 'P01'").await;
    let big_code: String = sqlx::query_scalar("SELECT code FROM revisions WHERE id = $1")
        .bind(big_latest)
        .fetch_one(pool)
        .await
        .unwrap();

    use Auth::{Api, Web};
    vec![
        get("readyz", Auth::None, "/readyz".into()),
        // search
        get("search: first page (web)", Web, "/items".into()),
        get("search: 'bracket' (web)", Web, "/items?q=bracket".into()),
        get("search: number (web)", Web, "/items?q=ASM-000123".into()),
        get("search: no hit (web)", Web, "/items?q=zzzqqq".into()),
        get(
            "search: 'solidworks' format (web)",
            Web,
            "/items?q=solidworks".into(),
        ),
        get(
            "search: assemblies in work (web)",
            Web,
            "/items?type=assembly&state=in_work".into(),
        ),
        get("search: page 100 (web)", Web, "/items?page=100".into()),
        get(
            "search: 'steel' (API)",
            Api,
            "/api/v1/items?q=steel&limit=50".into(),
        ),
        // item and revision pages
        get("item page ASM-000001", Web, format!("/items/{big_item}"))
            .expecting(StatusCode::SEE_OTHER),
        get(
            "revision page ASM-000001",
            Web,
            format!("/revisions/{big_latest}"),
        ),
        get(
            "revision page STD-0001 (used 60k times)",
            Web,
            format!("/revisions/{bolt_latest}"),
        ),
        get(
            "revision page PRT-040000",
            Web,
            format!("/revisions/{part_latest}"),
        ),
        get(
            "item API ASM-000001",
            Api,
            "/api/v1/items/ASM-000001".into(),
        ),
        get(
            "item history API STD-0001",
            Api,
            "/api/v1/items/STD-0001/history".into(),
        ),
        // BOM
        get(
            "BOM page ASM-000001 (all levels)",
            Web,
            format!("/revisions/{big_latest}/bom"),
        ),
        get(
            "BOM API ASM-000001 expanded",
            Api,
            format!("/api/v1/items/ASM-000001/revisions/{big_code}/bom?expand=true"),
        ),
        get(
            "BOM API ASM-000001 one level",
            Api,
            format!("/api/v1/items/ASM-000001/revisions/{big_code}/bom"),
        ),
        get(
            "where-used STD-0001 (one level)",
            Api,
            "/api/v1/items/STD-0001/where-used".into(),
        ),
        get(
            "where-used STD-0001 (10 levels)",
            Api,
            "/api/v1/items/STD-0001/where-used?levels=10".into(),
        ),
        get(
            "where-used PRT-040000 (10 levels)",
            Api,
            "/api/v1/items/PRT-040000/where-used?levels=10".into(),
        ),
        get(
            "compare ASM-000001 A/B (one level)",
            Web,
            format!("/revisions/{big_latest}/bom/compare?with={big_first}"),
        ),
        get(
            "compare ASM-000001 A/B (all levels)",
            Web,
            format!("/revisions/{big_latest}/bom/compare?with={big_first}&depth=all"),
        ),
        // the busy user's day
        get("dashboard (busy user)", Web, "/".into()),
        get("worklist API (busy user)", Api, "/api/v1/worklist".into()),
        get("notifications page", Web, "/notifications".into()),
        get(
            "notifications API",
            Api,
            "/api/v1/notifications?limit=50".into(),
        ),
        get("vault explorer: my check-outs", Web, "/vault".into()),
        get(
            "vault explorer: group P01",
            Web,
            format!("/vault?folder={big_folder}"),
        ),
        get(
            "vault explorer: search 'housing'",
            Web,
            "/vault?q=housing".into(),
        ),
        // changes
        get("changes list (web)", Web, "/changes".into()),
        get(
            "change page ECO-000001 (2000 items)",
            Web,
            format!("/changes/{big_change}"),
        ),
        get(
            "change API ECO-000001",
            Api,
            "/api/v1/changes/ECO-000001".into(),
        ),
        get(
            "release readiness ECO-000001",
            Api,
            "/api/v1/changes/ECO-000001/readiness".into(),
        ),
        get(
            "change impact ECO-000001",
            Api,
            "/api/v1/changes/ECO-000001/impact".into(),
        ),
        // MCP
        mcp(
            "MCP search_items 'gear'",
            "search_items",
            json!({ "q": "gear", "limit": 20 }),
        ),
        mcp(
            "MCP get_bom ASM-000001",
            "get_bom",
            json!({ "item": "ASM-000001" }),
        ),
        mcp(
            "MCP where_used STD-0001 (5 levels)",
            "where_used",
            json!({ "item": "STD-0001", "levels": 5 }),
        ),
        // REST lists with paging
        get(
            "REST items offset 0",
            Api,
            "/api/v1/items?limit=50&offset=0".into(),
        ),
        get(
            "REST items offset 90000",
            Api,
            "/api/v1/items?limit=50&offset=90000".into(),
        ),
        get(
            "REST changes offset 0",
            Api,
            "/api/v1/changes?limit=50".into(),
        ),
        get(
            "REST changes offset 15000",
            Api,
            "/api/v1/changes?limit=50&offset=15000".into(),
        ),
        get(
            "REST changes in review",
            Api,
            "/api/v1/changes?status=in_review&limit=50".into(),
        ),
        get("REST open impacts", Api, "/api/v1/impacts".into()),
        get("REST projects", Api, "/api/v1/projects".into()),
    ]
}

impl Step {
    fn expecting(mut self, status: StatusCode) -> Step {
        self.expect = status;
        self
    }
}

// ------------------------------------------------------------ report

struct Measured {
    name: String,
    uri: String,
    p50_ms: f64,
    max_ms: f64,
    bytes: usize,
    error: Option<String>,
    plans: Vec<Plan>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

async fn table_counts(pool: &PgPool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for table in ["users", "projects", "project_members"]
        .into_iter()
        .chain(BIG_TABLES)
    {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .expect("count rows");
        out.push((table.to_string(), n));
    }
    out
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|")
}

#[allow(clippy::too_many_arguments)]
fn write_report(
    dir: &std::path::Path,
    server: &str,
    seeded: &Seeded,
    counts: &[(String, i64)],
    runs: usize,
    plans_enabled: Option<&str>,
    measured: &[Measured],
    baseline: &BTreeMap<String, f64>,
) {
    std::fs::create_dir_all(dir).expect("create the report directory");
    let mut md = String::new();
    let _ = writeln!(md, "# TeamCraft scale check\n");
    let _ = writeln!(
        md,
        "PostgreSQL: {server}. Each step: one warm-up call, then {runs} timed calls \
         through the router (handler, services, SQL, template rendering / JSON). \
         Target: p50 < {TARGET_MS} ms.\n"
    );
    match plans_enabled {
        None => {
            let _ = writeln!(
                md,
                "Queries, slowest statement and sequential scans: one extra call per step \
                 with auto_explain (EXPLAIN ANALYZE, BUFFERS of every statement, see plans.md).\n"
            );
        }
        Some(why) => {
            let _ = writeln!(md, "Plans not collected: {why}\n");
        }
    }
    let base = !baseline.is_empty();
    let _ = writeln!(
        md,
        "| Step | {}p50 ms | max ms | queries | slowest stmt ms | seq scans (rows) | KiB |",
        if base { "before p50 ms | " } else { "" }
    );
    let _ = writeln!(
        md,
        "|---|{}---:|---:|---:|---:|---|---:|",
        if base { "---:|" } else { "" }
    );
    for m in measured {
        let flag = if m.error.is_some() {
            " **(failed)**"
        } else if m.p50_ms >= TARGET_MS {
            " ⚠"
        } else {
            ""
        };
        let slowest = m
            .plans
            .iter()
            .map(|p| p.duration_ms)
            .fold(0.0_f64, f64::max);
        let mut scans: BTreeMap<String, u64> = BTreeMap::new();
        for (table, rows) in m.plans.iter().flat_map(Plan::seq_scans) {
            *scans.entry(table).or_default() += rows;
        }
        let scans = scans
            .iter()
            .map(|(t, r)| format!("{t} ({r})"))
            .collect::<Vec<_>>()
            .join(", ");
        let before = if base {
            match baseline.get(&m.name) {
                Some(b) => format!("{b:.1} | "),
                None => "– | ".into(),
            }
        } else {
            String::new()
        };
        let _ = writeln!(
            md,
            "| {}{flag} | {before}{:.1} | {:.1} | {} | {:.1} | {} | {:.0} |",
            md_escape(&m.name),
            m.p50_ms,
            m.max_ms,
            if plans_enabled.is_none() {
                m.plans.len().to_string()
            } else {
                "–".into()
            },
            slowest,
            if scans.is_empty() {
                "–".into()
            } else {
                scans
            },
            m.bytes as f64 / 1024.0
        );
    }
    let failed: Vec<_> = measured.iter().filter(|m| m.error.is_some()).collect();
    if !failed.is_empty() {
        let _ = writeln!(md, "\n## Failed steps\n");
        for m in failed {
            let _ = writeln!(
                md,
                "* {} (`{}`): {}",
                m.name,
                m.uri,
                m.error.as_deref().unwrap_or("")
            );
        }
    }
    let _ = writeln!(md, "\n## Dataset\n\n| Table | Rows |\n|---|---:|");
    for (table, n) in counts {
        let _ = writeln!(md, "| {table} | {n} |");
    }
    let _ = writeln!(
        md,
        "\n## Preparation{}\n\n| Step | Seconds |\n|---|---:|",
        if seeded.reused {
            " (existing seed reused)"
        } else {
            ""
        }
    );
    for (name, d) in &seeded.steps {
        let _ = writeln!(md, "| {name} | {:.1} |", d.as_secs_f64());
    }
    std::fs::write(dir.join("report.md"), &md).expect("write report.md");

    let mut plans = String::from("# Query plans of the scale check\n\n");
    let _ = writeln!(
        plans,
        "Every statement of one call per step, EXPLAIN (ANALYZE, BUFFERS) via auto_explain, \
         slowest first.\n"
    );
    for m in measured {
        let _ = writeln!(
            plans,
            "## {}\n\n`{}` – {} statements\n",
            m.name,
            m.uri,
            m.plans.len()
        );
        let mut sorted: Vec<&Plan> = m.plans.iter().collect();
        sorted.sort_by(|a, b| b.duration_ms.total_cmp(&a.duration_ms));
        for p in sorted {
            let _ = writeln!(plans, "```text\n{}\n```\n", p.text.trim_end());
        }
    }
    std::fs::write(dir.join("plans.md"), plans).expect("write plans.md");

    let results: Vec<Value> = measured
        .iter()
        .map(|m| {
            json!({
                "name": m.name, "uri": m.uri, "p50_ms": m.p50_ms, "max_ms": m.max_ms,
                "queries": m.plans.len(), "bytes": m.bytes, "error": m.error,
            })
        })
        .collect();
    std::fs::write(
        dir.join("results.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .expect("write results.json");
    print!("{md}");
}

fn load_baseline() -> BTreeMap<String, f64> {
    let Some(path) = std::env::var_os("TEAMCRAFT_SCALE_BASELINE") else {
        return BTreeMap::new();
    };
    let raw = std::fs::read(&path).expect("read TEAMCRAFT_SCALE_BASELINE");
    let rows: Vec<Value> = serde_json::from_slice(&raw).expect("baseline is results.json");
    rows.iter()
        .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["p50_ms"].as_f64()?)))
        .collect()
}

// ------------------------------------------------------------ the check

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "scale check: seeds ~5M rows; run with --ignored (see the module docs)"]
async fn scale_check() {
    install_notice_capture();
    let admin = admin_options();
    let db_name = std::env::var("TEAMCRAFT_SCALE_DB").unwrap_or_else(|_| "teamcraft_scale".into());
    assert!(
        db_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "TEAMCRAFT_SCALE_DB must be a plain lower-case name"
    );
    let runs: usize = std::env::var("TEAMCRAFT_SCALE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .max(1);
    let (pool, seeded) = prepare(&admin, &db_name).await;
    let server: String = sqlx::query_scalar(
        "SELECT current_setting('server_version') || ' (shared_buffers '
                || current_setting('shared_buffers') || ', work_mem '
                || current_setting('work_mem') || ', effective_cache_size '
                || current_setting('effective_cache_size') || ', random_page_cost '
                || current_setting('random_page_cost') || '), database size '
                || pg_size_pretty(pg_database_size(current_database()))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let counts = table_counts(&pool).await;

    let router = teamcraft::router(AppState::new(app_config(), pool.clone()).await.unwrap());
    let cookie = login(&router).await;
    let actor = {
        let user = users::find_by_username(&pool, BUSY_USER)
            .await
            .unwrap()
            .expect("busy user");
        users::actor(&pool, user.id).await.unwrap()
    };
    let (_, token) = users::create_token(&pool, &actor, "scale check", None)
        .await
        .expect("token");
    let session = Session { cookie, token };

    // The plan pool: auto_explain on every connection (superuser only).
    let plan_pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute(AUTO_EXPLAIN).await?;
                Ok(())
            })
        })
        .connect_with(db::session_options(admin.clone().database(&db_name)))
        .await;
    let (plan_router, plans_disabled) = match plan_pool {
        Ok(p) => (
            Some(teamcraft::router(
                AppState::new(app_config(), p).await.unwrap(),
            )),
            None,
        ),
        Err(e) => (None, Some(format!("auto_explain not available ({e})"))),
    };

    let mut measured = Vec::new();
    for step in steps(&pool).await {
        let warm = call(&router, &session, &step).await;
        let error = failure(&step, &warm);
        let mut times = Vec::with_capacity(runs);
        if error.is_none() {
            for _ in 0..runs {
                let started = Instant::now();
                let outcome = call(&router, &session, &step).await;
                times.push(ms(started.elapsed()));
                assert_eq!(outcome.status, warm.status, "{}", step.name);
            }
        }
        times.sort_by(f64::total_cmp);
        let plans = match &plan_router {
            Some(r) => {
                drain_notices();
                call(r, &session, &step).await;
                drain_notices().iter().map(|n| Plan::parse(n)).collect()
            }
            None => Vec::new(),
        };
        eprintln!(
            "{:<45} p50 {:>8.1} ms  max {:>8.1} ms  {:>4} queries{}",
            step.name,
            median(&times),
            times.last().copied().unwrap_or(0.0),
            plans.len(),
            error
                .as_deref()
                .map(|e| format!("  FAILED: {e}"))
                .unwrap_or_default()
        );
        measured.push(Measured {
            name: step.name.clone(),
            uri: step.uri.clone(),
            p50_ms: median(&times),
            max_ms: times.last().copied().unwrap_or(0.0),
            bytes: warm.bytes,
            error,
            plans,
        });
    }

    write_report(
        &report_dir(),
        &server,
        &seeded,
        &counts,
        runs,
        plans_disabled.as_deref(),
        &measured,
        &load_baseline(),
    );
    let failed: Vec<_> = measured
        .iter()
        .filter(|m| m.error.is_some())
        .map(|m| m.name.as_str())
        .collect();
    assert!(failed.is_empty(), "failed steps: {failed:?}");
}
