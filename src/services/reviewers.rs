//! Suggested reviewers for a change order: who knows the affected items?
//!
//! Deterministic and explainable – no model, only the history TeamCraft
//! already keeps. Every candidate collects points from these sources, each
//! with a reason that is shown as is:
//!
//! | Source                                                            | Points            |
//! |-------------------------------------------------------------------|-------------------|
//! | edits of an affected item in the last 90 days (audit log)         | 3 + one per edit  |
//! | decision on an earlier change order with the same items           | 4 (pending: 2)    |
//! | owner (creator, author of the latest revision) of a dependent     | 2                 |
//! | manager of the project group of an affected item                  | 3                 |
//! | requester of a change request this change implements              | 1                 |
//!
//! Each source is capped per candidate so that one busy source cannot drown
//! the others. Candidates are active CAD editors and admins (as
//! `changes::add_reviewer` requires), never the author of the change and
//! never someone who is already a reviewer. People who changed the affected
//! revisions know the item best but may not review it (four-eyes): they are
//! listed after the real candidates with `cannot_review`, as information. Ties are broken by
//! name, so the same data always gives the same order.
//!
//! The whole lookup is one statement whose parts are bounded (affected
//! items, earlier changes, dependents, result rows).

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::AppResult,
    services::{access, changes},
};

/// Most suggestions in one answer.
pub const MAX_LIMIT: usize = 20;
/// Suggestions shown next to the reviewer picker.
pub const DEFAULT_LIMIT: usize = 5;
/// Look-back for edits.
pub const EDIT_DAYS: i32 = 90;
/// Reasons kept per suggestion.
const MAX_REASONS: usize = 4;
/// Bounds of the parts of the query.
const MAX_ITEMS: i64 = 200;
const MAX_EARLIER_CHANGES: i64 = 50;
const MAX_DEPENDENTS: i64 = 200;
const MAX_ROWS: i64 = 5000;
/// Rows per user and source that can still add points: the smallest
/// weight of a source divides its cap at most this often (review: 12 / 2).
const MAX_PER_SOURCE: i64 = 6;

/// Audit actions on a revision that count as editing it.
pub(crate) const EDIT_ACTIONS: &[&str] = &[
    "revision.checkin",
    "revision.update",
    "revision.revise",
    "file.upload",
    "file.version",
    "file.restore",
    "file.delete",
    "file.folder",
    "bom.add",
    "bom.update",
    "bom.remove",
    "reference.add",
    "reference.remove",
    "import.bom",
    "bom.replace",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Suggestion {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub score: i64,
    /// Knows the item but changed it, so the four-eyes principle forbids
    /// the review: shown as information, not offered as a reviewer.
    pub cannot_review: bool,
    /// Most important first, e.g. "edited PRT-000005 3 times in the last 90 days".
    pub reasons: Vec<String>,
}

/// One piece of evidence for one user (a row of the query).
#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct Signal {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    /// `edit`, `review`, `dependent`, `manager` or `request`.
    pub kind: String,
    /// Item, change, project group or request number.
    pub subject: String,
    /// For `dependent`: the affected item it depends on.
    pub via: String,
    /// For `review`: the decision; for `dependent`: the relation.
    pub detail: String,
    pub n: i64,
    /// Changed the affected revisions: knows the item but may not review it.
    pub editor: bool,
}

/// Latest, not obsolete revision of an item (SQL condition on `pr`).
const LATEST: &str = "pr.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = pr.item_id)
                      AND pr.state <> 'obsolete'";

