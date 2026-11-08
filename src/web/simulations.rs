//! Simulations in the web UI: basis links, result values (form, CSV/Excel
//! import, CSV export) and the comparison of results between revisions.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, parse_opt_uuid, render, WebCtx};
use crate::{
    domain::{self, BasisRole},
    error::{AppError, AppResult},
    services::{
        items, result_proposals, revisions,
        simulations::{self, NewBasis, ResultInput},
    },
    util, AppState,
};

fn back_to(id: Uuid, anchor: &str) -> String {
    format!("/revisions/{id}#{anchor}")
}

#[derive(Deserialize)]
pub struct AddBasisForm {
    #[serde(default)]
    item: String,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    note: String,
}

/// `POST /revisions/{id}/simulation/bases`
pub async fn add_basis(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddBasisForm>,
) -> AppResult<Response> {
    let back = back_to(id, "simulation-basis");
    let result = async {
        let role = BasisRole::parse(&form.role)
            .ok_or_else(|| AppError::BadRequest("Choose the role of the basis.".into()))?;
        simulations::add_basis(
            &st.db,
            &ctx.me,
            id,
            NewBasis {
                item_number: form.item.clone(),
                revision_code: form.revision.clone(),
                file_name: form.file.clone(),
                file_version_id: None,
                role,
                note: form.note.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |b| {
        let file = if b.file_name.is_empty() {
            String::new()
        } else {
            format!(", {} v{}", b.file_name, b.file_version.unwrap_or(0))
        };
        (
            back.clone(),
            format!("Based on {}/{}{file}", b.basis_number, b.basis_code),
        )
    })
}

/// `POST /simulation-bases/{id}/delete`
pub async fn delete_basis(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = simulations::remove_basis(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |b| {
        (
            back.clone(),
            format!("Removed the basis {}/{}", b.basis_number, b.basis_code),
        )
    })
}

#[derive(Deserialize)]
pub struct ResultForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
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
    #[serde(default)]
    solver_version: String,
    #[serde(default)]
    note: String,
    back: Option<String>,
}

impl ResultForm {
    fn input(&self) -> AppResult<ResultInput> {
        let value = domain::parse_decimal("Value", Some(&self.value))?
            .ok_or_else(|| AppError::BadRequest("Enter the value.".into()))?;
        Ok(ResultInput {
            name: self.name.clone(),
            value,
            unit: self.unit.clone(),
            lower_limit: domain::parse_decimal("Lower limit", Some(&self.lower_limit))?,
            upper_limit: domain::parse_decimal("Upper limit", Some(&self.upper_limit))?,
            load_case: self.load_case.clone(),
            solver: self.solver.clone(),
            solver_version: self.solver_version.clone(),
            note: self.note.clone(),
            source: String::new(),
        })
    }
}

/// `POST /revisions/{id}/simulation/results`
pub async fn add_result(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ResultForm>,
) -> AppResult<Response> {
    let back = back_to(id, "simulation-results");
    let result = match form.input() {
        Ok(input) => simulations::add_result(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |r| {
        (back.clone(), format!("Added the result {}", r.name))
    })
}

/// `POST /simulation-results/{id}`
pub async fn update_result(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ResultForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = match form.input() {
        Ok(input) => simulations::update_result(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |r| {
        (back.clone(), format!("Saved the result {}", r.name))
    })
}

/// `POST /simulation-results/{id}/delete`
pub async fn delete_result(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = simulations::remove_result(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |r| {
        (back.clone(), format!("Removed the result {}", r.name))
    })
}

/// `POST /revisions/{id}/simulation/results/import` – multipart: `file`
/// (CSV/TSV/Excel) and `replace=1` to drop the values missing in the file.
pub async fn import_results(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let back = back_to(id, "simulation-results");
    let result = async {
        let (file, fields) = super::receive_import(&mut multipart).await?;
        let (filename, bytes) = file
            .filter(|(name, bytes)| !name.is_empty() && !bytes.is_empty())
            .ok_or_else(|| AppError::BadRequest("Choose a CSV or Excel file.".into()))?;
        let rows =
            simulations::parse_results_table(&filename, &bytes).map_err(AppError::BadRequest)?;
        let replace = fields.get("replace").is_some_and(|v| v == "1");
        simulations::import_results(&st.db, &ctx.me, id, rows, replace).await
    }
    .await;
    finish(&st, result, &back, |s| {
        (
            back.clone(),
            format!(
                "Imported result values: {} added, {} updated, {} removed",
                s.added, s.updated, s.removed
            ),
        )
    })
}

/// `GET /revisions/{id}/simulation/results.csv`
pub async fn results_csv(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &ctx.me, id).await?;
    let results = simulations::results(&st.db, &ctx.me, id).await?;
    let filename = format!("{}_{}_results.csv", rev.item_number, rev.code);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                util::content_disposition(&filename),
            ),
        ],
        simulations::to_csv(&results),
    )
        .into_response())
}

