//! Revision page: attributes, lifecycle actions, files.

use axum::{
    body::Body,
    extract::{multipart::MultipartError, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use super::{finish, items::BackForm, render, WebCtx};
use crate::{
    auth::{Actor, WebUser},
    cadformat::FormatKind,
    domain::{
        self, BasisRole, DeviationKind, Disposition, ItemType, LifecycleState, ReferenceKind,
        COMMON_UNITS,
    },
    error::{AppError, AppResult},
    httprange,
    models::{AttributeDef, FileRecord, Revision},
    preview::PreviewFormat,
    services::{
        access_log::{self, AccessCtx},
        as_built, attributes, audit, bom, changes, files, impacts, interchange, items,
        notifications, ownership, projects, references, requests, result_proposals, revisions,
        similar, simulations, uploads,
    },
    thumbnail, util, AppState,
};

/// Logs a file download when the file's group keeps a read log.
async fn log_file(
    st: &AppState,
    log: &AccessCtx,
    me: &Actor,
    file_id: Uuid,
    filename: &str,
) -> AppResult<()> {
    let project_id = access_log::project_of_file(&st.db, file_id).await?;
    access_log::record(
        &st.db,
        log,
        me,
        access_log::Subject {
            kind: access_log::Kind::Download,
            project_id,
            object_type: "file",
            object_id: Some(file_id),
            label: filename,
            detail: "",
        },
    )
    .await
}

/// A stored attribute value as text for display and form fields
/// (`true` → `yes`, `12.0` → `12`).
fn attribute_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(true) => "yes".into(),
        serde_json::Value::Bool(false) => "no".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A stored attribute value for the revision page (`Yes`/`No`).
fn attribute_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(true) => "Yes".into(),
        serde_json::Value::Bool(false) => "No".into(),
        other => attribute_text(other),
    }
}

/// The custom attribute values of a revision for its page: every stored
/// value with the label and unit of its definition, in definition order.
fn attribute_rows(defs: &[AttributeDef], values: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(values) = values.as_object() else {
        return Vec::new();
    };
    let mut rows: Vec<serde_json::Value> = defs
        .iter()
        .filter_map(|d| {
            values.get(&d.key).map(|v| {
                json!({ "key": d.key, "label": d.label, "unit": d.unit,
                        "value": attribute_display(v), "active": d.active })
            })
        })
        .collect();
    // values of keys without a definition (should not happen) stay visible
    for (k, v) in values {
        if !defs.iter().any(|d| &d.key == k) {
            rows.push(json!({ "key": k, "label": k, "unit": "", "value": attribute_display(v), "active": false }));
        }
    }
    rows
}

/// Form fields `attr.<key>`: the active attributes of the item type, with
/// the stored (or submitted) value.
async fn attribute_fields(
    st: &AppState,
    rev: &Revision,
    submitted: Option<&std::collections::HashMap<String, String>>,
) -> AppResult<Vec<serde_json::Value>> {
    let defs = attributes::for_item_type(&st.db, &rev.item_type).await?;
    Ok(defs
        .iter()
        .map(|d| {
            let value = submitted
                .and_then(|s| s.get(&format!("attr.{}", d.key)).cloned())
                .unwrap_or_else(|| {
                    rev.attributes
                        .get(&d.key)
                        .map(attribute_text)
                        .unwrap_or_default()
                });
            json!({ "key": d.key, "label": d.label, "unit": d.unit, "data_type": d.data_type,
                    "choices": d.choices, "value": value })
        })
        .collect())
}

/// The 3D preview of a file: by its stored format, or by its name while
/// the format is not classified yet.
fn preview_of(file: &FileRecord) -> Option<PreviewFormat> {
    PreviewFormat::of_format(&file.format).or_else(|| PreviewFormat::of(&file.filename))
}

