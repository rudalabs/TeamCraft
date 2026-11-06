//! Result values proposed by the assistant from a report.
//!
//! The assistant reads a CFD or FEA report (`read_file_text`) and proposes key
//! numbers with `propose_result_values`. A proposal is a **draft**: it names
//! the file version it was read from and quotes the text, nothing more. It
//! becomes a result value only when a CAD editor of the project group
//! accepts it in TeamCraft while the simulation revision is checked out to
//! them – the same rule as for a value typed in. The assistant never writes a
//! final value and never releases anything. Every proposal, acceptance and
//! rejection is audited in its own transaction.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain,
    error::{AppError, AppResult},
    models::Revision,
    services::{
        access, audit, revisions,
        simulations::{self, Origin, ResultInput},
    },
};

/// Proposals in one call.
pub const MAX_PER_CALL: usize = 50;

/// Pending proposals of one revision.
pub const MAX_PENDING: i64 = 200;

/// Longest quote kept.
pub const MAX_QUOTE_CHARS: usize = 400;

/// A proposed result value.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Proposal {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub lower_limit: Option<f64>,
    pub upper_limit: Option<f64>,
    pub load_case: String,
    pub solver: String,
    pub solver_version: String,
    /// The file the value was read from, and its version.
    pub source_file: String,
    pub source_version: i32,
    /// The text of the report the value stands in.
    pub quote: String,
    /// `pending`, `accepted` or `rejected`.
    pub status: String,
    pub proposed_by: Uuid,
    pub proposed_by_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub decided_by_name: Option<String>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
}

const SELECT: &str = "
    SELECT p.id, p.revision_id, p.name, p.value, p.unit, p.lower_limit, p.upper_limit,
           p.load_case, p.solver, p.solver_version, p.source_file, p.source_version, p.quote,
           p.status, p.proposed_by, u.display_name AS proposed_by_name, p.created_at,
           d.display_name AS decided_by_name, p.decided_at
    FROM result_proposals p
    JOIN users u ON u.id = p.proposed_by
    LEFT JOIN users d ON d.id = p.decided_by";

fn not_found() -> AppError {
    AppError::not_found("Proposed value not found.")
}

/// What the assistant proposes.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProposalInput {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub unit: String,
    #[serde(default, alias = "limit_min", alias = "limit_low")]
    pub lower_limit: Option<f64>,
    #[serde(default, alias = "limit_max", alias = "limit_high")]
    pub upper_limit: Option<f64>,
    #[serde(default)]
    pub load_case: String,
    #[serde(default)]
    pub solver: String,
    #[serde(default)]
    pub solver_version: String,
    /// File name in the revision.
    pub file: String,
    /// The text of the report that holds the value, as `read_file_text` gave it.
    pub quote: String,
}

/// Lower case, runs of blanks as one space: quotes survive line breaks.
fn squash(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The proposals of a revision the actor may see, pending first.
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    status: Option<&str>,
) -> AppResult<Vec<Proposal>> {
    revisions::get(pool, actor, revision_id).await?;
    Ok(sqlx::query_as::<_, Proposal>(&format!(
        "{SELECT} WHERE p.revision_id = $1 AND ($2::text IS NULL OR p.status = $2)
         ORDER BY (p.status <> 'pending'), p.created_at DESC, lower(p.name)"
    ))
    .bind(revision_id)
    .bind(status)
    .fetch_all(pool)
    .await?)
}

/// One proposal, if the actor may see its revision.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Proposal> {
    let p = sqlx::query_as::<_, Proposal>(&format!("{SELECT} WHERE p.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(not_found)?;
    revisions::get(pool, actor, p.revision_id)
        .await
        .map_err(|_| not_found())?;
    Ok(p)
}

/// Whether the actor may work on the project group of the revision.
fn ensure_may_edit(rev: &Revision, actor: &Actor) -> AppResult<()> {
    if actor.can_edit() && actor.can_edit_project(rev.project_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only CAD editors of this project group can propose, accept or reject result values."
                .into(),
        ))
    }
}

