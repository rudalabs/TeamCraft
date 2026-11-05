//! Notifications and watch lists.
//!
//! Services create notifications inside the transaction of the event that
//! causes them (a change is submitted, a reviewer decides, a dependent is
//! flagged, …), so they exist exactly when the event happened. Each user sees
//! them in the web UI (bell and page), through the API and in the desktop
//! app; e-mail and Mattermost delivery read the same rows as an outbox
//! (`crate::delivery`). Users mute kinds and switch channels off on their
//! account page. The actor of an event is never notified about it.
//!
//! Who is notified:
//!
//! | Event                                   | Recipients                                         |
//! |-----------------------------------------|----------------------------------------------------|
//! | change submitted                        | reviewers (review request), watchers of the change |
//! | reviewer decides, change released / rejected / withdrawn / cancelled | author, reviewers, watchers of the change |
//! | revision released, revised, made obsolete, checked in | watchers of the item                   |
//! | dependent flagged (change propagation)  | creator of the dependent, author of its latest revision, its watchers |
//! | check-out released by somebody else     | the holder of the lock                             |
//! | change request raised                   | CAD editors of the affected project groups, watchers of the affected items |
//! | change request triaged, assigned, commented, decided, implemented | requester, assignee, commenters, watchers of the request |
//!
//! Creating an item makes its creator a watcher.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, NotificationKind},
    error::{AppError, AppResult},
    models::Revision,
    services::{access, audit, audit::Mentions},
};

/// What a notification is about. Recipients who may not read the subject
/// are left out (`notify`), so a notification never carries data of a
/// restricted project group to outsiders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// An item (or one of its revisions).
    Item(Uuid),
    Change(Uuid),
    Request(Uuid),
    /// About the recipient's own account (test notifications).
    Personal,
}

/// The content of a notification.
#[derive(Clone, Debug)]
pub struct Notice {
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    /// Path in the web UI.
    pub link: String,
    /// `change`, `change_request` or `revision` (for the desktop app), or empty.
    pub subject_type: &'static str,
    /// Change or request number, or `<item number>/<revision code>`.
    pub subject_ref: String,
    /// What it is about – decides who may receive it.
    pub subject: Subject,
    /// Other items and change orders the text names (the source of an
    /// impact, the revisions of a change, the change that released an item)
    /// and the text for recipients who may not see all of them.
    pub redacted: Option<(Mentions, String, String)>,
}

impl Notice {
    pub fn change(
        kind: NotificationKind,
        change_id: Uuid,
        number: &str,
        title: String,
        body: String,
    ) -> Notice {
        Notice {
            kind,
            title,
            body,
            link: format!("/changes/{change_id}"),
            subject_type: "change",
            subject_ref: number.to_string(),
            subject: Subject::Change(change_id),
            redacted: None,
        }
    }

    pub fn request(
        kind: NotificationKind,
        request_id: Uuid,
        number: &str,
        title: String,
        body: String,
    ) -> Notice {
        Notice {
            kind,
            title,
            body,
            link: format!("/requests/{request_id}"),
            subject_type: "change_request",
            subject_ref: number.to_string(),
            subject: Subject::Request(request_id),
            redacted: None,
        }
    }

    pub fn revision(kind: NotificationKind, rev: &Revision, title: String, body: String) -> Notice {
        Notice::revision_ref(
            kind,
            rev.id,
            rev.item_id,
            &rev.item_number,
            &rev.code,
            title,
            body,
        )
    }

    pub fn revision_ref(
        kind: NotificationKind,
        revision_id: Uuid,
        item_id: Uuid,
        item_number: &str,
        code: &str,
        title: String,
        body: String,
    ) -> Notice {
        Notice {
            kind,
            title,
            body,
            link: format!("/revisions/{revision_id}"),
            subject_type: "revision",
            subject_ref: format!("{item_number}/{code}"),
            subject: Subject::Item(item_id),
            redacted: None,
        }
    }

