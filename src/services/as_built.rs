//! As-built records: hardware as it was really built.
//!
//! An **as-built** item (type `as_built`, numbers `AB-…`) is one physical
//! unit or lot. It has the usual lifecycle, revisions, files (photos,
//! measurement reports, a 3D scan, the traveller) and change orders. A new
//! revision of the as-built item is a new state of the same unit (rework,
//! repair, retrofit); the old revision stays for traceability. On top of
//! that an as-built revision has
//!
//! * a **build record** – the design revision (part, assembly or software)
//!   the unit was built to, serial or lot number, build date and note. The
//!   serial number is unique per design item;
//! * **deviations** – how the unit departs from its design (kind, feature,
//!   as designed, as built) and the **disposition** (open, use as is,
//!   rework, repair, scrap);
//! * its **BOM** – the regular BOM of the revision, holding what was really
//!   installed. [`record_unit`] starts it as a copy of the design's BOM; the
//!   BOM comparison between the design and the as-built revision shows the
//!   differences.
//!
//! Record and deviations belong to the revision like BOM lines: changed only
//! while it is checked out, copied by Revise, frozen at release. A change
//! order with an as-built revision cannot be submitted while that revision
//! has no build record or an open deviation ([`ensure_ready`]).

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, Classification, DeviationKind, Disposition, ItemType},
    error::{AppError, AppResult},
    models::Revision,
    services::{access, audit, items, revisions},
};

/// At most this many deviations per as-built revision.
pub const MAX_DEVIATIONS: usize = 1000;

/// At most this many built units are listed on a design item.
pub const MAX_UNITS_LISTED: i64 = 500;

// ---------------------------------------------------------------------------
// Build record
// ---------------------------------------------------------------------------

/// What an as-built revision was built to.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct BuildRecord {
    /// The as-built revision.
    pub revision_id: Uuid,
    pub design_item_id: Uuid,
    pub design_number: String,
    pub design_type: String,
    pub design_revision_id: Uuid,
    pub design_code: String,
    #[serde(skip)]
    pub design_seq: i32,
    pub design_state: String,
    pub design_name: String,
    pub serial_number: String,
    pub built_on: Option<NaiveDate>,
    pub note: String,
    pub updated_by_name: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Newest released revision of the design item.
    pub released_code: Option<String>,
    #[serde(skip)]
    pub released_seq: Option<i32>,
    /// Set when a newer design revision was released after the one the unit
    /// was built to (e.g. "PRT-000012/C is released (built to: B)").
    #[sqlx(default)]
    pub newer_design: Option<String>,
    /// Project group of the design item (for the read check; never sent).
    #[serde(skip)]
    pub design_project_id: Option<Uuid>,
    /// The reader may not see the design: its identity is removed.
    #[sqlx(default)]
    pub restricted: bool,
}

impl BuildRecord {
    fn with_status(mut self) -> Self {
        self.newer_design = newer_design(
            &self.design_number,
            &self.design_code,
            self.design_seq,
            self.released_code.as_deref(),
            self.released_seq,
        );
        self
    }

    fn redact(mut self, actor: &Actor) -> Self {
        if !actor.can_see_project(self.design_project_id) {
            self.design_item_id = Uuid::nil();
            self.design_number = access::RESTRICTED_LABEL.into();
            self.design_type = String::new();
            self.design_revision_id = Uuid::nil();
            self.design_code = String::new();
            self.design_state = String::new();
            self.design_name = String::new();
            self.released_code = None;
            self.released_seq = None;
            self.newer_design = None;
            self.design_project_id = None;
            self.restricted = true;
        }
        self
    }
}

/// "PRT-1/C is released (built to: B)" when a design revision newer than
/// the one the unit was built to is released (pure).
pub fn newer_design(
    number: &str,
    built_code: &str,
    built_seq: i32,
    released_code: Option<&str>,
    released_seq: Option<i32>,
) -> Option<String> {
    match (released_code, released_seq) {
        (Some(code), Some(seq)) if seq > built_seq => Some(format!(
            "{number}/{code} is released (built to: {built_code})"
        )),
        _ => None,
    }
}

