//! Revision comparison: the pure diff of two snapshots and the `compare_revisions` tool.

use super::*;

// ---------------------------------------------------------------------------
// Revision comparison (pure)
// ---------------------------------------------------------------------------

/// A file of a revision as far as the comparison is concerned.
#[derive(Clone, Debug)]
pub struct FileState {
    pub filename: String,
    pub sha256: String,
    pub version: i32,
    pub size_bytes: i64,
}

/// A BOM line as far as the comparison is concerned.
#[derive(Clone, Debug)]
pub struct LineState {
    pub child: String,
    pub name: String,
    pub find_number: i32,
    pub quantity: f64,
}

/// What [`compare`] needs of a revision.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub label: String,
    /// (field, value) pairs in display order.
    pub attributes: Vec<(&'static str, Value)>,
    pub files: Vec<FileState>,
    pub bom: Vec<LineState>,
}

impl Snapshot {
    pub fn of(rev: &Revision, file_list: &[FileRecord], lines: &[BomLine]) -> Snapshot {
        Snapshot {
            label: revisions::label(rev),
            attributes: vec![
                ("name", json!(rev.name)),
                ("description", json!(rev.description)),
                ("material", json!(rev.material)),
                ("mass_kg", json!(rev.mass_kg)),
                ("unit_cost", json!(rev.unit_cost)),
            ],
            files: file_list
                .iter()
                .map(|f| FileState {
                    filename: f.filename.clone(),
                    sha256: f.sha256.clone(),
                    version: f.version,
                    size_bytes: f.size_bytes,
                })
                .collect(),
            bom: lines
                .iter()
                .map(|l| LineState {
                    child: l.child_number.clone(),
                    name: l.child_name.clone(),
                    find_number: l.find_number,
                    quantity: l.quantity,
                })
                .collect(),
        }
    }
}

/// The BOM lines of one child item, summed up (an item may appear on
/// several lines).
struct ChildUse<'a> {
    name: &'a str,
    quantity: f64,
    find_numbers: BTreeSet<i32>,
}

fn child_uses(lines: &[LineState]) -> BTreeMap<&str, ChildUse<'_>> {
    let mut out: BTreeMap<&str, ChildUse> = BTreeMap::new();
    for l in lines {
        let e = out.entry(l.child.as_str()).or_insert(ChildUse {
            name: &l.name,
            quantity: 0.0,
            find_numbers: BTreeSet::new(),
        });
        e.quantity += l.quantity;
        e.find_numbers.insert(l.find_number);
    }
    out
}

