//! Revision page: attributes, lifecycle actions, files.

use axum::{
    body::Body,
    extract::{multipart::MultipartError, Multipart, Path, Query, State},
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
    cadformat::FormatKind,
    domain::{self, LifecycleState, ReferenceKind, COMMON_UNITS},
    error::{AppError, AppResult},
    models::FileRecord,
    preview::PreviewFormat,
    services::{
        audit, bom, changes, files, impacts, items, notifications, projects, references, revisions,
    },
    thumbnail, util, AppState,
};

/// The 3D preview of a file: by its stored format, or by its name while
/// the format is not classified yet.
fn preview_of(file: &FileRecord) -> Option<PreviewFormat> {
    PreviewFormat::of_format(&file.format).or_else(|| PreviewFormat::of(&file.filename))
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, id).await?;
    let item = items::get(&st.db, rev.item_id).await?;
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
        rev_changes,
        projects,
    ) = tokio::try_join!(
        items::revisions(&st.db, item.id),
        files::versions_of_revision(&st.db, id),
        files::list(&st.db, id),
        bom::lines(&st.db, id),
        items::where_used(&st.db, item.id),
        // reference tracking and change propagation
        references::outgoing(&st.db, id),
        references::incoming(&st.db, item.id),
        references::files_of_revision(&st.db, id),
        references::file_referrers(&st.db, id),
        impacts::for_item(&st.db, item.id),
        notifications::is_watching(&st.db, ctx.me.id, "item", item.id),
        changes::for_revision(&st.db, id),
        // target groups for moving the item (only groups the user may edit)
        projects::editable_by(&st.db, &ctx.me),
    )?;
    let favorite = crate::services::personal::is_favorite(&st.db, ctx.me.id, item.id).await?;
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
    // The versions this page shows: uploads from its forms are based on them,
    // so a file that changed meanwhile is refused instead of overwritten.
    let file_bases = serde_json::Value::Object(
        file_list
            .iter()
            .map(|f| (f.filename.clone(), json!(f.version)))
            .collect(),
    )
    .to_string();
    // Every file together with its version history (newest first).
    let attachments: Vec<serde_json::Value> = file_list
        .into_iter()
        .map(|f| {
            let versions: Vec<_> = file_versions.iter().filter(|v| v.file_id == f.id).collect();
            let preview = preview_of(&f).is_some();
            let mut value = json!(f);
            value["versions"] = json!(versions);
            value["preview"] = json!(preview);
            value
        })
        .collect();
    let mut entity_ids: Vec<Uuid> = all_revisions.iter().map(|r| r.id).collect();
    entity_ids.push(item.id);
    let history = audit::for_entities(&st.db, &entity_ids, 50).await?;

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
        "acknowledge": me.can_edit() && me.can_edit_project(item.project_id),
    });
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
            "file_bases": file_bases,
            "native_without_preview": native_without_preview,
            "lines": lines,
            "where_used": where_used,
            "references": {
                "outgoing": outgoing,
                "incoming": incoming,
                "files": file_refs,
                "file_referrers": file_referrers,
                "suggestions": suggestions,
                "kinds": reference_kinds,
            },
            "open_impacts": open_impacts,
            "watching": watching,
            "favorite": favorite,
            "resolved_impacts": resolved_impacts,
            "changes": rev_changes,
            "open_change": open_change,
            "drafts": drafts,
            "history": history,
            "projects": projects,
            "units": COMMON_UNITS,
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
        let rev = revisions::get(&st.db, id).await?;
        if rev.checked_out_by != Some(ctx.me.id) {
            return Err(AppError::Conflict(format!(
                "You have not checked out {}.",
                revisions::label(&rev)
            )));
        }
        let received = receive_uploads(&st, &mut multipart).await?;
        let result =
            files::check_in(&st.db, &ctx.me, id, &received.uploads, &received.comment).await;
        if result.is_ok() {
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
    let received = receive_uploads(st, multipart).await?;
    let stored = files::attach(
        &st.db,
        actor,
        revision_id,
        &received.uploads,
        &received.comment,
    )
    .await?;
    warm_previews(st, &received.uploads);
    Ok(stored)
}