    /// For recipients who may not see `item_id` (e.g. the source of an
    /// impact), use this title and body instead.
    pub fn redacted_for(self, item_id: Uuid, title: String, body: String) -> Notice {
        self.naming(Mentions::items([item_id]), title, body)
    }

    /// The text names `mentions`; recipients who may not see all of them get
    /// this title and body instead. Empty mentions change nothing.
    pub fn naming(mut self, mentions: Mentions, title: String, body: String) -> Notice {
        if !mentions.is_empty() {
            self.redacted = Some((mentions, title, body));
        }
        self
    }
}

fn subject_kind(subject: Subject) -> Option<&'static str> {
    match subject {
        Subject::Item(_) => Some("item"),
        Subject::Change(_) => Some("change"),
        Subject::Request(_) => Some("request"),
        Subject::Personal => None,
    }
}

/// SQL guard: the notification aliased `n` may still be read by the user
/// `actor` (an SQL expression) whose hidden groups are `hidden`: its subject
/// and everything its text names are visible, and none of the groups those
/// items were in when it was written is hidden (`mentioned_projects`: this
/// also covers items deleted since). Rows written before the subject was
/// stored got it from their link in migration 0027; personal ones (and
/// old rows whose subject is gone) have none.
pub(crate) fn notification_visible(n: &str, actor: &str, hidden: &str) -> String {
    format!(
        "(NOT ({n}.mentioned_projects && {hidden})
          AND ({n}.subject_kind IS DISTINCT FROM 'item' OR NOT EXISTS (
             SELECT 1 FROM items ni WHERE ni.id = {n}.subject_id AND NOT {item_subject}))
          AND ({n}.subject_kind IS DISTINCT FROM 'change' OR NOT EXISTS (
             SELECT 1 FROM changes nc WHERE nc.id = {n}.subject_id AND NOT {change_subject}))
          AND ({n}.subject_kind IS DISTINCT FROM 'request' OR NOT EXISTS (
             SELECT 1 FROM change_requests nq WHERE nq.id = {n}.subject_id AND NOT {request}))
          AND NOT EXISTS (SELECT 1 FROM items nmi WHERE nmi.id = ANY({n}.mentioned_items)
                          AND NOT {item_mention})
          AND NOT EXISTS (SELECT 1 FROM changes nmc WHERE nmc.id = ANY({n}.mentioned_changes)
                          AND NOT {change_mention}))",
        item_subject = access::project_visible_in("ni.project_id", hidden),
        change_subject = access::change_visible_for("nc", actor, hidden),
        request = access::request_visible_for("nq", actor, hidden),
        item_mention = access::project_visible_in("nmi.project_id", hidden),
        change_mention = access::change_visible_for("nmc", actor, hidden),
    )
}

/// SQL expression: the notification aliased `n` is confidential *now* – the
/// stored flag, or its subject or something its text names is in a
/// restricted group at delivery time (the group may have been restricted
/// after the row was queued).
pub fn confidential_now(n: &str) -> String {
    let restricted = "cp.visibility = 'restricted'";
    format!(
        "({n}.confidential
          OR EXISTS (SELECT 1 FROM projects cp WHERE cp.id = ANY({n}.mentioned_projects) AND {restricted})
          OR EXISTS (SELECT 1 FROM items ci JOIN projects cp ON cp.id = ci.project_id
                     WHERE {restricted}
                       AND (ci.id = ANY({n}.mentioned_items)
                            OR ({n}.subject_kind = 'item' AND ci.id = {n}.subject_id)))
          OR EXISTS (SELECT 1 FROM change_items cci JOIN revisions cr ON cr.id = cci.revision_id
                     JOIN items ci ON ci.id = cr.item_id JOIN projects cp ON cp.id = ci.project_id
                     WHERE {restricted}
                       AND (cci.change_id = ANY({n}.mentioned_changes)
                            OR ({n}.subject_kind = 'change' AND cci.change_id = {n}.subject_id)))
          OR EXISTS (SELECT 1 FROM change_request_items cri JOIN items ci ON ci.id = cri.item_id
                     JOIN projects cp ON cp.id = ci.project_id
                     WHERE {restricted} AND {n}.subject_kind = 'request'
                       AND cri.request_id = {n}.subject_id))"
    )
}

