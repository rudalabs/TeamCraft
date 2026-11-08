//! Bills of materials: tree, import, packages, working sets and BOM lines.

use super::*;

// ---------------------------------------------------------------------------
// BOM
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct BomQuery {
    rule: Option<String>,
    /// `true` returns the expanded multi-level tree with roll-ups.
    expand: Option<bool>,
}

pub(super) async fn get_bom(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<BomQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if q.expand.unwrap_or(false) {
        let rule = bom_rule(q.rule.as_deref())?;
        // serialized straight from the tree: an expanded BOM can be tens of
        // megabytes, an intermediate JSON value would double the work
        Ok(Json(bom::tree(&st.db, &me, rev.id, rule).await?).into_response())
    } else {
        Ok(Json(bom::lines(&st.db, &me, rev.id).await?).into_response())
    }
}

pub(super) fn bom_rule(raw: Option<&str>) -> AppResult<RevisionRule> {
    match raw {
        Some(r) => RevisionRule::parse(r).ok_or_else(|| {
            AppError::BadRequest("rule must be latest, released or as_released.".into())
        }),
        None => Ok(RevisionRule::default()),
    }
}

/// The expanded BOM as Excel workbook.
pub(super) async fn get_bom_xlsx(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    log: AccessCtx,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<BomQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let rule = bom_rule(q.rule.as_deref())?;
    let reader = project_policy::package_actor(&st.db, &me, rev.project_id).await?;
    let tree = bom::tree(&st.db, &reader, rev.id, rule).await?;
    access_log::record_structure(
        &st.db,
        &log,
        &me,
        access_log::Kind::Export,
        &bom::tree_revision_ids(&tree),
    )
    .await?;
    let filename = format!(
        "{}_{}_bom_{}.xlsx",
        rev.item_number,
        rev.code,
        rule.as_str()
    );
    Ok(web::xlsx_response(bom::to_xlsx(&tree)?, &filename))
}

/// BOM import that creates a new top-level assembly (multipart: `file`,
/// `project`, `name`, `numbers`, `match_existing`, `dry_run`).
pub(super) async fn import_bom(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    mut multipart: Multipart,
) -> AppResult<Response> {
    bom_import(&st, &me, None, &mut multipart).await
}

/// BOM import into a revision the caller has checked out.
pub(super) async fn import_bom_into(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    bom_import(&st, &me, Some(rev.id), &mut multipart).await
}

/// `dry_run=true` returns the plan (what the preview shows), otherwise the
/// import runs in one transaction and returns what it created.
pub(super) async fn bom_import(
    st: &AppState,
    me: &crate::auth::Actor,
    target: Option<Uuid>,
    multipart: &mut Multipart,
) -> AppResult<Response> {
    let (file, fields) = web::receive_import(multipart).await?;
    let project_id = match fields.get("project").map(|p| p.trim()) {
        Some(code) if !code.is_empty() => Some(projects::get_by_code(&st.db, me, code).await?.id),
        _ => None,
    };
    let options = web::import_options(&fields, target, project_id)?;
    let (filename, content) = file.ok_or_else(|| {
        AppError::BadRequest(
            "Send the BOM table (CSV or Excel) or the STEP assembly in the multipart field 'file'."
                .into(),
        )
    })?;
    let table = imports::read_table(&filename, &content)?;
    let dry_run = fields
        .get("dry_run")
        .is_some_and(|v| matches!(v.trim(), "true" | "1" | "on" | "yes"));
    if dry_run {
        let mut conn = st.db.acquire().await?;
        let plan = imports::plan(&mut conn, me, &table, &filename, &options).await?;
        return Ok(Json(json!(plan)).into_response());
    }
    let source = imports::step_file(&st.vault, &filename, &content).await?;
    let result = imports::run(&st.db, me, &table, &filename, &options, source.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(json!(result))).into_response())
}

#[derive(Deserialize)]
pub(super) struct PackageQuery {
    rule: Option<String>,
}

