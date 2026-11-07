//! Simulations and their results, and draft result values.

use super::*;

fn basis_brief(b: &simulations::SimulationBasis) -> Value {
    json!({
        "role": b.role,
        "item": b.basis_number,
        "revision": b.basis_code,
        "name": b.basis_name,
        "type": b.basis_type,
        "state": b.basis_state,
        "file": (!b.file_name.is_empty()).then_some(&b.file_name),
        "file_version": b.file_version,
        "file_current_version": b.file_current_version,
        "newest_released_revision": b.released_code,
        "note": b.note,
        "out_of_date": b.out_of_date,
    })
}

fn result_brief(r: &simulations::SimulationResult) -> Value {
    json!({
        "name": r.name,
        "value": r.value,
        "unit": r.unit,
        "lower_limit": r.lower_limit,
        "upper_limit": r.upper_limit,
        "load_case": r.load_case,
        "status": r.status.map(|s| s.as_str()),
        "solver": r.solver,
        "solver_version": r.solver_version,
        "note": r.note,
    })
}

/// The basis links of one simulation, grouped (the service returns one row
/// per link, ordered by simulation number).
pub fn group_based(rows: &[simulations::BasedSimulation]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for row in rows {
        let same = out
            .last()
            .is_some_and(|v| v["simulation"] == row.simulation_number.as_str());
        if !same {
            out.push(json!({
                "simulation": row.simulation_number,
                "revision": row.simulation_code,
                "name": row.simulation_name,
                "state": row.simulation_state,
                "result_count": row.result_count,
                "failed_count": row.failed_count,
                "check_open": false,
                "out_of_date": false,
                "bases": [],
            }));
        }
        let entry = out.last_mut().expect("pushed above");
        if row.open_impact_id.is_some() {
            entry["check_open"] = json!(true);
        }
        if row.basis.out_of_date.is_some() {
            entry["out_of_date"] = json!(true);
        }
        if let Some(bases) = entry["bases"].as_array_mut() {
            bases.push(basis_brief(&row.basis));
        }
    }
    out
}

/// Simulations based on an item, for `get_item` / `get_revision`.
pub(super) async fn simulations_of(
    st: &AppState,
    me: &Actor,
    item_id: uuid::Uuid,
) -> AppResult<(Vec<Value>, bool)> {
    let (rows, _) = simulations::based_on_item(&st.db, me, item_id).await?;
    Ok(capped(group_based(&rows), MAX_RELATED))
}

fn ensure_simulation(rev: &Revision) -> AppResult<()> {
    if rev.item_type == ItemType::Simulation.as_str() {
        Ok(())
    } else {
        Err(bad(format!(
            "{} is a {}, not a simulation; list_simulations with item={} lists the simulations based on it.",
            revisions::label(rev),
            rev.item_type,
            rev.item_number
        )))
    }
}

/// Basis links and results of a simulation revision.
pub(super) async fn simulation_state(
    st: &AppState,
    me: &Actor,
    rev: &Revision,
) -> AppResult<Value> {
    let (bases, results) = tokio::try_join!(
        simulations::bases(&st.db, me, rev.id),
        simulations::results(&st.db, me, rev.id),
    )?;
    let reasons: Vec<&String> = bases
        .iter()
        .filter_map(|b| b.out_of_date.as_ref())
        .collect();
    let (shown, results_cut) = capped(results.iter().collect::<Vec<_>>(), simulations::MAX_RESULTS);
    Ok(json!({
        "out_of_date": !reasons.is_empty(),
        "out_of_date_reasons": reasons,
        "summary": simulations::summarize(&results),
        "bases": bases.iter().map(basis_brief).collect::<Vec<_>>(),
        "results": shown.into_iter().map(result_brief).collect::<Vec<_>>(),
        "results_truncated": results_cut,
    }))
}