/// [`notification_visible`] for the actor (`$actor_n` = id, `$hidden_n` =
/// hidden groups).
fn visible_to(actor_n: usize, hidden_n: usize) -> String {
    notification_visible(
        "notifications",
        &format!("${actor_n}"),
        &format!("${hidden_n}"),
    )
}

/// The recipient-side SQL condition for a subject: the user aliased `u`
/// may read it. `$10` is the subject id.
fn recipient_sees(subject: Subject) -> String {
    let hidden = access::hidden_of_user("u");
    match subject {
        Subject::Item(_) => access::user_sees_project(
            "u",
            "(SELECT si.project_id FROM items si WHERE si.id = $10)",
        ),
        Subject::Change(_) => format!(
            "EXISTS (SELECT 1 FROM changes sc WHERE sc.id = $10 AND {})",
            access::change_visible_for("sc", "u.id", &hidden)
        ),
        Subject::Request(_) => format!(
            "EXISTS (SELECT 1 FROM change_requests sq WHERE sq.id = $10 AND {})",
            access::request_visible_for("sq", "u.id", &hidden)
        ),
        Subject::Personal => "($10::uuid IS NULL OR TRUE)".into(),
    }
}

/// Whether the subject touches a restricted project group (then chat
/// channels get only a generic text).
async fn is_confidential(conn: &mut PgConnection, notice: &Notice) -> AppResult<bool> {
    let restricted = "p.visibility = 'restricted'";
    let sql = match notice.subject {
        Subject::Item(_) => format!(
            "SELECT EXISTS(SELECT 1 FROM items i JOIN projects p ON p.id = i.project_id
                           WHERE i.id = $1 AND {restricted})"
        ),
        Subject::Change(_) => format!(
            "SELECT EXISTS(SELECT 1 FROM change_items ci JOIN revisions r ON r.id = ci.revision_id
                           JOIN items i ON i.id = r.item_id JOIN projects p ON p.id = i.project_id
                           WHERE ci.change_id = $1 AND {restricted})"
        ),
        Subject::Request(_) => format!(
            "SELECT EXISTS(SELECT 1 FROM change_request_items ri JOIN items i ON i.id = ri.item_id
                           JOIN projects p ON p.id = i.project_id
                           WHERE ri.request_id = $1 AND {restricted})"
        ),
        Subject::Personal => return Ok(false),
    };
    let mut confidential = sqlx::query_scalar::<_, bool>(&sql)
        .bind(subject_id(notice.subject))
        .fetch_one(&mut *conn)
        .await?;
    if let Some((mentions, _, _)) = &notice.redacted {
        confidential |= sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM items i JOIN projects p ON p.id = i.project_id
                           WHERE i.id = ANY($1) AND {restricted})
                 OR EXISTS(SELECT 1 FROM change_items ci JOIN revisions r ON r.id = ci.revision_id
                           JOIN items i ON i.id = r.item_id JOIN projects p ON p.id = i.project_id
                           WHERE ci.change_id = ANY($2) AND {restricted})"
        ))
        .bind(&mentions.items)
        .bind(&mentions.changes)
        .fetch_one(&mut *conn)
        .await?;
    }
    Ok(confidential)
}

fn subject_id(subject: Subject) -> Option<Uuid> {
    match subject {
        Subject::Item(id) | Subject::Change(id) | Subject::Request(id) => Some(id),
        Subject::Personal => None,
    }
}

