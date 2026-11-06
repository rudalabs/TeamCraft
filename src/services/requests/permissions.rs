//! Who may triage and decide, and what a user may do with a request.

use super::*;

/// Project groups of the affected items (`None` = item without a group).
pub async fn project_ids(
    conn: &mut PgConnection,
    request_id: Uuid,
) -> AppResult<Vec<Option<Uuid>>> {
    Ok(sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT DISTINCT i.project_id FROM change_request_items cri JOIN items i ON i.id = cri.item_id
         WHERE cri.request_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether `actor` triages and decides a request on items of these project
/// groups: admins always, CAD editors who are members of one of them (any CAD
/// editor while no item is affected yet), viewers never.
pub fn may_triage(actor: &Actor, projects: &[Option<Uuid>]) -> bool {
    if actor.is_admin() {
        return true;
    }
    actor.can_edit() && (projects.is_empty() || projects.iter().any(|p| actor.can_edit_project(*p)))
}

/// What the actor may do with a request – the services enforce the same
/// rules; the web UI, the API and the desktop app show the matching actions.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Abilities {
    /// Change title, description, reason, priority, due date and the items.
    pub edit: bool,
    pub start_triage: bool,
    /// Accept, reject or defer.
    pub decide: bool,
    pub assign: bool,
    /// Deferred → in triage.
    pub reopen: bool,
    pub withdraw: bool,
    /// Create or link the change order of an accepted request.
    pub implement: bool,
    pub comment: bool,
    pub attach: bool,
}

pub fn abilities(actor: &Actor, request: &ChangeRequest, projects: &[Option<Uuid>]) -> Abilities {
    use RequestStatus::*;
    let status = status_of(request);
    let triage = may_triage(actor, projects);
    let requester = request.requested_by == actor.id;
    let undecided = matches!(status, New | InTriage);
    Abilities {
        edit: status.is_open() && (triage || (requester && undecided)),
        start_triage: triage && status == New,
        decide: triage && matches!(status, New | InTriage | Deferred),
        assign: triage && status.is_open(),
        reopen: triage && status == Deferred,
        withdraw: (requester || actor.is_admin()) && undecided,
        implement: triage && status == Accepted,
        comment: true,
        attach: status.is_open(),
    }
}

/// Loads the abilities of `actor` on a request (for pages and API responses).
pub async fn abilities_of(
    pool: &PgPool,
    actor: &Actor,
    request: &ChangeRequest,
) -> AppResult<Abilities> {
    let mut conn = pool.acquire().await?;
    let projects = project_ids(&mut conn, request.id).await?;
    Ok(abilities(actor, request, &projects))
}

pub(super) fn forbidden(request: &ChangeRequest, what: &str) -> AppError {
    AppError::Forbidden(format!(
        "Only CAD editors of the affected project groups or an administrator can {what} {}.",
        request.number
    ))
}

pub(super) fn not_now(request: &ChangeRequest) -> AppError {
    AppError::Conflict(format!(
        "{} is {}; this action is not possible in that state.",
        request.number,
        status_of(request).label()
    ))
}
