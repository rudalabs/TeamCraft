use axum::{extract::State, response::Response};
use serde_json::json;

use super::{render, WebCtx};
use crate::{
    error::AppResult,
    services::{audit, changes, revisions, stats},
    AppState,
};

pub async fn show(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let stats = stats::load(&st.db).await?;
    let checkouts = revisions::checked_out_by(&st.db, ctx.me.id).await?;
    let reviews = changes::pending_reviews(&st.db, ctx.me.id).await?;
    let my_changes = changes::open_by(&st.db, ctx.me.id).await?;
    let activity = audit::recent(&st.db, 15).await?;
    render(
        &st,
        &ctx,
        "dashboard.html",
        json!({
            "stats": stats,
            "checkouts": checkouts,
            "reviews": reviews,
            "my_changes": my_changes,
            "activity": activity,
        }),
    )
}