/// Starts the 3D preview conversion of freshly stored files in the
/// background, so the first viewer gets the model at once.
pub(crate) fn warm_previews(st: &AppState, uploads: &[files::Upload]) {
    for upload in uploads {
        if let Some(format) = PreviewFormat::of(&upload.filename) {
            st.previews.warm(&st.vault, &upload.blob.sha256, format);
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

/// The preview image stored inside a CAD file (Inventor, Solid Edge,
/// SolidWorks), read once and cached next to the 3D previews. Shared with
/// the REST API.
pub(crate) async fn thumbnail_response(st: &AppState, file_id: Uuid) -> AppResult<Response> {
    let f = files::get(&st.db, file_id).await?;
    if !thumbnail::supports(&f.format) {
        return Err(AppError::not_found(
            "Files of this type have no embedded preview image.",
        ));
    }
    let blob = st.vault.object_path(&f.sha256)?;
    let cache = st.vault.root().join("previews");
    let sha256 = f.sha256.clone();
    let found = tokio::task::spawn_blocking(move || thumbnail::cached(&cache, &sha256, &blob))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("thumbnail task failed: {e}")))??;
    let Some(image) = found else {
        return Err(AppError::not_found(format!(
            "{} has no embedded preview image.",
            f.filename
        )));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, image.content_type)
        .header(header::CACHE_CONTROL, "private, max-age=86400")
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
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    thumbnail_response(&st, id).await
}

pub async fn download_version(
    State(st): State<AppState>,
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    version_response(&st, id).await
}

/// 3D preview page of the current version of a file.
pub async fn preview(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, id).await?;
    let current = file.version;
    preview_page(&st, &ctx, file, current).await
}

/// 3D preview page of a specific (older) version of a file.
pub async fn preview_version(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, id).await?;
    let file = files::get(&st.db, version.file_id).await?;
    preview_page(&st, &ctx, file, version.version).await
}

fn no_preview() -> AppError {
    AppError::BadRequest(
        "There is no 3D preview for this file type – meshes (STL, OBJ, PLY, 3MF, glTF, …), \
         point clouds and STEP are supported; attach a STEP export of native CAD files."
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
        files::versions(&st.db, file.id),
        revisions::get(&st.db, file.revision_id)
    )?;
    let shown = versions
        .iter()
        .find(|v| v.version == version)
        .cloned()
        .ok_or_else(|| AppError::not_found("File version not found."))?;
    // start the conversion while the browser loads the page
    st.previews.warm(&st.vault, &shown.sha256, format);
    render(
        st,
        ctx,
        "files/preview.html",
        json!({
            "file": file,
            "shown": shown,
            "is_current": shown.version == file.version,
            "versions": versions,
            "rev": rev,
            "format": format.as_str(),
            "is_brep": format.is_brep(),
            "is_points": format.is_point_cloud(),
        }),
    )
}

/// The preview mesh of a blob (`static/viewer.js` format). The content is
/// addressed by its hash, so browsers may cache it for good. Shared with the
/// REST API.
pub(crate) async fn mesh_response(
    st: &AppState,
    file: &FileRecord,
    sha256: &str,
) -> AppResult<Response> {
    let format = preview_of(file).ok_or_else(no_preview)?;
    let path = st.previews.mesh(&st.vault, sha256, format).await?;
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
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header("x-content-sha256", sha256)
        .body(Body::from_stream(ReaderStream::new(handle)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("building the mesh response failed: {e}")))
}

/// Preview mesh of the current version of a file.
pub async fn mesh(
    State(st): State<AppState>,
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, id).await?;
    let sha = file.sha256.clone();
    let mut response = mesh_response(&st, &file, &sha).await?;
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
    WebUser(_me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, id).await?;
    let file = files::get(&st.db, version.file_id).await?;
    mesh_response(&st, &file, &version.sha256).await
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
