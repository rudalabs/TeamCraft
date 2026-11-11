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
//! What is logged: item and revision pages (and the same data over the REST
//! API), file downloads, Pack and Go, BOM exports, the desktop app's Get
//! ([`Kind::Workset`]) and the read tools of MCP clients and the built-in
//! assistant that name an item ([`record_tool_read`]). Lists and searches
//! are not logged, in no channel.

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
    /// A tool called by an MCP client.
    Mcp,
    /// A tool called by the built-in assistant.
    Assistant,
    /// The desktop app's Get.
    Desktop,
}

impl Channel {
    pub const ALL: [Channel; 5] = [
        Channel::Web,
        Channel::Api,
        Channel::Mcp,
        Channel::Assistant,
        Channel::Desktop,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Web => "web",
            Channel::Api => "api",
            Channel::Mcp => "mcp",
            Channel::Assistant => "assistant",
            Channel::Desktop => "desktop",
        }
    }

    pub fn parse(raw: &str) -> Option<Channel> {
        let raw = raw.trim().to_ascii_lowercase();
        Channel::ALL.into_iter().find(|c| c.as_str() == raw)
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Web => "Browser",
            Channel::Api => "REST API",
            Channel::Mcp => "MCP client",
            Channel::Assistant => "Assistant",
            Channel::Desktop => "Desktop app",
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
    /// The working set of the desktop app's Get.
    Workset,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::View,
        Kind::Download,
        Kind::Package,
        Kind::Export,
        Kind::Workset,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::View => "view",
            Kind::Download => "download",
            Kind::Package => "package",
            Kind::Export => "export",
            Kind::Workset => "workset",
        }
    }

    pub fn parse(raw: &str) -> Option<Kind> {
        let raw = raw.trim().to_ascii_lowercase();
        Kind::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::View => "View",
            Kind::Download => "Download",
            Kind::Package => "Pack and Go",
            Kind::Export => "BOM export",
            Kind::Workset => "Desktop Get",
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

    /// The same request seen through another channel (an MCP call arrives
    /// as an API request, a Get as a desktop one).
    pub fn through(self, channel: Channel) -> AccessCtx {
        AccessCtx { channel, ..self }
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
    /// What read it (a tool name); empty for pages and downloads.
    pub detail: &'a str,
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
                                 object_id, label, client_address, detail)
         SELECT $1::uuid, $2::uuid, $3::text, $4::text, $5::text, $6::text, $7::uuid, $8::text,
                $9::text, $11::text
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
    .bind(s.detail.chars().take(300).collect::<String>())
    .execute(pool)
    .await?;
    Ok(())
}

