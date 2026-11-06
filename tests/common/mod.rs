//! Test harness: every test gets its own temporary PostgreSQL database and
//! file vault and talks to the real router (no network involved).
//!
//! The database server is taken from `TEAMCRAFT_TEST_DATABASE_URL` (CI) or,
//! if unset, from the regular database configuration (environment, the
//! optional local configuration file, or the default: the development
//! database of docker-compose.yml). The role needs CREATEDB (the dev
//! container's `teamcraft` user has it). If no database server is reachable
//! the tests print a notice and pass trivially, so `cargo test` works on
//! machines without PostgreSQL; set `TEAMCRAFT_REQUIRE_DB_TESTS=1` (as CI
//! does) to turn that into a failure.

#![allow(dead_code)]

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
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
    Connection, PgConnection, PgPool,
};
use teamcraft::{
    auth::Actor,
    config::{self, Config},
    db,
    domain::Role,
    services::users::{self, NewLocalUser},
    AppState,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "integration-test-password";

pub struct TestApp {
    pub router: Router,
    /// The state behind the router (for services that take it).
    #[allow(dead_code)]
    pub state: AppState,
    pub pool: PgPool,
    admin_options: PgConnectOptions,
    /// Password of the test role, for the PostgreSQL client tools.
    admin_password: Option<String>,
    pub db_name: String,
    pub vault_dir: PathBuf,
    /// The place of this test database among [`db_slots`] (none for a
    /// second app of the same test, see [`TestApp::open`]).
    _slot: Option<OwnedSemaphorePermit>,
}

/// Project groups every test database starts with: (code, name, IdP group).
pub const FIXTURE_PROJECTS: [(&str, &str, &str); 8] = [
    ("ROCKET", "Sounding rocket", "teamcraft-rocket"),
    ("ROVER", "Planetary rover", "teamcraft-rover"),
    ("ARM", "Robot arm", "teamcraft-arm"),
    ("DRONE", "Survey drone", "teamcraft-drone"),
    ("SHOP", "Workshop and tooling", "teamcraft-shop"),
    ("PUMP", "Centrifugal pump", "teamcraft-pump"),
    ("GEARBOX", "Two-stage gearbox", "teamcraft-gearbox"),
    ("CONVEYOR", "Packaging line conveyor", "teamcraft-conveyor"),
];

fn require_db() -> bool {
    std::env::var("TEAMCRAFT_REQUIRE_DB_TESTS").is_ok()
}

pub struct Response {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
}

/// Connection to the server on which the temporary databases are created,
/// and the password of its role (for `pg_dump`/`pg_restore`).
fn admin_options() -> Option<(PgConnectOptions, Option<String>)> {
    if let Err(e) = config::load_local_env_file() {
        panic!("{} could not be parsed: {e}", config::LOCAL_ENV_FILE);
    }
    if let Ok(url) = std::env::var("TEAMCRAFT_TEST_DATABASE_URL") {
        let parsed = Config::from_lookup(|key| match key {
            "TEAMCRAFT_DATABASE_URL" => Some(url.clone()),
            "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
            _ => None,
        })
        .expect("valid TEAMCRAFT_TEST_DATABASE_URL");
        return Some((parsed.database, parsed.database_password));
    }
    match Config::from_env() {
        Ok(config) => Some((config.database, config.database_password)),
        Err(e) => {
            if require_db() {
                panic!("TEAMCRAFT_REQUIRE_DB_TESTS is set but no database is configured: {e:#}");
            }
            eprintln!("skipping database test: invalid database configuration ({e:#})");
            None
        }
    }
}

/// Creates the test application, or `None` if no database server is reachable.
pub async fn setup() -> Option<TestApp> {
    setup_with(&[]).await
}

/// Connections of the pool of one test database.
const POOL_SIZE: u32 = 5;

/// How long a request of a test may wait for a pooled connection. The
/// default 30 s is too short when several test binaries, builds and other
/// checkouts share one machine: a request that cannot get a connection
/// answers `500`, and the test fails for a reason that is not in the code.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(180);

/// How long the harness retries a connection the server refuses for now
/// (too many connections, starting up).
const CONNECT_RETRY: Duration = Duration::from_secs(300);

/// Test databases one test binary keeps open at the same time (default 6,
/// `TEAMCRAFT_TEST_DB_SLOTS` overrides it). The test threads (one per CPU by
/// default) would otherwise open up to `threads × POOL_SIZE` connections,
/// and a few binaries at once exceed PostgreSQL's `max_connections` (100):
/// the server refuses connections and requests fail at random.
fn db_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| {
            let n = std::env::var("TEAMCRAFT_TEST_DB_SLOTS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(6);
            Arc::new(Semaphore::new(n))
        })
        .clone()
}