/// Creates the notification for every recipient (each once, active users
/// only, not the actor, not for users who muted the kind, and only for
/// users who may read the subject – evaluated per recipient). E-mail and
/// Mattermost delivery are queued according to the user's settings.
/// Returns the number of notifications created.
pub(crate) async fn notify(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    recipients: &[Uuid],
    notice: &Notice,
) -> AppResult<u64> {
    if recipients.is_empty() {
        return Ok(0);
    }
    let confidential = is_confidential(conn, notice).await?;
    let sees = recipient_sees(notice.subject);
    // Recipients who may not see everything the text names get the redacted
    // text. The full text stores what it names, so the list re-checks it.
    let none = Mentions::default();
    let mentions = notice.redacted.as_ref().map(|(m, _, _)| m).unwrap_or(&none);
    let variants: Vec<(String, &str, &str, &Mentions)> = match &notice.redacted {
        None => vec![("TRUE".into(), &notice.title, &notice.body, &none)],
        Some((_, title, body)) => {
            let sees_all = format!(
                "(NOT EXISTS (SELECT 1 FROM items ri WHERE ri.id = ANY($12) AND NOT {item})
                  AND NOT EXISTS (SELECT 1 FROM changes rc WHERE rc.id = ANY($13) AND NOT {change}))",
                item = access::user_sees_project("u", "ri.project_id"),
                change = access::change_visible_for("rc", "u.id", &access::hidden_of_user("u")),
            );
            vec![
                (sees_all.clone(), &notice.title, &notice.body, mentions),
                (
                    format!("NOT {sees_all}"),
                    title.as_str(),
                    body.as_str(),
                    &none,
                ),
            ]
        }
    };
    let mut created = 0;
    for (condition, title, body, stored) in variants {
        created += sqlx::query(&format!(
            "INSERT INTO notifications (id, user_id, kind, title, body, link, subject_type, subject_ref,
                                        actor_name, email_state, chat_state, confidential,
                                        subject_kind, subject_id, mentioned_items, mentioned_changes,
                                        mentioned_projects)
             SELECT gen_random_uuid(), u.id, $2, $3, $4, $5, $6, $7, $8,
                    CASE WHEN u.notify_email AND COALESCE(u.email, '') <> '' THEN 'pending' ELSE 'none' END,
                    CASE WHEN u.notify_mattermost THEN 'pending' ELSE 'none' END,
                    $11, $16, $10, $14, $15,
                    ARRAY(SELECT DISTINCT pi.project_id FROM items pi
                          WHERE pi.project_id IS NOT NULL
                            AND (pi.id = ANY($14) OR ($16 = 'item' AND pi.id = $10)))
             FROM users u
             WHERE u.id = ANY($1) AND u.active AND u.id <> $9
               AND ($2 = 'test' OR NOT ($2 = ANY(u.notify_muted)))
               AND {sees}
               AND cardinality($12::uuid[]) >= 0 AND cardinality($13::uuid[]) >= 0
               AND {condition}"
        ))
        .bind(recipients)
        .bind(notice.kind.as_str())
        .bind(domain::truncate_chars(title, 300))
        .bind(domain::truncate_chars(body, 4000))
        .bind(&notice.link)
        .bind(notice.subject_type)
        .bind(&notice.subject_ref)
        .bind(actor.map(|a| a.display_name.as_str()).unwrap_or("TeamCraft"))
        .bind(actor.map(|a| a.id).unwrap_or(Uuid::nil()))
        .bind(subject_id(notice.subject))
        .bind(confidential)
        .bind(&mentions.items)
        .bind(&mentions.changes)
        .bind(&stored.items)
        .bind(&stored.changes)
        .bind(subject_kind(notice.subject))
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    Ok(created)
}

/// Users who watch any of the given entities.
pub(crate) async fn watchers(
    conn: &mut PgConnection,
    entity_type: &str,
    ids: &[Uuid],
) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT user_id FROM watches WHERE entity_type = $1 AND entity_id = ANY($2)",
    )
    .bind(entity_type)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?)
}

