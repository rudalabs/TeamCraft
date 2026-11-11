//! What a user still holds (issue #173, step 1): the report an administrator
//! or a group manager reads before somebody leaves – check-outs, change
//! orders they wrote that are still open, pending reviews, change requests
//! assigned to or raised by them, items they own, their memberships (and the
//! groups in which they are the only active manager) and their API tokens.
//!
//! The report is filtered by the **viewer's** rights, never the leaver's: a
//! lock in a group the leaver was removed from is still listed, which is the
//! lock nobody else would find. Administrators see every group they may
//! read; for a group that hides its content from them (need-to-know policy)
//! only the number of check-outs per group. A manager who is no
//! administrator sees what lies in the groups they manage, and no tokens.
//!
//! Read-only; every list is bounded by [`MAX_LIST`] with the full count next
//! to it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    models::ApiToken,
    services::{access, users},
};

/// Most entries of one kind in a report.
pub const MAX_LIST: i64 = 200;

/// A bounded list with the number of entries there are in all.
#[derive(Clone, Debug, Serialize)]
pub struct Listed<T> {
    pub total: i64,
    pub shown: Vec<T>,
}

impl<T> Listed<T> {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// A revision the user has checked out.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct HeldRevision {
    pub revision_id: Uuid,
    pub item_number: String,
    pub code: String,
    pub name: String,
    pub project_code: Option<String>,
    pub checked_out_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    total: i64,
}

/// A change order (authored and still open, or waiting for the user's review).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct HeldChange {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub status: String,
    #[serde(skip)]
    total: i64,
}

/// A change request assigned to or raised by the user.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct HeldRequest {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub status: String,
    #[serde(skip)]
    total: i64,
}

/// An item the user owns.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct HeldItem {
    pub id: Uuid,
    pub number: String,
    /// Name of the newest revision.
    pub name: String,
    pub project_code: Option<String>,
    #[serde(skip)]
    total: i64,
}

/// Check-outs in a group whose content the viewer may not read: the count.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct GroupCount {
    pub project_code: String,
    pub count: i64,
}

/// A membership of the user.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct HeldMembership {
    pub project_id: Uuid,
    pub project_code: String,
    pub project_name: String,
    /// `manual` or `oidc`.
    pub source: String,
    pub manager: bool,
    /// The user is a manager and no other active user manages the group.
    pub only_active_manager: bool,
}

/// Everything the viewer may know about what a user still holds.
#[derive(Clone, Debug, Serialize)]
pub struct Holdings {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub active: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    /// `all` for administrators, `managed` for a manager (only the groups
    /// they manage).
    pub scope: &'static str,
    pub checkouts: Listed<HeldRevision>,
    /// Administrators only: check-outs in groups that hide their content
    /// from them, as a count per group.
    pub hidden_checkouts: Vec<GroupCount>,
    /// Change orders the user wrote: draft, in review or rejected.
    pub authored_changes: Listed<HeldChange>,
    /// Change orders that wait for the user's review.
    pub pending_reviews: Listed<HeldChange>,
    /// Open change requests assigned to the user.
    pub assigned_requests: Listed<HeldRequest>,
    /// Open change requests the user raised.
    pub raised_requests: Listed<HeldRequest>,
    pub owned_items: Listed<HeldItem>,
    pub memberships: Vec<HeldMembership>,
    /// Administrators only (never the secret, see `models::ApiToken`).
    pub tokens: Option<Vec<ApiToken>>,
}

impl Holdings {
    /// Nothing listed: the user holds nothing the viewer may know of
    /// (memberships and tokens aside, which do not block work).
    pub fn blocks_nothing(&self) -> bool {
        self.checkouts.is_empty()
            && self.hidden_checkouts.is_empty()
            && self.authored_changes.is_empty()
            && self.pending_reviews.is_empty()
            && self.assigned_requests.is_empty()
            && self.raised_requests.is_empty()
            && self.owned_items.is_empty()
            && !self.memberships.iter().any(|m| m.only_active_manager)
    }
}

/// SQL: the change `c` touches a group of the scope bound at `n`
/// (`uuid[]`, NULL = no limit).
fn change_in_scope(n: usize) -> String {
    format!(
        "(${n}::uuid[] IS NULL OR EXISTS (
            SELECT 1 FROM change_items sci
            JOIN revisions sr ON sr.id = sci.revision_id
            JOIN items si ON si.id = sr.item_id
            WHERE sci.change_id = c.id AND si.project_id = ANY(${n})))"
    )
}

