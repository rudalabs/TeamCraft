//! Access review of a restricted project group (issue #151, step 5).
//!
//! The managers of a group (and administrators) see who may read it: every
//! member with role, source (manual or identity provider), last sign-in and
//! whether the account is active, the administrator policy and the history of
//! administrator elevations. Confirming the review writes a dated, audited
//! record that keeps the member list as it was; [`to_csv`] and the JSON form
//! of [`AccessReport`] are the evidence an auditor can be given.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{AdminRead, NotificationKind, ProjectVisibility},
    error::{AppError, AppResult},
    services::{
        access, audit,
        notifications::{self, Notice, Subject},
        project_policy::{self, Elevation},
        projects,
    },
    util,
};

/// A member without sign-in for this many days is flagged as stale.
pub const STALE_DAYS: i64 = 90;

/// Longest accepted note of a review (also the column's CHECK).
pub const MAX_NOTE_CHARS: usize = 1000;

/// One member as the review shows them.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ReviewMember {
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub manager: bool,
    /// `manual` or `oidc`.
    pub source: String,
    /// Whether the account can sign in.
    pub active: bool,
    pub added_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
    /// No sign-in for [`STALE_DAYS`] days (or never).
    pub stale: bool,
}

/// A confirmed review.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ReviewRecord {
    pub id: Uuid,
    pub username: String,
    pub note: String,
    pub reviewed_at: DateTime<Utc>,
    pub admin_read: String,
    pub member_count: i32,
}

/// Everything an access review shows; also the JSON evidence.
#[derive(Clone, Debug, Serialize)]
pub struct AccessReport {
    pub project_code: String,
    pub project_name: String,
    pub generated_at: DateTime<Utc>,
    pub admin_read: AdminRead,
    /// Active administrators who are not members (they read the group
    /// according to the policy).
    pub administrators_outside: i64,
    pub stale_days: i64,
    pub members: Vec<ReviewMember>,
    /// Newest first (at most 100).
    pub elevations: Vec<Elevation>,
    /// Newest first (at most 50).
    pub reviews: Vec<ReviewRecord>,
    /// The review schedule.
    pub schedule: ReviewSchedule,
}

/// When the access review of a group is due.
#[derive(Clone, Debug, Default, Serialize, sqlx::FromRow)]
pub struct ReviewSchedule {
    /// A review every this many days (`None` = no schedule).
    pub interval_days: Option<i32>,
    pub last_review_at: Option<DateTime<Utc>>,
    /// The next review is due at this time (`None` without a schedule; with
    /// a schedule but no review yet it is due now).
    pub next_review_at: Option<DateTime<Utc>>,
    /// No review within the interval.
    pub due: bool,
}

/// The schedule columns of the group `p` (`projects p` and
/// `project_policies pp`, left joined) with its newest review `lr`.
const SCHEDULE_COLUMNS: &str = "pp.review_interval_days AS interval_days, lr.at AS last_review_at,
    CASE WHEN pp.review_interval_days IS NULL THEN NULL
         ELSE COALESCE(lr.at + make_interval(days => pp.review_interval_days), now()) END
        AS next_review_at,
    COALESCE(p.visibility = 'restricted' AND pp.review_interval_days IS NOT NULL
             AND (lr.at IS NULL
                  OR lr.at <= now() - make_interval(days => pp.review_interval_days)), FALSE) AS due";

const SCHEDULE_JOINS: &str = "LEFT JOIN project_policies pp ON pp.project_id = p.id
    LEFT JOIN LATERAL (SELECT max(r.reviewed_at) AS at FROM project_access_reviews r
                       WHERE r.project_id = p.id) lr ON TRUE";

/// The review schedule of a group (no permission check: the callers show it
/// to people who manage the group).
pub async fn schedule(pool: &PgPool, project_id: Uuid) -> AppResult<ReviewSchedule> {
    Ok(sqlx::query_as::<_, ReviewSchedule>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM projects p {SCHEDULE_JOINS} WHERE p.id = $1"
    ))
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or_default())
}

/// The groups whose access review is due and that the actor manages (all of
/// them for administrators), for the "review due" flag of the group list.
pub async fn due_for(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT p.id FROM projects p {SCHEDULE_JOINS}
         WHERE p.active AND {DUE_CONDITION}
           AND ($2 OR EXISTS (SELECT 1 FROM project_members pm
                              WHERE pm.project_id = p.id AND pm.user_id = $1 AND pm.manager))"
    ))
    .bind(actor.id)
    .bind(actor.is_admin())
    .fetch_all(pool)
    .await?)
}

