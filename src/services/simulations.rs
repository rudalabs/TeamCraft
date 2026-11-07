//! Simulation management (FEA, CFD, …).
//!
//! A **simulation** is an item of type `simulation` with the usual
//! lifecycle, revisions, check-out/check-in and change orders. Its files are
//! the model, the solver deck, selected results, the report and pictures. On
//! top of that a simulation revision has
//!
//! * **basis links** – the item revisions it was computed with and,
//!   optionally, the exact file version (role geometry, mesh, material, load
//!   case or other). They belong to the simulation revision like BOM lines:
//!   changed only while it is checked out, copied by Revise and frozen at
//!   release;
//! * **result values** – key numbers such as the maximum stress or the
//!   pressure drop with unit, optional limits (pass/fail), load case and
//!   solver. They are entered in the UI, imported from a CSV/Excel table or
//!   written by a solver script through the REST API, and can be compared
//!   between two revisions.
//!
//! A simulation is **out of date** when a newer revision of a basis item was
//! released, the basis revision became obsolete or the simulated file has a
//! newer version. Releasing or obsoleting a basis also flags the simulation
//! for a check through the change propagation (`impacts`, relation
//! `simulation`); acknowledging that check means "still valid, because …".

use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    bomtable,
    domain::{self, BasisRole, ItemType, ResultStatus, WebhookEvent},
    error::{AppError, AppResult},
    models::Revision,
    services::{access, audit, bom_compare::Change, items, revisions, webhooks},
};

/// At most this many result values per simulation revision.
pub const MAX_RESULTS: usize = 2000;

// ---------------------------------------------------------------------------
// Basis links
// ---------------------------------------------------------------------------

/// A revision (and file version) a simulation revision is based on.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct SimulationBasis {
    pub id: Uuid,
    /// The simulation revision.
    pub revision_id: Uuid,
    pub role: String,
    pub note: String,
    pub basis_item_id: Uuid,
    pub basis_number: String,
    pub basis_type: String,
    pub basis_revision_id: Uuid,
    pub basis_code: String,
    #[serde(skip)]
    pub basis_seq: i32,
    pub basis_state: String,
    pub basis_name: String,
    /// The simulated file version, if one was chosen.
    pub file_version_id: Option<Uuid>,
    pub file_id: Option<Uuid>,
    pub file_name: String,
    pub file_version: Option<i32>,
    /// The current version of that file now.
    pub file_current_version: Option<i32>,
    /// Newest released revision of the basis item.
    pub released_revision_id: Option<Uuid>,
    pub released_code: Option<String>,
    #[serde(skip)]
    pub released_seq: Option<i32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Why the simulation no longer matches this basis (`None` = up to date).
    #[sqlx(default)]
    pub out_of_date: Option<String>,
    /// Project group of the basis item (for the read check; never sent).
    #[serde(skip)]
    pub basis_project_id: Option<Uuid>,
    /// Placeholder for a basis the reader may not see: the role stays; the
    /// identity, the note and whether it is out of date are removed.
    #[sqlx(default)]
    pub restricted: bool,
}

impl SimulationBasis {
    fn mask(&mut self) {
        let label = access::RESTRICTED_LABEL;
        self.basis_item_id = Uuid::nil();
        self.basis_number = label.into();
        self.basis_type = String::new();
        self.basis_revision_id = Uuid::nil();
        self.basis_code = String::new();
        self.basis_state = String::new();
        self.basis_name = String::new();
        self.file_version_id = None;
        self.file_id = None;
        self.file_name = String::new();
        self.file_version = None;
        self.file_current_version = None;
        self.released_revision_id = None;
        self.released_code = None;
        // written about the hidden basis, possibly by a member
        self.note = String::new();
        // computed from the hidden item (a release, a file check-in,
        // obsolescence): polling it would tell when the hidden item changes
        self.out_of_date = None;
        self.basis_project_id = None;
        self.restricted = true;
    }

    /// Masks the basis when the actor may not see it.
    fn redact(mut self, actor: &Actor) -> Self {
        if !actor.can_see_project(self.basis_project_id) {
            self.mask();
        }
        self
    }
}

/// Fails with "not found" unless the actor may read the revision.
async fn ensure_revision_visible(pool: &PgPool, actor: &Actor, revision_id: Uuid) -> AppResult<()> {
    revisions::get(pool, actor, revision_id).await.map(|_| ())
}

const BASIS_SELECT: &str = "
    SELECT sb.id, sb.revision_id, sb.role, sb.note,
           bi.id AS basis_item_id, bi.number AS basis_number, bi.item_type AS basis_type,
           br.id AS basis_revision_id, br.code AS basis_code, br.seq AS basis_seq,
           br.state AS basis_state, br.name AS basis_name,
           sb.file_version_id, f.id AS file_id, sb.file_name, sb.file_version,
           f.current_version AS file_current_version,
           lr.id AS released_revision_id, lr.code AS released_code, lr.seq AS released_seq,
           sb.created_at, bi.project_id AS basis_project_id
    FROM simulation_bases sb
    JOIN revisions br ON br.id = sb.basis_revision_id
    JOIN items bi ON bi.id = br.item_id
    LEFT JOIN file_versions fv ON fv.id = sb.file_version_id
    LEFT JOIN files f ON f.id = fv.file_id
    LEFT JOIN LATERAL (
        SELECT r.id, r.code, r.seq FROM revisions r
        WHERE r.item_id = bi.id AND r.released_at IS NOT NULL
        ORDER BY r.seq DESC LIMIT 1
    ) lr ON TRUE";

/// What makes a basis out of date, from its stored facts (pure, see the
/// module docs). `None` means the simulation still matches the basis.
pub fn out_of_date_reason(b: &SimulationBasis) -> Option<String> {
    if b.basis_state == "obsolete" {
        return Some(format!("{}/{} is obsolete", b.basis_number, b.basis_code));
    }
    if let (Some(seq), Some(code)) = (b.released_seq, b.released_code.as_deref()) {
        if seq > b.basis_seq {
            return Some(format!(
                "{}/{code} was released (simulated: {})",
                b.basis_number, b.basis_code
            ));
        }
    }
    match (b.file_version, b.file_version_id, b.file_current_version) {
        (Some(v), Some(_), Some(current)) if current > v => Some(format!(
            "{} has version {current} (simulated: v{v})",
            b.file_name
        )),
        (Some(v), None, _) => Some(format!(
            "the simulated version v{v} of {} was deleted",
            b.file_name
        )),
        _ => None,
    }
}

fn with_status(mut b: SimulationBasis) -> SimulationBasis {
    b.out_of_date = out_of_date_reason(&b);
    b
}