/// Ranked reviewer suggestions for a change order (at most `limit`, 1–20).
/// Read-only; it never assigns anybody.
pub async fn suggest(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    limit: usize,
) -> AppResult<Vec<Suggestion>> {
    let change = changes::get(pool, actor, change_id).await?;
    // Who may review depends on every item; with items the actor cannot see
    // the list would tell who belongs to their groups (and adding a
    // reviewer is refused anyway, see `changes::add_reviewer`).
    {
        let mut conn = pool.acquire().await?;
        if !changes::sees_all_items(&mut conn, actor, change_id).await? {
            return Ok(Vec::new());
        }
    }
    let sql = format!(
        "WITH ci AS MATERIALIZED (
             SELECT r.id AS revision_id, r.item_id, i.number, i.project_id
             FROM change_items ci
             JOIN revisions r ON r.id = ci.revision_id
             JOIN items i ON i.id = r.item_id
             WHERE ci.change_id = $1 AND {vis_i}
             ORDER BY i.number LIMIT $3
         ),
         ci_revs AS MATERIALIZED (
             SELECT r.id, ci.number FROM revisions r JOIN ci ON ci.item_id = r.item_id
         ),
         earlier AS (
             SELECT DISTINCT c.id, c.number, c.created_at
             FROM change_items oci
             JOIN ci_revs cr ON cr.id = oci.revision_id
             JOIN changes c ON c.id = oci.change_id
             WHERE c.id <> $1 AND c.status <> 'cancelled' AND {vis_c}
             ORDER BY c.created_at DESC LIMIT $4
         ),
         dep_edges AS (
             SELECT pr.item_id, ci.number AS via, 'bom'::text AS relation, 1 AS rank
             FROM bom_lines bl
             JOIN ci ON ci.item_id = bl.child_item_id
             JOIN revisions pr ON pr.id = bl.parent_revision_id
             WHERE {LATEST}
             UNION ALL
             SELECT pr.item_id, ci.number, ir.kind, 2
             FROM item_references ir
             JOIN ci ON ci.item_id = ir.to_item_id
             JOIN revisions pr ON pr.id = ir.from_revision_id
             WHERE {LATEST}
             UNION ALL
             SELECT pr.item_id, ci.number, 'file'::text, 3
             FROM ci
             JOIN files sf ON sf.revision_id = ci.revision_id
             JOIN file_references fr ON lower(fr.name) = lower(sf.filename)
             JOIN file_versions dv ON dv.id = fr.file_version_id
             JOIN files df ON df.id = dv.file_id AND dv.version = df.current_version
             JOIN revisions pr ON pr.id = df.revision_id
             WHERE {LATEST}
         ),
         deps_bounded AS (
             SELECT * FROM (
                 SELECT DISTINCT ON (e.item_id) e.item_id, di.number, e.via, e.relation,
                        di.created_by
                 FROM dep_edges e
                 JOIN items di ON di.id = e.item_id
                 WHERE NOT EXISTS (SELECT 1 FROM ci WHERE ci.item_id = e.item_id) AND {vis_di}
                 ORDER BY e.item_id, e.rank, e.via
             ) d
             ORDER BY d.number LIMIT $5
         ),
         -- the latest revision's author only for the bounded dependents
         deps AS (
             SELECT d.*, lr.created_by AS revision_by
             FROM deps_bounded d
             CROSS JOIN LATERAL (SELECT x.created_by FROM revisions x WHERE x.item_id = d.item_id
                                 ORDER BY x.seq DESC LIMIT 1) lr
         ),
         signals AS (
             SELECT a.user_id, 'edit'::text AS kind, cr.number AS subject, ''::text AS via,
                    ''::text AS detail, COUNT(*)::bigint AS n
             FROM audit_log a
             JOIN ci_revs cr ON a.entity_type = 'revision' AND a.entity_id = cr.id
             WHERE a.user_id IS NOT NULL AND a.action = ANY($6)
               AND a.at > now() - make_interval(days => $7)
             GROUP BY a.user_id, cr.number
             UNION ALL
             SELECT rv.reviewer_id, 'review'::text, e.number, ''::text, rv.decision, 1::bigint
             FROM earlier e JOIN change_reviews rv ON rv.change_id = e.id
             UNION ALL
             SELECT DISTINCT o.user_id, 'dependent'::text, d.number, d.via, d.relation, 1::bigint
             FROM deps d CROSS JOIN LATERAL (VALUES (d.created_by), (d.revision_by)) o(user_id)
             UNION ALL
             SELECT DISTINCT pm.user_id, 'manager'::text, p.code, ''::text, ''::text, 1::bigint
             FROM project_members pm JOIN projects p ON p.id = pm.project_id
             WHERE pm.manager AND pm.project_id IN (SELECT project_id FROM ci)
             UNION ALL
             SELECT rq.requested_by, 'request'::text, rq.number, ''::text, ''::text, 1::bigint
             FROM change_request_changes crc JOIN change_requests rq ON rq.id = crc.request_id
             WHERE crc.change_id = $1 AND {vis_rq}
         ),
         -- points as in weigh(); only the strongest rows per user and source
         -- can count (cap()), and a cut by $8 drops the weakest rows, not
         -- the users late in the alphabet
         eligible AS (
             SELECT s.*, u.username, u.display_name, u.role,
                    u.id IN (SELECT e.user_id FROM ({editors}) e WHERE e.user_id IS NOT NULL) AS editor,
                    CASE s.kind WHEN 'edit' THEN 3 + LEAST(s.n, 7)
                                WHEN 'review' THEN CASE WHEN s.detail IN ('approved', 'rejected')
                                                        THEN 4 ELSE 2 END
                                WHEN 'dependent' THEN 2
                                WHEN 'manager' THEN 3
                                ELSE 1 END AS points
             FROM signals s JOIN users u ON u.id = s.user_id
             WHERE u.active AND u.role IN ('editor', 'admin') AND u.id <> $2
               AND ($12::uuid IS NULL OR u.id <> $12)
               AND NOT EXISTS (SELECT 1 FROM change_reviews x
                               WHERE x.change_id = $1 AND x.reviewer_id = u.id)
               AND NOT EXISTS (SELECT 1 FROM change_items sci
                               JOIN revisions sr ON sr.id = sci.revision_id
                               JOIN items si ON si.id = sr.item_id
                               WHERE sci.change_id = $1 AND NOT {sees})
         ),
         ranked AS (
             SELECT e.*, ROW_NUMBER() OVER (PARTITION BY e.user_id, e.kind
                                            ORDER BY e.points DESC, e.subject, e.via) AS pos
             FROM eligible e
         )
         SELECT user_id, username, display_name, role, editor, kind, subject, via, detail, n
         FROM ranked
         WHERE pos <= $9
         ORDER BY points DESC, username, kind, subject, via
         LIMIT $8",
        vis_i = access::item_visible("i", 10),
        vis_di = access::item_visible("di", 10),
        vis_c = access::change_visible("c", 11, 10),
        vis_rq = access::request_visible("rq", 11, 10),
        sees = access::user_sees_project("u", "si.project_id"),
        editors = changes::editors_sql(6),
    );
    let signals = sqlx::query_as::<_, Signal>(&sql)
        .bind(change_id)
        .bind(change.created_by)
        .bind(MAX_ITEMS)
        .bind(MAX_EARLIER_CHANGES)
        .bind(MAX_DEPENDENTS)
        .bind(EDIT_ACTIONS)
        .bind(EDIT_DAYS)
        .bind(MAX_ROWS)
        .bind(MAX_PER_SOURCE)
        .bind(&actor.hidden_projects)
        .bind(actor.id)
        .bind((actor.is_admin() && change.created_by != actor.id).then_some(actor.id))
        .fetch_all(pool)
        .await?;
    Ok(rank(signals, limit))
}