/// Author, reviewers and watchers of a change.
pub(crate) async fn change_people(
    conn: &mut PgConnection,
    change_id: Uuid,
) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT created_by FROM changes WHERE id = $1
         UNION SELECT reviewer_id FROM change_reviews WHERE change_id = $1
         UNION SELECT user_id FROM watches WHERE entity_type = 'change' AND entity_id = $1",
    )
    .bind(change_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Makes `user_id` watch an entity (no-op if it already does).
pub(crate) async fn add_watch(
    conn: &mut PgConnection,
    user_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO watches (user_id, entity_type, entity_id) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(entity_type)
    .bind(entity_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link: String,
    pub subject_type: String,
    pub subject_ref: String,
    pub actor_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `none`, `pending`, `sent`, `failed` or `skipped`.
    pub email_state: String,
    pub chat_state: String,
}

const NOTIFICATION_COLUMNS: &str =
    "id, kind, title, body, link, subject_type, subject_ref, actor_name,
    created_at, read_at, email_state, chat_state";

/// The newest notifications of a user.
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    unread_only: bool,
    limit: i64,
) -> AppResult<Vec<Notification>> {
    Ok(sqlx::query_as::<_, Notification>(&format!(
        "SELECT {NOTIFICATION_COLUMNS} FROM notifications
         WHERE user_id = $1 AND (NOT $2 OR read_at IS NULL) AND {}
         ORDER BY created_at DESC LIMIT $3",
        visible_to(1, 4)
    ))
    .bind(actor.id)
    .bind(unread_only)
    .bind(limit.clamp(1, 500))
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Unread notifications the actor may still read (a group restricted
/// since, or a lost membership, hides the older ones).
pub async fn unread_count(pool: &PgPool, actor: &Actor) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL AND {}",
        visible_to(1, 2)
    ))
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?)
}

/// Marks the given notifications (all if `None`) of the actor as read and
/// returns the remaining unread count.
pub async fn mark_read(pool: &PgPool, actor: &Actor, ids: Option<&[Uuid]>) -> AppResult<i64> {
    sqlx::query(
        "UPDATE notifications SET read_at = now()
         WHERE user_id = $1 AND read_at IS NULL AND ($2::uuid[] IS NULL OR id = ANY($2))",
    )
    .bind(actor.id)
    .bind(ids)
    .execute(pool)
    .await?;
    unread_count(pool, actor).await
}

/// Marks one notification of the actor as read and returns it (for its link).
pub async fn open(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Notification> {
    sqlx::query_as::<_, Notification>(&format!(
        "UPDATE notifications SET read_at = COALESCE(read_at, now())
         WHERE id = $1 AND user_id = $2 AND {} RETURNING {NOTIFICATION_COLUMNS}",
        visible_to(2, 3)
    ))
    .bind(id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("Notification not found."))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    pub email: bool,
    /// Where e-mails go (from the user account / the IdP).
    pub email_address: Option<String>,
    pub mattermost: bool,
    /// Mattermost username if it differs from the TeamCraft username.
    pub mattermost_username: Option<String>,
    /// Muted kinds.
    pub muted: Vec<String>,
}

pub async fn settings(pool: &PgPool, user_id: Uuid) -> AppResult<Settings> {
    #[derive(sqlx::FromRow)]
    struct Row {
        notify_email: bool,
        email: Option<String>,
        notify_mattermost: bool,
        mattermost_username: Option<String>,
        notify_muted: Vec<String>,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT notify_email, email, notify_mattermost, mattermost_username, notify_muted
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("User not found."))?;
    Ok(Settings {
        email: row.notify_email,
        email_address: row.email.filter(|e| !e.is_empty()),
        mattermost: row.notify_mattermost,
        mattermost_username: row.mattermost_username,
        muted: row.notify_muted,
    })
}

#[derive(Clone, Debug)]
pub struct SettingsUpdate {
    pub email: bool,
    pub mattermost: bool,
    pub mattermost_username: Option<String>,
    pub muted: Vec<String>,
}

/// Mattermost usernames: 1–64 characters `a-z 0-9 . _ -`, without a leading `@`.
fn clean_mattermost_username(raw: Option<&str>) -> AppResult<Option<String>> {
    let Some(name) = raw
        .map(|n| n.trim().trim_start_matches('@').to_lowercase())
        .filter(|n| !n.is_empty())
    else {
        return Ok(None);
    };
    if name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(AppError::BadRequest(format!(
            "'{name}' is not a valid Mattermost username."
        )));
    }
    Ok(Some(name))
}

