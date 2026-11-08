//! As-built records in the web UI: the build record and the deviations of
//! an as-built revision, and "Record a built unit" on a design revision.

use axum::{
    extract::{Path, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{finish, parse_opt_uuid, WebCtx};
use crate::{
    domain::{DeviationKind, Disposition},
    error::{AppError, AppResult},
    services::as_built::{self, DeviationInput, NewUnit, RecordInput},
    util, AppState,
};

fn back_to(id: Uuid, anchor: &str) -> String {
    format!("/revisions/{id}#{anchor}")
}

#[derive(Deserialize)]
pub struct RecordForm {
    #[serde(default)]
    design: String,
    #[serde(default)]
    design_revision: String,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    built_on: String,
    #[serde(default)]
    note: String,
}

/// `POST /revisions/{id}/as-built/record`
pub async fn set_record(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<RecordForm>,
) -> AppResult<Response> {
    let back = back_to(id, "as-built");
    let result = async {
        let built_on = as_built::parse_built_on(Some(&form.built_on))?;
        as_built::set_record(
            &st.db,
            &ctx.me,
            id,
            RecordInput {
                design_number: form.design.clone(),
                design_revision: form.design_revision.clone(),
                serial_number: form.serial_number.clone(),
                built_on,
                note: form.note.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |r| {
        (
            back.clone(),
            format!("Built to {}/{}", r.design_number, r.design_code),
        )
    })
}

#[derive(Deserialize)]
pub struct DeviationForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    feature: String,
    #[serde(default)]
    designed: String,
    #[serde(default)]
    built: String,
    #[serde(default)]
    disposition: String,
    #[serde(default)]
    note: String,
    back: Option<String>,
}

impl DeviationForm {
    fn input(&self) -> AppResult<DeviationInput> {
        Ok(DeviationInput {
            kind: DeviationKind::parse(&self.kind)
                .ok_or_else(|| AppError::BadRequest("Choose the kind of deviation.".into()))?,
            feature: self.feature.clone(),
            designed: self.designed.clone(),
            built: self.built.clone(),
            disposition: Disposition::parse(&self.disposition)
                .ok_or_else(|| AppError::BadRequest("Choose a disposition.".into()))?,
            note: self.note.clone(),
        })
    }
}

/// `POST /revisions/{id}/as-built/deviations`
pub async fn add_deviation(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<DeviationForm>,
) -> AppResult<Response> {
    let back = back_to(id, "deviations");
    let result = match form.input() {
        Ok(input) => as_built::add_deviation(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |d| {
        (back.clone(), format!("Added the deviation {}", d.feature))
    })
}

/// `POST /as-built-deviations/{id}`
pub async fn update_deviation(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<DeviationForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = match form.input() {
        Ok(input) => as_built::update_deviation(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |d| {
        (back.clone(), format!("Saved the deviation {}", d.feature))
    })
}

/// `POST /as-built-deviations/{id}/delete`
pub async fn delete_deviation(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = as_built::remove_deviation(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |d| {
        (back.clone(), format!("Removed the deviation {}", d.feature))
    })
}

#[derive(Deserialize)]
pub struct UnitForm {
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    built_on: String,
    #[serde(default)]
    number: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    project: Option<String>,
    /// Checkbox: copy the design's BOM.
    #[serde(default)]
    with_bom: Option<String>,
}

/// `POST /revisions/{id}/built-units` – records a unit built to this design
/// revision and opens its as-built revision.
pub async fn record_unit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<UnitForm>,
) -> AppResult<Response> {
    let back = back_to(id, "built-units");
    let result = async {
        let built_on = as_built::parse_built_on(Some(&form.built_on))?;
        let project_id = parse_opt_uuid("Project", form.project.as_deref())?;
        as_built::record_unit(
            &st.db,
            &ctx.me,
            id,
            NewUnit {
                number: Some(form.number.clone()),
                name: form.name.clone(),
                serial_number: form.serial_number.clone(),
                built_on,
                note: String::new(),
                project_id,
                with_bom: form.with_bom.is_some(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |rev| {
        (
            format!("/revisions/{}#as-built", rev.id),
            format!("Recorded {} – {}", rev.item_number, rev.name),
        )
    })
}
