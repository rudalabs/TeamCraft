//! Projects, user administration and the audit log.

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, pagination, render, WebCtx, PER_PAGE};
use crate::{
    domain::{self, ProjectVisibility, Role},
    error::{AppError, AppResult},
    services::{audit, projects, users, Page},
    util, AppState,
};

/// Roles as `{value, label}` for select boxes.
fn roles() -> Value {
    json!(Role::ALL
        .iter()
        .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
        .collect::<Vec<_>>())
}

pub async fn projects(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let list = projects::list(&st.db, &ctx.me, false).await?;
    render(
        &st,
        &ctx,
        "projects/list.html",
        json!({ "projects": list, "my_projects": ctx.me.projects }),
    )
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    /// A numbering scheme to preview without saving it.
    scheme: Option<String>,
}

pub async fn project_show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<ProjectQuery>,
) -> AppResult<Response> {
    let (project, members, can_manage) = tokio::try_join!(
        projects::get(&st.db, &ctx.me, id),
        projects::members(&st.db, id),
        projects::is_manager(&st.db, &ctx.me, id),
    )?;
    // A candidate scheme from the preview button; an invalid one is shown
    // next to the field together with the current scheme's numbers.
    let candidate = q.scheme.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (numbering, scheme_error) =
        match projects::numbering_preview(&st.db, &project, candidate).await {
            Ok(p) => (p, None),
            Err(AppError::BadRequest(msg)) => (
                projects::numbering_preview(&st.db, &project, None).await?,
                Some(msg),
            ),
            Err(e) => return Err(e),
        };
    let all_users = if can_manage {
        users::list(&st.db)
            .await?
            .into_iter()
            .filter(|u| u.active && !members.iter().any(|m| m.user_id == u.id))
            .collect()
    } else {
        Vec::new()
    };
    render(
        &st,
        &ctx,
        "projects/show.html",
        json!({
            "project": project,
            "members": members,
            "candidates": all_users,
            "is_member": ctx.me.projects.contains(&id),
            "can_edit": ctx.me.can_edit_project(Some(id)),
            "can_manage": can_manage,
            "oidc_enabled": st.config.oidc.is_some(),
            "numbering": numbering,
            "scheme_input": q.scheme.as_deref().map(str::trim).unwrap_or_else(|| project.number_scheme.as_deref().unwrap_or("")),
            "previewing": candidate.is_some() && scheme_error.is_none(),
            "scheme_error": scheme_error,
            "default_scheme": domain::DEFAULT_NUMBER_SCHEME,
        }),
    )
}

#[derive(Deserialize)]
pub struct NumberingForm {
    #[serde(default)]
    scheme: String,
}

pub async fn update_numbering(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<NumberingForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = projects::set_number_scheme(&st.db, &ctx.me, id, &form.scheme).await;
    finish(&st, result, &back, |p| {
        (
            back.clone(),
            format!(
                "Numbering scheme of {}: {}",
                p.code,
                p.number_scheme
                    .as_deref()
                    .unwrap_or(domain::DEFAULT_NUMBER_SCHEME)
            ),
        )
    })
}

#[derive(Deserialize)]
pub struct NewProjectForm {
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    oidc_group: String,
    /// Checkbox: start as a restricted group.
    restricted: Option<String>,
}

pub async fn create_project(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<NewProjectForm>,
) -> AppResult<Response> {
    let result = projects::create(
        &st.db,
        &ctx.me,
        &form.code,
        &form.name,
        &form.description,
        ctx.me.is_admin().then_some(form.oidc_group.as_str()),
        if form.restricted.is_some() {
            ProjectVisibility::Restricted
        } else {
            ProjectVisibility::Internal
        },
    )
    .await;
    finish(&st, result, "/projects", |p| {
        (
            format!("/projects/{}", p.id),
            format!("Created project group {}", p.code),
        )
    })
}

#[derive(Deserialize)]
pub struct ProjectForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    active: Option<String>,
    #[serde(default)]
    oidc_group: String,
}

pub async fn update_project(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ProjectForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = projects::update(
        &st.db,
        &ctx.me,
        id,
        projects::ProjectUpdate {
            name: &form.name,
            description: &form.description,
            active: form.active.is_some(),
            oidc_group: Some(&form.oidc_group),
        },
    )
    .await;
    finish(&st, result, &back, |p| {
        (back.clone(), format!("Saved project group {}", p.code))
    })
}

#[derive(Deserialize)]
pub struct MemberForm {
    #[serde(default)]
    username: String,
    manager: Option<String>,
}

pub async fn add_member(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<MemberForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result =
        projects::add_member(&st.db, &ctx.me, id, &form.username, form.manager.is_some()).await;
    finish(&st, result, &back, |m| {
        (
            back.clone(),
            format!("Added {} to the project group", m.display_name),
        )
    })
}

