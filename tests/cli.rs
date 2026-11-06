//! The `teamcraft` command line, run as the real binary against a test
//! database: `migrate`, `user create|set-password|list`, `vault check`,
//! `rescan-references`, `assistant check`, `healthcheck`, the `mcp` bridge
//! and `preview-mesh` – their output, their exit codes and their refusals.
//! The binary gets only the configuration given here (no files: it runs in
//! an empty directory, and inherited `TEAMCRAFT_*` variables are removed).

mod common;

use std::{path::Path, process::Stdio};

use axum::http::StatusCode;
use serde_json::{json, Value};
use teamcraft::domain::Role;
use tokio::{io::AsyncWriteExt, process::Command};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Runs `teamcraft <args>` with `env` and `stdin`.
async fn run(env: &[(String, String)], args: &[&str], stdin: &[u8]) -> Run {
    let dir = std::env::temp_dir().join(format!("teamcraft-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_teamcraft"));
    for (key, _) in std::env::vars() {
        if key.starts_with("TEAMCRAFT_") || key.starts_with("MATTERMOST_") || key == "RUST_LOG" {
            cmd.env_remove(key);
        }
    }
    cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .args(args)
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("start teamcraft");
    let mut input = child.stdin.take().unwrap();
    // a big input may not be read completely (the bridge skips it)
    let _ = input.write_all(stdin).await;
    drop(input);
    let out = child.wait_with_output().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn env(app: &common::TestApp) -> Vec<(String, String)> {
    app.cli_env(&app.db_name, &app.vault_dir)
}

fn with(mut env: Vec<(String, String)>, key: &str, value: &str) -> Vec<(String, String)> {
    env.retain(|(k, _)| k != key);
    env.push((key.to_string(), value.to_string()));
    env
}

fn password_line() -> Vec<u8> {
    format!("{}\n", common::PASSWORD).into_bytes()
}

#[tokio::test]
async fn migrate_and_local_accounts() {
    let app = app_or_skip!();
    // an empty database gets the schema, a second run changes nothing
    let empty = app.create_empty_database("cli").await;
    let empty_env = app.cli_env(&empty, &app.vault_dir);
    for _ in 0..2 {
        let r = run(&empty_env, &["migrate"], b"").await;
        assert_eq!(r.code, 0, "{}", r.stderr);
        assert_eq!(r.stdout.trim(), "database schema is up to date");
    }
    let other = app.open(&empty, app.vault_dir.clone()).await;
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'items'",
    )
    .fetch_one(&other.pool)
    .await
    .unwrap();
    assert_eq!(tables, 1);
    other.teardown().await;

    // a broken configuration is refused before anything happens
    let r = run(
        &with(env(&app), "TEAMCRAFT_DB_PORT", "not-a-port"),
        &["migrate"],
        b"",
    )
    .await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("invalid configuration"), "{}", r.stderr);

    // user create: role, groups, password from stdin
    let e = env(&app);
    let r = run(
        &e,
        &[
            "user",
            "create",
            "--username",
            "Max",
            "--display-name",
            "Max Muster",
            "--email",
            "max@example.org",
            "--role",
            "admin",
            "--project",
            "rocket",
            "--project",
            "PUMP",
        ],
        &password_line(),
    )
    .await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout.trim(),
        "created user 'max' with role admin in project group(s) ROCKET, PUMP"
    );
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT p.code FROM project_members m JOIN projects p ON p.id = m.project_id
         JOIN users u ON u.id = m.user_id WHERE u.username = 'max' ORDER BY p.code",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(groups, ["PUMP", "ROCKET"]);
    let cookie = app.login("max").await;
    assert!(!cookie.is_empty(), "the password from stdin works");

    // refusals: nothing is created, the reason is on stderr
    let refusals: [(&[&str], &[u8], &str); 5] = [
        (
            &["user", "create", "--username", "max"],
            b"another-password-1\n",
            "already",
        ),
        (
            &["user", "create", "--username", "zoe", "--role", "owner"],
            b"another-password-1\n",
            "role must be viewer, editor or admin",
        ),
        (
            &["user", "create", "--username", "zoe", "--project", "NOPE"],
            b"another-password-1\n",
            "unknown project group 'NOPE'",
        ),
        (
            &["user", "create", "--username", "zoe"],
            b"",
            "no password provided on stdin",
        ),
        (&["user", "create", "--username", "zoe"], b"short\n", ""),
    ];
    for (args, stdin, message) in refusals {
        let r = run(&e, args, stdin).await;
        assert_eq!(r.code, 1, "{args:?}: {}", r.stdout);
        assert!(
            r.stderr.to_lowercase().contains(&message.to_lowercase()),
            "{args:?}: {}",
            r.stderr
        );
    }
    let zoe: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = 'zoe'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(zoe, 0);

    // set-password signs out the existing sessions; the old password is gone
    let r = run(
        &e,
        &["user", "set-password", "--username", "max"],
        b"a-new-password-for-max\n",
    )
    .await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("existing sessions were signed out"));
    let page = app.get_page("/", Some(&cookie)).await;
    assert_ne!(page.status, StatusCode::OK, "the old session is gone");
    let r = app
        .post_form(
            "/login",
            None,
            Some("http://localhost:8080"),
            &[("username", "max"), ("password", common::PASSWORD)],
        )
        .await;
    assert!(
        r.header("set-cookie")
            .is_none_or(|c| !c.starts_with("tc_session=")),
        "the old password no longer signs in"
    );
    let r = run(
        &e,
        &["user", "set-password", "--username", "nobody"],
        b"x\n",
    )
    .await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("no user named 'nobody'"), "{}", r.stderr);

    // list: a header and one line per account
    app.user("vera", Role::Viewer).await;
    let r = run(&e, &["user", "list"], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert!(lines[0].starts_with("USERNAME"), "{}", r.stdout);
    let max = lines.iter().find(|l| l.starts_with("max ")).unwrap();
    assert!(
        max.contains("admin") && max.contains("local") && max.contains("true"),
        "{max}"
    );
    assert!(max.ends_with("Max Muster"));
    assert!(lines.iter().any(|l| l.starts_with("vera ")));
    app.teardown().await;
}