const RECORD_SELECT: &str = "
    SELECT rec.revision_id, di.id AS design_item_id, di.number AS design_number,
           di.item_type AS design_type, dr.id AS design_revision_id, dr.code AS design_code,
           dr.seq AS design_seq, dr.state AS design_state, dr.name AS design_name,
           rec.serial_number, rec.built_on, rec.note, u.display_name AS updated_by_name,
           rec.updated_at, lr.code AS released_code, lr.seq AS released_seq,
           di.project_id AS design_project_id
    FROM as_built_records rec
    JOIN revisions dr ON dr.id = rec.design_revision_id
    JOIN items di ON di.id = dr.item_id
    JOIN users u ON u.id = rec.updated_by
    LEFT JOIN LATERAL (
        SELECT r.code, r.seq FROM revisions r
        WHERE r.item_id = di.id AND r.released_at IS NOT NULL
        ORDER BY r.seq DESC LIMIT 1
    ) lr ON TRUE";

/// The build record of an as-built revision the actor may read (`None`
/// while none was entered).
pub async fn record(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Option<BuildRecord>> {
    revisions::get(pool, actor, revision_id).await?;
    Ok(
        sqlx::query_as::<_, BuildRecord>(&format!("{RECORD_SELECT} WHERE rec.revision_id = $1"))
            .bind(revision_id)
            .fetch_optional(pool)
            .await?
            .map(|r| r.with_status().redact(actor)),
    )
}

/// Fails unless the revision belongs to an as-built item.
pub(crate) fn ensure_as_built(rev: &Revision) -> AppResult<()> {
    if rev.item_type == ItemType::AsBuilt.as_str() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "{} is not an as-built item; build records and deviations belong to items of type As-built.",
            revisions::label(rev)
        )))
    }
}

/// Locks an as-built revision the actor may edit.
async fn lock_editable(
    conn: &mut PgConnection,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Revision> {
    let rev = revisions::lock(conn, revision_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    ensure_as_built(&rev)?;
    revisions::ensure_editable(&rev, actor)?;
    Ok(rev)
}

/// Parses an optional build date (`YYYY-MM-DD`; empty = none).
pub fn parse_built_on(raw: Option<&str>) -> AppResult<Option<NaiveDate>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| {
                AppError::BadRequest(format!("Invalid build date '{s}': use YYYY-MM-DD."))
            }),
    }
}

/// The build record as entered.
#[derive(Clone, Debug, Default)]
pub struct RecordInput {
    /// Item number of the design.
    pub design_number: String,
    /// Revision code of the design; empty = its latest revision.
    pub design_revision: String,
    pub serial_number: String,
    pub built_on: Option<NaiveDate>,
    pub note: String,
}

struct Facts {
    serial_number: String,
    note: String,
}

fn validate_facts(serial_number: &str, note: &str) -> AppResult<Facts> {
    let serial_number = domain::limit_text("Serial number", serial_number.trim(), 100)?;
    if serial_number.chars().any(char::is_control) {
        return Err(AppError::BadRequest(
            "The serial number must be a single line.".into(),
        ));
    }
    Ok(Facts {
        serial_number,
        note: domain::limit_text("Note", note, 2000)?,
    })
}

/// Resolves the design revision a unit is built to (read-checked).
async fn design_revision(
    pool: &PgPool,
    actor: &Actor,
    number: &str,
    code: &str,
) -> AppResult<Revision> {
    if number.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Enter the item number of the design the unit was built to.".into(),
        ));
    }
    let item = items::get_by_number(pool, actor, number).await?;
    let code = code.trim();
    if code.is_empty() || code.eq_ignore_ascii_case("latest") {
        items::latest_revision(pool, actor, item.id).await
    } else {
        revisions::get_by_code(pool, actor, item.id, code).await
    }
}