/// What changed from `old` to `new`: attributes, files (matched by name,
/// compared by content hash) and BOM lines (matched by child item).
pub fn compare(old: &Snapshot, new: &Snapshot) -> Value {
    let attributes: Vec<Value> = old
        .attributes
        .iter()
        .zip(&new.attributes)
        .filter(|((_, a), (_, b))| a != b)
        .map(|((field, a), (_, b))| json!({ "field": field, "from": a, "to": b }))
        .collect();

    let key = |f: &FileState| f.filename.to_lowercase();
    let old_files: BTreeMap<String, &FileState> = old.files.iter().map(|f| (key(f), f)).collect();
    let new_files: BTreeMap<String, &FileState> = new.files.iter().map(|f| (key(f), f)).collect();
    let mut files_added = Vec::new();
    let mut files_changed = Vec::new();
    let mut files_unchanged = 0usize;
    for (k, f) in &new_files {
        match old_files.get(k) {
            None => files_added.push(json!({
                "filename": f.filename, "version": f.version, "size_bytes": f.size_bytes,
            })),
            Some(o) if o.sha256 != f.sha256 => files_changed.push(json!({
                "filename": f.filename,
                "from_version": o.version,
                "to_version": f.version,
                "from_size_bytes": o.size_bytes,
                "to_size_bytes": f.size_bytes,
            })),
            Some(_) => files_unchanged += 1,
        }
    }
    let files_removed: Vec<Value> = old_files
        .iter()
        .filter(|(k, _)| !new_files.contains_key(*k))
        .map(|(_, f)| json!({ "filename": f.filename, "version": f.version }))
        .collect();

    let old_bom = child_uses(&old.bom);
    let new_bom = child_uses(&new.bom);
    let mut bom_added = Vec::new();
    let mut bom_changed = Vec::new();
    for (child, n) in &new_bom {
        match old_bom.get(child) {
            None => bom_added.push(json!({
                "item": child, "name": n.name, "quantity": n.quantity, "find_numbers": n.find_numbers,
            })),
            Some(o) => {
                let quantity = (o.quantity - n.quantity).abs() > 1e-9;
                let find = o.find_numbers != n.find_numbers;
                if quantity || find {
                    bom_changed.push(json!({
                        "item": child,
                        "name": n.name,
                        "from_quantity": o.quantity,
                        "to_quantity": n.quantity,
                        "from_find_numbers": o.find_numbers,
                        "to_find_numbers": n.find_numbers,
                    }));
                }
            }
        }
    }
    let bom_removed: Vec<Value> = old_bom
        .iter()
        .filter(|(c, _)| !new_bom.contains_key(*c))
        .map(|(c, o)| json!({ "item": c, "name": o.name, "quantity": o.quantity }))
        .collect();

    let identical = attributes.is_empty()
        && files_added.is_empty()
        && files_changed.is_empty()
        && files_removed.is_empty()
        && bom_added.is_empty()
        && bom_changed.is_empty()
        && bom_removed.is_empty();
    json!({
        "from": old.label,
        "to": new.label,
        "identical": identical,
        "attributes": attributes,
        "files": {
            "added": files_added,
            "removed": files_removed,
            "new_content": files_changed,
            "unchanged": files_unchanged,
        },
        "bom": {
            "added": bom_added,
            "removed": bom_removed,
            "changed": bom_changed,
        },
    })
}

async fn snapshot(st: &AppState, me: &Actor, rev: &Revision) -> AppResult<Snapshot> {
    let file_list = files::list(&st.db, me, rev.id).await?;
    let lines = bom::lines(&st.db, me, rev.id).await?;
    Ok(Snapshot::of(rev, &file_list, &lines))
}

/// The two revisions of `item` a comparison is about: `to` (default the
/// latest) and `from` (default the revision before `to`).
pub(super) async fn revision_pair(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<(Revision, Revision)> {
    let item = items::get_by_number(&st.db, me, &a.req_str("item")?).await?;
    // newest first
    let revs = items::revisions(&st.db, me, item.id).await?;
    let find = |code: &str| {
        revs.iter()
            .find(|r| r.code.eq_ignore_ascii_case(code))
            .ok_or_else(|| AppError::NotFound(format!("{} has no revision {code}.", item.number)))
    };
    let to = match a.opt_str("to")? {
        Some(c) if !c.eq_ignore_ascii_case("latest") => find(&c)?,
        _ => revs
            .first()
            .ok_or_else(|| AppError::NotFound(format!("{} has no revisions.", item.number)))?,
    };
    let from = match a.opt_str("from")? {
        Some(c) => find(&c)?,
        None => revs.iter().find(|r| r.seq < to.seq).ok_or_else(|| {
            bad(format!(
                "{} is the first revision; there is nothing before it to compare with.",
                revisions::label(to)
            ))
        })?,
    };
    Ok((from.clone(), to.clone()))
}

pub(super) async fn compare_revisions(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let (from, to) = revision_pair(st, me, a).await?;
    let (from, to) = (&from, &to);
    let old = snapshot(st, me, from).await?;
    let new = snapshot(st, me, to).await?;
    let mut value = compare(&old, &new);
    value["from_state"] = json!(from.state);
    value["to_state"] = json!(to.state);
    Ok(value)
}
