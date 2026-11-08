//! Revisions: attributes, check-out/check-in, revise and obsolete.

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, LifecycleState, NotificationKind, Role, WebhookEvent},
    error::{AppError, AppResult},
    models::Revision,
    services::{
        access, as_built, attributes, audit, impacts,
        notifications::{self, Notice},
        project_policy, references, simulations, webhooks,
    },
};

pub(crate) const REVISION_SELECT: &str = "
    SELECT r.id, r.item_id, i.number AS item_number, i.item_type, i.unit,
           i.project_id, rp.code AS project_code,
           r.seq, r.code, r.state, r.name, r.description, r.material,
           r.mass_kg, r.unit_cost, r.attributes, i.classification,
           r.checked_out_by, co.display_name AS checked_out_by_name, r.checked_out_at,
           r.created_by, cb.display_name AS created_by_name,
           r.created_at, r.updated_at, r.released_at
    FROM revisions r
    JOIN items i ON i.id = r.item_id
    JOIN users cb ON cb.id = r.created_by
    LEFT JOIN users co ON co.id = r.checked_out_by
    LEFT JOIN projects rp ON rp.id = i.project_id";

pub fn state_of(rev: &Revision) -> LifecycleState {
    LifecycleState::parse(&rev.state).unwrap_or(LifecycleState::Obsolete)
}

/// Human readable label such as `PRT-000042/B`.
pub fn label(rev: &Revision) -> String {
    format!("{}/{}", rev.item_number, rev.code)
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<Revision> {
    sqlx::query_as::<_, Revision>(&format!("{REVISION_SELECT} WHERE r.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Revision not found."))
}

pub(crate) async fn get_by_code_unchecked(
    pool: &PgPool,
    item_id: Uuid,
    code: &str,
) -> AppResult<Revision> {
    sqlx::query_as::<_, Revision>(&format!(
        "{REVISION_SELECT} WHERE r.item_id = $1 AND r.code = $2"
    ))
    .bind(item_id)
    .bind(code.trim().to_ascii_uppercase())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found(format!("Revision '{code}' not found.")))
}

