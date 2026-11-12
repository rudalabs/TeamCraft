//! The file browser of a project group: the folders its files have
//! (`files.folder`, the structure of the engineer's working folder) and the
//! files in them, one folder level per request (read-only; a big group is
//! never loaded as a whole).
//!
//! A file belongs to the latest revision of its item, so the browser shows
//! the group the way a new working folder would look. Items whose latest
//! revision has no file are listed at the top of the group, after the files,
//! so every item of the group can be reached from the browser.
//!
//! # Invariants
//!
//! * **Permission:** a hidden group answers "not found"; nothing of it is
//!   counted or listed.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    cadformat::{self, FormatKind},
    error::{AppError, AppResult},
    services::{access, Page},
};

/// More sub-folders than this are never returned for one folder.
pub const MAX_FOLDERS: i64 = 1000;

/// A folder directly below the listed one.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct SubFolder {
    pub name: String,
    /// Path inside the group (`chassis/brackets`), the `folder` to ask for.
    #[sqlx(skip)]
    pub path: String,
    /// Files in it and in the folders below it.
    pub files: i64,
}

/// A file of the listed folder with the item it belongs to – or, at the top
/// of the group, an item without files (`file_id` and `filename` are null).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub file_id: Option<Uuid>,
    pub filename: Option<String>,
    pub size_bytes: Option<i64>,
    /// Format id (see `cadformat`), `""` if unknown or without a file.
    pub format: String,
    /// Label of the format ("SolidWorks assembly"); null if unknown.
    #[sqlx(skip)]
    pub format_label: Option<&'static str>,
    /// What the file contains (`part`, `assembly`, `drawing`, …); null if unknown.
    #[sqlx(skip)]
    pub format_kind: Option<FormatKind>,
    pub item_number: String,
    pub item_type: String,
    /// Name, code and state of the item's latest revision.
    pub name: String,
    pub code: String,
    pub state: String,
    pub checked_out_by_name: Option<String>,
}

/// One folder of a group.
#[derive(Clone, Debug, Serialize)]
pub struct Level {
    /// The listed folder (`""` = the top of the group).
    pub folder: String,
    pub folders: Vec<SubFolder>,
    /// The requested page of the folder's entries, files first, by name.
    pub entries: Vec<Entry>,
    /// Entries of the folder in all (0 beyond the last page).
    pub total: i64,
}

/// The latest revision of the item aliased `i`.
const LATEST: &str = "CROSS JOIN LATERAL (SELECT r.id, r.code, r.state, r.name, r.checked_out_by
                             FROM revisions r WHERE r.item_id = i.id
                             ORDER BY r.seq DESC LIMIT 1) lr";

/// The sub-folders and entries of `folder` (normalised by
/// `domain::normalize_folder`; `""` = the top) in a project group.
pub async fn level(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    folder: &str,
    page: Page,
) -> AppResult<Level> {
    access::ensure_visible(
        actor,
        Some(project_id),
        AppError::not_found("Project group not found."),
    )?;
    let visible = access::item_visible("i", 2);
    // $3 = the folder; a file is below it when its folder starts with
    // "<folder>/" (every folder is below the top). The comparison is on the
    // prefix itself, not a LIKE pattern: folder names may contain % and _.
    let mut folders = sqlx::query_as::<_, SubFolder>(&format!(
        "SELECT sub.name, COUNT(*) AS files
         FROM (SELECT split_part(CASE WHEN $3 = '' THEN f.folder
                                      ELSE substr(f.folder, length($3) + 2) END, '/', 1) AS name
               FROM items i {LATEST}
               JOIN files f ON f.revision_id = lr.id
               WHERE i.project_id = $1 AND {visible}
                 AND CASE WHEN $3 = '' THEN f.folder <> ''
                          ELSE left(f.folder, length($3) + 1) = $3 || '/' END) sub
         GROUP BY sub.name
         ORDER BY lower(sub.name), sub.name
         LIMIT {MAX_FOLDERS}"
    ))
    .bind(project_id)
    .bind(&actor.hidden_projects)
    .bind(folder)
    .fetch_all(pool)
    .await?;
    for sub in &mut folders {
        sub.path = if folder.is_empty() {
            sub.name.clone()
        } else {
            format!("{folder}/{}", sub.name)
        };
    }

    #[derive(sqlx::FromRow)]
    struct Counted {
        #[sqlx(flatten)]
        entry: Entry,
        total: i64,
    }
    let rows = sqlx::query_as::<_, Counted>(&format!(
        "SELECT e.*, COUNT(*) OVER () AS total
         FROM (SELECT f.id AS file_id, f.filename, f.size_bytes, COALESCE(f.format, '') AS format,
                      i.number AS item_number, i.item_type, lr.name, lr.code, lr.state,
                      u.display_name AS checked_out_by_name
               FROM items i {LATEST}
               JOIN files f ON f.revision_id = lr.id
               LEFT JOIN users u ON u.id = lr.checked_out_by
               WHERE i.project_id = $1 AND {visible} AND f.folder = $3
               UNION ALL
               SELECT NULL::uuid, NULL::text, NULL::bigint, '',
                      i.number, i.item_type, lr.name, lr.code, lr.state, u.display_name
               FROM items i {LATEST}
               LEFT JOIN users u ON u.id = lr.checked_out_by
               WHERE i.project_id = $1 AND {visible} AND $3 = ''
                 AND NOT EXISTS (SELECT 1 FROM files f WHERE f.revision_id = lr.id)) e
         ORDER BY (e.filename IS NULL), lower(COALESCE(e.filename, e.name)), e.filename,
                  e.item_number
         LIMIT $4 OFFSET $5"
    ))
    .bind(project_id)
    .bind(&actor.hidden_projects)
    .bind(folder)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await?;
    let total = rows.first().map(|r| r.total).unwrap_or(0);
    let entries = rows
        .into_iter()
        .map(|r| {
            let mut entry = r.entry;
            if let Some(info) = cadformat::info(&entry.format) {
                entry.format_label = Some(info.label);
                entry.format_kind = Some(info.kind);
            }
            entry
        })
        .collect();
    Ok(Level {
        folder: folder.to_string(),
        folders,
        entries,
        total,
    })
}
