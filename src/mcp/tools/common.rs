//! Compact views and lookups shared by the tools.

use super::*;

pub(super) fn rev_brief(r: &Revision) -> Value {
    json!({
        "item": r.item_number,
        "revision": r.code,
        "name": r.name,
        "type": r.item_type,
        "state": r.state,
        "project": r.project_code,
        "checked_out_by": r.checked_out_by_name,
    })
}

pub(super) fn rev_full(r: &Revision) -> Value {
    json!({
        "item": r.item_number,
        "revision": r.code,
        "name": r.name,
        "type": r.item_type,
        "unit": r.unit,
        "state": r.state,
        "project": r.project_code,
        "description": r.description,
        "material": r.material,
        "mass_kg": r.mass_kg,
        "unit_cost": r.unit_cost,
        "classification": r.classification,
        "attributes": r.attributes,
        "checked_out_by": r.checked_out_by_name,
        "checked_out_at": r.checked_out_at,
        "created_by": r.created_by_name,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "released_at": r.released_at,
    })
}

pub(super) fn change_brief(c: &Change) -> Value {
    json!({
        "number": c.number,
        "title": c.title,
        "status": c.status,
        "priority": c.priority,
        "created_by": c.created_by_name,
        "created_at": c.created_at,
        "updated_at": c.updated_at,
        "submitted_at": c.submitted_at,
        "closed_at": c.closed_at,
    })
}

pub(super) fn file_brief(f: &FileRecord) -> Value {
    let info = f.format_info.as_ref();
    json!({
        "filename": f.filename,
        "format": info.map(|i| i.label),
        "cad_program": info.map(|i| i.system_name),
        "kind": info.map(|i| i.kind_label),
        "version": f.version,
        "size_bytes": f.size_bytes,
        "sha256": f.sha256,
        "uploaded_by": f.uploaded_by_name,
        "uploaded_at": f.uploaded_at,
    })
}

pub(super) fn bom_line_brief(l: &BomLine) -> Value {
    json!({
        "find_number": l.find_number,
        "item": l.child_number,
        "name": l.child_name,
        "type": l.child_type,
        "quantity": l.quantity,
        "unit": l.child_unit,
        "latest_revision": l.child_latest_code,
        "latest_state": l.child_latest_state,
        "released_with": l.released_child_code,
        "notes": l.notes,
        "restricted": l.restricted,
    })
}

pub(super) fn web_url(st: &AppState, path: &str) -> String {
    format!("{}{path}", st.config.public_url)
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// A revision by item number and code; `None` or `latest` = the newest.
pub(super) async fn revision_at(
    st: &AppState,
    me: &Actor,
    number: &str,
    code: Option<&str>,
) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, me, number).await?;
    match code {
        None => items::latest_revision(&st.db, me, item.id).await,
        Some(c) if c.eq_ignore_ascii_case("latest") => {
            items::latest_revision(&st.db, me, item.id).await
        }
        Some(c) => revisions::get_by_code(&st.db, me, item.id, c).await,
    }
}

pub(super) async fn revision_arg(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Revision> {
    let number = a.req_str("item")?;
    revision_at(st, me, &number, a.opt_str("revision")?.as_deref()).await
}

/// The revision a change order should carry: the given code, or the item's
/// in-work revision.
pub(super) async fn change_revision(
    st: &AppState,
    me: &Actor,
    reference: &str,
) -> AppResult<Revision> {
    let (number, code) = parse_item_ref(reference);
    match code {
        Some(code) => revision_at(st, me, &number, Some(&code)).await,
        None => {
            let item = items::get_by_number(&st.db, me, &number).await?;
            items::open_revision(&st.db, me, item.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!(
                        "{} has no in-work revision; revise it first.",
                        item.number
                    ))
                })
        }
    }
}

/// Default rule: as released for released (and obsolete) revisions.
pub(super) fn default_rule(rev: &Revision) -> RevisionRule {
    if matches!(rev.state.as_str(), "released" | "obsolete") {
        RevisionRule::AsReleased
    } else {
        RevisionRule::Latest
    }
}