async fn lock_visible(
    conn: &mut PgConnection,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Revision> {
    let rev = revisions::lock(conn, revision_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    Ok(rev)
}

/// Stores proposals as drafts (all or none). Needs a simulation revision
/// that is still in work and a CAD editor of its project group; the file
/// must exist in the revision and the quote must stand in its text.
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group; the revision is visible
///   (404), belongs to a simulation (400) and is in work (409). No check-out
///   is needed: a proposal is a draft, not a result value.
/// * **Locks:** the simulation revision (`FOR UPDATE`).
/// * **Data:** at most 50 values per call (400); the file must be in the
///   revision and the quote in its current text (400). A newer proposal
///   replaces a pending one of the same name and load case; more than 200
///   pending proposals on the revision is a 400.
/// * **Audit:** `simulation.proposal.create` per proposal, including the quote
///   and the file version.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction; all proposals are stored or none.
pub async fn propose(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    inputs: Vec<ProposalInput>,
) -> AppResult<Vec<Proposal>> {
    let mut tx = pool.begin().await?;
    let rev = lock_visible(&mut tx, actor, revision_id).await?;
    simulations::ensure_simulation(&rev)?;
    ensure_may_edit(&rev, actor)?;
    if !revisions::state_of(&rev).is_editable() {
        return Err(AppError::Conflict(format!(
            "{} is {} – values are proposed for revisions that are still in work.",
            revisions::label(&rev),
            revisions::state_of(&rev).label()
        )));
    }
    if inputs.is_empty() {
        return Err(AppError::BadRequest("Give at least one value.".into()));
    }
    if inputs.len() > MAX_PER_CALL {
        return Err(AppError::BadRequest(format!(
            "At most {MAX_PER_CALL} values can be proposed at once."
        )));
    }
    let mut ids = Vec::with_capacity(inputs.len());
    let mut seen = std::collections::HashSet::new();
    for (i, input) in inputs.into_iter().enumerate() {
        let at = |e: AppError| AppError::BadRequest(format!("values[{i}]: {}", e.public_message()));
        let checked = ResultInput {
            name: input.name.clone(),
            value: input.value,
            unit: input.unit.clone(),
            lower_limit: input.lower_limit,
            upper_limit: input.upper_limit,
            load_case: input.load_case.clone(),
            solver: input.solver.clone(),
            solver_version: input.solver_version.clone(),
            note: String::new(),
            source: String::new(),
        }
        .validated()
        .map_err(at)?;
        if !seen.insert((
            checked.name.to_lowercase(),
            checked.load_case.to_lowercase(),
        )) {
            return Err(AppError::BadRequest(format!(
                "values[{i}]: '{}' is proposed twice.",
                checked.name
            )));
        }
        let file = domain::require_text("File", &input.file, 255).map_err(at)?;
        let quote = domain::require_text("Quote", &input.quote, MAX_QUOTE_CHARS).map_err(at)?;
        // the file, its text and the quote
        let row: Option<(String, i32, String)> = sqlx::query_as(
            "SELECT f.filename, f.current_version, v.content_text
             FROM files f JOIN file_versions v ON v.file_id = f.id AND v.version = f.current_version
             WHERE f.revision_id = $1 AND lower(f.filename) = lower($2)",
        )
        .bind(rev.id)
        .bind(&file)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((filename, version, text)) = row else {
            return Err(AppError::BadRequest(format!(
                "values[{i}]: the file '{file}' is not in {}.",
                revisions::label(&rev)
            )));
        };
        if !squash(&text).contains(&squash(&quote)) {
            return Err(AppError::BadRequest(format!(
                "values[{i}]: the quote for '{}' does not stand in {filename} v{version} – quote \
                 the text exactly as read_file_text returned it.",
                checked.name
            )));
        }
        // a newer proposal replaces a pending one of the same value
        sqlx::query(
            "DELETE FROM result_proposals
             WHERE revision_id = $1 AND status = 'pending'
               AND lower(name) = lower($2) AND lower(load_case) = lower($3)",
        )
        .bind(rev.id)
        .bind(&checked.name)
        .bind(&checked.load_case)
        .execute(&mut *tx)
        .await?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO result_proposals (id, revision_id, name, value, unit, lower_limit,
                    upper_limit, load_case, solver, solver_version, source_file, source_version,
                    quote, proposed_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(rev.id)
        .bind(&checked.name)
        .bind(checked.value)
        .bind(&checked.unit)
        .bind(checked.lower_limit)
        .bind(checked.upper_limit)
        .bind(&checked.load_case)
        .bind(&checked.solver)
        .bind(&checked.solver_version)
        .bind(&filename)
        .bind(version)
        .bind(&quote)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            Some(actor),
            "simulation.proposal.create",
            "revision",
            Some(rev.id),
            format!(
                "{}: proposed {} = {} {} from {filename} v{version}",
                revisions::label(&rev),
                checked.name,
                checked.value,
                checked.unit
            ),
            json!({ "item_id": rev.item_id, "proposal_id": id, "file": filename,
                    "version": version, "quote": quote }),
        )
        .await?;
        ids.push(id);
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM result_proposals WHERE revision_id = $1 AND status = 'pending'",
    )
    .bind(rev.id)
    .fetch_one(&mut *tx)
    .await?;
    if pending > MAX_PENDING {
        return Err(AppError::BadRequest(format!(
            "{} has {MAX_PENDING} pending proposals already – accept or reject them first.",
            revisions::label(&rev)
        )));
    }
    tx.commit().await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(get(pool, actor, id).await?);
    }
    Ok(out)
}

/// Makes a pending proposal a result value (same name and load case = update).
/// The revision must be checked out to the actor, like for any change of its
/// values.
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group, simulation revision in
///   work and checked out by the actor (403 / 409), as for a value typed in.
///   A proposal that is not pending gets a 409.
/// * **Locks:** the proposal row, then the simulation revision (both
///   `FOR UPDATE`), so two accepts of one proposal cannot both succeed.
/// * **Data:** the value is written by `simulations::write_results` with its
///   source (file, version, proposer); the proposal becomes `accepted`.
/// * **Audit:** `simulation.proposal.accept` (written by `write_results`).
/// * **Notifications:** none.
/// * **Webhooks:** `simulation.result_added`, in the same transaction.
/// * **Atomic:** one transaction (`tests/result_proposals.rs`); a second accept
///   gets a 409.
pub async fn accept(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Proposal> {
    let existing = get(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM result_proposals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(not_found)?;
    if status != "pending" {
        return Err(AppError::Conflict(format!(
            "This value was already {status}."
        )));
    }
    let rev = simulations::lock_editable(&mut tx, actor, existing.revision_id).await?;
    ensure_may_edit(&rev, actor)?;
    let input = ResultInput {
        name: existing.name.clone(),
        value: existing.value,
        unit: existing.unit.clone(),
        lower_limit: existing.lower_limit,
        upper_limit: existing.upper_limit,
        load_case: existing.load_case.clone(),
        solver: existing.solver.clone(),
        solver_version: existing.solver_version.clone(),
        note: String::new(),
        source: format!(
            "{} v{} (proposed by {})",
            existing.source_file, existing.source_version, existing.proposed_by_name
        ),
    };
    simulations::write_results(
        &mut tx,
        actor,
        &rev,
        &[input],
        false,
        &Origin::Proposal(existing.source_file.clone()),
    )
    .await?;
    sqlx::query(
        "UPDATE result_proposals SET status = 'accepted', decided_by = $2, decided_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Rejects a pending proposal (no check-out needed: nothing changes in the
/// revision).
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group; a proposal that is not
///   pending gets a 409.
/// * **Locks:** the proposal row, then the simulation revision (both
///   `FOR UPDATE`).
/// * **Audit:** `simulation.proposal.reject`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction (`tests/result_proposals.rs`).
pub async fn reject(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Proposal> {
    let existing = get(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM result_proposals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(not_found)?;
    if status != "pending" {
        return Err(AppError::Conflict(format!(
            "This value was already {status}."
        )));
    }
    let rev = lock_visible(&mut tx, actor, existing.revision_id).await?;
    ensure_may_edit(&rev, actor)?;
    sqlx::query(
        "UPDATE result_proposals SET status = 'rejected', decided_by = $2, decided_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "simulation.proposal.reject",
        "revision",
        Some(rev.id),
        format!(
            "{}: rejected the proposed value {} from {}",
            revisions::label(&rev),
            existing.name,
            existing.source_file
        ),
        json!({ "item_id": rev.item_id, "proposal_id": id, "name": existing.name }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_match_across_line_breaks_and_case() {
        let text = "Pressure   drop\nat nominal flow: 388.25 Pa";
        assert!(squash(text).contains(&squash("pressure drop at nominal flow: 388.25 PA")));
        assert!(!squash(text).contains(&squash("pressure drop: 388.25")));
    }
}