/// Whether the server refused a connection only for now: too many
/// connections (`53300`) or still starting up (`57P03`). A server whose
/// `lc_messages` is not English (e.g. `German_Germany.1252`) sends that
/// refusal in a non-UTF-8 encoding, which sqlx reports as a protocol error
/// without the code – retried as well (sqlx's own pool does not). On
/// Windows the refusal may also arrive as a reset connection (os error
/// 10054) before the message is read. A server that is not running at all
/// (connection refused) is not retried: the tests are skipped at once.
fn refused_for_now(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db) => matches!(db.code().as_deref(), Some("53300" | "57P03")),
        sqlx::Error::Protocol(message) => message.contains("non-UTF-8"),
        sqlx::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        ),
        _ => false,
    }
}

/// Connects to the test database server, retrying while it refuses
/// connections for now (see [`refused_for_now`]).
async fn connect_retrying(options: &PgConnectOptions) -> Result<PgConnection, sqlx::Error> {
    let deadline = Instant::now() + CONNECT_RETRY;
    let mut wait = Duration::from_millis(50);
    loop {
        match PgConnection::connect_with(options).await {
            Err(e) if refused_for_now(&e) && Instant::now() < deadline => {
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(2));
            }
            result => return result,
        }
    }
}

/// A pool on database `database` of the test server with all of its
/// [`POOL_SIZE`] connections opened up front and kept open: a test never
/// waits for the server to accept a new connection in the middle of a
/// request. While the server refuses connections for now (see
/// [`refused_for_now`]), the pool is closed again – so that tests waiting
/// for connections do not each hold a few – and opened anew a little later.
async fn test_pool(options: &PgConnectOptions, database: &str) -> Result<PgPool, sqlx::Error> {
    let deadline = Instant::now() + CONNECT_RETRY;
    let mut wait = Duration::from_millis(50);
    loop {
        let pool = PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .min_connections(POOL_SIZE)
            .idle_timeout(None)
            .max_lifetime(None)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_lazy_with(options.clone().database(database));
        let mut open = Vec::new();
        // sqlx itself retries a refusal it can read (until the acquire
        // timeout) – too long to hold the connections opened so far
        let refused = loop {
            if open.len() == POOL_SIZE as usize {
                break None;
            }
            match tokio::time::timeout(Duration::from_secs(10), pool.acquire()).await {
                Ok(Ok(conn)) => open.push(conn),
                Ok(Err(e)) if !refused_for_now(&e) => break Some(Err(e)),
                Ok(Err(_)) | Err(_) => break Some(Ok(())),
            }
        };
        drop(open);
        match refused {
            None => return Ok(pool),
            Some(Err(e)) => return Err(e),
            Some(Ok(())) if Instant::now() < deadline => {
                pool.close().await;
                // a little jitter, so waiting tests do not retry in step
                let jitter = Duration::from_millis(u64::from(Uuid::new_v4().as_bytes()[0]));
                tokio::time::sleep(wait + jitter).await;
                wait = (wait * 2).min(Duration::from_secs(2));
            }
            Some(Ok(())) => return Err(sqlx::Error::PoolTimedOut),
        }
    }
}

/// Sends the server's log (warnings and errors, or `RUST_LOG`) to the output
/// of the test, which shows it when the test fails: the cause of a `500` is
/// only logged, never sent to the client.
fn init_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

