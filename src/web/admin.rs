//! Projects, user administration and the audit log.

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, pagination, render, WebCtx, PER_PAGE};
use crate::{
    domain::Role,
    error::{AppError, AppResult},
    services::{audit, projects, users, Page},
    util, AppState,
};

pub async fn projects(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let list = projects::list(&st.db, false).await?;
    render(&st, &ctx, "projects/list.html", json!({ "projects": list }))
}

#[derive(Deserialize)]
pub struct NewProjectForm {
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

pub async fn create_project(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<NewProjectForm>,
) -> AppResult<Response> {
    let result = projects::create(&st.db, &ctx.me, &form.code, &form.name, &form.description).await;
    finish(&st, result, "/projects", |p| {
        ("/projects".into(), format!("Created project {}", p.code))
    })
}

#[derive(Deserialize)]
pub struct ProjectForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    active: Option<String>,
}

pub async fn update_project(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ProjectForm>,
) -> AppResult<Response> {
    let result = projects::update(
        &st.db,
        &ctx.me,
        id,
        &form.name,
        &form.description,
        form.active.is_some(),
    )
    .await;
    finish(&st, result, "/projects", |p| {
        ("/projects".into(), format!("Saved project {}", p.code))
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
            "roles": Role::ALL.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "oidc": st.config.oidc.as_ref().map(|o| json!({
                "admin_group": o.admin_group,
                "engineer_group": o.engineer_group,
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
    render(
        &st,
        &ctx,
        "admin/user_edit.html",
        json!({
            "user": user,
            "tokens": tokens,
            "roles": Role::ALL.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
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