/// The "due" condition of [`SCHEDULE_COLUMNS`] as a WHERE clause.
const DUE_CONDITION: &str = "p.visibility = 'restricted' AND pp.review_interval_days IS NOT NULL
    AND (lr.at IS NULL OR lr.at <= now() - make_interval(days => pp.review_interval_days))";

/// Reminds the managers of every group whose access review is due (the
/// hourly housekeeping): once when the review becomes due and again after
/// each further interval without one. A group without managers reminds the
/// active administrators.
///
/// # Invariants
///
/// * **Permission:** none; a background task, not an actor.
/// * **Locks:** the policy row of the group (`FOR UPDATE`), so two servers do
///   not both remind.
/// * **Data:** sets `project_policies.review_reminded_at`.
/// * **Audit:** `project.review_reminder` per group.
/// * **Notifications:** an `access_alert` to each manager (not mutable);
///   **webhooks:** none.
/// * **Atomic:** one transaction per group.
pub async fn remind_due(pool: &PgPool) -> AppResult<u64> {
    let due: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT p.id FROM projects p {SCHEDULE_JOINS}
         WHERE p.active AND {DUE_CONDITION}
           AND (pp.review_reminded_at IS NULL
                OR pp.review_reminded_at < COALESCE(lr.at, '-infinity'::timestamptz)
                OR pp.review_reminded_at <= now() - make_interval(days => pp.review_interval_days))"
    ))
    .fetch_all(pool)
    .await?;
    let mut sent = 0;
    for project_id in due {
        let mut tx = pool.begin().await?;
        // checked again under the lock: another server may have reminded
        let still: Option<(String, i32)> = sqlx::query_as(&format!(
            "SELECT p.code, pp.review_interval_days FROM projects p {SCHEDULE_JOINS}
             WHERE p.id = $1 AND p.active AND {DUE_CONDITION}
               AND (pp.review_reminded_at IS NULL
                    OR pp.review_reminded_at < COALESCE(lr.at, '-infinity'::timestamptz)
                    OR pp.review_reminded_at
                       <= now() - make_interval(days => pp.review_interval_days))
             FOR UPDATE OF pp"
        ))
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((code, days)) = still else {
            tx.rollback().await?;
            continue;
        };
        let mut to: Vec<Uuid> = sqlx::query_scalar(
            "SELECT pm.user_id FROM project_members pm JOIN users u ON u.id = pm.user_id
             WHERE pm.project_id = $1 AND pm.manager AND u.active",
        )
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await?;
        if to.is_empty() {
            to = sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' AND active")
                .fetch_all(&mut *tx)
                .await?;
        }
        let notice = Notice {
            kind: NotificationKind::AccessAlert,
            title: format!("Access review of {code} is due"),
            body: format!(
                "The access review of {code} is due (every {days} days). Check who may read the \
                 group and confirm the review."
            ),
            link: format!("/projects/{project_id}/access-review"),
            subject_type: "",
            subject_ref: String::new(),
            subject: Subject::Personal,
            redacted: None,
        };
        notifications::notify(&mut tx, None, &to, &notice).await?;
        sqlx::query(
            "UPDATE project_policies SET review_reminded_at = now() WHERE project_id = $1",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            None,
            "project.review_reminder",
            "project",
            Some(project_id),
            format!("Access review of {code} is due: reminded {} people", to.len()),
            json!({ "interval_days": days, "recipients": to.len() }),
        )
        .await?;
        tx.commit().await?;
        sent += 1;
    }
    Ok(sent)
}

