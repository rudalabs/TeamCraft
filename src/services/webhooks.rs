//! Outgoing event webhooks: administration and the events.
//!
//! Administrators register webhooks (URL, events, optional project group).
//! The services call [`emit`] inside the transaction of the event, which
//! writes one delivery per matching active webhook into the outbox
//! `webhook_deliveries`; `crate::webhooks` sends them. A rolled back event
//! leaves no delivery behind.
//!
//! Every payload has the same envelope – `event`, `id` (one per event, the
//! same for every webhook), `occurred_at`, `actor.username`, `instance` – plus
//! the objects of the event (`revision`, `change`, `change_request`, …) with
//! their identifiers, `web_url` and `api_url`. File contents and secrets are
//! never part of a payload.
//!
//! | Event                     | Emitted by                                   | Objects |
//! |---------------------------|----------------------------------------------|---------|
//! | `revision.released`       | the last approval of a change, per revision  | `revision`, `change` |
//! | `revision.obsoleted`      | marking a released revision obsolete         | `revision` |
//! | `change.submitted`        | submitting a change for review               | `change`, `revisions` |
//! | `change.approved`         | every approval of a reviewer                 | `change`, `review`, `revisions` |
//! | `change.rejected`         | a reviewer's rejection (rejects the change)  | `change`, `review`, `revisions` |
//! | `change.released`         | the last approval                            | `change`, `revisions` |
//! | `change_request.created`  | raising a change request                     | `change_request`, `items` |
//! | `change_request.decided`  | accepting, rejecting or deferring a request  | `change_request`, `decision` |
//! | `simulation.result_added` | adding or importing simulation result values | `revision`, `result` or `import` |

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::{self, Actor},
    domain::{self, Role, WebhookEvent},
    error::{AppError, AppResult},
    models::{Change, ChangeRequest, Revision},
    services::audit,
    util,
    webhooks::TargetPolicy,
};

/// Prefix of generated webhook secrets.
pub const SECRET_PREFIX: &str = "tc_whsec_";
/// Most webhooks of one instance.
pub const MAX_WEBHOOKS: i64 = 100;

/// A webhook as shown to administrators – without its secret.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    /// `tc_whsec_…1a2b`: enough to tell secrets apart, useless for signing.
    pub secret_hint: String,
    pub events: Vec<String>,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub active: bool,
    pub created_by: Option<Uuid>,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub secret_rotated_at: DateTime<Utc>,
    /// Deliveries waiting to be sent or retried.
    pub pending: i64,
    /// Deliveries that gave up during the last 7 days.
    pub failed_recently: i64,
    /// Status and time of the latest delivery.
    pub last_status: Option<String>,
    pub last_delivery_at: Option<DateTime<Utc>>,
}

const WEBHOOK_SELECT: &str = "
    SELECT w.id, w.name, w.url, w.secret_hint, w.events, w.project_id, p.code AS project_code,
           w.active, w.created_by, u.display_name AS created_by_name,
           w.created_at, w.updated_at, w.secret_rotated_at,
           (SELECT COUNT(*) FROM webhook_deliveries d
             WHERE d.webhook_id = w.id AND d.status = 'pending') AS pending,
           (SELECT COUNT(*) FROM webhook_deliveries d
             WHERE d.webhook_id = w.id AND d.status = 'failed'
               AND d.updated_at > now() - interval '7 days') AS failed_recently,
           last.status AS last_status, last.created_at AS last_delivery_at
    FROM webhooks w
    LEFT JOIN projects p ON p.id = w.project_id
    LEFT JOIN users u ON u.id = w.created_by
    LEFT JOIN LATERAL (
        SELECT d.status, d.created_at FROM webhook_deliveries d
        WHERE d.webhook_id = w.id ORDER BY d.created_at DESC LIMIT 1
    ) last ON TRUE";

/// One entry of the outbox.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Delivery {
    /// Sent as `X-TeamCraft-Delivery`.
    pub id: Uuid,
    pub webhook_id: Uuid,
    /// The payload's `id`.
    pub event_id: Uuid,
    pub event: String,
    pub payload: Value,
    /// `pending`, `sent` or `failed`.
    pub status: String,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_status_code: Option<i32>,
    pub last_error: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