/// Logs a read tool of an MCP client or the built-in assistant that named an
/// item (`item`, as `PRT-000012` or `PRT-000012/B`, plus an optional
/// `revision`): one view of the item per tool, labelled with what the tool
/// was asked for. Write tools, tools without an item (searches, lists,
/// change orders) and items the actor may not see log nothing. The tool name
/// is the entry's detail (`read_file_text` adds the file).
pub async fn record_tool_read(
    pool: &PgPool,
    ctx: &AccessCtx,
    actor: &Actor,
    tool: &str,
    args: &serde_json::Value,
) -> AppResult<()> {
    let Some(raw) = args.get("item").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    // `PRT-000012/B`: item numbers never contain a slash
    let (number, code) = match raw.trim().split_once('/') {
        Some((n, c)) => (n.trim(), Some(c.trim().to_string()).filter(|c| !c.is_empty())),
        None => (raw.trim(), None),
    };
    if number.is_empty() {
        return Ok(());
    }
    let item = match crate::services::items::get_by_number(pool, actor, number).await {
        Ok(item) => item,
        // the tool answered (an error names nothing of a hidden item)
        Err(AppError::NotFound(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    let code = code.or_else(|| {
        ["revision", "to"]
            .iter()
            .find_map(|k| args.get(*k).and_then(serde_json::Value::as_str))
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
    });
    let label = match &code {
        Some(c) => format!("{}/{c}", item.number),
        None => item.number.clone(),
    };
    let detail = match args.get("file").and_then(serde_json::Value::as_str) {
        Some(file) if tool == "read_file_text" => format!("{tool}: {file}"),
        _ => tool.to_string(),
    };
    record(
        pool,
        ctx,
        actor,
        Subject {
            kind: Kind::View,
            project_id: item.project_id,
            object_type: "item",
            object_id: Some(item.id),
            label: &label,
            detail: &detail,
        },
    )
    .await
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
            detail: "",
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
                detail: "",
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
    /// What read it (a tool name); empty for pages and downloads.
    pub detail: String,
}

/// Filter of [`list`].
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub username: Option<String>,
    pub kind: Option<Kind>,
    pub channel: Option<Channel>,
    /// Part of the label (item number, revision, file name) or the detail,
    /// case-insensitive.
    pub object: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    /// Paging: only entries after (older than) this one, in the order of
    /// the list.
    pub before: Option<Uuid>,
}

/// The filter as a query string sends it (REST API and the log page): every
/// field optional, empty = not set. Times are RFC 3339 or a date
/// (`YYYY-MM-DD`, UTC); a date `until` includes that whole day.
#[derive(Clone, Debug, Default, serde::Deserialize, Serialize)]
pub struct FilterQuery {
    pub user: Option<String>,
    pub kind: Option<String>,
    pub channel: Option<String>,
    pub object: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<i64>,
    pub before: Option<Uuid>,
}

fn present(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// A time of a filter; `end` moves a bare date to the start of the next day.
fn filter_time(field: &str, raw: &str, end: bool) -> AppResult<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Ok(t.with_timezone(&Utc));
    }
    let day = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        AppError::BadRequest(format!(
            "{field} must be a date (YYYY-MM-DD) or an RFC 3339 time."
        ))
    })?;
    let day = if end { day.succ_opt().unwrap_or(day) } else { day };
    Ok(day.and_time(chrono::NaiveTime::MIN).and_utc())
}

impl FilterQuery {
    /// The filter; an unknown kind or channel or a bad time is a 400.
    pub fn parse(&self) -> AppResult<Filter> {
        let names = |all: Vec<&str>| all.join(", ");
        let kind = present(&self.kind)
            .map(|raw| {
                Kind::parse(raw).ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "kind must be one of {}.",
                        names(Kind::ALL.iter().map(|k| k.as_str()).collect())
                    ))
                })
            })
            .transpose()?;
        let channel = present(&self.channel)
            .map(|raw| {
                Channel::parse(raw).ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "channel must be one of {}.",
                        names(Channel::ALL.iter().map(|c| c.as_str()).collect())
                    ))
                })
            })
            .transpose()?;
        Ok(Filter {
            username: present(&self.user).map(str::to_string),
            kind,
            channel,
            object: present(&self.object).map(str::to_string),
            since: present(&self.since)
                .map(|t| filter_time("since", t, false))
                .transpose()?,
            until: present(&self.until)
                .map(|t| filter_time("until", t, true))
                .transpose()?,
            limit: self.limit,
            before: self.before,
        })
    }
}

/// One page of the log.
#[derive(Clone, Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// The id to pass as [`Filter::before`] for the next (older) page;
    /// `None` on the last page.
    pub next: Option<Uuid>,
}