/// Uses listed in the "Where used" card; a standard part can have tens of
/// thousands (the REST API and MCP have the full reports).
const WHERE_USED_SHOWN: usize = 200;

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    log: AccessCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let rev = revisions::get(&st.db, me, id).await?;
    access_log::record_revision(&st.db, &log, me, &rev, access_log::Kind::View).await?;
    let item = items::get(&st.db, me, rev.item_id).await?;
    // independent queries run concurrently
    let (
        all_revisions,
        file_versions,
        file_list,
        lines,
        where_used,
        outgoing,
        incoming,
        file_refs,
        file_referrers,
        item_impacts,
        watching,
        (rev_changes, hidden_changes),
        projects,
        open_requests,
    ) = tokio::try_join!(
        items::revisions(&st.db, me, item.id),
        files::versions_of_revision(&st.db, id),
        files::list(&st.db, me, id),
        bom::lines(&st.db, me, id),
        items::where_used(&st.db, me, item.id, WHERE_USED_SHOWN as i64 + 1),
        // reference tracking and change propagation
        references::outgoing(&st.db, me, id),
        references::incoming(&st.db, me, item.id),
        references::files_of_revision(&st.db, me, id),
        references::file_referrers(&st.db, me, id),
        impacts::for_item(&st.db, me, item.id),
        notifications::is_watching(&st.db, ctx.me.id, "item", item.id),
        changes::for_revision(&st.db, me, id),
        // target groups for moving the item (only groups the user may edit)
        projects::editable_by(&st.db, &ctx.me),
        // change requests (ECR) on the item that are not finished
        requests::open_for_item(&st.db, me, item.id),
    )?;
    let (hidden_parents, hidden_incoming) = tokio::try_join!(
        items::hidden_parents(&st.db, me, item.id),
        references::incoming_hidden(&st.db, me, item.id),
    )?;
    let favorite = crate::services::personal::is_favorite(&st.db, ctx.me.id, item.id).await?;
    // alternates and supersession of the item, superseded children of the BOM
    let child_ids: Vec<Uuid> = lines
        .iter()
        .filter(|l| !l.restricted)
        .map(|l| l.child_item_id)
        .collect();
    let (alternates, alternate_for, successor, predecessors, superseded) = tokio::try_join!(
        interchange::alternates(&st.db, me, item.id),
        interchange::alternate_for(&st.db, me, item.id),
        interchange::successor(&st.db, me, item.id),
        interchange::predecessors(&st.db, me, item.id),
        interchange::superseded_among(&st.db, me, &child_ids),
    )?;
    let superseded: serde_json::Map<String, serde_json::Value> = superseded
        .into_iter()
        .map(|(id, number)| (id.to_string(), json!(number)))
        .collect();
    let interchange = json!({
        "alternates": alternates,
        "alternate_for": alternate_for,
        "successor": successor,
        "predecessors": predecessors,
        "superseded": superseded,
        "can_edit": me.can_edit() && me.can_edit_project(item.project_id),
    });
    // Similar parts by the shape of its 3D model: one indexed query; a
    // missing signature is computed in the background – waited for briefly
    // only when the conversion is done (a quick job from the cached mesh,
    // or its cached failure), never for a conversion, so the page is not
    // held up.
    let wait = match similar::pick_file(&file_list, None) {
        Ok(Some((f, _))) if st.previews.is_converted(&f.sha256).await => {
            std::time::Duration::from_millis(1500)
        }
        _ => std::time::Duration::ZERO,
    };
    let similar_parts = match similar::for_files(
        &st,
        &ctx.me,
        &rev,
        &file_list,
        None,
        similar::Options {
            limit: 6,
            ignore_scale: false,
        },
        wait,
    )
    .await
    {
        Ok(report) => Some(report),
        Err(e) => {
            tracing::warn!(revision = %rev.id, error = %e, "similar part search failed");
            None
        }
    };
    // Native CAD files can only be viewed in 3D through a neutral export.
    let native_without_preview = !file_list.iter().any(|f| preview_of(f).is_some())
        && file_list.iter().any(|f| {
            f.format_info.is_some_and(|i| {
                matches!(
                    i.kind,
                    FormatKind::Part | FormatKind::Assembly | FormatKind::Model
                )
            })
        });
    // Result files the browser can show in 3D (VTK, VTU, VTP, FRD); a few
    // on the Results card (time series have dozens), all in the file list.
    let result_views: Vec<serde_json::Value> = file_list
        .iter()
        .filter(|f| preview_of(f).is_some_and(PreviewFormat::is_result))
        .take(3)
        .map(|f| json!({ "id": f.id, "filename": f.filename, "version": f.version }))
        .collect();
    // Rendered pictures of a simulation (PNG, JPEG, WebP, size-capped).
    let gallery: Vec<serde_json::Value> = file_list
        .iter()
        .filter(|f| simulations::is_gallery_image(&f.filename, f.size_bytes))
        .take(simulations::GALLERY_MAX_IMAGES)
        .map(|f| json!({ "id": f.id, "filename": f.filename, "version": f.version, "sha256": f.sha256 }))
        .collect();
    // The versions this page shows: uploads from its forms are based on them,
    // so a file that changed meanwhile is refused instead of overwritten.
    let file_bases = serde_json::Value::Object(
        file_list
            .iter()
            .map(|f| (f.filename.clone(), json!(f.version)))
            .collect(),
    )
    .to_string();
    // "Fill with AI…": report files to choose from, and whether the button
    // is offered (assistant configured, editor of the group, group has not
    // switched the assistant off).
    let report_files: Vec<String> = file_list
        .iter()
        .filter(|f| crate::services::assistant::is_report_file(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    let can_fill_with_ai = crate::services::assistant::enabled(&st)
        && me.can_edit()
        && me.can_edit_project(rev.project_id)
        && match rev.project_id {
            Some(p) => !crate::services::project_policy::denied_groups(
                &st.db,
                &[crate::services::project_policy::Interface::Assistant],
            )
            .await?
            .contains(&p),
            None => true,
        };
    // Every file together with its version history (newest first).
    let attachments: Vec<serde_json::Value> = file_list
        .into_iter()
        .map(|f| {
            let versions: Vec<_> = file_versions.iter().filter(|v| v.file_id == f.id).collect();
            let preview = preview_of(&f).is_some();
            let mut value = json!(f);
            value["versions"] = json!(versions);
            value["preview"] = json!(preview);
            value["used_by"] = json!(used_by_warning(&f.filename, &file_referrers));
            value
        })
        .collect();
    let mut entity_ids: Vec<Uuid> = all_revisions.iter().map(|r| r.id).collect();
    entity_ids.push(item.id);
    let history = audit::for_entities(&st.db, me, &entity_ids, 50).await?;
    let mut where_used = where_used;
    let where_used_more = where_used.len() > WHERE_USED_SHOWN;
    where_used.truncate(WHERE_USED_SHOWN);

    let state = revisions::state_of(&rev);
    // editable = checked out by me; can_work = could check out / add to a change
    let editable = revisions::ensure_editable(&rev, me).is_ok();
    let can_work = revisions::ensure_checkout_possible(&rev, me).is_ok();
    let item_has_open_revision = all_revisions
        .iter()
        .any(|r| revisions::state_of(r).is_open());
    let open_change = rev_changes
        .iter()
        .find(|c| changes::status_of(c).is_open())
        .cloned();
    let drafts = if can_work && open_change.is_none() {
        changes::drafts(&st.db, me)
            .await?
            .into_iter()
            .filter(|c| c.created_by == me.id || me.is_admin())
            .collect()
    } else {
        Vec::new()
    };
    let in_project = revisions::ensure_project_access(&rev, me).is_ok();
    let owner_manageable = match item.project_id {
        Some(p) => projects::is_manager(&st.db, me, p).await?,
        None => me.is_admin(),
    };
    let owner_candidates = if owner_manageable {
        ownership::owner_candidates(&st.db, item.project_id).await?
    } else {
        Vec::new()
    };
    let can = json!({
        "edit": editable,
        "checkout": can_work && rev.checked_out_by.is_none(),
        "checkin": rev.checked_out_by == Some(me.id),
        "cancel_checkout": revisions::may_cancel_checkout(&st.db, me, &rev).await?,
        "revise": in_project && !item_has_open_revision,
        "obsolete": in_project && state == LifecycleState::Released,
        "delete": ((item.created_by == me.id && in_project) || me.is_admin())
            && all_revisions.len() == 1
            && state == LifecycleState::InWork
            && where_used.is_empty()
            && hidden_parents == 0
            && rev_changes.is_empty()
            && hidden_changes == 0,
        "master": in_project,
        "owner": owner_manageable,
        "add_to_change": can_work && open_change.is_none(),
        "acknowledge": me.can_edit() && me.can_edit_project(item.project_id),
    });
    // CAD files of this revision that lead to other items (assemblies and
    // NX models): offers "Create BOM from CAD references"
    let cad_structure = file_refs.iter().any(|f| {
        matches!(
            f.format_kind,
            Some(crate::cadformat::FormatKind::Assembly | crate::cadformat::FormatKind::Model)
        ) && f
            .references
            .iter()
            .any(|r| r.matches.iter().any(|m| !m.same_revision))
    });
    let files_hint = domain::files_hint(
        state,
        rev.checked_out_by,
        me.id,
        editable,
        can_work,
        in_project && !item_has_open_revision,
    );
    let suggestions = if editable {
        let children: Vec<String> = lines.iter().map(|l| l.child_number.clone()).collect();
        references::suggestions(&file_refs, &children, &outgoing)
    } else {
        references::Suggestions::default()
    };
    let (open_impacts, resolved_impacts): (Vec<_>, Vec<_>) = item_impacts
        .into_iter()
        .partition(|i| i.resolved_at.is_none());
    let reference_kinds: Vec<_> = ReferenceKind::ALL
        .iter()
        .map(|k| json!({ "value": k.as_str(), "label": k.label() }))
        .collect();
    let latest_id = all_revisions.first().map(|r| r.id);
    // Simulations: basis links and result values of a simulation revision;
    // for every other item the simulations based on it.
    let is_simulation = rev.item_type == ItemType::Simulation.as_str();
    let simulation = if is_simulation {
        let (bases, results, proposals) = tokio::try_join!(
            simulations::bases(&st.db, me, id),
            simulations::results(&st.db, me, id),
            result_proposals::list(&st.db, me, id, Some("pending"))
        )?;
        let out_of_date = bases.iter().filter(|b| b.out_of_date.is_some()).count();
        let roles: Vec<_> = BasisRole::ALL
            .iter()
            .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
            .collect();
        json!({
            "summary": simulations::summarize(&results),
            "bases": bases,
            "results": results,
            "out_of_date": out_of_date,
            "roles": roles,
            "views": result_views,
            "images": gallery,
            "proposals": proposals,
            "can_decide": me.can_edit() && me.can_edit_project(rev.project_id),
            "can_fill_with_ai": can_fill_with_ai,
            "report_files": report_files,
        })
    } else {
        serde_json::Value::Null
    };
    let (based_simulations, hidden_simulations) = if is_simulation {
        (Vec::new(), 0)
    } else {
        simulations::based_on_item(&st.db, me, item.id).await?
    };
    // As-built: the build record and deviations of an as-built revision;
    // for a design (part, assembly, software) the units built to it.
    let as_built = if rev.item_type == ItemType::AsBuilt.as_str() {
        let (record, deviations, concessions) = tokio::try_join!(
            as_built::record(&st.db, me, id),
            as_built::deviations(&st.db, me, id),
            as_built::concession_review(&st.db, me, &rev)
        )?;
        json!({
            "record": record,
            "summary": as_built::summarize(&deviations),
            "deviations": deviations,
            "concessions": concessions,
            "kinds": DeviationKind::ALL
                .iter()
                .map(|k| json!({ "value": k.as_str(), "label": k.label() }))
                .collect::<Vec<_>>(),
            "dispositions": Disposition::ALL
                .iter()
                .map(|d| json!({ "value": d.as_str(), "label": d.label() }))
                .collect::<Vec<_>>(),
        })
    } else {
        serde_json::Value::Null
    };
    let can_be_built_to = ItemType::parse(&rev.item_type).is_some_and(ItemType::can_be_built_to);
    let (built_units, hidden_units) = if can_be_built_to {
        as_built::built_units(&st.db, me, item.id).await?
    } else {
        (Vec::new(), 0)
    };
    let rev_attributes = rev.attributes.clone();
    render(
        &st,
        &ctx,
        "revisions/show.html",
        json!({
            "rev": rev,
            "item": item,
            "revisions": all_revisions,
            "is_latest": latest_id == Some(rev.id),
            "latest_id": latest_id,
            "file_groups": super::file_groups(&me.preferred_cad, &attachments),
            "files": attachments,
            "file_bases": file_bases,
            "native_without_preview": native_without_preview,
            "similar": similar_parts,
            "lines": lines,
            "cad_structure": cad_structure,
            "where_used": where_used,
            "interchange": interchange,
            "hidden": {
                "parents": hidden_parents,
                "incoming": hidden_incoming,
                "changes": hidden_changes,
                "simulations": hidden_simulations,
                "units": hidden_units,
            },
            "references": {
                "outgoing": outgoing,
                "incoming": incoming,
                "files": file_refs,
                "file_referrers": file_referrers,
                "suggestions": suggestions,
                "kinds": reference_kinds,
            },
            "impact_groups": impacts::group_by_dependent(open_impacts),
            "files_hint": {
                "kind": files_hint.as_str(),
                "can_revise": matches!(files_hint, domain::FilesHint::Frozen { can_revise: true }),
                "type_hint": domain::files_type_hint(&rev.item_type),
            },
            "simulation": simulation,
            "based_simulations": based_simulations,
            "as_built": as_built,
            "built_units": built_units,
            "can_be_built_to": can_be_built_to,
            "watching": watching,
            "favorite": favorite,
            "resolved_impacts": resolved_impacts,
            "changes": rev_changes,
            "open_requests": open_requests,
            "open_change": open_change,
            "drafts": drafts,
            "history": history,
            "owner_candidates": owner_candidates,
            "projects": projects,
            "units": COMMON_UNITS,
            "where_used_more": where_used_more,
            "attribute_rows": attribute_rows(&attributes::list(&st.db, false).await?, &rev_attributes),
            "classifications": domain::Classification::ALL
                .iter()
                .map(|c| json!({ "value": c.as_str(), "label": c.label() }))
                .collect::<Vec<_>>(),
            "can": can,
            "max_upload_mb": st.config.max_upload_bytes / (1024 * 1024),
            "lifecycle": json!({
                "states": LifecycleState::ALL,
                "next": domain::rules_from(state),
                "rules": domain::LIFECYCLE_RULES,
            }),
        }),
    )
}

pub async fn edit_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &ctx.me, id).await?;
    revisions::ensure_editable(&rev, &ctx.me)?;
    let fields = attribute_fields(&st, &rev, None).await?;
    render(
        &st,
        &ctx,
        "revisions/edit.html",
        json!({ "rev": rev, "error": null, "attribute_fields": fields,
                "materials": super::mass::material_options(&st).await? }),
    )
}