#[tokio::test]
async fn unknown_environment_variables_are_warned_about() {
    let app = app_or_skip!();
    // a typo is ignored, but named with the nearest known variable
    let typo = with(env(&app), "TEAMCRAFT_PUBLC_URL", "http://example.invalid");
    let r = run(&typo, &["user", "list"], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stderr.contains("TEAMCRAFT_PUBLC_URL") && r.stderr.contains("TEAMCRAFT_PUBLIC_URL"),
        "{}",
        r.stderr
    );
    assert!(!r.stdout.contains("TEAMCRAFT_PUBLC_URL"), "{}", r.stdout);
    // the _FILE form is checked by its base name
    let typo_file = with(env(&app), "TEAMCRAFT_DB_PASWORD_FILE", "/nonexistent");
    let r = run(&typo_file, &["user", "list"], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stderr.contains("TEAMCRAFT_DB_PASSWORD_FILE"),
        "{}",
        r.stderr
    );
    // known names (also as _FILE) and tooling variables stay quiet
    let quiet = with(
        with(env(&app), "TEAMCRAFT_INSTANCE_NAME", "cli test"),
        "TEAMCRAFT_REQUIRE_DB_TESTS",
        "1",
    );
    let r = run(&quiet, &["user", "list"], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        !r.stderr.contains("unknown environment variable"),
        "{}",
        r.stderr
    );
    // a warning does not hide a real configuration error
    let both = with(typo, "TEAMCRAFT_DB_PORT", "not-a-port");
    let r = run(&both, &["user", "list"], b"").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("invalid configuration"), "{}", r.stderr);
}