pub async fn remove_member(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let leaving = user_id == ctx.me.id;
    let result = projects::remove_member(&st.db, &ctx.me, id, user_id).await;
    finish(&st, result, &back, |held| {
        let mut message = if leaving {
            "You left the project group".to_string()
        } else {
            "Removed the member".to_string()
        };
        if held > 0 {
            message.push_str(&format!(
                " – {held} check-outs in this group are still held; a manager can cancel them"
            ));
        }
        // A former member of a restricted group cannot see its page any more.
        let target = if leaving {
            "/projects".into()
        } else {
            back.clone()
        };
        (target, message)
    })
}

#[derive(Deserialize)]
pub struct VisibilityForm {
    #[serde(default)]
    visibility: String,
}

/// Restricts a group to its members or opens it again (managers, admins).
pub async fn update_visibility(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<VisibilityForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = match ProjectVisibility::parse(&form.visibility) {
        Some(v) => projects::set_visibility(&st.db, &ctx.me, id, v).await,
        None => Err(AppError::BadRequest(
            "Choose internal or restricted.".into(),
        )),
    };
    finish(&st, result, &back, |p| {
        let message = if p.is_restricted() {
            format!(
                "{} is restricted: only its members and administrators see it",
                p.code
            )
        } else {
            format!("{} is open to all users again", p.code)
        };
        (back.clone(), message)
    })
}

pub async fn users(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let list = users::list(&st.db).await?;
    render(
        &st,
        &ctx,
        "admin/users.html",
        json!({
            "users": list,
            "roles": roles(),
            "oidc": st.config.oidc.as_ref().map(|o| json!({
                "admin_group": o.admin_group,
                "editor_group": o.editor_group,
                "viewer_group": o.viewer_group,
            })),
        }),
    )
}

#[derive(Deserialize)]
pub struct NewUserForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    password: String,
}

pub async fn create_user(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<NewUserForm>,
) -> AppResult<Response> {
    let result = async {
        if !st.config.local_login {
            return Err(AppError::BadRequest(
                "Local accounts are disabled (TEAMCRAFT_LOCAL_LOGIN=false); users are provisioned via single sign-on.".into(),
            ));
        }
        let role = Role::parse(&form.role).ok_or_else(|| AppError::BadRequest("Unknown role.".into()))?;
        users::create_local(
            &st.db,
            Some(&ctx.me),
            users::NewLocalUser {
                username: form.username.clone(),
                display_name: form.display_name.clone(),
                email: Some(form.email.clone()),
                role,
                password: form.password.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, "/admin/users", |u| {
        (
            "/admin/users".into(),
            format!("Created user {}", u.username),
        )
    })
}

pub async fn user_edit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let user = users::get(&st.db, id).await?;
    let tokens = users::list_tokens(&st.db, id).await?;
    let memberships = projects::memberships_of(&st.db, id).await?;
    render(
        &st,
        &ctx,
        "admin/user_edit.html",
        json!({
            "user": user,
            "tokens": tokens,
            "memberships": memberships,
            "roles": roles(),
            "oidc_enabled": st.config.oidc.is_some(),
        }),
    )
}

#[derive(Deserialize)]
pub struct UserForm {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    role: String,
    active: Option<String>,
}

pub async fn update_user(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<UserForm>,
) -> AppResult<Response> {
    let back = format!("/admin/users/{id}");
    let result = async {
        let role =
            Role::parse(&form.role).ok_or_else(|| AppError::BadRequest("Unknown role.".into()))?;
        users::update(
            &st.db,
            &ctx.me,
            id,
            users::UserUpdate {
                display_name: form.display_name.clone(),
                email: Some(form.email.clone()),
                role,
                active: form.active.is_some(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |u| {
        (back.clone(), format!("Saved user {}", u.username))
    })
}

#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    password: String,
}

pub async fn reset_password(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<PasswordForm>,
) -> AppResult<Response> {
    let back = format!("/admin/users/{id}");
    let result = users::set_password(&st.db, Some(&ctx.me), id, &form.password).await;
    finish(&st, result, &back, |_| {
        (
            back.clone(),
            "Password was reset; the user was signed out everywhere".into(),
        )
    })
}

#[derive(Deserialize)]
pub struct AuditQuery {
    entity_type: Option<String>,
    username: Option<String>,
    action: Option<String>,
    page: Option<i64>,
}

pub async fn audit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<AuditQuery>,
) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let filter = audit::AuditFilter {
        entity_type: clean(&q.entity_type),
        username: clean(&q.username),
        action: clean(&q.action),
    };
    let page_no = q.page.unwrap_or(1).max(1);
    let (entries, total) =
        audit::list(&st.db, &filter, Page::from_page_number(Some(page_no), 100)).await?;
    let base_query = [
        ("entity_type", filter.entity_type.as_deref()),
        ("username", filter.username.as_deref()),
        ("action", filter.action.as_deref()),
    ]
    .iter()
    .filter_map(|(k, v)| v.map(|v| format!("{k}={}", util::pct_encode(v))))
    .collect::<Vec<_>>()
    .join("&");
    render(
        &st,
        &ctx,
        "admin/audit.html",
        json!({
            "entries": entries,
            "filter": {
                "entity_type": filter.entity_type,
                "username": filter.username,
                "action": filter.action,
            },
            "base_query": base_query,
            "pagination": pagination(page_no, total, 100),
            "per_page": PER_PAGE,
        }),
    )
}
