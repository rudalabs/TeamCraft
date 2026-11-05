//! Import of bills of materials exported by CAD programs (see
//! [`crate::bomtable`]) and of the product structure of STEP assemblies (see
//! [`crate::stepbom`]).
//!
//! An import creates the items that are not in TeamCraft yet and the
//! multi-level BOM below an assembly: a new top-level assembly or a revision
//! the user has checked out. The preview and the import use the same planning
//! code, so the preview shows exactly what the import does; the import runs in
//! one transaction (all or nothing) and writes the usual audit entries per
//! item and BOM line plus one `import.bom` summary.
//!
//! Rules (defaults in brackets):
//!
//! * rows with the same part number (or, without one, the same name) are one
//!   item; repeated rows below the same parent are merged (quantities added),
//! * `match_existing` [on]: a part number that is an item number in TeamCraft
//!   uses that item – its own BOM is never changed by an import,
//! * `numbers` [`file`]: new items get the part number of the file when it is
//!   a valid, unused item number, otherwise a generated one (the CAD number
//!   is then kept in the description); `generate` always generates numbers,
//! * a sub-assembly takes its children from its first occurrence; when the
//!   rows below it are skipped there (e.g. below an existing item) or come
//!   later, they are read at the occurrence that creates the sub-assembly,
//! * components already in the BOM of the target revision are not added
//!   again, so importing the same table twice does not duplicate lines.
//!
//! A STEP file is attached to the new top-level assembly it creates, so its 3D
//! preview shows the whole assembly. Web uploads wait in `staged_imports`
//! until the user confirms the preview – tables in the row, STEP files in the
//! vault.

use std::collections::{hash_map::Entry, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    bomtable::{self, BomRow, BomTable, Column},
    domain::{self, ItemType, Role},
    error::{AppError, AppResult},
    services::{
        access, audit, bom,
        files::{self, Upload},
        items, revisions,
    },
    stepbom,
    vault::{StoredBlob, Vault},
};

/// Largest BOM table accepted.
pub const MAX_TABLE_BYTES: usize = 20 * 1024 * 1024;
/// Largest STEP assembly accepted.
pub const MAX_STEP_BYTES: usize = 200 * 1024 * 1024;

/// Largest import file accepted with this name.
pub fn max_bytes(filename: &str) -> usize {
    if stepbom::is_step(filename) {
        MAX_STEP_BYTES
    } else {
        MAX_TABLE_BYTES
    }
}

/// "Tables (BOM, result values) can be at most 20 MiB." / "STEP files …"
pub fn too_large(filename: &str) -> AppError {
    AppError::PayloadTooLarge(format!(
        "{} can be at most {} MiB.",
        if stepbom::is_step(filename) {
            "STEP files"
        } else {
            "Tables (BOM, result values)"
        },
        max_bytes(filename) / 1024 / 1024
    ))
}

/// Staged uploads older than this are removed by the housekeeping task.
pub const STAGED_HOURS: i32 = 24;

/// Where the numbers of new items come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberSource {
    /// The part number of the file if it is a valid, unused item number.
    #[default]
    File,
    /// Always a generated number (`PRT-000042`).
    Generate,
}