#[derive(Deserialize)]
pub struct RevisionForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    material: String,
    #[serde(default)]
    mass_kg: String,
    #[serde(default)]
    unit_cost: String,
    /// Custom attribute fields `attr.<key>`.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

impl RevisionForm {
    fn attribute_changes(&self) -> Vec<(String, String)> {
        self.extra
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("attr.")
                    .map(|key| (key.to_string(), v.clone()))
            })
            .collect()
    }
}

pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<RevisionForm>,
) -> AppResult<Response> {
    let parsed = (|| -> AppResult<revisions::RevisionUpdate> {
        Ok(revisions::RevisionUpdate {
            name: form.name.clone(),
            description: form.description.clone(),
            material: form.material.clone(),
            mass_kg: domain::parse_decimal("Mass", Some(&form.mass_kg))?,
            unit_cost: domain::parse_decimal("Unit cost", Some(&form.unit_cost))?,
            attributes: form.attribute_changes(),
        })
    })();
    let result = match parsed {
        Ok(upd) => revisions::update(&st.db, &ctx.me, id, upd).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(rev) => Ok(super::redirect_flash(
            &st,
            &format!("/revisions/{id}"),
            super::Flash::success(format!("Saved {}", revisions::label(&rev))),
        )),
        Err(e) if super::is_user_error(&e) => {
            // Re-render the form with the submitted values.
            let mut rev = revisions::get(&st.db, &ctx.me, id).await?;
            rev.name = form.name;
            rev.description = form.description;
            rev.material = form.material;
            let fields = attribute_fields(&st, &rev, Some(&form.extra)).await?;
            let mut page = render(
                &st,
                &ctx,
                "revisions/edit.html",
                json!({ "rev": rev, "error": e.public_message(), "mass_raw": form.mass_kg,
                        "cost_raw": form.unit_cost, "attribute_fields": fields,
                        "materials": super::mass::material_options(&st).await? }),
            )?;
            *page.status_mut() = e.status();
            Ok(page)
        }
        Err(e) => Err(e),
    }
}