/// Like [`setup`], with additional configuration variables.
pub async fn setup_with(extra: &'static [(&'static str, &'static str)]) -> Option<TestApp> {
    init_logging();
    let (admin_options, admin_password) = admin_options()?;
    let slot = db_slots()
        .acquire_owned()
        .await
        .expect("the semaphore is never closed");
    let db_name = format!("teamcraft_test_{}", Uuid::new_v4().simple());
    let mut admin = match connect_retrying(&admin_options).await {
        Ok(conn) => conn,
        Err(e) if !require_db() => {
            eprintln!("skipping database test: no database server reachable ({e}) – start one with `docker compose up -d db`");
            return None;
        }
        Err(e) => panic!("connect to the test database server: {e}"),
    };
    sqlx::query(&format!("CREATE DATABASE {db_name}"))
        .execute(&mut admin)
        .await
        .expect("CREATE DATABASE (the test role needs CREATEDB)");
    admin.close().await.ok();

    let pool = test_pool(&admin_options, &db_name)
        .await
        .expect("connect to the temporary database");
    db::migrate(&pool).await.expect("migrations apply");
    for (code, name, group) in FIXTURE_PROJECTS {
        sqlx::query(
            "INSERT INTO projects (id, code, name, description, oidc_group)
             VALUES (gen_random_uuid(), $1, $2, 'Test fixture', $3)",
        )
        .bind(code)
        .bind(name)
        .bind(group)
        .execute(&pool)
        .await
        .expect("create the fixture project groups");
    }

    let vault_dir = std::env::temp_dir().join(format!("teamcraft-test-vault-{}", Uuid::new_v4()));
    let vault = vault_dir.to_string_lossy().to_string();
    // The pool is injected below; the database settings only satisfy validation.
    // `extra` wins over the test defaults (e.g. a larger upload limit).
    let config = Config::from_lookup(move |key| {
        if let Some((_, v)) = extra.iter().find(|(k, _)| *k == key) {
            return Some(v.to_string());
        }
        match key {
            "TEAMCRAFT_VAULT_DIR" => Some(vault.clone()),
            "TEAMCRAFT_MAX_UPLOAD_MB" => Some("1".into()),
            "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
            _ => None,
        }
    })
    .expect("test configuration");
    let state = AppState::new(config, pool.clone())
        .await
        .expect("app state");
    Some(TestApp {
        router: teamcraft::router(state.clone()),
        state,
        pool,
        admin_options,
        admin_password,
        db_name,
        vault_dir,
        _slot: Some(slot),
    })
}

impl TestApp {
    /// Configuration of database `database` on the test server with vault
    /// `vault_dir` – what `teamcraft backup`/`restore` would read.
    pub fn client_config(&self, database: &str, vault_dir: &std::path::Path) -> Config {
        let o = &self.admin_options;
        let vars: Vec<(&str, String)> = vec![
            ("TEAMCRAFT_DB_HOST", o.get_host().to_string()),
            ("TEAMCRAFT_DB_PORT", o.get_port().to_string()),
            ("TEAMCRAFT_DB_USER", o.get_username().to_string()),
            ("TEAMCRAFT_DB_NAME", database.to_string()),
            (
                "TEAMCRAFT_VAULT_DIR",
                vault_dir.to_string_lossy().to_string(),
            ),
            ("TEAMCRAFT_LOCAL_LOGIN", "true".to_string()),
        ];
        let password = self.admin_password.clone();
        Config::from_lookup(move |key| {
            if key == "TEAMCRAFT_DB_PASSWORD" {
                return password.clone();
            }
            vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        })
        .expect("client configuration")
    }