const DELIVERY_SELECT: &str = "
    SELECT id, webhook_id, event_id, event, payload, status, attempts, next_attempt_at,
           last_status_code, last_error, created_at, updated_at, delivered_at
    FROM webhook_deliveries";

/// What an administrator enters.
#[derive(Clone, Debug)]
pub struct WebhookInput {
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub project_id: Option<Uuid>,
    pub active: bool,
}

struct Valid {
    name: String,
    url: String,
    events: Vec<String>,
}

fn validate(policy: &TargetPolicy, input: &WebhookInput) -> AppResult<Valid> {
    let name = domain::require_text("Name", &input.name, 100)?;
    if input.url.trim().chars().count() > 2000 {
        return Err(AppError::BadRequest(
            "The URL is longer than 2000 characters.".into(),
        ));
    }
    let url = policy
        .check_url(&input.url)
        .map_err(AppError::BadRequest)?
        .to_string();
    let mut events = Vec::new();
    for raw in &input.events {
        let event = WebhookEvent::parse(raw)
            .ok_or_else(|| AppError::BadRequest(format!("Unknown event '{}'.", raw.trim())))?;
        if !events.contains(&event) {
            events.push(event);
        }
    }
    if events.is_empty() {
        return Err(AppError::BadRequest(
            "Choose at least one event for the webhook.".into(),
        ));
    }
    // Stored in the canonical order of the event list.
    let events = WebhookEvent::ALL
        .iter()
        .filter(|e| events.contains(e))
        .map(|e| e.as_str().to_string())
        .collect();
    Ok(Valid { name, url, events })
}

/// The host of a URL (audit records never contain the full URL: its path or
/// query may carry a token of the receiver).
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn new_secret() -> (String, String) {
    let secret = auth::generate_token(SECRET_PREFIX);
    let hint = format!("{SECRET_PREFIX}…{}", &secret[secret.len() - 4..]);
    (secret, hint)
}

async fn ensure_project(conn: &mut PgConnection, project_id: Option<Uuid>) -> AppResult<()> {
    if let Some(id) = project_id {
        let exists =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *conn)
                .await?;
        if !exists {
            return Err(AppError::BadRequest("Unknown project group.".into()));
        }
    }
    Ok(())
}