/// Where a lock action returns to: `?back=` (the vault explorer) or the revision page.
fn back_of(id: Uuid, query: &BackForm) -> String {
    match query.back.as_deref() {
        Some(back) => util::safe_redirect_target(Some(back)),
        None => format!("/revisions/{id}"),
    }
}

pub async fn checkout(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(query): Query<BackForm>,
) -> AppResult<Response> {
    let back = back_of(id, &query);
    let result = revisions::checkout(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            back.clone(),
            format!("Checked out {}", revisions::label(&rev)),
        )
    })
}

/// Check-in form (multipart): optional new file versions + comment; the files
/// are stored and the lock is released in one transaction.
pub async fn checkin(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(query): Query<BackForm>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = back_of(id, &query);
    let result = async {
        let rev = revisions::get(&st.db, &ctx.me, id).await?;
        if rev.checked_out_by != Some(ctx.me.id) {
            return Err(AppError::Conflict(format!(
                "You have not checked out {}.",
                revisions::label(&rev)
            )));
        }
        let mut received = receive_uploads(&st, &mut multipart).await?;
        // Large files sent before in chunks by app.js (field `uploads`).
        let sessions =
            take_session_uploads(&st, &ctx.me, uploads::Scope::Revision(id), &mut received).await?;
        let result =
            files::check_in(&st.db, &ctx.me, id, &received.uploads, &received.comment).await;
        if result.is_ok() {
            uploads::forget(&st.db, &st.vault, &sessions).await;
            warm_previews(&st, &received.uploads);
        }
        result
    }
    .await;
    finish(&st, result, &back, |(rev, stored)| {
        let changed: Vec<String> = stored
            .iter()
            .filter(|s| s.changed)
            .map(|s| format!("{} v{}", s.filename, s.version))
            .collect();
        let message = if changed.is_empty() {
            format!("Checked in {}", revisions::label(&rev))
        } else {
            format!(
                "Checked in {} with {}",
                revisions::label(&rev),
                changed.join(", ")
            )
        };
        (back.clone(), message)
    })
}

/// Releases the lock without changes (admins: release someone else's lock).
pub async fn cancel_checkout(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(query): Query<BackForm>,
) -> AppResult<Response> {
    let back = back_of(id, &query);
    let result = revisions::cancel_checkout(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            back.clone(),
            format!("Released the check-out of {}", revisions::label(&rev)),
        )
    })
}

pub async fn revise(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
    let result = revisions::revise(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            format!("/revisions/{}", rev.id),
            format!("Created revision {} (in work)", revisions::label(&rev)),
        )
    })
}

