//! Concurrency management around check-outs: the sync state of the files of
//! a revision as a client's workspace sees them, and the bulk check-in of
//! everything a user has checked out.
//!
//! The lock itself (`revisions::checkout` / `cancel_checkout`) and the
//! optimistic base-version guard of uploads (`files::store_uploads`) live in
//! their own services; this module combines them for the workspace views.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, BaseVersion, FileSyncState, ServerFile},
    error::{AppError, AppResult},
    models::Revision,
    services::{access, files, projects, revisions},
};

/// A file of the client's workspace, as the client reports it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClientFile {
    pub filename: String,
    /// SHA-256 of the local content; `None` = not in the workspace.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The version the local file was checked out as (number) …
    #[serde(default)]
    pub base_version: Option<i32>,
    /// … or its content hash at check-out time.
    #[serde(default)]
    pub base_sha256: Option<String>,
}

impl ClientFile {
    fn base(&self) -> AppResult<Option<BaseVersion>> {
        if let Some(sha) = self.base_sha256.as_deref().filter(|s| !s.trim().is_empty()) {
            return match BaseVersion::parse(sha) {
                Some(b @ BaseVersion::Sha256(_)) => Ok(Some(b)),
                _ => Err(AppError::BadRequest(format!(
                    "{}: base_sha256 must be a SHA-256 in hex.",
                    self.filename
                ))),
            };
        }
        match self.base_version {
            Some(v) if v < 0 => Err(AppError::BadRequest(format!(
                "{}: base_version must not be negative.",
                self.filename
            ))),
            Some(v) => Ok(Some(BaseVersion::Version(v))),
            None => Ok(None),
        }
    }
}

/// One file of a revision with its server state and – if the client
/// reported its workspace – the sync state of the local copy.
#[derive(Clone, Debug, Serialize)]
pub struct FileStatus {
    pub filename: String,
    /// `None` for files that exist only in the client's workspace.
    pub file_id: Option<Uuid>,
    pub version: Option<i32>,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    /// Folder inside the project group (`None` for files only in the workspace).
    pub folder: Option<String>,
    pub uploaded_by_name: Option<String>,
    pub uploaded_at: Option<DateTime<Utc>>,
    /// The local file as the client reported it.
    pub local_sha256: Option<String>,
    /// The server version the local content equals, if any.
    pub local_version: Option<i32>,
    /// `up_to_date`, `modified`, `outdated`, `conflict`, `new`, `missing`;
    /// `None` when the client reported no files.
    pub state: Option<FileSyncState>,
}

/// The files of a revision and who holds its lock.
#[derive(Clone, Debug, Serialize)]
pub struct RevisionStatus {
    pub revision_id: Uuid,
    pub item_number: String,
    pub code: String,
    pub state: String,
    pub locked_by: Option<Uuid>,
    pub locked_by_name: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    pub locked_by_me: bool,
    /// `true` if somebody else holds the lock: local changes cannot be
    /// checked in right now.
    pub locked_by_other: bool,
    pub files: Vec<FileStatus>,
}

