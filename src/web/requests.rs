//! Engineering change requests (ECR): list, raising, discussion, triage,
//! decision and the hand-over to a change order.

use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::header,
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use super::{
    finish, is_user_error, pagination, parse_opt_uuid, redirect_flash, render,
    revisions::multipart_error, Flash, WebCtx, PER_PAGE,
};
use crate::{
    auth::WebUser,
    domain::{Priority, RequestDecision, RequestReason, RequestStatus},
    error::{AppError, AppResult},
    models::RequestAttachment,
    services::{
        audit, changes, items, notifications,
        requests::{self, AffectedItem, RequestFilter, RequestInput},
        revisions, users, Page,
    },
    util, AppState,
};

pub(crate) fn reasons() -> Value {
    json!(RequestReason::ALL
        .iter()
        .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
        .collect::<Vec<_>>())
}

fn statuses() -> Value {
    json!(RequestStatus::ALL
        .iter()
        .map(|s| json!({ "value": s.as_str(), "label": s.label() }))
        .collect::<Vec<_>>())
}

fn priorities() -> Value {
    json!(Priority::ALL.iter().map(|p| p.as_str()).collect::<Vec<_>>())
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// A status, `open` (default) or `all`.
    status: Option<String>,
    /// `mine`, `assigned` or empty (everybody's).
    scope: Option<String>,
    /// Item number.
    item: Option<String>,
    page: Option<i64>,
}

pub async fn list(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    let status_raw = q.status.as_deref().unwrap_or("open").trim().to_string();
    let scope = q.scope.as_deref().unwrap_or("").trim().to_string();
    let item_number = q.item.as_deref().unwrap_or("").trim().to_ascii_uppercase();
    // an unknown item number simply finds nothing
    let item_id = if item_number.is_empty() {
        None
    } else {
        match items::get_by_number(&st.db, &ctx.me, &item_number).await {
            Ok(item) => Some(item.id),
            Err(AppError::NotFound(_)) => Some(Uuid::nil()),
            Err(e) => return Err(e),
        }
    };
    let filter = RequestFilter {
        status: RequestStatus::parse(&status_raw),
        open_only: status_raw == "open",
        requested_by: (scope == "mine").then_some(ctx.me.id),
        assignee: (scope == "assigned").then_some(ctx.me.id),
        item_id,
    };
    let page_no = q.page.unwrap_or(1).max(1);
    let (rows, total) = requests::list(
        &st.db,
        &ctx.me,
        &filter,
        Page::from_page_number(Some(page_no), PER_PAGE),
    )
    .await?;
    let base_query = format!(
        "status={}&scope={}&item={}",
        util::pct_encode(&status_raw),
        util::pct_encode(&scope),
        util::pct_encode(&item_number)
    );
    render(
        &st,
        &ctx,
        "requests/list.html",
        json!({
            "requests": rows,
            "filter": { "status": status_raw, "scope": scope, "item": item_number },
            "statuses": statuses(),
            "base_query": base_query,
            "pagination": pagination(page_no, total, PER_PAGE),
        }),
    )
}

#[derive(Deserialize)]
pub struct NewQuery {
    revision: Option<String>,
    item: Option<String>,
    /// A file name the request is about (from a file row or the desktop app).
    file: Option<String>,
}

#[derive(Deserialize, serde::Serialize, Default)]
pub struct RequestForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    due_date: String,
    #[serde(default)]
    item_number: String,
    #[serde(default)]
    revision_id: String,
    #[serde(default)]
    note: String,
}

async fn render_new(
    st: &AppState,
    ctx: &WebCtx,
    form: &RequestForm,
    error: Option<String>,
) -> AppResult<Response> {
    let revision = match parse_opt_uuid("Revision", Some(&form.revision_id)).unwrap_or(None) {
        Some(id) => Some(revisions::get(&st.db, &ctx.me, id).await?),
        None => None,
    };
    render(
        st,
        ctx,
        "requests/new.html",
        json!({ "form": form, "error": error, "priorities": priorities(), "reasons": reasons(),
                "revision": revision }),
    )
}