#[tokio::test]
async fn vault_check_finds_missing_and_corrupt_files() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let number = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let r = app
        .api_multipart(
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            &token,
            &[("fin.step", b"STEP fin"), ("fin.pdf", b"%PDF drawing")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let e = env(&app);

    let r = run(&e, &["vault", "check"], b"").await;
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains("checked 2 blob(s): 0 missing, hashes not verified (use --verify-hashes)"),
        "{}",
        r.stdout
    );
    let r = run(&e, &["vault", "check", "--verify-hashes"], b"").await;
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("0 missing, 0 corrupt"), "{}", r.stdout);

    // damage one blob, remove the other
    let hashes: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT sha256 FROM files ORDER BY sha256")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    let vault = app.vault().await;
    let (damaged, gone) = (&hashes[0], &hashes[1]);
    std::fs::write(vault.object_path(damaged).unwrap(), b"something else").unwrap();
    std::fs::remove_file(vault.object_path(gone).unwrap()).unwrap();

    let r = run(&e, &["vault", "check"], b"").await;
    assert_eq!(r.code, 1, "a missing file fails the check");
    assert!(
        r.stdout.contains(&format!("MISSING  {gone}")),
        "{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("CORRUPT"),
        "not verified without the flag"
    );
    let r = run(&e, &["vault", "check", "--verify-hashes"], b"").await;
    assert_eq!(r.code, 1);
    assert!(
        r.stdout.contains(&format!("CORRUPT  {damaged}")),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("1 missing, 1 corrupt"), "{}", r.stdout);

    // rescan-references: only formats that are scanned
    let r = run(&e, &["rescan-references", "--format", "pdf"], b"").await;
    assert_eq!(r.code, 1);
    assert!(
        r.stderr
            .contains("'pdf' is not a format that is scanned for references"),
        "{}",
        r.stderr
    );
    let r = run(&e, &["rescan-references"], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout.trim(),
        "rescanned 0 file version(s) (solidworks-assembly, solidworks-drawing)"
    );
    app.teardown().await;
}

/// A one-shot HTTP server on a local port that answers `status` to anything.
fn http_once(status: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::{Read, Write};
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(format!("HTTP/1.0 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes());
        }
    });
    addr
}

#[tokio::test]
async fn commands_without_a_database() {
    let none: Vec<(String, String)> = Vec::new();
    // healthcheck: 200 is healthy, anything else or nothing is not
    let r = run(&none, &["healthcheck", "--addr", &http_once("200 OK")], b"").await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    let r = run(
        &none,
        &[
            "healthcheck",
            "--addr",
            &http_once("503 Service Unavailable"),
        ],
        b"",
    )
    .await;
    assert_eq!(r.code, 1);
    assert!(
        r.stderr.contains("/healthz did not return 200"),
        "{}",
        r.stderr
    );
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let r = run(&none, &["healthcheck", "--addr", &closed], b"").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.starts_with("unhealthy:"), "{}", r.stderr);
    // the address may also come from TEAMCRAFT_BIND_ADDR; 0.0.0.0 means here
    let port = http_once("200 OK").rsplit(':').next().unwrap().to_string();
    let r = run(
        &[("TEAMCRAFT_BIND_ADDR".into(), format!("0.0.0.0:{port}"))],
        &["healthcheck"],
        b"",
    )
    .await;
    assert_eq!(r.code, 0, "{}", r.stderr);

    // assistant check without an assistant
    let r = run(&none, &["assistant", "check"], b"").await;
    assert_eq!(r.code, 1);
    assert!(
        r.stdout.contains("The assistant is not configured"),
        "{}",
        r.stdout
    );

    // preview-mesh by hand
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cube.step");
    let fixture = fixture.to_str().unwrap();
    let r = run(
        &none,
        &["preview-mesh", "--format", "step", "--input", fixture],
        b"",
    )
    .await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("give --output and/or --signature"));
    let out = std::env::temp_dir().join(format!("cli-cube-{}.tcm", uuid::Uuid::new_v4()));
    let r = run(
        &none,
        &[
            "preview-mesh",
            "--format",
            "step",
            "--input",
            fixture,
            "--output",
            out.to_str().unwrap(),
        ],
        b"",
    )
    .await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("triangles"), "{}", r.stdout);
    assert!(std::fs::metadata(&out).unwrap().len() > 0);
    let _ = std::fs::remove_file(&out);

    // text-extract refuses what is not a PDF
    let r = run(&none, &["text-extract"], b"not a pdf at all").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.starts_with("Error:"), "{}", r.stderr);

    // the MCP bridge needs a token and an http(s) server
    let r = run(&none, &["mcp"], b"").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("set TEAMCRAFT_TOKEN"), "{}", r.stderr);
    let token = [("TEAMCRAFT_TOKEN".to_string(), "tc_pat_x".to_string())];
    let r = run(&token, &["mcp", "--server", "pdm:8080"], b"").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("must start with http"), "{}", r.stderr);
    let r = run(&none, &["mcp", "--token-file", "does-not-exist.txt"], b"").await;
    assert_eq!(r.code, 1);
    assert!(
        r.stderr.contains("cannot read the token file"),
        "{}",
        r.stderr
    );
    let blank = [("TEAMCRAFT_TOKEN".to_string(), "two words".to_string())];
    let r = run(&blank, &["mcp"], b"").await;
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("empty or malformed"), "{}", r.stderr);
}

