//! Mass properties of a revision (`/revisions/{id}/mass`, CSV and JSON
//! exports, the mass settings form) and the material library (`/materials`).

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Form, Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, render, WebCtx};
use crate::{
    auth::WebUser,
    domain::{RevisionRule, Role},
    error::{AppError, AppResult},
    massprops::LengthUnit,
    services::{
        access_log::{self, AccessCtx},
        mass_properties::{self as mass, MassSettingsUpdate, MaterialInput},
        project_policy, revisions,
    },
    util, AppState,
};

/// The material library as suggestions for the Material field of a form
/// (`[{name, label}]`, label = density), in the library's order.
pub(super) async fn material_options(st: &AppState) -> AppResult<serde_json::Value> {
    Ok(json!(mass::materials(&st.db)
        .await?
        .iter()
        .map(|m| json!({ "name": m.name, "label": format!("{} kg/m³", m.density_kg_m3) }))
        .collect::<Vec<_>>()))
}

#[derive(Deserialize)]
pub struct MassQuery {
    rule: Option<String>,
    /// Reference point `x,y,z` in metres.
    #[serde(rename = "ref")]
    reference: Option<String>,
}

fn rule_of(q: &MassQuery) -> RevisionRule {
    q.rule
        .as_deref()
        .and_then(RevisionRule::parse)
        .unwrap_or_default()
}

/// `GET /revisions/{id}/mass` – the roll-up and the settings of the revision.
pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<MassQuery>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let rev = revisions::get(&st.db, me, id).await?;
    let rule = rule_of(&q);
    let (reference, reference_error) = match mass::parse_point(q.reference.as_deref()) {
        Ok(p) => (p, None),
        Err(e) => ([0.0; 3], Some(e.public_message())),
    };
    let report = mass::rollup(&st, me, id, rule, reference).await?;
    let settings = mass::revision_settings(&st.db, me, id).await?;
    let editable = revisions::ensure_editable(&rev, me).is_ok();
    let chosen = settings
        .geometry_file_id
        .and_then(|fid| settings.files.iter().find(|f| f.id == fid));
    let units: Vec<_> = LengthUnit::ALL
        .iter()
        .map(|u| json!({ "value": u.as_str() }))
        .collect();
    let s = &settings.settings;
    let cg = s.measured_cg_m.clone().unwrap_or_default();
    let inertia = s.measured_inertia_kg_m2.clone().unwrap_or_default();
    let reference_text = q
        .reference
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "0,0,0".into());
    render(
        &st,
        &ctx,
        "revisions/mass.html",
        json!({
            "rev": rev,
            "rule": rule.as_str(),
            "report": report,
            "reference": reference_text,
            "reference_error": reference_error,
            "settings": settings,
            "chosen": chosen,
            "chosen_unit": chosen.map(|f| f.unit().as_str()),
            "units": units,
            "measured_cg": cg.iter().map(|v| util::format_number(*v, 9)).collect::<Vec<_>>(),
            "measured_inertia": inertia.iter().map(|v| util::format_number(*v, 9)).collect::<Vec<_>>(),
            "editable": editable,
            "is_admin": me.require(Role::Admin).is_ok(),
        }),
    )
}

/// The report for a download: the root's group must allow BOM downloads,
/// items of groups that do not are left out (like the BOM exports); the
/// read is logged.
async fn export(
    st: &AppState,
    me: &crate::auth::Actor,
    log: &AccessCtx,
    id: Uuid,
    q: &MassQuery,
) -> AppResult<(crate::models::Revision, RevisionRule, mass::MassReport)> {
    let rev = revisions::get(&st.db, me, id).await?;
    let rule = rule_of(q);
    let reference = mass::parse_point(q.reference.as_deref())?;
    let reader = project_policy::package_actor(&st.db, me, rev.project_id).await?;
    let report = mass::rollup(st, &reader, id, rule, reference).await?;
    access_log::record_structure(
        &st.db,
        log,
        me,
        access_log::Kind::Export,
        &mass::report_revision_ids(&report),
    )
    .await?;
    Ok((rev, rule, report))
}

/// `GET /revisions/{id}/mass.csv`
pub async fn csv(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    log: AccessCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<MassQuery>,
) -> AppResult<Response> {
    let (rev, rule, report) = export(&st, &me, &log, id, &q).await?;
    let filename = format!(
        "{}_{}_mass_{}.csv",
        rev.item_number,
        rev.code,
        rule.as_str()
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                util::content_disposition(&filename),
            ),
        ],
        mass::to_csv(&report),
    )
        .into_response())
}

/// `GET /revisions/{id}/mass.json`
pub async fn json_export(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    log: AccessCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<MassQuery>,
) -> AppResult<Response> {
    let (rev, rule, report) = export(&st, &me, &log, id, &q).await?;
    let filename = format!(
        "{}_{}_mass_{}.json",
        rev.item_number,
        rev.code,
        rule.as_str()
    );
    let mut response = Json(report).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&util::content_disposition(&filename)) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(response)
}