/// The form for a new request – every signed-in user may raise one. A
/// `revision` (or `item`) and a `file` name prefill it.
pub async fn new_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<NewQuery>,
) -> AppResult<Response> {
    let mut form = RequestForm {
        priority: "normal".into(),
        reason: "defect".into(),
        revision_id: q.revision.unwrap_or_default(),
        item_number: q.item.unwrap_or_default(),
        ..Default::default()
    };
    if let Some(file) = q.file.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        let file = requests::clean_filename(file);
        form.note = format!("File: {file}");
        form.description = format!("Found in {file}:\n\n");
    }
    render_new(&st, &ctx, &form, None).await
}

fn request_input(form: &RequestForm) -> AppResult<RequestInput> {
    Ok(RequestInput {
        title: form.title.clone(),
        description: form.description.clone(),
        reason: RequestReason::parse(&form.reason)
            .ok_or_else(|| AppError::BadRequest("Unknown reason.".into()))?,
        priority: Priority::parse(&form.priority)
            .ok_or_else(|| AppError::BadRequest("Unknown priority.".into()))?,
        due_date: requests::parse_due_date(Some(&form.due_date))?,
    })
}

/// The affected item of the form: the given revision, or the item number.
async fn affected_of(
    st: &AppState,
    me: &crate::auth::Actor,
    form: &RequestForm,
) -> AppResult<Option<AffectedItem>> {
    if let Some(rev_id) = parse_opt_uuid("Revision", Some(&form.revision_id))? {
        let rev = revisions::get(&st.db, me, rev_id).await?;
        return Ok(Some(AffectedItem {
            item_id: rev.item_id,
            revision_id: Some(rev.id),
            note: form.note.clone(),
        }));
    }
    let number = form.item_number.trim();
    if number.is_empty() {
        return Ok(None);
    }
    let item = items::get_by_number(&st.db, me, number).await?;
    Ok(Some(AffectedItem {
        item_id: item.id,
        revision_id: None,
        note: form.note.clone(),
    }))
}

pub async fn create(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<RequestForm>,
) -> AppResult<Response> {
    let created = async {
        let input = request_input(&form)?;
        let affected = affected_of(&st, &ctx.me, &form).await?;
        requests::create(&st.db, &ctx.me, input, affected.into_iter().collect()).await
    }
    .await;
    match created {
        Ok(r) => Ok(redirect_flash(
            &st,
            &format!("/requests/{}", r.id),
            Flash::success(format!(
                "Raised change request {} – the CAD editors of the affected project groups are notified",
                r.number
            )),
        )),
        Err(e) if is_user_error(&e) => render_new(&st, &ctx, &form, Some(e.public_message())).await,
        Err(e) => Err(e),
    }
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let request = requests::get(&st.db, &ctx.me, id).await?;
    let ids = [id];
    let (affected, comments, attachments, (linked, hidden_changes), history, watching, can) = tokio::try_join!(
        requests::items(&st.db, &ctx.me, id),
        requests::comments(&st.db, id),
        requests::attachments(&st.db, id),
        requests::linked_changes(&st.db, &ctx.me, id),
        audit::for_entities(&st.db, &ctx.me, &ids, 100),
        notifications::is_watching(&st.db, ctx.me.id, "change_request", id),
        requests::abilities_of(&st.db, &ctx.me, &request),
    )?;
    let candidates = if can.assign {
        users::reviewer_candidates(&st.db).await?
    } else {
        Vec::new()
    };
    // Open change orders an accepted request can be linked to.
    let open_changes: Vec<_> = if can.implement {
        changes::drafts(&st.db, &ctx.me)
            .await?
            .into_iter()
            .filter(|c| !linked.iter().any(|l| l.id == c.id))
            .collect()
    } else {
        Vec::new()
    };
    // What "Create change order" takes along.
    let in_work: Vec<_> = affected
        .iter()
        .filter(|i| i.latest_state == "in_work")
        .collect();
    let images: Vec<&RequestAttachment> = attachments
        .iter()
        .filter(|a| a.content_type.starts_with("image/"))
        .collect();
    render(
        &st,
        &ctx,
        "requests/show.html",
        json!({
            "request": request,
            "status_label": requests::status_of(&request).label(),
            "reason_label": RequestReason::parse(&request.reason).map(|r| r.label()).unwrap_or("Other"),
            "items": affected,
            "in_work": in_work,
            "comments": comments,
            "attachments": attachments,
            "images": images,
            "changes": linked,
            "hidden_changes": hidden_changes,
            "open_changes": open_changes,
            "candidates": candidates,
            "history": history,
            "watching": watching,
            "can": can,
            "priorities": priorities(),
            "reasons": reasons(),
            "max_attachment_mb": requests::MAX_ATTACHMENT_BYTES / (1024 * 1024),
        }),
    )
}

