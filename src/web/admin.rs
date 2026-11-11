//! Projects, user administration and the audit log.

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, pagination, render, WebCtx, PER_PAGE};
use crate::{
    domain::{self, AdminRead, ProjectVisibility, Role},
    error::{AppError, AppResult},
    resultscan::TemplateInput,
    services::{
        access_log, access_review, audit, checkouts, ownership, project_policy, projects,
        result_templates, users, Page,
    },
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
    let list = projects::list_managed(&st.db, &ctx.me).await?;
    // groups whose access review is due, among those the user manages
    let review_due = access_review::due_for(&st.db, &ctx.me).await?;
    // administrators: groups without a present manager (#173)
    let gaps = if ctx.me.is_admin() {
        crate::services::manager_gaps::list(&st.db, &ctx.me).await?
    } else {
        Vec::new()
    };
    render(
        &st,
        &ctx,
        "projects/list.html",
        json!({
            "projects": list,
            "my_projects": ctx.me.projects,
            "review_due": review_due,
            "manager_gaps": crate::services::manager_gaps::to_json(&gaps),
            "manager_gap_ids": gaps.iter().map(|g| g.project_id).collect::<Vec<_>>(),
            "stale_days": crate::services::manager_gaps::STALE_DAYS,
        }),
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
    origin: crate::auth::Origin,
    Path(id): Path<Uuid>,
    Query(q): Query<ProjectQuery>,
) -> AppResult<Response> {
    let (project, members, can_manage) = tokio::try_join!(
        projects::get_for_management(&st.db, &ctx.me, id),
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
    // An administrator whose need-to-know policy hides the group maintains
    // it here but reads nothing of its content.
    let readable = ctx.me.can_see_project(Some(id));
    let templates = if readable {
        result_templates::list(&st.db, &ctx.me, id).await?
    } else {
        Vec::new()
    };
    let policy = if can_manage && project.visibility == "restricted" {
        Some(project_policy::get(&st.db, &ctx.me, id).await?)
    } else {
        None
    };
    let interface_rows: Vec<Value> = match &policy {
        Some(p) => project_policy::Interface::ALL
            .iter()
            .map(|i| {
                json!({ "name": i.as_str(), "label": i.label(),
                        "allowed": p.access.interfaces.allowed(*i) })
            })
            .collect(),
        None => Vec::new(),
    };
    let review_schedule = match &policy {
        Some(_) => Some(access_review::schedule(&st.db, id).await?),
        None => None,
    };
    let access_entries = match &policy {
        Some(_) if readable => {
            let filter = access_log::Filter {
                limit: Some(50),
                ..Default::default()
            };
            access_log::list(&st.db, &ctx.me, id, &filter).await?
        }
        _ => Vec::new(),
    };
    let review_policy = if can_manage {
        Some(project_policy::get_review_policy(&st.db, &ctx.me, id).await?)
    } else {
        None
    };
    let admin_modes: Vec<Value> = [AdminRead::Implicit, AdminRead::WithReason, AdminRead::None]
        .iter()
        .map(|m| json!({ "value": m.as_str(), "label": m.label() }))
        .collect();
    let all_users = if can_manage {
        users::list(&st.db)
            .await?
            .into_iter()
            .filter(|u| u.active && !members.iter().any(|m| m.user_id == u.id))
            .collect()
    } else {
        Vec::new()
    };
    let former_holders = if can_manage {
        projects::former_holders(&st.db, &ctx.me, id).await?
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
            "former_holders": former_holders,
            "candidates": all_users,
            "is_member": ctx.me.projects.contains(&id),
            "can_edit": ctx.me.can_edit_project(Some(id)),
            "can_manage": can_manage,
            "readable": readable,
            "policy": policy,
            "review_policy": review_policy,
            "admin_modes": admin_modes,
            "interface_rows": interface_rows,
            "access_entries": access_entries,
            "review_schedule": review_schedule,
            "claim_lines": policy.as_ref().map(|p| {
                crate::services::membership_conditions::claim_lines(&p.conditions.required_claims)
            }),
            "my_address": origin.client.map(|ip| ip.to_string()),
            "elevation_minutes": project_policy::ELEVATION_MINUTES,
            "result_templates": templates,
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
pub struct PolicyForm {
    #[serde(default)]
    admin_read: String,
}

/// `POST /projects/{id}/policy` – managers of the group, admins.
pub async fn update_policy(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<PolicyForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = match AdminRead::parse(&form.admin_read) {
        Some(m) => project_policy::set_admin_read(&st.db, &ctx.me, id, m).await,
        None => Err(AppError::BadRequest(
            "Choose how administrators read the group.".into(),
        )),
    };
    finish(&st, result, &back, |p| {
        (
            back.clone(),
            format!("Administrator access: {}", p.admin_read.label()),
        )
    })
}

#[derive(Deserialize)]
pub struct AccessForm {
    #[serde(default)]
    api_tokens: Option<String>,
    #[serde(default)]
    mcp: Option<String>,
    #[serde(default)]
    assistant: Option<String>,
    #[serde(default)]
    webhooks: Option<String>,
    #[serde(default)]
    packages: Option<String>,
    #[serde(default)]
    notification_content: Option<String>,
    #[serde(default)]
    external_llm: Option<String>,
    #[serde(default)]
    desktop_sync: Option<String>,
    #[serde(default)]
    read_log: Option<String>,
    #[serde(default)]
    retention_days: String,
    #[serde(default)]
    review_interval_days: String,
}

/// An optional number of days from a form field (empty = none).
fn optional_days(raw: &str, what: &str) -> AppResult<Option<i32>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<i32>()
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("The {what} is a number of days (or empty).")))
}

/// `POST /projects/{id}/access` – managers of the group, admins: interface
/// switches (a ticked box allows the interface) and the read log.
pub async fn update_access(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AccessForm>,
) -> AppResult<Response> {
    use project_policy::Interface;
    let back = format!("/projects/{id}");
    let days = optional_days(&form.retention_days, "retention").and_then(|days| {
        Ok((
            days,
            optional_days(&form.review_interval_days, "review interval")?,
        ))
    });
    let result = match days {
        Ok((days, review_days)) => {
            let update = project_policy::AccessUpdate {
                interfaces: vec![
                    (Interface::ApiTokens, form.api_tokens.is_some()),
                    (Interface::Mcp, form.mcp.is_some()),
                    (Interface::Assistant, form.assistant.is_some()),
                    (Interface::Webhooks, form.webhooks.is_some()),
                    (Interface::Packages, form.packages.is_some()),
                    (
                        Interface::NotificationContent,
                        form.notification_content.is_some(),
                    ),
                    (Interface::ExternalLlm, form.external_llm.is_some()),
                    (Interface::DesktopSync, form.desktop_sync.is_some()),
                ],
                read_log: Some(form.read_log.is_some()),
                log_retention_days: Some(days),
                review_interval_days: Some(review_days),
            };
            project_policy::set_access(&st.db, &ctx.me, id, update).await
        }
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |_| {
        (back.clone(), "Interfaces and read log saved.".to_string())
    })
}

#[derive(Deserialize)]
pub struct ConditionsForm {
    /// One claim condition per line: `claim = value, value`.
    #[serde(default)]
    required_claims: String,
    /// One address range per line (or separated by commas).
    #[serde(default)]
    allowed_networks: String,
    #[serde(default)]
    max_signin_age_hours: String,
}

/// The conditions update of the form; a malformed field is a 400.
fn conditions_update(
    form: &ConditionsForm,
) -> AppResult<crate::services::membership_conditions::ConditionsUpdate> {
    use crate::services::membership_conditions as mc;
    let hours = match form.max_signin_age_hours.trim() {
        "" => None,
        raw => Some(raw.parse::<i32>().map_err(|_| {
            AppError::BadRequest("The maximum sign-in age is a number of hours (or empty).".into())
        })?),
    };
    Ok(mc::ConditionsUpdate {
        required_claims: Some(mc::parse_claim_lines(&form.required_claims)?),
        allowed_networks: Some(
            form.allowed_networks
                .split(['\n', ','])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        max_signin_age_hours: Some(hours),
    })
}

/// `POST /projects/{id}/conditions` – managers of the group, admins: the
/// conditions on membership (#151, step 2).
pub async fn update_conditions(
    State(st): State<AppState>,
    ctx: WebCtx,
    origin: crate::auth::Origin,
    Path(id): Path<Uuid>,
    Form(form): Form<ConditionsForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = match conditions_update(&form) {
        Ok(update) => {
            crate::services::membership_conditions::set(&st.db, &ctx.me, id, update, &origin).await
        }
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |_| {
        (back.clone(), "Conditions on membership saved.".to_string())
    })
}

#[derive(Deserialize)]
pub struct ReviewPolicyForm {
    #[serde(default)]
    min_approvals: String,
    #[serde(default)]
    required_reviewers: String,
    #[serde(default)]
    role_group: String,
    #[serde(default)]
    obsolete_requires_change: Option<String>,
    #[serde(default)]
    requester_cannot_decide: Option<String>,
    #[serde(default)]
    concessions_need_review: Option<String>,
}

/// `POST /projects/{id}/review-policy` – managers of the group, admins.
pub async fn update_review_policy(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ReviewPolicyForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let min = form.min_approvals.trim();
    let result = match (min.is_empty(), min.parse::<i32>()) {
        (true, _) => Ok(None),
        (false, Ok(n)) => Ok(Some(n)),
        _ => Err(AppError::BadRequest(
            "Minimum approvals must be a number, or empty for all reviewers.".into(),
        )),
    };
    let result = match result {
        Ok(min_approvals) => {
            let input = project_policy::ReviewPolicyInput {
                min_approvals,
                required_usernames: form
                    .required_reviewers
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .map(str::to_string)
                    .collect(),
                role_group_code: Some(form.role_group),
                obsolete_requires_change: Some(form.obsolete_requires_change.is_some()),
                requester_cannot_decide: Some(form.requester_cannot_decide.is_some()),
                concessions_need_review: Some(form.concessions_need_review.is_some()),
            };
            project_policy::set_review_policy(&st.db, &ctx.me, id, &input).await
        }
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |_| {
        (back.clone(), "Saved the review policy".to_string())
    })
}

#[derive(Deserialize)]
pub struct ElevateForm {
    #[serde(default)]
    reason: String,
}

/// `POST /projects/{id}/elevate` – administrators, policy `with_reason`.
pub async fn elevate(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ElevateForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = project_policy::elevate(&st.db, &ctx.me, id, &form.reason).await;
    finish(&st, result, &back, |e| {
        (
            back.clone(),
            format!(
                "Administrator access opened until {}",
                e.expires_at.format("%H:%M UTC")
            ),
        )
    })
}

/// `POST /projects/{id}/elevate/end` – ends the running elevation early.
pub async fn end_elevation(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = project_policy::end_elevation(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |()| {
        (back.clone(), "Administrator access ended".to_string())
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

/// `POST /projects/{id}/members/{user_id}/demote` – managers of the group, admins.
pub async fn demote_member(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = projects::demote_manager(&st.db, &ctx.me, id, user_id).await;
    finish(&st, result, &back, |()| {
        (
            back.clone(),
            "The member is no longer a manager".to_string(),
        )
    })
}

/// `POST /projects/{id}/members/{user_id}/release-checkouts` – managers of
/// the group (for deactivated and former members), admins.
pub async fn release_member_checkouts(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}");
    let result = checkouts::release_held(&st.db, &ctx.me, user_id, Some(id)).await;
    finish(&st, result, &back, |outcomes| {
        (back.clone(), checkouts::release_summary(&outcomes))
    })
}

/// `POST /admin/users/{id}/release-checkouts` – administrators: every
/// check-out the user holds in groups the administrator can read.
pub async fn release_user_checkouts(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/admin/users/{id}");
    let result = async {
        ctx.me.require(Role::Admin)?;
        checkouts::release_held(&st.db, &ctx.me, id, None).await
    }
    .await;
    finish(&st, result, &back, |outcomes| {
        (back.clone(), checkouts::release_summary(&outcomes))
    })
}

/// `GET /users/{id}/holdings` – what a user still holds (issue #173):
/// administrators, and managers for the groups they manage.
pub async fn user_holdings(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let holdings = crate::services::offboarding::holdings(&st.db, &ctx.me, id).await?;
    let nothing = holdings.blocks_nothing();
    render(
        &st,
        &ctx,
        "admin/holdings.html",
        json!({ "h": holdings, "nothing": nothing }),
    )
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

/// `GET /admin/ai`: which AI the users have – the local-agent switch and the
/// server-side assistant (provider, model, host, limits; never the key).
pub async fn ai(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    render(
        &st,
        &ctx,
        "admin/ai.html",
        crate::services::assistant::admin_overview(
            st.config.local_agents,
            st.config.assistant.as_ref(),
        ),
    )
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
    let handover_targets: Vec<_> = users::reviewer_candidates(&st.db)
        .await?
        .into_iter()
        .filter(|u| u.id != id)
        .collect();
    let memberships = projects::memberships_of(&st.db, id).await?;
    let held_checkouts = checkouts::held_count(&st.db, &ctx.me, id).await?;
    let sole_manager_of =
        crate::services::deactivation::sole_manager_codes(&st.db, &ctx.me, id).await?;
    render(
        &st,
        &ctx,
        "admin/user_edit.html",
        json!({
            "held_checkouts": held_checkouts,
            "sole_manager_of": sole_manager_of,
            "handover_targets": handover_targets,
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
    let (entries, total) = audit::list(
        &st.db,
        &ctx.me,
        &filter,
        Page::from_page_number(Some(page_no), 100),
    )
    .await?;
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

#[derive(Deserialize)]
pub struct AuditExportQuery {
    entity_type: Option<String>,
    username: Option<String>,
    action: Option<String>,
    /// `csv` (default) or `json`.
    format: Option<String>,
}

/// The audit filter of a query: empty values mean "any".
pub(crate) fn audit_filter(
    entity_type: &Option<String>,
    username: &Option<String>,
    action: &Option<String>,
) -> audit::AuditFilter {
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    audit::AuditFilter {
        entity_type: clean(entity_type),
        username: clean(username),
        action: clean(action),
    }
}

/// An audit log export as a download (CSV or JSON).
pub(crate) fn audit_download(
    entries: &[crate::models::AuditEntry],
    truncated: bool,
    json: bool,
) -> AppResult<Response> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let (content_type, filename, body) = if json {
        (
            "application/json",
            format!("teamcraft-audit-{stamp}.json"),
            serde_json::to_string_pretty(&json!({
                "exported_at": chrono::Utc::now(),
                "truncated": truncated,
                "entries": entries,
            }))
            .map_err(|e| AppError::Other(e.into()))?,
        )
    } else {
        (
            "text/csv; charset=utf-8",
            format!("teamcraft-audit-{stamp}.csv"),
            audit::to_csv(entries),
        )
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                util::content_disposition(&filename),
            ),
            (
                header::HeaderName::from_static("x-truncated"),
                truncated.to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

/// `GET /admin/audit/export?format=csv|json` – the filtered audit log as a
/// download (administrators; at most `audit::EXPORT_MAX` entries).
pub async fn audit_export(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<AuditExportQuery>,
) -> AppResult<Response> {
    let filter = audit_filter(&q.entity_type, &q.username, &q.action);
    let (entries, truncated) = audit::export(&st.db, &ctx.me, &filter).await?;
    audit_download(&entries, truncated, q.format.as_deref() == Some("json"))
}

#[derive(Deserialize)]
pub struct TemplateForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    file_pattern: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    csv_column: String,
    #[serde(default)]
    csv_key_column: String,
    #[serde(default)]
    csv_key_value: String,
    #[serde(default)]
    pick: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    lower_limit: String,
    #[serde(default)]
    upper_limit: String,
    #[serde(default)]
    load_case: String,
    #[serde(default)]
    solver: String,
}

impl TemplateForm {
    fn input(self) -> AppResult<TemplateInput> {
        Ok(TemplateInput {
            lower_limit: domain::parse_decimal("Lower limit", Some(&self.lower_limit))?,
            upper_limit: domain::parse_decimal("Upper limit", Some(&self.upper_limit))?,
            name: self.name,
            file_pattern: self.file_pattern,
            method: self.method,
            pattern: self.pattern,
            csv_column: self.csv_column,
            csv_key_column: self.csv_key_column,
            csv_key_value: self.csv_key_value,
            pick: self.pick,
            unit: self.unit,
            load_case: self.load_case,
            solver: self.solver,
            enabled: true,
        })
    }
}

/// `POST /projects/{id}/result-templates` – managers of the group, admins.
pub async fn add_result_template(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<TemplateForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}#result-templates");
    // a hidden group answers like a missing one before the form is looked at
    projects::get(&st.db, &ctx.me, id).await?;
    let result = match form.input() {
        Ok(input) => result_templates::create(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |t| {
        (
            back.clone(),
            format!("Extraction template {} added.", t.name),
        )
    })
}

/// `POST /result-templates/{id}/delete`
pub async fn delete_result_template(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let existing = result_templates::get(&st.db, &ctx.me, id).await?;
    let back = format!("/projects/{}#result-templates", existing.project_id);
    let result = result_templates::delete(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |t| {
        (
            back.clone(),
            format!("Extraction template {} removed.", t.name),
        )
    })
}

/// `GET /projects/{id}/access-review` – managers of the group, admins.
pub async fn access_review(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let report = access_review::report(&st.db, &ctx.me, id).await?;
    render(
        &st,
        &ctx,
        "projects/access_review.html",
        json!({ "project_id": id, "report": report, "max_note": access_review::MAX_NOTE_CHARS }),
    )
}

/// `GET /projects/{id}/access-log` – the read and download log of a group
/// with filters and paging (managers and administrators who read it).
pub async fn access_log_page(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<access_log::FilterQuery>,
) -> AppResult<Response> {
    let project = projects::get_for_management(&st.db, &ctx.me, id).await?;
    let (page, error) = match q.parse() {
        Ok(filter) => (
            Some(
                access_log::page(
                    &st.db,
                    &ctx.me,
                    id,
                    &access_log::Filter {
                        limit: Some(100),
                        ..filter
                    },
                )
                .await?,
            ),
            None,
        ),
        // a bad field is shown next to the form, the rights are still checked
        Err(AppError::BadRequest(msg)) => {
            access_log::list(
                &st.db,
                &ctx.me,
                id,
                &access_log::Filter {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await?;
            (None, Some(msg))
        }
        Err(e) => return Err(e),
    };
    // the filter as a query string, for the paging links
    let fields: Vec<(&str, Option<String>)> = vec![
        ("user", q.user.clone()),
        ("kind", q.kind.clone()),
        ("channel", q.channel.clone()),
        ("object", q.object.clone()),
        ("since", q.since.clone()),
        ("until", q.until.clone()),
    ];
    let filtered = fields
        .iter()
        .any(|(_, v)| v.as_deref().is_some_and(|v| !v.trim().is_empty()));
    let base: String = fields
        .iter()
        .filter_map(|(k, v)| {
            v.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}={}&", util::pct_encode(v)))
        })
        .collect();
    let link = |before: Option<Uuid>| {
        let query = match before {
            Some(b) => format!("{base}before={b}"),
            None => base.trim_end_matches('&').to_string(),
        };
        if query.is_empty() {
            format!("/projects/{id}/access-log")
        } else {
            format!("/projects/{id}/access-log?{query}")
        }
    };
    let read_log = {
        let mut conn = st.db.acquire().await?;
        project_policy::settings(&mut conn, id).await?.read_log
    };
    let kinds: Vec<Value> = access_log::Kind::ALL
        .iter()
        .map(|k| json!({ "value": k.as_str(), "label": k.label() }))
        .collect();
    let channels: Vec<Value> = access_log::Channel::ALL
        .iter()
        .map(|c| json!({ "value": c.as_str(), "label": c.label() }))
        .collect();
    let (entries, next) = match page {
        Some(p) => (p.entries, p.next),
        None => (Vec::new(), None),
    };
    render(
        &st,
        &ctx,
        "projects/access_log.html",
        json!({
            "project": project,
            "query": q,
            "entries": entries,
            "next_link": next.map(|n| link(Some(n))),
            "first_page_link": q.before.map(|_| link(None)),
            "filtered": filtered,
            "read_log": read_log,
            "error": error,
            "kinds": kinds,
            "channels": channels,
            "collapse_minutes": access_log::COLLAPSE_MINUTES,
        }),
    )
}

#[derive(Deserialize)]
pub struct ReviewForm {
    #[serde(default)]
    note: String,
}

/// `POST /projects/{id}/access-review` – confirms the review.
pub async fn confirm_access_review(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ReviewForm>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}/access-review");
    let result = access_review::confirm(&st.db, &ctx.me, id, &form.note).await;
    finish(&st, result, &back, |r| {
        (
            back.clone(),
            format!("Access review confirmed ({} members)", r.member_count),
        )
    })
}

/// `GET /projects/{id}/access-review.csv` – the evidence report.
pub async fn access_review_csv(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let report = access_review::report(&st.db, &ctx.me, id).await?;
    let filename = format!("{}_access_review.csv", report.project_code);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                util::content_disposition(&filename),
            ),
        ],
        access_review::to_csv(&report),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct HandoverForm {
    to: Uuid,
}

#[derive(Deserialize)]
pub struct OffboardForm {
    /// A user id, or empty for nobody.
    #[serde(default)]
    hand_over_to: String,
    release_checkouts: Option<String>,
    revoke_tokens: Option<String>,
    confirm_no_manager: Option<String>,
}

/// The successor field of the offboarding form: a user id or empty.
fn successor(raw: &str) -> AppResult<Option<Uuid>> {
    match raw.trim() {
        "" => Ok(None),
        id => Uuid::parse_str(id)
            .map(Some)
            .map_err(|_| AppError::BadRequest("Choose the successor from the list.".into())),
    }
}

/// `POST /admin/users/{id}/offboard` – deactivates a leaver with the chosen
/// offboarding steps (issue #173, step 4).
pub async fn offboard(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<OffboardForm>,
) -> AppResult<Response> {
    let back = format!("/admin/users/{id}");
    let result = async {
        let opts = crate::services::deactivation::Offboarding {
            release_checkouts: form.release_checkouts.is_some(),
            hand_over_to: successor(&form.hand_over_to)?,
            revoke_tokens: form.revoke_tokens.is_some(),
            confirm_no_manager: form.confirm_no_manager.is_some(),
        };
        crate::services::deactivation::deactivate(&st.db, &ctx.me, id, &opts).await
    }
    .await;
    finish(&st, result, &back, |report| (back.clone(), report.summary()))
}

/// Moves the work of a user to another one and shows what was moved.
pub async fn handover(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<HandoverForm>,
) -> AppResult<Response> {
    let back = format!("/admin/users/{id}");
    let result = ownership::hand_over(&st.db, &ctx.me, id, form.to).await;
    finish(&st, result, &back, |report| {
        (back.clone(), handover_summary(&report))
    })
}

/// Flash text of a hand-over: counts and the first objects that stayed.
fn handover_summary(report: &ownership::HandoverReport) -> String {
    let mut text = format!(
        "Handed over the work of {} to {}: {} items, {} changes, {} requests, {} reviews.",
        report.from_name,
        report.to_name,
        report.count("item"),
        report.count("change"),
        report.count("request"),
        report.count("review"),
    );
    if !report.skipped.is_empty() {
        text.push_str(&format!(" {} not moved:", report.skipped.len()));
        for e in report.skipped.iter().take(4) {
            text.push_str(&format!(" {} {} ({});", e.kind, e.number, e.reason));
        }
    }
    if report.truncated {
        text.push_str(" More remains: run the hand-over again.");
    }
    text
}