#[derive(Deserialize, Default)]
pub struct CompareQuery {
    /// Revision id to compare with (default: the previous revision).
    with: Option<String>,
}

/// `GET /revisions/{id}/simulation/compare?with=` – result values of two
/// revisions of the same simulation side by side.
pub async fn compare(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &ctx.me, id).await?;
    let all = items::revisions(&st.db, &ctx.me, rev.item_id).await?;
    let other_id = match parse_opt_uuid("Revision", q.with.as_deref())? {
        Some(other) => other,
        None => all
            .iter()
            .filter(|r| r.seq < rev.seq)
            .max_by_key(|r| r.seq)
            .or_else(|| all.iter().find(|r| r.id != rev.id))
            .map(|r| r.id)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "{} has only one revision – nothing to compare with.",
                    rev.item_number
                ))
            })?,
    };
    let other = revisions::get(&st.db, &ctx.me, other_id).await?;
    let (before, after) = if other.seq < rev.seq {
        (other, rev.clone())
    } else {
        (rev.clone(), other)
    };
    let diffs = simulations::compare(&st.db, &before, &after).await?;
    let summary = simulations::compare_summary(&diffs);
    render(
        &st,
        &ctx,
        "revisions/simulation_compare.html",
        json!({
            "rev": rev,
            "before": before,
            "after": after,
            "revisions": all,
            "diffs": diffs,
            "summary": summary,
        }),
    )
}

/// `POST /simulation-proposals/{id}/accept` – makes a value proposed from a
/// report a result value (the revision must be checked out to the user).
pub async fn accept_proposal(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = result_proposals::accept(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |p| {
        (
            back.clone(),
            format!("Accepted the proposed value {}", p.name),
        )
    })
}

/// `POST /simulation-proposals/{id}/reject`
pub async fn reject_proposal(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = result_proposals::reject(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |p| {
        (
            back.clone(),
            format!("Rejected the proposed value {}", p.name),
        )
    })
}

#[derive(Deserialize)]
pub struct FillWithAiForm {
    #[serde(default)]
    wanted: String,
    #[serde(default)]
    limits: String,
    /// The ticked report files arrive as `file_0`, `file_1`, … (the value is
    /// the file name); a form cannot send a list otherwise.
    #[serde(flatten)]
    rest: std::collections::HashMap<String, String>,
}

/// The answer of the assistant, flattened for the flash message.
fn flash_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `POST /revisions/{id}/simulation/fill-with-ai` – the *Fill with AI…* box
/// of the Results card: the assistant proposes the listed values as drafts.
pub async fn fill_with_ai(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<FillWithAiForm>,
) -> AppResult<Response> {
    let back = back_to(id, "simulation-results");
    let mut files: Vec<String> = form
        .rest
        .iter()
        .filter(|(k, v)| k.starts_with("file_") && !v.trim().is_empty())
        .map(|(_, v)| v.trim().to_string())
        .collect();
    files.sort();
    let result = crate::services::assistant::fill_result_values(
        &st,
        &ctx.me,
        id,
        &form.wanted,
        &form.limits,
        &files,
    )
    .await;
    finish(&st, result, &back, |(run, created)| {
        let head = match created {
            0 => "The assistant proposed no new values.".to_string(),
            1 => "The assistant proposed 1 value (a draft below).".to_string(),
            n => format!("The assistant proposed {n} values (drafts below)."),
        };
        (
            format!("/revisions/{id}#result-proposals"),
            format!("{head} {}", flash_text(&run.answer.text)),
        )
    })
}