pub async fn list(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Webhook>> {
    actor.require(Role::Admin)?;
    Ok(sqlx::query_as::<_, Webhook>(&format!(
        "{WEBHOOK_SELECT} ORDER BY lower(w.name), w.created_at"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Webhook> {
    actor.require(Role::Admin)?;
    fetch(pool, id).await
}

async fn fetch(pool: &PgPool, id: Uuid) -> AppResult<Webhook> {
    sqlx::query_as::<_, Webhook>(&format!("{WEBHOOK_SELECT} WHERE w.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Webhook not found."))
}

/// Locks a webhook row; returns its name and URL.
async fn lock(conn: &mut PgConnection, id: Uuid) -> AppResult<(String, String, bool)> {
    sqlx::query_as::<_, (String, String, bool)>(
        "SELECT name, url, active FROM webhooks WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::not_found("Webhook not found."))
}

/// Registers a webhook. Returns it and its secret – the only time the
/// secret leaves the server.
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    policy: &TargetPolicy,
    input: WebhookInput,
) -> AppResult<(Webhook, String)> {
    actor.require(Role::Admin)?;
    let v = validate(policy, &input)?;
    let mut tx = pool.begin().await?;
    // Serialises concurrent creations for the limit check.
    sqlx::query("LOCK TABLE webhooks IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM webhooks")
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_WEBHOOKS {
        return Err(AppError::Conflict(format!(
            "There are already {MAX_WEBHOOKS} webhooks; delete one first."
        )));
    }
    ensure_project(&mut tx, input.project_id).await?;
    let (secret, hint) = new_secret();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, secret_hint, events, project_id, active, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(&v.name)
    .bind(&v.url)
    .bind(&secret)
    .bind(&hint)
    .bind(&v.events)
    .bind(input.project_id)
    .bind(input.active)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.create",
        "webhook",
        Some(id),
        format!("Created webhook {} → {}", v.name, host_of(&v.url)),
        json!({ "host": host_of(&v.url), "events": v.events, "project_id": input.project_id,
                "active": input.active }),
    )
    .await?;
    tx.commit().await?;
    Ok((fetch(pool, id).await?, secret))
}

/// Changes name, URL, events, project group and the active flag. Pending
/// deliveries of a webhook that is switched off are given up.
pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    policy: &TargetPolicy,
    id: Uuid,
    input: WebhookInput,
) -> AppResult<Webhook> {
    actor.require(Role::Admin)?;
    let v = validate(policy, &input)?;
    let mut tx = pool.begin().await?;
    let (old_name, old_url, was_active) = lock(&mut tx, id).await?;
    ensure_project(&mut tx, input.project_id).await?;
    sqlx::query(
        "UPDATE webhooks SET name = $2, url = $3, events = $4, project_id = $5, active = $6,
                updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&v.name)
    .bind(&v.url)
    .bind(&v.events)
    .bind(input.project_id)
    .bind(input.active)
    .execute(&mut *tx)
    .await?;
    let dropped = if was_active && !input.active {
        sqlx::query(
            "UPDATE webhook_deliveries
             SET status = 'failed', last_error = 'The webhook was deactivated.', updated_at = now()
             WHERE webhook_id = $1 AND status = 'pending'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
    } else {
        0
    };
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.update",
        "webhook",
        Some(id),
        format!(
            "Changed webhook {}{}",
            v.name,
            match (was_active, input.active) {
                (true, false) => " (deactivated)",
                (false, true) => " (activated)",
                _ => "",
            }
        ),
        json!({ "name_before": old_name, "host_before": host_of(&old_url), "host": host_of(&v.url),
                "url_changed": old_url != v.url, "events": v.events, "project_id": input.project_id,
                "active": input.active, "pending_given_up": dropped }),
    )
    .await?;
    tx.commit().await?;
    fetch(pool, id).await
}

/// Deletes a webhook together with its deliveries.
pub async fn delete(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<String> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let (name, url, _) = lock(&mut tx, id).await?;
    let deliveries = sqlx::query("DELETE FROM webhook_deliveries WHERE webhook_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.delete",
        "webhook",
        Some(id),
        format!("Deleted webhook {name} → {}", host_of(&url)),
        json!({ "host": host_of(&url), "deliveries": deliveries }),
    )
    .await?;
    tx.commit().await?;
    Ok(name)
}

/// Replaces the secret. Returns the webhook and the new secret (shown once).
/// Deliveries that are still pending are signed with the new secret.
pub async fn rotate_secret(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<(Webhook, String)> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let (name, _, _) = lock(&mut tx, id).await?;
    let (secret, hint) = new_secret();
    sqlx::query(
        "UPDATE webhooks SET secret = $2, secret_hint = $3, secret_rotated_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&secret)
    .bind(&hint)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.rotate_secret",
        "webhook",
        Some(id),
        format!("Rotated the secret of webhook {name}"),
        json!({ "secret_hint": hint }),
    )
    .await?;
    tx.commit().await?;
    Ok((fetch(pool, id).await?, secret))
}

/// Queues a `ping` delivery to an active webhook; returns its id (the
/// caller may send it at once with `Dispatcher::deliver_now`).
pub async fn send_test(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Uuid> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let (name, _, active) = lock(&mut tx, id).await?;
    if !active {
        return Err(AppError::Conflict(format!(
            "Webhook {name} is inactive; activate it before sending a test."
        )));
    }
    let event_id = Uuid::new_v4();
    let mut payload = envelope("ping", event_id, Some(actor));
    payload.insert(
        "webhook".into(),
        json!({ "id": id, "name": name, "web_url": format!("/admin/webhooks/{id}"),
                "api_url": format!("/api/v1/webhooks/{id}") }),
    );
    payload.insert(
        "message".into(),
        json!("Test delivery – the webhook is reachable."),
    );
    let delivery_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event, payload)
         VALUES ($1, $2, $3, 'ping', $4)",
    )
    .bind(delivery_id)
    .bind(id)
    .bind(event_id)
    .bind(Value::Object(payload))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.test",
        "webhook",
        Some(id),
        format!("Sent a test delivery to webhook {name}"),
        json!({ "delivery_id": delivery_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(delivery_id)
}

/// Schedules a failed (or waiting) delivery again, now, with a fresh set of
/// attempts. The delivery id stays the same, so receivers can deduplicate.
pub async fn retry(pool: &PgPool, actor: &Actor, delivery_id: Uuid) -> AppResult<Delivery> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query_as::<_, (Uuid, String, String, bool)>(
        "SELECT d.webhook_id, d.status, w.name, w.active
         FROM webhook_deliveries d JOIN webhooks w ON w.id = d.webhook_id
         WHERE d.id = $1 FOR UPDATE OF d",
    )
    .bind(delivery_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Delivery not found."))?;
    let (webhook_id, status, name, active) = row;
    if status == "sent" {
        return Err(AppError::Conflict(
            "This delivery was already sent successfully.".into(),
        ));
    }
    if !active {
        return Err(AppError::Conflict(format!(
            "Webhook {name} is inactive; activate it before retrying."
        )));
    }
    sqlx::query(
        "UPDATE webhook_deliveries
         SET status = 'pending', attempts = 0, next_attempt_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(delivery_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "webhook.retry",
        "webhook",
        Some(webhook_id),
        format!("Retried a delivery of webhook {name}"),
        json!({ "delivery_id": delivery_id, "status_before": status }),
    )
    .await?;
    tx.commit().await?;
    get_delivery(pool, actor, delivery_id).await
}

/// The latest deliveries of a webhook, newest first.
pub async fn deliveries(
    pool: &PgPool,
    actor: &Actor,
    webhook_id: Uuid,
    limit: i64,
) -> AppResult<Vec<Delivery>> {
    actor.require(Role::Admin)?;
    Ok(sqlx::query_as::<_, Delivery>(&format!(
        "{DELIVERY_SELECT} WHERE webhook_id = $1 ORDER BY created_at DESC, id LIMIT $2"
    ))
    .bind(webhook_id)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?)
}

pub async fn get_delivery(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Delivery> {
    actor.require(Role::Admin)?;
    sqlx::query_as::<_, Delivery>(&format!("{DELIVERY_SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Delivery not found."))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

fn envelope(event: &str, id: Uuid, actor: Option<&Actor>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("event".into(), json!(event));
    map.insert("id".into(), json!(id));
    map.insert(
        "occurred_at".into(),
        json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    map.insert(
        "actor".into(),
        json!({ "username": actor.map(|a| a.username.as_str()).unwrap_or("system") }),
    );
    map
}

/// Builds the payload of an event: the envelope plus `objects` (a JSON
/// object whose keys are added at the top level).
pub fn payload(event: WebhookEvent, id: Uuid, actor: Option<&Actor>, objects: Value) -> Value {
    let mut map = envelope(event.as_str(), id, actor);
    if let Value::Object(objects) = objects {
        for (k, v) in objects {
            map.entry(k).or_insert(v);
        }
    }
    Value::Object(map)
}

/// Queues an event for every active webhook that subscribed to it and whose
/// project group (if any) is one of `projects`. Call it inside the event's
/// transaction. Returns the number of deliveries queued.
///
/// A webhook limited to a project group receives only the entries of a
/// top-level `revisions` list that belong to its group (a change can hold
/// revisions of several groups; the other groups' items are none of that
/// receiver's business).
pub async fn emit(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    event: WebhookEvent,
    projects: &[Option<Uuid>],
    objects: Value,
) -> AppResult<u64> {
    let projects: Vec<Uuid> = projects.iter().flatten().copied().collect();
    let event_id = Uuid::new_v4();
    let body = payload(event, event_id, actor, objects);
    Ok(sqlx::query(
        "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event, payload)
         SELECT gen_random_uuid(), w.id, $1, $2,
                CASE WHEN p.code IS NULL OR jsonb_typeof($3::jsonb -> 'revisions') IS DISTINCT FROM 'array'
                     THEN $3::jsonb
                     ELSE jsonb_set($3::jsonb, '{revisions}', COALESCE(
                         (SELECT jsonb_agg(r.value ORDER BY r.ordinality)
                          FROM jsonb_array_elements($3::jsonb -> 'revisions') WITH ORDINALITY AS r
                          WHERE r.value ->> 'project' = p.code),
                         '[]'::jsonb))
                END
         FROM webhooks w
         LEFT JOIN projects p ON p.id = w.project_id
         WHERE w.active AND $2 = ANY(w.events)
           AND (w.project_id IS NULL OR w.project_id = ANY($4))",
    )
    .bind(event_id)
    .bind(event.as_str())
    .bind(body)
    .bind(&projects)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// One event per entry of `each` (e.g. `revision.released` for every
/// revision of a change) in a single statement: each entry is
/// `(project group, objects)`. Returns the number of deliveries queued.
pub(crate) async fn emit_each(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    event: WebhookEvent,
    each: Vec<(Option<Uuid>, Value)>,
) -> AppResult<u64> {
    if each.is_empty() {
        return Ok(0);
    }
    let mut ids = Vec::with_capacity(each.len());
    let mut projects = Vec::with_capacity(each.len());
    let mut bodies = Vec::with_capacity(each.len());
    for (project, objects) in each {
        let id = Uuid::new_v4();
        ids.push(id);
        projects.push(project);
        bodies.push(payload(event, id, actor, objects));
    }
    Ok(sqlx::query(
        "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event, payload)
         SELECT gen_random_uuid(), w.id, e.event_id, $1, e.payload
         FROM unnest($2::uuid[], $3::uuid[], $4::jsonb[]) AS e (event_id, project_id, payload)
         JOIN webhooks w ON w.active AND $1 = ANY(w.events)
                        AND (w.project_id IS NULL OR w.project_id = e.project_id)",
    )
    .bind(event.as_str())
    .bind(&ids)
    .bind(&projects)
    .bind(&bodies)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// Whether any active webhook subscribed to one of `events` (lets a service
/// skip building payloads nobody receives).
pub(crate) async fn wanted(conn: &mut PgConnection, events: &[WebhookEvent]) -> AppResult<bool> {
    let names: Vec<&str> = events.iter().map(|e| e.as_str()).collect();
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM webhooks WHERE active AND events && $1::text[])",
    )
    .bind(&names)
    .fetch_one(&mut *conn)
    .await?)
}

/// A revision in a payload. `state` is the state after the event.
pub fn revision_object(rev: &Revision, state: &str) -> Value {
    json!({
        "id": rev.id,
        "item_id": rev.item_id,
        "item_number": rev.item_number,
        "item_type": rev.item_type,
        "code": rev.code,
        "label": format!("{}/{}", rev.item_number, rev.code),
        "name": rev.name,
        "state": state,
        "project": rev.project_code,
        "web_url": format!("/revisions/{}", rev.id),
        "api_url": format!("/api/v1/items/{}/revisions/{}",
                           util::pct_encode(&rev.item_number), util::pct_encode(&rev.code)),
    })
}

/// A change in a payload. `status` is the status after the event.
pub fn change_object(change: &Change, status: &str) -> Value {
    json!({
        "id": change.id,
        "number": change.number,
        "title": change.title,
        "priority": change.priority,
        "status": status,
        "author": change.created_by_name,
        "web_url": format!("/changes/{}", change.id),
        "api_url": format!("/api/v1/changes/{}", util::pct_encode(&change.number)),
    })
}

/// A change request in a payload (with its current status).
pub fn request_object(request: &ChangeRequest) -> Value {
    json!({
        "id": request.id,
        "number": request.number,
        "title": request.title,
        "reason": request.reason,
        "priority": request.priority,
        "status": request.status,
        "requested_by": request.requested_by_name,
        "due_date": request.due_date,
        "web_url": format!("/requests/{}", request.id),
        "api_url": format!("/api/v1/change-requests/{}", util::pct_encode(&request.number)),
    })
}

/// The revisions of a change as payload objects (current states) and the
/// project group of each.
pub(crate) async fn change_revisions(
    conn: &mut PgConnection,
    change_id: Uuid,
) -> AppResult<(Vec<Value>, Vec<Option<Uuid>>)> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        item_id: Uuid,
        item_number: String,
        item_type: String,
        code: String,
        name: String,
        state: String,
        project_id: Option<Uuid>,
        project_code: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT r.id, r.item_id, i.number AS item_number, i.item_type, r.code, r.name, r.state,
                i.project_id, p.code AS project_code
         FROM change_items ci
         JOIN revisions r ON r.id = ci.revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN projects p ON p.id = i.project_id
         WHERE ci.change_id = $1
         ORDER BY i.number, r.seq",
    )
    .bind(change_id)
    .fetch_all(&mut *conn)
    .await?;
    let projects = rows.iter().map(|r| r.project_id).collect();
    let objects = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "item_id": r.item_id,
                "item_number": r.item_number,
                "item_type": r.item_type,
                "code": r.code,
                "label": format!("{}/{}", r.item_number, r.code),
                "name": r.name,
                "state": r.state,
                "project": r.project_code,
                "web_url": format!("/revisions/{}", r.id),
                "api_url": format!("/api/v1/items/{}/revisions/{}",
                                   util::pct_encode(&r.item_number), util::pct_encode(&r.code)),
            })
        })
        .collect();
    Ok((objects, projects))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        Actor {
            id: Uuid::nil(),
            username: "erika".into(),
            display_name: "Erika".into(),
            role: Role::Editor,
            skin: String::new(),
            projects: Vec::new(),
        }
    }

    #[test]
    fn envelope_and_objects() {
        let id = Uuid::new_v4();
        let p = payload(
            WebhookEvent::RevisionObsoleted,
            id,
            Some(&actor()),
            json!({ "revision": { "id": 1 }, "event": "spoofed" }),
        );
        assert_eq!(
            p["event"], "revision.obsoleted",
            "objects cannot override the envelope"
        );
        assert_eq!(p["id"], json!(id));
        assert_eq!(p["actor"], json!({ "username": "erika" }));
        assert_eq!(p["revision"]["id"], 1);
        let at = p["occurred_at"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(at).is_ok() && at.ends_with('Z'));
        assert_eq!(
            payload(WebhookEvent::ChangeReleased, id, None, json!({}))["actor"]["username"],
            "system"
        );
    }

    #[test]
    fn secrets_and_hints() {
        let (secret, hint) = new_secret();
        assert!(secret.starts_with(SECRET_PREFIX) && secret.len() == SECRET_PREFIX.len() + 64);
        assert!(hint.starts_with(SECRET_PREFIX) && hint.ends_with(&secret[secret.len() - 4..]));
        assert!(
            hint.chars().count() < 20,
            "the hint does not reveal the secret"
        );
        assert_ne!(new_secret().0, secret);
    }

    #[test]
    fn input_validation() {
        let policy = TargetPolicy::default();
        let input = |url: &str, events: &[&str]| WebhookInput {
            name: "ERP export".into(),
            url: url.into(),
            events: events.iter().map(|e| e.to_string()).collect(),
            project_id: None,
            active: true,
        };
        let v = validate(
            &policy,
            &input(
                " https://erp.example.org/hook ",
                &["change.released", "revision.released", "change.released"],
            ),
        )
        .unwrap();
        assert_eq!(v.url, "https://erp.example.org/hook");
        assert_eq!(
            v.events,
            ["revision.released", "change.released"],
            "canonical order, once"
        );
        assert!(validate(&policy, &input("https://erp.example.org", &[])).is_err());
        assert!(validate(
            &policy,
            &input("https://erp.example.org", &["item.deleted"])
        )
        .is_err());
        assert!(validate(
            &policy,
            &input("http://192.168.0.10/", &["change.released"])
        )
        .is_err());
        assert!(validate(
            &policy,
            &input("ftp://x.example.org/", &["change.released"])
        )
        .is_err());
        assert_eq!(
            host_of("https://ci.example.org/x?token=abc"),
            "ci.example.org"
        );
    }

    #[test]
    fn object_urls_are_relative_and_encoded() {
        let now = Utc::now();
        let rev = Revision {
            id: Uuid::nil(),
            item_id: Uuid::nil(),
            item_number: "PRT-000001".into(),
            item_type: "part".into(),
            unit: "each".into(),
            project_id: None,
            project_code: Some("ROVER".into()),
            seq: 1,
            code: "A".into(),
            state: "in_review".into(),
            name: "Bracket".into(),
            description: String::new(),
            material: String::new(),
            mass_kg: None,
            unit_cost: None,
            checked_out_by: None,
            checked_out_by_name: None,
            checked_out_at: None,
            created_by: Uuid::nil(),
            created_by_name: "x".into(),
            created_at: now,
            updated_at: now,
            released_at: None,
        };
        let o = revision_object(&rev, "released");
        assert_eq!(o["state"], "released");
        assert_eq!(o["label"], "PRT-000001/A");
        assert_eq!(o["project"], "ROVER");
        assert_eq!(o["web_url"], format!("/revisions/{}", Uuid::nil()));
        assert_eq!(o["api_url"], "/api/v1/items/PRT-000001/revisions/A");
    }
}
