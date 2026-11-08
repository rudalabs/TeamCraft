//! Need-to-know project groups, step 3, remaining switches (issue #151):
//! `notification_content`, `external_llm` and `desktop_sync`, proven against
//! a database. The source guard is `tests/interface_switches.rs`; the other
//! switches are in `tests/access_log.rs`.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    domain::Role,
    services::project_policy::{self, AccessUpdate, Interface},
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

struct Fixture {
    owner: String,
    item: String,
}

/// A restricted group `NTK` owned by `erika` with one item and one file.
async fn group(app: &common::TestApp) -> Fixture {
    let (_, owner) = app.user("erika", Role::Editor).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &owner,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let item = app
        .create_item(
            &owner,
            json!({ "item_type": "part", "name": "Secret part", "project": "NTK" }),
        )
        .await;
    let r = app
        .api_upload(
            &format!("/api/v1/items/{item}/revisions/A/files"),
            &owner,
            "secret.step",
            b"ISO-10303-21; secret",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    Fixture { owner, item }
}

async fn get(app: &common::TestApp, token: &str, uri: &str) -> StatusCode {
    app.api(Method::GET, uri, token, None).await.status
}

async fn set(app: &common::TestApp, i: Interface, allowed: bool) {
    let actor = app.actor("erika").await;
    let id = app.project_id("NTK").await;
    project_policy::set_access(
        &app.pool,
        &actor,
        id,
        AccessUpdate {
            interfaces: vec![(i, allowed)],
            ..Default::default()
        },
    )
    .await
    .expect("set_access");
}

const NEW: [&str; 3] = ["notification_content", "external_llm", "desktop_sync"];

#[tokio::test]
async fn the_new_switches_default_to_allowed_are_set_through_the_api_and_audited() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let id = app.project_id("NTK").await;
    let r = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &f.owner, None)
        .await;
    for name in NEW {
        assert_eq!(r.json()["access"]["interfaces"][name], true, "{name}");
    }
    let put = |body: Value| {
        app.api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(body),
        )
    };
    let r = put(json!({ "interfaces": { "notification_content": false,
                                         "external_llm": false, "desktop_sync": false } }))
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let access = &r.json()["access"]["interfaces"];
    for name in NEW {
        assert_eq!(access[name], false, "{name}");
    }
    assert_eq!(access["mcp"], true, "the others stay as they were");
    assert!(app
        .audit_actions(id)
        .await
        .contains(&"project.interfaces".to_string()));
    // an unknown name is refused
    let r = put(json!({ "interfaces": { "carrier_pigeon": false } })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // somebody who may not read the group does not see its policy
    let (_, outsider) = app.user("olaf", Role::Editor).await;
    let r = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &outsider, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // allowing again works for the manager
    let r = put(json!({ "interfaces": { "desktop_sync": true } })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["access"]["interfaces"]["desktop_sync"], true);
    app.teardown().await;
}

