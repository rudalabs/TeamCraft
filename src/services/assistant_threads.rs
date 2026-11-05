//! The assistant's conversation: one thread per user that survives page
//! navigation and reloads.
//!
//! What is stored: the text of each question and of each answer, the quick
//! action, and *where* the question was asked (the label of the resolved page
//! context, for example `PRT-000009/A · bom`). **Not** stored: tool results,
//! the model's reasoning, the page context itself. A follow-up replays the
//! stored text to the model and calls the tools again with the user's
//! *current* rights, so nothing they lost access to comes back through a
//! tool. Only the owner reads a thread: every query here is keyed by the
//! user id of the actor, there is no function that takes a thread id from a
//! client. Threads expire after `TEAMCRAFT_ASSISTANT_RETENTION_DAYS` of
//! inactivity and go with the user.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::{auth::Actor, error::AppResult, services::audit};

/// Default retention of an idle thread (days).
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
/// Most messages replayed to the model on a follow-up (the newest ones).
pub const MAX_HISTORY_MESSAGES: usize = 16;
/// Most characters replayed per message.
pub const MAX_HISTORY_CHARS: usize = 8_000;
/// Most messages kept in a thread; the oldest go first.
pub const MAX_STORED_MESSAGES: i64 = 200;

/// One stored message, as the thread view shows it.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct StoredMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    /// The quick action of a user message (`ask` for a free question).
    pub action: String,
    /// Kind and label of the page the user asked on (user messages).
    pub context_kind: String,
    pub context_label: String,
    /// The run that produced this answer had read restricted content (see
    /// `assistant::McpTools`); a follow-up starts tainted.
    pub tainted: bool,
    pub created_at: DateTime<Utc>,
}

/// The thread of a user.
#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub messages: Vec<StoredMessage>,
    pub retention_days: u32,
}

/// Deletes the idle threads of everybody (the periodic clean-up) and
/// returns how many went.
pub async fn purge_expired(pool: &PgPool, retention_days: u32) -> AppResult<u64> {
    let done = sqlx::query(
        "DELETE FROM assistant_threads WHERE updated_at < now() - make_interval(days => $1)",
    )
    .bind(retention_days as i32)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// The user's thread, newest message last. An expired thread is deleted and
/// reads as empty.
pub async fn load(pool: &PgPool, me: &Actor, retention_days: u32) -> AppResult<Thread> {
    sqlx::query(
        "DELETE FROM assistant_threads
         WHERE user_id = $1 AND updated_at < now() - make_interval(days => $2)",
    )
    .bind(me.id)
    .bind(retention_days as i32)
    .execute(pool)
    .await?;
    let messages = sqlx::query_as::<_, StoredMessage>(
        "SELECT m.role, m.text, m.action, m.context_kind, m.context_label, m.tainted, m.created_at
         FROM assistant_messages m
         JOIN assistant_threads t ON t.id = m.thread_id
         WHERE t.user_id = $1
         ORDER BY m.seq",
    )
    .bind(me.id)
    .fetch_all(pool)
    .await?;
    Ok(Thread {
        messages,
        retention_days,
    })
}

/// What a follow-up replays to the model: the newest messages, each cut.
pub fn history_of(thread: &Thread) -> Vec<(String, String)> {
    let skip = thread.messages.len().saturating_sub(MAX_HISTORY_MESSAGES);
    let mut out: Vec<(String, String)> = thread
        .messages
        .iter()
        .skip(skip)
        .map(|m| {
            let text: String = m.text.chars().take(MAX_HISTORY_CHARS).collect();
            (m.role.clone(), text)
        })
        .collect();
    // the conversation must start with the user
    while out.first().is_some_and(|(role, _)| role != "user") {
        out.remove(0);
    }
    out
}

/// Whether an earlier answer of the thread came from a tainted run.
pub fn was_tainted(thread: &Thread) -> bool {
    thread.messages.iter().any(|m| m.tainted)
}

/// A question and its answer to append.
pub struct Exchange<'a> {
    pub action: &'a str,
    /// What the user typed, or the label of the quick action.
    pub question: &'a str,
    pub context_kind: &'a str,
    pub context_label: &'a str,
    pub answer: &'a str,
    pub tainted: bool,
}

/// Appends a question and its answer in one transaction (the thread is
/// created on the first exchange). Questions that were cancelled or failed
/// are never stored.
pub async fn append(conn: &mut PgConnection, me: &Actor, x: &Exchange<'_>) -> AppResult<()> {
    let thread: Uuid = sqlx::query_scalar(
        "INSERT INTO assistant_threads (id, user_id) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(me.id)
    .fetch_one(&mut *conn)
    .await?;
    // serialise concurrent questions of the same user (two tabs)
    sqlx::query("SELECT id FROM assistant_threads WHERE id = $1 FOR UPDATE")
        .bind(thread)
        .fetch_one(&mut *conn)
        .await?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM assistant_messages WHERE thread_id = $1",
    )
    .bind(thread)
    .fetch_one(&mut *conn)
    .await?;
    let rows = [
        ("user", x.question, x.context_kind, x.context_label, false),
        ("assistant", x.answer, "", "", x.tainted),
    ];
    for (n, (role, text, kind, label, tainted)) in rows.into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO assistant_messages
                 (thread_id, seq, role, text, action, context_kind, context_label, tainted)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(thread)
        .bind(next + n as i32)
        .bind(role)
        .bind(text)
        .bind(if role == "user" { x.action } else { "" })
        .bind(kind)
        .bind(label)
        .bind(tainted)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "DELETE FROM assistant_messages
         WHERE thread_id = $1
           AND seq <= (SELECT MAX(seq) FROM assistant_messages WHERE thread_id = $1) - $2",
    )
    .bind(thread)
    .bind(MAX_STORED_MESSAGES)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// "New conversation": deletes the user's thread.
pub async fn reset(pool: &PgPool, me: &Actor) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query("DELETE FROM assistant_threads WHERE user_id = $1")
        .bind(me.id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() > 0 {
        audit::record(
            &mut tx,
            Some(me),
            "assistant.thread_reset",
            "assistant",
            None,
            "Assistant: conversation deleted",
            serde_json::json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str, tainted: bool) -> StoredMessage {
        StoredMessage {
            role: role.into(),
            text: text.into(),
            action: "ask".into(),
            context_kind: String::new(),
            context_label: String::new(),
            tainted,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn history_is_capped_and_starts_with_the_user() {
        let mut messages = Vec::new();
        for n in 0..20 {
            messages.push(msg("user", &format!("q{n}"), false));
            messages.push(msg("assistant", &"a".repeat(MAX_HISTORY_CHARS + 50), false));
        }
        let thread = Thread {
            messages,
            retention_days: 30,
        };
        let h = history_of(&thread);
        assert_eq!(h.len(), MAX_HISTORY_MESSAGES);
        assert_eq!(h[0].0, "user");
        assert_eq!(h[0].1, "q12");
        assert!(h
            .iter()
            .all(|(_, t)| t.chars().count() <= MAX_HISTORY_CHARS));
        // an odd cut drops the leading answer
        let thread = Thread {
            messages: vec![msg("assistant", "x", false), msg("user", "y", false)],
            retention_days: 30,
        };
        assert_eq!(
            history_of(&thread),
            vec![("user".to_string(), "y".to_string())]
        );
    }

    #[test]
    fn taint_is_remembered() {
        let mut thread = Thread {
            messages: vec![msg("user", "q", false), msg("assistant", "a", false)],
            retention_days: 30,
        };
        assert!(!was_tainted(&thread));
        thread.messages[1].tainted = true;
        assert!(was_tainted(&thread));
    }
}