pub(super) async fn list_simulations(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let out_of_date_only = a.flag("out_of_date_only", false)?;
    let limit = a.limit("limit", 20, MAX_SIMULATIONS as i64)? as usize;
    if let Some(number) = a.opt_str("item")? {
        let item = items::get_by_number(&st.db, me, &number).await?;
        let (rows, _) = simulations::based_on_item(&st.db, me, item.id).await?;
        let mut list = group_based(&rows);
        if out_of_date_only {
            list.retain(|s| s["out_of_date"] == true);
        }
        let (list, truncated) = capped(list, limit);
        let mut value = json!({
            "based_on": item.number,
            "simulations": list,
            "truncated": truncated,
        });
        if item.item_type == ItemType::Simulation.as_str() {
            value["hint"] = json!(format!(
                "{} is itself a simulation: get_simulation shows its bases and results.",
                item.number
            ));
        }
        return Ok(value);
    }
    let project_id = match a.opt_str("project")? {
        Some(code) => Some(projects::get_by_code(&st.db, me, &code).await?.id),
        None => None,
    };
    // With the filter every simulation up to the cap is checked.
    let scan = if out_of_date_only {
        MAX_SIMULATIONS
    } else {
        limit
    };
    let filter = items::ItemFilter {
        query: a.opt_str("q")?,
        item_type: Some(ItemType::Simulation),
        project_id,
        state: None,
        system: None,
        ..Default::default()
    };
    let (rows, total) =
        items::search(&st.db, me, &filter, Page::new(Some(scan as i64), None)).await?;
    let mut list = Vec::new();
    for sim in &rows {
        let (bases, results) = tokio::try_join!(
            simulations::bases(&st.db, me, sim.latest_revision_id),
            simulations::results(&st.db, me, sim.latest_revision_id),
        )?;
        let reasons: Vec<String> = bases.iter().filter_map(|b| b.out_of_date.clone()).collect();
        if out_of_date_only && reasons.is_empty() {
            continue;
        }
        list.push(json!({
            "simulation": sim.number,
            "revision": sim.latest_code,
            "name": sim.name,
            "state": sim.latest_state,
            "project": sim.project_code,
            "out_of_date": !reasons.is_empty(),
            "out_of_date_reasons": reasons,
            "based_on": bases.iter().map(|b| format!("{}/{}", b.basis_number, b.basis_code)).collect::<Vec<_>>(),
            "results": simulations::summarize(&results),
        }));
    }
    let (list, cut) = capped(list, limit);
    Ok(json!({
        "total_simulations": total,
        "checked": rows.len(),
        "simulations": list,
        "truncated": cut || total > rows.len() as i64,
    }))
}

pub(super) async fn get_simulation(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    ensure_simulation(&rev)?;
    let mut value = simulation_state(st, me, &rev).await?;
    value["simulation"] = rev_brief(&rev);
    value["url"] = json!(web_url(st, &format!("/items/{}", rev.item_id)));
    Ok(value)
}

/// One matched result value of [`simulations::compare_results`].
pub fn diff_brief(d: &simulations::ResultDiff) -> Value {
    let side = |r: &Option<simulations::SimulationResult>| {
        r.as_ref().map(|r| {
            json!({
                "value": r.value,
                "unit": r.unit,
                "lower_limit": r.lower_limit,
                "upper_limit": r.upper_limit,
                "status": r.status.map(|s| s.as_str()),
            })
        })
    };
    json!({
        "change": d.change,
        "name": d.name,
        "load_case": d.load_case,
        "unit": d.unit,
        "before": side(&d.before),
        "after": side(&d.after),
        "delta": d.delta,
        "delta_percent": d.delta_percent.map(|p| (p * 100.0).round() / 100.0),
    })
}

pub(super) async fn compare_simulation_results(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let (from, to) = revision_pair(st, me, a).await?;
    ensure_simulation(&to)?;
    let changes_only = a.flag("changes_only", false)?;
    let diffs = simulations::compare(&st.db, &from, &to).await?;
    let summary = simulations::compare_summary(&diffs);
    let shown: Vec<&simulations::ResultDiff> = diffs
        .iter()
        .filter(|d| !changes_only || d.change != crate::services::bom_compare::Change::Unchanged)
        .collect();
    let (shown, truncated) = capped(shown, simulations::MAX_RESULTS);
    Ok(json!({
        "from": revisions::label(&from),
        "to": revisions::label(&to),
        "from_state": from.state,
        "to_state": to.state,
        "summary": summary,
        "diffs": shown.into_iter().map(diff_brief).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// `propose_result_values`: drafts for a user to accept; never a result.
pub(super) async fn propose_result_values(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let Some(Value::Array(list)) = a.get("values") else {
        return Err(bad("`values` must be a list of values."));
    };
    let mut inputs = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        let input: result_proposals::ProposalInput =
            serde_json::from_value(v.clone()).map_err(|e| bad(format!("values[{i}]: {e}")))?;
        inputs.push(input);
    }
    let proposals = result_proposals::propose(&st.db, me, rev.id, inputs).await?;
    Ok(json!({
        "item": rev.item_number,
        "revision": rev.code,
        "proposed": proposals.iter().map(|p| json!({
            "name": p.name, "value": p.value, "unit": p.unit, "load_case": p.load_case,
            "file": p.source_file, "version": p.source_version, "status": p.status,
        })).collect::<Vec<_>>(),
        "note": "These are drafts. A CAD editor accepts or rejects them on the simulation page in TeamCraft \
                 (accepting needs the revision checked out). You cannot accept them – tell the user where \
                 to find them.",
        "url": web_url(st, &format!("/revisions/{}#result-proposals", rev.id)),
    }))
}