#[tokio::test]
async fn the_mcp_bridge_forwards_requests_and_reports_errors() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.create_item(
        &token,
        json!({ "item_type": "part", "name": "Bridge bolt", "project": "ROCKET" }),
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let router = app.router.clone();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": { "name": "cli-test", "version": "1" } } })
        .to_string(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        String::new(),
        json!({ "jsonrpc": "2.0", "id": "two", "method": "tools/list" }).to_string(),
        "x".repeat(4 * 1024 * 1024 + 1),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "search_items", "arguments": { "query": "Bridge" } } })
        .to_string(),
    ]
    .join("\n")
        + "\n";
    let e = [
        ("TEAMCRAFT_TOKEN".to_string(), token.clone()),
        ("TEAMCRAFT_SERVER".to_string(), format!("{server}/")),
    ];
    let r = run(&e, &["mcp"], input.as_bytes()).await;
    assert_eq!(r.code, 0, "{}", r.stderr);
    let answers: Vec<Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // one line per answer: initialize, tools/list, the refusal, the tool call
    assert_eq!(answers.len(), 4, "{}", r.stdout);
    assert_eq!(answers[0]["id"], 1);
    assert!(
        answers[0]["result"]["serverInfo"].is_object(),
        "{}",
        answers[0]
    );
    assert_eq!(answers[1]["id"], "two");
    assert!(answers[1]["result"]["tools"].as_array().unwrap().len() > 5);
    assert_eq!(answers[2]["error"]["code"], -32600);
    assert_eq!(answers[2]["id"], Value::Null);
    assert!(
        r.stderr.contains("skipped a message longer than"),
        "{}",
        r.stderr
    );
    assert_eq!(answers[3]["id"], 3);
    assert!(
        answers[3].to_string().contains("PRT-000001"),
        "{}",
        answers[3]
    );
    assert!(!r.stderr.contains(&token), "the token is never printed");

    // a rejected token: requests get an error answer, notifications none
    let bad = [
        (
            "TEAMCRAFT_TOKEN".to_string(),
            "tc_pat_not-a-real-token".to_string(),
        ),
        ("TEAMCRAFT_SERVER".to_string(), server.clone()),
    ];
    let input = format!(
        "{}\n{}\n",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" })
    );
    let r = run(&bad, &["mcp"], input.as_bytes()).await;
    assert_eq!(r.code, 0);
    let answers: Vec<Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(answers.len(), 1, "{}", r.stdout);
    assert_eq!(answers[0]["id"], 7);
    assert_eq!(answers[0]["error"]["code"], -32000);
    assert!(
        answers[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rejected the token (401)"),
        "{}",
        answers[0]
    );
    assert!(!r.stderr.contains("not-a-real-token"));

    // an unreachable server
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let input = format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" })
    );
    let r = run(
        &[
            ("TEAMCRAFT_TOKEN".to_string(), token.clone()),
            ("TEAMCRAFT_SERVER".to_string(), closed),
        ],
        &["mcp"],
        input.as_bytes(),
    )
    .await;
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("not reachable"), "{}", r.stdout);
    app.teardown().await;
}
