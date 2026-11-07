//! Read and download log of restricted groups (#151, step 4).
//!
//! Only groups whose need-to-know policy has `read_log` write here, so the
//! volume stays bounded and nothing is logged for other groups. The log is
//! separate from the audit log, which records changes. An entry says who
//! viewed or downloaded what, through which channel, from which address.
//!
//! Repeated reads of the same object by the same user through the same
//! channel within [`COLLAPSE_MINUTES`] are one entry, so a resumed or ranged
//! download (many requests) is logged once.
//!
//! Not logged yet: MCP and assistant tool reads, desktop Get (see
//! `docs/OPEN_WORK.md`).

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    services::{audit, projects},
};

/// Reads of one object within this window are logged once.
pub const COLLAPSE_MINUTES: i32 = 5;

/// Most entries one listing returns.
pub const MAX_LIST: i64 = 1000;

/// The way a request came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Browser session.
    Web,
    /// Bearer token: REST API, desktop app.
    Api,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Web => "web",
            Channel::Api => "api",
        }
    }
}

/// What was done with the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An item or revision page (or the same data over the REST API).
    View,
    /// A file or file version.
    Download,
    /// Pack and Go.
    Package,
    /// A BOM export (CSV, Excel).
    Export,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::View => "view",
            Kind::Download => "download",
            Kind::Package => "package",
            Kind::Export => "export",
        }
    }

    pub fn parse(raw: &str) -> Option<Kind> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "view" => Some(Kind::View),
            "download" => Some(Kind::Download),
            "package" => Some(Kind::Package),
            "export" => Some(Kind::Export),
            _ => None,
        }
    }
}

/// Channel and client address of the request, set by the extractor
/// `auth::AccessCtx`.
#[derive(Clone, Copy, Debug)]
pub struct AccessCtx {
    pub channel: Channel,
    pub client: Option<IpAddr>,
}

impl AccessCtx {
    /// For tests and background callers.
    pub fn web() -> AccessCtx {
        AccessCtx {
            channel: Channel::Web,
            client: None,
        }
    }
}

/// An object that was read.
pub struct Subject<'a> {
    pub kind: Kind,
    pub project_id: Option<Uuid>,
    /// `item`, `revision`, `file`.
    pub object_type: &'a str,
    pub object_id: Option<Uuid>,
    /// Item number, revision label or file name: kept as text.
    pub label: &'a str,
}

/// Logs a read when the object's group logs reads. Without a group, or when
/// the group does not log, nothing is written. A failing write is an error:
/// a download of a logged group that cannot be logged does not happen.
pub async fn record(
    pool: &PgPool,
    ctx: &AccessCtx,
    actor: &Actor,
    s: Subject<'_>,
) -> AppResult<()> {
    let Some(project_id) = s.project_id else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO access_log (project_id, user_id, username, channel, kind, object_type,
                                 object_id, label, client_address)
         SELECT $1::uuid, $2::uuid, $3::text, $4::text, $5::text, $6::text, $7::uuid, $8::text,
                $9::text
         WHERE EXISTS (SELECT 1 FROM project_policies pp
                       JOIN projects p ON p.id = pp.project_id
                       WHERE pp.project_id = $1 AND pp.read_log AND p.visibility = 'restricted')
           AND NOT EXISTS (SELECT 1 FROM access_log l
                           WHERE l.user_id = $2 AND l.kind = $5 AND l.channel = $4
                             AND l.object_id IS NOT DISTINCT FROM $7
                             AND l.at > now() - ($10::int * interval '1 minute'))",
    )
    .bind(project_id)
    .bind(actor.id)
    .bind(&actor.username)
    .bind(ctx.channel.as_str())
    .bind(s.kind.as_str())
    .bind(s.object_type)
    .bind(s.object_id)
    .bind(s.label.chars().take(300).collect::<String>())
    .bind(ctx.client.map(|ip| ip.to_string()))
    .bind(COLLAPSE_MINUTES)
    .execute(pool)
    .await?;
    Ok(())
}

/// Logs a read of a revision (its page, package or BOM export).
pub async fn record_revision(
    pool: &PgPool,
    ctx: &AccessCtx,
    actor: &Actor,
    rev: &crate::models::Revision,
    kind: Kind,
) -> AppResult<()> {
    let label = format!("{}/{}", rev.item_number, rev.code);
    record(
        pool,
        ctx,
        actor,
        Subject {
            kind,
            project_id: rev.project_id,
            object_type: "revision",
            object_id: Some(rev.id),
            label: &label,
        },
    )
    .await
}