    /// Creates an empty database next to the test database (dropped by
    /// `teardown` of the app returned by [`TestApp::open`]).
    pub async fn create_empty_database(&self, suffix: &str) -> String {
        let name = format!("{}_{suffix}", self.db_name);
        let mut admin = connect_retrying(&self.admin_options)
            .await
            .expect("connect to the test database server");
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut admin)
            .await
            .expect("CREATE DATABASE");
        admin.close().await.ok();
        name
    }

    /// A second application on another database and vault of the same
    /// server (e.g. a restored backup), without migrating or seeding it.
    pub async fn open(&self, database: &str, vault_dir: PathBuf) -> TestApp {
        let pool = test_pool(&self.admin_options, database)
            .await
            .expect("connect to the other database");
        let config = self.client_config(database, &vault_dir);
        let state = AppState::new(config, pool.clone())
            .await
            .expect("app state");
        TestApp {
            router: teamcraft::router(state.clone()),
            state,
            pool,
            admin_options: self.admin_options.clone(),
            admin_password: self.admin_password.clone(),
            db_name: database.to_string(),
            vault_dir,
            _slot: None,
        }
    }

    /// Drops the temporary database and vault.
    pub async fn teardown(self) {
        self.pool.close().await;
        // retried: a database left behind under load stays on the server
        if let Ok(mut admin) = connect_retrying(&self.admin_options).await {
            let _ = sqlx::query(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.db_name
            ))
            .execute(&mut admin)
            .await;
        }
        let _ = std::fs::remove_dir_all(&self.vault_dir);
    }

    /// Creates a local user without project memberships and returns it
    /// together with an API token.
    pub async fn user(&self, username: &str, role: Role) -> (Actor, String) {
        self.user_in(username, role, &[]).await
    }

    /// Creates a local user who is a member of the given project groups
    /// (codes of [`FIXTURE_PROJECTS`], e.g. "ROCKET").
    pub async fn user_in(&self, username: &str, role: Role, projects: &[&str]) -> (Actor, String) {
        let user = users::create_local(
            &self.pool,
            None,
            NewLocalUser {
                username: username.into(),
                display_name: format!("{username} (test)"),
                email: None,
                role,
                password: PASSWORD.into(),
            },
        )
        .await
        .expect("create user");
        for code in projects {
            sqlx::query(
                "INSERT INTO project_members (project_id, user_id)
                 SELECT id, $2 FROM projects WHERE code = $1",
            )
            .bind(code)
            .bind(user.id)
            .execute(&self.pool)
            .await
            .expect("add project membership");
        }
        let actor = self.actor(username).await;
        let (_, token) = users::create_token(&self.pool, &actor, "tests", None)
            .await
            .expect("create token");
        (actor, token)
    }

    /// Loads the actor (incl. memberships) of an existing user.
    pub async fn actor(&self, username: &str) -> Actor {
        let user = users::find_by_username(&self.pool, username)
            .await
            .expect("query user")
            .expect("user exists");
        users::actor(&self.pool, user.id).await.expect("load actor")
    }

    /// The file vault of the test instance.
    pub async fn vault(&self) -> teamcraft::vault::Vault {
        teamcraft::vault::Vault::open(&self.vault_dir)
            .await
            .expect("open the test vault")
    }

    /// Id of a (fixture) project group.
    pub async fn project_id(&self, code: &str) -> Uuid {
        sqlx::query_scalar("SELECT id FROM projects WHERE code = $1")
            .bind(code)
            .fetch_one(&self.pool)
            .await
            .expect("project exists")
    }

    pub async fn send(&self, request: Request<Body>) -> Response {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router call");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body")
            .to_vec();
        Response {
            status,
            headers,
            body,
        }
    }

    /// JSON API call with a bearer token.
    pub async fn api(
        &self,
        method: Method,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        let body = match body {
            Some(json) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&json).unwrap())
            }
            None => Body::empty(),
        };
        self.send(builder.body(body).unwrap()).await
    }

    /// Multipart upload of one file via the API.
    pub async fn api_upload(
        &self,
        uri: &str,
        token: &str,
        filename: &str,
        content: &[u8],
    ) -> Response {
        self.api_multipart(uri, token, &[(filename, content)], &[])
            .await
    }

    /// Multipart request with several `file` parts and text fields.
    pub async fn api_multipart(
        &self,
        uri: &str,
        token: &str,
        files: &[(&str, &[u8])],
        fields: &[(&str, &str)],
    ) -> Response {
        let boundary = "teamcraft-test-boundary";
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        for (filename, content) in files {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        self.send(request).await
    }

    /// Browser-style GET with an optional session cookie.
    pub async fn get_page(&self, uri: &str, cookie: Option<&str>) -> Response {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        if let Some(c) = cookie {
            builder = builder.header(header::COOKIE, c);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    /// Browser-style form POST.
    pub async fn post_form(
        &self,
        uri: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        form: &[(&str, &str)],
    ) -> Response {
        let body = form
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    teamcraft::util::pct_encode(k),
                    teamcraft::util::pct_encode(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, "localhost:8080")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            builder = builder.header(header::COOKIE, c);
        }
        if let Some(o) = origin {
            builder = builder.header(header::ORIGIN, o);
        }
        self.send(builder.body(Body::from(body)).unwrap()).await
    }

    /// Logs in through the login form and returns the session cookie.
    pub async fn login(&self, username: &str) -> String {
        let response = self
            .post_form(
                "/login",
                None,
                Some("http://localhost:8080"),
                &[
                    ("username", username),
                    ("password", PASSWORD),
                    ("next", "/"),
                ],
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::SEE_OTHER,
            "login of {username}: {}",
            response.text()
        );
        let cookie = response
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("tc_session="))
            .expect("session cookie");
        cookie.split(';').next().unwrap().to_string()
    }
}