impl NumberSource {
    pub fn parse(raw: &str) -> AppResult<NumberSource> {
        match raw.trim() {
            "" | "file" => Ok(NumberSource::File),
            "generate" => Ok(NumberSource::Generate),
            other => Err(AppError::BadRequest(format!(
                "Unknown number source '{other}': use 'file' or 'generate'."
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NumberSource::File => "file",
            NumberSource::Generate => "generate",
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportOptions {
    /// Revision to import into (checked out by the user); `None` creates a
    /// new top-level assembly.
    #[serde(default)]
    pub target: Option<Uuid>,
    /// Project group of new items; default: the target's.
    #[serde(default)]
    pub project_id: Option<Uuid>,
    /// Name of a new top-level assembly; default: the assembly row of the
    /// table or the file name.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub numbers: NumberSource,
    #[serde(default = "yes")]
    pub match_existing: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            target: None,
            project_id: None,
            name: String::new(),
            numbers: NumberSource::File,
            match_existing: true,
        }
    }
}

/// An item the import uses or creates.
#[derive(Clone, Debug, Serialize)]
pub struct PlannedItem {
    /// Part number as written in the file.
    pub file_number: String,
    /// Number of an existing item or the number taken from the file;
    /// `None` = generated during the import.
    pub number: Option<String>,
    /// Existing item (not changed by the import).
    pub existing: Option<Uuid>,
    /// Latest revision of an existing item, e.g. `PRT-000001/B`.
    pub existing_label: Option<String>,
    pub item_type: ItemType,
    pub name: String,
    pub description: String,
    pub material: String,
    pub mass_kg: Option<f64>,
    pub unit: String,
}

/// A BOM line the import adds.
#[derive(Clone, Debug, Serialize)]
pub struct PlannedLine {
    /// Row in the file.
    pub row: usize,
    /// 1 = direct child of the root.
    pub level: usize,
    /// Index into [`Plan::items`]; `None` = the root.
    pub parent: Option<usize>,
    pub child: usize,
    pub quantity: f64,
    pub find_number: i32,
}

/// What an import would do.
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub filename: String,
    /// Existing revision the BOM is imported into.
    pub target: Option<Uuid>,
    /// `ASM-000001/A` for a target, the name of a new assembly otherwise.
    pub root_label: String,
    /// Number of a new top-level assembly; `None` = generated.
    pub root_number: Option<String>,
    pub root_name: String,
    pub project_id: Uuid,
    pub project_code: String,
    pub items: Vec<PlannedItem>,
    /// In file order (depth first), so it can be shown as an indented tree.
    pub lines: Vec<PlannedLine>,
    pub columns: Vec<Column>,
    pub warnings: Vec<String>,
    pub new_items: usize,
    pub existing_items: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreatedItem {
    pub file_number: String,
    pub number: String,
    pub name: String,
}

/// What an import did.
#[derive(Clone, Debug, Serialize)]
pub struct ImportResult {
    pub root_revision_id: Uuid,
    /// `ASM-000007/A`.
    pub root: String,
    pub created: Vec<CreatedItem>,
    pub existing_items: usize,
    pub lines: usize,
    pub warnings: Vec<String>,
}

/// Reads an uploaded BOM table or STEP assembly (format and size checks
/// included).
pub fn read_table(filename: &str, bytes: &[u8]) -> AppResult<BomTable> {
    let step = stepbom::is_step(filename);
    if !step && !bomtable::supported(filename) {
        return Err(AppError::BadRequest(format!(
            "{filename}: BOMs are read from CSV, TSV, TXT, XLSX, XLSM, XLSB, XLS and ODS tables \
             and from STEP assemblies (STEP, STP, P21)."
        )));
    }
    if bytes.len() > max_bytes(filename) {
        return Err(too_large(filename));
    }
    if step {
        stepbom::read(bytes).map_err(AppError::BadRequest)
    } else {
        bomtable::read(filename, bytes).map_err(AppError::BadRequest)
    }
}

/// Stores an uploaded STEP assembly in the vault, ready to be attached to the
/// assembly it creates; `None` for tables.
pub async fn step_file(vault: &Vault, filename: &str, bytes: &[u8]) -> AppResult<Option<Upload>> {
    if !stepbom::is_step(filename) {
        return Ok(None);
    }
    let blob = vault.store_bytes(bytes).await?;
    Ok(Some(
        Upload::classify(vault, blob, filename.to_string(), "").await,
    ))
}

/// Rows with the same part number – or, without one, the same name – are one item.
fn item_key(row: &BomRow) -> String {
    let number = row.number.trim();
    if number.is_empty() {
        format!("name:{}", row.name.trim().to_lowercase())
    } else {
        format!("number:{}", number.to_uppercase())
    }
}

fn shown(row: &BomRow) -> &str {
    if row.number.is_empty() {
        &row.name
    } else {
        &row.number
    }
}

fn file_stem(filename: &str) -> &str {
    filename
        .rsplit_once('.')
        .map_or(filename, |(stem, _)| stem)
        .trim()
}

struct Existing {
    id: Uuid,
    item_type: ItemType,
    latest: String,
}

/// Whether `to` can be reached from `from` along the planned lines.
fn reaches(children: &HashMap<usize, Vec<usize>>, from: usize, to: usize) -> bool {
    let mut stack = vec![from];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == to {
            return true;
        }
        if seen.insert(n) {
            if let Some(c) = children.get(&n) {
                stack.extend(c);
            }
        }
    }
    false
}

/// Plans an import. Used for the preview (any connection) and by [`run`]
/// (inside its transaction, the target revision stays locked).
pub async fn plan(
    conn: &mut PgConnection,
    actor: &Actor,
    table: &BomTable,
    filename: &str,
    options: &ImportOptions,
) -> AppResult<Plan> {
    actor.require(Role::Editor)?;
    let mut warnings = table.warnings.clone();

    let target = match options.target {
        Some(id) => {
            let rev = revisions::lock(conn, id).await?;
            revisions::ensure_editable(&rev, actor)?;
            Some(rev)
        }
        None => None,
    };
    let project_id = options
        .project_id
        .or_else(|| target.as_ref().and_then(|t| t.project_id))
        .ok_or_else(|| {
            AppError::BadRequest("Choose the project group the new items belong to.".into())
        })?;
    let (project_code, active) =
        sqlx::query_as::<_, (String, bool)>("SELECT code, active FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(access::unknown_project)?;
    access::ensure_visible(actor, Some(project_id), access::unknown_project())?;
    if !active {
        return Err(AppError::BadRequest(format!(
            "The project group {project_code} is archived."
        )));
    }
    if !actor.can_edit_project(Some(project_id)) {
        return Err(AppError::Forbidden(format!(
            "You are not a member of the project group {project_code}; ask an admin to add you."
        )));
    }

    // Items of TeamCraft with the part numbers of the file.
    let numbers: Vec<String> = table
        .root
        .iter()
        .chain(&table.rows)
        .filter_map(|r| domain::normalize_item_number(&r.number).ok())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let found = sqlx::query_as::<_, (Uuid, String, String, String, Option<Uuid>)>(
        "SELECT i.id, i.number, i.item_type,
                COALESCE((SELECT r.code FROM revisions r WHERE r.item_id = i.id
                          ORDER BY r.seq DESC LIMIT 1), '?'),
                i.project_id
         FROM items i WHERE i.number = ANY($1)",
    )
    .bind(&numbers)
    .fetch_all(&mut *conn)
    .await?;
    // Numbers of items the actor may not see: taken, but never matched or
    // linked, and neither type nor revision is shown.
    let unavailable: HashSet<String> = found
        .iter()
        .filter(|f| !actor.can_see_project(f.4))
        .map(|f| f.1.clone())
        .collect();
    let existing: HashMap<String, Existing> = found
        .into_iter()
        .filter(|f| actor.can_see_project(f.4))
        .map(|(id, number, item_type, code, _)| {
            let latest = format!("{number}/{code}");
            let item_type = ItemType::parse(&item_type).unwrap_or(ItemType::Part);
            (
                number,
                Existing {
                    id,
                    item_type,
                    latest,
                },
            )
        })
        .collect();

    // The root: the target revision or a new top-level assembly.
    let (root_label, root_number, root_name, root_item_number) = match &target {
        Some(rev) => (
            revisions::label(rev),
            None,
            rev.name.clone(),
            Some(rev.item_number.clone()),
        ),
        None => {
            let row = table.root.as_ref();
            let name = [
                options.name.trim(),
                row.map_or("", |r| r.name.as_str()),
                file_stem(filename),
            ]
            .into_iter()
            .find(|n| !n.is_empty())
            .unwrap_or("Imported assembly");
            let name = domain::truncate_chars(name, 200);
            let number = match row.and_then(|r| domain::normalize_item_number(&r.number).ok()) {
                Some(n) if options.numbers == NumberSource::File && unavailable.contains(&n) => {
                    warnings.push(format!(
                        "{n} is not available: the new assembly gets a generated number."
                    ));
                    None
                }
                Some(n) if options.numbers == NumberSource::File && !existing.contains_key(&n) => {
                    Some(n)
                }
                Some(n) if options.numbers == NumberSource::File => {
                    warnings.push(format!(
                        "{n} exists already: the new assembly gets a generated number. To fill \
                         the existing assembly, check it out and import into it."
                    ));
                    None
                }
                _ => None,
            };
            (name.clone(), number.clone(), name, number)
        }
    };

    // Lines already in the target BOM.
    let mut used_find: HashMap<Option<usize>, HashSet<i32>> = HashMap::new();
    let mut target_children = HashSet::new();
    if let Some(rev) = &target {
        for (find, child) in sqlx::query_as::<_, (i32, Uuid)>(
            "SELECT find_number, child_item_id FROM bom_lines WHERE parent_revision_id = $1",
        )
        .bind(rev.id)
        .fetch_all(&mut *conn)
        .await?
        {
            used_find.entry(None).or_default().insert(find);
            target_children.insert(child);
        }
    }

    let mut items: Vec<PlannedItem> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut expanded: HashSet<usize> = HashSet::new();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut lines: Vec<PlannedLine> = Vec::new();
    let mut requested: Vec<Option<i32>> = Vec::new();
    let mut line_of: HashMap<(Option<usize>, usize), usize> = HashMap::new();
    // (level, item) of the current ancestors; the root is level 0.
    let mut stack: Vec<(usize, Option<usize>)> = vec![(0, None)];
    let mut skip_below: Option<usize> = None;
    let rows = &table.rows;
    // The rows below the first occurrence of each sub-assembly (index of its
    // row): tables that list a BOM only once (STEP files) need them again
    // when that occurrence is skipped, e.g. below an existing item.
    let mut bom_rows: HashMap<String, usize> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        if rows.get(i + 1).is_some_and(|n| n.level > row.level) {
            bom_rows.entry(item_key(row)).or_insert(i);
        }
    }
    let mut queue: VecDeque<BomRow> = rows.iter().cloned().collect();
    let mut copied = 0;
    while let Some(row) = queue.pop_front() {
        let row = &row;
        if let Some(l) = skip_below {
            if row.level > l {
                continue;
            }
            skip_below = None;
        }
        while stack.len() > 1 && stack.last().is_some_and(|(l, _)| *l >= row.level) {
            stack.pop();
        }
        let parent = stack.last().and_then(|(_, p)| *p);
        let mut has_children = queue.front().is_some_and(|n| n.level > row.level);
        let skip = |reason: String, warnings: &mut Vec<String>, skip_below: &mut Option<usize>| {
            warnings.push(format!("Row {}: {reason}", row.line));
            if has_children {
                *skip_below = Some(row.level);
            }
        };
        let normalized = domain::normalize_item_number(&row.number).ok();
        if normalized.is_some() && normalized == root_item_number {
            skip(
                format!("{} is the assembly itself and is skipped.", row.number),
                &mut warnings,
                &mut skip_below,
            );
            continue;
        }
        let matched = normalized
            .as_ref()
            .filter(|_| options.match_existing)
            .and_then(|n| existing.get(n));
        if parent.is_none() && matched.is_some_and(|e| target_children.contains(&e.id)) {
            skip(
                format!(
                    "{} is already in the BOM of {root_label} – not added again.",
                    shown(row)
                ),
                &mut warnings,
                &mut skip_below,
            );
            continue;
        }
        if let (Some(t), Some(e)) = (&target, matched) {
            if parent.is_none() && bom::creates_cycle(conn, t.item_id, e.id).await? {
                skip(
                    format!(
                        "{} contains {root_label} – adding it would create a circular BOM.",
                        e.latest
                    ),
                    &mut warnings,
                    &mut skip_below,
                );
                continue;
            }
        }
        let key = item_key(row);
        let known = by_key.get(&key).copied();
        if let Some(c) = known {
            let circular = match parent {
                Some(p) => p == c || reaches(&children, c, p),
                None => false,
            };
            if circular {
                skip(
                    format!("{} would contain itself – the row is skipped.", shown(row)),
                    &mut warnings,
                    &mut skip_below,
                );
                continue;
            }
        }
        let child = match known {
            Some(c) => c,
            None => {
                // Why the number from the file cannot be used, if it cannot.
                let taken = normalized.as_ref().and_then(|n| {
                    if unavailable.contains(n) {
                        Some("is not available")
                    } else if existing.contains_key(n) {
                        Some("exists already")
                    } else {
                        None
                    }
                });
                items.push(planned_item(
                    row,
                    normalized,
                    matched,
                    taken,
                    options,
                    &mut warnings,
                ));
                by_key.insert(key, items.len() - 1);
                items.len() - 1
            }
        };
        match line_of.get(&(parent, child)) {
            Some(&l) => {
                lines[l].quantity += row.quantity;
                warnings.push(format!(
                    "Row {}: {} appears again below the same assembly – the quantities are added.",
                    row.line,
                    shown(row)
                ));
            }
            None => {
                line_of.insert((parent, child), lines.len());
                if let Some(p) = parent {
                    children.entry(p).or_default().push(child);
                }
                lines.push(PlannedLine {
                    row: row.line,
                    level: row.level,
                    parent,
                    child,
                    quantity: row.quantity,
                    find_number: 0,
                });
                requested.push(row.find_number);
            }
        }
        // A new sub-assembly whose BOM rows were skipped (or come later) at
        // another occurrence: read them here.
        if !has_children && items[child].existing.is_none() && !expanded.contains(&child) {
            if let Some(&j) = bom_rows.get(&item_key(row)) {
                let below: Vec<BomRow> = rows[j + 1..]
                    .iter()
                    .take_while(|r| r.level > rows[j].level)
                    .map(|r| BomRow {
                        level: r.level - rows[j].level + row.level,
                        ..r.clone()
                    })
                    .collect();
                copied += below.len();
                if copied > bomtable::MAX_ROWS {
                    return Err(AppError::BadRequest(format!(
                        "The BOM has more than {} rows.",
                        bomtable::MAX_ROWS
                    )));
                }
                for r in below.into_iter().rev() {
                    queue.push_front(r);
                }
                has_children = true;
            }
        }
        if has_children {
            if items[child].existing.is_some() {
                warnings.push(format!(
                    "Row {}: {} exists already – its BOM is not changed by the import, the rows \
                     below it are skipped.",
                    row.line,
                    shown(row)
                ));
                skip_below = Some(row.level);
            } else if !expanded.insert(child) {
                warnings.push(format!(
                    "Row {}: the BOM of {} was read from an earlier row – the rows below it are \
                     skipped.",
                    row.line,
                    shown(row)
                ));
                skip_below = Some(row.level);
            } else {
                items[child].item_type = ItemType::Assembly;
                stack.push((row.level, Some(child)));
            }
        }
    }
    if lines.is_empty() {
        return Err(AppError::BadRequest(
            "Nothing to import: every row of the table was skipped.".into(),
        ));
    }

    // Find numbers: the file's where free, then the next multiples of 10.
    for (i, line) in lines.iter_mut().enumerate() {
        if let Some(f) = requested[i] {
            if used_find.entry(line.parent).or_default().insert(f) {
                line.find_number = f;
            } else {
                warnings.push(format!(
                    "Row {}: find number {f} is already used – the next free one is taken.",
                    line.row
                ));
            }
        }
    }
    for line in lines.iter_mut().filter(|l| l.find_number == 0) {
        let used = used_find.entry(line.parent).or_default();
        let next = (used.iter().max().copied().unwrap_or(0) / 10 + 1) * 10;
        used.insert(next);
        line.find_number = next;
    }

    let existing_items = items.iter().filter(|i| i.existing.is_some()).count();
    Ok(Plan {
        filename: filename.to_string(),
        target: target.as_ref().map(|t| t.id),
        root_label,
        root_number,
        root_name,
        project_id,
        project_code,
        new_items: items.len() - existing_items,
        existing_items,
        items,
        lines,
        columns: table.columns.clone(),
        warnings,
    })
}

fn planned_item(
    row: &BomRow,
    normalized: Option<String>,
    matched: Option<&Existing>,
    taken: Option<&'static str>,
    options: &ImportOptions,
    warnings: &mut Vec<String>,
) -> PlannedItem {
    let mut item = PlannedItem {
        file_number: row.number.clone(),
        number: None,
        existing: None,
        existing_label: None,
        item_type: ItemType::Part,
        name: domain::truncate_chars(&row.name, 200),
        description: domain::truncate_chars(&row.description, 9_000),
        material: domain::truncate_chars(&row.material, 200),
        mass_kg: row.mass_kg,
        unit: if row.unit.is_empty() {
            "each".into()
        } else {
            row.unit.clone()
        },
    };
    match (normalized, matched) {
        (Some(n), Some(e)) => {
            item.existing = Some(e.id);
            item.existing_label = Some(e.latest.clone());
            item.item_type = e.item_type;
            item.number = Some(n);
        }
        (Some(n), None) if options.numbers == NumberSource::File => {
            if let Some(why) = taken {
                warnings.push(format!(
                    "Row {}: {n} {why} – the new item gets a generated number.",
                    row.line
                ));
            } else {
                item.number = Some(n);
            }
        }
        (None, _) if !row.number.is_empty() && options.numbers == NumberSource::File => {
            warnings.push(format!(
                "Row {}: '{}' is not a valid item number (A–Z, 0–9, '.', '_', '-') – a number is \
                 generated.",
                row.line, row.number
            ));
        }
        _ => {}
    }
    if item.existing.is_none() && item.number.is_none() && !row.number.is_empty() {
        let note = format!("Part number in CAD: {}", row.number);
        item.description = if item.description.is_empty() {
            note
        } else {
            format!("{}\n{note}", item.description)
        };
    }
    item
}

/// Runs an import in one transaction. `source` (the STEP file, see
/// [`step_file`]) is attached to a new top-level assembly.
pub async fn run(
    pool: &PgPool,
    actor: &Actor,
    table: &BomTable,
    filename: &str,
    options: &ImportOptions,
    source: Option<&Upload>,
) -> AppResult<ImportResult> {
    let mut tx = pool.begin().await?;
    let result = run_tx(&mut tx, actor, table, filename, options, source).await?;
    tx.commit().await?;
    Ok(result)
}

async fn run_tx(
    conn: &mut PgConnection,
    actor: &Actor,
    table: &BomTable,
    filename: &str,
    options: &ImportOptions,
    source: Option<&Upload>,
) -> AppResult<ImportResult> {
    let plan = plan(conn, actor, table, filename, options).await?;
    let notes = domain::truncate_chars(&format!("Imported from {}", plan.filename), 500);

    // (item id, number, new revision) per planned item. Existing items and
    // items with numbers from the file come first, so that generated numbers
    // (the root's included) can never take one of them.
    let mut ids: Vec<Option<(Uuid, String, Option<Uuid>)>> = vec![None; plan.items.len()];
    let mut created = Vec::new();
    for (i, it) in plan.items.iter().enumerate() {
        match (it.existing, &it.number) {
            (Some(id), Some(n)) => ids[i] = Some((id, n.clone(), None)),
            (None, Some(_)) => {
                ids[i] = Some(create_item(conn, actor, &plan, it, &mut created).await?)
            }
            _ => {}
        }
    }
    let root = match plan.target {
        Some(id) => revisions::lock(conn, id).await?,
        None => {
            let c = items::create_tx(
                conn,
                actor,
                items::NewItem {
                    number: plan.root_number.clone(),
                    item_type: ItemType::Assembly,
                    unit: "each".into(),
                    project_id: Some(plan.project_id),
                    name: plan.root_name.clone(),
                    description: format!("Imported from {}", plan.filename),
                    material: String::new(),
                    mass_kg: None,
                    unit_cost: None,
                    classification: Default::default(),
                    attributes: Vec::new(),
                },
            )
            .await?;
            let rev = revisions::lock(conn, c.revision_id).await?;
            if let Some(upload) = source {
                files::store_uploads(conn, actor, &rev, std::slice::from_ref(upload), &notes)
                    .await?;
            }
            rev
        }
    };
    for (i, it) in plan.items.iter().enumerate() {
        if ids[i].is_none() {
            ids[i] = Some(create_item(conn, actor, &plan, it, &mut created).await?);
        }
    }

    let mut parents: HashMap<usize, crate::models::Revision> = HashMap::new();
    for line in &plan.lines {
        let Some((child_id, child_number, _)) = ids[line.child].clone() else {
            return Err(AppError::Other(anyhow::anyhow!("import: item not created")));
        };
        let parent: &crate::models::Revision = match line.parent {
            None => &root,
            Some(p) => match parents.entry(p) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(v) => {
                    let rev_id = ids[p].as_ref().and_then(|(_, _, r)| *r).ok_or_else(|| {
                        AppError::Other(anyhow::anyhow!("import: parent without new revision"))
                    })?;
                    v.insert(revisions::lock(conn, rev_id).await?)
                }
            },
        };
        bom::add_tx(
            conn,
            actor,
            parent,
            child_id,
            &child_number,
            line.quantity,
            Some(line.find_number),
            &notes,
        )
        .await?;
    }

    let label = revisions::label(&root);
    audit::record(
        conn,
        Some(actor),
        "import.bom",
        "revision",
        Some(root.id),
        format!(
            "Imported {} BOM lines from {} into {label} ({} new items)",
            plan.lines.len(),
            plan.filename,
            created.len()
        ),
        json!({
            "item_id": root.item_id,
            "filename": plan.filename,
            "created": created.len(),
            "existing": plan.existing_items,
            "lines": plan.lines.len(),
            "warnings": plan.warnings.len(),
        }),
    )
    .await?;
    Ok(ImportResult {
        root_revision_id: root.id,
        root: label,
        created,
        existing_items: plan.existing_items,
        lines: plan.lines.len(),
        warnings: plan.warnings,
    })
}

async fn create_item(
    conn: &mut PgConnection,
    actor: &Actor,
    plan: &Plan,
    it: &PlannedItem,
    created: &mut Vec<CreatedItem>,
) -> AppResult<(Uuid, String, Option<Uuid>)> {
    let c = items::create_tx(
        conn,
        actor,
        items::NewItem {
            number: it.number.clone(),
            item_type: it.item_type,
            unit: it.unit.clone(),
            project_id: Some(plan.project_id),
            name: it.name.clone(),
            description: it.description.clone(),
            material: it.material.clone(),
            mass_kg: it.mass_kg,
            unit_cost: None,
            classification: Default::default(),
            attributes: Vec::new(),
        },
    )
    .await?;
    created.push(CreatedItem {
        file_number: it.file_number.clone(),
        number: c.number.clone(),
        name: it.name.clone(),
    });
    Ok((c.item_id, c.number, Some(c.revision_id)))
}

// ---------------------------------------------------------------------------
// Staged uploads (web UI: upload → preview → import)
// ---------------------------------------------------------------------------

/// An upload waiting for the user's confirmation.
#[derive(Clone, Debug)]
pub struct Staged {
    pub id: Uuid,
    pub filename: String,
    pub options: ImportOptions,
    content: Vec<u8>,
    /// Vault blob of a STEP file.
    sha256: Option<String>,
}

impl Staged {
    pub fn table(&self) -> AppResult<BomTable> {
        read_table(&self.filename, &self.content)
    }

    /// The staged STEP file, to attach to the new assembly.
    async fn source(&self, vault: &Vault) -> Option<Upload> {
        let blob = StoredBlob {
            sha256: self.sha256.clone()?,
            size: self.content.len() as u64,
        };
        Some(Upload::classify(vault, blob, self.filename.clone(), "").await)
    }
}

/// Stores an upload for the preview; fails right away if the file cannot be
/// read. STEP files go to the vault, tables into the row.
pub async fn stage(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    filename: &str,
    content: Vec<u8>,
    options: ImportOptions,
) -> AppResult<Uuid> {
    actor.require(Role::Editor)?;
    read_table(filename, &content)?;
    let (kind, row_content, sha256) = if stepbom::is_step(filename) {
        let blob = vault.store_bytes(&content).await?;
        ("step", None, Some(blob.sha256))
    } else {
        ("bom_table", Some(content.as_slice()), None)
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO staged_imports (id, user_id, kind, filename, size_bytes, content, sha256, options)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(actor.id)
    .bind(kind)
    .bind(domain::truncate_chars(filename, 255))
    .bind(content.len() as i64)
    .bind(row_content)
    .bind(sha256)
    .bind(sqlx::types::Json(&options))
    .execute(pool)
    .await?;
    Ok(id)
}

const GONE: &str = "This import is no longer available – upload the file again.";

type StagedRow = (
    Uuid,
    String,
    sqlx::types::Json<ImportOptions>,
    Option<Vec<u8>>,
    Option<String>,
);

async fn staged_of(vault: &Vault, row: StagedRow) -> AppResult<Staged> {
    let (id, filename, options, content, sha256) = row;
    let content = match (content, &sha256) {
        (Some(content), _) => content,
        (None, Some(sha)) => tokio::fs::read(vault.object_path(sha)?)
            .await
            .map_err(|_| AppError::not_found(GONE))?,
        (None, None) => return Err(AppError::not_found(GONE)),
    };
    Ok(Staged {
        id,
        filename,
        options: options.0,
        content,
        sha256,
    })
}

/// A staged upload of the actor.
pub async fn staged(pool: &PgPool, vault: &Vault, actor: &Actor, id: Uuid) -> AppResult<Staged> {
    let row = sqlx::query_as::<_, StagedRow>(
        "SELECT id, filename, options, content, sha256 FROM staged_imports
         WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(actor.id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found(GONE))?;
    staged_of(vault, row).await
}

/// The preview of a staged upload.
pub async fn preview(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
) -> AppResult<(Staged, Plan)> {
    let staged = staged(pool, vault, actor, id).await?;
    let table = staged.table()?;
    let mut conn = pool.acquire().await?;
    let plan = plan(&mut conn, actor, &table, &staged.filename, &staged.options).await?;
    Ok((staged, plan))
}

/// Runs a staged import and removes the upload (a second click finds nothing).
pub async fn run_staged(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
) -> AppResult<ImportResult> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query_as::<_, StagedRow>(
        "SELECT id, filename, options, content, sha256 FROM staged_imports
         WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(actor.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found(GONE))?;
    let staged = staged_of(vault, row).await?;
    let table = staged.table()?;
    let source = staged.source(vault).await;
    let result = run_tx(
        &mut tx,
        actor,
        &table,
        &staged.filename,
        &staged.options,
        source.as_ref(),
    )
    .await?;
    sqlx::query("DELETE FROM staged_imports WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result)
}

/// Discards a staged upload; returns its options (to go back to the target).
pub async fn discard(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ImportOptions> {
    let options = sqlx::query_scalar::<_, sqlx::types::Json<ImportOptions>>(
        "DELETE FROM staged_imports WHERE id = $1 AND user_id = $2 RETURNING options",
    )
    .bind(id)
    .bind(actor.id)
    .fetch_optional(pool)
    .await?
    .map(|o| o.0)
    .unwrap_or_default();
    Ok(options)
}

/// Removes staged uploads older than [`STAGED_HOURS`] (housekeeping).
pub async fn purge_stale(pool: &PgPool) -> AppResult<u64> {
    Ok(sqlx::query(
        "DELETE FROM staged_imports WHERE created_at < now() - make_interval(hours => $1)",
    )
    .bind(STAGED_HOURS)
    .execute(pool)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_keys() {
        assert_eq!(NumberSource::parse("").unwrap(), NumberSource::File);
        assert_eq!(
            NumberSource::parse("generate").unwrap().as_str(),
            "generate"
        );
        assert!(NumberSource::parse("random").is_err());
        let o: ImportOptions = serde_json::from_str("{}").unwrap();
        assert!(o.match_existing && o.target.is_none() && o.numbers == NumberSource::File);
        let t = read_table("a.csv", b"Part Number,Name,QTY\nab-1,One,1\n,Screw M6,2\n").unwrap();
        assert_eq!(item_key(&t.rows[0]), "number:AB-1");
        assert_eq!(item_key(&t.rows[1]), "name:screw m6");
        assert_eq!(file_stem("Pump BOM.xlsx"), "Pump BOM");
        assert!(read_table("a.pdf", b"x").is_err());
    }

    #[test]
    fn reachability() {
        let children = HashMap::from([(0, vec![1]), (1, vec![2]), (3, vec![0])]);
        assert!(reaches(&children, 0, 2));
        assert!(reaches(&children, 3, 2));
        assert!(!reaches(&children, 2, 0));
    }
}