/// Logs the read of a whole structure (Pack and Go, BOM export): one entry per
/// group of the structure that logs, labelled with the first revision of that
/// group. `revision_ids` holds the root and every revision of the tree.
pub async fn record_structure(
    pool: &PgPool,
    ctx: &AccessCtx,
    actor: &Actor,
    kind: Kind,
    revision_ids: &[Uuid],
) -> AppResult<()> {
    let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (i.project_id) r.id, i.project_id, i.number, r.code
         FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE r.id = ANY($1) AND i.project_id IS NOT NULL
         ORDER BY i.project_id, array_position($1, r.id)",
    )
    .bind(revision_ids)
    .fetch_all(pool)
    .await?;
    for (id, project_id, number, code) in rows {
        let label = format!("{number}/{code}");
        record(
            pool,
            ctx,
            actor,
            Subject {
                kind,
                project_id: Some(project_id),
                object_type: "revision",
                object_id: Some(id),
                label: &label,
            },
        )
        .await?;
    }
    Ok(())
}

/// The group of a file (through its revision and item).
pub async fn project_of_file(pool: &PgPool, file_id: Uuid) -> AppResult<Option<Uuid>> {
    Ok(sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT i.project_id FROM files f
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE f.id = $1",
    )
    .bind(file_id)
    .fetch_optional(pool)
    .await?
    .flatten())
}

/// One entry of the log.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    pub username: String,
    pub channel: String,
    pub kind: String,
    pub object_type: String,
    pub object_id: Option<Uuid>,
    pub label: String,
    pub client_address: Option<String>,
}

/// Filter of [`list`].
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub username: Option<String>,
    pub kind: Option<Kind>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

/// The log of a group, newest first (managers of the group and
/// administrators; a group the actor may not know answers "not found").
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    filter: &Filter,
) -> AppResult<Vec<Entry>> {
    projects::get_for_management(pool, actor, project_id).await?;
    // Managing the group is not reading it: an administrator shut out by the
    // need-to-know policy sees neither items nor the labels in the log.
    if !actor.can_see_project(Some(project_id)) {
        return Err(AppError::not_found("Project not found."));
    }
    let mut conn = pool.acquire().await?;
    if !projects::can_manage(&mut conn, actor, project_id).await? {
        return Err(AppError::Forbidden(
            "Only the managers of this project group (and administrators) can read its access log."
                .into(),
        ));
    }
    Ok(sqlx::query_as::<_, Entry>(
        "SELECT id, at, user_id, username, channel, kind, object_type, object_id, label,
                client_address
         FROM access_log
         WHERE project_id = $1
           AND ($2::text IS NULL OR lower(username) = lower($2))
           AND ($3::text IS NULL OR kind = $3)
           AND ($4::timestamptz IS NULL OR at >= $4)
           AND ($5::timestamptz IS NULL OR at < $5)
         ORDER BY at DESC, id
         LIMIT $6",
    )
    .bind(project_id)
    .bind(filter.username.as_deref())
    .bind(filter.kind.map(Kind::as_str))
    .bind(filter.since)
    .bind(filter.until)
    .bind(filter.limit.unwrap_or(200).clamp(1, MAX_LIST))
    .fetch_all(&mut *conn)
    .await?)
}

/// The id of a group by its code, for the CLI (no actor).
pub async fn project_id_by_code(pool: &PgPool, code: &str) -> AppResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects WHERE upper(code) = upper($1)")
        .bind(code.trim())
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Project not found."))
}

/// Deletes old entries (the `teamcraft access-log purge` command; no actor,
/// the operator of the server).
///
/// With `older_than_days` every entry older than that goes (of one group, or
/// of all); without it each group's own `log_retention_days` applies and
/// groups without one keep their log.
///
/// # Invariants
///
/// * **Permission:** none here; only the CLI calls it.
/// * **Audit:** `access_log.purge` with the count, in the same transaction.
/// * **Atomic:** one transaction.
pub async fn purge(
    pool: &PgPool,
    project_id: Option<Uuid>,
    older_than_days: Option<i32>,
) -> AppResult<u64> {
    if older_than_days.is_some_and(|d| d < 0) {
        return Err(AppError::BadRequest(
            "--older-than-days must not be negative.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let deleted = match older_than_days {
        Some(days) => {
            sqlx::query(
                "DELETE FROM access_log
                 WHERE at < now() - ($1::int * interval '1 day')
                   AND ($2::uuid IS NULL OR project_id = $2)",
            )
            .bind(days)
            .bind(project_id)
            .execute(&mut *tx)
            .await?
        }
        None => {
            sqlx::query(
                "DELETE FROM access_log l USING project_policies pp
                 WHERE pp.project_id = l.project_id AND pp.log_retention_days IS NOT NULL
                   AND l.at < now() - (pp.log_retention_days * interval '1 day')
                   AND ($1::uuid IS NULL OR l.project_id = $1)",
            )
            .bind(project_id)
            .execute(&mut *tx)
            .await?
        }
    }
    .rows_affected();
    audit::record(
        &mut tx,
        None,
        "access_log.purge",
        "project",
        project_id,
        format!("Access log purged: {deleted} entries"),
        json!({ "deleted": deleted, "older_than_days": older_than_days,
                "project_id": project_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_channels_have_stable_names() {
        for k in [Kind::View, Kind::Download, Kind::Package, Kind::Export] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("delete"), None);
        assert_eq!(Channel::Web.as_str(), "web");
        assert_eq!(Channel::Api.as_str(), "api");
    }
}
