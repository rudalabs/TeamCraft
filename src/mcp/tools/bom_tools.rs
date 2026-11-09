//! Read tools on the product structure: BOM, where-used, impact, replacement.

use super::*;

pub(super) async fn get_bom(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    let tree = bom::tree(&st.db, me, rev.id, rule).await?;
    let (rows, truncated) = capped(bom::flatten(&tree.children), MAX_BOM_ROWS);
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            let n = &row.node;
            json!({
                "level": n.level,
                "find_number": n.find_number,
                "item": n.item_number,
                "name": n.revision.as_ref().map(|r| r.name.clone()),
                "type": n.item_type,
                "revision": n.revision.as_ref().map(|r| r.code.clone()),
                "state": n.revision.as_ref().map_or(if n.restricted { "restricted" } else { "unresolved" }, |r| r.state.as_str()),
                "restricted": n.restricted,
                "quantity": n.quantity,
                "unit": n.unit,
                "unit_mass_kg": n.unit_mass_kg,
                "total_mass_kg": n.total_mass_kg,
                "total_cost": n.total_cost,
                "has_children": row.has_children,
                "notes": n.notes,
                "cycle": n.cycle,
            })
        })
        .collect();
    Ok(json!({
        "root": format!("{}/{}", tree.root_item_number, tree.root.code),
        "root_state": tree.root.state,
        "rule": rule.as_str(),
        "line_count": tree.line_count,
        "unresolved_count": tree.unresolved_count,
        "restricted_lines": tree.restricted_lines,
        "rollup_incomplete": tree.rollup_incomplete,
        "total_mass_kg": tree.total_mass_kg,
        "mass_complete": tree.mass_complete,
        "total_cost": tree.total_cost,
        "cost_complete": tree.cost_complete,
        "rows": rows,
        "truncated": truncated,
    }))
}

/// `get_mass_properties`: the mass roll-up with CG and inertia.
pub(super) async fn get_mass_properties(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    let reference = mass_properties::parse_point(a.opt_str("reference")?.as_deref())?;
    let r = mass_properties::rollup(st, me, rev.id, rule, reference).await?;
    let (lines, truncated) = capped(r.lines, MAX_BOM_ROWS);
    let lines: Vec<Value> = lines
        .iter()
        .map(|l| {
            json!({
                "level": l.level,
                "find_number": l.find_number,
                "item": l.item_number,
                "revision": l.revision_code,
                "name": l.name,
                "quantity": l.quantity,
                "source": l.source,
                "detail": l.detail,
                "unit_mass_kg": l.unit_mass_kg,
                "total_mass_kg": l.total_mass_kg,
                "cg_m": l.cg_m,
                "warnings": l.warnings,
            })
        })
        .collect();
    Ok(json!({
        "root": r.root_label,
        "rule": rule.as_str(),
        "root_source": r.root_source,
        "mass_kg": r.mass_kg,
        "placed_mass_kg": r.placed_mass_kg,
        "unplaced_mass_kg": r.unplaced_mass_kg,
        "cg_m": r.cg_m,
        "inertia_about_cg_kg_m2": r.inertia_cg.map(tensor),
        "reference_m": r.reference_m,
        "inertia_about_reference_kg_m2": r.inertia_ref.map(tensor),
        "principal_moments_kg_m2": r.principal_moments,
        "complete": r.complete,
        "warnings": r.warnings,
        "restricted_lines": r.restricted_lines,
        "line_count": r.line_count,
        "lines": lines,
        "truncated": truncated,
    }))
}

/// A tensor `[xx, yy, zz, xy, yz, xz]` with named entries.
fn tensor(t: [f64; 6]) -> Value {
    json!({ "ixx": t[0], "iyy": t[1], "izz": t[2], "ixy": t[3], "iyz": t[4], "ixz": t[5] })
}