/// "Pack and Go": the revision and its structure as ZIP (files, bom.csv,
/// manifest.json). Default rule: as_released for released revisions,
/// latest otherwise.
pub(super) async fn get_package(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    log: AccessCtx,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<PackageQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if let Some(r) = q.rule.as_deref().filter(|r| !r.trim().is_empty()) {
        RevisionRule::parse(r).ok_or_else(|| {
            AppError::BadRequest("rule must be latest, released or as_released.".into())
        })?;
    }
    let rule = web::package_rule(q.rule.as_deref(), &rev.state);
    web::package_response(package::build(&st.db, &st.vault, &me, rev.id, rule, &log).await?)
}

#[derive(Deserialize)]
pub(super) struct WorksetQuery {
    rule: Option<String>,
    /// `false` leaves out the items found through references inside CAD files.
    references: Option<bool>,
}

/// The working set for the desktop "Get": the revision, its multi-level BOM
/// under a revision rule (default latest) and the items holding the CAD files
/// they reference – every file with folder, version, SHA-256 and the lock of
/// its revision.
pub(super) async fn get_workset(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<WorksetQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let rule = bom_rule(q.rule.as_deref().filter(|r| !r.trim().is_empty()))?;
    Ok(Json(json!(
        workset::resolve(&st.db, &me, rev.id, rule, q.references.unwrap_or(true)).await?
    )))
}

#[derive(Deserialize)]
pub(super) struct SimilarQuery {
    limit: Option<i64>,
    /// Compare proportions and shape only.
    ignore_scale: Option<bool>,
    /// The 3D file to compare by (default: the STEP model, else the first mesh).
    file: Option<String>,
    /// Seconds to wait for a shape signature that is still being computed.
    wait: Option<u64>,
}

/// Similar parts by shape (see `services::similar`).
pub(super) async fn get_similar(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<SimilarQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let options = similar::Options {
        limit: q.limit.unwrap_or(10).clamp(1, similar::MAX_LIMIT),
        ignore_scale: q.ignore_scale.unwrap_or(false),
    };
    let wait = std::time::Duration::from_secs(q.wait.unwrap_or(30).min(200));
    let report = similar::for_revision(&st, &me, &rev, q.file.as_deref(), options, wait).await?;
    let mut out = json!(report);
    out["item"] = json!(rev.item_number);
    out["revision"] = json!(rev.code);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub(super) struct AddBomLine {
    child: String,
    quantity: f64,
    find_number: Option<i32>,
    #[serde(default)]
    notes: String,
}

pub(super) async fn add_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddBomLine>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let line = bom::add(
        &st.db,
        &me,
        rev.id,
        bom::NewBomLine {
            child_number: body.child,
            quantity: body.quantity,
            find_number: body.find_number,
            notes: body.notes,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(line))).into_response())
}

#[derive(Deserialize)]
pub(super) struct UpdateBomLine {
    quantity: Option<f64>,
    find_number: Option<i32>,
    notes: Option<String>,
}

pub(super) async fn update_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateBomLine>,
) -> AppResult<Json<Value>> {
    let current = bom::line(&st.db, &me, id).await?;
    let line = bom::update(
        &st.db,
        &me,
        id,
        bom::BomLineUpdate {
            quantity: body.quantity.unwrap_or(current.quantity),
            find_number: body.find_number.unwrap_or(current.find_number),
            notes: body.notes.unwrap_or(current.notes),
        },
    )
    .await?;
    Ok(Json(json!(line)))
}

pub(super) async fn delete_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    bom::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The BOM the CAD references of the revision lead to (read-only).
pub(super) async fn get_bom_from_references(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(bom_references::plan(&st, &me, rev.id).await?).into_response())
}

/// Adds the chosen children of the plan as BOM lines (one transaction).
pub(super) async fn post_bom_from_references(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<bom_references::ApplyRequest>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let result = bom_references::apply(&st, &me, rev.id, &body).await?;
    Ok((StatusCode::OK, Json(json!(result))).into_response())
}