/// The basis links of a simulation revision the actor may read; bases the
/// actor may not see are placeholders.
pub async fn bases(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<SimulationBasis>> {
    ensure_revision_visible(pool, actor, revision_id).await?;
    let mut rows: Vec<SimulationBasis> = sqlx::query_as::<_, SimulationBasis>(&format!(
        "{BASIS_SELECT} WHERE sb.revision_id = $1 ORDER BY sb.role, bi.number, sb.file_name"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|b| with_status(b).redact(actor))
    .collect();
    // placeholders last in their role: their position would bound the
    // hidden number
    rows.sort_by(|a, b| (&a.role, a.restricted).cmp(&(&b.role, b.restricted)));
    Ok(rows)
}

pub async fn get_basis(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<SimulationBasis> {
    Ok(get_basis_unmasked(pool, actor, id).await?.redact(actor))
}

/// [`get_basis`] without the placeholder (for the audit trail of a write).
async fn get_basis_unmasked(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<SimulationBasis> {
    let b = sqlx::query_as::<_, SimulationBasis>(&format!("{BASIS_SELECT} WHERE sb.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(with_status)
        .ok_or_else(|| AppError::not_found("Basis link not found."))?;
    ensure_revision_visible(pool, actor, b.revision_id)
        .await
        .map_err(|_| AppError::not_found("Basis link not found."))?;
    Ok(b)
}

/// A simulation based on a revision of an item (the other side of a basis
/// link), shown on the part or assembly.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct BasedSimulation {
    pub simulation_item_id: Uuid,
    pub simulation_number: String,
    pub simulation_code: String,
    pub simulation_state: String,
    pub simulation_name: String,
    /// Result values of the simulation revision and how many failed.
    pub result_count: i64,
    pub failed_count: i64,
    /// An open check (impact) of the simulation against this item.
    pub open_impact_id: Option<Uuid>,
    /// The link itself; `basis.revision_id` is the simulation revision.
    #[sqlx(flatten)]
    pub basis: SimulationBasis,
}

/// Simulations whose latest revision is based on a revision of the item.
/// Only simulations the actor may see; the second value counts the others.
pub async fn based_on_item(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<(Vec<BasedSimulation>, i64)> {
    let all = based_on_item_unchecked(pool, item_id).await?;
    let ids: Vec<Uuid> = all.iter().map(|s| s.simulation_item_id).collect();
    let visible: std::collections::HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT i.id FROM items i WHERE i.id = ANY($1) AND {}",
        access::item_visible("i", 2)
    ))
    .bind(&ids)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let hidden = all
        .iter()
        .filter(|s| !visible.contains(&s.simulation_item_id))
        .map(|s| s.simulation_item_id)
        .collect::<std::collections::HashSet<_>>()
        .len() as i64;
    Ok((
        all.into_iter()
            .filter(|s| visible.contains(&s.simulation_item_id))
            .collect(),
        hidden,
    ))
}

async fn based_on_item_unchecked(pool: &PgPool, item_id: Uuid) -> AppResult<Vec<BasedSimulation>> {
    let rows = sqlx::query_as::<_, BasedSimulation>(&format!(
        "SELECT s.*, si.id AS simulation_item_id, si.number AS simulation_number,
                sr.code AS simulation_code, sr.state AS simulation_state, sr.name AS simulation_name,
                (SELECT COUNT(*) FROM simulation_results x WHERE x.revision_id = sr.id) AS result_count,
                (SELECT COUNT(*) FROM simulation_results x WHERE x.revision_id = sr.id
                   AND ((x.lower_limit IS NOT NULL AND x.value < x.lower_limit)
                     OR (x.upper_limit IS NOT NULL AND x.value > x.upper_limit))) AS failed_count,
                (SELECT im.id FROM impacts im WHERE im.item_id = si.id AND im.source_item_id = $1
                   AND im.resolved_at IS NULL LIMIT 1) AS open_impact_id
         FROM ({BASIS_SELECT} WHERE br.item_id = $1) s
         JOIN revisions sr ON sr.id = s.revision_id
         JOIN items si ON si.id = sr.item_id
         WHERE sr.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = sr.item_id)
         ORDER BY si.number, s.role, s.file_name"
    ))
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|mut s| {
            s.basis = with_status(s.basis);
            s
        })
        .collect())
}

/// Fails unless the revision belongs to a simulation item.
pub(crate) fn ensure_simulation(rev: &Revision) -> AppResult<()> {
    if rev.item_type == ItemType::Simulation.as_str() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "{} is not a simulation; basis links and result values belong to items of type Simulation.",
            revisions::label(rev)
        )))
    }
}

#[derive(Clone, Debug)]
pub struct NewBasis {
    /// Item number of the basis.
    pub item_number: String,
    /// Revision code; empty = the latest revision.
    pub revision_code: String,
    /// Name of a file of that revision: its current version is pinned.
    pub file_name: String,
    /// Or the exact file version (API).
    pub file_version_id: Option<Uuid>,
    pub role: BasisRole,
    pub note: String,
}