pub async fn obsolete(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
    let result = revisions::obsolete(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            back.clone(),
            format!("{} is now obsolete", revisions::label(&rev)),
        )
    })
}

pub(crate) fn multipart_error(e: MultipartError) -> AppError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::PayloadTooLarge("The upload exceeds the configured size limit.".into())
    } else {
        AppError::BadRequest(format!("Upload failed: {}", e.body_text()))
    }
}

/// Files and the comment field of a multipart upload.
#[derive(Default)]
pub(crate) struct Received {
    pub uploads: Vec<files::Upload>,
    pub comment: String,
    /// Every other text field of the form.
    pub fields: std::collections::HashMap<String, String>,
}

/// Streams every `file` field of a multipart request into the vault and
/// collects the `comment` and all other text fields. Nothing is registered in
/// the database yet.
/// Shared with the REST API.
pub(crate) async fn receive_uploads(
    st: &AppState,
    multipart: &mut Multipart,
) -> AppResult<Received> {
    let mut received = Received::default();
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("comment") => {
                received.comment = field.text().await.map_err(multipart_error)?;
            }
            Some("file") => {
                let filename = field.file_name().map(str::to_string).unwrap_or_default();
                if filename.trim().is_empty() {
                    continue; // empty <input type=file>
                }
                let content_type = files::clean_content_type(field.content_type());
                let mut upload = st.vault.begin_upload(st.config.max_upload_bytes).await?;
                while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                    upload.write(&chunk).await?;
                }
                let blob = upload.finish().await?;
                received
                    .uploads
                    .push(files::Upload::classify(&st.vault, blob, filename, &content_type).await);
            }
            Some(other) => {
                let name = other.to_string();
                let value = field.text().await.map_err(multipart_error)?;
                received.fields.insert(name, value);
            }
            None => {}
        }
    }
    // Optimistic concurrency: the versions the uploads are based on.
    if let Some(raw) = received.fields.get("bases") {
        let bases = files::parse_bases(raw)?;
        files::apply_bases(&mut received.uploads, &bases);
    }
    // Where the files sit in the project group (the desktop working folder).
    let (folder, folders) = files::parse_folders(
        received.fields.get("folder").map(String::as_str),
        received.fields.get("folders").map(String::as_str),
    )?;
    files::apply_folders(&mut received.uploads, folder.as_deref(), &folders);
    Ok(received)
}

/// Upload into a revision that is checked out by the actor (new files or new
/// versions). Shared with the REST API.
pub(crate) async fn upload_files(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    multipart: &mut Multipart,
) -> AppResult<Vec<files::Stored>> {
    files::ensure_can_attach(&st.db, actor, revision_id).await?;
    let mut received = receive_uploads(st, multipart).await?;
    let sessions = take_session_uploads(
        st,
        actor,
        uploads::Scope::Revision(revision_id),
        &mut received,
    )
    .await?;
    let stored = files::attach(
        &st.db,
        actor,
        revision_id,
        &received.uploads,
        &received.comment,
    )
    .await?;
    uploads::forget(&st.db, &st.vault, &sessions).await;
    warm_previews(st, &received.uploads);
    Ok(stored)
}

/// Adds the chunked uploads listed in the `uploads` field (ids of complete
/// `/api/v1/uploads` sessions of the actor) to the received files, so large
/// files join a multipart upload or check-in – stored in the same
/// transaction. A session's own base and folder win over the `bases`,
/// `folders` and `folder` fields. Returns the session ids, to be forgotten
/// once the versions are stored.
pub(crate) async fn take_session_uploads(
    st: &AppState,
    actor: &Actor,
    scope: uploads::Scope,
    received: &mut Received,
) -> AppResult<Vec<Uuid>> {
    let Some(raw) = received.fields.get("uploads") else {
        return Ok(Vec::new());
    };
    let ids = uploads::parse_ids(raw)?;
    let bases = match received.fields.get("bases") {
        Some(raw) => files::parse_bases(raw)?,
        None => Default::default(),
    };
    let (folder, folders) = files::parse_folders(
        received.fields.get("folder").map(String::as_str),
        received.fields.get("folders").map(String::as_str),
    )?;
    for id in &ids {
        let mut upload = uploads::complete_in(&st.db, &st.vault, actor, *id, scope).await?;
        let key = files::stored_name(&upload.filename).to_lowercase();
        if upload.base.is_none() {
            upload.base = bases.get(&key).cloned();
        }
        if upload.folder.is_none() {
            upload.folder = folders.get(&key).cloned().or_else(|| folder.clone());
        }
        received.uploads.push(upload);
    }
    Ok(ids)
}

/// Starts the 3D preview conversion of freshly stored files in the
/// background, so the first viewer gets the model at once – together with
/// the shape signature for the similar part search.
pub(crate) fn warm_previews(st: &AppState, uploads: &[files::Upload]) {
    for upload in uploads {
        match PreviewFormat::of(&upload.filename) {
            Some(format) if format.is_point_cloud() => {
                st.previews.warm(&st.vault, &upload.blob.sha256, format)
            }
            Some(format) => {
                similar::spawn_ensure(st, &upload.blob.sha256, format);
            }
            None => {}
        }
    }
}

pub async fn upload(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}#files");
    let result = upload_files(&st, &ctx.me, id, &mut multipart).await;
    finish(&st, result, &back, |stored| {
        let names: Vec<String> = stored
            .iter()
            .map(|s| {
                if s.changed {
                    format!("{} v{}", s.filename, s.version)
                } else {
                    format!("{} (unchanged)", s.filename)
                }
            })
            .collect();
        (back.clone(), format!("Uploaded {}", names.join(", ")))
    })
}