/// The status of every file of a revision for one workspace. With an empty
/// `client` list only the server side is returned (`state` = `None`).
pub async fn file_status(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    client: &[ClientFile],
) -> AppResult<RevisionStatus> {
    let rev = revisions::get(pool, actor, revision_id).await?;
    let (list, versions) = tokio::try_join!(
        files::list(pool, actor, revision_id),
        files::versions_of_revision(pool, revision_id)
    )?;
    let mut history: HashMap<Uuid, Vec<(i32, String)>> = HashMap::new();
    for v in versions {
        history
            .entry(v.file_id)
            .or_default()
            .push((v.version, v.sha256));
    }
    let mut reported: HashMap<String, &ClientFile> = HashMap::new();
    for c in client {
        reported.insert(files::stored_name(&c.filename).to_lowercase(), c);
    }
    let with_client = !client.is_empty();
    let mut out = Vec::with_capacity(list.len());
    for f in &list {
        let key = f.filename.to_lowercase();
        let local = reported.remove(&key);
        let hist: Vec<(i32, &str)> = history
            .get(&f.id)
            .map(|h| h.iter().map(|(v, s)| (*v, s.as_str())).collect())
            .unwrap_or_default();
        let local_sha = local
            .and_then(|c| c.sha256.as_deref())
            .map(str::to_ascii_lowercase);
        let state = if with_client {
            let base = local.map(ClientFile::base).transpose()?.flatten();
            Some(domain::sync_state(
                Some(ServerFile {
                    version: f.version,
                    sha256: &f.sha256,
                }),
                &hist,
                local_sha.as_deref(),
                base.as_ref(),
            ))
        } else {
            None
        };
        let local_version = local_sha.as_deref().and_then(|sha| {
            hist.iter()
                .filter(|(_, s)| s.eq_ignore_ascii_case(sha))
                .map(|(v, _)| *v)
                .max()
        });
        out.push(FileStatus {
            filename: f.filename.clone(),
            file_id: Some(f.id),
            version: Some(f.version),
            sha256: Some(f.sha256.clone()),
            size_bytes: Some(f.size_bytes),
            folder: Some(f.folder.clone()),
            uploaded_by_name: Some(f.uploaded_by_name.clone()),
            uploaded_at: Some(f.uploaded_at),
            local_sha256: local_sha,
            local_version,
            state,
        });
    }
    // Files only in the workspace.
    let mut only_local: Vec<&ClientFile> = reported
        .into_values()
        .filter(|c| c.sha256.is_some())
        .collect();
    only_local.sort_by_key(|c| c.filename.to_lowercase());
    for c in only_local {
        let base = c.base()?;
        out.push(FileStatus {
            filename: files::stored_name(&c.filename),
            file_id: None,
            version: None,
            sha256: None,
            size_bytes: None,
            folder: None,
            uploaded_by_name: None,
            uploaded_at: None,
            local_sha256: c.sha256.as_deref().map(str::to_ascii_lowercase),
            local_version: None,
            // A local file that was checked out from the server but is gone there.
            state: Some(match base {
                Some(BaseVersion::Version(v)) if v > 0 => FileSyncState::Conflict,
                Some(BaseVersion::Sha256(_)) => FileSyncState::Conflict,
                _ => FileSyncState::New,
            }),
        });
    }
    Ok(RevisionStatus {
        revision_id: rev.id,
        item_number: rev.item_number.clone(),
        code: rev.code.clone(),
        state: rev.state.clone(),
        locked_by: rev.checked_out_by,
        locked_by_name: rev.checked_out_by_name.clone(),
        locked_at: rev.checked_out_at,
        locked_by_me: rev.checked_out_by == Some(actor.id),
        locked_by_other: rev.checked_out_by.is_some_and(|id| id != actor.id),
        files: out,
    })
}

/// Result of the bulk check-in for one revision.
#[derive(Clone, Debug, Serialize)]
pub struct CheckInOutcome {
    pub revision_id: Uuid,
    pub item_number: String,
    pub code: String,
    pub name: String,
    pub ok: bool,
    /// Why the check-in failed (`ok` = false).
    pub error: Option<String>,
}

impl CheckInOutcome {
    fn of(rev: &Revision, result: AppResult<()>) -> CheckInOutcome {
        CheckInOutcome {
            revision_id: rev.id,
            item_number: rev.item_number.clone(),
            code: rev.code.clone(),
            name: rev.name.clone(),
            ok: result.is_ok(),
            error: result.err().map(|e| match e {
                AppError::Database(_) | AppError::Io(_) | AppError::Other(_) => {
                    tracing::error!(revision = %rev.id, error = %e, "bulk check-in failed");
                    "Internal error – see the server log.".to_string()
                }
                e => e.to_string(),
            }),
        }
    }
}

/// The comment of every check-in of [`check_in_all`] (shown in the audit log).
pub const BULK_COMMENT: &str = "Bulk check-in: lock released without new files";

