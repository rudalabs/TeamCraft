//! Change propagation to dependents.
//!
//! When a revision is **released**, TeamCraft
//!
//! 1. freezes the configuration of the released revisions: every BOM line and
//!    item reference remembers the revision of its target at that moment
//!    (revision rule *as released*);
//! 2. resolves the open impacts of the released items themselves – a new
//!    release was reviewed with the current components (*superseded*);
//! 3. flags every **dependent** for a check: items whose latest revision uses
//!    the released item in its BOM, references it (drawing, specification,
//!    reference), has a CAD file that references one of its files, or is a
//!    simulation based on another revision of it (or on a file version that
//!    is no longer current; see `simulations`). Items released in the same
//!    change are not flagged.
//!
//! When the latest revision of an item is made **obsolete**, its dependents
//! are flagged the same way and its own open impacts are closed.
//!
//! An impact stays open until a CAD editor of the dependent's project group
//! acknowledges it with a note ("form, fit and function unchanged"), a newer
//! revision of the dependent is released, or the dependent becomes obsolete.
//! Propagation is one level per event: when the dependent is revised and
//! released, its own dependents are flagged in turn.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, NotificationKind, Role},
    error::{AppError, AppResult},
    models::{Impact, Revision},
    services::{
        access, audit,
        notifications::{self, Notice},
        references, revisions,
    },
};

const IMPACT_SELECT: &str = "
    SELECT im.id, im.item_id, i.number AS item_number, i.project_id, p.code AS project_code,
           r.id AS revision_id, r.code AS revision_code, r.state AS revision_state, r.name AS item_name,
           im.source_item_id, si.number AS source_number, sr.id AS source_revision_id,
           sr.code AS source_code, sr.name AS source_name, im.event, im.relation, im.detail,
           im.change_id, c.number AS change_number, im.created_at, im.resolved_at, im.resolution,
           u.display_name AS resolved_by_name, im.note, si.project_id AS source_project_id
    FROM impacts im
    JOIN items i ON i.id = im.item_id
    LEFT JOIN projects p ON p.id = i.project_id
    JOIN revisions r ON r.id = im.revision_id
    JOIN items si ON si.id = im.source_item_id
    JOIN revisions sr ON sr.id = im.source_revision_id
    LEFT JOIN changes c ON c.id = im.change_id
    LEFT JOIN users u ON u.id = im.resolved_by";

/// Latest, not obsolete revision of an item (SQL condition on `pr`).
const LATEST: &str = "pr.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = pr.item_id)
                      AND pr.state <> 'obsolete'";

/// An impact on an item the actor may read; a hidden source is redacted.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Impact> {
    let impact = sqlx::query_as::<_, Impact>(&format!("{IMPACT_SELECT} WHERE im.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Impact not found."))?;
    access::ensure_visible(
        actor,
        impact.project_id,
        AppError::not_found("Impact not found."),
    )?;
    Ok(impact.redact(actor))
}

