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

use std::path::PathBuf;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use serde_json::Value;
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
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "integration-test-password";

pub struct TestApp {
    pub router: Router,
    pub pool: PgPool,
    admin_options: PgConnectOptions,
    db_name: String,
    vault_dir: PathBuf,
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

/// Connection to the server on which the temporary databases are created.
fn admin_options() -> Option<PgConnectOptions> {
    if let Err(e) = config::load_local_env_file() {
        panic!("{} could not be parsed: {e}", config::LOCAL_ENV_FILE);
    }
    if let Ok(url) = std::env::var("TEAMCRAFT_TEST_DATABASE_URL") {
        return Some(url.parse().expect("valid TEAMCRAFT_TEST_DATABASE_URL"));
    }
    match Config::from_env() {
        Ok(config) => Some(config.database),
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

/// Like [`setup`], with additional configuration variables.
pub async fn setup_with(extra: &'static [(&'static str, &'static str)]) -> Option<TestApp> {
    let admin_options = admin_options()?;
    let db_name = format!("teamcraft_test_{}", Uuid::new_v4().simple());
    let mut admin = match PgConnection::connect_with(&admin_options).await {
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

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(admin_options.clone().database(&db_name))
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
        router: teamcraft::router(state),
        pool,
        admin_options,
        db_name,
        vault_dir,
    })
}

impl TestApp {
    /// Drops the temporary database and vault.
    pub async fn teardown(self) {
        self.pool.close().await;
        if let Ok(mut admin) = PgConnection::connect_with(&self.admin_options).await {
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