/// Checks in every revision the actor has checked out – or only those of
/// `only` – **without new files**: the lock is released, the revision keeps
/// its files. Each revision is checked in in its own transaction with its own
/// audit record (`revision.checkin`), so one failure does not block the
/// others; the outcome lists every revision. Clients with a local workspace
/// (desktop) pass only the revisions without local changes.
pub async fn check_in_all(
    pool: &PgPool,
    actor: &Actor,
    only: Option<&[Uuid]>,
) -> AppResult<Vec<CheckInOutcome>> {
    actor.require(domain::Role::Editor)?;
    let mine = revisions::checked_out_by(pool, actor).await?;
    let mut outcomes = Vec::new();
    if let Some(ids) = only {
        // Requested revisions that are not (or no longer) checked out by the actor.
        for id in ids {
            if mine.iter().any(|r| r.id == *id) {
                continue;
            }
            if let Ok(rev) = revisions::get(pool, actor, *id).await {
                outcomes.push(CheckInOutcome::of(
                    &rev,
                    Err(AppError::Conflict(format!(
                        "You have not checked out {}.",
                        revisions::label(&rev)
                    ))),
                ));
            }
        }
    }
    for rev in mine {
        if only.is_some_and(|ids| !ids.contains(&rev.id)) {
            continue;
        }
        let result = files::check_in(pool, actor, rev.id, &[], BULK_COMMENT)
            .await
            .map(|_| ());
        outcomes.push(CheckInOutcome::of(&rev, result));
    }
    Ok(outcomes)
}

/// Most check-outs one [`release_held`] run releases.
pub const MAX_RELEASE: i64 = 1000;

/// Releases the check-outs `holder` still has – in every group, or only in
/// `project` – for a leaver (issue #173). Each release goes through
/// `revisions::cancel_checkout` in its own transaction, so the permission
/// rule stays in one place, every release is audited
/// (`revision.checkout_forced_release`) and one refusal does not stop the
/// others; the outcome lists every revision.
///
/// # Invariants
///
/// * **Permission:** CAD editor. Administrators release in every group they
///   can read; anyone else needs to manage `project` ("not found" for a hidden
///   group, 403 otherwise) or, without `project`, at least one group (403),
///   and only check-outs in the groups they manage are considered. Per
///   revision `cancel_checkout` decides: a manager only releases the locks of
///   users who are no longer members or are deactivated; the rest are listed
///   as refused (an active member's lock needs a reason, one at a time).
/// * **Visibility:** revisions the actor may not read are left out, as if
///   they did not exist (a need-to-know group with `admin_read = none` hides
///   its revisions from administrators too).
/// * **Bounds:** at most [`MAX_RELEASE`] revisions per run.
/// * **Audit / notifications:** those of `cancel_checkout`, per revision.
/// * **Atomic:** one transaction per revision.
pub async fn release_held(
    pool: &PgPool,
    actor: &Actor,
    holder: Uuid,
    project: Option<Uuid>,
) -> AppResult<Vec<CheckInOutcome>> {
    release_held_because(pool, actor, holder, project, None).await
}

/// [`release_held`] with a reason for every release: a manager then also
/// releases the check-outs of an *active* member of their groups (issue #173,
/// the season handover); the reason is audited and told to the holder.
///
/// # Invariants
///
/// * **Permission / visibility / bounds / atomic:** as [`release_held`];
///   per revision `cancel_checkout` decides with the reason.
/// * **Data:** a reason has `revisions::RELEASE_REASON_MIN` to
///   `RELEASE_REASON_MAX` characters (400 before anything is released).
/// * **Audit / notifications:** those of `cancel_checkout`, per revision,
///   with the reason.
pub async fn release_held_because(
    pool: &PgPool,
    actor: &Actor,
    holder: Uuid,
    project: Option<Uuid>,
    reason: Option<&str>,
) -> AppResult<Vec<CheckInOutcome>> {
    actor.require(domain::Role::Editor)?;
    revisions::check_release_reason(reason)?;
    let managed: Option<Vec<Uuid>> = if actor.is_admin() {
        if let Some(p) = project {
            projects::get_for_management(pool, actor, p).await?;
        }
        None
    } else {
        if let Some(p) = project {
            let mut conn = pool.acquire().await?;
            projects::require_manager(&mut conn, actor, p).await?;
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT project_id FROM project_members WHERE user_id = $1 AND manager",
        )
        .bind(actor.id)
        .fetch_all(pool)
        .await?;
        if ids.is_empty() {
            return Err(AppError::Forbidden(
                "Only managers of a project group and administrators can release other users' \
                 check-outs."
                    .into(),
            ));
        }
        Some(ids)
    };
    let held = sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.checked_out_by = $1 AND {}
           AND ($3::uuid IS NULL OR i.project_id = $3)
           AND ($4::uuid[] IS NULL OR i.project_id = ANY($4))
         ORDER BY i.number, r.code LIMIT $5",
        revisions::REVISION_SELECT,
        access::item_visible("i", 2)
    ))
    .bind(holder)
    .bind(&actor.hidden_projects)
    .bind(project)
    .bind(managed.as_deref())
    .bind(MAX_RELEASE)
    .fetch_all(pool)
    .await?;
    let mut outcomes = Vec::with_capacity(held.len());
    for rev in held {
        let result = revisions::cancel_checkout(pool, actor, rev.id, reason)
            .await
            .map(|_| ());
        outcomes.push(CheckInOutcome::of(&rev, result));
    }
    Ok(outcomes)
}

