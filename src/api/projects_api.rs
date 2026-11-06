//! Project groups: list, create, members, visibility and numbering.

use super::*;

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

pub(super) async fn list_projects(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(projects::list(&st.db, &me, false).await?)))
}

#[derive(Deserialize)]
pub(super) struct CreateProject {
    code: String,
    name: String,
    #[serde(default)]
    description: String,
    oidc_group: Option<String>,
    /// `internal` (default) or `restricted`.
    visibility: Option<String>,
}

pub(super) async fn create_project(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateProject>,
) -> AppResult<Response> {
    let project = projects::create(
        &st.db,
        &me,
        &body.code,
        &body.name,
        &body.description,
        body.oidc_group.as_deref(),
        parse_visibility(body.visibility.as_deref().unwrap_or("internal"))?,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(project))).into_response())
}

pub(super) fn parse_visibility(raw: &str) -> AppResult<ProjectVisibility> {
    ProjectVisibility::parse(raw)
        .ok_or_else(|| AppError::BadRequest("visibility must be internal or restricted.".into()))
}

#[derive(Deserialize)]
pub(super) struct SetVisibility {
    visibility: String,
}

/// `PUT /api/v1/projects/{code}/visibility` – managers of the group, admins.
pub(super) async fn set_project_visibility(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<SetVisibility>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let visibility = parse_visibility(&body.visibility)?;
    Ok(Json(json!(
        projects::set_visibility(&st.db, &me, project.id, visibility).await?
    )))
}

pub(super) async fn project_members(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    Ok(Json(json!(projects::members(&st.db, project.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct AddMember {
    username: String,
    #[serde(default)]
    manager: bool,
}

pub(super) async fn add_project_member(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<AddMember>,
) -> AppResult<Response> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let member =
        projects::add_member(&st.db, &me, project.id, &body.username, body.manager).await?;
    Ok((StatusCode::CREATED, Json(json!(member))).into_response())
}

#[derive(Deserialize)]
pub(super) struct NumberingQuery {
    scheme: Option<String>,
}

pub(super) async fn project_numbering(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    Query(q): Query<NumberingQuery>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let preview = projects::numbering_preview(&st.db, &project, q.scheme.as_deref()).await?;
    Ok(Json(json!(preview)))
}

#[derive(Deserialize)]
pub(super) struct SetNumbering {
    #[serde(default)]
    scheme: Option<String>,
}

pub(super) async fn set_project_numbering(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<SetNumbering>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let project = projects::set_number_scheme(
        &st.db,
        &me,
        project.id,
        body.scheme.as_deref().unwrap_or(""),
    )
    .await?;
    let preview = projects::numbering_preview(&st.db, &project, None).await?;
    Ok(Json(json!({ "project": project, "numbering": preview })))
}