/// Like [`list`], with the cursor of the next page.
pub async fn page(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    filter: &Filter,
) -> AppResult<Page> {
    let limit = filter.limit.unwrap_or(200).clamp(1, MAX_LIST);
    let mut entries = list(
        pool,
        actor,
        project_id,
        &Filter {
            limit: Some(limit + 1),
            ..filter.clone()
        },
    )
    .await?;
    let more = entries.len() as i64 > limit;
    entries.truncate(limit as usize);
    Ok(Page {
        next: if more {
            entries.last().map(|e| e.id)
        } else {
            None
        },
        entries,
    })
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
    // the cursor is an entry of this group; another id ends the list
    Ok(sqlx::query_as::<_, Entry>(
        "SELECT id, at, user_id, username, channel, kind, object_type, object_id, label,
                client_address, detail
         FROM access_log l
         WHERE project_id = $1
           AND ($2::text IS NULL OR lower(username) = lower($2))
           AND ($3::text IS NULL OR kind = $3)
           AND ($4::timestamptz IS NULL OR at >= $4)
           AND ($5::timestamptz IS NULL OR at < $5)
           AND ($7::text IS NULL OR channel = $7)
           AND ($8::text IS NULL OR strpos(lower(label), lower($8)) > 0
                OR strpos(lower(detail), lower($8)) > 0)
           AND ($9::uuid IS NULL OR (at, id) < (SELECT c.at, c.id FROM access_log c
                                                WHERE c.id = $9 AND c.project_id = $1))
         ORDER BY at DESC, id DESC
         LIMIT $6",
    )
    .bind(project_id)
    .bind(
        filter
            .username
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(filter.kind.map(Kind::as_str))
    .bind(filter.since)
    .bind(filter.until)
    .bind(filter.limit.unwrap_or(200).clamp(1, MAX_LIST))
    .bind(filter.channel.map(Channel::as_str))
    .bind(
        filter
            .object
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(filter.before)
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
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
            assert!(!k.label().is_empty());
        }
        assert_eq!(Kind::parse(" Workset "), Some(Kind::Workset));
        assert_eq!(Kind::parse("delete"), None);
        for c in Channel::ALL {
            assert_eq!(Channel::parse(c.as_str()), Some(c));
            assert!(!c.label().is_empty());
        }
        assert_eq!(Channel::Web.as_str(), "web");
        assert_eq!(Channel::Api.as_str(), "api");
        assert_eq!(Channel::parse("MCP"), Some(Channel::Mcp));
        assert_eq!(Channel::parse("email"), None);
    }

    #[test]
    fn query_strings_become_filters() {
        let q = FilterQuery {
            user: Some("  ".into()),
            kind: Some("Download".into()),
            channel: Some("assistant".into()),
            object: Some(" PRT-1 ".into()),
            since: Some("2026-11-01".into()),
            until: Some("2026-11-02".into()),
            limit: Some(20),
            before: None,
        };
        let f = q.parse().unwrap();
        assert_eq!(f.username, None, "blank is not set");
        assert_eq!(f.kind, Some(Kind::Download));
        assert_eq!(f.channel, Some(Channel::Assistant));
        assert_eq!(f.object.as_deref(), Some("PRT-1"));
        assert_eq!(f.since.unwrap().to_rfc3339(), "2026-11-01T00:00:00+00:00");
        // a date `until` includes the whole day
        assert_eq!(f.until.unwrap().to_rfc3339(), "2026-11-03T00:00:00+00:00");
        let exact = FilterQuery {
            until: Some("2026-11-02T10:30:00Z".into()),
            ..Default::default()
        };
        assert_eq!(
            exact.parse().unwrap().until.unwrap().to_rfc3339(),
            "2026-11-02T10:30:00+00:00"
        );
        for bad in [
            FilterQuery {
                kind: Some("delete".into()),
                ..Default::default()
            },
            FilterQuery {
                channel: Some("email".into()),
                ..Default::default()
            },
            FilterQuery {
                since: Some("yesterday".into()),
                ..Default::default()
            },
        ] {
            assert!(matches!(bad.parse(), Err(AppError::BadRequest(_))));
        }
        assert!(FilterQuery::default().parse().unwrap().kind.is_none());
    }

    #[test]
    fn the_names_match_the_check_constraints() {
        // migrations/0066: the CHECK constraints list exactly these
        let sql = include_str!("../../migrations/0066_access_log_tools_and_desktop.sql");
        let channels: Vec<_> = Channel::ALL.iter().map(|c| format!("'{}'", c.as_str())).collect();
        assert!(sql.contains(&format!("channel IN ({})", channels.join(", "))));
        let kinds: Vec<_> = Kind::ALL.iter().map(|k| format!("'{}'", k.as_str())).collect();
        assert!(sql.contains(&format!("kind IN ({})", kinds.join(", "))));
    }
}