#[tokio::test]
async fn the_desktop_switch_hides_the_group_from_the_working_set() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let uri = format!("/api/v1/items/{}/revisions/A/workset", f.item);
    assert_eq!(get(&app, &f.owner, &uri).await, StatusCode::OK);
    set(&app, Interface::DesktopSync, false).await;
    assert_eq!(
        get(&app, &f.owner, &uri).await,
        StatusCode::NOT_FOUND,
        "the group looks non-existent to the desktop Get"
    );
    // other interfaces still reach it
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/items/{}", f.item)).await,
        StatusCode::OK
    );
    set(&app, Interface::DesktopSync, true).await;
    assert_eq!(get(&app, &f.owner, &uri).await, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn the_desktop_switch_leaves_a_group_out_of_a_structure() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    let parent = app
        .create_item(&admin, json!({ "item_type": "assembly", "name": "Outer" }))
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            &admin,
            Some(json!({ "child": f.item, "quantity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let uri = format!("/api/v1/items/{parent}/revisions/A/workset?rule=latest");
    let r = app.api(Method::GET, &uri, &admin, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.json().to_string().contains("secret.step"),
        "the file is in the working set: {}",
        r.json()
    );
    set(&app, Interface::DesktopSync, false).await;
    let r = app.api(Method::GET, &uri, &admin, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        !r.json().to_string().contains("secret.step"),
        "left out of the working set: {}",
        r.json()
    );
    app.teardown().await;
}

fn llm(base_url: &str, internal: &[&str]) -> teamcraft::config::AssistantConfig {
    teamcraft::config::AssistantConfig {
        provider: teamcraft::config::AssistantProvider::OpenAiCompatible,
        base_url: base_url.into(),
        api_key: None,
        model: "m".into(),
        max_steps: 4,
        max_tokens: 1024,
        request_timeout: std::time::Duration::from_secs(30),
        run_timeout: std::time::Duration::from_secs(60),
        max_concurrent: 1,
        retention_days: 30,
        internal_urls: internal.iter().map(|u| u.to_string()).collect(),
    }
}

#[tokio::test]
async fn the_external_llm_switch_depends_on_the_internal_url_list() {
    use teamcraft::services::assistant::switches;
    let app = app_or_skip!();
    let _ = group(&app).await;
    let id = app.project_id("NTK").await;
    let me = app.actor("erika").await;
    let external = llm("https://api.provider.example/v1", &[]);
    let internal = llm("http://ollama.corp:11434/v1", &["http://ollama.corp:11434"]);
    let lookalike = llm(
        "http://ollama.corp.evil:11434/v1",
        &["http://ollama.corp:11434"],
    );
    let sees = |cfg: Option<&teamcraft::config::AssistantConfig>| {
        let (me, pool, switches) = (me.clone(), app.pool.clone(), switches(cfg));
        async move {
            project_policy::narrow(&pool, me, &switches)
                .await
                .unwrap()
                .can_see_project(Some(id))
        }
    };
    assert!(sees(Some(&external)).await, "allowed by default");
    set(&app, Interface::ExternalLlm, false).await;
    assert!(
        !sees(Some(&external)).await,
        "an external provider does not get the group"
    );
    assert!(
        !sees(Some(&lookalike)).await,
        "a look-alike host is external"
    );
    assert!(
        sees(Some(&internal)).await,
        "a provider on the internal list does"
    );
    // the assistant switch applies to an internal provider as well
    set(&app, Interface::Assistant, false).await;
    assert!(!sees(Some(&internal)).await);
    set(&app, Interface::Assistant, true).await;
    set(&app, Interface::ExternalLlm, true).await;
    assert!(sees(Some(&external)).await);
    app.teardown().await;
}

/// Records e-mails and chat posts instead of sending them.
#[derive(Default)]
struct Outbox {
    mails: std::sync::Mutex<Vec<String>>,
    chats: std::sync::Mutex<Vec<String>>,
}

impl teamcraft::delivery::MailSender for Outbox {
    fn send(
        &self,
        message: lettre::Message,
    ) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        let text = String::from_utf8_lossy(&message.formatted()).to_string();
        self.mails.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

impl teamcraft::delivery::ChatSender for Outbox {
    fn post(&self, payload: Value) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        self.chats.lock().unwrap().push(payload.to_string());
        Box::pin(async { Ok(()) })
    }
}

/// Queues a notification about the item for erika and delivers it.
async fn deliver(app: &common::TestApp, number: &str, direct: bool, title: &str) -> Outbox {
    let item_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(number)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let user = app.actor("erika").await;
    sqlx::query("UPDATE users SET email = 'erika@example.test' WHERE id = $1")
        .bind(user.id)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO notifications (id, user_id, kind, title, body, link, subject_kind, subject_id,
                                    email_state, chat_state)
         VALUES (gen_random_uuid(), $1, 'item_update', $2, 'Body of the secret part', '/items/x',
                 'item', $3, 'pending', 'pending')",
    )
    .bind(user.id)
    .bind(title)
    .bind(item_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let outbox = std::sync::Arc::new(Outbox::default());
    let delivery = teamcraft::delivery::Delivery::with_senders(
        "https://plm.example.test",
        "TeamCraft",
        Some((outbox.clone(), "TeamCraft <teamcraft@example.test>")),
        Some((outbox.clone(), direct)),
    )
    .unwrap();
    let report = delivery.run_once(&app.pool).await.unwrap();
    assert_eq!((report.emails, report.chats), (1, 1));
    std::sync::Arc::try_unwrap(outbox).ok().unwrap()
}

#[tokio::test]
async fn the_notification_switch_sends_a_generic_text_and_the_link_only() {
    let app = app_or_skip!();
    let f = group(&app).await;
    // allowed (the default): the e-mail shows the text as before
    let out = deliver(&app, &f.item, true, "Zebra title one").await;
    assert!(out.mails.lock().unwrap()[0].contains("Zebra title one"));
    assert!(out.chats.lock().unwrap()[0].contains("Zebra title one"));
    // switched off: neither e-mail nor chat (direct messages too) carry the text
    set(&app, Interface::NotificationContent, false).await;
    for direct in [true, false] {
        let out = deliver(&app, &f.item, direct, "Zebra title two").await;
        let mails = out.mails.lock().unwrap();
        let chats = out.chats.lock().unwrap();
        assert_eq!((mails.len(), chats.len()), (1, 1));
        for text in mails.iter().chain(chats.iter()) {
            assert!(!text.contains("Zebra title two"), "{text}");
            assert!(!text.contains("Body of the secret part"), "{text}");
            assert!(!text.contains(&f.item), "{text}");
            assert!(text.contains("new TeamCraft notification"), "{text}");
            assert!(
                text.contains("plm.example.test/items/x"),
                "the link stays: {text}"
            );
        }
    }
    // the in-app notification keeps its text
    let titles: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE title LIKE 'Zebra%'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(titles, 3);
    // allowing it again restores the text
    set(&app, Interface::NotificationContent, true).await;
    let out = deliver(&app, &f.item, true, "Zebra title three").await;
    assert!(out.mails.lock().unwrap()[0].contains("Zebra title three"));
    app.teardown().await;
}