/// Points and reason of one signal.
fn weigh(s: &Signal) -> Option<(&'static str, i64, String)> {
    let n = s.n.max(1);
    Some(match s.kind.as_str() {
        "edit" => (
            "edit",
            3 + n.min(7),
            if n == 1 {
                format!("edited {} once in the last {EDIT_DAYS} days", s.subject)
            } else {
                format!(
                    "edited {} {n} times in the last {EDIT_DAYS} days",
                    s.subject
                )
            },
        ),
        "review" => match s.detail.as_str() {
            "approved" | "rejected" => {
                ("review", 4, format!("reviewed {} (same items)", s.subject))
            }
            _ => (
                "review",
                2,
                format!("asked to review {} (same items)", s.subject),
            ),
        },
        "dependent" => {
            let how = match s.detail.as_str() {
                "bom" => format!("which uses {}", s.via),
                "drawing" => format!("a drawing of {}", s.via),
                "specification" => format!("a specification for {}", s.via),
                "file" => format!("whose CAD files reference {}", s.via),
                _ => format!("which references {}", s.via),
            };
            ("dependent", 2, format!("owns {}, {how}", s.subject))
        }
        "manager" => ("manager", 3, format!("manager of {}", s.subject)),
        "request" => (
            "request",
            1,
            format!("raised {}, implemented by this change", s.subject),
        ),
        _ => return None,
    })
}

/// Most points one source gives a candidate.
fn cap(kind: &str) -> i64 {
    match kind {
        "edit" => 15,
        "review" => 12,
        "dependent" => 6,
        "manager" => 6,
        _ => 2,
    }
}

