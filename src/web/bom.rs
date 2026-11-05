//! BOM views and editing.

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, render, WebCtx};
use crate::{
    auth::WebUser,
    domain::{self, RevisionRule},
    error::AppResult,
    services::{bom, interchange, package, revisions},
    util, AppState,
};

#[derive(Deserialize)]
pub struct RuleQuery {
    rule: Option<String>,
}

fn rule_of(q: &RuleQuery) -> RevisionRule {
    q.rule
        .as_deref()
        .and_then(RevisionRule::parse)
        .unwrap_or_default()
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<RuleQuery>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let rev = revisions::get(&st.db, me, id).await?;
    let rule = rule_of(&q);
    let tree = bom::tree(&st.db, me, id, rule).await?;
    let rows = bom::flatten(&tree.children);
    render(
        &st,
        &ctx,
        "revisions/bom.html",
        json!({
            "rev": rev,
            "rule": rule.as_str(),
            "tree": {
                "total_mass_kg": tree.total_mass_kg,
                "total_cost": tree.total_cost,
                "mass_complete": tree.mass_complete,
                "cost_complete": tree.cost_complete,
                "line_count": tree.line_count,
                "unresolved_count": tree.unresolved_count,
                "restricted_lines": tree.restricted_lines,
                "rollup_incomplete": tree.rollup_incomplete,
            },
            "rows": rows,
        }),
    )
}

pub async fn csv(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
    Query(q): Query<RuleQuery>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &me, id).await?;
    let rule = rule_of(&q);
    let tree = bom::tree(&st.db, &me, id, rule).await?;
    let filename = format!("{}_{}_bom_{}.csv", rev.item_number, rev.code, rule.as_str());
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                util::content_disposition(&filename),
            ),
        ],
        bom::to_csv(&tree),
    )
        .into_response())
}

/// `GET /revisions/{id}/bom.xlsx` – the expanded BOM as Excel workbook.
pub async fn xlsx(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
    Query(q): Query<RuleQuery>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &me, id).await?;
    let rule = rule_of(&q);
    let tree = bom::tree(&st.db, &me, id, rule).await?;
    let filename = format!(
        "{}_{}_bom_{}.xlsx",
        rev.item_number,
        rev.code,
        rule.as_str()
    );
    Ok(super::xlsx_response(bom::to_xlsx(&tree)?, &filename))
}

#[derive(Deserialize)]
pub struct AddLineForm {
    #[serde(default)]
    child: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    find_number: String,
    #[serde(default)]
    notes: String,
}

fn parse_find_number(raw: &str) -> AppResult<Option<i32>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<i32>().map(Some).map_err(|_| {
        crate::error::AppError::BadRequest(format!("Find number '{raw}' is not a whole number."))
    })
}

fn parse_quantity(raw: &str) -> AppResult<f64> {
    Ok(domain::parse_decimal("Quantity", Some(raw))?.unwrap_or(1.0))
}

pub async fn add_line(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddLineForm>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}#bom");
    let result = async {
        let input = bom::NewBomLine {
            child_number: form.child.clone(),
            quantity: parse_quantity(&form.quantity)?,
            find_number: parse_find_number(&form.find_number)?,
            notes: form.notes.clone(),
        };
        let line = bom::add(&st.db, &ctx.me, id, input).await?;
        // a superseded child may be added on purpose – with a hint
        let superseded = interchange::superseded_among(&st.db, &ctx.me, &[line.child_item_id])
            .await?
            .remove(&line.child_item_id);
        Ok::<_, crate::error::AppError>((line, superseded))
    }
    .await;
    finish(&st, result, &back, |(line, superseded)| {
        let hint = match superseded {
            Some(Some(successor)) => {
                format!(" – note: it is superseded by {successor}, use that for new designs")
            }
            Some(None) => " – note: it is superseded, do not use it for new designs".to_string(),
            None => String::new(),
        };
        (
            back.clone(),
            format!(
                "Added {} (find no. {}){hint}",
                line.child_number, line.find_number
            ),
        )
    })
}

#[derive(Deserialize)]
pub struct UpdateLineForm {
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    find_number: String,
    #[serde(default)]
    notes: String,
    back: Option<String>,
}

pub async fn update_line(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(line_id): Path<Uuid>,
    Form(form): Form<UpdateLineForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = async {
        let find_number = parse_find_number(&form.find_number)?.ok_or_else(|| {
            crate::error::AppError::BadRequest("A find number is required.".into())
        })?;
        bom::update(
            &st.db,
            &ctx.me,
            line_id,
            bom::BomLineUpdate {
                quantity: parse_quantity(&form.quantity)?,
                find_number,
                notes: form.notes.clone(),
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |line| {
        (
            back.clone(),
            format!("Updated BOM line {}", line.child_number),
        )
    })
}

pub async fn delete_line(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(line_id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = bom::remove(&st.db, &ctx.me, line_id).await;
    finish(&st, result, &back, |line| {
        (
            back.clone(),
            format!("Removed {} from the BOM", line.child_number),
        )
    })
}

/// Default rule of a package: released revisions ship what they were
/// released with, revisions in work their latest children.
pub(crate) fn package_rule(requested: Option<&str>, root_state: &str) -> RevisionRule {
    requested
        .and_then(RevisionRule::parse)
        .filter(|_| requested.is_some_and(|r| !r.trim().is_empty()))
        .unwrap_or(if matches!(root_state, "released" | "obsolete") {
            RevisionRule::AsReleased
        } else {
            RevisionRule::Latest
        })
}

/// Streams a finished package as a ZIP download.
pub(crate) fn package_response(p: package::Package) -> AppResult<Response> {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, p.size)
        .header(
            header::CONTENT_DISPOSITION,
            util::content_disposition(&p.filename),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("x-teamcraft-files", p.files)
        .body(axum::body::Body::from_stream(
            tokio_util::io::ReaderStream::new(p.file),
        ))
        .map_err(|e| {
            crate::error::AppError::Other(anyhow::anyhow!("building the response failed: {e}"))
        })
}

/// "Pack and Go": the revision with its whole structure as a ZIP file.
pub async fn package(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
    Query(q): Query<RuleQuery>,
) -> AppResult<Response> {
    let rev = revisions::get(&st.db, &me, id).await?;
    let rule = package_rule(q.rule.as_deref(), &rev.state);
    package_response(package::build(&st.db, &st.vault, &me, id, rule).await?)
}