fn back_to(id: Uuid) -> String {
    format!("/requests/{id}")
}

pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<RequestForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = match request_input(&form) {
        Ok(input) => requests::update(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |r| {
        (back.clone(), format!("Saved {}", r.number))
    })
}

#[derive(Deserialize)]
pub struct CommentForm {
    #[serde(default)]
    body: String,
}

pub async fn comment(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<CommentForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::comment(&st.db, &ctx.me, id, &form.body).await;
    finish(&st, result, &back, |_| {
        (format!("{back}#discussion"), "Comment added".into())
    })
}

pub async fn triage(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::start_triage(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |r| {
        (back.clone(), format!("{} is in triage", r.number))
    })
}

#[derive(Deserialize)]
pub struct AssignForm {
    #[serde(default)]
    username: String,
}

pub async fn assign(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AssignForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::assign(&st.db, &ctx.me, id, Some(&form.username)).await;
    finish(&st, result, &back, |r| {
        (
            back.clone(),
            match r.assignee_name {
                Some(name) => format!("{} is assigned to {name}", r.number),
                None => format!("{} is not assigned to anybody", r.number),
            },
        )
    })
}

#[derive(Deserialize)]
pub struct DecideForm {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    comment: String,
}

pub async fn decide(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<DecideForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = match RequestDecision::parse(&form.decision) {
        Some(d) => requests::decide(&st.db, &ctx.me, id, d, &form.comment).await,
        None => Err(AppError::BadRequest(
            "Choose accept, reject or defer.".into(),
        )),
    };
    finish(&st, result, &back, |r| {
        let message = match requests::status_of(&r) {
            RequestStatus::Accepted => format!(
                "{} is accepted – create its change order or link an open one",
                r.number
            ),
            s => format!("{} is {}", r.number, s.label().to_lowercase()),
        };
        (back.clone(), message)
    })
}

pub async fn reopen(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::reopen(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |r| {
        (back.clone(), format!("{} is in triage again", r.number))
    })
}

pub async fn withdraw(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::withdraw(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |r| {
        (back.clone(), format!("{} was withdrawn", r.number))
    })
}

#[derive(Deserialize)]
pub struct CreateChangeForm {
    #[serde(default)]
    title: String,
}

