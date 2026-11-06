//! Read-only checks: BOM health, duplicate files, similar parts.

use super::*;

pub(super) async fn bom_health(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let rev = revision_arg(st, me, a).await?;
    let rule = a.rule()?.unwrap_or_else(|| default_rule(&rev));
    Ok(json!(insights::bom_health(&st.db, me, rev.id, rule).await?))
}

/// Duplicate contents grouped by SHA-256 (largest first, as returned).
pub fn group_duplicates(rows: &[insights::DuplicateFile]) -> Vec<Value> {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: BTreeMap<&str, Vec<&insights::DuplicateFile>> = BTreeMap::new();
    for r in rows {
        let entry = groups.entry(r.sha256.as_str()).or_default();
        if entry.is_empty() {
            order.push(r.sha256.as_str());
        }
        entry.push(r);
    }
    order
        .into_iter()
        .map(|sha| {
            let copies = &groups[sha];
            json!({
                "sha256": sha,
                "size_bytes": copies[0].size_bytes,
                "copies": copies.iter().map(|c| json!({
                    "item": c.item_number,
                    "revision": c.revision_code,
                    "state": c.revision_state,
                    "filename": c.filename,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

pub(super) async fn find_duplicate_files(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let limit = a.limit("limit", 100, 1000)?;
    let revision = match a.opt_str("item")? {
        Some(number) => {
            Some(revision_at(st, me, &number, a.opt_str("revision")?.as_deref()).await?)
        }
        None => None,
    };
    let rows =
        insights::duplicate_files(&st.db, me, revision.as_ref().map(|r| r.id), limit).await?;
    Ok(json!({
        "scope": revision.as_ref().map(revisions::label),
        "groups": group_duplicates(&rows),
        "truncated": rows.len() as i64 >= limit,
    }))
}

pub(super) async fn find_similar_parts(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let (number, code) = parse_item_ref(&a.req_str("item")?);
    let code = match a.opt_str("revision")? {
        Some(c) => Some(c),
        None => code,
    };
    let rev = revision_at(st, me, &number, code.as_deref()).await?;
    let options = similar::Options {
        limit: a.limit("limit", 10, similar::MAX_LIMIT)?,
        ignore_scale: a.flag("ignore_scale", false)?,
    };
    let file = a.opt_str("file")?;
    let report = similar::for_revision(
        st,
        me,
        &rev,
        file.as_deref(),
        options,
        std::time::Duration::from_secs(60),
    )
    .await?;
    Ok(json!({
        "source": revisions::label(&rev),
        "status": report.status,
        "message": report.message,
        "file": report.source.as_ref().map(|s| &s.filename),
        "signature": report.signature,
        "ignore_scale": report.ignore_scale,
        "similar": report.candidates.iter().map(|c| json!({
            "item": c.item_number,
            "revision": c.revision_code,
            "name": c.name,
            "type": c.item_type,
            "state": c.state,
            "project": c.project_code,
            "file": c.filename,
            "score": c.score,
            "shape": c.shape,
            "size": c.size,
            "identical": c.identical,
            "explanation": c.explanation,
        })).collect::<Vec<_>>(),
    }))
}
