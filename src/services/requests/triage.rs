//! Triage and decision: status moves, assignment, accept/reject/defer, reopen, withdraw.

use super::*;

/// Moves the request to `to`, checking the transition rules.
pub(super) async fn set_status(
    conn: &mut PgConnection,
    request: &ChangeRequest,
    to: RequestStatus,
) -> AppResult<()> {
    if !status_of(request).can_transition_to(to) {
        return Err(not_now(request));
    }
    let closes = !to.is_open();
    sqlx::query(
        "UPDATE change_requests SET status = $2, updated_at = now(),
                closed_at = CASE WHEN $3 THEN now() ELSE NULL END
         WHERE id = $1",
    )
    .bind(request.id)
    .bind(to.as_str())
    .bind(closes)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Starts the triage (new → in triage); an unassigned request is assigned to
/// the triaging user.
pub async fn start_triage(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "triage"));
    }
    set_status(&mut tx, &request, RequestStatus::InTriage).await?;
    if request.assignee.is_none() {
        sqlx::query("UPDATE change_requests SET assignee = $2 WHERE id = $1")
            .bind(id)
            .bind(actor.id)
            .execute(&mut *tx)
            .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.triage",
        "change_request",
        Some(id),
        format!("Started the triage of {}", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} is in triage", request.number),
        format!("{} – {} looks into it.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Assigns the request to a CAD editor or admin (`None` = nobody).
pub async fn assign(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    username: Option<&str>,
) -> AppResult<ChangeRequest> {
    let assignee = match username.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(name) => {
            let user = users::find_by_username(pool, name)
                .await?
                .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{name}'.")))?;
            if !user.active {
                return Err(AppError::BadRequest(format!(
                    "{} is deactivated.",
                    user.display_name
                )));
            }
            if Role::parse(&user.role).unwrap_or(Role::Viewer) < Role::Editor {
                return Err(AppError::BadRequest(format!(
                    "{} has the viewer role; assign a CAD editor or an admin.",
                    user.display_name
                )));
            }
            Some(user)
        }
    };
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "assign"));
    }
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    if let Some(user) = &assignee {
        // The check below looks at every item; for items the actor cannot
        // see it would tell who is a member of their group.
        if !projects.iter().all(|p| actor.can_see_project(*p)) {
            return Err(AppError::Forbidden(
                "You can no longer see all affected items of this request.".into(),
            ));
        }
        let blind = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM change_request_items cri
                           JOIN items i ON i.id = cri.item_id
                           JOIN users u ON u.id = $2
                           WHERE cri.request_id = $1 AND NOT {})",
            access::user_sees_project("u", "i.project_id")
        ))
        .bind(id)
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        if blind {
            return Err(AppError::Conflict(format!(
                "{} cannot see every affected item of {} (not a member of a restricted project group).",
                user.display_name, request.number
            )));
        }
    }
    sqlx::query("UPDATE change_requests SET assignee = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(assignee.as_ref().map(|u| u.id))
        .execute(&mut *tx)
        .await?;
    let who = assignee
        .as_ref()
        .map(|u| u.display_name.clone())
        .unwrap_or_else(|| "nobody".into());
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.assign",
        "change_request",
        Some(id),
        format!("Assigned {} to {who}", request.number),
        json!({ "assignee": assignee.as_ref().map(|u| u.id) }),
    )
    .await?;
    if let Some(user) = &assignee {
        tell(
            &mut tx,
            actor,
            &request,
            &[user.id],
            format!("{} was assigned to you", request.number),
            format!("{} – assigned by {}.", request.title, actor.display_name),
        )
        .await?;
    }
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Accepts, rejects or defers a request. Rejecting and deferring need a reason.
/// In a group whose review policy has `requester_cannot_decide`, the
/// requester may not decide (403) – an administrator neither.
pub async fn decide(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    decision: RequestDecision,
    comment: &str,
) -> AppResult<ChangeRequest> {
    let comment = domain::limit_text("Comment", comment, 4000)?;
    if decision != RequestDecision::Accept && comment.is_empty() {
        return Err(AppError::BadRequest(
            "Please give a reason when you reject or defer a change request.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "decide on"));
    }
    if request.requested_by == actor.id
        && crate::services::project_policy::requester_cannot_decide(&mut tx, &projects).await?
    {
        return Err(AppError::Forbidden(format!(
            "You raised {}; in its project group another CAD editor decides on it.",
            request.number
        )));
    }
    let to = decision.status();
    set_status(&mut tx, &request, to).await?;
    sqlx::query(
        "UPDATE change_requests SET decision_comment = $2, decided_by = $3, decided_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&comment)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    let verb = match decision {
        RequestDecision::Accept => "accepted",
        RequestDecision::Reject => "rejected",
        RequestDecision::Defer => "deferred",
    };
    audit::record(
        &mut tx,
        Some(actor),
        &format!("change_request.{verb}"),
        "change_request",
        Some(id),
        format!(
            "{} {verb}{}",
            request.number,
            if comment.is_empty() {
                String::new()
            } else {
                format!(": {comment}")
            }
        ),
        json!({ "comment": comment }),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} was {verb} by {}", request.number, actor.display_name),
        if comment.is_empty() {
            match decision {
                RequestDecision::Accept => {
                    format!("{} – a change order follows.", request.title)
                }
                _ => request.title.clone(),
            }
        } else {
            format!("{} – {comment}", request.title)
        },
    )
    .await?;
    let decided = lock(&mut tx, actor, id).await?;
    webhooks::emit(
        &mut tx,
        Some(actor),
        WebhookEvent::ChangeRequestDecided,
        &projects,
        json!({ "change_request": webhooks::request_object(&decided),
                "decision": { "decision": verb, "comment": comment, "decided_by": actor.username } }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Takes a deferred request up again (deferred → in triage).
pub async fn reopen(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "reopen"));
    }
    set_status(&mut tx, &request, RequestStatus::InTriage).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.reopen",
        "change_request",
        Some(id),
        format!("Took up {} again", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} is in triage again", request.number),
        format!("{} – reopened by {}.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// The requester (or an admin) takes the request back before the decision.
pub async fn withdraw(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    if request.requested_by != actor.id && !actor.is_admin() {
        return Err(AppError::Forbidden(format!(
            "Only the requester or an administrator can withdraw {}.",
            request.number
        )));
    }
    set_status(&mut tx, &request, RequestStatus::Withdrawn).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.withdraw",
        "change_request",
        Some(id),
        format!("Withdrew {}", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} was withdrawn", request.number),
        format!("{} – withdrawn by {}.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}