/// Parses an optional number of a form field (empty = none).
fn number(field: &str, raw: &str) -> AppResult<Option<f64>> {
    match raw.trim() {
        "" => Ok(None),
        v => v
            .replace(',', ".")
            .parse::<f64>()
            .map(Some)
            .map_err(|_| AppError::BadRequest(format!("{field}: '{v}' is not a number."))),
    }
}

/// Parses `n` numbers of a form, all empty = none, some empty = error.
fn numbers<const N: usize>(field: &str, raw: [&str; N]) -> AppResult<Option<[f64; N]>> {
    if raw.iter().all(|r| r.trim().is_empty()) {
        return Ok(None);
    }
    let mut out = [0.0; N];
    for (o, r) in out.iter_mut().zip(raw) {
        *o = number(field, r)?.ok_or_else(|| {
            AppError::BadRequest(format!("{field}: fill in every value or none."))
        })?;
    }
    Ok(Some(out))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SettingsForm {
    density: String,
    geometry_file: String,
    length_unit: String,
    instances: String,
    measured_mass: String,
    cg_x: String,
    cg_y: String,
    cg_z: String,
    ixx: String,
    iyy: String,
    izz: String,
    ixy: String,
    iyz: String,
    ixz: String,
    note: String,
}

fn settings_from(f: &SettingsForm) -> AppResult<MassSettingsUpdate> {
    let geometry_file_id = match f.geometry_file.trim() {
        "" | "auto" => None,
        v => Some(
            Uuid::parse_str(v)
                .map_err(|_| AppError::BadRequest("Unknown geometry file.".into()))?,
        ),
    };
    let instances = match f.instances.trim() {
        "" => None,
        v => Some(v.parse::<i32>().map_err(|_| {
            AppError::BadRequest(format!("Instances: '{v}' is not a whole number."))
        })?),
    };
    Ok(MassSettingsUpdate {
        density_kg_m3: number("Density", &f.density)?,
        geometry_file_id,
        length_unit: Some(f.length_unit.clone()).filter(|u| !u.trim().is_empty()),
        instances,
        measured_mass_kg: number("Measured mass", &f.measured_mass)?,
        measured_cg_m: numbers("Centre of gravity", [&f.cg_x, &f.cg_y, &f.cg_z])?,
        measured_inertia_kg_m2: numbers(
            "Inertia tensor",
            [&f.ixx, &f.iyy, &f.izz, &f.ixy, &f.iyz, &f.ixz],
        )?,
        measured_note: f.note.clone(),
    })
}

/// `POST /revisions/{id}/mass` – the mass settings form.
pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<SettingsForm>,
) -> AppResult<Response> {
    let back = format!("/revisions/{id}/mass");
    let result = match settings_from(&f) {
        Ok(upd) => mass::update_settings(&st.db, &ctx.me, id, upd).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |_| {
        (back.clone(), "Saved the mass settings.".to_string())
    })
}

// ---------------------------------------------------------------------------
// Material library
// ---------------------------------------------------------------------------

/// `GET /materials` – every signed-in user reads the library.
pub async fn materials(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let list = mass::materials(&st.db).await?;
    render(
        &st,
        &ctx,
        "materials.html",
        json!({
            "materials": list,
            "is_admin": ctx.me.require(Role::Admin).is_ok(),
        }),
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct MaterialForm {
    name: String,
    density: String,
    notes: String,
}

fn material_from(f: &MaterialForm) -> AppResult<MaterialInput> {
    Ok(MaterialInput {
        name: f.name.clone(),
        density_kg_m3: number("Density", &f.density)?
            .ok_or_else(|| AppError::BadRequest("Density must not be empty.".into()))?,
        notes: f.notes.clone(),
    })
}

/// `POST /materials` (administrators).
pub async fn create_material(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(f): Form<MaterialForm>,
) -> AppResult<Response> {
    let result = match material_from(&f) {
        Ok(input) => mass::create_material(&st.db, &ctx.me, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, "/materials", |m| {
        ("/materials".into(), format!("Added material {}.", m.name))
    })
}

/// `POST /materials/{id}` (administrators).
pub async fn update_material(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<MaterialForm>,
) -> AppResult<Response> {
    let result = match material_from(&f) {
        Ok(input) => mass::update_material(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, "/materials", |m| {
        ("/materials".into(), format!("Saved material {}.", m.name))
    })
}

/// `POST /materials/{id}/delete` (administrators).
pub async fn delete_material(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let result = mass::delete_material(&st.db, &ctx.me, id).await;
    finish(&st, result, "/materials", |m| {
        ("/materials".into(), format!("Removed material {}.", m.name))
    })
}
