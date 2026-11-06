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
    services::{bom, interchange, package, projects, revisions},
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

/// Query of the BOM page: the exports take only `rule` and stay complete.
#[derive(Deserialize)]
pub struct ShowQuery {
    rule: Option<String>,
    /// Levels shown: a number, `all` or empty.
    depth: Option<String>,
    /// Project group code: keep its lines and their ancestors.
    group: Option<String>,
}

/// `?depth=`: a positive number of levels; anything else (`all`, empty,
/// junk) shows every level.
pub(crate) fn parse_depth(raw: Option<&str>) -> Option<usize> {
    raw.and_then(|d| d.trim().parse::<usize>().ok())
        .filter(|d| *d >= 1)
}

/// Lines the BOM page and the BOM comparison list at most: a large product
/// expands to tens of thousands (megabytes of HTML), the CSV and Excel
/// downloads have them all.
pub(crate) const ROWS_SHOWN: usize = 2000;

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<ShowQuery>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let rev = revisions::get(&st.db, me, id).await?;
    let rule = q
        .rule
        .as_deref()
        .and_then(RevisionRule::parse)
        .unwrap_or_default();
    let depth = parse_depth(q.depth.as_deref());
    let group_code = q.group.as_deref().map(str::trim).filter(|g| !g.is_empty());
    let group = match group_code {
        Some(code) => Some(projects::get_by_code(&st.db, me, code).await?),
        None => None,
    };
    let tree = bom::tree(&st.db, me, id, rule).await?;
    // The roll-ups cover the whole structure; the view only decides which
    // lines are listed.
    let mut rows = bom::view_rows(
        &tree.children,
        bom::ViewOptions {
            depth,
            group: group.as_ref().map(|g| g.id),
        },
    );
    let codes = bom::project_codes(&st.db, &bom::tree_projects(&tree.children)).await?;
    for row in &mut rows {
        row.project_code = row.node.project_id.and_then(|p| codes.get(&p).cloned());
    }
    let mut groups: Vec<(Uuid, String)> = codes.into_iter().collect();
    groups.sort_by(|a, b| a.1.cmp(&b.1));
    let groups: Vec<_> = groups
        .into_iter()
        .map(|(id, code)| json!({ "id": id, "code": code }))
        .collect();
    let rows_total = rows.len();
    rows.truncate(ROWS_SHOWN);
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
            "rows_total": rows_total,
            "depth": depth,
            "group": group.as_ref().map(|g| g.code.clone()),
            "groups": groups,
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