/// Shorthands for the steps many functional tests repeat.
impl TestApp {
    /// Creates an item through the API (`body` as for `POST /api/v1/items`)
    /// and returns its number.
    pub async fn create_item(&self, token: &str, body: Value) -> String {
        let r = self
            .api(Method::POST, "/api/v1/items", token, Some(body))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "create item: {}", r.text());
        r.json()["item_number"].as_str().unwrap().to_string()
    }

    /// Creates a change order with the in-work revisions of `items` and the
    /// given reviewers (usernames), submits it and returns its number.
    pub async fn submit_change(
        &self,
        author: &str,
        title: &str,
        items: &[&str],
        reviewers: &[&str],
    ) -> String {
        let r = self
            .api(
                Method::POST,
                "/api/v1/changes",
                author,
                Some(json!({ "title": title })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "create change: {}", r.text());
        let number = r.json()["number"].as_str().unwrap().to_string();
        for item in items {
            let r = self
                .api(
                    Method::POST,
                    &format!("/api/v1/changes/{number}/items"),
                    author,
                    Some(json!({ "item": item })),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "add {item}: {}", r.text());
        }
        for reviewer in reviewers {
            let r = self
                .api(
                    Method::POST,
                    &format!("/api/v1/changes/{number}/reviewers"),
                    author,
                    Some(json!({ "username": reviewer })),
                )
                .await;
            assert_eq!(
                r.status,
                StatusCode::OK,
                "reviewer {reviewer}: {}",
                r.text()
            );
        }
        let r = self
            .api(
                Method::POST,
                &format!("/api/v1/changes/{number}/submit"),
                author,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "submit {number}: {}", r.text());
        number
    }

    /// A reviewer's decision on a change.
    pub async fn review(
        &self,
        change: &str,
        token: &str,
        decision: &str,
        comment: &str,
    ) -> Response {
        self.api(
            Method::POST,
            &format!("/api/v1/changes/{change}/review"),
            token,
            Some(json!({ "decision": decision, "comment": comment })),
        )
        .await
    }

    /// Releases the in-work revisions of `items` through a change order that
    /// `reviewer` (username, token) approves; returns the change number.
    pub async fn release(&self, author: &str, reviewer: (&str, &str), items: &[&str]) -> String {
        let number = self
            .submit_change(author, "Release", items, &[reviewer.0])
            .await;
        let r = self.review(&number, reviewer.1, "approve", "").await;
        assert_eq!(r.status, StatusCode::OK, "approve {number}: {}", r.text());
        assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
        number
    }

    /// Actions of the audit log entries about one entity, oldest first.
    pub async fn audit_actions(&self, entity_id: Uuid) -> Vec<String> {
        sqlx::query_scalar("SELECT action FROM audit_log WHERE entity_id = $1 ORDER BY id")
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await
            .expect("read the audit log")
    }
}
