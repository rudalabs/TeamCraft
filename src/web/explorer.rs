//! The vault explorer page: folder tree, item list with locks, detail pane
//! with files and one-click check-out / check-in / cancel.

use axum::{
    extract::{Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{render, WebCtx};
use crate::{
    error::{AppError, AppResult},
    preview::PreviewFormat,
    services::{
        explorer::{self, Listing},
        files, revisions,
    },
    util, AppState,
};

#[derive(Deserialize, Default)]
pub struct ExplorerQuery {
    /// `mine` (default), `locked`, `none`, `search` or a project group id.
    folder: Option<String>,
    /// Selected revision (detail pane).
    rev: Option<String>,
    #[serde(default)]
    q: String,
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(query): Query<ExplorerQuery>,
) -> AppResult<Response> {
    let q = query.q.trim().to_string();
    let folder = match query.folder.as_deref().map(str::trim) {
        Some("") | None if !q.is_empty() => "search".to_string(),
        Some("") | None => "mine".to_string(),
        Some(f) => f.to_string(),
    };
    let listing = match folder.as_str() {
        "mine" => Listing::CheckedOutBy(ctx.me.id),
        "favorites" => Listing::Favorites(ctx.me.id),
        "locked" => Listing::AllCheckedOut,
        "none" => Listing::Unassigned,
        "search" => Listing::Search(&q),
        id => Listing::Project(
            Uuid::parse_str(id).map_err(|_| AppError::BadRequest("Unknown folder.".into()))?,
        ),
    };
    let (folders, entries) = tokio::try_join!(
        explorer::folders(&st.db),
        explorer::list(&st.db, listing, &q)
    )?;

    // the detail pane of the selected revision
    let rev_id = super::parse_opt_uuid("Revision", query.rev.as_deref())?;
    let selected = match rev_id {
        None => None,
        Some(id) => {
            let (rev, file_list) =
                tokio::try_join!(revisions::get(&st.db, id), files::list(&st.db, id))?;
            let me = &ctx.me;
            let can_work = revisions::ensure_checkout_possible(&rev, me).is_ok();
            let file_list: Vec<_> = file_list
                .into_iter()
                .map(|f| {
                    let preview = PreviewFormat::of_format(&f.format)
                        .or_else(|| PreviewFormat::of(&f.filename))
                        .is_some();
                    let mut value = json!(f);
                    value["preview"] = json!(preview);
                    value
                })
                .collect();
            Some(json!({
                "rev": rev,
                "files": file_list,
                "can": {
                    "checkout": can_work && rev.checked_out_by.is_none(),
                    "checkin": rev.checked_out_by == Some(me.id),
                    "cancel_checkout": rev.checked_out_by.is_some()
                        && me.can_edit()
                        && (rev.checked_out_by == Some(me.id) || me.is_admin()),
                },
            }))
        }
    };
    let here = format!(
        "/vault?folder={}{}{}",
        util::pct_encode(&folder),
        if q.is_empty() {
            String::new()
        } else {
            format!("&q={}", util::pct_encode(&q))
        },
        rev_id.map(|id| format!("&rev={id}")).unwrap_or_default(),
    );
    render(
        &st,
        &ctx,
        "explorer.html",
        json!({
            "folders": folders,
            "folder": folder,
            "entries": entries,
            "limit": explorer::LIMIT,
            "q": q,
            "selected": selected,
            // return address of the lock actions (`?back=`)
            "back": util::pct_encode(&here),
            "max_upload_mb": st.config.max_upload_bytes / (1024 * 1024),
        }),
    )
}