/// Loads a revision and locks its row until the transaction ends.
pub(crate) async fn lock(conn: &mut PgConnection, id: Uuid) -> AppResult<Revision> {
    sqlx::query_as::<_, Revision>(&format!(
        "{REVISION_SELECT} WHERE r.id = $1 FOR UPDATE OF r"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::not_found("Revision not found."))
}

/// A revision the actor may read; hidden ones answer like missing ones.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Revision> {
    let rev = get_unchecked(pool, id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    Ok(rev)
}

/// A revision of an item by its code (`A`, `B`, …), if the actor may read it.
pub async fn get_by_code(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    code: &str,
) -> AppResult<Revision> {
    let rev = get_by_code_unchecked(pool, item_id, code).await?;
    access::ensure_visible(
        actor,
        rev.project_id,
        AppError::not_found(format!("Revision '{code}' not found.")),
    )?;
    Ok(rev)
}

/// Revisions currently checked out by the actor (only those it may still
/// read: a user who left a restricted group asks a manager of the group or
/// an admin to cancel them, see [`cancel_checkout`]).
pub async fn checked_out_by(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Revision>> {
    Ok(sqlx::query_as::<_, Revision>(&format!(
        "{REVISION_SELECT} WHERE r.checked_out_by = $1 AND {}
         ORDER BY r.checked_out_at DESC",
        access::item_visible("i", 2)
    ))
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Fails unless `actor` may change data of the revision's project:
/// CAD editor (or admin) and member of the item's project group. A revision
/// the actor may not even read answers "not found", so the message below
/// (which names the group) never reaches outsiders of restricted groups.
pub fn ensure_project_access(rev: &Revision, actor: &Actor) -> AppResult<()> {
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    actor.require(Role::Editor)?;
    if actor.can_edit_project(rev.project_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "Only members of the project group {} can modify {}.",
            rev.project_code.as_deref().unwrap_or("?"),
            label(rev)
        )))
    }
}

/// Fails unless `actor` could work on `rev`: project access, revision in work
/// and not checked out by somebody else (the precondition for checking out).
pub fn ensure_checkout_possible(rev: &Revision, actor: &Actor) -> AppResult<()> {
    ensure_project_access(rev, actor)?;
    let state = state_of(rev);
    if !state.is_editable() {
        return Err(AppError::Conflict(format!(
            "{} is {} and cannot be modified. Create a new revision instead.",
            label(rev),
            state.label()
        )));
    }
    if let Some(owner) = rev.checked_out_by {
        if owner != actor.id {
            return Err(AppError::Conflict(format!(
                "{} is checked out by {}.",
                label(rev),
                rev.checked_out_by_name.as_deref().unwrap_or("another user")
            )));
        }
    }
    Ok(())
}

/// Fails unless `actor` may modify `rev` right now: everything of
/// [`ensure_checkout_possible`] **and** the revision is checked out by the
/// actor. Every change to attributes, files and BOM requires the lock.
pub fn ensure_editable(rev: &Revision, actor: &Actor) -> AppResult<()> {
    ensure_checkout_possible(rev, actor)?;
    if rev.checked_out_by != Some(actor.id) {
        return Err(AppError::Conflict(format!(
            "Check out {} before changing it.",
            label(rev)
        )));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RevisionUpdate {
    pub name: String,
    pub description: String,
    pub material: String,
    pub mass_kg: Option<f64>,
    pub unit_cost: Option<f64>,
    /// Custom attribute changes, `key → value as text`; an empty text removes
    /// the value, keys not listed keep theirs (see `services::attributes`).
    pub attributes: Vec<(String, String)>,
}

impl RevisionUpdate {
    pub(crate) fn validated(self) -> AppResult<RevisionUpdate> {
        Ok(RevisionUpdate {
            name: domain::require_text("Name", &self.name, 200)?,
            description: domain::limit_text("Description", &self.description, 10_000)?,
            material: domain::limit_text("Material", &self.material, 200)?,
            mass_kg: domain::validate_non_negative("Mass", self.mass_kg)?,
            unit_cost: domain::validate_non_negative("Unit cost", self.unit_cost)?,
            attributes: self.attributes,
        })
    }
}

/// Changes name, description, material, mass, cost and custom attributes of a revision.
///
/// # Invariants
///
/// * **Permission:** the revision is visible ("not found" otherwise), checked out by the actor
///   (409 otherwise) and editable (`revisions::ensure_editable`: in work, CAD editor of its
///   group).
/// * **Locks:** the revision row (`revisions::lock`).
/// * **Audit:** `revision.update`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    upd: RevisionUpdate,
) -> AppResult<Revision> {
    let upd = upd.validated()?;
    let mut tx = pool.begin().await?;
    let rev = lock(&mut tx, id).await?;
    ensure_editable(&rev, actor)?;
    let attributes =
        attributes::apply_changes(&mut tx, &rev.item_type, &rev.attributes, &upd.attributes)
            .await?;
    sqlx::query(
        "UPDATE revisions SET name = $2, description = $3, material = $4, mass_kg = $5,
                              unit_cost = $6, attributes = $7, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&upd.name)
    .bind(&upd.description)
    .bind(&upd.material)
    .bind(upd.mass_kg)
    .bind(upd.unit_cost)
    .bind(&attributes)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "revision.update",
        "revision",
        Some(id),
        format!("Updated attributes of {}", label(&rev)),
        json!({
            "item_id": rev.item_id,
            "name": upd.name,
            "material": upd.material,
            "mass_kg": upd.mass_kg,
            "unit_cost": upd.unit_cost,
            "attributes": attributes,
        }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Takes the exclusive edit lock of an in-work revision.
///
/// # Invariants
///
/// * **Permission:** CAD editor; the revision is visible ("not found" otherwise), in a group the
///   actor may edit (403), in work and not checked out by someone else (409).
/// * **Locks:** the revision row (`revisions::lock`); two check-outs at once give one winner and
///   a 409.
/// * **Audit:** `revision.checkout`; nothing when the actor already holds the check-out.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn checkout(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Revision> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let rev = lock(&mut tx, id).await?;
    ensure_checkout_possible(&rev, actor)?;
    if rev.checked_out_by == Some(actor.id) {
        return Ok(rev);
    }
    sqlx::query("UPDATE revisions SET checked_out_by = $2, checked_out_at = now() WHERE id = $1")
        .bind(id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "revision.checkout",
        "revision",
        Some(id),
        format!("Checked out {}", label(&rev)),
        json!({ "item_id": rev.item_id }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Releases the edit lock without uploading anything ("cancel check-out").
/// Administrators may release locks of other users, managers of the item's
/// group those of users who are gone: no longer members (who cannot release
/// them themselves once a restricted group hides the revision) or
/// deactivated. A check-in with new file versions is `files::check_in`;
/// the bulk release for one holder is `checkouts::release_held`.
///
/// # Invariants
///
/// * **Permission:** CAD editor; the revision is visible ("not found" otherwise). Nothing happens
///   when it is not checked out. Another user's check-out may be released only by an
///   administrator or by a manager of the group when that user is no longer a member or is
///   deactivated (403 otherwise).
/// * **Locks:** the revision row (`revisions::lock`).
/// * **Audit:** `revision.checkout_cancel`; `revision.checkout_forced_release` when the check-out
///   was held by someone else.
/// * **Notifications:** on a forced release the former holder; otherwise none.
/// * **Webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn cancel_checkout(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Revision> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let rev = lock(&mut tx, id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    let Some(owner) = rev.checked_out_by else {
        return Ok(rev);
    };
    if owner != actor.id && !actor.is_admin() && !orphaned_lock(&mut tx, actor, &rev, owner).await?
    {
        return Err(AppError::Forbidden(format!(
            "{} is checked out by {}; only they or an administrator can check it in.",
            label(&rev),
            rev.checked_out_by_name.as_deref().unwrap_or("another user")
        )));
    }
    sqlx::query("UPDATE revisions SET checked_out_by = NULL, checked_out_at = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let forced = owner != actor.id;
    audit::record(
        &mut tx,
        Some(actor),
        if forced {
            "revision.checkout_forced_release"
        } else {
            "revision.checkout_cancel"
        },
        "revision",
        Some(id),
        if forced {
            format!(
                "Released check-out of {} held by {}",
                label(&rev),
                rev.checked_out_by_name.as_deref().unwrap_or("?")
            )
        } else {
            format!("Cancelled the check-out of {}", label(&rev))
        },
        json!({ "item_id": rev.item_id }),
    )
    .await?;
    if forced {
        let notice = Notice::revision(
            NotificationKind::Unlock,
            &rev,
            format!(
                "{} released your check-out of {}",
                actor.display_name,
                label(&rev)
            ),
            "Files you changed in your workspace stay there; check the revision out again to \
             continue."
                .into(),
        );
        notifications::notify(&mut tx, Some(actor), &[owner], &notice).await?;
    }
    tx.commit().await?;
    get(pool, actor, id).await
}

/// For pages: whether [`cancel_checkout`] would release the revision's lock.
pub async fn may_cancel_checkout(pool: &PgPool, actor: &Actor, rev: &Revision) -> AppResult<bool> {
    let Some(owner) = rev.checked_out_by else {
        return Ok(false);
    };
    if !actor.can_edit() {
        return Ok(false);
    }
    if owner == actor.id || actor.is_admin() {
        return Ok(true);
    }
    let mut conn = pool.acquire().await?;
    orphaned_lock(&mut conn, actor, rev, owner).await
}

/// A lock that a manager of the revision's group may release: its owner is
/// gone – no longer a member of the group (removed, or never one before the
/// group was restricted), so they can neither see nor check in the revision
/// of a restricted group nor edit it in an internal one, or deactivated, so
/// they cannot sign in to release it at all (issue #173: a deactivated member
/// keeps their memberships, IdP-synchronised ones included).
async fn orphaned_lock(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    owner: Uuid,
) -> AppResult<bool> {
    let Some(project_id) = rev.project_id else {
        return Ok(false);
    };
    if !crate::services::projects::can_manage(conn, actor, project_id).await? {
        return Ok(false);
    }
    let owner_is_active_member = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM project_members pm JOIN users u ON u.id = pm.user_id
                       WHERE pm.project_id = $1 AND pm.user_id = $2 AND u.active)",
    )
    .bind(project_id)
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    Ok(!owner_is_active_member)
}

/// Tells the watchers of an item about something that happened to a revision.
pub(crate) async fn tell_watchers(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    title: String,
    body: String,
) -> AppResult<()> {
    let watchers = notifications::watchers(conn, "item", &[rev.item_id]).await?;
    let notice = Notice::revision(NotificationKind::ItemUpdate, rev, title, body);
    notifications::notify(conn, Some(actor), &watchers, &notice).await?;
    Ok(())
}

/// Creates the next revision of an item, copying attributes, files and BOM
/// lines from revision `id`. Only possible when the item has no open revision.
///
/// # Invariants
///
/// * **Permission:** CAD editor; the source revision is visible ("not found" otherwise) and in a
///   group the actor may edit (403); the item has no revision in work or in review (409).
/// * **Locks:** the source revision row (`revisions::lock`), then the item row (`FOR UPDATE`),
///   which serialises concurrent revisions of the same item.
/// * **Audit:** `revision.revise`.
/// * **Notifications:** the watchers of the item.
/// * **Webhooks:** none.
/// * **Data:** the new revision is checked out by the actor and takes over attributes, files
///   (as version 1), references, simulations and BOM lines.
/// * **Atomic:** one transaction.
pub async fn revise(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Revision> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let src = lock(&mut tx, id).await?;
    ensure_project_access(&src, actor)?;
    // Serialise concurrent revise operations on the same item.
    sqlx::query("SELECT id FROM items WHERE id = $1 FOR UPDATE")
        .bind(src.item_id)
        .execute(&mut *tx)
        .await?;
    let open = sqlx::query_scalar::<_, String>(
        "SELECT code FROM revisions WHERE item_id = $1 AND state IN ('in_work', 'in_review')",
    )
    .bind(src.item_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(code) = open {
        return Err(AppError::Conflict(format!(
            "{} already has the open revision {code}; release or finish it first.",
            src.item_number
        )));
    }
    let max_seq = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(seq), 0) FROM revisions WHERE item_id = $1",
    )
    .bind(src.item_id)
    .fetch_one(&mut *tx)
    .await?;
    let seq = max_seq + 1;
    let code = domain::revision_code(seq);
    let new_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO revisions (id, item_id, seq, code, state, name, description, material,
                                mass_kg, unit_cost, attributes, created_by, checked_out_by, checked_out_at)
         SELECT $2, item_id, $3, $4, 'in_work', name, description, material, mass_kg, unit_cost, attributes, $5, $5, now()
         FROM revisions WHERE id = $1",
    )
    .bind(src.id)
    .bind(new_id)
    .bind(seq)
    .bind(&code)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    // The new revision starts with the current version of every file as v1.
    let files = sqlx::query(
        "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by, uploaded_at,
                            format, folder, retention)
         SELECT gen_random_uuid(), $2, filename, content_type, size_bytes, sha256, uploaded_by, uploaded_at, format,
                folder, retention
         FROM files WHERE revision_id = $1",
    )
    .bind(src.id)
    .bind(new_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                                    uploaded_by, uploaded_at, content_text, references_scanned,
                                    export_name, export_name_scanned, solver_summary)
         SELECT gen_random_uuid(), f.id, 1, f.sha256, f.size_bytes, f.content_type,
                'Taken over from revision ' || $2, f.uploaded_by, f.uploaded_at,
                COALESCE(ov.content_text, ''), COALESCE(ov.references_scanned, FALSE), ov.export_name,
                COALESCE(ov.export_name_scanned, FALSE), ov.solver_summary
         FROM files f
         LEFT JOIN files ofl ON ofl.revision_id = $3 AND ofl.filename = f.filename
         LEFT JOIN file_versions ov ON ov.file_id = ofl.id AND ov.version = ofl.current_version
         WHERE f.revision_id = $1",
    )
    .bind(new_id)
    .bind(&src.code)
    .bind(src.id)
    .execute(&mut *tx)
    .await?;
    // The CAD file references of the current versions come along.
    sqlx::query(
        "INSERT INTO file_references (file_version_id, name)
         SELECT nv.id, fr.name
         FROM files nf
         JOIN file_versions nv ON nv.file_id = nf.id
         JOIN files ofl ON ofl.revision_id = $2 AND ofl.filename = nf.filename
         JOIN file_versions ov ON ov.file_id = ofl.id AND ov.version = ofl.current_version
         JOIN file_references fr ON fr.file_version_id = ov.id
         WHERE nf.revision_id = $1",
    )
    .bind(new_id)
    .bind(src.id)
    .execute(&mut *tx)
    .await?;
    let references = references::copy_to_revision(&mut tx, src.id, new_id).await?;
    let (simulation_bases, simulation_results) =
        simulations::copy_to_revision(&mut tx, src.id, new_id).await?;
    let (as_built_records, as_built_deviations) =
        as_built::copy_to_revision(&mut tx, src.id, new_id).await?;
    let lines = sqlx::query(
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes)
         SELECT gen_random_uuid(), $2, child_item_id, find_number, quantity, notes
         FROM bom_lines WHERE parent_revision_id = $1",
    )
    .bind(src.id)
    .bind(new_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    audit::record(
        &mut tx,
        Some(actor),
        "revision.revise",
        "revision",
        Some(new_id),
        format!(
            "Created revision {}/{code} from {}",
            src.item_number, src.code
        ),
        json!({ "item_id": src.item_id, "from": src.code, "files": files, "bom_lines": lines,
                "references": references, "simulation_bases": simulation_bases,
                "simulation_results": simulation_results,
                "as_built_records": as_built_records, "as_built_deviations": as_built_deviations }),
    )
    .await?;
    let new_rev = lock(&mut tx, new_id).await?;
    tell_watchers(
        &mut tx,
        actor,
        &new_rev,
        format!("New revision {}", label(&new_rev)),
        format!(
            "{} started revision {code} of {} ({}) from {}.",
            actor.display_name, src.item_number, src.name, src.code
        ),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, new_id).await
}