pub async fn create_change(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<CreateChangeForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    match requests::create_change(&st.db, &ctx.me, id, Some(&form.title), None).await {
        Ok(created) => {
            let to = format!("/changes/{}", created.change.id);
            let mut message = format!("Created {}", created.change.number);
            if !created.added.is_empty() {
                message.push_str(&format!(" with {}", created.added.join(", ")));
            }
            if created.skipped.is_empty() {
                message.push_str(" – add reviewers and submit it");
                Ok(redirect_flash(&st, &to, Flash::success(message)))
            } else {
                message.push_str(&format!(". Not added: {}", created.skipped.join("; ")));
                Ok(redirect_flash(&st, &to, Flash::info(message)))
            }
        }
        Err(e) if is_user_error(&e) => {
            Ok(redirect_flash(&st, &back, Flash::error(e.public_message())))
        }
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
pub struct LinkForm {
    #[serde(default)]
    change: String,
}

pub async fn link_change(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<LinkForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = async {
        let change_id = match parse_opt_uuid("Change", Some(&form.change)) {
            Ok(Some(id)) => id,
            Ok(None) => return Err(AppError::BadRequest("Choose a change order.".into())),
            // a change number such as CHG-000004
            Err(_) => {
                changes::get_by_number(&st.db, &ctx.me, &form.change)
                    .await?
                    .id
            }
        };
        requests::link_change(&st.db, &ctx.me, id, change_id).await
    }
    .await;
    finish(&st, result, &back, |c| {
        (back.clone(), format!("Linked {}", c.number))
    })
}

#[derive(Deserialize)]
pub struct AddItemForm {
    #[serde(default)]
    item_number: String,
    #[serde(default)]
    note: String,
}

pub async fn add_item(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddItemForm>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = async {
        let item = items::get_by_number(&st.db, &ctx.me, &form.item_number).await?;
        requests::add_item(
            &st.db,
            &ctx.me,
            id,
            AffectedItem {
                item_id: item.id,
                revision_id: None,
                note: form.note.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |label| {
        (back.clone(), format!("Added {label}"))
    })
}

pub async fn remove_item(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = requests::remove_item(&st.db, &ctx.me, id, item_id).await;
    finish(&st, result, &back, |_| {
        (back.clone(), "Removed the item".into())
    })
}

/// Reads the `file` field of a multipart upload into memory, at most
/// [`requests::MAX_ATTACHMENT_BYTES`]. Shared with the REST API.
pub(crate) async fn receive_attachment(multipart: &mut Multipart) -> AppResult<(String, Vec<u8>)> {
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().map(str::to_string).unwrap_or_default();
        if filename.trim().is_empty() {
            continue; // empty <input type=file>
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
            if bytes.len() + chunk.len() > requests::MAX_ATTACHMENT_BYTES {
                return Err(AppError::PayloadTooLarge(format!(
                    "Attachments may be at most {} MiB.",
                    requests::MAX_ATTACHMENT_BYTES / (1024 * 1024)
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok((filename, bytes));
    }
    Err(AppError::BadRequest("Choose a file to attach.".into()))
}

pub async fn attach(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = back_to(id);
    let result = async {
        let (filename, bytes) = receive_attachment(&mut multipart).await?;
        requests::add_attachment(&st.db, &st.vault, &ctx.me, id, &filename, &bytes).await
    }
    .await;
    finish(&st, result, &back, |a| {
        (
            format!("{back}#attachments"),
            format!("Attached {}", a.filename),
        )
    })
}

#[derive(Deserialize)]
pub struct AttachmentQuery {
    inline: Option<String>,
}

/// The bytes of an attachment; images are shown inline when asked for
/// (`?inline=1`), everything else downloads. Shared with the REST API.
pub(crate) async fn attachment_response(
    me: &crate::auth::Actor,
    st: &AppState,
    id: Uuid,
    inline: bool,
) -> AppResult<Response> {
    let a = requests::attachment(&st.db, me, id).await?;
    let file = st.vault.open_blob(&a.sha256).await?;
    let mut disposition = util::content_disposition(&a.filename);
    if inline && a.content_type.starts_with("image/") {
        disposition = disposition.replacen("attachment", "inline", 1);
    }
    Response::builder()
        .header(header::CONTENT_TYPE, a.content_type.as_str())
        .header(header::CONTENT_LENGTH, a.size_bytes)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        )
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|e| {
            AppError::Other(anyhow::anyhow!(
                "building the attachment response failed: {e}"
            ))
        })
}

pub async fn download_attachment(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
    Query(q): Query<AttachmentQuery>,
) -> AppResult<Response> {
    attachment_response(&me, &st, id, q.inline.as_deref() == Some("1")).await
}

pub async fn delete_attachment(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let result = requests::delete_attachment(&st.db, &ctx.me, id).await;
    let back = match &result {
        Ok(a) => back_to(a.request_id),
        Err(_) => match requests::attachment(&st.db, &ctx.me, id).await {
            Ok(a) => back_to(a.request_id),
            Err(_) => "/requests".into(),
        },
    };
    finish(&st, result, &back, |a| {
        (
            format!("{back}#attachments"),
            format!("Removed {}", a.filename),
        )
    })
}