pub async fn update_settings(
    pool: &PgPool,
    actor: &Actor,
    upd: SettingsUpdate,
) -> AppResult<Settings> {
    let mattermost_username = clean_mattermost_username(upd.mattermost_username.as_deref())?;
    let mut muted = Vec::new();
    for kind in &upd.muted {
        match domain::NotificationKind::parse(kind) {
            Some(k) if NotificationKind::MUTABLE.contains(&k) => {
                if !muted.contains(&k.as_str().to_string()) {
                    muted.push(k.as_str().to_string());
                }
            }
            _ => {
                return Err(AppError::BadRequest(format!(
                    "Unknown notification kind '{kind}'."
                )))
            }
        }
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE users SET notify_email = $2, notify_mattermost = $3, mattermost_username = $4,
                          notify_muted = $5
         WHERE id = $1",
    )
    .bind(actor.id)
    .bind(upd.email)
    .bind(upd.mattermost)
    .bind(&mattermost_username)
    .bind(&muted)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "user.notifications",
        "user",
        Some(actor.id),
        format!(
            "{} changed the notification settings (e-mail {}, Mattermost {}, muted: {})",
            actor.username,
            if upd.email { "on" } else { "off" },
            if upd.mattermost { "on" } else { "off" },
            if muted.is_empty() {
                "nothing".to_string()
            } else {
                muted.join(", ")
            }
        ),
        json!({ "email": upd.email, "mattermost": upd.mattermost, "muted": muted }),
    )
    .await?;
    tx.commit().await?;
    settings(pool, actor.id).await
}

/// Sends a test notification to the actor (checks e-mail and Mattermost).
pub async fn send_test(pool: &PgPool, actor: &Actor) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let notice = Notice {
        kind: NotificationKind::Test,
        title: "Test notification".into(),
        body: format!(
            "Hello {}, this is a test message from TeamCraft. If it arrives by e-mail or in \
             Mattermost, these channels work.",
            actor.display_name
        ),
        link: "/notifications".into(),
        subject_type: "",
        subject_ref: String::new(),
        subject: Subject::Personal,
        redacted: None,
    };
    // The actor is the recipient here, so it is not excluded as the sender.
    notify(&mut tx, None, &[actor.id], &notice).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Watch lists
// ---------------------------------------------------------------------------

/// Starts (`watch = true`) or stops watching an item or a change. Returns
/// whether the actor watches it afterwards.
pub async fn set_watch(
    pool: &PgPool,
    actor: &Actor,
    entity_type: &str,
    entity_id: Uuid,
    watch: bool,
) -> AppResult<bool> {
    let mut conn = pool.acquire().await?;
    // Hidden objects give the same "not found" as missing ones.
    let exists = match entity_type {
        "item" => {
            sqlx::query_scalar::<_, bool>(&format!(
                "SELECT EXISTS(SELECT 1 FROM items i WHERE i.id = $1 AND {})",
                access::item_visible("i", 2)
            ))
            .bind(entity_id)
            .bind(&actor.hidden_projects)
            .fetch_one(&mut *conn)
            .await?
        }
        "change" => crate::services::changes::is_visible(&mut conn, actor, entity_id).await?,
        "change_request" => {
            crate::services::requests::is_visible(&mut conn, actor, entity_id).await?
        }
        _ => false,
    };
    if !exists {
        return Err(AppError::not_found(format!("No such {entity_type}.")));
    }
    if watch {
        add_watch(&mut conn, actor.id, entity_type, entity_id).await?;
    } else {
        sqlx::query(
            "DELETE FROM watches WHERE user_id = $1 AND entity_type = $2 AND entity_id = $3",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(watch)
}

pub async fn is_watching(
    pool: &PgPool,
    user_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM watches WHERE user_id = $1 AND entity_type = $2 AND entity_id = $3)",
    )
    .bind(user_id)
    .bind(entity_type)
    .bind(entity_id)
    .fetch_one(pool)
    .await?)
}

