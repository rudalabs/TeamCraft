//! Revision page: attributes, lifecycle actions, files.

use axum::{
    body::Body,
    extract::{multipart::MultipartError, Multipart, Path, State},
    http::{header, StatusCode},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use super::{finish, items::BackForm, render, WebCtx};
use crate::{
    auth::{Actor, WebUser},
    domain::{self, LifecycleState, COMMON_UNITS},
    error::{AppError, AppResult},
    models::FileRecord,
    services::{audit, bom, changes, files, items, projects, revisions},
    util, AppState,
};

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, id).await?;
    let item = items::get(&st.db, rev.item_id).await?;
    let all_revisions = items::revisions(&st.db, item.id).await?;
    let attachments = files::list(&st.db, id).await?;
    let lines = bom::lines(&st.db, id).await?;
    let where_used = items::where_used(&st.db, item.id).await?;
    let rev_changes = changes::for_revision(&st.db, id).await?;
    let mut entity_ids: Vec<Uuid> = all_revisions.iter().map(|r| r.id).collect();
    entity_ids.push(item.id);
    let history = audit::for_entities(&st.db, &entity_ids, 50).await?;
    // Target groups for moving the item (only groups the user may edit).
    let projects = projects::editable_by(&st.db, &ctx.me).await?;

    let state = revisions::state_of(&rev);
    let me = &ctx.me;
    let editable = revisions::ensure_editable(&rev, me).is_ok();
    let item_has_open_revision = all_revisions
        .iter()
        .any(|r| revisions::state_of(r).is_open());
    let open_change = rev_changes
        .iter()
        .find(|c| changes::status_of(c).is_open())
        .cloned();
    let drafts = if editable && open_change.is_none() {
        changes::drafts(&st.db)
            .await?
            .into_iter()
            .filter(|c| c.created_by == me.id || me.is_admin())
            .collect()
    } else {
        Vec::new()
    };
    let in_project = revisions::ensure_project_access(&rev, me).is_ok();
    let can = json!({
        "edit": editable,
        "checkout": editable && rev.checked_out_by.is_none(),
        "checkin": rev.checked_out_by.is_some()
            && me.can_edit()
            && (rev.checked_out_by == Some(me.id) || me.is_admin()),
        "revise": in_project && !item_has_open_revision,
        "obsolete": in_project && state == LifecycleState::Released,
        "delete": ((item.created_by == me.id && in_project) || me.is_admin())
            && all_revisions.len() == 1
            && state == LifecycleState::InWork
            && where_used.is_empty()
            && rev_changes.is_empty(),
        "master": in_project,
        "add_to_change": editable && open_change.is_none(),
    });
    let latest_id = all_revisions.first().map(|r| r.id);
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
            "files": attachments,
            "lines": lines,
            "where_used": where_used,
            "changes": rev_changes,
            "open_change": open_change,
            "drafts": drafts,
            "history": history,
            "projects": projects,
            "units": COMMON_UNITS,
            "can": can,
            "max_upload_mb": st.config.max_upload_bytes / (1024 * 1024),
        }),
    )
}

pub async fn edit_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, id).await?;
    revisions::ensure_editable(&rev, &ctx.me)?;
    render(
        &st,
        &ctx,
        "revisions/edit.html",
        json!({ "rev": rev, "error": null }),
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
            let mut rev = revisions::get(&st.db, id).await?;
            rev.name = form.name;
            rev.description = form.description;
            rev.material = form.material;
            let mut page = render(
                &st,
                &ctx,
                "revisions/edit.html",
                json!({ "rev": rev, "error": e.public_message(), "mass_raw": form.mass_kg, "cost_raw": form.unit_cost }),
            )?;
            *page.status_mut() = e.status();
            Ok(page)
        }
        Err(e) => Err(e),
    }
}

pub async fn checkout(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
    let result = revisions::checkout(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            back.clone(),
            format!("Checked out {}", revisions::label(&rev)),
        )
    })
}

pub async fn checkin(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
    let result = revisions::checkin(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |rev| {
        (
            back.clone(),
            format!("Checked in {}", revisions::label(&rev)),
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

fn multipart_error(e: MultipartError) -> AppError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::PayloadTooLarge("The upload exceeds the configured size limit.".into())
    } else {
        AppError::BadRequest(format!("Upload failed: {}", e.body_text()))
    }
}

/// Streams every `file` field of a multipart request into the vault and
/// attaches it to the revision. Shared with the REST API.
pub(crate) async fn receive_files(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    multipart: &mut Multipart,
) -> AppResult<Vec<FileRecord>> {
    files::ensure_can_attach(&st.db, actor, revision_id).await?;
    let mut stored = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("file") {
            continue;
        }
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
        stored.push(
            files::attach(&st.db, actor, revision_id, &blob, &filename, &content_type).await?,
        );
    }
    if stored.is_empty() {
        return Err(AppError::BadRequest(
            "Choose at least one file to upload.".into(),
        ));
    }
    Ok(stored)
}

pub async fn upload(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}#files");
    let result = receive_files(&st, &ctx.me, id, &mut multipart).await;
    finish(&st, result, &back, |stored| {
        let names: Vec<String> = stored.iter().map(|f| f.filename.clone()).collect();
        (back.clone(), format!("Uploaded {}", names.join(", ")))
    })
}

/// Streams a file from the vault as a download. Shared with the REST API.
pub(crate) async fn file_response(st: &AppState, file_id: Uuid) -> AppResult<Response> {
    let record = files::get(&st.db, file_id).await?;
    let file = st.vault.open_blob(&record.sha256).await?;
    Response::builder()
        .header(header::CONTENT_TYPE, record.content_type.as_str())
        .header(header::CONTENT_LENGTH, record.size_bytes)
        .header(
            header::CONTENT_DISPOSITION,
            util::content_disposition(&record.filename),
        )
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header("x-content-sha256", record.sha256.as_str())
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|e| {
            AppError::Other(anyhow::anyhow!(
                "building the download response failed: {e}"
            ))
        })
}

pub async fn download(
    State(st): State<AppState>,
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    file_response(&st, id).await
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
