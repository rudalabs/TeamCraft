//! Files of a revision: versions, previews, downloads and metadata.

use super::*;

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

pub(super) async fn list_files(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(files::list(&st.db, &me, rev.id).await?)))
}

pub(super) async fn upload_files(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let stored = web::upload_files(&st, &me, rev.id, &mut multipart).await?;
    let mut records = Vec::with_capacity(stored.len());
    for s in &stored {
        records.push(files::get(&st.db, &me, s.file_id).await?);
    }
    Ok((StatusCode::CREATED, Json(json!(records))).into_response())
}

pub(super) async fn file_versions(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(files::versions(&st.db, &me, id).await?)))
}

#[derive(Deserialize)]
pub(super) struct RestoreBody {
    version: i32,
}

pub(super) async fn restore_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<RestoreBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        files::restore_version(&st.db, &me, id, body.version).await?
    )))
}

pub(super) async fn download_version(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    web::version_response(&st, &me, id, &headers).await
}

/// 3D preview mesh of the current version (binary `TCMESH01`, see `mesh`).
pub(super) async fn file_mesh(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, &me, id).await?;
    let sha = file.sha256.clone();
    web::mesh_response(&st, &file, &sha).await
}

/// 3D preview mesh of a file version.
pub(super) async fn version_mesh(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, &me, id).await?;
    let file = files::get(&st.db, &me, version.file_id).await?;
    web::mesh_response(&st, &file, &version.sha256).await
}

pub(super) async fn file_meta(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(files::get(&st.db, &me, id).await?)))
}

#[derive(Deserialize)]
pub(super) struct UpdateFile {
    /// Folder inside the project group (`""` = top).
    folder: String,
}

/// Changes the folder of a file while its revision is checked out.
pub(super) async fn update_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateFile>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        files::set_folder(&st.db, &me, id, &body.folder).await?
    )))
}

pub(super) async fn download(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    web::file_response(&st, &me, id, &headers).await
}

/// The preview image embedded in an Inventor, Solid Edge or SolidWorks file.
pub(super) async fn file_thumbnail(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::thumbnail_response(&st, &me, id).await
}

pub(super) async fn delete_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    files::delete(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
