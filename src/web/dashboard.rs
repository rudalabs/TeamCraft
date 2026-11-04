use axum::{extract::State, response::Response};
use serde_json::json;

use super::{render, WebCtx};
use crate::{
    error::AppResult,
    services::{audit, changes, impacts, requests, revisions, stats},
    AppState,
};

pub async fn show(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let (stats, checkouts, reviews, my_changes, activity, impacts, assigned, my_requests) = tokio::try_join!(
        stats::load(&st.db),
        revisions::checked_out_by(&st.db, ctx.me.id),
        changes::pending_reviews(&st.db, ctx.me.id),
        changes::open_by(&st.db, ctx.me.id),
        audit::recent(&st.db, 15),
        impacts::open_for(&st.db, &ctx.me),
        requests::assigned_to(&st.db, ctx.me.id),
        requests::open_by(&st.db, ctx.me.id),
    )?;
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
            "impacts": impacts,
            "assigned_requests": assigned,
            "my_requests": my_requests,
        }),
    )
}