async fn restricted_code(pool: &PgPool, project_id: Uuid) -> AppResult<(String, String)> {
    let p = sqlx::query_as::<_, (String, String, String)>(
        "SELECT code, name, visibility FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(access::not_found_project)?;
    if p.2 != ProjectVisibility::Restricted.as_str() {
        return Err(AppError::Conflict(format!(
            "{} is not restricted: an access review is for restricted groups.",
            p.0
        )));
    }
    Ok((p.0, p.1))
}

const MEMBER_SELECT: &str = "SELECT u.username, u.display_name, u.role, pm.manager, pm.source,
            u.active, pm.added_at, u.last_login_at,
            (u.last_login_at IS NULL
             OR u.last_login_at < now() - make_interval(days => $2)) AS stale
     FROM project_members pm JOIN users u ON u.id = pm.user_id
     WHERE pm.project_id = $1 ORDER BY pm.manager DESC, u.username";

/// The access report of a restricted group.
///
/// # Invariants
///
/// * **Permission:** managers of the group and administrators (403
///   otherwise; a group hidden from the actor is "not found"); the group
///   must be restricted (409).
/// * **Data:** read-only; the clock of the database decides what is stale.
pub async fn report(pool: &PgPool, actor: &Actor, project_id: Uuid) -> AppResult<AccessReport> {
    // the policy view enforces the management permission
    let policy = project_policy::get(pool, actor, project_id).await?;
    let (code, name) = restricted_code(pool, project_id).await?;
    let members = sqlx::query_as::<_, ReviewMember>(MEMBER_SELECT)
        .bind(project_id)
        .bind(STALE_DAYS as i32)
        .fetch_all(pool)
        .await?;
    let administrators_outside: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users u
         WHERE u.role = 'admin' AND u.active
           AND NOT EXISTS (SELECT 1 FROM project_members pm
                           WHERE pm.project_id = $1 AND pm.user_id = u.id)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let reviews = sqlx::query_as::<_, ReviewRecord>(
        "SELECT id, username, note, reviewed_at, admin_read, member_count
         FROM project_access_reviews WHERE project_id = $1
         ORDER BY reviewed_at DESC, id DESC LIMIT 50",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(AccessReport {
        project_code: code,
        project_name: name,
        generated_at: sqlx::query_scalar("SELECT now()").fetch_one(pool).await?,
        admin_read: policy.admin_read,
        administrators_outside,
        stale_days: STALE_DAYS,
        members,
        elevations: policy.elevations,
        reviews,
        schedule: schedule(pool, project_id).await?,
    })
}