/// SQL: the request `r` names an item of a group of the scope bound at `n`.
fn request_in_scope(n: usize) -> String {
    format!(
        "(${n}::uuid[] IS NULL OR EXISTS (
            SELECT 1 FROM change_request_items sri
            JOIN items si ON si.id = sri.item_id
            WHERE sri.request_id = r.id AND si.project_id = ANY(${n})))"
    )
}

/// The open change requests whose `column` (`assignee` or `requested_by`,
/// never input) is the user, as the actor may see them.
async fn open_requests(
    pool: &PgPool,
    actor: &Actor,
    user_id: Uuid,
    column: &'static str,
    scope: Option<&[Uuid]>,
) -> AppResult<Vec<HeldRequest>> {
    Ok(sqlx::query_as::<_, HeldRequest>(&format!(
        "SELECT r.id, r.number, r.title, r.status, COUNT(*) OVER () AS total
         FROM change_requests r
         WHERE r.{column} = $1 AND r.status IN ('new', 'in_triage', 'accepted', 'deferred')
           AND {} AND {}
         ORDER BY r.number LIMIT $5",
        access::request_visible("r", 2, 3),
        request_in_scope(4)
    ))
    .bind(user_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .bind(scope)
    .bind(MAX_LIST)
    .fetch_all(pool)
    .await?)
}

/// The holdings of a user as `actor` may see them.
///
/// # Invariants
///
/// * **Permission:** administrators, and managers of at least one project
///   group (403 otherwise); an unknown user is "not found".
/// * **Visibility:** every list is filtered with the guards of
///   `services::access` for the **actor**; a manager additionally sees only
///   what lies in the groups they manage (items without a group are for
///   administrators). Check-outs in groups hidden from an administrator are
///   a count per group, without item numbers. Tokens are for administrators.
/// * **Data:** read-only; at most [`MAX_LIST`] entries per list.
pub async fn holdings(pool: &PgPool, actor: &Actor, user_id: Uuid) -> AppResult<Holdings> {
    // the groups a non-administrator may report on: managed and readable
    let scope: Option<Vec<Uuid>> = if actor.is_admin() {
        None
    } else {
        let managed: Vec<Uuid> = if actor.can_edit() {
            sqlx::query_scalar(
                "SELECT project_id FROM project_members WHERE user_id = $1 AND manager",
            )
            .bind(actor.id)
            .fetch_all(pool)
            .await?
        } else {
            Vec::new()
        };
        let managed: Vec<Uuid> = managed
            .into_iter()
            .filter(|p| actor.can_see_project(Some(*p)))
            .collect();
        if managed.is_empty() {
            return Err(AppError::Forbidden(
                "Only administrators and managers of a project group can see what a user holds."
                    .into(),
            ));
        }
        Some(managed)
    };
    let user = users::get(pool, user_id).await?;

    let checkouts = sqlx::query_as::<_, HeldRevision>(&format!(
        "SELECT r.id AS revision_id, i.number AS item_number, r.code, r.name,
                p.code AS project_code, r.checked_out_at, COUNT(*) OVER () AS total
         FROM revisions r JOIN items i ON i.id = r.item_id
         LEFT JOIN projects p ON p.id = i.project_id
         WHERE r.checked_out_by = $1 AND {}
           AND ($3::uuid[] IS NULL OR i.project_id = ANY($3))
         ORDER BY r.checked_out_at NULLS LAST, i.number, r.code LIMIT $4",
        access::item_visible("i", 2)
    ))
    .bind(user_id)
    .bind(&actor.hidden_projects)
    .bind(scope.as_deref())
    .bind(MAX_LIST)
    .fetch_all(pool)
    .await?;

    let hidden_checkouts = if actor.is_admin() && !actor.hidden_projects.is_empty() {
        sqlx::query_as::<_, GroupCount>(
            "SELECT p.code AS project_code, COUNT(*) AS count
             FROM revisions r JOIN items i ON i.id = r.item_id
             JOIN projects p ON p.id = i.project_id
             WHERE r.checked_out_by = $1 AND i.project_id = ANY($2)
             GROUP BY p.code ORDER BY p.code",
        )
        .bind(user_id)
        .bind(&actor.hidden_projects)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };

    let authored_changes = sqlx::query_as::<_, HeldChange>(&format!(
        "SELECT c.id, c.number, c.title, c.status, COUNT(*) OVER () AS total
         FROM changes c
         WHERE c.created_by = $1 AND c.status IN ('draft', 'in_review', 'rejected')
           AND {} AND {}
         ORDER BY c.number LIMIT $5",
        access::change_visible("c", 2, 3),
        change_in_scope(4)
    ))
    .bind(user_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .bind(scope.as_deref())
    .bind(MAX_LIST)
    .fetch_all(pool)
    .await?;

    let pending_reviews = sqlx::query_as::<_, HeldChange>(&format!(
        "SELECT c.id, c.number, c.title, c.status, COUNT(*) OVER () AS total
         FROM change_reviews rv JOIN changes c ON c.id = rv.change_id
         WHERE rv.reviewer_id = $1 AND rv.decision = 'pending'
           AND c.status IN ('draft', 'in_review')
           AND {} AND {}
         ORDER BY c.number LIMIT $5",
        access::change_visible("c", 2, 3),
        change_in_scope(4)
    ))
    .bind(user_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .bind(scope.as_deref())
    .bind(MAX_LIST)
    .fetch_all(pool)
    .await?;

    let assigned_requests =
        open_requests(pool, actor, user_id, "assignee", scope.as_deref()).await?;
    let raised_requests =
        open_requests(pool, actor, user_id, "requested_by", scope.as_deref()).await?;

    let owned_items = sqlx::query_as::<_, HeldItem>(&format!(
        "SELECT i.id, i.number,
                COALESCE((SELECT lr.name FROM revisions lr WHERE lr.item_id = i.id
                          ORDER BY lr.seq DESC LIMIT 1), '') AS name,
                p.code AS project_code, COUNT(*) OVER () AS total
         FROM items i LEFT JOIN projects p ON p.id = i.project_id
         WHERE i.owner_id = $1 AND {}
           AND ($3::uuid[] IS NULL OR i.project_id = ANY($3))
         ORDER BY i.number LIMIT $4",
        access::item_visible("i", 2)
    ))
    .bind(user_id)
    .bind(&actor.hidden_projects)
    .bind(scope.as_deref())
    .bind(MAX_LIST)
    .fetch_all(pool)
    .await?;

    let memberships = sqlx::query_as::<_, HeldMembership>(
        "SELECT p.id AS project_id, p.code AS project_code, p.name AS project_name,
                pm.source, pm.manager,
                (pm.manager AND NOT EXISTS (
                    SELECT 1 FROM project_members o JOIN users ou ON ou.id = o.user_id
                    WHERE o.project_id = p.id AND o.manager AND ou.active
                      AND o.user_id <> pm.user_id)) AS only_active_manager
         FROM project_members pm JOIN projects p ON p.id = pm.project_id
         WHERE pm.user_id = $1 AND ($2::uuid[] IS NULL OR p.id = ANY($2))
         ORDER BY p.code",
    )
    .bind(user_id)
    .bind(scope.as_deref())
    .fetch_all(pool)
    .await?;

    let tokens = if actor.is_admin() {
        Some(users::list_tokens(pool, user_id).await?)
    } else {
        None
    };

    Ok(Holdings {
        user_id,
        username: user.username,
        display_name: user.display_name,
        active: user.active,
        last_login_at: user.last_login_at,
        scope: if scope.is_none() { "all" } else { "managed" },
        checkouts: Listed {
            total: checkouts.first().map_or(0, |r| r.total),
            shown: checkouts,
        },
        hidden_checkouts,
        authored_changes: Listed {
            total: authored_changes.first().map_or(0, |r| r.total),
            shown: authored_changes,
        },
        pending_reviews: Listed {
            total: pending_reviews.first().map_or(0, |r| r.total),
            shown: pending_reviews,
        },
        assigned_requests: Listed {
            total: assigned_requests.first().map_or(0, |r| r.total),
            shown: assigned_requests,
        },
        raised_requests: Listed {
            total: raised_requests.first().map_or(0, |r| r.total),
            shown: raised_requests,
        },
        owned_items: Listed {
            total: owned_items.first().map_or(0, |r| r.total),
            shown: owned_items,
        },
        memberships,
        tokens,
    })
}

/// The report as plain text lines (the `teamcraft user holdings` command).
pub fn to_text(h: &Holdings) -> String {
    fn section<T>(out: &mut String, title: &str, list: &Listed<T>, line: impl Fn(&T) -> String) {
        out.push_str(&format!("{title}: {}\n", list.total));
        for entry in &list.shown {
            out.push_str(&format!("  {}\n", line(entry)));
        }
        if list.total > list.shown.len() as i64 {
            out.push_str(&format!(
                "  … and {} more\n",
                list.total - list.shown.len() as i64
            ));
        }
    }
    let group = |code: &Option<String>| code.clone().unwrap_or_else(|| "-".into());
    let mut out = format!(
        "{} ({}), {}, last sign-in: {}\n",
        h.display_name,
        h.username,
        if h.active { "active" } else { "deactivated" },
        h.last_login_at
            .map_or_else(|| "never".to_string(), |t| t.to_rfc3339())
    );
    section(&mut out, "Check-outs", &h.checkouts, |r| {
        format!(
            "{}/{}  {}  [{}]  since {}",
            r.item_number,
            r.code,
            r.name,
            group(&r.project_code),
            r.checked_out_at
                .map_or_else(|| "?".to_string(), |t| t.to_rfc3339())
        )
    });
    for g in &h.hidden_checkouts {
        out.push_str(&format!(
            "  {} check-outs in {} (content hidden from administrators)\n",
            g.count, g.project_code
        ));
    }
    let change = |c: &HeldChange| format!("{}  {}  ({})", c.number, c.title, c.status);
    section(
        &mut out,
        "Open change orders they wrote",
        &h.authored_changes,
        change,
    );
    section(&mut out, "Pending reviews", &h.pending_reviews, change);
    let request = |r: &HeldRequest| format!("{}  {}  ({})", r.number, r.title, r.status);
    section(
        &mut out,
        "Open change requests assigned to them",
        &h.assigned_requests,
        request,
    );
    section(
        &mut out,
        "Open change requests they raised",
        &h.raised_requests,
        request,
    );
    section(&mut out, "Items they own", &h.owned_items, |i| {
        format!("{}  {}  [{}]", i.number, i.name, group(&i.project_code))
    });
    out.push_str(&format!("Memberships: {}\n", h.memberships.len()));
    for m in &h.memberships {
        out.push_str(&format!(
            "  {}  {}  {}{}{}\n",
            m.project_code,
            m.project_name,
            if m.source == "oidc" {
                "identity provider"
            } else {
                "manual"
            },
            if m.manager { ", manager" } else { "" },
            if m.only_active_manager {
                " – the ONLY active manager"
            } else {
                ""
            }
        ));
    }
    if let Some(tokens) = &h.tokens {
        out.push_str(&format!("API tokens: {}\n", tokens.len()));
        for t in tokens {
            out.push_str(&format!(
                "  {}  {}  last used: {}  expires: {}\n",
                t.name,
                t.scope,
                t.last_used_at
                    .map_or_else(|| "never".to_string(), |x| x.to_rfc3339()),
                t.expires_at
                    .map_or_else(|| "never".to_string(), |x| x.to_rfc3339())
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty<T>() -> Listed<T> {
        Listed {
            total: 0,
            shown: Vec::new(),
        }
    }

    fn report() -> Holdings {
        Holdings {
            user_id: Uuid::nil(),
            username: "lena".into(),
            display_name: "Lena".into(),
            active: false,
            last_login_at: None,
            scope: "all",
            checkouts: Listed {
                total: 3,
                shown: vec![HeldRevision {
                    revision_id: Uuid::nil(),
                    item_number: "PRT-000001".into(),
                    code: "B".into(),
                    name: "Upright".into(),
                    project_code: Some("CHASSIS".into()),
                    checked_out_at: None,
                    total: 3,
                }],
            },
            hidden_checkouts: vec![GroupCount {
                project_code: "NTK".into(),
                count: 2,
            }],
            authored_changes: empty(),
            pending_reviews: empty(),
            assigned_requests: empty(),
            raised_requests: empty(),
            owned_items: empty(),
            memberships: vec![HeldMembership {
                project_id: Uuid::nil(),
                project_code: "CHASSIS".into(),
                project_name: "Chassis".into(),
                source: "oidc".into(),
                manager: true,
                only_active_manager: true,
            }],
            tokens: Some(Vec::new()),
        }
    }

    #[test]
    fn the_text_report_counts_lists_and_flags() {
        let text = to_text(&report());
        assert!(text.starts_with("Lena (lena), deactivated, last sign-in: never\n"));
        assert!(text.contains("Check-outs: 3\n  PRT-000001/B  Upright  [CHASSIS]"));
        assert!(text.contains("… and 2 more"), "the list is cut: {text}");
        assert!(text.contains("2 check-outs in NTK (content hidden"));
        assert!(text.contains("Pending reviews: 0\n"));
        assert!(text.contains("identity provider, manager – the ONLY active manager"));
        assert!(text.contains("API tokens: 0"));
    }

    #[test]
    fn memberships_and_tokens_alone_block_nothing() {
        let mut h = report();
        assert!(!h.blocks_nothing());
        h.checkouts = empty();
        h.hidden_checkouts.clear();
        assert!(!h.blocks_nothing(), "still the only active manager");
        h.memberships[0].only_active_manager = false;
        assert!(h.blocks_nothing());
    }

    #[test]
    fn the_scope_fragments_bind_one_position() {
        assert!(change_in_scope(4).contains("$4::uuid[] IS NULL"));
        assert!(change_in_scope(4).contains("ANY($4)"));
        assert!(request_in_scope(7).contains("ANY($7)"));
    }
}
