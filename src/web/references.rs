//! References between items and impacts (change propagation) in the web UI.

use axum::{
    extract::{Path, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{finish, WebCtx};
use crate::{
    domain::ReferenceKind,
    error::AppError,
    error::AppResult,
    services::{impacts, references},
    util, AppState,
};

#[derive(Deserialize)]
pub struct AddReferenceForm {
    #[serde(default)]
    item: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    note: String,
}

pub async fn add(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddReferenceForm>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}#references");
    let result = async {
        let kind = ReferenceKind::parse(&form.kind)
            .ok_or_else(|| AppError::BadRequest("Choose the kind of reference.".into()))?;
        references::add(
            &st.db,
            &ctx.me,
            id,
            references::NewReference {
                to_number: form.item.clone(),
                kind,
                note: form.note.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |r| {
        (
            back.clone(),
            format!(
                "Added: {} {}",
                ReferenceKind::parse(&r.kind).map_or("", |k| k.label()),
                r.to_number
            ),
        )
    })
}

pub async fn delete(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = references::remove(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |r| {
        (
            back.clone(),
            format!("Removed the reference to {}", r.to_number),
        )
    })
}

#[derive(Deserialize)]
pub struct AcknowledgeForm {
    #[serde(default)]
    note: String,
    back: Option<String>,
}

pub async fn acknowledge(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AcknowledgeForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = impacts::acknowledge(&st.db, &ctx.me, id, &form.note).await;
    finish(&st, result, &back, |i| {
        (
            back.clone(),
            format!(
                "{} checked against {}/{}",
                i.item_number, i.source_number, i.source_code
            ),
        )
    })
}

#[derive(Deserialize)]
pub struct AcknowledgeAllForm {
    #[serde(default)]
    note: String,
    /// Comma-separated ids of the impacts shown (empty: all open ones).
    #[serde(default)]
    impacts: String,
    back: Option<String>,
}

/// "Acknowledge all": every open impact shown for a dependent revision.
pub async fn acknowledge_all(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AcknowledgeAllForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = async {
        let only = impacts::parse_ids(&form.impacts)?;
        impacts::acknowledge_all(&st.db, &ctx.me, id, only.as_deref(), &form.note).await
    }
    .await;
    finish(&st, result, &back, |a| {
        let n = a.acknowledged.len();
        let mut message = format!(
            "{}/{} checked: {n} impact{} acknowledged",
            a.item_number,
            a.revision_code,
            if n == 1 { "" } else { "s" }
        );
        if a.skipped > 0 {
            message.push_str(&format!(" ({} resolved in the meantime)", a.skipped));
        }
        (back.clone(), message)
    })
}