/// Adds a basis link to a simulation revision checked out by the actor.
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group of the simulation, which
///   is in work and checked out by the actor (otherwise 403 / 409); the basis
///   item and revision must be visible to the actor (looked up before the
///   transaction, hidden = 404). A simulation cannot be based on itself (400).
/// * **Locks:** the simulation revision (`FOR UPDATE`) and the basis item
///   (`FOR SHARE`), so a concurrent `items::delete` cannot remove it.
/// * **Audit:** `simulation.basis.add` (related to the basis item). A second
///   identical link (same basis revision and role) gets a 409.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn add_basis(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    input: NewBasis,
) -> AppResult<SimulationBasis> {
    let note = domain::limit_text("Note", &input.note, 500)?;
    let item = items::get_by_number(pool, actor, &input.item_number).await?;
    let code = input.revision_code.trim();
    let basis = if code.is_empty() || code.eq_ignore_ascii_case("latest") {
        items::latest_revision(pool, actor, item.id).await?
    } else {
        revisions::get_by_code(pool, actor, item.id, code).await?
    };
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    ensure_simulation(&rev)?;
    revisions::ensure_editable(&rev, actor)?;
    if basis.item_id == rev.item_id {
        return Err(AppError::BadRequest(
            "A simulation cannot be based on itself.".into(),
        ));
    }
    // Hold the basis item against a concurrent delete (`items::delete` locks
    // it FOR UPDATE and then checks `item_is_basis`).
    let basis_exists =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM items WHERE id = $1 FOR SHARE")
            .bind(basis.item_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
    if !basis_exists {
        return Err(AppError::not_found("Item not found."));
    }
    // The simulated file: an exact version or the current version of a file.
    #[derive(sqlx::FromRow)]
    struct Pin {
        id: Uuid,
        filename: String,
        version: i32,
    }
    let pin = if let Some(version_id) = input.file_version_id {
        Some(
            sqlx::query_as::<_, Pin>(
                "SELECT fv.id, f.filename, fv.version FROM file_versions fv
                 JOIN files f ON f.id = fv.file_id
                 WHERE fv.id = $1 AND f.revision_id = $2",
            )
            .bind(version_id)
            .bind(basis.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "The file version does not belong to {}.",
                    revisions::label(&basis)
                ))
            })?,
        )
    } else if !input.file_name.trim().is_empty() {
        let name = input.file_name.trim();
        Some(
            sqlx::query_as::<_, Pin>(
                "SELECT fv.id, f.filename, fv.version FROM files f
                 JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
                 WHERE f.revision_id = $1 AND lower(f.filename) = lower($2)",
            )
            .bind(basis.id)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "{} has no file named '{name}'.",
                    revisions::label(&basis)
                ))
            })?,
        )
    } else {
        None
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO simulation_bases (id, revision_id, basis_revision_id, file_version_id, file_name,
                                       file_version, role, note, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(rev.id)
    .bind(basis.id)
    .bind(pin.as_ref().map(|p| p.id))
    .bind(pin.as_ref().map_or("", |p| p.filename.as_str()))
    .bind(pin.as_ref().map(|p| p.version))
    .bind(input.role.as_str())
    .bind(&note)
    .bind(actor.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!(
                "{} is already based on {} ({}).",
                revisions::label(&rev),
                revisions::label(&basis),
                input.role.label().to_lowercase()
            ),
        )
    })?;
    let file = pin
        .as_ref()
        .map(|p| format!(", {} v{}", p.filename, p.version))
        .unwrap_or_default();
    audit::record_related(
        &mut tx,
        Some(actor),
        "simulation.basis.add",
        "revision",
        Some(rev.id),
        Some(basis.item_id),
        format!(
            "{} based on {}{file} ({})",
            revisions::label(&rev),
            revisions::label(&basis),
            input.role.label().to_lowercase()
        ),
        json!({ "item_id": rev.item_id, "basis_id": id, "basis_revision_id": basis.id,
                "file_version_id": pin.as_ref().map(|p| p.id), "role": input.role.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get_basis(pool, actor, id).await
}

/// Removes a basis link from a simulation revision checked out by the actor.
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group, simulation revision in
///   work and checked out by the actor (403 / 409); a link the actor cannot
///   see is a 404.
/// * **Locks:** the simulation revision (`FOR UPDATE`).
/// * **Audit:** `simulation.basis.remove` (related to the basis item).
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn remove_basis(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<SimulationBasis> {
    let existing = get_basis_unmasked(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, existing.revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    let removed = sqlx::query("DELETE FROM simulation_bases WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found("Basis link not found."));
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "simulation.basis.remove",
        "revision",
        Some(rev.id),
        Some(existing.basis_item_id),
        format!(
            "{} no longer based on {}/{}",
            revisions::label(&rev),
            existing.basis_number,
            existing.basis_code
        ),
        json!({ "item_id": rev.item_id, "basis_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing.redact(actor))
}

/// Copies basis links and result values of `from` to the new revision `to`
/// (Revise). Returns (basis links, result values).
pub(crate) async fn copy_to_revision(
    conn: &mut PgConnection,
    from: Uuid,
    to: Uuid,
) -> AppResult<(u64, u64)> {
    let bases = sqlx::query(
        "INSERT INTO simulation_bases (id, revision_id, basis_revision_id, file_version_id, file_name,
                                       file_version, role, note, created_by, created_at)
         SELECT gen_random_uuid(), $2, basis_revision_id, file_version_id, file_name, file_version,
                role, note, created_by, created_at
         FROM simulation_bases WHERE revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let results = sqlx::query(
        "INSERT INTO simulation_results (id, revision_id, position, name, value, unit, lower_limit,
                                         upper_limit, load_case, solver, solver_version, note,
                                         created_by, created_at, updated_at)
         SELECT gen_random_uuid(), $2, position, name, value, unit, lower_limit, upper_limit,
                load_case, solver, solver_version, note, created_by, created_at, updated_at
         FROM simulation_results WHERE revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok((bases, results))
}

/// Whether a revision is the basis of a simulation (an item with such a
/// revision cannot be deleted).
pub(crate) async fn item_is_basis(conn: &mut PgConnection, item_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM simulation_bases sb JOIN revisions r ON r.id = sb.basis_revision_id
                       WHERE r.item_id = $1)",
    )
    .bind(item_id)
    .fetch_one(&mut *conn)
    .await?)
}

// ---------------------------------------------------------------------------
// Result values
// ---------------------------------------------------------------------------

/// A key result value of a simulation revision.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct SimulationResult {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub position: i32,
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub lower_limit: Option<f64>,
    pub upper_limit: Option<f64>,
    pub load_case: String,
    pub solver: String,
    pub solver_version: String,
    pub note: String,
    /// Where an automatic value came from (`file:line`), empty for typed ones.
    pub source: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `pass`, `fail` or `none` (no limits).
    #[sqlx(skip)]
    pub status: Option<ResultStatus>,
}

impl SimulationResult {
    fn with_status(mut self) -> Self {
        self.status = Some(domain::result_status(
            self.value,
            self.lower_limit,
            self.upper_limit,
        ));
        self
    }

    fn key(&self) -> (String, String) {
        (self.name.to_lowercase(), self.load_case.to_lowercase())
    }
}

const RESULT_SELECT: &str = "
    SELECT id, revision_id, position, name, value, unit, lower_limit, upper_limit, load_case,
           solver, solver_version, note, source, updated_at
    FROM simulation_results";

/// The result values of a revision the actor may read, in their order.
pub async fn results(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<SimulationResult>> {
    ensure_revision_visible(pool, actor, revision_id).await?;
    results_unchecked(pool, revision_id).await
}

pub(crate) async fn results_unchecked(
    pool: &PgPool,
    revision_id: Uuid,
) -> AppResult<Vec<SimulationResult>> {
    Ok(sqlx::query_as::<_, SimulationResult>(&format!(
        "{RESULT_SELECT} WHERE revision_id = $1 ORDER BY position, lower(load_case), lower(name)"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(SimulationResult::with_status)
    .collect())
}

pub async fn get_result(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<SimulationResult> {
    let r = sqlx::query_as::<_, SimulationResult>(&format!("{RESULT_SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(SimulationResult::with_status)
        .ok_or_else(|| AppError::not_found("Result value not found."))?;
    ensure_revision_visible(pool, actor, r.revision_id)
        .await
        .map_err(|_| AppError::not_found("Result value not found."))?;
    Ok(r)
}

/// How many values passed and failed.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ResultSummary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub without_limits: usize,
}

pub fn summarize(results: &[SimulationResult]) -> ResultSummary {
    let mut s = ResultSummary {
        total: results.len(),
        ..Default::default()
    };
    for r in results {
        match r.status {
            Some(ResultStatus::Pass) => s.pass += 1,
            Some(ResultStatus::Fail) => s.fail += 1,
            _ => s.without_limits += 1,
        }
    }
    s
}

/// A result value as entered or imported.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct ResultInput {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub unit: String,
    #[serde(default, alias = "limit_low")]
    pub lower_limit: Option<f64>,
    #[serde(default, alias = "limit_high")]
    pub upper_limit: Option<f64>,
    #[serde(default)]
    pub load_case: String,
    #[serde(default)]
    pub solver: String,
    #[serde(default)]
    pub solver_version: String,
    #[serde(default)]
    pub note: String,
    /// Where the value came from (`file:line`, set by exchange files and
    /// templates; never read from a request).
    #[serde(skip)]
    pub source: String,
}

impl ResultInput {
    pub fn validated(self) -> AppResult<ResultInput> {
        let name = domain::require_text("Result name", &self.name, 200)?;
        domain::validate_result_numbers(&name, self.value, self.lower_limit, self.upper_limit)?;
        Ok(ResultInput {
            unit: domain::limit_text("Unit", &self.unit, 32)?,
            load_case: domain::limit_text("Load case", &self.load_case, 200)?,
            solver: domain::limit_text("Solver", &self.solver, 100)?,
            solver_version: domain::limit_text("Solver version", &self.solver_version, 100)?,
            note: domain::limit_text("Note", &self.note, 1000)?,
            name,
            ..self
        })
    }
}

/// Locks a simulation revision the actor may edit.
pub(crate) async fn lock_editable(
    conn: &mut PgConnection,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Revision> {
    let rev = revisions::lock(conn, revision_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    ensure_simulation(&rev)?;
    revisions::ensure_editable(&rev, actor)?;
    Ok(rev)
}

async fn insert_result(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    input: &ResultInput,
) -> AppResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO simulation_results (id, revision_id, position, name, value, unit, lower_limit,
                                         upper_limit, load_case, solver, solver_version, note, created_by, source)
         VALUES ($1, $2, (SELECT COALESCE(MAX(position), 0) + 1 FROM simulation_results WHERE revision_id = $2),
                 $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(id)
    .bind(rev.id)
    .bind(&input.name)
    .bind(input.value)
    .bind(&input.unit)
    .bind(input.lower_limit)
    .bind(input.upper_limit)
    .bind(&input.load_case)
    .bind(&input.solver)
    .bind(&input.solver_version)
    .bind(&input.note)
    .bind(actor.id)
    .bind(&input.source)
    .execute(&mut *conn)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!(
                "{} already has a result '{}'{}.",
                revisions::label(rev),
                input.name,
                if input.load_case.is_empty() {
                    String::new()
                } else {
                    format!(" for load case '{}'", input.load_case)
                }
            ),
        )
    })?;
    Ok(id)
}

async fn count_results(conn: &mut PgConnection, revision_id: Uuid) -> AppResult<usize> {
    let n = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM simulation_results WHERE revision_id = $1",
    )
    .bind(revision_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

/// Adds a result value to a simulation revision checked out by the actor.
///
/// # Invariants
///
/// * **Permission:** CAD editor in the project group, revision of a
///   simulation item (400 otherwise), in work and checked out by the actor
///   (403 / 409); a hidden revision is a 404.
/// * **Locks:** the simulation revision (`FOR UPDATE`).
/// * **Data:** at most 2000 result values per revision (400); the same name
///   and load case twice gets a 409.
/// * **Audit:** `simulation.result.add`.
/// * **Notifications:** none.
/// * **Webhooks:** `simulation.result_added`, emitted in the same transaction.
/// * **Atomic:** one transaction.
pub async fn add_result(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    input: ResultInput,
) -> AppResult<SimulationResult> {
    let input = input.validated()?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, revision_id).await?;
    if count_results(&mut tx, rev.id).await? >= MAX_RESULTS {
        return Err(AppError::BadRequest(format!(
            "A simulation revision can hold at most {MAX_RESULTS} result values."
        )));
    }
    let id = insert_result(&mut tx, actor, &rev, &input).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "simulation.result.add",
        "revision",
        Some(rev.id),
        format!(
            "{}: result {} = {} {}",
            revisions::label(&rev),
            input.name,
            input.value,
            input.unit
        ),
        json!({ "item_id": rev.item_id, "result_id": id }),
    )
    .await?;
    let status = domain::result_status(input.value, input.lower_limit, input.upper_limit);
    webhooks::emit(
        &mut tx,
        Some(actor),
        WebhookEvent::SimulationResultAdded,
        &[rev.project_id],
        json!({
            "revision": webhooks::revision_object(&rev, &rev.state),
            "result": {
                "id": id, "name": input.name, "value": input.value, "unit": input.unit,
                "lower_limit": input.lower_limit, "upper_limit": input.upper_limit,
                "load_case": input.load_case, "solver": input.solver,
                "solver_version": input.solver_version, "status": status,
            },
        }),
    )
    .await?;
    tx.commit().await?;
    get_result(pool, actor, id).await
}

/// Changes a result value of a simulation revision checked out by the actor.
///
/// # Invariants
///
/// * **Permission:** as [`add_result`]; a value the actor cannot see is a 404.
/// * **Locks:** the simulation revision (`FOR UPDATE`).
/// * **Data:** a hand edit clears the value's `source` (the origin of an
///   import or exchange file); the same name and load case as another value
///   gets a 409.
/// * **Audit:** `simulation.result.update` (with the previous value and unit).
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update_result(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: ResultInput,
) -> AppResult<SimulationResult> {
    let input = input.validated()?;
    let existing = get_result(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, existing.revision_id).await?;
    let updated = sqlx::query(
        "UPDATE simulation_results SET name = $2, value = $3, unit = $4, lower_limit = $5,
                upper_limit = $6, load_case = $7, solver = $8, solver_version = $9, note = $10,
                source = '', updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&input.name)
    .bind(input.value)
    .bind(&input.unit)
    .bind(input.lower_limit)
    .bind(input.upper_limit)
    .bind(&input.load_case)
    .bind(&input.solver)
    .bind(&input.solver_version)
    .bind(&input.note)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!(
                "{} already has a result '{}' for this load case.",
                revisions::label(&rev),
                input.name
            ),
        )
    })?
    .rows_affected();
    if updated == 0 {
        return Err(AppError::not_found("Result value not found."));
    }
    audit::record(
        &mut tx,
        Some(actor),
        "simulation.result.update",
        "revision",
        Some(rev.id),
        format!(
            "{}: result {} = {} {} (was {} {})",
            revisions::label(&rev),
            input.name,
            input.value,
            input.unit,
            existing.value,
            existing.unit
        ),
        json!({ "item_id": rev.item_id, "result_id": id }),
    )
    .await?;
    tx.commit().await?;
    get_result(pool, actor, id).await
}

/// Removes a result value from a simulation revision checked out by the actor.
///
/// # Invariants
///
/// * **Permission:** as [`add_result`]; a value the actor cannot see is a 404.
/// * **Locks:** the simulation revision (`FOR UPDATE`).
/// * **Audit:** `simulation.result.remove`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn remove_result(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<SimulationResult> {
    let existing = get_result(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, existing.revision_id).await?;
    let removed = sqlx::query("DELETE FROM simulation_results WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found("Result value not found."));
    }
    audit::record(
        &mut tx,
        Some(actor),
        "simulation.result.remove",
        "revision",
        Some(rev.id),
        format!(
            "{}: removed the result {}",
            revisions::label(&rev),
            existing.name
        ),
        json!({ "item_id": rev.item_id, "result_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing)
}

/// What an import of result values did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

/// Writes a set of result values in one transaction: values with the same
/// name and load case are updated, new ones appended. With `replace`, the
/// values that are not in the set are removed (the set becomes the result
/// table – what a solver script wants).
///
/// # Invariants
///
/// * **Permission:** as [`add_result`].
/// * **Locks:** the simulation revision (`FOR UPDATE`); the existing values are
///   read under that lock, so a value added concurrently is updated, not
///   inserted twice.
/// * **Data:** rows are validated first; a name and load case listed twice, or
///   more than 2000 values (in the set or in the result table afterwards), is
///   a 400 and nothing is written.
/// * **Audit:** one `simulation.result.import` for the whole set.
/// * **Notifications:** none.
/// * **Webhooks:** one `simulation.result_added`, only when a value was added
///   or updated.
/// * **Atomic:** one transaction.
pub async fn import_results(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    rows: Vec<ResultInput>,
    replace: bool,
) -> AppResult<ImportSummary> {
    if rows.len() > MAX_RESULTS {
        return Err(AppError::BadRequest(format!(
            "At most {MAX_RESULTS} result values can be imported at once."
        )));
    }
    let mut inputs: Vec<ResultInput> = Vec::with_capacity(rows.len());
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let row = row.validated()?;
        if !seen.insert((row.name.to_lowercase(), row.load_case.to_lowercase())) {
            return Err(AppError::BadRequest(format!(
                "The result '{}'{} is listed twice.",
                row.name,
                if row.load_case.is_empty() {
                    String::new()
                } else {
                    format!(" (load case '{}')", row.load_case)
                }
            )));
        }
        inputs.push(row);
    }
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, revision_id).await?;
    let summary = write_results(&mut tx, actor, &rev, &inputs, replace, &Origin::Import).await?;
    tx.commit().await?;
    Ok(summary)
}

/// Where a set of result values comes from (audit trail).
pub(crate) enum Origin {
    /// A table or the REST API.
    Import,
    /// The exchange file `teamcraft-results.json|csv` of a check-in.
    Exchange(String),
    /// Extraction templates of the project group.
    Templates(Vec<String>),
    /// A proposal from a report that a CAD editor accepted (file name).
    Proposal(String),
}

impl Origin {
    fn action(&self) -> &'static str {
        match self {
            Origin::Import => "simulation.result.import",
            Origin::Exchange(_) => "simulation.result.exchange",
            Origin::Templates(_) => "simulation.result.template",
            Origin::Proposal(_) => "simulation.proposal.accept",
        }
    }

    fn what(&self) -> String {
        match self {
            Origin::Import => "imported result values".into(),
            Origin::Exchange(file) => format!("result values from {file}"),
            Origin::Templates(files) => {
                format!("result values extracted from {}", files.join(", "))
            }
            Origin::Proposal(file) => format!("accepted a proposed result value from {file}"),
        }
    }

    fn details(&self) -> serde_json::Value {
        match self {
            Origin::Import => json!("import"),
            Origin::Exchange(file) => json!({ "exchange_file": file }),
            Origin::Templates(files) => json!({ "template_files": files }),
            Origin::Proposal(file) => json!({ "proposal_file": file }),
        }
    }
}

/// Writes validated, unique result values into a locked, editable simulation
/// revision inside the caller's transaction: same name and load case =
/// update, new = append; with `replace` the others are removed. One audit
/// entry (and webhook) for the whole set.
pub(crate) async fn write_results(
    tx: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    inputs: &[ResultInput],
    replace: bool,
    origin: &Origin,
) -> AppResult<ImportSummary> {
    // Read the current values under the revision lock: a value added
    // concurrently must be updated, not inserted a second time.
    #[derive(sqlx::FromRow)]
    struct Existing {
        id: Uuid,
        name: String,
        load_case: String,
    }
    let existing: HashMap<(String, String), Uuid> = sqlx::query_as::<_, Existing>(
        "SELECT id, name, load_case FROM simulation_results WHERE revision_id = $1",
    )
    .bind(rev.id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| ((r.name.to_lowercase(), r.load_case.to_lowercase()), r.id))
    .collect();
    let mut summary = ImportSummary::default();
    if replace {
        let keep: Vec<Uuid> = inputs
            .iter()
            .filter_map(|i| {
                existing
                    .get(&(i.name.to_lowercase(), i.load_case.to_lowercase()))
                    .copied()
            })
            .collect();
        summary.removed = sqlx::query(
            "DELETE FROM simulation_results WHERE revision_id = $1 AND NOT (id = ANY($2))",
        )
        .bind(rev.id)
        .bind(&keep)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;
    }
    for input in inputs {
        let key = (input.name.to_lowercase(), input.load_case.to_lowercase());
        match existing.get(&key) {
            Some(id) => {
                sqlx::query(
                    "UPDATE simulation_results SET name = $2, value = $3, unit = $4, lower_limit = $5,
                            upper_limit = $6, load_case = $7, solver = $8, solver_version = $9,
                            note = $10, source = $11, updated_at = now()
                     WHERE id = $1",
                )
                .bind(id)
                .bind(&input.name)
                .bind(input.value)
                .bind(&input.unit)
                .bind(input.lower_limit)
                .bind(input.upper_limit)
                .bind(&input.load_case)
                .bind(&input.solver)
                .bind(&input.solver_version)
                .bind(&input.note)
                .bind(&input.source)
                .execute(&mut *tx)
                .await?;
                summary.updated += 1;
            }
            None => {
                insert_result(tx, actor, rev, input).await?;
                summary.added += 1;
            }
        }
    }
    if count_results(tx, rev.id).await? > MAX_RESULTS {
        return Err(AppError::BadRequest(format!(
            "A simulation revision can hold at most {MAX_RESULTS} result values."
        )));
    }
    audit::record(
        tx,
        Some(actor),
        origin.action(),
        "revision",
        Some(rev.id),
        format!(
            "{}: {} ({} added, {} updated, {} removed)",
            revisions::label(rev),
            origin.what(),
            summary.added,
            summary.updated,
            summary.removed
        ),
        json!({ "item_id": rev.item_id, "added": summary.added, "updated": summary.updated,
                "removed": summary.removed, "replace": replace, "origin": origin.details() }),
    )
    .await?;
    if summary.added + summary.updated > 0 {
        webhooks::emit(
            tx,
            Some(actor),
            WebhookEvent::SimulationResultAdded,
            &[rev.project_id],
            json!({
                "revision": webhooks::revision_object(rev, &rev.state),
                "import": { "added": summary.added, "updated": summary.updated,
                            "removed": summary.removed, "replace": replace },
            }),
        )
        .await?;
    }
    Ok(summary)
}

/// Result values from the files stored with a simulation revision: the
/// exchange file `teamcraft-results.json|csv` (at most 1 MB, schema in
/// [`crate::resultscan::parse_exchange`]) and the extraction templates of the
/// project group applied to the small text files. Runs inside the
/// transaction of the upload or check-in: a malformed exchange file refuses
/// it as a whole, nothing is half-written. Other item types: nothing.
pub(crate) async fn apply_file_values(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    uploads: &[crate::services::files::Upload],
) -> AppResult<()> {
    use crate::{resultscan, services::result_templates};
    if rev.item_type != ItemType::Simulation.as_str() {
        return Ok(());
    }
    let named =
        |u: &crate::services::files::Upload| crate::services::files::stored_name(&u.filename);
    let exchange: Vec<_> = uploads
        .iter()
        .filter(|u| resultscan::is_exchange_name(&named(u)))
        .collect();
    if exchange.len() > 1 {
        return Err(AppError::BadRequest(format!(
            "Check in either {} or {}, not both.",
            resultscan::EXCHANGE_JSON,
            resultscan::EXCHANGE_CSV
        )));
    }
    // templates of the group: small text files, never the exchange file
    let mut found: Vec<ResultInput> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if let Some(project_id) = rev.project_id {
        let templates = result_templates::enabled_for(conn, project_id).await?;
        if !templates.is_empty() {
            for upload in uploads.iter().filter(|u| !u.scan_head.is_empty()) {
                let name = named(upload);
                if resultscan::is_exchange_name(&name) {
                    continue;
                }
                let values = resultscan::extract_with_tail(
                    &name,
                    &upload.scan_head,
                    &upload.scan_tail,
                    &templates,
                );
                if !values.is_empty() {
                    files.push(name);
                }
                found.extend(values);
            }
        }
    }
    if !found.is_empty() {
        let rows = unique_rows(found)?;
        write_results(conn, actor, rev, &rows, false, &Origin::Templates(files)).await?;
    }
    if let Some(upload) = exchange.first() {
        let name = named(upload);
        if upload.blob.size > resultscan::EXCHANGE_MAX_BYTES {
            return Err(AppError::BadRequest(format!(
                "{name} is larger than {} MB – the exchange file holds key numbers, not result fields.",
                resultscan::EXCHANGE_MAX_BYTES / (1024 * 1024)
            )));
        }
        let rows =
            resultscan::parse_exchange(&name, &upload.scan_head).map_err(AppError::BadRequest)?;
        let rows: Vec<ResultInput> = rows
            .into_iter()
            .enumerate()
            .map(|(i, mut r)| {
                r.source = format!("{name}:{}", i + 1);
                r
            })
            .collect();
        let rows = unique_rows_strict(rows, &name)?;
        write_results(conn, actor, rev, &rows, false, &Origin::Exchange(name)).await?;
    }
    Ok(())
}

/// One value per name and load case; a later one replaces an earlier one
/// (two templates, or the same template in two files).
fn unique_rows(rows: Vec<ResultInput>) -> AppResult<Vec<ResultInput>> {
    let mut out: Vec<ResultInput> = Vec::new();
    for row in rows {
        let row = row.validated()?;
        let key = (row.name.to_lowercase(), row.load_case.to_lowercase());
        match out
            .iter_mut()
            .find(|r| (r.name.to_lowercase(), r.load_case.to_lowercase()) == key)
        {
            Some(existing) => *existing = row,
            None => out.push(row),
        }
    }
    Ok(out)
}

/// Like an import: a result listed twice in the exchange file is an error.
fn unique_rows_strict(rows: Vec<ResultInput>, filename: &str) -> AppResult<Vec<ResultInput>> {
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        if !seen.insert((row.name.to_lowercase(), row.load_case.to_lowercase())) {
            return Err(AppError::BadRequest(format!(
                "{filename}: the result '{}'{} is listed twice.",
                row.name,
                if row.load_case.is_empty() {
                    String::new()
                } else {
                    format!(" (load case '{}')", row.load_case)
                }
            )));
        }
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Result tables (CSV / Excel)
// ---------------------------------------------------------------------------

/// The column a header names (case, spaces and punctuation ignored).
fn result_field(header: &str) -> Option<&'static str> {
    let h: String = header
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    Some(match h.as_str() {
        "name" | "result" | "quantity" | "parameter" | "kennwert" | "grosse" | "größe" => "name",
        "value" | "wert" | "resultvalue" => "value",
        "unit" | "units" | "einheit" => "unit",
        "limitlow" | "limitmin" | "lowerlimit" | "lower" | "min" | "minimum" | "lowlimit"
        | "untergrenze" => "lower_limit",
        "limithigh" | "limitmax" | "upperlimit" | "upper" | "max" | "maximum" | "highlimit"
        | "limit" | "obergrenze" => "upper_limit",
        "loadcase" | "case" | "lastfall" | "subcase" | "step" => "load_case",
        "solver" | "löser" | "loser" => "solver",
        "solverversion" | "version" => "solver_version",
        "note" | "notes" | "comment" | "bemerkung" => "note",
        _ => return None,
    })
}

/// A number in a result table: `2.5e8`, `-0,35`, `1.234,5`, `125 MPa`.
pub(crate) fn table_number(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(v) = s.parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    if let Ok(v) = s.replace(',', ".").parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    // the number in front of a unit (`125 MPa`), without exponents
    let first = s.split_whitespace().next().unwrap_or(s);
    first
        .parse::<f64>()
        .ok()
        .or_else(|| first.replace(',', ".").parse::<f64>().ok())
        .or_else(|| bomtable::parse_number(s))
        .filter(|v| v.is_finite())
}

/// Reads result values from a CSV/TSV or spreadsheet table with the header
/// `name,value,unit,limit_low,limit_high,load_case` (further columns:
/// `solver`, `solver_version`, `note`; German names and `min`/`max` work
/// too). Rows without a name are skipped; a value that is not a number is
/// an error naming the row.
pub fn parse_results_table(filename: &str, bytes: &[u8]) -> Result<Vec<ResultInput>, String> {
    let grid = bomtable::read_grid(filename, bytes)?;
    let header_at = grid
        .iter()
        .take(30)
        .position(|row| {
            let fields: Vec<_> = row.iter().filter_map(|c| result_field(c)).collect();
            fields.contains(&"name") && fields.contains(&"value")
        })
        .ok_or_else(|| {
            format!(
                "{filename}: no header row with the columns name and value \
                 (expected: name,value,unit,limit_low,limit_high,load_case)."
            )
        })?;
    let mut columns: HashMap<&'static str, usize> = HashMap::new();
    for (i, cell) in grid[header_at].iter().enumerate() {
        if let Some(field) = result_field(cell) {
            columns.entry(field).or_insert(i);
        }
    }
    let cell = |row: &[String], field: &str| -> String {
        columns
            .get(field)
            .and_then(|i| row.get(*i))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (index, row) in grid.iter().enumerate().skip(header_at + 1) {
        let line = index + 1;
        let name = cell(row, "name");
        if name.is_empty() {
            continue;
        }
        let number = |field: &str, label: &str| -> Result<Option<f64>, String> {
            let raw = cell(row, field);
            if raw.is_empty() {
                return Ok(None);
            }
            table_number(&raw)
                .map(Some)
                .ok_or_else(|| format!("Row {line}: {label} '{raw}' of '{name}' is not a number."))
        };
        let value = number("value", "the value")?
            .ok_or_else(|| format!("Row {line}: '{name}' has no value."))?;
        out.push(ResultInput {
            value,
            unit: cell(row, "unit"),
            lower_limit: number("lower_limit", "the lower limit")?,
            upper_limit: number("upper_limit", "the upper limit")?,
            load_case: cell(row, "load_case"),
            solver: cell(row, "solver"),
            solver_version: cell(row, "solver_version"),
            note: cell(row, "note"),
            source: String::new(),
            name,
        });
        if out.len() > MAX_RESULTS {
            return Err(format!(
                "{filename} has more than {MAX_RESULTS} result values."
            ));
        }
    }
    if out.is_empty() {
        return Err(format!("{filename} contains no result values."));
    }
    Ok(out)
}

/// The result values as CSV, in the import layout.
pub fn to_csv(results: &[SimulationResult]) -> String {
    use crate::util::csv_field;
    let num = |v: Option<f64>| v.map(|v| v.to_string()).unwrap_or_default();
    let mut out = String::from(
        "name,value,unit,limit_low,limit_high,load_case,solver,solver_version,note,status\n",
    );
    for r in results {
        let fields = [
            csv_field(&r.name),
            r.value.to_string(),
            csv_field(&r.unit),
            num(r.lower_limit),
            num(r.upper_limit),
            csv_field(&r.load_case),
            csv_field(&r.solver),
            csv_field(&r.solver_version),
            csv_field(&r.note),
            r.status.map_or("none", ResultStatus::as_str).to_string(),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Comparison between two revisions
// ---------------------------------------------------------------------------

/// One result value in either revision.
#[derive(Clone, Debug, Serialize)]
pub struct ResultDiff {
    pub change: Change,
    pub name: String,
    pub load_case: String,
    pub unit: String,
    pub before: Option<SimulationResult>,
    pub after: Option<SimulationResult>,
    /// `after − before` and the relative change in percent.
    pub delta: Option<f64>,
    pub delta_percent: Option<f64>,
}

/// Matches the values of two revisions by name and load case (pure).
pub fn compare_results(before: &[SimulationResult], after: &[SimulationResult]) -> Vec<ResultDiff> {
    let mut out = Vec::new();
    let old: HashMap<(String, String), &SimulationResult> =
        before.iter().map(|r| (r.key(), r)).collect();
    let mut matched = std::collections::HashSet::new();
    for a in after {
        let key = a.key();
        match old.get(&key) {
            Some(b) => {
                matched.insert(key);
                let same = b.value == a.value
                    && b.unit == a.unit
                    && b.lower_limit == a.lower_limit
                    && b.upper_limit == a.upper_limit;
                let delta = a.value - b.value;
                out.push(ResultDiff {
                    change: if same {
                        Change::Unchanged
                    } else {
                        Change::Changed
                    },
                    name: a.name.clone(),
                    load_case: a.load_case.clone(),
                    unit: a.unit.clone(),
                    delta: Some(delta),
                    delta_percent: (b.value != 0.0 && b.unit == a.unit)
                        .then(|| delta / b.value.abs() * 100.0),
                    before: Some((*b).clone()),
                    after: Some(a.clone()),
                });
            }
            None => out.push(ResultDiff {
                change: Change::Added,
                name: a.name.clone(),
                load_case: a.load_case.clone(),
                unit: a.unit.clone(),
                before: None,
                after: Some(a.clone()),
                delta: None,
                delta_percent: None,
            }),
        }
    }
    for b in before {
        if !matched.contains(&b.key()) {
            out.push(ResultDiff {
                change: Change::Removed,
                name: b.name.clone(),
                load_case: b.load_case.clone(),
                unit: b.unit.clone(),
                before: Some(b.clone()),
                after: None,
                delta: None,
                delta_percent: None,
            });
        }
    }
    out
}

/// Counts of a comparison.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CompareSummary {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
}

pub fn compare_summary(diffs: &[ResultDiff]) -> CompareSummary {
    let mut s = CompareSummary::default();
    for d in diffs {
        match d.change {
            Change::Added => s.added += 1,
            Change::Removed => s.removed += 1,
            Change::Changed => s.changed += 1,
            Change::Unchanged => s.unchanged += 1,
        }
    }
    s
}

/// Compares the result values of two revisions of the same simulation.
pub async fn compare(
    pool: &PgPool,
    before: &Revision,
    after: &Revision,
) -> AppResult<Vec<ResultDiff>> {
    if before.item_id != after.item_id {
        return Err(AppError::BadRequest(
            "Only revisions of the same simulation can be compared.".into(),
        ));
    }
    let (b, a) = tokio::try_join!(
        results_unchecked(pool, before.id),
        results_unchecked(pool, after.id)
    )?;
    Ok(compare_results(&b, &a))
}

/// Largest picture shown on the simulation page (the browser loads each one).
pub const GALLERY_MAX_BYTES: i64 = 8 * 1024 * 1024;

/// Pictures shown on the simulation page at most.
pub const GALLERY_MAX_IMAGES: usize = 12;

/// Whether a file of a simulation revision is shown as a picture in the
/// gallery: PNG, JPEG or WebP (by file name) of at most [`GALLERY_MAX_BYTES`].
/// Rendered result pictures are the cheap way to show a CFD or FEA result –
/// nothing is parsed, the browser draws the file.
pub fn is_gallery_image(filename: &str, size_bytes: i64) -> bool {
    let lower = filename.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    matches!(ext, "png" | "jpg" | "jpeg" | "webp")
        && size_bytes > 0
        && size_bytes <= GALLERY_MAX_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gallery_takes_small_pictures_only() {
        assert!(is_gallery_image("pressure.PNG", 1000));
        assert!(is_gallery_image("v.v2.jpeg", GALLERY_MAX_BYTES));
        assert!(is_gallery_image("streamlines.webp", 1));
        assert!(!is_gallery_image("huge.png", GALLERY_MAX_BYTES + 1));
        assert!(!is_gallery_image("empty.png", 0));
        assert!(!is_gallery_image("logo.svg", 10), "SVG can carry scripts");
        assert!(!is_gallery_image("result.vtu", 10));
        assert!(!is_gallery_image("png", 10));
    }

    fn result(name: &str, load_case: &str, value: f64, upper: Option<f64>) -> SimulationResult {
        SimulationResult {
            id: Uuid::new_v4(),
            revision_id: Uuid::nil(),
            position: 0,
            name: name.into(),
            value,
            unit: "MPa".into(),
            lower_limit: None,
            upper_limit: upper,
            load_case: load_case.into(),
            solver: String::new(),
            solver_version: String::new(),
            note: String::new(),
            source: String::new(),
            updated_at: chrono::Utc::now(),
            status: None,
        }
        .with_status()
    }

    #[test]
    fn csv_tables_of_results() {
        let csv = b"name,value,unit,limit_low,limit_high,load_case\n\
                    Max. von Mises stress,182.5,MPa,,250,LC1 pressure\n\
                    Safety factor,1.37,,1.5,,LC1 pressure\n\
                    First eigenfrequency,2.35e2,Hz,200,,modal\n\
                    ,,,,,\n";
        let rows = parse_results_table("results.csv", csv).unwrap();
        assert_eq!(rows.len(), 3, "empty rows are skipped");
        assert_eq!(rows[0].name, "Max. von Mises stress");
        assert_eq!(rows[0].value, 182.5);
        assert_eq!(rows[0].upper_limit, Some(250.0));
        assert_eq!(rows[0].lower_limit, None);
        assert_eq!(rows[0].load_case, "LC1 pressure");
        assert_eq!(rows[1].lower_limit, Some(1.5));
        assert_eq!(rows[2].value, 235.0, "exponent notation");
        let statuses: Vec<_> = rows
            .iter()
            .map(|r| domain::result_status(r.value, r.lower_limit, r.upper_limit))
            .collect();
        assert_eq!(
            statuses,
            [ResultStatus::Pass, ResultStatus::Fail, ResultStatus::Pass]
        );

        // German Excel: semicolons, decimal commas, German headers, a title row
        let german =
            "Ergebnisse Halter\nKennwert;Wert;Einheit;Untergrenze;Obergrenze;Lastfall;Löser\n\
                      Verschiebung;0,35;mm;;0,5;LF1;CalculiX\n";
        let rows = parse_results_table("ergebnisse.csv", german.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, 0.35);
        assert_eq!(rows[0].upper_limit, Some(0.5));
        assert_eq!(rows[0].solver, "CalculiX");

        // min/max and extra columns
        let rows = parse_results_table(
            "r.tsv",
            b"Result\tValue\tUnit\tMin\tMax\tSolver\tSolver version\tNote\n\
              dp\t1250 Pa\tPa\t\t1500\tOpenFOAM\tv2312\tsimpleFoam\n",
        )
        .unwrap();
        assert_eq!(rows[0].value, 1250.0, "unit after the number");
        assert_eq!(rows[0].solver_version, "v2312");
        assert_eq!(rows[0].note, "simpleFoam");

        let err = parse_results_table("r.csv", b"name,value\nstress,high\n").unwrap_err();
        assert!(err.contains("Row 2") && err.contains("high"), "{err}");
        let err = parse_results_table("r.csv", b"name,value\nstress,\n").unwrap_err();
        assert!(err.contains("no value"), "{err}");
        let err = parse_results_table("r.csv", b"a,b\n1,2\n").unwrap_err();
        assert!(err.contains("no header row"), "{err}");
        assert!(parse_results_table("r.csv", b"name,value\n").is_err());
    }

    #[test]
    fn input_validation() {
        let ok = ResultInput {
            name: " Stress ".into(),
            value: 1.0,
            lower_limit: Some(0.0),
            upper_limit: Some(2.0),
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert_eq!(ok.name, "Stress");
        assert!(ResultInput {
            name: "".into(),
            value: 1.0,
            ..Default::default()
        }
        .validated()
        .is_err());
        assert!(ResultInput {
            name: "x".into(),
            value: 1.0,
            lower_limit: Some(3.0),
            upper_limit: Some(2.0),
            ..Default::default()
        }
        .validated()
        .is_err());
        // JSON of solver scripts: limit_low / limit_high are accepted
        let input: ResultInput = serde_json::from_value(
            serde_json::json!({ "name": "dp", "value": 3.5, "limit_high": 4.0 }),
        )
        .unwrap();
        assert_eq!(input.upper_limit, Some(4.0));
    }

    #[test]
    fn comparison_and_csv_export() {
        let before = vec![
            result("Stress", "LC1", 200.0, Some(250.0)),
            result("Stress", "LC2", 150.0, Some(250.0)),
            result("Mass", "", 1.0, None),
        ];
        let after = vec![
            result("stress", "lc1", 180.0, Some(250.0)),
            result("Stress", "LC2", 150.0, Some(250.0)),
            result("Frequency", "modal", 240.0, None),
        ];
        let diffs = compare_results(&before, &after);
        let s = compare_summary(&diffs);
        assert_eq!((s.added, s.removed, s.changed, s.unchanged), (1, 1, 1, 1));
        let changed = diffs.iter().find(|d| d.change == Change::Changed).unwrap();
        assert_eq!(changed.delta, Some(-20.0));
        assert_eq!(changed.delta_percent, Some(-10.0));
        assert_eq!(
            diffs
                .iter()
                .find(|d| d.change == Change::Removed)
                .unwrap()
                .name,
            "Mass"
        );

        let summary = summarize(&before);
        assert_eq!(
            (summary.pass, summary.fail, summary.without_limits),
            (2, 0, 1)
        );
        let failing = vec![result("Stress, max", "LC1", 300.0, Some(250.0))];
        assert_eq!(summarize(&failing).fail, 1);
        let csv = to_csv(&failing);
        assert!(csv.starts_with("name,value,unit,limit_low,limit_high,load_case"));
        assert!(
            csv.contains("\"Stress, max\",300,MPa,,250,LC1,,,,fail"),
            "{csv}"
        );
        // the export can be imported again
        let rows = parse_results_table("export.csv", csv.as_bytes()).unwrap();
        assert_eq!(rows[0].name, "Stress, max");
        assert_eq!(rows[0].upper_limit, Some(250.0));
    }

    fn basis() -> SimulationBasis {
        SimulationBasis {
            id: Uuid::nil(),
            revision_id: Uuid::nil(),
            role: "geometry".into(),
            note: String::new(),
            basis_item_id: Uuid::nil(),
            basis_number: "PRT-000001".into(),
            basis_type: "part".into(),
            basis_revision_id: Uuid::nil(),
            basis_code: "A".into(),
            basis_seq: 1,
            basis_state: "released".into(),
            basis_name: "Bracket".into(),
            file_version_id: None,
            file_id: None,
            file_name: String::new(),
            file_version: None,
            file_current_version: None,
            released_revision_id: None,
            released_code: Some("A".into()),
            released_seq: Some(1),
            created_at: chrono::Utc::now(),
            out_of_date: None,
            basis_project_id: None,
            restricted: false,
        }
    }

    #[test]
    fn out_of_date_rules() {
        assert_eq!(out_of_date_reason(&basis()), None);
        let newer = SimulationBasis {
            released_code: Some("B".into()),
            released_seq: Some(2),
            ..basis()
        };
        assert_eq!(
            out_of_date_reason(&newer).as_deref(),
            Some("PRT-000001/B was released (simulated: A)")
        );
        let obsolete = SimulationBasis {
            basis_state: "obsolete".into(),
            ..basis()
        };
        assert!(out_of_date_reason(&obsolete).unwrap().contains("obsolete"));
        let pinned = SimulationBasis {
            basis_state: "in_work".into(),
            released_code: None,
            released_seq: None,
            file_version_id: Some(Uuid::nil()),
            file_name: "bracket.step".into(),
            file_version: Some(2),
            file_current_version: Some(2),
            ..basis()
        };
        assert_eq!(out_of_date_reason(&pinned), None);
        let newer_file = SimulationBasis {
            file_current_version: Some(3),
            ..pinned.clone()
        };
        assert_eq!(
            out_of_date_reason(&newer_file).as_deref(),
            Some("bracket.step has version 3 (simulated: v2)")
        );
        let deleted = SimulationBasis {
            file_version_id: None,
            file_current_version: None,
            ..pinned
        };
        assert!(out_of_date_reason(&deleted).unwrap().contains("deleted"));
    }
}