/// Impacts on an item: open ones first, then the 20 most recently resolved.
/// Sources the actor may not see are redacted.
pub async fn for_item(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Vec<Impact>> {
    let rows = sqlx::query_as::<_, Impact>(&format!(
        "({IMPACT_SELECT} WHERE im.item_id = $1 AND im.resolved_at IS NULL ORDER BY im.created_at DESC)
         UNION ALL
         ({IMPACT_SELECT} WHERE im.item_id = $1 AND im.resolved_at IS NOT NULL
          ORDER BY im.resolved_at DESC LIMIT 20)"
    ))
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    if let Some(first) = rows.first() {
        access::ensure_visible(actor, first.project_id, access::not_found_item())?;
    }
    Ok(rows.into_iter().map(|i| i.redact(actor)).collect())
}

/// Open impacts the actor can resolve: on items of their project groups
/// (all for admins, none for viewers), newest first.
pub async fn open_for(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Impact>> {
    if !actor.can_edit() {
        return Ok(Vec::new());
    }
    let projects: Vec<Uuid> = actor.projects.to_vec();
    Ok(sqlx::query_as::<_, Impact>(&format!(
        "{IMPACT_SELECT} WHERE im.resolved_at IS NULL AND ($1 OR i.project_id = ANY($2))
           AND {}
         ORDER BY im.created_at DESC LIMIT 200",
        access::item_visible("i", 3)
    ))
    .bind(actor.is_admin())
    .bind(&projects)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|i| i.redact(actor))
    .collect())
}

/// Marks an open impact as checked: the dependent needs no change.
pub async fn acknowledge(pool: &PgPool, actor: &Actor, id: Uuid, note: &str) -> AppResult<Impact> {
    actor.require(Role::Editor)?;
    let note = domain::require_text("Note", note, 2000).map_err(|_| {
        AppError::BadRequest(
            "Say why the dependent needs no change (e.g. form, fit and function unchanged).".into(),
        )
    })?;
    let impact = get(pool, actor, id).await?;
    if impact.resolved_at.is_some() {
        return Err(AppError::Conflict(format!(
            "This impact on {} is already resolved.",
            impact.item_number
        )));
    }
    if !actor.can_edit_project(impact.project_id) {
        return Err(AppError::Forbidden(format!(
            "Only CAD editors of the project group {} (or admins) can acknowledge impacts on {}.",
            impact.project_code.as_deref().unwrap_or("—"),
            impact.item_number
        )));
    }
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE impacts SET resolved_at = now(), resolved_by = $2, resolution = 'acknowledged', note = $3
         WHERE id = $1 AND resolved_at IS NULL",
    )
    .bind(id)
    .bind(actor.id)
    .bind(&note)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(AppError::Conflict(
            "The impact was resolved in the meantime.".into(),
        ));
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "impact.acknowledge",
        "item",
        Some(impact.item_id),
        Some(impact.source_item_id),
        format!(
            "{} checked against {}/{} ({}): {note}",
            impact.item_number, impact.source_number, impact.source_code, impact.event
        ),
        json!({ "impact_id": id, "source_revision_id": impact.source_revision_id }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// The result of [`acknowledge_all`].
#[derive(Clone, Debug, Serialize)]
pub struct Acknowledged {
    pub item_number: String,
    pub revision_code: String,
    /// The impacts acknowledged now (resolved, with the note).
    pub acknowledged: Vec<Impact>,
    /// Requested impacts that were resolved in the meantime (skipped).
    pub skipped: usize,
}

/// Acknowledges every open impact on a dependent revision with one note –
/// or only those in `only` (the ones the user saw, so a check flagged a
/// moment ago is not acknowledged unseen). Impacts resolved in the meantime
/// are skipped; if none is left, nothing changes (conflict). Same rules as
/// [`acknowledge`]; one transaction, one audit entry per impact.
pub async fn acknowledge_all(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    only: Option<&[Uuid]>,
    note: &str,
) -> AppResult<Acknowledged> {
    actor.require(Role::Editor)?;
    let note = domain::require_text("Note", note, 2000).map_err(|_| {
        AppError::BadRequest(
            "Say why the dependent needs no change (e.g. form, fit and function unchanged).".into(),
        )
    })?;
    let rev = revisions::get(pool, actor, revision_id).await?;
    if !actor.can_edit_project(rev.project_id) {
        return Err(AppError::Forbidden(format!(
            "Only CAD editors of the project group {} (or admins) can acknowledge impacts on {}.",
            rev.project_code.as_deref().unwrap_or("—"),
            rev.item_number
        )));
    }
    // The requested ids, without duplicates; an empty list is a mistake of
    // the caller, not "resolved in the meantime".
    let wanted = only.map(|o| {
        let mut wanted = o.to_vec();
        wanted.sort_unstable();
        wanted.dedup();
        wanted
    });
    if wanted.as_ref().is_some_and(Vec::is_empty) {
        return Err(AppError::BadRequest(
            "List the impacts to acknowledge (or leave the list out for all).".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    if let Some(wanted) = &wanted {
        // Every listed impact has to be one on this revision; otherwise the
        // answer would count foreign or unknown ids as "resolved meanwhile".
        let known = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM impacts WHERE revision_id = $1 AND id = ANY($2)",
        )
        .bind(revision_id)
        .bind(wanted)
        .fetch_one(&mut *tx)
        .await?;
        if known != wanted.len() as i64 {
            return Err(AppError::BadRequest(format!(
                "Only impacts on {} can be acknowledged here.",
                revisions::label(&rev)
            )));
        }
    }
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM impacts
         WHERE revision_id = $1 AND resolved_at IS NULL AND ($2::uuid[] IS NULL OR id = ANY($2))
         ORDER BY created_at
         FOR UPDATE",
    )
    .bind(revision_id)
    .bind(wanted.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    if ids.is_empty() {
        return Err(AppError::Conflict(format!(
            "The checks on {} were resolved in the meantime.",
            revisions::label(&rev)
        )));
    }
    let skipped = wanted.map_or(0, |w| w.len().saturating_sub(ids.len()));
    sqlx::query(
        "UPDATE impacts SET resolved_at = now(), resolved_by = $2, resolution = 'acknowledged', note = $3
         WHERE id = ANY($1) AND resolved_at IS NULL",
    )
    .bind(&ids)
    .bind(actor.id)
    .bind(&note)
    .execute(&mut *tx)
    .await?;
    let acknowledged = sqlx::query_as::<_, Impact>(&format!(
        "{IMPACT_SELECT} WHERE im.id = ANY($1) ORDER BY si.number"
    ))
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    for impact in &acknowledged {
        audit::record_related(
            &mut tx,
            Some(actor),
            "impact.acknowledge",
            "item",
            Some(impact.item_id),
            Some(impact.source_item_id),
            format!(
                "{} checked against {}/{} ({}): {note}",
                impact.item_number, impact.source_number, impact.source_code, impact.event
            ),
            json!({ "impact_id": impact.id, "source_revision_id": impact.source_revision_id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Acknowledged {
        item_number: rev.item_number,
        revision_code: rev.code,
        acknowledged,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Grouping by dependent (worklists)
// ---------------------------------------------------------------------------

/// The open impacts on one dependent revision: everything that has to be
/// checked on one assembly, drawing or simulation.
#[derive(Clone, Debug, Serialize)]
pub struct DependentGroup {
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub revision_state: String,
    /// When the newest of its impacts was flagged.
    pub latest_at: DateTime<Utc>,
    /// The impact ids, comma-separated (the hidden field of the web form
    /// that acknowledges exactly the impacts shown).
    pub impact_ids: String,
    /// The impacts, ordered by source item number.
    pub impacts: Vec<Impact>,
}

/// Groups impacts by the dependent revision. Groups keep the order in which
/// they first appear (newest first for [`open_for`]); inside a group the
/// sources are ordered by item number and revision.
pub fn group_by_dependent(impacts: Vec<Impact>) -> Vec<DependentGroup> {
    let mut groups: Vec<DependentGroup> = Vec::new();
    for impact in impacts {
        if let Some(g) = groups
            .iter_mut()
            .find(|g| g.revision_id == impact.revision_id)
        {
            g.latest_at = g.latest_at.max(impact.created_at);
            g.impacts.push(impact);
            continue;
        }
        groups.push(DependentGroup {
            item_id: impact.item_id,
            item_number: impact.item_number.clone(),
            item_name: impact.item_name.clone(),
            project_id: impact.project_id,
            project_code: impact.project_code.clone(),
            revision_id: impact.revision_id,
            revision_code: impact.revision_code.clone(),
            revision_state: impact.revision_state.clone(),
            latest_at: impact.created_at,
            impact_ids: String::new(),
            impacts: vec![impact],
        });
    }
    for g in &mut groups {
        g.impacts.sort_by(|a, b| {
            (&a.source_number, &a.source_code).cmp(&(&b.source_number, &b.source_code))
        });
        g.impact_ids = g
            .impacts
            .iter()
            .map(|i| i.id.to_string())
            .collect::<Vec<_>>()
            .join(",");
    }
    groups
}

/// Parses the comma-separated impact ids of a web form (see
/// [`DependentGroup::impact_ids`]); empty means all open impacts.
pub fn parse_ids(text: &str) -> AppResult<Option<Vec<Uuid>>> {
    let ids = text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            Uuid::parse_str(s).map_err(|_| AppError::BadRequest("Invalid impact list.".into()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok((!ids.is_empty()).then_some(ids))
}

/// Flags the dependents of `source_revision` (see the module docs) and
/// returns their item ids. Items in `exclude` are not flagged.
async fn flag_dependents(
    conn: &mut PgConnection,
    source: &Revision,
    event: &str,
    change_id: Option<Uuid>,
    exclude: &[Uuid],
) -> AppResult<Vec<Uuid>> {
    // On conflict the newer event replaces the open impact of the same source.
    const UPSERT: &str = "
        ON CONFLICT (item_id, source_item_id) WHERE resolved_at IS NULL
        DO UPDATE SET revision_id = EXCLUDED.revision_id, source_revision_id = EXCLUDED.source_revision_id,
                      event = EXCLUDED.event, relation = EXCLUDED.relation, detail = EXCLUDED.detail,
                      change_id = EXCLUDED.change_id, created_at = now()
        RETURNING item_id";
    // CAD files of dependents that reference a file of the source revision.
    let by_file = format!(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event,
                              relation, detail, change_id)
         SELECT DISTINCT ON (pr.item_id) gen_random_uuid(), pr.item_id, pr.id, $1, $2, $3, 'file',
                df.filename || ' references ' || sf.filename, $4
         FROM files sf
         JOIN file_references fr ON lower(fr.name) = lower(sf.filename)
         JOIN file_versions dv ON dv.id = fr.file_version_id
         JOIN files df ON df.id = dv.file_id AND dv.version = df.current_version
         JOIN revisions pr ON pr.id = df.revision_id
         JOIN items di ON di.id = pr.item_id
         WHERE sf.revision_id = $2 AND pr.item_id <> $1 AND {LATEST} AND NOT (pr.item_id = ANY($5))
           -- a file-name match alone never crosses the boundary of a
           -- restricted project group (it would tell one side about the other)
           AND (di.project_id IS NOT DISTINCT FROM (SELECT si.project_id FROM items si WHERE si.id = $1)
                OR NOT EXISTS (SELECT 1 FROM projects xp
                               WHERE xp.visibility = 'restricted'
                                 AND (xp.id = di.project_id
                                      OR xp.id = (SELECT si.project_id FROM items si WHERE si.id = $1))))
         ORDER BY pr.item_id, df.filename
         {UPSERT}"
    );
    // Drawings, specifications and references to the source item.
    let by_reference = format!(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event,
                              relation, detail, change_id)
         SELECT DISTINCT ON (pr.item_id) gen_random_uuid(), pr.item_id, pr.id, $1, $2, $3, ir.kind,
                ir.note, $4
         FROM item_references ir
         JOIN revisions pr ON pr.id = ir.from_revision_id
         WHERE ir.to_item_id = $1 AND {LATEST} AND NOT (pr.item_id = ANY($5))
         ORDER BY pr.item_id, ir.kind
         {UPSERT}"
    );
    // Simulations based on a revision of the source item: flagged when a
    // different revision is released, when the simulated file version is no
    // longer the current one, or when the source becomes obsolete.
    let by_simulation = format!(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event,
                              relation, detail, change_id)
         SELECT DISTINCT ON (pr.item_id) gen_random_uuid(), pr.item_id, pr.id, $1, $2, $3, 'simulation',
                'based on ' || br.code ||
                CASE WHEN sb.file_name <> '' THEN ', ' || sb.file_name || ' v' || sb.file_version ELSE '' END,
                $4
         FROM simulation_bases sb
         JOIN revisions br ON br.id = sb.basis_revision_id
         JOIN revisions pr ON pr.id = sb.revision_id
         LEFT JOIN file_versions fv ON fv.id = sb.file_version_id
         LEFT JOIN files f ON f.id = fv.file_id
         WHERE br.item_id = $1 AND {LATEST} AND NOT (pr.item_id = ANY($5))
           AND ($3 = 'obsolete' OR br.id <> $2
                OR (sb.file_name <> '' AND (fv.id IS NULL OR fv.version <> f.current_version)))
         ORDER BY pr.item_id, sb.role
         {UPSERT}"
    );
    // Assemblies using the source item (last, so the BOM wins if several apply).
    let by_bom = format!(
        "INSERT INTO impacts (id, item_id, revision_id, source_item_id, source_revision_id, event,
                              relation, detail, change_id)
         SELECT DISTINCT ON (pr.item_id) gen_random_uuid(), pr.item_id, pr.id, $1, $2, $3, 'bom',
                'find no. ' || bl.find_number, $4
         FROM bom_lines bl
         JOIN revisions pr ON pr.id = bl.parent_revision_id
         WHERE bl.child_item_id = $1 AND {LATEST} AND NOT (pr.item_id = ANY($5))
         ORDER BY pr.item_id, bl.find_number
         {UPSERT}"
    );
    let mut flagged = Vec::new();
    for sql in [by_file, by_simulation, by_reference, by_bom] {
        let ids = sqlx::query_scalar::<_, Uuid>(&sql)
            .bind(source.item_id)
            .bind(source.id)
            .bind(event)
            .bind(change_id)
            .bind(exclude)
            .fetch_all(&mut *conn)
            .await?;
        for id in ids {
            if !flagged.contains(&id) {
                flagged.push(id);
            }
        }
    }
    Ok(flagged)
}

/// Writes an audit entry that lists the flagged dependents.
async fn record_flags(
    conn: &mut PgConnection,
    actor: &Actor,
    source: &Revision,
    event: &str,
    flagged: &[Uuid],
) -> AppResult<()> {
    if flagged.is_empty() {
        return Ok(());
    }
    // One entry per project group of the dependents, each naming only its own
    // items: a reader who may not see a restricted group's dependents still
    // reads the entries about the others.
    let rows = sqlx::query_as::<_, (Option<Uuid>, Uuid, String)>(
        "SELECT project_id, id, number FROM items WHERE id = ANY($1)
         ORDER BY project_id NULLS FIRST, number",
    )
    .bind(flagged)
    .fetch_all(&mut *conn)
    .await?;
    let mut groups: Vec<(Option<Uuid>, Vec<Uuid>, Vec<String>)> = Vec::new();
    for (project, id, number) in rows {
        match groups.last_mut() {
            Some((p, ids, numbers)) if *p == project => {
                ids.push(id);
                numbers.push(number);
            }
            _ => groups.push((project, vec![id], vec![number])),
        }
    }
    for (_, ids, numbers) in groups {
        audit::record_mentioning(
            conn,
            Some(actor),
            "impact.flag",
            "revision",
            Some(source.id),
            &audit::Mentions::items(ids.iter().copied()),
            format!(
                "{} {event}: check {}",
                revisions::label(source),
                numbers.join(", ")
            ),
            json!({ "item_id": source.item_id, "dependents": ids }),
        )
        .await?;
    }
    notify_flagged(conn, actor, source, flagged).await
}

/// Tells the creator of each flagged item, the author of its latest revision
/// and its watchers that it has to be checked.
async fn notify_flagged(
    conn: &mut PgConnection,
    actor: &Actor,
    source: &Revision,
    flagged: &[Uuid],
) -> AppResult<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        item_id: Uuid,
        number: String,
        item_creator: Uuid,
        revision_id: Uuid,
        code: String,
        revision_creator: Uuid,
        event: String,
        relation: String,
        detail: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT i.id AS item_id, i.number, i.created_by AS item_creator, lr.id AS revision_id,
                lr.code, lr.created_by AS revision_creator, im.event, im.relation, im.detail
         FROM impacts im
         JOIN items i ON i.id = im.item_id
         CROSS JOIN LATERAL (SELECT r.id, r.code, r.created_by FROM revisions r
                             WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1) lr
         WHERE im.source_item_id = $1 AND im.item_id = ANY($2) AND im.resolved_at IS NULL",
    )
    .bind(source.item_id)
    .bind(flagged)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        let mut recipients = notifications::watchers(conn, "item", &[row.item_id]).await?;
        recipients.extend([row.item_creator, row.revision_creator]);
        let how = match row.relation.as_str() {
            "bom" => "uses it in its BOM",
            "drawing" => "is a drawing of it",
            "specification" => "is a specification for it",
            "reference" => "references it",
            "simulation" => "is a simulation based on it",
            _ => "has a CAD file that references its files",
        };
        let notice = Notice::revision_ref(
            NotificationKind::Impact,
            row.revision_id,
            row.item_id,
            &row.number,
            &row.code,
            format!(
                "Check needed: {} – {} {}",
                row.number,
                revisions::label(source),
                if row.event == "released" {
                    "was released"
                } else {
                    "is obsolete"
                }
            ),
            format!(
                "{} {how}{}. Acknowledge with a note if it needs no change, or revise it.",
                row.number,
                if row.detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", row.detail)
                }
            ),
        )
        // Recipients who may not see the source learn only that something
        // restricted changed.
        .redacted_for(
            source.item_id,
            format!(
                "Check needed: {} – a restricted item this depends on {}",
                row.number,
                if row.event == "released" {
                    "was released"
                } else {
                    "is obsolete"
                }
            ),
            format!(
                "{} {how}. Acknowledge with a note if it needs no change, or revise it.",
                row.number
            ),
        );
        notifications::notify(conn, Some(actor), &recipients, &notice).await?;
    }
    Ok(())
}

/// Change propagation after revisions were released together (one change).
/// Runs inside the releasing transaction, after the state change.
pub(crate) async fn on_release(
    conn: &mut PgConnection,
    actor: &Actor,
    change_id: Option<Uuid>,
    released: &[Uuid],
) -> AppResult<()> {
    if released.is_empty() {
        return Ok(());
    }
    // 1. As-released configuration of the released revisions.
    sqlx::query(
        "UPDATE bom_lines bl SET released_child_revision_id = (
             SELECT r.id FROM revisions r
             WHERE r.item_id = bl.child_item_id AND r.state = 'released'
             ORDER BY r.seq DESC LIMIT 1)
         WHERE bl.parent_revision_id = ANY($1)",
    )
    .bind(released)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE item_references ir SET released_to_revision_id = (
             SELECT r.id FROM revisions r
             WHERE r.item_id = ir.to_item_id AND r.state = 'released'
             ORDER BY r.seq DESC LIMIT 1)
         WHERE ir.from_revision_id = ANY($1)",
    )
    .bind(released)
    .execute(&mut *conn)
    .await?;
    // 2. The released items were reviewed with the current components.
    sqlx::query(
        "UPDATE impacts im SET resolved_at = now(), resolved_by = $2, resolution = 'superseded',
                note = 'Released as ' || r.code
         FROM revisions r
         WHERE r.id = ANY($1) AND im.item_id = r.item_id AND im.resolved_at IS NULL",
    )
    .bind(released)
    .bind(actor.id)
    .execute(&mut *conn)
    .await?;
    // 3. Flag the dependents.
    let mut revs = Vec::with_capacity(released.len());
    for id in released {
        revs.push(revisions::lock(conn, *id).await?);
    }
    let batch: Vec<Uuid> = revs.iter().map(|r| r.item_id).collect();
    for rev in &revs {
        let flagged = flag_dependents(conn, rev, "released", change_id, &batch).await?;
        record_flags(conn, actor, rev, "released", &flagged).await?;
    }
    Ok(())
}

/// Change propagation after a revision was made obsolete: if it was the
/// latest revision of its item, its dependents have to replace it.
pub(crate) async fn on_obsolete(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
) -> AppResult<()> {
    let is_latest = sqlx::query_scalar::<_, bool>(
        "SELECT $2 = (SELECT MAX(seq) FROM revisions WHERE item_id = $1)",
    )
    .bind(rev.item_id)
    .bind(rev.seq)
    .fetch_one(&mut *conn)
    .await?;
    if !is_latest {
        return Ok(());
    }
    sqlx::query(
        "UPDATE impacts SET resolved_at = now(), resolved_by = $2, resolution = 'obsolete',
                note = 'The item is obsolete'
         WHERE item_id = $1 AND resolved_at IS NULL",
    )
    .bind(rev.item_id)
    .bind(actor.id)
    .execute(&mut *conn)
    .await?;
    let flagged = flag_dependents(conn, rev, "obsolete", None, &[rev.item_id]).await?;
    record_flags(conn, actor, rev, "is obsolete", &flagged).await
}

// ---------------------------------------------------------------------------
// Impact analysis (before a change is approved)
// ---------------------------------------------------------------------------

/// An item that depends – directly (level 1) or through other dependents –
/// on a revision of a change.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Dependent {
    /// The item of the change it depends on.
    pub root_item_id: Uuid,
    pub level: i32,
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub revision_state: String,
    /// `bom`, `drawing`, `specification`, `reference`, `simulation` or `file`.
    pub relation: String,
    /// The item it uses or references.
    pub via_number: String,
    /// Released together in the same change (not flagged).
    #[sqlx(default)]
    pub in_change: bool,
    /// Whether the reader may see the dependent (hidden ones are dropped).
    #[sqlx(default)]
    #[serde(skip)]
    pub visible: bool,
}

/// The dependents of the given revisions: multi-level through BOMs and item
/// references of the latest revisions, plus CAD files that reference their
/// files.
pub async fn analysis(
    pool: &PgPool,
    actor: &Actor,
    revision_ids: &[Uuid],
) -> AppResult<(Vec<Dependent>, i64)> {
    if revision_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut out = sqlx::query_as::<_, Dependent>(&format!(
        "WITH RECURSIVE edges AS (
             SELECT bl.child_item_id AS target, pr.item_id AS dependent, 'bom'::text AS relation
             FROM bom_lines bl JOIN revisions pr ON pr.id = bl.parent_revision_id
             WHERE {LATEST}
             UNION
             SELECT ir.to_item_id, pr.item_id, ir.kind
             FROM item_references ir JOIN revisions pr ON pr.id = ir.from_revision_id
             WHERE {LATEST}
             UNION
             SELECT br.item_id, pr.item_id, 'simulation'::text
             FROM simulation_bases sb
             JOIN revisions br ON br.id = sb.basis_revision_id
             JOIN revisions pr ON pr.id = sb.revision_id
             WHERE {LATEST}
         ),
         up(root, item_id, level, relation, via, path, visible) AS (
             SELECT r.item_id, r.item_id, 0, ''::text, r.item_id, ARRAY[r.item_id], {root_vis}
             FROM revisions r JOIN items ri ON ri.id = r.item_id WHERE r.id = ANY($1)
             UNION ALL
             SELECT up.root, e.dependent, up.level + 1, e.relation, up.item_id, up.path || e.dependent,
                    {vis}
             FROM up JOIN edges e ON e.target = up.item_id
             JOIN items ui ON ui.id = e.dependent
             WHERE up.visible AND NOT (e.dependent = ANY(up.path)) AND up.level < 12
         )
         SELECT DISTINCT ON (up.root, up.item_id)
                up.root AS root_item_id, up.level, up.item_id, i.number AS item_number, lr.name AS item_name,
                lr.id AS revision_id, lr.code AS revision_code, lr.state AS revision_state,
                up.relation, vi.number AS via_number, up.visible
         FROM up
         JOIN items i ON i.id = up.item_id
         JOIN items vi ON vi.id = up.via
         CROSS JOIN LATERAL (
             SELECT r.id, r.code, r.state, r.name FROM revisions r
             WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
         ) lr
         WHERE up.level > 0
         ORDER BY up.root, up.item_id, up.level",
        vis = access::item_visible("ui", 2),
        root_vis = access::item_visible("ri", 2)
    ))
    .bind(revision_ids)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    // Hidden dependents are dropped (the walk stopped at them) and counted.
    let hidden_items: std::collections::HashSet<Uuid> = out
        .iter()
        .filter(|d| !d.visible)
        .map(|d| d.item_id)
        .collect();
    out.retain(|d| d.visible);
    // CAD files of other items that reference the files (one level).
    for id in revision_ids {
        let rev = revisions::get_unchecked(pool, *id).await?;
        if !actor.can_see_project(rev.project_id) {
            continue;
        }
        // hidden file-name matches are left out without a count
        let referrers = references::file_referrers(pool, actor, *id).await?;
        for f in referrers {
            if out
                .iter()
                .any(|d| d.root_item_id == rev.item_id && d.item_id == f.item_id)
            {
                continue;
            }
            out.push(Dependent {
                root_item_id: rev.item_id,
                level: 1,
                item_id: f.item_id,
                item_number: f.item_number,
                item_name: f.item_name,
                revision_id: f.revision_id,
                revision_code: f.revision_code,
                revision_state: f.revision_state,
                relation: "file".into(),
                via_number: rev.item_number.clone(),
                in_change: false,
                visible: true,
            });
        }
    }
    let in_change: Vec<Uuid> =
        sqlx::query_scalar("SELECT item_id FROM revisions WHERE id = ANY($1)")
            .bind(revision_ids)
            .fetch_all(pool)
            .await?;
    for d in &mut out {
        d.in_change = in_change.contains(&d.item_id);
    }
    out.sort_by(|a, b| {
        (a.level, &a.item_number, &a.via_number).cmp(&(b.level, &b.item_number, &b.via_number))
    });
    out.dedup_by(|a, b| a.item_id == b.item_id && a.via_number == b.via_number);
    Ok((out, hidden_items.len() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn impact(n: u128, dependent: u128, source: &str, minute: u32) -> Impact {
        Impact {
            id: Uuid::from_u128(n),
            item_id: Uuid::from_u128(1000 + dependent),
            item_number: format!("ASM-{dependent:06}"),
            project_id: None,
            project_code: Some("ROVER".into()),
            revision_id: Uuid::from_u128(2000 + dependent),
            revision_code: "A".into(),
            revision_state: "released".into(),
            item_name: "Enclosure".into(),
            source_item_id: Uuid::from_u128(3000 + n),
            source_number: source.into(),
            source_revision_id: Uuid::from_u128(4000 + n),
            source_code: "A".into(),
            source_name: "Screw".into(),
            event: "released".into(),
            relation: "bom".into(),
            detail: String::new(),
            change_id: None,
            change_number: None,
            created_at: Utc.with_ymd_and_hms(2026, 11, 5, 9, minute, 0).unwrap(),
            resolved_at: None,
            resolution: None,
            resolved_by_name: None,
            note: String::new(),
            source_project_id: None,
            source_restricted: false,
        }
    }

    #[test]
    fn groups_by_dependent_revision_in_first_seen_order() {
        // newest first, as open_for returns them
        let groups = group_by_dependent(vec![
            impact(1, 2, "PRT-000008", 50),
            impact(2, 1, "PRT-000008", 40),
            impact(3, 2, "PRT-000005", 30),
            impact(4, 2, "PRT-000006", 20),
            impact(5, 1, "PRT-000006", 10),
        ]);
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|g| {
                (
                    g.item_number.as_str(),
                    g.impacts.iter().map(|i| i.source_number.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("ASM-000002", vec!["PRT-000005", "PRT-000006", "PRT-000008"]),
                ("ASM-000001", vec!["PRT-000006", "PRT-000008"]),
            ]
        );
        assert_eq!(groups[0].latest_at.format("%M").to_string(), "50");
        assert_eq!(groups[0].revision_id, Uuid::from_u128(2002));
        assert_eq!(
            groups[1].impact_ids,
            format!("{},{}", Uuid::from_u128(5), Uuid::from_u128(2))
        );
        assert_eq!(parse_ids(&groups[1].impact_ids).unwrap().unwrap().len(), 2);
    }

    #[test]
    fn revisions_of_the_same_item_are_separate_groups() {
        let mut older = impact(1, 1, "PRT-000001", 1);
        older.revision_id = Uuid::from_u128(99);
        older.revision_code = "A".into();
        let mut newer = impact(2, 1, "PRT-000002", 2);
        newer.revision_code = "B".into();
        let groups = group_by_dependent(vec![newer, older]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].revision_code, "B");
        assert!(group_by_dependent(Vec::new()).is_empty());
    }

    #[test]
    fn parses_impact_id_lists() {
        assert_eq!(parse_ids("").unwrap(), None);
        assert_eq!(parse_ids(" , ").unwrap(), None);
        let id = Uuid::from_u128(7);
        assert_eq!(parse_ids(&format!(" {id} ,")).unwrap(), Some(vec![id]));
        assert!(parse_ids("nope").is_err());
    }
}