/// Streams a blob from the vault as a download.
pub(crate) async fn blob_response(
    st: &AppState,
    sha256: &str,
    filename: &str,
    content_type: &str,
    size: i64,
) -> AppResult<Response> {
    let file = st.vault.open_blob(sha256).await?;
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, size)
        .header(
            header::CONTENT_DISPOSITION,
            util::content_disposition(filename),
        )
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header("x-content-sha256", sha256)
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|e| {
            AppError::Other(anyhow::anyhow!(
                "building the download response failed: {e}"
            ))
        })
}

/// [`blob_response`] that answers `Range` / `If-Range` (a client resuming
/// a download half-way, see `httprange`): `206` with the asked bytes, `416`
/// past the end, otherwise the whole file. Every answer names
/// `Accept-Ranges: bytes` and the ETag `"<sha256>"`.
pub(crate) async fn blob_response_ranged(
    st: &AppState,
    sha256: &str,
    filename: &str,
    content_type: &str,
    size: i64,
    headers: &HeaderMap,
) -> AppResult<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let etag = httprange::etag_of(sha256);
    let total = u64::try_from(size).unwrap_or(0);
    let value = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    let decision = httprange::decide(value(header::RANGE), value(header::IF_RANGE), &etag, total);
    let built = match decision {
        httprange::RangeDecision::Full => {
            let mut response = blob_response(st, sha256, filename, content_type, size).await?;
            let h = response.headers_mut();
            h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            if let Ok(v) = HeaderValue::from_str(&etag) {
                h.insert(header::ETAG, v);
            }
            return Ok(response);
        }
        httprange::RangeDecision::Unsatisfiable => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, &etag)
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty()),
        httprange::RangeDecision::Partial { start, end } => {
            let mut file = st.vault.open_blob(sha256).await?;
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(AppError::Io)?;
            let len = end - start + 1;
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, len)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .header(
                    header::CONTENT_DISPOSITION,
                    util::content_disposition(filename),
                )
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::ETAG, &etag)
                .header(header::CACHE_CONTROL, "private, no-cache")
                .header("x-content-sha256", sha256)
                .body(Body::from_stream(ReaderStream::new(file.take(len))))
        }
    };
    built.map_err(|e| {
        AppError::Other(anyhow::anyhow!(
            "building the download response failed: {e}"
        ))
    })
}

/// Current version of a file (resumable with `Range`). Shared with the REST API.
pub(crate) async fn file_response(
    st: &AppState,
    me: &Actor,
    log: &AccessCtx,
    file_id: Uuid,
    headers: &HeaderMap,
) -> AppResult<Response> {
    let f = files::get(&st.db, me, file_id).await?;
    log_file(st, log, me, f.id, &f.filename).await?;
    blob_response_ranged(
        st,
        &f.sha256,
        &f.filename,
        &f.content_type,
        f.size_bytes,
        headers,
    )
    .await
}

/// A specific version of a file (`name.v2.ext`, resumable with `Range`).
/// Shared with the REST API.
pub(crate) async fn version_response(
    st: &AppState,
    me: &Actor,
    log: &AccessCtx,
    version_id: Uuid,
    headers: &HeaderMap,
) -> AppResult<Response> {
    let (v, filename) = files::get_version(&st.db, me, version_id).await?;
    log_file(st, log, me, v.file_id, &filename).await?;
    let versioned = match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.v{}.{ext}", v.version),
        _ => format!("{filename}.v{}", v.version),
    };
    blob_response_ranged(
        st,
        &v.sha256,
        &versioned,
        &v.content_type,
        v.size_bytes,
        headers,
    )
    .await
}

pub async fn download(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    log: AccessCtx,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> AppResult<Response> {
    file_response(&st, &me, &log, id, &headers).await
}

/// The thumbnail of a format that is rendered from its 3D preview (NX
/// parts and assemblies). A part's image is cached by its content and drawn
/// from its cached preview mesh (converted first, in the isolated preview
/// process); an assembly's image sits next to its composed preview, whose
/// name depends on the components used. `None` when the format is not
/// rendered or there is no 3D preview (a drawing, an assembly without any
/// part in TeamCraft) – the image stored in the file is used then.
async fn rendered_thumbnail(
    st: &AppState,
    me: &Actor,
    file: &FileRecord,
    cache: &std::path::Path,
) -> AppResult<Option<thumbnail::Thumbnail>> {
    let Some(preview) = crate::preview::PreviewFormat::of_format(&file.format) else {
        return Ok(None);
    };
    if !thumbnail::rendered(&file.format) {
        return Ok(None);
    }
    let sha256 = file.sha256.clone();
    let blocking_failed =
        |e: tokio::task::JoinError| AppError::Other(anyhow::anyhow!("thumbnail task failed: {e}"));
    let store_failed =
        |e: std::io::Error| AppError::Other(anyhow::anyhow!("cannot store the thumbnail: {e}"));
    if let Ok(mesh_path) = st.previews.mesh(&st.vault, &sha256, preview).await {
        let cache = cache.to_path_buf();
        return tokio::task::spawn_blocking(
            move || -> std::io::Result<Option<thumbnail::Thumbnail>> {
                if let Some(Some(known)) = thumbnail::lookup(&cache, &sha256)? {
                    return Ok(Some(known));
                }
                let Some(image) = thumbnail::render_mesh(&std::fs::read(mesh_path)?) else {
                    return Ok(None);
                };
                thumbnail::store(&cache, &sha256, Some(&image))?;
                Ok(Some(image))
            },
        )
        .await
        .map_err(blocking_failed)?
        .map_err(store_failed);
    }
    let Some(mesh_path) =
        crate::services::assemblies::mesh(st, me, file.revision_id, &sha256, &file.format).await?
    else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || -> std::io::Result<Option<thumbnail::Thumbnail>> {
        let png = mesh_path.with_extension("png");
        if let Ok(bytes) = std::fs::read(&png) {
            return Ok(Some(thumbnail::Thumbnail {
                content_type: "image/png",
                bytes,
            }));
        }
        let Some(image) = thumbnail::render_mesh(&std::fs::read(&mesh_path)?) else {
            return Ok(None);
        };
        let tmp = png.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
        std::fs::write(&tmp, &image.bytes)?;
        std::fs::rename(&tmp, &png)?;
        Ok(Some(image))
    })
    .await
    .map_err(blocking_failed)?
    .map_err(store_failed)
}

