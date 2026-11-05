use axum::{extract::State, response::Response};
use serde_json::json;

use super::{finish, redirect_flash, render, Flash, WebCtx};
use crate::{
    error::AppResult,
    services::{audit, changes, checkouts, impacts, requests, revisions, stats},
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
            "impact_groups": impacts::group_by_dependent(impacts),
            "assigned_requests": assigned,
            "my_requests": my_requests,
        }),
    )
}

/// "Check in all my check-outs": releases every lock of the user without new
/// files (one transaction per revision) and reports the result.
pub async fn checkin_all(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let result = checkouts::check_in_all(&st.db, &ctx.me, None).await;
    match result {
        Ok(outcomes) => {
            let message = checkouts::summary(&outcomes);
            let flash = if outcomes.iter().all(|o| o.ok) {
                Flash::success(message)
            } else {
                Flash::error(message)
            };
            Ok(redirect_flash(&st, "/", flash))
        }
        Err(e) => finish(&st, Err::<(), _>(e), "/", |()| ("/".into(), String::new())),
    }
}