/// What a user watches, for the notifications page and the API.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Watched {
    pub entity_type: String,
    pub entity_id: Uuid,
    /// Item number or change number.
    pub number: String,
    pub name: String,
    pub state: String,
    /// Page in the web UI.
    pub link: String,
    /// Latest revision code (items).
    pub revision_code: Option<String>,
}

/// What the actor watches – only what it may still read (the watches of
/// objects it lost access to stay and come back with the access).
pub async fn watched(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Watched>> {
    Ok(sqlx::query_as::<_, Watched>(&format!(
        "SELECT 'item' AS entity_type, i.id AS entity_id, i.number, lr.name, lr.state,
                '/revisions/' || lr.id AS link, lr.code AS revision_code
         FROM watches w JOIN items i ON i.id = w.entity_id
         CROSS JOIN LATERAL (SELECT r.id, r.code, r.name, r.state FROM revisions r
                             WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1) lr
         WHERE w.user_id = $1 AND w.entity_type = 'item' AND {item}
         UNION ALL
         SELECT 'change', c.id, c.number, c.title, c.status, '/changes/' || c.id, NULL
         FROM watches w JOIN changes c ON c.id = w.entity_id
         WHERE w.user_id = $1 AND w.entity_type = 'change' AND {change}
         UNION ALL
         SELECT 'change_request', cr.id, cr.number, cr.title, cr.status, '/requests/' || cr.id, NULL
         FROM watches w JOIN change_requests cr ON cr.id = w.entity_id
         WHERE w.user_id = $1 AND w.entity_type = 'change_request' AND {request}
         ORDER BY 1, 3",
        item = access::item_visible("i", 2),
        change = access::change_visible("c", 1, 2),
        request = access::request_visible("cr", 1, 2),
    ))
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mattermost_usernames() {
        assert_eq!(
            clean_mattermost_username(Some(" @Erika.Muster ")).unwrap(),
            Some("erika.muster".into())
        );
        assert_eq!(clean_mattermost_username(Some("  ")).unwrap(), None);
        assert_eq!(clean_mattermost_username(None).unwrap(), None);
        assert!(clean_mattermost_username(Some("a b")).is_err());
        assert!(clean_mattermost_username(Some(&"x".repeat(65))).is_err());
    }

    #[test]
    fn notices_link_to_their_subject() {
        let id = Uuid::nil();
        let n = Notice::change(
            NotificationKind::ReviewRequested,
            id,
            "CHG-000004",
            "t".into(),
            "b".into(),
        );
        assert_eq!(n.link, format!("/changes/{id}"));
        assert_eq!(
            (n.subject_type, n.subject_ref.as_str()),
            ("change", "CHG-000004")
        );
        let n = Notice::revision_ref(
            NotificationKind::Impact,
            id,
            id,
            "ASM-000001",
            "B",
            "t".into(),
            "b".into(),
        );
        assert_eq!(n.subject_ref, "ASM-000001/B");
        assert_eq!(n.link, format!("/revisions/{id}"));
        let n = Notice::request(
            NotificationKind::ChangeRequest,
            id,
            "ECR-000002",
            "t".into(),
            "b".into(),
        );
        assert_eq!(n.link, format!("/requests/{id}"));
        assert_eq!(
            (n.subject_type, n.subject_ref.as_str()),
            ("change_request", "ECR-000002")
        );
    }
}