/// The preview image of a CAD file, read once and cached next to the 3D
/// previews: stored inside the file (Inventor, Solid Edge, SolidWorks, NX
/// assemblies and drawings) or rendered from the 3D preview (NX parts,
/// SOLIDWORKS parts and assemblies).
/// Shared with the REST API.
pub(crate) async fn thumbnail_response(
    st: &AppState,
    me: &Actor,
    file_id: Uuid,
) -> AppResult<Response> {
    let f = files::get(&st.db, me, file_id).await?;
    if !thumbnail::supports(&f.format) {
        return Err(AppError::not_found(
            "Files of this type have no embedded preview image.",
        ));
    }
    let blob = st.vault.object_path(&f.sha256)?;
    let cache = st.vault.root().join("previews");
    let found = match rendered_thumbnail(st, me, &f, &cache).await? {
        Some(rendered) => Some(rendered),
        None => {
            let sha256 = f.sha256.clone();
            tokio::task::spawn_blocking(move || thumbnail::cached(&cache, &sha256, &blob))
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("thumbnail task failed: {e}")))??
        }
    };
    let Some(image) = found else {
        return Err(AppError::not_found(format!(
            "{} has no embedded preview image.",
            f.filename
        )));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, image.content_type)
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(header::ETAG, format!("\"{}-thumb\"", f.sha256))
        .body(Body::from(image.bytes))
        .map_err(|e| {
            AppError::Other(anyhow::anyhow!(
                "building the thumbnail response failed: {e}"
            ))
        })
}

pub async fn thumbnail(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    thumbnail_response(&st, &me, id).await
}

pub async fn download_version(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    log: AccessCtx,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> AppResult<Response> {
    version_response(&st, &me, &log, id, &headers).await
}

/// 3D preview page of the current version of a file.
pub async fn preview(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, &ctx.me, id).await?;
    let current = file.version;
    preview_page(&st, &ctx, file, current).await
}

/// The where-used warning of a file before it is removed: the readable CAD
/// files of other items that reference it ("ASM-000001/B frame.iam", at most
/// three named, then "and n more"); empty when nothing references it.
fn used_by_warning(
    filename: &str,
    referrers: &[crate::services::references::FileReferrer],
) -> String {
    let users: Vec<String> = referrers
        .iter()
        .filter(|r| r.referenced.eq_ignore_ascii_case(filename))
        .map(|r| format!("{}/{} {}", r.item_number, r.revision_code, r.filename))
        .collect();
    match users.len() {
        0 => String::new(),
        n => {
            let named = users[..n.min(3)].join(", ");
            let more = if n > 3 {
                format!(" and {} more", n - 3)
            } else {
                String::new()
            };
            format!(
                " It is still referenced by {n} file{} of other items: {named}{more} – their references break.",
                if n == 1 { "" } else { "s" }
            )
        }
    }
}

#[derive(Deserialize)]
pub struct NoteBody {
    x: f64,
    y: f64,
    z: f64,
    body: String,
}

/// `GET /file-versions/{id}/annotations`: the 3D notes (JSON, viewer.js).
pub async fn annotations(
    State(st): State<AppState>,
    crate::auth::WebUser(me): crate::auth::WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<axum::Json<Value>> {
    let notes = crate::services::annotations::list(&st.db, &me, id).await?;
    Ok(axum::Json(json!(notes)))
}

/// `POST /file-versions/{id}/annotations`: pins a note (JSON, viewer.js).
pub async fn add_annotation(
    State(st): State<AppState>,
    crate::auth::WebUser(me): crate::auth::WebUser,
    Path(id): Path<Uuid>,
    crate::api::ApiJson(body): crate::api::ApiJson<NoteBody>,
) -> AppResult<axum::Json<Value>> {
    let note = crate::services::annotations::add(
        &st.db,
        &me,
        id,
        crate::services::annotations::NewAnnotation {
            x: body.x,
            y: body.y,
            z: body.z,
            body: body.body,
        },
    )
    .await?;
    Ok(axum::Json(json!(note)))
}

/// `POST /annotations/{id}/delete`: removes a note (JSON, viewer.js).
pub async fn remove_annotation(
    State(st): State<AppState>,
    crate::auth::WebUser(me): crate::auth::WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<axum::Json<Value>> {
    crate::services::annotations::remove(&st.db, &me, id).await?;
    Ok(axum::Json(json!({ "deleted": true })))
}

/// 3D preview page of a specific (older) version of a file.
pub async fn preview_version(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, &ctx.me, id).await?;
    let file = files::get(&st.db, &ctx.me, version.file_id).await?;
    preview_page(&st, &ctx, file, version.version).await
}

fn no_preview() -> AppError {
    AppError::BadRequest(
        "There is no 3D preview for this file type – meshes (STL, OBJ, PLY, 3MF, glTF, …), \
         point clouds, STEP and simulation results (VTK, VTU, VTP, CalculiX FRD) are \
         supported; attach a STEP export of native CAD files."
            .into(),
    )
}

async fn preview_page(
    st: &AppState,
    ctx: &WebCtx,
    file: FileRecord,
    version: i32,
) -> AppResult<Response> {
    let format = preview_of(&file).ok_or_else(no_preview)?;
    let (versions, rev) = tokio::try_join!(
        files::versions(&st.db, &ctx.me, file.id),
        revisions::get(&st.db, &ctx.me, file.revision_id)
    )?;
    let shown = versions
        .iter()
        .find(|v| v.version == version)
        .cloned()
        .ok_or_else(|| AppError::not_found("File version not found."))?;
    // earlier/later models of the same file for the graphical comparison
    // (meshes only: point clouds and result fields are not overlaid)
    let compare = if format.is_point_cloud() || format.is_result() {
        Vec::new()
    } else {
        files::compare_candidates(&st.db, &ctx.me, file.id, &shown.sha256).await?
    };
    // start the conversion while the browser loads the page (with the
    // shape signature, unless it is a point cloud)
    if format.is_point_cloud() {
        st.previews.warm(&st.vault, &shown.sha256, format);
    } else {
        similar::spawn_ensure(st, &shown.sha256, format);
    }
    render(
        st,
        ctx,
        "files/preview.html",
        json!({
            "file": file,
            "shown": shown,
            "is_current": shown.version == file.version,
            "versions": versions,
            "compare": compare,
            "rev": rev,
            "format": format.as_str(),
            "is_brep": format.is_brep(),
            "is_points": format.is_point_cloud(),
            "is_result": format.is_result(),
        }),
    )
}

/// The preview mesh of a blob (`shared/js/viewer.js` format). The content is
/// addressed by its hash, so browsers may cache it for good. Shared with the
/// REST API.
pub(crate) async fn mesh_response(
    st: &AppState,
    me: &Actor,
    file: &FileRecord,
    sha256: &str,
) -> AppResult<Response> {
    let format = preview_of(file).ok_or_else(no_preview)?;
    let path = match st.previews.mesh(&st.vault, sha256, format).await {
        Ok(path) => path,
        // an NX or SOLIDWORKS assembly: put together from its parts' previews
        Err(e) if format.may_be_assembly() => {
            crate::services::assemblies::mesh(st, me, file.revision_id, sha256, format.as_str())
                .await?
                .ok_or(e)?
        }
        Err(e) => return Err(e),
    };
    let handle = tokio::fs::File::open(&path)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("cannot open the preview: {e}")))?;
    let size = handle
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| AppError::Other(anyhow::anyhow!("cannot read the preview: {e}")))?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size)
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header("x-content-sha256", sha256)
        .body(Body::from_stream(ReaderStream::new(handle)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("building the mesh response failed: {e}")))
}

