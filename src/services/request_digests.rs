//! E-mail digest of the change requests that wait for triage, per project
//! group.
//!
//! Every CAD editor (and admin) of a project group gets – besides the
//! immediate notification of every new request – a summary every
//! `TEAMCRAFT_REQUEST_DIGEST_HOURS` (default: daily) of the requests of the
//! group that are still *new*: the ones raised since the previous digest,
//! and how many older ones still wait. No digest when nothing new came in.
//!
//! A digest is a notification of the kind `request_digest` (muted on the
//! account page like every other kind), queued for e-mail only and stored as
//! read, so it does not count in the bell: [`crate::delivery`] sends it with
//! its retries and checks again at sending time that the user may still read
//! the group and every item the text names (`mentioned_projects`,
//! `mentioned_items`). The table `request_digests` remembers per user and
//! group up to when the digests covered the requests; it is updated in the
//! transaction that queues the digest, so a restart or a second server never
//! sends a period twice.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{domain, error::AppResult, services::access};

/// How often the background task looks for due digests.
const TICK: Duration = Duration::from_secs(600);
/// Request lines listed in one digest (the rest is counted).
const MAX_LINES: usize = 50;
/// Numbers of older waiting requests listed (the rest is counted).
const MAX_OLDER: usize = 20;

/// A request in a digest.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct DigestLine {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub priority: String,
    pub requested_by_name: String,
    pub created_at: DateTime<Utc>,
    /// Raised since the previous digest.
    pub fresh: bool,
}

/// Title and body of the digest of `project` (`None` = nothing new).
pub fn digest_text(project: &str, lines: &[DigestLine]) -> Option<(String, String)> {
    let fresh: Vec<&DigestLine> = lines.iter().filter(|l| l.fresh).collect();
    if fresh.is_empty() {
        return None;
    }
    let older: Vec<&DigestLine> = lines.iter().filter(|l| !l.fresh).collect();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let title = format!(
        "{} new change request{} in {project} wait{} for triage",
        fresh.len(),
        plural(fresh.len()),
        if fresh.len() == 1 { "s" } else { "" }
    );
    let mut body =
        format!("Raised in project group {project} since the last digest and not triaged yet:\n\n");
    for l in fresh.iter().take(MAX_LINES) {
        let priority = if l.priority == "normal" {
            String::new()
        } else {
            format!("{}, ", l.priority)
        };
        body.push_str(&format!(
            "* {} {} ({priority}raised by {} on {})\n",
            l.number,
            l.title,
            l.requested_by_name,
            l.created_at.format("%Y-%m-%d")
        ));
    }
    if fresh.len() > MAX_LINES {
        body.push_str(&format!("* … and {} more\n", fresh.len() - MAX_LINES));
    }
    if !older.is_empty() {
        let numbers: Vec<&str> = older
            .iter()
            .take(MAX_OLDER)
            .map(|l| l.number.as_str())
            .collect();
        let more = if older.len() > MAX_OLDER {
            format!(" and {} more", older.len() - MAX_OLDER)
        } else {
            String::new()
        };
        body.push_str(&format!(
            "\n{} older request{} still wait{} for triage: {}{more}.\n",
            older.len(),
            plural(older.len()),
            if older.len() == 1 { "s" } else { "" },
            numbers.join(", ")
        ));
    }
    body.push_str(
        "\nTriage them in the change request list: accept, reject or defer them, \
         or start the triage so they leave this digest.",
    );
    Some((title, body))
}

/// A user and a project group whose digest is due.
#[derive(sqlx::FromRow)]
struct Due {
    user_id: Uuid,
    project_id: Uuid,
    project_code: String,
}

/// Queues the digests that are due (every `period_hours`; `0` = never).
/// Returns how many were queued.
pub async fn run_once(pool: &PgPool, period_hours: u64) -> AppResult<u64> {
    if period_hours == 0 {
        return Ok(0);
    }
    let period = format!("{period_hours} hours");
    // CAD editors of active groups with e-mail on and the kind not muted,
    // whose last digest of the group is a period ago, in groups with
    // requests waiting for triage. Members of a restricted group see it.
    let due = sqlx::query_as::<_, Due>(
        "SELECT u.id AS user_id, p.id AS project_id, p.code AS project_code
         FROM users u
         JOIN project_members pm ON pm.user_id = u.id
         JOIN projects p ON p.id = pm.project_id
         LEFT JOIN request_digests d ON d.user_id = u.id AND d.project_id = p.id
         WHERE u.active AND u.role IN ('editor', 'admin') AND p.active
           AND u.notify_email AND COALESCE(u.email, '') <> ''
           AND NOT ('request_digest' = ANY(u.notify_muted))
           AND (d.covered_until IS NULL OR d.covered_until <= now() - $1::interval)
           AND EXISTS (SELECT 1 FROM change_requests r
                       JOIN change_request_items cri ON cri.request_id = r.id
                       JOIN items i ON i.id = cri.item_id
                       WHERE i.project_id = p.id AND r.status = 'new')
         ORDER BY u.id, p.code",
    )
    .bind(&period)
    .fetch_all(pool)
    .await?;
    let mut queued = 0;
    for d in due {
        let mut tx = pool.begin().await?;
        if queue_one(&mut tx, &d, &period).await? {
            queued += 1;
        }
        tx.commit().await?;
    }
    Ok(queued)
}

