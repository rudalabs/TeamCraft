//! Season handover page of a project group (issue #173, step 8):
//! `GET/POST /projects/{id}/season`, managers of the group and admins.

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, render, WebCtx};
use crate::{
    error::{AppError, AppResult},
    services::{
        projects, revisions,
        season::{self, SeasonPlan},
    },
    AppState,
};

#[derive(Deserialize)]
pub struct SeasonQuery {
    /// Preselects members without activity since this date (`YYYY-MM-DD`).
    inactive_since: Option<String>,
}

fn date(raw: &str) -> AppResult<Option<NaiveDate>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("'{raw}' is not a date (YYYY-MM-DD).")))
}

/// Whether a member is ticked: their membership has expired, or (with a
/// date) they are deactivated or have not been active since.
fn preselect(m: &season::SeasonMember, since: Option<NaiveDate>) -> bool {
    let Some(since) = since else {
        return m.expired;
    };
    let idle = match m.last_active_at {
        Some(t) => t.date_naive() < since,
        None => true,
    };
    m.expired || !m.active || idle
}

/// `GET /projects/{id}/season` – the members with their activity, check-outs
/// and "member until"; `?inactive_since=` ticks those who have been inactive
/// since then (and those whose membership has expired).
pub async fn page(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<SeasonQuery>,
) -> AppResult<Response> {
    let project = projects::get_for_management(&st.db, &ctx.me, id).await?;
    let members = season::members(&st.db, &ctx.me, id).await?;
    let since = date(q.inactive_since.as_deref().unwrap_or(""))?;
    let chosen: Vec<Uuid> = members
        .iter()
        .filter(|m| preselect(m, since))
        .map(|m| m.user_id)
        .collect();
    render(
        &st,
        &ctx,
        "projects/season.html",
        json!({
            "project": project,
            "members": members,
            "chosen": chosen,
            "inactive_since": since.map(|d| d.to_string()).unwrap_or_default(),
            "release_reason": {
                "min": revisions::RELEASE_REASON_MIN,
                "max": revisions::RELEASE_REASON_MAX,
            },
            "max_members": season::MAX_MEMBERS,
        }),
    )
}

/// The plan from the form: per member the checkboxes `release`, `demote`,
/// `expire` and `remove` (value = user id), `new_lead` (username), `reason`
/// and `until`.
fn plan_from(pairs: Vec<(String, String)>) -> AppResult<SeasonPlan> {
    let mut plan = SeasonPlan::default();
    for (key, value) in pairs {
        let list = match key.as_str() {
            "release" => &mut plan.release,
            "demote" => &mut plan.demote,
            "expire" => &mut plan.expire,
            "remove" => &mut plan.remove,
            "new_lead" => {
                plan.new_lead = Some(value);
                continue;
            }
            "reason" => {
                plan.reason = Some(value);
                continue;
            }
            "until" => {
                plan.until = date(&value)?;
                continue;
            }
            _ => continue,
        };
        let id = Uuid::parse_str(value.trim())
            .map_err(|_| AppError::BadRequest("Invalid member.".into()))?;
        if !list.contains(&id) {
            list.push(id);
        }
    }
    Ok(plan)
}

/// `POST /projects/{id}/season` – runs the plan; every step is audited on its
/// own and the flash message counts what was done and refused.
pub async fn run(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let back = format!("/projects/{id}/season");
    let result = async {
        let plan = plan_from(pairs)?;
        season::run(&st.db, &ctx.me, id, &plan).await
    }
    .await;
    finish(&st, result, &back, |report| {
        (back.clone(), report.summary())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_becomes_a_plan() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let pairs = vec![
            ("release".to_string(), a.to_string()),
            ("release".to_string(), a.to_string()),
            ("remove".to_string(), b.to_string()),
            ("expire".to_string(), b.to_string()),
            ("until".to_string(), "2027-07-31".to_string()),
            ("new_lead".to_string(), "tom".to_string()),
            ("reason".to_string(), "End of the season".to_string()),
            ("csrf".to_string(), "ignored".to_string()),
        ];
        let plan = plan_from(pairs).unwrap();
        assert_eq!(plan.release, vec![a]);
        assert_eq!(plan.remove, vec![b]);
        assert_eq!(plan.expire, vec![b]);
        assert_eq!(plan.until, NaiveDate::from_ymd_opt(2027, 7, 31));
        assert_eq!(plan.new_lead.as_deref(), Some("tom"));
        assert!(plan.demote.is_empty());
        assert!(plan_from(vec![("remove".into(), "x".into())]).is_err());
        assert!(plan_from(vec![("until".into(), "31.07.2027".into())]).is_err());
        assert_eq!(
            plan_from(vec![("until".into(), " ".into())]).unwrap().until,
            None
        );
    }
}