/// Preview mesh of the current version of a file.
pub async fn mesh(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, &me, id).await?;
    let sha = file.sha256.clone();
    let mut response = mesh_response(&st, &me, &file, &sha).await?;
    // "current version" changes with every check-in
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// Preview mesh of a specific file version.
pub async fn mesh_version(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, &me, id).await?;
    let file = files::get(&st.db, &me, version.file_id).await?;
    mesh_response(&st, &me, &file, &version.sha256).await
}

#[derive(Deserialize)]
pub struct RestoreForm {
    version: i32,
    back: Option<String>,
}

/// Rollback of a file to an older version (stored as a new version).
pub async fn restore_version(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<RestoreForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = files::restore_version(&st.db, &ctx.me, id, form.version).await;
    finish(&st, result, &back, |f| {
        (
            back.clone(),
            format!("Restored {} – it is now version {}", f.filename, f.version),
        )
    })
}

#[derive(Deserialize)]
pub struct FolderForm {
    #[serde(default)]
    folder: String,
    back: Option<String>,
}

/// Moves a file to another folder of the project group (its place in the
/// desktop working folder) while the revision is checked out.
pub async fn set_file_folder(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<FolderForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = files::set_folder(&st.db, &ctx.me, id, &form.folder).await;
    finish(&st, result, &back, |file| {
        (
            format!("/revisions/{}#files", file.revision_id),
            if file.folder.is_empty() {
                format!("{} is now at the top of the project group", file.filename)
            } else {
                format!("{} is now in {}/", file.filename, file.folder)
            },
        )
    })
}

#[derive(Deserialize)]
pub struct RetentionForm {
    retention: String,
    back: Option<String>,
}

/// Keeps every version of a file or only the latest one (the housekeeping
/// removes older versions after a grace period) while the revision is
/// checked out.
pub async fn set_file_retention(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<RetentionForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = match crate::domain::FileRetention::parse(&form.retention) {
        Some(retention) => files::set_retention(&st.db, &ctx.me, id, retention).await,
        None => Err(AppError::BadRequest(
            "The retention is all or latest.".into(),
        )),
    };
    finish(&st, result, &back, |file| {
        (
            format!("/revisions/{}#files", file.revision_id),
            if file.retention == "latest" {
                format!(
                    "{}: only the latest version is kept, older versions are removed after the grace period",
                    file.filename
                )
            } else {
                format!("{}: every version is kept", file.filename)
            },
        )
    })
}

pub async fn delete_file(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = files::delete(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |file| {
        (
            format!("/revisions/{}#files", file.revision_id),
            format!("Removed {}", file.filename),
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::services::references::FileReferrer;
    use uuid::Uuid;

    fn referrer(number: &str, filename: &str, referenced: &str) -> FileReferrer {
        FileReferrer {
            item_id: Uuid::nil(),
            item_number: number.into(),
            revision_id: Uuid::nil(),
            revision_code: "A".into(),
            revision_state: "in_work".into(),
            item_name: "Frame".into(),
            file_id: Uuid::nil(),
            filename: filename.into(),
            referenced: referenced.into(),
            export_name: false,
        }
    }

    #[test]
    fn removing_a_referenced_file_warns_with_its_users() {
        assert_eq!(super::used_by_warning("bracket.prt", &[]), "");
        let one = [referrer("ASM-000001", "frame.asm", "Bracket.PRT")];
        let text = super::used_by_warning("bracket.prt", &one);
        assert!(
            text.contains("1 file of other items: ASM-000001/A frame.asm"),
            "{text}"
        );
        let many: Vec<_> = (1..=5)
            .map(|i| referrer(&format!("ASM-00000{i}"), "frame.asm", "bracket.prt"))
            .collect();
        let text = super::used_by_warning("bracket.prt", &many);
        assert!(
            text.contains("5 files") && text.contains("and 2 more"),
            "{text}"
        );
        assert_eq!(super::used_by_warning("other.prt", &many), "");
    }
}