/// Confirms that the access of a restricted group was reviewed.
///
/// # Invariants
///
/// * **Permission:** managers of the group and administrators (403); the
///   group must be restricted (409); the note has at most
///   [`MAX_NOTE_CHARS`] characters (400).
/// * **Locks:** the group row (`FOR UPDATE`), so the member list that is
///   stored is the one the confirmation refers to.
/// * **Data:** appends a record with the member list and the policy as they
///   are now; records are never changed.
/// * **Audit:** `project.access_review` with the note and the member count.
/// * **Notifications / webhooks:** the other managers get an access alert; no
///   webhooks.
/// * **Atomic:** one transaction.
pub async fn confirm(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    note: &str,
) -> AppResult<ReviewRecord> {
    let note = note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(AppError::BadRequest(format!(
            "The note has at most {MAX_NOTE_CHARS} characters."
        )));
    }
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, (String, String)>(
        "SELECT code, visibility FROM projects WHERE id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(access::not_found_project)?;
    projects::require_manager(&mut tx, actor, project_id).await?;
    if project.1 != ProjectVisibility::Restricted.as_str() {
        return Err(AppError::Conflict(format!(
            "{} is not restricted: an access review is for restricted groups.",
            project.0
        )));
    }
    let members = sqlx::query_as::<_, ReviewMember>(MEMBER_SELECT)
        .bind(project_id)
        .bind(STALE_DAYS as i32)
        .fetch_all(&mut *tx)
        .await?;
    let mode = sqlx::query_scalar::<_, String>(
        "SELECT admin_read FROM project_policies WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_else(|| AdminRead::Implicit.as_str().to_string());
    let record = sqlx::query_as::<_, ReviewRecord>(
        "INSERT INTO project_access_reviews
             (project_id, reviewed_by, username, note, admin_read, member_count, snapshot)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, username, note, reviewed_at, admin_read, member_count",
    )
    .bind(project_id)
    .bind(actor.id)
    .bind(&actor.username)
    .bind(note)
    .bind(&mode)
    .bind(members.len() as i32)
    .bind(json!(members))
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.access_review",
        "project",
        Some(project_id),
        format!(
            "{} confirmed the access review of {} ({} members)",
            actor.username,
            project.0,
            members.len()
        ),
        json!({ "review_id": record.id, "note": note, "members": members.len(),
                "admin_read": mode }),
    )
    .await?;
    project_policy::alert_managers(
        &mut tx,
        actor,
        project_id,
        format!("Access review of {} confirmed", project.0),
        format!(
            "{} confirmed the access review of {} with {} members.",
            actor.username,
            project.0,
            members.len()
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(record)
}

/// The report as one CSV table (RFC 4180, formula-safe): a row per member,
/// elevation and confirmed review, told apart by the first column.
pub fn to_csv(report: &AccessReport) -> String {
    fn time(t: Option<DateTime<Utc>>) -> String {
        t.map(|t| t.to_rfc3339()).unwrap_or_default()
    }
    let mut out = String::from(
        "record,group,username,name,role,manager,source,active,stale,last_sign_in,detail,from,until\n",
    );
    let mut row = |cells: Vec<String>| {
        let line: Vec<String> = cells.iter().map(|c| util::csv_field(c)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    };
    let g = report.project_code.clone();
    row(vec![
        "policy".into(),
        g.clone(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        format!(
            "administrators: {}; {} active administrators are not members; review: {}",
            report.admin_read.as_str(),
            report.administrators_outside,
            match report.schedule.interval_days {
                None => "no schedule".to_string(),
                Some(days) => format!(
                    "every {days} days, {}",
                    if report.schedule.due { "due" } else { "not due" }
                ),
            }
        ),
        time(Some(report.generated_at)),
        String::new(),
    ]);
    for m in &report.members {
        row(vec![
            "member".into(),
            g.clone(),
            m.username.clone(),
            m.display_name.clone(),
            m.role.clone(),
            m.manager.to_string(),
            m.source.clone(),
            m.active.to_string(),
            m.stale.to_string(),
            time(m.last_login_at),
            String::new(),
            time(Some(m.added_at)),
            String::new(),
        ]);
    }
    for e in &report.elevations {
        row(vec![
            "elevation".into(),
            g.clone(),
            e.username.clone(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            e.reason.clone(),
            time(Some(e.started_at)),
            time(Some(e.ended_at.unwrap_or(e.expires_at))),
        ]);
    }
    for r in &report.reviews {
        row(vec![
            "review".into(),
            g.clone(),
            r.username.clone(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            format!(
                "{} members; administrators: {}; {}",
                r.member_count, r.admin_read, r.note
            ),
            time(Some(r.reviewed_at)),
            String::new(),
        ]);
    }
    out
}

/// The member list as it was at a confirmed review (managers and
/// administrators), for the auditor's question "who had access then".
///
/// # Invariants
///
/// * **Permission:** like [`report`]; a review of another group is "not
///   found".
pub async fn snapshot(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    review_id: Uuid,
) -> AppResult<serde_json::Value> {
    projects::get_for_management(pool, actor, project_id).await?;
    let mut conn = pool.acquire().await?;
    if !projects::can_manage(&mut conn, actor, project_id).await? {
        return Err(AppError::Forbidden(
            "Only the managers of this project group (and administrators) can see its reviews."
                .into(),
        ));
    }
    let row = sqlx::query(
        "SELECT snapshot FROM project_access_reviews WHERE id = $1 AND project_id = $2",
    )
    .bind(review_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("Review not found."))?;
    Ok(row.get("snapshot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_has_one_row_per_record_and_neutralises_formulas() {
        let now = Utc::now();
        let report = AccessReport {
            project_code: "NTK".into(),
            project_name: "Need".into(),
            generated_at: now,
            admin_read: AdminRead::WithReason,
            administrators_outside: 2,
            stale_days: 90,
            members: vec![ReviewMember {
                username: "erika".into(),
                display_name: "=Erika, Mustermann".into(),
                role: "editor".into(),
                manager: true,
                source: "oidc".into(),
                active: true,
                added_at: now,
                last_login_at: None,
                stale: true,
            }],
            elevations: vec![],
            reviews: vec![ReviewRecord {
                id: Uuid::nil(),
                username: "root".into(),
                note: "all \"fine\"".into(),
                reviewed_at: now,
                admin_read: "none".into(),
                member_count: 1,
            }],
            schedule: ReviewSchedule {
                interval_days: Some(90),
                due: true,
                ..Default::default()
            },
        };
        let csv = to_csv(&report);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 4, "{csv}");
        assert!(lines[0].starts_with("record,group,"));
        assert!(lines[1].starts_with("policy,NTK,"));
        assert!(lines[1].contains("review: every 90 days, due"), "{csv}");
        assert!(lines[2]
            .starts_with("member,NTK,erika,\"'=Erika, Mustermann\",editor,true,oidc,true,true,"));
        assert!(lines[3].contains("\"1 members; administrators: none; all \"\"fine\"\"\""));
    }
}