/// Queues the digest of one user and group if it is (still) due, and moves
/// the covered period on.
async fn queue_one(tx: &mut Transaction<'_, Postgres>, d: &Due, period: &str) -> AppResult<bool> {
    // The first digest covers the last period.
    sqlx::query(
        "INSERT INTO request_digests (user_id, project_id, covered_until)
         VALUES ($1, $2, now() - $3::interval) ON CONFLICT DO NOTHING",
    )
    .bind(d.user_id)
    .bind(d.project_id)
    .bind(period)
    .execute(&mut **tx)
    .await?;
    // Locked: another server (or an overlapping run) waits here and then
    // finds the period covered.
    let (since, due): (DateTime<Utc>, bool) = sqlx::query_as(
        "SELECT covered_until, covered_until <= now() - $3::interval
         FROM request_digests WHERE user_id = $1 AND project_id = $2 FOR UPDATE",
    )
    .bind(d.user_id)
    .bind(d.project_id)
    .bind(period)
    .fetch_one(&mut **tx)
    .await?;
    if !due {
        return Ok(false);
    }
    // The requests of the group the user may read (a request that also
    // touches a group hidden from the user stays out).
    let lines = sqlx::query_as::<_, DigestLine>(&format!(
        "SELECT DISTINCT r.id, r.number, r.title, r.priority,
                COALESCE(ru.display_name, '') AS requested_by_name, r.created_at,
                r.created_at > $3 AS fresh
         FROM change_requests r
         JOIN change_request_items cri ON cri.request_id = r.id
         JOIN items i ON i.id = cri.item_id
         JOIN users u ON u.id = $1
         LEFT JOIN users ru ON ru.id = r.requested_by
         WHERE i.project_id = $2 AND r.status = 'new' AND {}
         ORDER BY r.created_at, r.number",
        access::request_visible_for("r", "u.id", &access::hidden_of_user("u"))
    ))
    .bind(d.user_id)
    .bind(d.project_id)
    .bind(since)
    .fetch_all(&mut **tx)
    .await?;
    let queued = match digest_text(&d.project_code, &lines) {
        None => false,
        Some((title, body)) => {
            let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
            sqlx::query(
                "INSERT INTO notifications (id, user_id, kind, title, body, link, actor_name,
                                            read_at, email_state, chat_state, confidential,
                                            mentioned_items, mentioned_projects)
                 SELECT gen_random_uuid(), $1, 'request_digest', $3, $4, '/requests?status=new',
                        'TeamCraft', now(), 'pending', 'none',
                        EXISTS (SELECT 1 FROM projects WHERE id = $2 AND visibility = 'restricted'),
                        ARRAY(SELECT DISTINCT item_id FROM change_request_items
                              WHERE request_id = ANY($5)),
                        ARRAY[$2]::uuid[]",
            )
            .bind(d.user_id)
            .bind(d.project_id)
            .bind(domain::truncate_chars(&title, 300))
            .bind(domain::truncate_chars(&body, 4000))
            .bind(&ids)
            .execute(&mut **tx)
            .await?;
            true
        }
    };
    sqlx::query(
        "UPDATE request_digests SET covered_until = now() WHERE user_id = $1 AND project_id = $2",
    )
    .bind(d.user_id)
    .bind(d.project_id)
    .execute(&mut **tx)
    .await?;
    Ok(queued)
}

/// Starts the background task (nothing when `period_hours` is `0`).
pub fn spawn(pool: PgPool, period_hours: u64) {
    if period_hours == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match run_once(&pool, period_hours).await {
                Ok(n) if n > 0 => tracing::info!(digests = n, "queued change request digests"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "change request digests failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(number: &str, fresh: bool, priority: &str) -> DigestLine {
        DigestLine {
            id: Uuid::new_v4(),
            number: number.into(),
            title: format!("Title of {number}"),
            priority: priority.into(),
            requested_by_name: "Erika Muster".into(),
            created_at: DateTime::parse_from_rfc3339("2026-11-05T08:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            fresh,
        }
    }

    #[test]
    fn nothing_new_no_digest() {
        assert!(digest_text("ROVER", &[]).is_none());
        assert!(digest_text("ROVER", &[line("CR-000001", false, "normal")]).is_none());
    }

    #[test]
    fn new_and_older_requests() {
        let (title, body) = digest_text(
            "ROVER",
            &[
                line("CR-000001", false, "normal"),
                line("CR-000004", true, "high"),
                line("CR-000005", true, "normal"),
            ],
        )
        .unwrap();
        assert_eq!(title, "2 new change requests in ROVER wait for triage");
        assert!(body.contains(
            "* CR-000004 Title of CR-000004 (high, raised by Erika Muster on 2026-11-05)"
        ));
        assert!(
            body.contains("* CR-000005 Title of CR-000005 (raised by Erika Muster on 2026-11-05)")
        );
        assert!(body.contains("1 older request still waits for triage: CR-000001."));
        let (title, _) = digest_text("ROVER", &[line("CR-000009", true, "normal")]).unwrap();
        assert_eq!(title, "1 new change request in ROVER waits for triage");
    }

    #[test]
    fn long_digests_are_cut() {
        let mut lines: Vec<DigestLine> = (0..60)
            .map(|i| line(&format!("CR-{i:06}"), true, "normal"))
            .collect();
        lines.extend((0..25).map(|i| line(&format!("CR-9{i:05}"), false, "normal")));
        let (_, body) = digest_text("ROVER", &lines).unwrap();
        assert!(body.contains("* … and 10 more"));
        assert!(body.contains("25 older requests still wait for triage:"));
        assert!(body.contains(" and 5 more."));
        assert!(!body.contains("CR-000055"));
    }
}
