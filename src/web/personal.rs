//! Favorites, saved searches, Save As and the BOM comparison (the most used
//! working features of Teamcenter, see docs/src/research/teamcenter.md).

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, parse_opt_uuid, render, xlsx_response, WebCtx};
use crate::{
    error::{AppError, AppResult},
    models::Revision,
    services::{
        bom_compare, items, personal, projects, revisions,
        save_as::{self, SaveAs},
    },
    util, AppState,
};

// ---------------------------------------------------------------- favorites

#[derive(Deserialize)]
pub struct FavoriteForm {
    /// `1` adds, anything else removes.
    #[serde(default)]
    favorite: String,
    back: Option<String>,
}

pub async fn set_favorite(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<FavoriteForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let on = form.favorite == "1";
    let result = personal::set_favorite(&st.db, &ctx.me, id, on).await;
    finish(&st, result, &back, |_| {
        (
            back.clone(),
            if on {
                "Added to your favorites (Vault → ★ Favorites)".to_string()
            } else {
                "Removed from your favorites".to_string()
            },
        )
    })
}

// ----------------------------------------------------------- saved searches

#[derive(Deserialize)]
pub struct SaveSearchForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    item_type: String,
    #[serde(default)]
    project: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    system: String,
}

pub async fn save_search(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<SaveSearchForm>,
) -> AppResult<Response> {
    let pairs = [
        ("q", form.q.as_str()),
        ("type", form.item_type.as_str()),
        ("project", form.project.as_str()),
        ("state", form.state.as_str()),
        ("system", form.system.as_str()),
    ];
    let back = format!("/items?{}", personal::normalize_query(&pairs));
    let result = personal::save_search(&st.db, &ctx.me, &form.name, &pairs).await;
    finish(&st, result, &back, |s| {
        (
            format!("/items?{}", s.query),
            format!("Saved the search “{}”", s.name),
        )
    })
}

pub async fn delete_search(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let result = personal::delete_search(&st.db, &ctx.me, id).await;
    finish(&st, result, "/items", |_| {
        ("/items".to_string(), "Deleted the saved search".to_string())
    })
}

// ------------------------------------------------------------------ save as

pub async fn save_as_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (rev, projects, lines) = tokio::try_join!(
        revisions::get(&st.db, id),
        projects::editable_by(&st.db, &ctx.me),
        crate::services::bom::lines(&st.db, id),
    )?;
    let files = crate::services::files::list(&st.db, id).await?;
    render(
        &st,
        &ctx,
        "revisions/save_as.html",
        json!({ "rev": rev, "projects": projects, "file_count": files.len(), "line_count": lines.len() }),
    )
}

#[derive(Deserialize)]
pub struct SaveAsForm {
    #[serde(default)]
    number: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    project_id: String,
    files: Option<String>,
    bom: Option<String>,
}

pub async fn save_as_submit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<SaveAsForm>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}/save-as");
    let result = async {
        let input = SaveAs {
            number: Some(form.number.clone()),
            name: form.name.clone(),
            project_id: parse_opt_uuid("Project group", Some(&form.project_id))?,
            with_files: form.files.is_some(),
            with_bom: form.bom.is_some(),
        };
        save_as::save_as(&st.db, &ctx.me, id, input).await
    }
    .await;
    finish(&st, result, &back, |rev| {
        (
            format!("/revisions/{}", rev.id),
            format!(
                "Created {}/{} – checked out to you",
                rev.item_number, rev.code
            ),
        )
    })
}

// ------------------------------------------------------------ BOM compare

#[derive(Deserialize, Default)]
pub struct CompareQuery {
    /// The revision to compare with (default: the previous one).
    with: Option<String>,
}

/// The two revisions of a comparison and their BOM differences.
struct Comparison {
    /// The revision whose page asked for the comparison.
    rev: Revision,
    /// All revisions of the item (choice in the page).
    revisions: Vec<Revision>,
    before: Revision,
    after: Revision,
    diffs: Vec<bom_compare::LineDiff>,
}

impl Comparison {
    /// File name of the Excel download.
    fn file_name(&self) -> String {
        format!(
            "bom-compare-{}-{}-{}.xlsx",
            self.rev.item_number, self.before.code, self.after.code
        )
    }
}

/// Loads both revisions (older first) and compares their BOMs. `with` is a
/// revision id; the default is the previous revision of the item.
async fn comparison(st: &AppState, id: Uuid, with: Option<&str>) -> AppResult<Comparison> {
    let rev = revisions::get(&st.db, id).await?;
    let all = items::revisions(&st.db, rev.item_id).await?;
    let other_id = match parse_opt_uuid("Revision", with)? {
        Some(other) => other,
        // the revision before this one, else the one after it
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
    let other = revisions::get(&st.db, other_id).await?;
    // two revisions of one item: the older one first; two items: this one
    // first, compared with the other
    let (before, after) = if other.item_id == rev.item_id && other.seq < rev.seq {
        (other, rev.clone())
    } else {
        (rev.clone(), other)
    };
    let diffs = bom_compare::compare(&st.db, before.id, after.id).await?;
    Ok(Comparison {
        rev,
        revisions: all,
        before,
        after,
        diffs,
    })
}

pub async fn bom_compare(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Response> {
    let cmp = comparison(&st, id, q.with.as_deref()).await?;
    let summary = bom_compare::summary(&cmp.diffs);
    render(
        &st,
        &ctx,
        "revisions/bom_compare.html",
        json!({
            "rev": cmp.rev,
            "before": cmp.before,
            "after": cmp.after,
            "revisions": cmp.revisions,
            "diffs": cmp.diffs,
            "summary": summary,
        }),
    )
}

/// `GET /revisions/{id}/bom/compare.xlsx` – the comparison as an Excel
/// workbook (same `with` query as the page).
pub async fn bom_compare_xlsx(
    State(st): State<AppState>,
    _ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Response> {
    let cmp = comparison(&st, id, q.with.as_deref()).await?;
    let bytes = bom_compare::to_xlsx(&cmp.diffs)?;
    Ok(xlsx_response(bytes, &cmp.file_name()))
}