/// Marks a released revision as obsolete.
///
/// # Invariants
///
/// * **Permission:** CAD editor; the revision is visible ("not found" otherwise) and in a group
///   the actor may edit (403); it is released (409 otherwise), and its group does not require a
///   change order for it (`obsolete_requires_change`, 409).
/// * **Locks:** the revision row (`revisions::lock`).
/// * **Audit:** `revision.obsolete`; when it is the latest revision of its item, also
///   `impact.flag` for its dependents (`impacts::on_obsolete`).
/// * **Notifications:** the watchers of the item; when the latest revision became obsolete also
///   the people responsible for the flagged dependents.
/// * **Webhooks:** `revision.obsoleted`.
/// * **Data:** when it is the latest revision, the open impacts on the item are resolved and
///   its dependents are flagged.
/// * **Atomic:** one transaction.
pub async fn obsolete(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Revision> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let rev = lock(&mut tx, id).await?;
    ensure_project_access(&rev, actor)?;
    if project_policy::obsolete_requires_change(&mut tx, rev.project_id).await? {
        return Err(AppError::Conflict(format!(
            "{} is in project group {}, which requires a change order to obsolete a revision: add an obsolete line to a change order.",
            label(&rev),
            rev.project_code.as_deref().unwrap_or("?")
        )));
    }
    obsolete_in(&mut tx, actor, &rev).await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// The obsoleting itself, in the caller's transaction: used by [`obsolete`] and when a
/// change order with an obsolete line is released. The revision row must be locked and the
/// permission checked by the caller.
pub(crate) async fn obsolete_in(
    tx: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
) -> AppResult<()> {
    let id = rev.id;
    let rev = rev.clone();
    transition(&mut *tx, &rev, LifecycleState::Obsolete).await?;
    // Dependents of an item without a current revision have to replace it.
    impacts::on_obsolete(tx, actor, &rev).await?;
    audit::record(
        tx,
        Some(actor),
        "revision.obsolete",
        "revision",
        Some(id),
        format!("Marked {} as obsolete", label(&rev)),
        json!({ "item_id": rev.item_id }),
    )
    .await?;
    tell_watchers(
        tx,
        actor,
        &rev,
        format!("{} is obsolete", label(&rev)),
        format!("{} marked {} as obsolete.", actor.display_name, rev.name),
    )
    .await?;
    webhooks::emit(
        tx,
        Some(actor),
        WebhookEvent::RevisionObsoleted,
        &[rev.project_id],
        json!({ "revision": webhooks::revision_object(&rev, LifecycleState::Obsolete.as_str()) }),
    )
    .await?;
    Ok(())
}

/// Moves a (locked) revision to another lifecycle state after validating
/// the transition. Entering review clears any check-out.
pub(crate) async fn transition(
    conn: &mut PgConnection,
    rev: &Revision,
    to: LifecycleState,
) -> AppResult<()> {
    let from = state_of(rev);
    if !from.can_transition_to(to) {
        return Err(AppError::Conflict(format!(
            "{} cannot go from {} to {}.",
            label(rev),
            from.label(),
            to.label()
        )));
    }
    sqlx::query(
        "UPDATE revisions SET state = $2, updated_at = now(),
             released_at = CASE WHEN $2 = 'released' THEN now() ELSE released_at END,
             checked_out_by = CASE WHEN $2 = 'in_work' THEN checked_out_by ELSE NULL END,
             checked_out_at = CASE WHEN $2 = 'in_work' THEN checked_out_at ELSE NULL END
         WHERE id = $1",
    )
    .bind(rev.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    Ok(())
}
