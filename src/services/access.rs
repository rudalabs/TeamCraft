//! Read access control: the only place that writes the SQL guards.
//!
//! A project group is either `internal` (everybody reads) or `restricted`
//! (only its members and administrators read). The [`Actor`] carries the
//! restricted groups it may *not* read (`hidden_projects`, empty for admins);
//! every guard binds that array. Hidden objects answer with exactly the same
//! "not found" error as missing ones, so their existence is not confirmed.
//!
//! Change orders and change requests have no group of their own; their
//! rules are [`change_visible`] and [`request_visible`].

use uuid::Uuid;

use crate::{auth::Actor, error::AppError};

/// SQL guard: the item aliased `alias` is visible. `n` is the bind position
/// of `actor.hidden_projects` (`uuid[]`).
///
/// The `IS NULL OR` prefix is mandatory: `NULL <> ALL('{x}')` is NULL and
/// would hide items without a group.
pub fn item_visible(alias: &str, n: usize) -> String {
    format!("({alias}.project_id IS NULL OR {alias}.project_id <> ALL(${n}))")
}

/// SQL guard on a bare project-id expression (e.g. `w.project_id`).
pub fn project_visible(expr: &str, n: usize) -> String {
    project_visible_in(expr, &format!("${n}"))
}

/// Like [`project_visible`] with an SQL expression for the hidden groups.
pub fn project_visible_in(expr: &str, hidden: &str) -> String {
    format!("({expr} IS NULL OR {expr} <> ALL({hidden}))")
}

/// SQL guard: the change aliased `c` is visible to the actor bound at
/// `actor_n` (uuid) whose hidden groups are bound at `hidden_n`. Visible to
/// administrators, its author, its reviewers and anyone who sees every
/// affected item of a change that has items (an empty draft is therefore
/// visible to its author, reviewers and admins only).
pub fn change_visible(c: &str, actor_n: usize, hidden_n: usize) -> String {
    change_visible_for(c, &format!("${actor_n}"), &format!("${hidden_n}"))
}

/// [`change_visible`] with SQL expressions for the actor id and the hidden
/// groups (used for notification recipients).
pub fn change_visible_for(c: &str, actor: &str, hidden: &str) -> String {
    format!(
        "({c}.created_by = {actor}
          OR EXISTS (SELECT 1 FROM users aau WHERE aau.id = {actor} AND aau.role = 'admin')
          OR EXISTS (SELECT 1 FROM change_reviews acr
                     WHERE acr.change_id = {c}.id AND acr.reviewer_id = {actor})
          OR (EXISTS (SELECT 1 FROM change_items aci WHERE aci.change_id = {c}.id)
              AND NOT EXISTS (SELECT 1 FROM change_items aci
                              JOIN revisions ar ON ar.id = aci.revision_id
                              JOIN items ai ON ai.id = ar.item_id
                              WHERE aci.change_id = {c}.id
                                AND NOT {})))",
        project_visible_in("ai.project_id", hidden)
    )
}

/// SQL guard: the change request aliased `r` is visible: requester,
/// assignee, or every affected item visible (which includes requests
/// without items).
pub fn request_visible(r: &str, actor_n: usize, hidden_n: usize) -> String {
    request_visible_for(r, &format!("${actor_n}"), &format!("${hidden_n}"))
}

/// [`request_visible`] with SQL expressions for actor id and hidden groups.
pub fn request_visible_for(r: &str, actor: &str, hidden: &str) -> String {
    format!(
        "({r}.requested_by = {actor}
          OR {r}.assignee IS NOT DISTINCT FROM {actor}
          OR NOT EXISTS (SELECT 1 FROM change_request_items acri
                         JOIN items ai ON ai.id = acri.item_id
                         WHERE acri.request_id = {r}.id AND NOT {}))",
        project_visible_in("ai.project_id", hidden)
    )
}

/// Recipient-side check: the user aliased `u` may see an item of project
/// expression `p` (admins, items without a group, internal groups, members).
pub fn user_sees_project(u: &str, p: &str) -> String {
    format!(
        "({u}.role = 'admin' OR {p} IS NULL
          OR NOT EXISTS (SELECT 1 FROM projects asp WHERE asp.id = {p} AND asp.visibility = 'restricted')
          OR EXISTS (SELECT 1 FROM project_members asm WHERE asm.project_id = {p} AND asm.user_id = {u}.id))"
    )
}

/// The restricted groups a user may not read, as an SQL expression
/// (`uuid[]`) for the user aliased `u`. Used to evaluate the change and
/// request guards for notification recipients.
pub fn hidden_of_user(u: &str) -> String {
    format!(
        "(CASE WHEN {u}.role = 'admin' THEN '{{}}'::uuid[] ELSE
           COALESCE((SELECT array_agg(hp.id) FROM projects hp
                     WHERE hp.visibility = 'restricted'
                       AND NOT EXISTS (SELECT 1 FROM project_members hpm
                                       WHERE hpm.project_id = hp.id AND hpm.user_id = {u}.id)),
                    '{{}}'::uuid[]) END)"
    )
}

/// Fails with the regular "not found" error unless the actor may read the
/// project group. `not_found` is the exact error a missing object gives.
pub fn ensure_visible(
    actor: &Actor,
    project_id: Option<Uuid>,
    not_found: AppError,
) -> Result<(), AppError> {
    if actor.can_see_project(project_id) {
        Ok(())
    } else {
        Err(not_found)
    }
}

pub fn not_found_item() -> AppError {
    AppError::not_found("Item not found.")
}

pub fn not_found_revision() -> AppError {
    AppError::not_found("Revision not found.")
}

pub fn not_found_file() -> AppError {
    AppError::not_found("File not found.")
}

pub fn not_found_project() -> AppError {
    AppError::not_found("Project not found.")
}

/// The error of a form field naming an unknown project group (also for a
/// hidden one: its code and state are never told).
pub fn unknown_project() -> AppError {
    AppError::BadRequest("Unknown project.".into())
}

/// Text of a placeholder row that stands for a hidden item.
pub const RESTRICTED_LABEL: &str = "restricted item";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_guard_handles_items_without_group() {
        assert_eq!(
            item_visible("i", 3),
            "(i.project_id IS NULL OR i.project_id <> ALL($3))"
        );
        assert!(!item_visible("i", 3).contains("NOT (")); // never NOT (x = ANY(..))
        assert!(change_visible("c", 1, 2).contains("ai.project_id <> ALL($2)"));
        assert!(request_visible("r", 1, 2).contains("$1"));
        assert!(hidden_of_user("u").contains("'{}'::uuid[]"));
    }
}
