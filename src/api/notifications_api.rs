//! Notifications and watch lists.

use super::*;

// ---------------------------------------------------------------------------
// Notifications and watch lists
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct NotificationQuery {
    unread: Option<bool>,
    limit: Option<i64>,
}

/// My newest notifications and the unread count.
pub(super) async fn list_notifications(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<NotificationQuery>,
) -> AppResult<Json<Value>> {
    let list = notifications::list(
        &st.db,
        &me,
        q.unread.unwrap_or(false),
        q.limit.unwrap_or(50),
    )
    .await?;
    let unread = notifications::unread_count(&st.db, &me).await?;
    Ok(Json(json!({ "unread": unread, "notifications": list })))
}

#[derive(Deserialize, Default)]
pub(super) struct MarkRead {
    /// Notification ids; all if missing.
    ids: Option<Vec<Uuid>>,
}

/// Marks notifications as read: `{ids: [...]}`, or all with an empty body.
pub(super) async fn read_notifications(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let ids = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        serde_json::from_slice::<MarkRead>(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON: {e}")))?
            .ids
    };
    let unread = notifications::mark_read(&st.db, &me, ids.as_deref()).await?;
    Ok(Json(json!({ "unread": unread })))
}

pub(super) async fn get_notification_settings(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(notifications::settings(&st.db, me.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct NotificationSettingsBody {
    email: bool,
    mattermost: bool,
    mattermost_username: Option<String>,
    #[serde(default)]
    muted: Vec<String>,
}

pub(super) async fn put_notification_settings(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<NotificationSettingsBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        notifications::update_settings(
            &st.db,
            &me,
            notifications::SettingsUpdate {
                email: body.email,
                mattermost: body.mattermost,
                mattermost_username: body.mattermost_username,
                muted: body.muted,
            },
        )
        .await?
    )))
}

pub(super) async fn test_notification(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<StatusCode> {
    notifications::send_test(&st.db, &me).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(super) async fn list_watches(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(notifications::watched(&st.db, &me).await?)))
}

pub(super) async fn watch_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: axum::http::Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let watching = notifications::set_watch(
        &st.db,
        &me,
        "item",
        item.id,
        method != axum::http::Method::DELETE,
    )
    .await?;
    Ok(Json(json!({ "item": item.number, "watching": watching })))
}

pub(super) async fn watch_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: axum::http::Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let watching = notifications::set_watch(
        &st.db,
        &me,
        "change",
        change.id,
        method != axum::http::Method::DELETE,
    )
    .await?;
    Ok(Json(
        json!({ "change": change.number, "watching": watching }),
    ))
}