/// How many revisions `holder` has checked out that the actor may read – the
/// number [`release_held`] would look at for an administrator.
pub async fn held_count(pool: &PgPool, actor: &Actor, holder: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE r.checked_out_by = $1 AND {}",
        access::item_visible("i", 2)
    ))
    .bind(holder)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?)
}

/// `"Released 4 check-outs; 1 refused: PRT-000003/A (…)"` for a flash message.
pub fn release_summary(outcomes: &[CheckInOutcome]) -> String {
    let ok = outcomes.iter().filter(|o| o.ok).count();
    let failed: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.ok)
        .take(5)
        .map(|o| {
            format!(
                "{}/{} ({})",
                o.item_number,
                o.code,
                o.error.as_deref().unwrap_or("refused")
            )
        })
        .collect();
    let refused = outcomes.len() - ok;
    let head = match ok {
        0 if refused == 0 => return "No check-outs to release.".into(),
        1 => "Released 1 check-out".to_string(),
        n => format!("Released {n} check-outs"),
    };
    if refused == 0 {
        format!("{head}.")
    } else {
        let more = if refused > failed.len() { ", …" } else { "" };
        format!("{head}; {refused} refused: {}{more}", failed.join(", "))
    }
}

/// `"Checked in 4 revisions; 1 failed: PRT-000003/A (…)"` for a flash message.
pub fn summary(outcomes: &[CheckInOutcome]) -> String {
    let ok = outcomes.iter().filter(|o| o.ok).count();
    let failed: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.ok)
        .map(|o| {
            format!(
                "{}/{} ({})",
                o.item_number,
                o.code,
                o.error.as_deref().unwrap_or("failed")
            )
        })
        .collect();
    let head = match ok {
        0 if failed.is_empty() => return "Nothing was checked out.".into(),
        1 => "Checked in 1 revision".to_string(),
        n => format!("Checked in {n} revisions"),
    };
    if failed.is_empty() {
        format!("{head}.")
    } else {
        format!("{head}; {} failed: {}", failed.len(), failed.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(number: &str, error: Option<&str>) -> CheckInOutcome {
        CheckInOutcome {
            revision_id: Uuid::nil(),
            item_number: number.into(),
            code: "A".into(),
            name: "x".into(),
            ok: error.is_none(),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn summaries() {
        assert_eq!(summary(&[]), "Nothing was checked out.");
        assert_eq!(summary(&[outcome("PRT-1", None)]), "Checked in 1 revision.");
        assert_eq!(
            summary(&[
                outcome("PRT-1", None),
                outcome("PRT-2", None),
                outcome("PRT-3", Some("no access"))
            ]),
            "Checked in 2 revisions; 1 failed: PRT-3/A (no access)"
        );
        assert_eq!(
            summary(&[outcome("PRT-3", Some("no access"))]),
            "Checked in 0 revisions; 1 failed: PRT-3/A (no access)"
        );
    }

    #[test]
    fn client_bases() {
        let c = ClientFile {
            filename: "a.step".into(),
            base_version: Some(3),
            ..Default::default()
        };
        assert_eq!(c.base().unwrap(), Some(BaseVersion::Version(3)));
        let c = ClientFile {
            filename: "a.step".into(),
            base_version: Some(3),
            base_sha256: Some("CD".repeat(32)),
            ..Default::default()
        };
        assert_eq!(
            c.base().unwrap(),
            Some(BaseVersion::Sha256("cd".repeat(32))),
            "the hash wins"
        );
        let c = ClientFile {
            filename: "a.step".into(),
            base_sha256: Some("12".into()),
            ..Default::default()
        };
        assert!(c.base().is_err());
        assert_eq!(ClientFile::default().base().unwrap(), None);
    }
}