/// Aggregates the signals per user and ranks them: score, then name.
pub(crate) fn rank(signals: Vec<Signal>, limit: usize) -> Vec<Suggestion> {
    struct Acc {
        s: Suggestion,
        per_kind: BTreeMap<&'static str, i64>,
        reasons: Vec<(i64, String)>,
    }
    let mut by_user: BTreeMap<Uuid, Acc> = BTreeMap::new();
    for sig in signals {
        let Some((kind, points, reason)) = weigh(&sig) else {
            continue;
        };
        let acc = by_user.entry(sig.user_id).or_insert_with(|| Acc {
            s: Suggestion {
                user_id: sig.user_id,
                username: sig.username.clone(),
                display_name: sig.display_name.clone(),
                role: sig.role.clone(),
                score: 0,
                cannot_review: false,
                reasons: Vec::new(),
            },
            per_kind: BTreeMap::new(),
            reasons: Vec::new(),
        });
        acc.s.cannot_review |= sig.editor;
        if acc.reasons.iter().any(|(_, r)| *r == reason) {
            continue;
        }
        let used = acc.per_kind.entry(kind).or_insert(0);
        let add = points.min(cap(kind) - *used).max(0);
        *used += add;
        acc.s.score += add;
        acc.reasons.push((points, reason));
    }
    let mut out: Vec<Suggestion> = by_user
        .into_values()
        .filter(|a| a.s.score > 0)
        .map(|mut a| {
            a.reasons
                .sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
            a.s.reasons = a
                .reasons
                .into_iter()
                .take(MAX_REASONS)
                .map(|(_, r)| r)
                .collect();
            a.s
        })
        .collect();
    out.sort_by(|a, b| {
        a.cannot_review
            .cmp(&b.cannot_review)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.display_name.cmp(&b.display_name))
            .then_with(|| a.username.cmp(&b.username))
    });
    out.truncate(limit.clamp(1, MAX_LIMIT));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(user: u128, name: &str, kind: &str, subject: &str, detail: &str, n: i64) -> Signal {
        Signal {
            user_id: Uuid::from_u128(user),
            username: name.to_lowercase(),
            display_name: name.into(),
            role: "editor".into(),
            kind: kind.into(),
            subject: subject.into(),
            via: "PRT-000001".into(),
            detail: detail.into(),
            n,
            editor: false,
        }
    }

    #[test]
    fn ranks_by_score_then_name_with_reasons() {
        let out = rank(
            vec![
                sig(1, "Max", "manager", "ROVER", "", 1),
                sig(2, "Erika", "edit", "PRT-000005", "", 3),
                sig(2, "Erika", "review", "ECO-000012", "approved", 1),
                sig(3, "Anna", "manager", "ROVER", "", 1),
                sig(4, "Bob", "dependent", "ASM-000003", "bom", 1),
                sig(4, "Bob", "dependent", "ASM-000003", "bom", 1),
                sig(5, "Zed", "unknown", "X", "", 1),
            ],
            5,
        );
        let names: Vec<&str> = out.iter().map(|s| s.display_name.as_str()).collect();
        assert_eq!(names, ["Erika", "Anna", "Max", "Bob"]);
        assert_eq!(out[0].score, 6 + 4);
        assert_eq!(
            out[0].reasons,
            [
                "edited PRT-000005 3 times in the last 90 days",
                "reviewed ECO-000012 (same items)"
            ]
        );
        // the duplicate row counts once
        assert_eq!(out[3].score, 2);
        assert_eq!(out[3].reasons, ["owns ASM-000003, which uses PRT-000001"]);
        assert_eq!(rank(Vec::new(), 5), Vec::new());
    }

    #[test]
    fn editors_are_listed_last_and_flagged() {
        let mut editor = sig(1, "Anna", "edit", "PRT-000001", "", 5);
        editor.editor = true;
        let out = rank(vec![editor, sig(2, "Zed", "manager", "ROVER", "", 1)], 5);
        let names: Vec<(&str, bool)> = out
            .iter()
            .map(|s| (s.display_name.as_str(), s.cannot_review))
            .collect();
        assert_eq!(names, [("Zed", false), ("Anna", true)]);
        // the limit cuts the editors first
        let mut editor = sig(1, "Anna", "edit", "PRT-000001", "", 5);
        editor.editor = true;
        let out = rank(vec![editor, sig(2, "Zed", "manager", "ROVER", "", 1)], 1);
        assert_eq!(out[0].display_name, "Zed");
    }

    #[test]
    fn sources_are_capped_and_limit_is_bounded() {
        let mut signals: Vec<Signal> = (0..50)
            .map(|i| sig(1, "Busy", "dependent", &format!("ASM-{i:06}"), "bom", 1))
            .collect();
        signals.push(sig(1, "Busy", "edit", "PRT-000001", "", 1));
        signals.push(sig(2, "Calm", "edit", "PRT-000001", "", 1_000_000));
        let out = rank(signals, 0);
        assert_eq!(out.len(), 1, "limit 0 means one");
        assert_eq!(out[0].display_name, "Busy");
        assert_eq!(out[0].score, 6 + 4);
        assert_eq!(out[0].reasons.len(), MAX_REASONS);
        assert_eq!(
            out[0].reasons[0],
            "edited PRT-000001 once in the last 90 days"
        );
        let out = rank(
            vec![sig(2, "Calm", "edit", "PRT-000001", "", 1_000_000)],
            99,
        );
        assert_eq!(out[0].score, 10);
        let many: Vec<Signal> = (0..40)
            .map(|i| sig(i, &format!("U{i:02}"), "manager", "ROVER", "", 1))
            .collect();
        assert_eq!(rank(many, 1000).len(), MAX_LIMIT);
    }
}