pub(super) async fn where_used(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let levels = a.limit("levels", 1, i64::from(items::MAX_WHERE_USED_LEVELS))?;
    let rows = items::where_used_levels(&st.db, me, item.id, levels as i32).await?;
    let hidden_parents = items::hidden_parents(&st.db, me, item.id).await?;
    let (rows, truncated) = capped(rows, MAX_LIST);
    Ok(json!({
        "item": item.number,
        "levels": levels,
        "hidden_parents": hidden_parents,
        "used_in": rows.iter().map(|row| (row.level, &row.child_number, &row.usage)).map(|(level, via, p)| json!({
            "level": level,
            "via": via,
            "item": p.parent_number,
            "revision": p.parent_code,
            "name": p.parent_name,
            "state": p.parent_state,
            "find_number": p.find_number,
            "quantity": p.quantity,
            "released_with": p.released_child_code,
        })).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// `impact_summary`: what changing an item touches, in one call.
pub(super) async fn impact_summary(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let levels = a.limit("levels", 3, MAX_IMPACT_LEVELS)?;
    let latest = items::latest_revision(&st.db, me, item.id).await?;
    let rows = items::where_used_levels(&st.db, me, item.id, levels as i32).await?;
    let hidden_parents = items::hidden_parents(&st.db, me, item.id).await?;

    // dependents: distinct parent items per level; the released ones in full
    let mut per_level: BTreeMap<i32, BTreeSet<&str>> = BTreeMap::new();
    let mut all: BTreeSet<&str> = BTreeSet::new();
    let mut released_items: BTreeSet<&str> = BTreeSet::new();
    let mut released = Vec::new();
    for row in &rows {
        let p = &row.usage;
        per_level
            .entry(row.level)
            .or_default()
            .insert(p.parent_number.as_str());
        all.insert(p.parent_number.as_str());
        if p.parent_state == LifecycleState::Released.as_str() {
            released_items.insert(p.parent_number.as_str());
            released.push(json!({
                "level": row.level,
                "via": row.child_number,
                "item": p.parent_number,
                "revision": p.parent_code,
                "name": p.parent_name,
                "find_number": p.find_number,
                "quantity": p.quantity,
                "released_with": p.released_child_code,
            }));
        }
    }
    let (released, released_cut) = capped(released, MAX_IMPACT_LISTED);

    // open change orders that contain a recent revision of the item
    let mut revs = items::revisions(&st.db, me, item.id).await?;
    revs.truncate(IMPACT_REVISIONS);
    let mut open_changes: Vec<Change> = Vec::new();
    for rev in &revs {
        let (list, _hidden) = changes::for_revision(&st.db, me, rev.id).await?;
        for c in list {
            let open = matches!(
                ChangeStatus::parse(&c.status),
                Some(ChangeStatus::Draft | ChangeStatus::InReview)
            );
            if open && !open_changes.iter().any(|o| o.id == c.id) {
                open_changes.push(c);
            }
        }
    }
    let open_change_count = open_changes.len();
    let (open_changes, changes_cut) = capped(open_changes, MAX_IMPACT_LISTED);

    let (open_requests, requests_cut) = open_requests_of(st, me, item.id).await?;
    let mut open_impacts: Vec<Impact> = impacts::for_item(&st.db, me, item.id).await?;
    open_impacts.retain(|i| i.resolved_at.is_none());
    let open_impact_count = open_impacts.len();
    let (open_impacts, impacts_cut) = capped(open_impacts, MAX_IMPACT_LISTED);
    let (sims, sims_cut) = if item.item_type == ItemType::Simulation.as_str() {
        (Vec::new(), false)
    } else {
        simulations_of(st, me, item.id).await?
    };
    let out_of_date = sims.iter().filter(|s| s["out_of_date"] == true).count();

    Ok(json!({
        "item": item.number,
        "latest_revision": rev_brief(&latest),
        "levels": levels,
        "summary": {
            "dependents": all.len(),
            "released_dependents": released_items.len(),
            "open_change_orders": open_change_count,
            "open_change_requests": open_requests.len(),
            "open_impacts_on_item": open_impact_count,
            "simulations": sims.len(),
            "simulations_out_of_date": out_of_date,
            "hidden_parents": hidden_parents,
        },
        "dependents_by_level": per_level.iter().map(|(level, set)| json!({
            "level": level,
            "items": set.len(),
        })).collect::<Vec<_>>(),
        "released_dependents": released,
        "open_change_orders": open_changes.iter().map(change_brief).collect::<Vec<_>>(),
        "open_change_requests": open_requests,
        "open_impacts_on_item": open_impacts.iter().map(|i| json!({
            "source": format!("{}/{}", i.source_number, i.source_code),
            "event": i.event,
            "relation": i.relation,
            "change": i.change_number,
            "since": i.created_at,
        })).collect::<Vec<_>>(),
        "simulations": sims,
        "truncated": released_cut || changes_cut || requests_cut || impacts_cut || sims_cut,
        "url": web_url(st, &format!("/items/{}/where-used", item.id)),
    }))
}

/// An alternate, successor or predecessor in a compact form.
pub(super) fn linked_brief(l: &interchange::LinkedItem, with_direction: bool) -> Value {
    if l.restricted {
        return json!({ "item": "restricted item" });
    }
    let mut v = json!({
        "item": l.number,
        "name": l.name,
        "latest_revision": l.latest_code,
        "latest_state": l.latest_state,
        "note": l.note,
    });
    if with_direction {
        v["two_way"] = json!(l.two_way);
    }
    v
}

pub(super) async fn replace_preview(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    let plan = interchange::plan_replace(&st.db, me, item.id, &a.req_str("with")?).await?;
    let (rows, truncated) = capped(plan.rows, MAX_LIST);
    Ok(json!({
        "item": plan.old_number,
        "with": plan.new_number,
        "would_replace": plan.replaced,
        "hidden_parents": plan.hidden,
        "lines": rows.iter().map(|r| json!({
            "assembly": r.parent_number,
            "revision": r.parent_code,
            "name": r.parent_name,
            "state": r.parent_state,
            "find_number": r.find_number,
            "quantity": r.quantity,
            "action": r.action,
            "checked_out_by": r.checked_out_by_name,
        })).collect::<Vec<_>>(),
        "truncated": truncated,
        "url": web_url(st, &format!(
            "/items/{}/where-used?with={}",
            item.id,
            crate::util::pct_encode(&plan.new_number)
        )),
    }))
}