/// Writes the build record of the locked as-built revision `rev` inside the
/// caller's transaction: type checks, the unique serial number per design
/// item, upsert and audit entry.
async fn write_record(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    design: &Revision,
    facts: &Facts,
    built_on: Option<NaiveDate>,
) -> AppResult<()> {
    let design_type = ItemType::parse(&design.item_type);
    if !design_type.is_some_and(ItemType::can_be_built_to) {
        return Err(AppError::BadRequest(format!(
            "{} is {}; a unit is built to a part, an assembly or software.",
            revisions::label(design),
            design_type.map_or("of an unknown type", |t| match t {
                ItemType::AsBuilt => "an as-built record itself",
                ItemType::Simulation => "a simulation",
                _ => "a document",
            })
        )));
    }
    // Serialise serial-number checks per design item (and hold it against
    // a concurrent delete, which locks it FOR UPDATE and then checks
    // `item_is_design`).
    let exists = sqlx::query_scalar::<_, Uuid>("SELECT id FROM items WHERE id = $1 FOR UPDATE")
        .bind(design.item_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some();
    if !exists {
        return Err(access::not_found_item());
    }
    if !facts.serial_number.is_empty() {
        let holder = sqlx::query_as::<_, (String, Option<Uuid>)>(
            "SELECT ai.number, ai.project_id FROM as_built_records rec
             JOIN revisions ar ON ar.id = rec.revision_id
             JOIN items ai ON ai.id = ar.item_id
             WHERE rec.design_item_id = $1 AND lower(rec.serial_number) = lower($2)
               AND ar.item_id <> $3
             LIMIT 1",
        )
        .bind(design.item_id)
        .bind(&facts.serial_number)
        .bind(rev.item_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((number, project)) = holder {
            let who = if actor.can_see_project(project) {
                number
            } else {
                "another unit".into()
            };
            return Err(AppError::Conflict(format!(
                "Serial number {} of {} is already recorded by {who}.",
                facts.serial_number, design.item_number
            )));
        }
    }
    let previous = sqlx::query_scalar::<_, Uuid>(
        "SELECT design_revision_id FROM as_built_records WHERE revision_id = $1",
    )
    .bind(rev.id)
    .fetch_optional(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO as_built_records (revision_id, design_revision_id, design_item_id,
                                       serial_number, built_on, note, updated_by, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())
         ON CONFLICT (revision_id) DO UPDATE SET
             design_revision_id = EXCLUDED.design_revision_id,
             design_item_id = EXCLUDED.design_item_id,
             serial_number = EXCLUDED.serial_number, built_on = EXCLUDED.built_on,
             note = EXCLUDED.note, updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(rev.id)
    .bind(design.id)
    .bind(design.item_id)
    .bind(&facts.serial_number)
    .bind(built_on)
    .bind(&facts.note)
    .bind(actor.id)
    .execute(&mut *conn)
    .await?;
    let serial = if facts.serial_number.is_empty() {
        String::new()
    } else {
        format!(", serial {}", facts.serial_number)
    };
    audit::record_related(
        conn,
        Some(actor),
        if previous.is_some() {
            "as_built.record.update"
        } else {
            "as_built.record.set"
        },
        "revision",
        Some(rev.id),
        Some(design.item_id),
        format!(
            "{} built to {}{serial}",
            revisions::label(rev),
            revisions::label(design)
        ),
        json!({ "item_id": rev.item_id, "design_revision_id": design.id,
                "previous_design_revision_id": previous,
                "serial_number": facts.serial_number, "built_on": built_on }),
    )
    .await?;
    Ok(())
}

/// Sets the build record of an as-built revision checked out by the actor.
pub async fn set_record(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    input: RecordInput,
) -> AppResult<BuildRecord> {
    let facts = validate_facts(&input.serial_number, &input.note)?;
    let design = design_revision(pool, actor, &input.design_number, &input.design_revision).await?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, revision_id).await?;
    write_record(&mut tx, actor, &rev, &design, &facts, input.built_on).await?;
    tx.commit().await?;
    record(pool, actor, revision_id)
        .await?
        .ok_or_else(|| AppError::not_found("Build record not found."))
}

/// A new physical unit built to a design revision ([`record_unit`]).
#[derive(Clone, Debug, Default)]
pub struct NewUnit {
    /// Item number of the as-built item; empty = generated (`AB-…`).
    pub number: Option<String>,
    /// Name of the as-built item; empty = "<design name> – SN <serial>".
    pub name: String,
    pub serial_number: String,
    pub built_on: Option<NaiveDate>,
    pub note: String,
    /// Project group of the as-built item; `None` = the design's group.
    pub project_id: Option<Uuid>,
    /// Start the as-built BOM as a copy of the design's BOM.
    pub with_bom: bool,
}

/// Records a physical unit built to the design revision `design_revision_id`:
/// creates the as-built item (revision A, checked out by the actor) with its
/// build record and, optionally, the design's BOM – in one transaction.
pub async fn record_unit(
    pool: &PgPool,
    actor: &Actor,
    design_revision_id: Uuid,
    input: NewUnit,
) -> AppResult<Revision> {
    let facts = validate_facts(&input.serial_number, &input.note)?;
    let design = revisions::get(pool, actor, design_revision_id).await?;
    let name = match input.name.trim() {
        "" if facts.serial_number.is_empty() => format!("{} – as built", design.name),
        "" => format!("{} – SN {}", design.name, facts.serial_number),
        name => name.to_string(),
    };
    let target = input.project_id.or(design.project_id);
    let mut tx = pool.begin().await?;
    // The copied BOM takes the design's structure along: like Save As, out
    // of a restricted group only for its managers.
    if let Some(source) = design.project_id {
        if target != Some(source) && input.with_bom {
            items::ensure_may_take_out(&mut tx, actor, source, "Copying").await?;
        }
    }
    // creation checks the role and the membership in the target group
    let created = items::create_tx(
        &mut tx,
        actor,
        items::NewItem {
            number: input.number.filter(|n| !n.trim().is_empty()),
            item_type: ItemType::AsBuilt,
            unit: design.unit.clone(),
            project_id: target,
            name,
            description: String::new(),
            material: design.material.clone(),
            mass_kg: None,
            unit_cost: None,
            classification: Classification::default(),
            attributes: Vec::new(),
        },
    )
    .await?;
    let rev = revisions::lock(&mut tx, created.revision_id).await?;
    write_record(&mut tx, actor, &rev, &design, &facts, input.built_on).await?;
    let mut lines = 0;
    if input.with_bom {
        lines = sqlx::query(
            "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes)
             SELECT gen_random_uuid(), $2, child_item_id, find_number, quantity, notes
             FROM bom_lines WHERE parent_revision_id = $1",
        )
        .bind(design.id)
        .bind(rev.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "as_built.unit.create",
        "item",
        Some(created.item_id),
        Some(design.item_id),
        format!(
            "Recorded {} as built to {}",
            created.number,
            revisions::label(&design)
        ),
        json!({ "revision_id": rev.id, "design_revision_id": design.id, "bom_lines": lines }),
    )
    .await?;
    tx.commit().await?;
    revisions::get(pool, actor, created.revision_id).await
}

// ---------------------------------------------------------------------------
// Deviations
// ---------------------------------------------------------------------------

/// How the built unit departs from its design.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Deviation {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub position: i32,
    pub kind: String,
    pub feature: String,
    pub designed: String,
    pub built: String,
    pub disposition: String,
    pub note: String,
    pub created_by_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const DEVIATION_SELECT: &str = "
    SELECT d.id, d.revision_id, d.position, d.kind, d.feature, d.designed, d.built,
           d.disposition, d.note, u.display_name AS created_by_name, d.created_at, d.updated_at
    FROM as_built_deviations d
    JOIN users u ON u.id = d.created_by";

/// The deviations of an as-built revision the actor may read.
pub async fn deviations(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<Deviation>> {
    revisions::get(pool, actor, revision_id).await?;
    Ok(sqlx::query_as::<_, Deviation>(&format!(
        "{DEVIATION_SELECT} WHERE d.revision_id = $1 ORDER BY d.position, d.created_at"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?)
}

pub async fn get_deviation(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Deviation> {
    let d = sqlx::query_as::<_, Deviation>(&format!("{DEVIATION_SELECT} WHERE d.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Deviation not found."))?;
    revisions::get(pool, actor, d.revision_id)
        .await
        .map_err(|_| AppError::not_found("Deviation not found."))?;
    Ok(d)
}

/// Deviation counts of an as-built revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DeviationSummary {
    pub total: usize,
    /// Without a disposition yet (block the release).
    pub open: usize,
    pub use_as_is: usize,
    pub rework: usize,
    pub repair: usize,
    pub scrap: usize,
}

pub fn summarize(deviations: &[Deviation]) -> DeviationSummary {
    let mut s = DeviationSummary {
        total: deviations.len(),
        ..DeviationSummary::default()
    };
    for d in deviations {
        match Disposition::parse(&d.disposition) {
            Some(Disposition::Open) | None => s.open += 1,
            Some(Disposition::UseAsIs) => s.use_as_is += 1,
            Some(Disposition::Rework) => s.rework += 1,
            Some(Disposition::Repair) => s.repair += 1,
            Some(Disposition::Scrap) => s.scrap += 1,
        }
    }
    s
}

/// A deviation as entered.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct DeviationInput {
    pub kind: DeviationKind,
    pub feature: String,
    #[serde(default)]
    pub designed: String,
    #[serde(default)]
    pub built: String,
    #[serde(default = "open")]
    pub disposition: Disposition,
    #[serde(default)]
    pub note: String,
}

fn open() -> Disposition {
    Disposition::Open
}

impl DeviationInput {
    pub fn validated(self) -> AppResult<Self> {
        Ok(DeviationInput {
            feature: domain::require_text("Feature", &self.feature, 200)?,
            designed: domain::limit_text("As designed", self.designed.trim(), 200)?,
            built: domain::limit_text("As built", self.built.trim(), 200)?,
            note: domain::limit_text("Note", &self.note, 2000)?,
            ..self
        })
    }
}

/// Adds a deviation to an as-built revision checked out by the actor.
pub async fn add_deviation(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    input: DeviationInput,
) -> AppResult<Deviation> {
    let input = input.validated()?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, revision_id).await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM as_built_deviations WHERE revision_id = $1",
    )
    .bind(rev.id)
    .fetch_one(&mut *tx)
    .await?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_DEVIATIONS {
        return Err(AppError::BadRequest(format!(
            "An as-built revision can hold at most {MAX_DEVIATIONS} deviations."
        )));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO as_built_deviations (id, revision_id, position, kind, feature, designed, built,
                                          disposition, note, created_by)
         VALUES ($1, $2, (SELECT COALESCE(MAX(position), 0) + 1 FROM as_built_deviations WHERE revision_id = $2),
                 $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(rev.id)
    .bind(input.kind.as_str())
    .bind(&input.feature)
    .bind(&input.designed)
    .bind(&input.built)
    .bind(input.disposition.as_str())
    .bind(&input.note)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "as_built.deviation.add",
        "revision",
        Some(rev.id),
        format!(
            "{}: deviation {} ({}, {})",
            revisions::label(&rev),
            input.feature,
            input.kind.label().to_lowercase(),
            input.disposition.label().to_lowercase()
        ),
        json!({ "item_id": rev.item_id, "deviation_id": id,
                "disposition": input.disposition.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get_deviation(pool, actor, id).await
}

/// Changes a deviation of an as-built revision checked out by the actor.
pub async fn update_deviation(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: DeviationInput,
) -> AppResult<Deviation> {
    let input = input.validated()?;
    let existing = get_deviation(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, existing.revision_id).await?;
    let updated = sqlx::query(
        "UPDATE as_built_deviations SET kind = $2, feature = $3, designed = $4, built = $5,
                disposition = $6, note = $7, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(input.kind.as_str())
    .bind(&input.feature)
    .bind(&input.designed)
    .bind(&input.built)
    .bind(input.disposition.as_str())
    .bind(&input.note)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(AppError::not_found("Deviation not found."));
    }
    let disposition = if existing.disposition == input.disposition.as_str() {
        String::new()
    } else {
        format!(
            ", disposition {} → {}",
            Disposition::parse(&existing.disposition).map_or("?", Disposition::label),
            input.disposition.label()
        )
    };
    audit::record(
        &mut tx,
        Some(actor),
        "as_built.deviation.update",
        "revision",
        Some(rev.id),
        format!(
            "{}: deviation {}{disposition}",
            revisions::label(&rev),
            input.feature
        ),
        json!({ "item_id": rev.item_id, "deviation_id": id,
                "disposition": input.disposition.as_str(),
                "previous_disposition": existing.disposition }),
    )
    .await?;
    tx.commit().await?;
    get_deviation(pool, actor, id).await
}

/// Removes a deviation from an as-built revision checked out by the actor.
pub async fn remove_deviation(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Deviation> {
    let existing = get_deviation(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = lock_editable(&mut tx, actor, existing.revision_id).await?;
    let removed = sqlx::query("DELETE FROM as_built_deviations WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found("Deviation not found."));
    }
    audit::record(
        &mut tx,
        Some(actor),
        "as_built.deviation.remove",
        "revision",
        Some(rev.id),
        format!(
            "{}: removed the deviation {}",
            revisions::label(&rev),
            existing.feature
        ),
        json!({ "item_id": rev.item_id, "deviation_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing)
}

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

/// Copies the build record and the deviations of `from` to the new revision
/// `to` (Revise). Returns (records, deviations).
pub(crate) async fn copy_to_revision(
    conn: &mut PgConnection,
    from: Uuid,
    to: Uuid,
) -> AppResult<(u64, u64)> {
    let records = sqlx::query(
        "INSERT INTO as_built_records (revision_id, design_revision_id, design_item_id,
                                       serial_number, built_on, note, updated_by, updated_at)
         SELECT $2, design_revision_id, design_item_id, serial_number, built_on, note,
                updated_by, updated_at
         FROM as_built_records WHERE revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let deviations = sqlx::query(
        "INSERT INTO as_built_deviations (id, revision_id, position, kind, feature, designed, built,
                                          disposition, note, created_by, created_at, updated_at)
         SELECT gen_random_uuid(), $2, position, kind, feature, designed, built, disposition,
                note, created_by, created_at, updated_at
         FROM as_built_deviations WHERE revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok((records, deviations))
}

/// Whether units were recorded as built to a revision of the item (such an
/// item cannot be deleted).
pub(crate) async fn item_is_design(conn: &mut PgConnection, item_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM as_built_records WHERE design_item_id = $1)",
    )
    .bind(item_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// Fails when the as-built revision `rev` is not ready to be submitted for
/// release: it has no build record or a deviation is still open. Other
/// item types always pass.
pub(crate) async fn ensure_ready(conn: &mut PgConnection, rev: &Revision) -> AppResult<()> {
    if rev.item_type != ItemType::AsBuilt.as_str() {
        return Ok(());
    }
    let (has_record, open) = sqlx::query_as::<_, (bool, i64)>(
        "SELECT EXISTS(SELECT 1 FROM as_built_records WHERE revision_id = $1),
                (SELECT COUNT(*) FROM as_built_deviations WHERE revision_id = $1 AND disposition = 'open')",
    )
    .bind(rev.id)
    .fetch_one(&mut *conn)
    .await?;
    if !has_record {
        return Err(AppError::Conflict(format!(
            "{} has no build record; enter the design revision it was built to first.",
            revisions::label(rev)
        )));
    }
    if open > 0 {
        return Err(AppError::Conflict(format!(
            "{} has {open} open deviation{}; decide the disposition (use as is, rework, repair or scrap) first.",
            revisions::label(rev),
            if open == 1 { "" } else { "s" }
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The design side
// ---------------------------------------------------------------------------

/// A unit built to a revision of a design item (the latest revision of its
/// as-built item).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct BuiltUnit {
    pub unit_item_id: Uuid,
    pub unit_number: String,
    pub unit_revision_id: Uuid,
    pub unit_code: String,
    pub unit_state: String,
    pub unit_name: String,
    pub serial_number: String,
    pub built_on: Option<NaiveDate>,
    pub design_revision_id: Uuid,
    pub design_code: String,
    #[serde(skip)]
    pub design_seq: i32,
    #[serde(skip)]
    pub design_number: String,
    #[serde(skip)]
    pub released_code: Option<String>,
    #[serde(skip)]
    pub released_seq: Option<i32>,
    pub deviation_count: i64,
    pub open_count: i64,
    /// Set when a newer design revision is released.
    #[sqlx(default)]
    pub newer_design: Option<String>,
}

/// Units built to any revision of the design item the actor may see (at
/// most [`MAX_UNITS_LISTED`]); the second value counts the hidden ones.
pub async fn built_units(
    pool: &PgPool,
    actor: &Actor,
    design_item_id: Uuid,
) -> AppResult<(Vec<BuiltUnit>, i64)> {
    items::get(pool, actor, design_item_id).await?;
    let latest = "ar.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = ar.item_id)";
    let rows = sqlx::query_as::<_, BuiltUnit>(&format!(
        "SELECT ai.id AS unit_item_id, ai.number AS unit_number, ar.id AS unit_revision_id,
                ar.code AS unit_code, ar.state AS unit_state, ar.name AS unit_name,
                rec.serial_number, rec.built_on, dr.id AS design_revision_id,
                dr.code AS design_code, dr.seq AS design_seq, di.number AS design_number,
                lr.code AS released_code, lr.seq AS released_seq,
                (SELECT COUNT(*) FROM as_built_deviations d WHERE d.revision_id = ar.id) AS deviation_count,
                (SELECT COUNT(*) FROM as_built_deviations d WHERE d.revision_id = ar.id
                   AND d.disposition = 'open') AS open_count
         FROM as_built_records rec
         JOIN revisions ar ON ar.id = rec.revision_id
         JOIN items ai ON ai.id = ar.item_id
         JOIN revisions dr ON dr.id = rec.design_revision_id
         JOIN items di ON di.id = dr.item_id
         LEFT JOIN LATERAL (
             SELECT r.code, r.seq FROM revisions r
             WHERE r.item_id = di.id AND r.released_at IS NOT NULL
             ORDER BY r.seq DESC LIMIT 1
         ) lr ON TRUE
         WHERE rec.design_item_id = $1 AND {latest} AND {}
         ORDER BY ai.number
         LIMIT $3",
        access::item_visible("ai", 2)
    ))
    .bind(design_item_id)
    .bind(&actor.hidden_projects)
    .bind(MAX_UNITS_LISTED)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|mut u| {
        u.newer_design = newer_design(
            &u.design_number,
            &u.design_code,
            u.design_seq,
            u.released_code.as_deref(),
            u.released_seq,
        );
        u
    })
    .collect();
    let hidden = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM as_built_records rec
         JOIN revisions ar ON ar.id = rec.revision_id
         JOIN items ai ON ai.id = ar.item_id
         WHERE rec.design_item_id = $1 AND {latest} AND NOT {}",
        access::item_visible("ai", 2)
    ))
    .bind(design_item_id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?;
    Ok((rows, hidden))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_design_only_when_a_later_revision_is_released() {
        assert_eq!(newer_design("PRT-1", "B", 2, None, None), None);
        assert_eq!(newer_design("PRT-1", "B", 2, Some("B"), Some(2)), None);
        assert_eq!(newer_design("PRT-1", "B", 2, Some("A"), Some(1)), None);
        assert_eq!(
            newer_design("PRT-1", "B", 2, Some("C"), Some(3)).as_deref(),
            Some("PRT-1/C is released (built to: B)")
        );
    }

    #[test]
    fn build_dates_and_serials_are_validated() {
        assert_eq!(parse_built_on(None).unwrap(), None);
        assert_eq!(parse_built_on(Some("  ")).unwrap(), None);
        assert_eq!(
            parse_built_on(Some("2026-11-06")).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 6)
        );
        assert!(parse_built_on(Some("06.11.2026")).is_err());
        assert_eq!(
            validate_facts("  SN-001 ", "").unwrap().serial_number,
            "SN-001"
        );
        assert!(validate_facts("SN\n2", "").is_err());
        assert!(validate_facts(&"9".repeat(101), "").is_err());
    }

    fn deviation(disposition: &str) -> Deviation {
        Deviation {
            id: Uuid::nil(),
            revision_id: Uuid::nil(),
            position: 1,
            kind: "dimension".into(),
            feature: "Bore".into(),
            designed: String::new(),
            built: String::new(),
            disposition: disposition.into(),
            note: String::new(),
            created_by_name: String::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn summary_counts_dispositions() {
        let s = summarize(&[
            deviation("open"),
            deviation("use_as_is"),
            deviation("use_as_is"),
            deviation("scrap"),
        ]);
        assert_eq!(
            s,
            DeviationSummary {
                total: 4,
                open: 1,
                use_as_is: 2,
                rework: 0,
                repair: 0,
                scrap: 1,
            }
        );
    }
}
