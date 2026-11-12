//! End-to-end tests of the HTML user interface (login, pages, CSRF guard).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn health_endpoints() {
    let app = app_or_skip!();
    let r = app.get_page("/healthz", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.text(), "ok");
    let r = app.get_page("/readyz", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["database"], "ok");
    assert_eq!(r.json()["vault"], "ok");
    app.teardown().await;
}

#[tokio::test]
async fn anonymous_users_are_sent_to_login() {
    let app = app_or_skip!();
    let r = app.get_page("/items?q=x", None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some("/login?next=%2Fitems%3Fq%3Dx")
    );
    let r = app.get_page("/login", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("Sign in"));
    assert!(r.text().contains("data-skin=\"teamcraft\""), "default skin");
    // the remembered skin also styles the login page
    let r = app.get_page("/login", Some("tc_skin=opensuse")).await;
    assert!(r.text().contains("data-skin=\"opensuse\""));
    let r = app.get_page("/login", Some("tc_skin=<script>")).await;
    assert!(
        r.text().contains("data-skin=\"teamcraft\""),
        "unknown skins fall back"
    );
    // self-hosted fonts
    for font in [
        "montserrat-latin-wght-normal.woff2",
        "oswald-latin-ext-wght-normal.woff2",
    ] {
        let r = app
            .get_page(&format!("/static/shared/fonts/{font}"), None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{font}");
        assert_eq!(r.header("content-type").as_deref(), Some("font/woff2"));
        assert_eq!(&r.body[..4], b"wOF2");
    }
    let r = app.get_page("/static/shared/../../Cargo.toml", None).await;
    assert_ne!(r.status, StatusCode::OK);

    // Cache busting: pages link the current asset version, which may be
    // cached for good; anything else is revalidated with the ETag.
    let v = teamcraft::assets::version();
    let html = app.get_page("/login", None).await.text();
    assert!(
        html.contains(&format!("/static/style.css?v={v}")),
        "stylesheet link carries the asset version"
    );
    assert!(html.contains(&format!("TeamCraft_Logo_White.svg?v={v}")));
    assert!(html.contains(&format!("/static/shared/css/skins.css?v={v}")));
    // shared scripts: versioned path, immutable for the current version
    let r = app
        .get_page(&format!("/static/shared/v/{v}/js/viewer.js"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("cache-control").unwrap().contains("immutable"));
    let r = app
        .get_page("/static/shared/v/000000000000/js/viewer.js", None)
        .await;
    assert_eq!(r.header("cache-control").as_deref(), Some("no-cache"));
    let r = app
        .get_page(&format!("/static/style.css?v={v}"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("cache-control").unwrap().contains("immutable"));
    let r = app.get_page("/static/style.css?v=0.1.0", None).await;
    assert_eq!(
        r.header("cache-control").as_deref(),
        Some("no-cache"),
        "outdated links are revalidated"
    );
    let etag = r.header("etag").expect("etag");
    let r = app
        .send(
            axum::http::Request::builder()
                .uri("/static/style.css")
                .header("if-none-match", &etag)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());
    // embedded media
    let r = app
        .get_page("/static/media/logos/TeamCraft_Icon.svg", None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("image/svg+xml"));
    let r = app
        .get_page("/static/media/icons/Green_tick.svg", None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = app.get_page("/static/media/../Cargo.toml", None).await;
    assert_ne!(r.status, StatusCode::OK);
    let r = app.get_page("/favicon.ico", None).await;
    assert_eq!(r.status, StatusCode::OK);
    // static assets and security headers
    let r = app.get_page("/static/style.css", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert!(r
        .header("content-security-policy")
        .unwrap()
        .contains("default-src 'self'"));
    app.teardown().await;
}

/// Binary STL with one triangle (80-byte header, count, 50-byte facet).
fn binary_stl() -> Vec<u8> {
    let mut stl = b"binary STL exported by CAD".to_vec();
    stl.resize(80, 0);
    stl.extend_from_slice(&1u32.to_le_bytes());
    for v in [
        0.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0, 0.0,
    ] {
        stl.extend_from_slice(&v.to_le_bytes());
    }
    stl.extend_from_slice(&[0, 0]);
    stl
}

#[tokio::test]
async fn three_d_preview_pages_and_meshes() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("cad", Role::Editor, &["ROCKET"]).await;
    app.user("vera", Role::Viewer).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev_id = r.json()["id"].as_str().unwrap().to_string();
    let files_uri = "/api/v1/items/PRT-000001/revisions/A/files";
    let r = app
        .api_multipart(
            files_uri,
            &token,
            &[
                ("fin.stl", b"solid fin\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid fin\n"),
                ("fin.step", include_bytes!("fixtures/cube.step")),
                ("broken.stp", b"ISO-10303-21; not really a step file"),
                ("notes.txt", b"plain text"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id_of = |name: &str| {
        r.json()
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .map(|f| f["id"].as_str().unwrap().to_string())
            .unwrap()
    };
    let (stl, step, broken, txt) = (
        id_of("fin.stl"),
        id_of("fin.step"),
        id_of("broken.stp"),
        id_of("notes.txt"),
    );
    // a second (binary) version of the STL
    let r = app
        .api_upload(files_uri, &token, "fin.stl", &binary_stl())
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()[0]["version"], 2);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{stl}/versions"),
            &token,
            None,
        )
        .await;
    let v2 = r.json()[0]["id"].as_str().unwrap().to_string();
    let v1 = r.json()[1]["id"].as_str().unwrap().to_string();

    // Viewers may look at previews; anonymous users are sent to the login.
    let r = app.get_page(&format!("/files/{stl}/preview"), None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let cookie = app.login("vera").await;
    let r = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await;
    assert!(r.text().contains(&format!("/files/{stl}/preview")));
    assert!(r.text().contains(&format!("/file-versions/{v1}/preview")));
    assert!(!r.text().contains(&format!("/files/{txt}/preview")));

    // current version
    let r = app
        .get_page(&format!("/files/{stl}/preview"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let html = r.text();
    assert!(html.contains(&format!("data-src=\"/file-versions/{v2}/mesh\"")));
    assert!(!html.contains("older version"));
    let csp = r.header("content-security-policy").unwrap();
    assert!(
        csp.contains("script-src 'self'") && !csp.contains("unsafe-eval"),
        "pages keep the strict CSP: {csp}"
    );
    // the viewer gets exactly that version as a server-side mesh
    let r = app
        .get_page(&format!("/file-versions/{v2}/mesh"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // not cached for good: access can be withdrawn (restricted project groups)
    assert!(r.header("cache-control").unwrap().contains("no-cache"));
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).expect("TCMESH01");
    assert_eq!(mesh.triangle_count(), 1);
    assert_eq!(mesh.bounds()[3], 10.0, "the binary v2 (10 units wide)");
    let r = app
        .get_page(&format!("/file-versions/{v1}/mesh"), Some(&cookie))
        .await;
    assert_eq!(
        teamcraft::mesh::Mesh::decode(&r.body).unwrap().bounds()[3],
        1.0
    );
    // an older version
    let r = app
        .get_page(&format!("/file-versions/{v1}/preview"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("older version"));
    assert!(r.text().contains(&format!("/file-versions/{v1}/mesh")));
    // no preview for other file types
    let r = app
        .get_page(&format!("/files/{txt}/preview"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .get_page(&format!("/files/{txt}/mesh"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]["message"].as_str().is_some(),
        "the viewer gets a JSON message, not an HTML page"
    );

    // STEP is tessellated on the server (pure Rust B-rep kernel)
    let r = app
        .get_page(&format!("/files/{step}/preview"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("tessellated"));
    let r = app
        .get_page(&format!("/files/{step}/mesh"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cube = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert!(cube.triangle_count() >= 12, "{}", cube.triangle_count());
    // a broken STEP file: an explanation, also on the second attempt (cached)
    for _ in 0..2 {
        let r = app
            .get_page(&format!("/files/{broken}/mesh"), Some(&cookie))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        assert!(r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("STEP"));
    }
    // the same mesh through the REST API (desktop app)
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{step}/mesh"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(teamcraft::mesh::Mesh::decode(&r.body).unwrap(), cube);
    let r = app
        .get_page(
            &format!(
                "/static/shared/v/{}/js/viewer.js",
                teamcraft::assets::version()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    app.teardown().await;
}

/// Desktop app sign-in: browser confirmation → one-time code on the loopback
/// interface → code + PKCE verifier exchanged for a personal access token.
#[tokio::test]
async fn desktop_sign_in_through_the_browser() {
    let app = app_or_skip!();
    app.user_in("leo", Role::Editor, &["ROCKET"]).await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = teamcraft::services::desktop::challenge_of(verifier);
    let state = "state-0123456789abcdef";
    let page = format!(
        "/desktop/authorize?port=53682&state={state}&challenge={challenge}&device=Leo%27s%20Laptop"
    );

    // signed out: login first (SSO or password), then back to this page
    let r = app.get_page(&page, None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r.header("location").unwrap().starts_with("/login?next="));
    let cookie = app.login("leo").await;
    let r = app.get_page(&page, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Leo&#x27;s Laptop"));
    let csp = r.header("content-security-policy").unwrap();
    assert!(
        csp.contains("form-action 'self' http://127.0.0.1:53682"),
        "{csp}"
    );
    for bad in [
        format!("/desktop/authorize?port=80&state={state}&challenge={challenge}"),
        format!("/desktop/authorize?port=53682&state=x&challenge={challenge}"),
        format!("/desktop/authorize?port=53682&state={state}&challenge=short"),
    ] {
        let r = app.get_page(&bad, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }

    let origin = Some("http://localhost:8080");
    let decide = |decision: &'static str| {
        let challenge = challenge.clone();
        let cookie = cookie.clone();
        let app = &app;
        async move {
            let r = app
                .post_form(
                    "/desktop/authorize",
                    Some(&cookie),
                    origin,
                    &[
                        ("port", "53682"),
                        ("state", state),
                        ("challenge", &challenge),
                        ("device", "Leo's Laptop"),
                        ("decision", decision),
                    ],
                )
                .await;
            assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
            r.header("location").unwrap()
        }
    };
    let code_of = |location: &str| {
        assert!(
            location.starts_with("http://127.0.0.1:53682/callback?code=tc_dsk_"),
            "{location}"
        );
        assert!(location.ends_with(&format!("&state={state}")), "{location}");
        location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string()
    };
    let exchange = |code: String, verifier: &'static str| {
        let app = &app;
        async move {
            app.api(
                Method::POST,
                "/api/v1/desktop/token",
                "",
                Some(json!({ "code": code, "verifier": verifier })),
            )
            .await
        }
    };

    // "Cancel" tells the app, no code
    let location = decide("deny").await;
    assert_eq!(
        location,
        format!("http://127.0.0.1:53682/callback?error=access_denied&state={state}")
    );

    // A wrong verifier fails and burns the code.
    let code = code_of(&decide("allow").await);
    let r = exchange(code.clone(), "wrong-verifier-wrong-verifier-wrong-verifier").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = exchange(code, verifier).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "codes are single-use");

    // The right verifier yields a working token – exactly once.
    let code = code_of(&decide("allow").await);
    let r = exchange(code.clone(), verifier).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let token = r.json()["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("tc_pat_"));
    assert_eq!(r.json()["user"]["username"], "leo");
    assert_eq!(r.json()["token_name"], "TeamCraft Desktop – Leo's Laptop");
    assert!(!r.json()["expires_at"].is_null());
    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["username"], "leo");
    // full scope (the app writes), sliding lifetime (F2)
    assert_eq!(r.json()["token_scope"], "full");
    let (scope, idle): (String, Option<i32>) =
        sqlx::query_as("SELECT scope, idle_days FROM api_tokens WHERE name = $1")
            .bind("TeamCraft Desktop – Leo's Laptop")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(
        (scope.as_str(), idle),
        (
            "full",
            Some(teamcraft::services::desktop::TOKEN_DAYS as i32)
        )
    );
    let r = exchange(code, verifier).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "already used");

    // Expired codes are refused.
    let code = code_of(&decide("allow").await);
    sqlx::query("UPDATE desktop_auth_codes SET expires_at = now() - interval '1 minute'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = exchange(code, verifier).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let purged = teamcraft::services::users::purge_expired(&app.pool)
        .await
        .unwrap();
    assert!(purged >= 1);

    // Cross-site posts cannot approve a sign-in.
    let r = app
        .post_form(
            "/desktop/authorize",
            Some(&cookie),
            Some("https://evil.example"),
            &[
                ("port", "53682"),
                ("state", state),
                ("challenge", &challenge),
                ("decision", "allow"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let app = app_or_skip!();
    app.user("max", Role::Editor).await;
    let r = app
        .post_form(
            "/login",
            None,
            Some("http://localhost:8080"),
            &[("username", "max"), ("password", "wrong-password")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.text().contains("Invalid username or password"));
    app.teardown().await;
}

#[tokio::test]
async fn cross_site_posts_are_blocked() {
    let app = app_or_skip!();
    app.user("max", Role::Editor).await;
    let cookie = app.login("max").await;
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            Some("https://evil.example"),
            &[("item_type", "part"), ("name", "CSRF")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.text().contains("Cross-site request blocked"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.teardown().await;
}

#[tokio::test]
async fn every_page_renders_for_an_admin() {
    let app = app_or_skip!();
    let (_, token) = app.user("root", Role::Admin).await;
    app.user("rolf", Role::Editor).await;
    let cookie = app.login("root").await;
    let origin = Some("http://localhost:8080");

    // Create data through the forms.
    let r = app
        .post_form(
            "/projects",
            Some(&cookie),
            origin,
            &[
                ("code", "EXP"),
                ("name", "Experimental"),
                ("description", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let rocket = app.project_id("ROCKET").await.to_string();
    // Admins manage the members of a project group
    let r = app
        .post_form(
            &format!("/projects/{rocket}/members"),
            Some(&cookie),
            origin,
            &[("username", "rolf")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app
        .get_page(&format!("/projects/{rocket}"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("rolf (test)"));
    assert!(app
        .actor("rolf")
        .await
        .projects
        .contains(&rocket.parse().unwrap()));

    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            origin,
            &[
                ("item_type", "part"),
                ("name", "Nozzle"),
                ("mass_kg", "1,25"),
                ("unit", "each"),
                ("project_id", &rocket),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let rev_url = r.header("location").unwrap();
    assert!(rev_url.starts_with("/revisions/"));

    // Validation errors re-render the form
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            origin,
            &[("item_type", "part"), ("name", ""), ("mass_kg", "abc")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("is not a number") || r.text().contains("must not be empty"));

    // Flash message after redirect, then the page
    let r = app.get_page(&rev_url, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Nozzle"));
    assert!(r.text().contains("1.25 kg"));

    let rev_id = rev_url.trim_start_matches("/revisions/").to_string();
    let r = app
        .post_form(
            &format!("/revisions/{rev_id}/bom/lines"),
            Some(&cookie),
            origin,
            &[("child", "PRT-000001"), ("quantity", "1")],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "self reference redirects back with an error"
    );

    // A change via the form, then the change page
    let r = app
        .post_form(
            "/changes",
            Some(&cookie),
            origin,
            &[
                ("title", "Release nozzle"),
                ("priority", "normal"),
                ("revision_id", &rev_id),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let change_url = r.header("location").unwrap();

    // Token-based API data for the remaining pages
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "assembly", "name": "Engine", "project": "ROCKET" })),
        )
        .await;
    let asm_rev = r.json()["id"].as_str().unwrap().to_string();
    app.api(
        Method::POST,
        "/api/v1/items/ASM-000001/revisions/A/bom",
        &token,
        Some(json!({ "child": "PRT-000001", "quantity": 2 })),
    )
    .await;

    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'rolf'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let pages = [
        "/".to_string(),
        "/items".to_string(),
        "/items?q=nozzle&type=part&state=in_work".to_string(),
        "/items?system=solidworks".to_string(),
        "/items?q=SolidWorks&system=nonsense".to_string(),
        "/formats".to_string(),
        "/import".to_string(),
        format!("/import?into={asm_rev}"),
        "/items/new".to_string(),
        rev_url.clone(),
        format!("/revisions/{rev_id}/edit"),
        format!("/revisions/{asm_rev}"),
        format!("/revisions/{asm_rev}/bom"),
        format!("/revisions/{asm_rev}/bom?rule=released"),
        format!("/revisions/{asm_rev}/bom.csv"),
        "/changes".to_string(),
        "/changes?status=draft".to_string(),
        "/changes/new".to_string(),
        change_url.clone(),
        "/projects".to_string(),
        format!("/projects/{rocket}"),
        "/admin/users".to_string(),
        format!("/admin/users/{user_id}"),
        "/admin/audit".to_string(),
        "/account".to_string(),
    ];
    for page in &pages {
        let r = app.get_page(page, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::OK, "GET {page}: {}", r.text());
    }
    let r = app.get_page("/formats", Some(&cookie)).await;
    for expected in [
        "SolidWorks",
        "Siemens NX",
        "Teamcenter",
        ".catproduct",
        ".f3d",
        ".x_t",
        ".jt",
    ] {
        assert!(r.text().contains(expected), "formats page lists {expected}");
    }
    let r = app.get_page("/items", Some(&cookie)).await;
    assert!(
        r.text().contains("<option value=\"inventor\""),
        "CAD program filter"
    );

    // Short links and 404 pages
    let r = app.get_page("/i/prt-000001", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app.get_page("/nope", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.text().contains("404"));

    // Non-admins cannot open the admin pages
    app.user("vera", Role::Viewer).await;
    let viewer_cookie = app.login("vera").await;
    let r = app.get_page("/admin/users", Some(&viewer_cookie)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Skins: switching stores the choice and remembers it in a cookie
    let r = app
        .post_form(
            "/account/skin",
            Some(&cookie),
            origin,
            &[("skin", "cachyos")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r
        .headers
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").starts_with("tc_skin=cachyos")));
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(r.text().contains("data-skin=\"cachyos\""));
    let r = app
        .post_form(
            "/account/skin",
            Some(&cookie),
            origin,
            &[("skin", "windows")],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "unknown skins redirect back with an error"
    );
    let r = app.get_page("/", Some(&cookie)).await;
    assert!(r.text().contains("data-skin=\"cachyos\""), "unchanged");

    // Logout ends the session
    let r = app.post_form("/logout", Some(&cookie), origin, &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app.get_page("/", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    app.teardown().await;
}

#[tokio::test]
async fn references_and_impacts_in_the_web_ui() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("eng").await;
    let origin = Some("http://localhost:8080");
    for (item_type, name) in [("part", "Bracket"), ("document", "Bracket drawing")] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let revision_id = |number: &str, code: &str| {
        let uri = format!("/api/v1/items/{number}/revisions/{code}");
        let eng = eng.clone();
        let app = &app;
        async move {
            app.api(Method::GET, &uri, &eng, None).await.json()["revision"]["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let doc_a = revision_id("DOC-000001", "A").await;

    // The drawing references the part (form).
    let r = app
        .post_form(
            &format!("/revisions/{doc_a}/references"),
            Some(&cookie),
            origin,
            &[
                ("kind", "drawing"),
                ("item", "PRT-000001"),
                ("note", "sheet 1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = app
        .get_page(&format!("/revisions/{doc_a}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("Drawing of") && page.contains("PRT-000001") && page.contains("sheet 1"));

    // Release both, then a new revision of the part alone.
    let release = |items: Vec<(&'static str, &'static str)>| {
        let (eng, rev) = (eng.clone(), rev.clone());
        let app = &app;
        async move {
            let r = app
                .api(
                    Method::POST,
                    "/api/v1/changes",
                    &eng,
                    Some(json!({ "title": "Release" })),
                )
                .await;
            let chg = r.json()["number"].as_str().unwrap().to_string();
            for (item, code) in items {
                app.api(
                    Method::POST,
                    &format!("/api/v1/items/{item}/revisions/{code}/checkin"),
                    &eng,
                    None,
                )
                .await;
                app.api(
                    Method::POST,
                    &format!("/api/v1/changes/{chg}/items"),
                    &eng,
                    Some(json!({ "item": item, "revision": code })),
                )
                .await;
            }
            app.api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/reviewers"),
                &eng,
                Some(json!({ "username": "rev" })),
            )
            .await;
            app.api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/submit"),
                &eng,
                None,
            )
            .await;
            let r = app
                .api(
                    Method::POST,
                    &format!("/api/v1/changes/{chg}/review"),
                    &rev,
                    Some(json!({ "decision": "approve" })),
                )
                .await;
            assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
            r.json()["change"]["id"].as_str().unwrap().to_string()
        }
    };
    release(vec![("PRT-000001", "A"), ("DOC-000001", "A")]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items/PRT-000001/revisions/A/revise",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let change_id = release(vec![("PRT-000001", "B")]).await;

    // The change page shows the impact analysis, the dashboard the open check.
    let page = app
        .get_page(&format!("/changes/{change_id}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("Impact analysis") && page.contains("is a drawing of PRT-000001"));
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(page.contains("Dependents to check") && page.contains("DOC-000001/A"));
    let page = app
        .get_page(&format!("/revisions/{doc_a}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("Check needed:") && page.contains("is a drawing of it"));
    let impact_id = app
        .api(Method::GET, "/api/v1/impacts", &eng, None)
        .await
        .json()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Acknowledge through the form; an empty note is refused.
    let back = format!("/revisions/{doc_a}");
    let r = app
        .post_form(
            &format!("/impacts/{impact_id}/acknowledge"),
            Some(&cookie),
            origin,
            &[("note", ""), ("back", &back)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        app.api(Method::GET, "/api/v1/impacts", &eng, None)
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1,
        "still open"
    );
    let r = app
        .post_form(
            &format!("/impacts/{impact_id}/acknowledge"),
            Some(&cookie),
            origin,
            &[
                ("note", "Dimensions on the drawing are unchanged"),
                ("back", &back),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.header("location").as_deref(), Some(back.as_str()));
    let page = app.get_page(&back, Some(&cookie)).await.text();
    assert!(!page.contains("Check needed:"));
    assert!(
        page.contains("Earlier checks") && page.contains("Dimensions on the drawing are unchanged")
    );
    let page = app.get_page("/", Some(&cookie)).await.text();
    assert!(!page.contains("Dependents to check"));
    // "Acknowledge all" on a revision without open checks: back with an error.
    let r = app
        .post_form(
            &format!("/revisions/{doc_a}/impacts/acknowledge"),
            Some(&cookie),
            origin,
            &[
                ("note", "Nothing changed"),
                ("impacts", &impact_id),
                ("back", &back),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.header("location").as_deref(), Some(back.as_str()));
    let r = app
        .get_page(
            &format!(
                "/revisions/{}/bom?rule=as_released",
                revision_id("PRT-000001", "B").await
            ),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn notifications_in_the_web_ui() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("rolf").await;
    let origin = Some("http://localhost:8080");
    app.api(
        Method::POST,
        "/api/v1/items",
        &erika,
        Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" })),
    )
    .await;
    let item_id = app
        .api(Method::GET, "/api/v1/items/PRT-000001", &rolf, None)
        .await
        .json()["item"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Watch the item through the form on the revision page.
    let rev_id = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/revisions/A",
            &rolf,
            None,
        )
        .await
        .json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let back = format!("/revisions/{rev_id}");
    let page = app.get_page(&back, Some(&cookie)).await.text();
    assert!(page.contains(&format!("/items/{item_id}/watch")) && page.contains(">Watch<"));
    let r = app
        .post_form(
            &format!("/items/{item_id}/watch"),
            Some(&cookie),
            origin,
            &[("watch", "1"), ("back", &back)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(app
        .get_page(&back, Some(&cookie))
        .await
        .text()
        .contains(">Unwatch<"));

    // A review request shows up under the bell.
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &erika,
            Some(json!({ "title": "Bracket" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    let change_id = r.json()["id"].as_str().unwrap().to_string();
    for (path, body) in [
        ("items", json!({ "item": "PRT-000001" })),
        ("reviewers", json!({ "username": "rolf" })),
    ] {
        app.api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/{path}"),
            &erika,
            Some(body),
        )
        .await;
    }
    app.api(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/A/checkin",
        &erika,
        None,
    )
    .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let home = app.get_page("/", Some(&cookie)).await.text();
    assert!(
        home.contains("class=\"bell has-unread\"")
            && home.contains("<span class=\"bell-count\">1</span>")
    );
    let page = app.get_page("/notifications", Some(&cookie)).await.text();
    assert!(
        page.contains(&format!("Review requested: {chg} – Bracket"))
            && page.contains("class=\"unread\"")
    );
    let note_id = app
        .api(Method::GET, "/api/v1/notifications", &rolf, None)
        .await
        .json()["notifications"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .get_page(&format!("/notifications/{note_id}/open"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.header("location"), Some(format!("/changes/{change_id}")));
    let home = app.get_page("/", Some(&cookie)).await.text();
    assert!(
        home.contains("class=\"bell\"") && !home.contains("bell-count"),
        "read"
    );
    // Somebody else's notification cannot be opened.
    let erika_cookie = app.login("erika").await;
    let r = app
        .get_page(
            &format!("/notifications/{note_id}/open"),
            Some(&erika_cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Settings: mute item updates, keep the rest; the account page shows it.
    let r = app
        .post_form(
            "/account/notifications",
            Some(&cookie),
            origin,
            &[
                ("want", "review_requested"),
                ("want", "change_update"),
                ("want", "impact"),
                ("want", "unlock"),
                ("want", "change_request"),
                ("want", "request_digest"),
                ("mattermost", "on"),
                ("mattermost_username", "Rolf.R"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let settings = app
        .api(Method::GET, "/api/v1/notifications/settings", &rolf, None)
        .await
        .json();
    assert_eq!(settings["muted"], json!(["item_update"]));
    assert_eq!(settings["email"], false);
    assert_eq!(settings["mattermost_username"], "rolf.r");
    let account = app.get_page("/account", Some(&cookie)).await.text();
    assert!(
        account.contains("value=\"item_update\" >") && account.contains("value=\"impact\" checked")
    );
    // Test notification and "mark all as read"
    let r = app
        .post_form("/account/notifications/test", Some(&cookie), origin, &[])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(app
        .get_page("/notifications", Some(&cookie))
        .await
        .text()
        .contains("Test notification"));
    let r = app
        .post_form(
            "/notifications/read-all",
            Some(&cookie),
            origin,
            &[("back", "/notifications")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(!app
        .get_page("/", Some(&cookie))
        .await
        .text()
        .contains("bell-count"));
    // The change page offers Watch.
    let page = app
        .get_page(&format!("/changes/{change_id}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains(&format!("/changes/{change_id}/watch")));
    app.teardown().await;
}

/// First start of a local server: the login page offers to create the
/// administrator – exactly once.
#[tokio::test]
async fn first_start_creates_the_administrator() {
    let app = app_or_skip!();
    let origin = Some("http://localhost:8080");
    let html = app.get_page("/login", None).await.text();
    assert!(html.contains("Create the administrator"), "no accounts yet");
    let r = app
        .post_form(
            "/setup",
            None,
            origin,
            &[
                ("username", "root"),
                ("password", "long enough password"),
                ("password_repeat", "different"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("do not match"));
    let r = app
        .post_form(
            "/setup",
            None,
            origin,
            &[
                ("username", "root"),
                ("display_name", "Root"),
                ("password", "long enough password"),
                ("password_repeat", "long enough password"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(r.header("location").as_deref(), Some("/projects"));
    let session = r
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|c| c.to_str().ok())
        .find(|c| c.starts_with("tc_session="))
        .map(|c| c.split(';').next().unwrap().to_string())
        .expect("signed in");
    let html = app.get_page("/admin/users", Some(&session)).await;
    assert_eq!(
        html.status,
        StatusCode::OK,
        "the new account is an administrator"
    );
    // afterwards the form is gone and the endpoint is closed
    let html = app.get_page("/login", None).await.text();
    assert!(!html.contains("Create the administrator"));
    let r = app
        .post_form(
            "/setup",
            None,
            origin,
            &[
                ("username", "mallory"),
                ("password", "long enough password"),
                ("password_repeat", "long enough password"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

/// The vault explorer: folders, locks, and check-out / cancel / check-in
/// that return to the explorer.
#[tokio::test]
async fn vault_explorer_check_out_and_in() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("leo", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Wheel hub", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = r.json()["id"].as_str().unwrap().to_string();
    let rover = app.project_id("ROVER").await.to_string();
    let cookie = app.login("leo").await;
    let origin = Some("http://localhost:8080");

    // new items are checked out to their author: "My check-outs" lists it
    let html = app.get_page("/vault", Some(&cookie)).await.text();
    assert!(
        html.contains("My check-outs") && html.contains("PRT-000001"),
        "{html}"
    );
    let folder = format!("/vault?folder={rover}&rev={rev}");
    let html = app.get_page(&folder, Some(&cookie)).await.text();
    assert!(html.contains("Wheel hub") && html.contains("Check in and release"));

    // cancel returns to the explorer at once, without a confirmation step
    let back = teamcraft::util::pct_encode(&folder);
    let r = app
        .post_form(
            &format!("/revisions/{rev}/cancel-checkout?back={back}"),
            Some(&cookie),
            origin,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.header("location").as_deref(), Some(folder.as_str()));
    let html = app.get_page(&folder, Some(&cookie)).await.text();
    assert!(html.contains(&format!("/revisions/{rev}/checkout?back=")));
    assert!(
        !html.contains("data-confirm=\"Release"),
        "own check-outs need no confirmation"
    );
    let html = app
        .get_page("/vault?folder=mine", Some(&cookie))
        .await
        .text();
    assert!(html.contains("You have nothing checked out"));

    // check out again from the explorer
    let r = app
        .post_form(
            &format!("/revisions/{rev}/checkout?back={back}"),
            Some(&cookie),
            origin,
            &[],
        )
        .await;
    assert_eq!(r.header("location").as_deref(), Some(folder.as_str()));
    // unsafe return addresses are ignored
    let r = app
        .post_form(
            &format!("/revisions/{rev}/cancel-checkout?back=https%3A%2F%2Fevil.example"),
            Some(&cookie),
            origin,
            &[],
        )
        .await;
    assert_eq!(r.header("location").as_deref(), Some("/"));
    // search over every folder
    let html = app.get_page("/vault?q=hub", Some(&cookie)).await.text();
    assert!(html.contains("PRT-000001") && html.contains("ROVER"));
    let r = app.get_page("/vault?folder=nonsense", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    app.teardown().await;
}

/// Uploads sent by `static/app.js` (progress bar, cancel button) get the
/// redirect target as JSON, with the flash cookie, instead of a 303 that
/// XMLHttpRequest would follow silently.
#[tokio::test]
async fn script_sent_forms_get_the_redirect_as_json() {
    let app = app_or_skip!();
    app.user("leo", Role::Editor).await;
    let cookie = app.login("leo").await;
    let request = |scripted: bool| {
        let mut builder = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/account/skin")
            .header("host", "localhost:8080")
            .header("origin", "http://localhost:8080")
            .header("cookie", &cookie)
            .header("content-type", "application/x-www-form-urlencoded");
        if scripted {
            builder = builder.header("x-teamcraft-script", "1");
        }
        builder.body(axum::body::Body::from("skin=kde")).unwrap()
    };
    let r = app.send(request(false)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "browsers get the redirect");
    let r = app.send(request(true)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["location"].as_str().unwrap().starts_with('/'));
    assert!(
        r.headers
            .get_all("set-cookie")
            .iter()
            .any(|c| c.to_str().unwrap().starts_with("tc_flash=")),
        "the flash message survives"
    );
    app.teardown().await;
}

/// Multipart form POST like a browser (session cookie, same-origin).
async fn post_multipart(
    app: &common::TestApp,
    uri: &str,
    cookie: &str,
    file: (&str, &[u8]),
    fields: &[(&str, &str)],
) -> common::Response {
    use axum::{body::Body, http::header, http::Request};
    let boundary = "teamcraft-web-boundary";
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: text/csv\r\n\r\n",
            file.0
        )
        .as_bytes(),
    );
    body.extend_from_slice(file.1);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    app.send(request).await
}

#[tokio::test]
async fn bom_import_in_the_web_ui() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("erika").await;
    let origin = Some("http://localhost:8080");
    let project = app.project_id("ROCKET").await.to_string();

    let page = app.get_page("/import", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Upload and preview"));
    let items = app.get_page("/items", Some(&cookie)).await.text();
    assert!(
        items.contains("href=\"/import\""),
        "linked from the item list"
    );

    let csv = b"Level,Part Number,Name,Quantity\n0,ROVER,Rover,1\n1,WHEEL,Wheel,6\n2,HUB,Hub <alu>,1\n1,FRAME,Frame,1\n";
    let r = post_multipart(
        &app,
        "/import",
        &cookie,
        ("rover.csv", csv),
        &[
            ("project_id", &project),
            ("numbers", "file"),
            ("match_existing_sent", "1"),
            ("match_existing", "on"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let preview_url = r.header("location").unwrap();
    assert!(preview_url.starts_with("/import/"), "{preview_url}");
    let preview = app.get_page(&preview_url, Some(&cookie)).await;
    assert_eq!(preview.status, StatusCode::OK);
    let html = preview.text();
    for expected in [
        "Import preview",
        "<span class=\"mono\">ROVER</span>",
        "Hub &lt;alu&gt;",
        "indent-2",
        "new assembly",
    ] {
        assert!(html.contains(expected), "preview shows {expected}");
    }
    let r = app
        .api(Method::GET, "/api/v1/items/ROVER", &token, None)
        .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "the preview creates nothing"
    );

    let r = app
        .post_form(&format!("{preview_url}/run"), Some(&cookie), origin, &[])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let target = r.header("location").unwrap();
    assert!(
        target.starts_with("/revisions/") && target.ends_with("#bom"),
        "{target}"
    );
    let rover = app
        .api(Method::GET, "/api/v1/items/ROVER/revisions/A", &token, None)
        .await
        .json();
    assert_eq!(rover["bom"].as_array().unwrap().len(), 2, "WHEEL and FRAME");
    let page = app
        .get_page(target.trim_end_matches("#bom"), Some(&cookie))
        .await
        .text();
    assert!(
        page.contains("Imported 3 BOM lines"),
        "flash after the import"
    );
    assert!(
        page.contains("Import BOM…"),
        "checked out: import more from here"
    );
    // The staged upload is gone: a second click finds nothing.
    let r = app
        .post_form(&format!("{preview_url}/run"), Some(&cookie), origin, &[])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = app.get_page(&preview_url, Some(&cookie)).await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "gone → back to the upload page"
    );
    assert_eq!(r.header("location").as_deref(), Some("/import"));

    // Discard, and unreadable files go back to the form with a message.
    let r = post_multipart(
        &app,
        "/import",
        &cookie,
        ("rover.csv", csv),
        &[("project_id", &project)],
    )
    .await;
    let url = r.header("location").unwrap();
    let r = app
        .post_form(&format!("{url}/discard"), Some(&cookie), origin, &[])
        .await;
    assert_eq!(r.header("location").as_deref(), Some("/items"));
    let r = post_multipart(
        &app,
        "/import",
        &cookie,
        ("notes.csv", b"just some text\n"),
        &[("project_id", &project)],
    )
    .await;
    assert_eq!(r.header("location").as_deref(), Some("/import"));
    app.teardown().await;
}

#[tokio::test]
async fn new_item_with_files() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let cookie = app.login("erika").await;
    let project = app.project_id("ROVER").await.to_string();
    let form = app.get_page("/items/new", Some(&cookie)).await.text();
    assert!(
        form.contains("enctype=\"multipart/form-data\"") && form.contains("name=\"file\" multiple"),
        "the form takes files"
    );

    // No name: the item is named after its first file and has it right away.
    let r = post_multipart(
        &app,
        "/items",
        &cookie,
        ("Bracket.step", b"ISO-10303-21; bracket"),
        &[
            ("item_type", "part"),
            ("name", ""),
            ("project_id", &project),
            ("unit", "each"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let rev = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/revisions/A",
            &token,
            None,
        )
        .await
        .json();
    assert_eq!(rev["revision"]["name"], "Bracket");
    let files = rev["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["filename"], "Bracket.step");
    assert_eq!(files[0]["format"], "step");
    let page = app
        .get_page(&r.header("location").unwrap(), Some(&cookie))
        .await
        .text();
    assert!(
        page.contains("Bracket.step"),
        "the revision page lists the file"
    );

    // Errors re-render the form and ask for the files again; nothing is created.
    let r = post_multipart(
        &app,
        "/items",
        &cookie,
        ("Frame.ipt", b"OLE"),
        &[("item_type", "part"), ("project_id", "")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("Choose the files again"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    app.teardown().await;
}

#[tokio::test]
async fn numbering_scheme_on_the_project_page() {
    use chrono::Datelike;
    let app = app_or_skip!();
    app.user("root", Role::Admin).await;
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("root").await;
    let origin = Some("http://localhost:8080");
    let rocket = app.project_id("ROCKET").await;
    let page = format!("/projects/{rocket}");
    let year = chrono::Utc::now().year();

    let html = app.get_page(&page, Some(&cookie)).await.text();
    assert!(html.contains("Item numbers") && html.contains("(built-in default)"));
    assert!(html.contains("PRT-000001") && html.contains("SW-000001"));

    // the preview button shows the numbers of a scheme without saving it
    let html = app
        .get_page(
            &format!("{page}?scheme=%7BPROJECT%7D-%7BTYPE%7D%7BYY%7D-%7BSEQ:3%7D"),
            Some(&cookie),
        )
        .await
        .text();
    assert!(html.contains("preview – not saved"), "{html}");
    assert!(html.contains(&format!("ROCKET-DOC{:02}-001", year % 100)));
    let html = app
        .get_page(&format!("{page}?scheme=%7BTYPE%7D"), Some(&cookie))
        .await
        .text();
    assert!(html.contains("flash-error") && html.contains("sequence placeholder"));
    assert!(
        html.contains("PRT-000001"),
        "falls back to the saved scheme"
    );

    // saving it
    let r = app
        .post_form(
            &format!("{page}/numbering"),
            Some(&cookie),
            origin,
            &[("scheme", "{PROJECT}-{TYPE}{YY}-{SEQ:3}")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let html = app.get_page(&page, Some(&cookie)).await.text();
    assert!(html.contains("value=\"{PROJECT}-{TYPE}{YY}-{SEQ:3}\""));
    assert!(!html.contains("(built-in default)"));
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["item_number"],
        format!("ROCKET-PRT{:02}-001", year % 100)
    );
    app.teardown().await;
}

#[tokio::test]
async fn simulations_in_the_web_ui() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let cookie = app.login("eng").await;
    let origin = Some("http://localhost:8080");
    for (item_type, name) in [("part", "Bracket"), ("simulation", "Bracket – modal")] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let revision_id = |number: &str| {
        let uri = format!("/api/v1/items/{number}/revisions/A");
        let eng = eng.clone();
        let app = &app;
        async move {
            app.api(Method::GET, &uri, &eng, None).await.json()["revision"]["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let prt_a = revision_id("PRT-000001").await;
    let sim_a = revision_id("SIM-000001").await;
    app.api_upload(
        "/api/v1/items/PRT-000001/revisions/A/files",
        &eng,
        "bracket.step",
        b"ISO-10303-21;",
    )
    .await;

    // The new-item form offers the type, the simulation page its cards.
    let form = app
        .get_page("/items/new?type=simulation", Some(&cookie))
        .await
        .text();
    assert!(
        form.contains("value=\"simulation\" selected"),
        "type preselected"
    );
    let page = app
        .get_page(&format!("/revisions/{sim_a}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("id=\"simulation-basis\"") && page.contains("id=\"simulation-results\""));
    assert!(page.contains("No basis yet"));

    // Basis and results through the forms.
    let r = app
        .post_form(
            &format!("/revisions/{sim_a}/simulation/bases"),
            Some(&cookie),
            origin,
            &[
                ("role", "geometry"),
                ("item", "PRT-000001"),
                ("revision", ""),
                ("file", "bracket.step"),
                ("note", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = app
        .post_form(
            &format!("/revisions/{sim_a}/simulation/results"),
            Some(&cookie),
            origin,
            &[
                ("name", "First eigenfrequency"),
                ("value", "182,5"),
                ("unit", "Hz"),
                ("lower_limit", "200"),
                ("upper_limit", ""),
                ("load_case", "modal"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = post_multipart(
        &app,
        &format!("/revisions/{sim_a}/simulation/results/import"),
        &cookie,
        (
            "results.csv",
            b"name;value;unit;limit_low;limit_high;load_case\nMax. displacement;0,35;mm;;0,5;LC1\n",
        ),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = app
        .get_page(&format!("/revisions/{sim_a}"), Some(&cookie))
        .await
        .text();
    for expected in [
        "PRT-000001/A",
        "bracket.step",
        "Up to date",
        "First eigenfrequency",
        "Max. displacement",
        "1 failed",
        "1 passed",
        "badge badge-fail",
    ] {
        assert!(page.contains(expected), "simulation page shows {expected}");
    }
    let csv = app
        .get_page(
            &format!("/revisions/{sim_a}/simulation/results.csv"),
            Some(&cookie),
        )
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert!(
        csv.text().contains("Max. displacement,0.35,mm,,0.5,LC1"),
        "{}",
        csv.text()
    );

    // A VTU result file: the Results card links the 3D view, the mesh
    // carries the result fields.
    let vtu = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sim/beam-zlib.vtu"),
    )
    .unwrap();
    let r = app
        .api_upload(
            "/api/v1/items/SIM-000001/revisions/A/files",
            &eng,
            "beam.vtu",
            &vtu,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let result_file = r.json()[0]["id"].as_str().unwrap().to_string();
    let page = app
        .get_page(&format!("/revisions/{sim_a}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("View result in 3D"));
    assert!(page.contains(&format!("/files/{result_file}/preview")));
    let r = app
        .get_page(&format!("/files/{result_file}/preview"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("coloured by a result field"));
    let r = app
        .get_page(&format!("/files/{result_file}/mesh"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert_eq!(mesh.triangle_count(), 20);
    assert_eq!(mesh.fields[0].name, "temperature");

    // The part lists the simulation.
    let part = app
        .get_page(&format!("/revisions/{prt_a}"), Some(&cookie))
        .await
        .text();
    assert!(
        part.contains("id=\"simulations\"") && part.contains("SIM-000001/A"),
        "{part}"
    );

    // A part a simulation is based on cannot be deleted.
    let part_item = app
        .api(Method::GET, "/api/v1/items/PRT-000001", &eng, None)
        .await
        .json()["item"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .post_form(
            &format!("/items/{part_item}/delete"),
            Some(&cookie),
            origin,
            &[("back", format!("/revisions/{prt_a}").as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/items/PRT-000001", &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "the basis item still exists");
    app.teardown().await;
}

/// Regression (#182): the sign-in and error pages carry both logo variants and
/// skins.css must show exactly one. A `display` rule on the logo classes in
/// style.css (higher specificity than the :where() skin rules) showed both.
#[test]
fn logo_variants_are_switched_by_skins_css_only() {
    let root = env!("CARGO_MANIFEST_DIR");
    let read = |p: &str| std::fs::read_to_string(format!("{root}/{p}")).unwrap();
    let skins = read("shared/css/skins.css");
    assert!(
        skins.contains(".on-dark, .skin-logo-dark { display: none; }"),
        "the dark variant is hidden by default"
    );
    assert!(skins.contains(":is(.on-light, .skin-logo-light) { display: none; }"));
    for file in ["static/style.css", "desktop/ui/app.css"] {
        for rule in read(file).split('}') {
            let Some((selector, body)) = rule.split_once('{') else {
                continue;
            };
            let touches_logo = [
                ".on-light",
                ".on-dark",
                ".login-logo",
                ".skin-logo",
                ".connect-logo",
            ]
            .iter()
            .any(|c| selector.contains(c));
            assert!(
                !(touches_logo && body.contains("display")),
                "{file}: `{}` must not set display (it would show both logos)",
                selector.trim()
            );
        }
    }
    for tpl in ["templates/login.html", "templates/error.html"] {
        let html = read(tpl);
        assert!(html.contains("login-logo on-dark") && html.contains("login-logo on-light"));
    }
}

#[tokio::test]
async fn material_field_suggests_the_library() {
    use teamcraft::services::mass_properties::{self as mass, MaterialInput};

    let app = app_or_skip!();
    let (admin, _) = app.user("ada", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    for (name, density) in [("Steel <1.4301>", 7900.0), ("Aluminium 7075-T6", 2810.0)] {
        let input = MaterialInput {
            name: name.into(),
            density_kg_m3: density,
            notes: String::new(),
        };
        mass::create_material(&app.pool, &admin, input)
            .await
            .unwrap();
    }
    app.api(
        Method::POST,
        "/api/v1/items",
        &erika,
        Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" })),
    )
    .await;
    let rev_id = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/revisions/A",
            &erika,
            None,
        )
        .await
        .json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let editor = app.login("erika").await;
    for uri in [
        format!("/revisions/{rev_id}/edit"),
        "/items/new".to_string(),
    ] {
        let page = app.get_page(&uri, Some(&editor)).await.text();
        assert!(page.contains("name=\"material\"") && page.contains("list=\"material-options\""));
        // the library's names (escaped), with the density, in the library's order
        let alu = page
            .find("<option value=\"Aluminium 7075-T6\">2810 kg&#x2f;m³</option>")
            .expect("aluminium is suggested");
        let steel = page
            .find("<option value=\"Steel &lt;1.4301&gt;\">7900 kg&#x2f;m³</option>")
            .expect("steel is suggested");
        assert!(alu < steel, "{uri}: library order");
        assert!(page.contains("Not in the material library"), "{uri}: hint");
        assert!(
            !page.contains("href=\"/materials\">Open the library"),
            "{uri}: only admins get the link"
        );
    }

    // A name that is not in the library can still be saved.
    let r = app
        .post_form(
            &format!("/revisions/{rev_id}/edit"),
            Some(&editor),
            Some("http://localhost:8080"),
            &[("name", "Bracket"), ("material", "Alu 7075")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = app
        .get_page(&format!("/revisions/{rev_id}/edit"), Some(&editor))
        .await
        .text();
    assert!(page.contains("value=\"Alu 7075\""));

    // Administrators get the link to the library.
    let root = app.login("ada").await;
    let page = app.get_page("/items/new", Some(&root)).await.text();
    assert!(page.contains("href=\"/materials\">Open the library"));
}
