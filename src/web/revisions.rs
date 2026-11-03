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
    let file_versions = files::versions_of_revision(&st.db, id).await?;
    // Every file together with its version history (newest first).
    let attachments: Vec<serde_json::Value> = files::list(&st.db, id)
        .await?
        .into_iter()
        .map(|f| {
            let versions: Vec<_> = file_versions.iter().filter(|v| v.file_id == f.id).collect();
            let mut value = json!(f);
            value["versions"] = json!(versions);
            value
        })
        .collect();
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
        "checkout": can_work && rev.checked_out_by.is_none(),
        "checkin": rev.checked_out_by == Some(me.id),
        "cancel_checkout": rev.checked_out_by.is_some()
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
        "add_to_change": can_work && open_change.is_none(),
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

/// Check-in form (multipart): optional new file versions + comment; the files
/// are stored and the lock is released in one transaction.
pub async fn checkin(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
    let result = async {
        let rev = revisions::get(&st.db, id).await?;
        if rev.checked_out_by != Some(ctx.me.id) {
            return Err(AppError::Conflict(format!(
                "You have not checked out {}.",
                revisions::label(&rev)
            )));
        }
        let received = receive_uploads(&st, &mut multipart).await?;
        files::check_in(&st.db, &ctx.me, id, &received.uploads, &received.comment).await
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
) -> AppResult<Response> {
    let back = format!("/revisions/{id}");
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

fn multipart_error(e: MultipartError) -> AppError {
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
}

/// Streams every `file` field of a multipart request into the vault and
/// collects the `comment` field. Nothing is registered in the database yet.
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
                received.uploads.push(files::Upload {
                    blob,
                    filename,
                    content_type,
                });
            }
            _ => {}
        }
    }
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
    let received = receive_uploads(st, multipart).await?;
    files::attach(
        &st.db,
        actor,
        revision_id,
        &received.uploads,
        &received.comment,
    )
    .await
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

/// Current version of a file. Shared with the REST API.
pub(crate) async fn file_response(st: &AppState, file_id: Uuid) -> AppResult<Response> {
    let f = files::get(&st.db, file_id).await?;
    blob_response(st, &f.sha256, &f.filename, &f.content_type, f.size_bytes).await
}

/// A specific version of a file (`name.v2.ext`). Shared with the REST API.
pub(crate) async fn version_response(st: &AppState, version_id: Uuid) -> AppResult<Response> {
    let (v, filename) = files::get_version(&st.db, version_id).await?;
    let versioned = match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.v{}.{ext}", v.version),
        _ => format!("{filename}.v{}", v.version),
    };
    blob_response(st, &v.sha256, &versioned, &v.content_type, v.size_bytes).await
}

pub async fn download(
    State(st): State<AppState>,
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    file_response(&st, id).await
}

pub async fn download_version(
    State(st): State<AppState>,
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    version_response(&st, id).await
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
