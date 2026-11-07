//! Notifications to the people of a request.

use super::*;

/// Requester, assignee, commenters and watchers of a request.
pub(super) async fn people(conn: &mut PgConnection, request_id: Uuid) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT requested_by FROM change_requests WHERE id = $1
         UNION SELECT assignee FROM change_requests WHERE id = $1 AND assignee IS NOT NULL
         UNION SELECT author_id FROM change_request_comments WHERE request_id = $1
         UNION SELECT user_id FROM watches WHERE entity_type = 'change_request' AND entity_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Who triages a new request: the active CAD editors and admins in the
/// project groups of the affected items, and the watchers of these items.
pub(super) async fn triagers(conn: &mut PgConnection, request_id: Uuid) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT u.id FROM change_request_items cri
         JOIN items i ON i.id = cri.item_id
         JOIN project_members pm ON pm.project_id = i.project_id
         JOIN users u ON u.id = pm.user_id
         WHERE cri.request_id = $1 AND u.active AND u.role IN ('editor', 'admin')
         UNION
         SELECT w.user_id FROM watches w JOIN change_request_items cri ON cri.item_id = w.entity_id
         WHERE w.entity_type = 'item' AND cri.request_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

pub(super) async fn tell(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
    recipients: &[Uuid],
    title: String,
    body: String,
) -> AppResult<()> {
    let notice = Notice::request(
        NotificationKind::ChangeRequest,
        request.id,
        &request.number,
        title,
        body,
    );
    notifications::notify(conn, Some(actor), recipients, &notice).await?;
    Ok(())
}

pub(super) async fn tell_people(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
    title: String,
    body: String,
) -> AppResult<()> {
    let people = people(conn, request.id).await?;
    tell(conn, actor, request, &people, title, body).await
}

/// [`tell_people`] with a text that names a change order (or items):
/// people who may see the request but not all of `named` get the
/// `redacted` title and body.
pub(super) async fn tell_people_naming(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
    (title, body): (String, String),
    named: audit::Mentions,
    (redacted_title, redacted_body): (String, String),
) -> AppResult<()> {
    let people = people(conn, request.id).await?;
    let notice = Notice::request(
        NotificationKind::ChangeRequest,
        request.id,
        &request.number,
        title,
        body,
    )
    .naming(named, redacted_title, redacted_body);
    notifications::notify(conn, Some(actor), &people, &notice).await?;
    Ok(())
}

pub(super) async fn touch(conn: &mut PgConnection, request_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE change_requests SET updated_at = now() WHERE id = $1")
        .bind(request_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
