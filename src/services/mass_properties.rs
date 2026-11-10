//! Mass properties of assemblies: the material library, the mass settings of
//! revisions and files, and the roll-up of mass, centre of gravity and
//! inertia tensor over the multi-level BOM (see `massprops` and
//! `docs/src/research/mass-properties.md`).
//!
//! * **Frames:** every geometry file is taken as exported in the common
//!   assembly / vehicle frame – no placements are applied. A file shows
//!   `instances` units of its item; a BOM line contributes
//!   `quantity / instances` times the file's body (copies at the same place
//!   when that is not 1, which is flagged).
//! * **Sources per revision**, first that applies: the measured mass (with
//!   optional CG and inertia; it covers the whole sub-tree), the sum of the
//!   BOM children, the geometry file × density (override, else the material
//!   library entry of the revision's material), the geometry file with the
//!   entered mass (uniform density), the entered mass alone (counts in the
//!   total mass but has no position), nothing (missing).
//! * **Computation:** the mesh integrals come from the isolated preview
//!   process and are cached per content hash (`Previews::mass_geometry`);
//!   this module only scales and adds them (`report` is pure).
//! * **Read rights:** the structure is loaded like the BOM page
//!   (`bom::load_structure`); restricted lines are left out and counted.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, RevisionRule, Role},
    error::{AppError, AppResult},
    massprops::{self, Body, Geometry, LengthUnit, Sym, V3},
    preview::PreviewFormat,
    services::{
        audit,
        bom::{self, BomNode, ResolvedRevision},
        mass_geometry::{self, GeometryInfo, Hints, Resolution},
        revisions,
    },
    util, AppState,
};

/// Highest density accepted (kg/m³; osmium is 22 590).
pub const MAX_DENSITY: f64 = 100_000.0;
/// Geometry files computed for one roll-up at most (each may need a
/// conversion in the preview process).
pub const MAX_GEOMETRY_FILES: usize = 2000;
/// Formats whose 3D preview can serve as geometry, in the order the
/// automatic choice prefers them (exported meshes first, then STEP, then the
/// lightweight representations stored in native CAD files).
pub const GEOMETRY_FORMATS: [&str; 14] = [
    "stl",
    "3mf",
    "obj",
    "ply",
    "off",
    "amf",
    "gltf",
    "glb",
    "3ds",
    "step",
    "nx-part",
    "solidworks-part",
    "jt",
    "rhino",
];
/// Entered and computed mass differing by more than this share are flagged.
const MASS_MISMATCH: f64 = 0.05;

// ---------------------------------------------------------------------------
// Material library
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Material {
    pub id: Uuid,
    pub name: String,
    pub density_kg_m3: f64,
    pub notes: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const MATERIAL_COLUMNS: &str = "id, name, density_kg_m3, notes, created_at, updated_at";

/// The material library, by name (every signed-in user may read it).
pub async fn materials(pool: &PgPool) -> AppResult<Vec<Material>> {
    Ok(sqlx::query_as::<_, Material>(&format!(
        "SELECT {MATERIAL_COLUMNS} FROM materials ORDER BY lower(name)"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn material(pool: &PgPool, id: Uuid) -> AppResult<Material> {
    sqlx::query_as::<_, Material>(&format!(
        "SELECT {MATERIAL_COLUMNS} FROM materials WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("Material not found."))
}

/// Validates a density in kg/m³.
pub fn validate_density(value: f64) -> AppResult<f64> {
    if value.is_finite() && value > 0.0 && value <= MAX_DENSITY {
        Ok(value)
    } else {
        Err(AppError::BadRequest(format!(
            "Density must be a number above 0 and at most {MAX_DENSITY} kg/m³."
        )))
    }
}

/// The editable fields of a material.
#[derive(Clone, Debug, Default)]
pub struct MaterialInput {
    pub name: String,
    pub density_kg_m3: f64,
    pub notes: String,
}

impl MaterialInput {
    fn validated(&self) -> AppResult<MaterialInput> {
        Ok(MaterialInput {
            name: domain::require_text("Name", &self.name, 200)?,
            density_kg_m3: validate_density(self.density_kg_m3)?,
            notes: domain::limit_text("Notes", &self.notes, 2000)?,
        })
    }
}

/// Adds a material to the library (administrators).
///
/// # Invariants
///
/// * **Permission:** admin role (403 otherwise); name 1–200 characters,
///   density above 0 and at most [`MAX_DENSITY`] (400); a name already in the
///   library (case-insensitive) is a 409.
/// * **Locks:** none beyond the unique name index.
/// * **Audit:** `material.create`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn create_material(
    pool: &PgPool,
    actor: &Actor,
    input: MaterialInput,
) -> AppResult<Material> {
    actor.require(Role::Admin)?;
    let input = input.validated()?;
    let mut tx = pool.begin().await?;
    let m = sqlx::query_as::<_, Material>(&format!(
        "INSERT INTO materials (id, name, density_kg_m3, notes, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING {MATERIAL_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&input.name)
    .bind(input.density_kg_m3)
    .bind(&input.notes)
    .bind(actor.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!("The material '{}' is already in the library.", input.name),
        )
    })?;
    audit::record(
        &mut tx,
        Some(actor),
        "material.create",
        "material",
        Some(m.id),
        format!("Added material {} ({} kg/m³)", m.name, m.density_kg_m3),
        json!({ "name": m.name, "density_kg_m3": m.density_kg_m3 }),
    )
    .await?;
    tx.commit().await?;
    Ok(m)
}

/// Changes name, density and notes of a material (administrators).
///
/// # Invariants
///
/// * **Permission:** admin role (403 otherwise); validation as
///   [`create_material`]; an unknown material is "not found".
/// * **Locks:** the material row (`FOR UPDATE`).
/// * **Audit:** `material.update` with the old and new density.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update_material(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: MaterialInput,
) -> AppResult<Material> {
    actor.require(Role::Admin)?;
    let input = input.validated()?;
    let mut tx = pool.begin().await?;
    let old = sqlx::query_as::<_, Material>(&format!(
        "SELECT {MATERIAL_COLUMNS} FROM materials WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Material not found."))?;
    let m = sqlx::query_as::<_, Material>(&format!(
        "UPDATE materials SET name = $2, density_kg_m3 = $3, notes = $4, updated_at = now()
         WHERE id = $1 RETURNING {MATERIAL_COLUMNS}"
    ))
    .bind(id)
    .bind(&input.name)
    .bind(input.density_kg_m3)
    .bind(&input.notes)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!("The material '{}' is already in the library.", input.name),
        )
    })?;
    audit::record(
        &mut tx,
        Some(actor),
        "material.update",
        "material",
        Some(id),
        format!("Updated material {}", m.name),
        json!({ "name": m.name, "old_name": old.name,
                "density_kg_m3": m.density_kg_m3, "old_density_kg_m3": old.density_kg_m3 }),
    )
    .await?;
    tx.commit().await?;
    Ok(m)
}

/// Removes a material from the library (administrators). Revisions keep
/// their material text; their mass is then reported as missing a density.
///
/// # Invariants
///
/// * **Permission:** admin role (403 otherwise); an unknown material is
///   "not found".
/// * **Locks:** the material row (`FOR UPDATE`).
/// * **Audit:** `material.delete`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn delete_material(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Material> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let m = sqlx::query_as::<_, Material>(&format!(
        "SELECT {MATERIAL_COLUMNS} FROM materials WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Material not found."))?;
    sqlx::query("DELETE FROM materials WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "material.delete",
        "material",
        Some(id),
        format!("Removed material {}", m.name),
        json!({ "name": m.name, "density_kg_m3": m.density_kg_m3 }),
    )
    .await?;
    tx.commit().await?;
    Ok(m)
}

// ---------------------------------------------------------------------------
// Mass settings of revisions and files
// ---------------------------------------------------------------------------

/// The stored mass settings of a revision (defaults when none are stored).
#[derive(Clone, Debug, Default, Serialize, sqlx::FromRow)]
pub struct MassSettings {
    pub revision_id: Uuid,
    pub density_kg_m3: Option<f64>,
    pub geometry_file_id: Option<Uuid>,
    pub measured_mass_kg: Option<f64>,
    pub measured_cg_m: Option<Vec<f64>>,
    pub measured_inertia_kg_m2: Option<Vec<f64>>,
    pub measured_note: String,
}

const SETTINGS_COLUMNS: &str = "revision_id, density_kg_m3, geometry_file_id, measured_mass_kg,
    measured_cg_m, measured_inertia_kg_m2, measured_note";

/// A file of a revision that can serve as geometry, with its settings.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct GeometryFile {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub filename: String,
    pub sha256: String,
    pub format: String,
    /// Stored unit; `None` = the format's default (see [`GeometryFile::unit`]).
    pub length_unit: Option<String>,
    pub instances: i32,
}

impl GeometryFile {
    pub fn unit(&self) -> LengthUnit {
        self.length_unit
            .as_deref()
            .and_then(LengthUnit::parse)
            .unwrap_or_else(|| LengthUnit::default_for(&self.format))
    }
}

async fn settings_of(pool: &PgPool, ids: &[Uuid]) -> AppResult<HashMap<Uuid, MassSettings>> {
    let rows = sqlx::query_as::<_, MassSettings>(&format!(
        "SELECT {SETTINGS_COLUMNS} FROM revision_mass WHERE revision_id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|s| (s.revision_id, s)).collect())
}

async fn geometry_files_of(pool: &PgPool, ids: &[Uuid]) -> AppResult<Vec<GeometryFile>> {
    Ok(sqlx::query_as::<_, GeometryFile>(
        "SELECT f.id, f.revision_id, f.filename, f.sha256, f.format, fm.length_unit,
                COALESCE(fm.instances, 1) AS instances
         FROM files f LEFT JOIN file_mass_settings fm ON fm.file_id = f.id
         WHERE f.revision_id = ANY($1) AND f.format = ANY($2)
         ORDER BY f.filename",
    )
    .bind(ids)
    .bind(GEOMETRY_FORMATS.to_vec())
    .fetch_all(pool)
    .await?)
}

/// The files of a revision as `(id, name)` with the file names their
/// current versions reference, for the root of an assembly.
async fn reference_roots_of(pool: &PgPool, revision_id: Uuid) -> AppResult<HashSet<Uuid>> {
    let files: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, filename FROM files WHERE revision_id = $1")
            .bind(revision_id)
            .fetch_all(pool)
            .await?;
    let references: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT f.id, fr.name
         FROM files f
         JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
         JOIN file_references fr ON fr.file_version_id = fv.id
         WHERE f.revision_id = $1",
    )
    .bind(revision_id)
    .fetch_all(pool)
    .await?;
    Ok(mass_geometry::reference_roots(&files, &references))
}

/// The geometry file of a revision (see [`mass_geometry`]): the chosen one;
/// else the best file of the only model; else *ambiguous*. The root of the
/// file references is looked up only when the files are ambiguous.
async fn resolve_geometry<'a>(
    pool: &PgPool,
    files: &'a [GeometryFile],
    chosen: Option<Uuid>,
    preferred: &str,
    number: &str,
    name: &str,
) -> AppResult<Resolution<'a>> {
    let hints = Hints {
        item_number: number,
        item_name: name,
        roots: None,
    };
    let first = mass_geometry::resolve(files, chosen, preferred, &hints);
    if !first.is_ambiguous() {
        return Ok(first);
    }
    let roots = reference_roots_of(pool, files[0].revision_id).await?;
    let hints = Hints {
        roots: Some(&roots),
        ..hints
    };
    Ok(mass_geometry::resolve(files, chosen, preferred, &hints))
}

/// The mass settings of a revision the actor may read, its candidate
/// geometry files and the file that is used.
#[derive(Clone, Debug, Serialize)]
pub struct RevisionMass {
    pub settings: MassSettings,
    pub files: Vec<GeometryFile>,
    /// The file the mass is computed from (`None` when ambiguous).
    pub geometry_file_id: Option<Uuid>,
    /// How it was decided, with the candidates by model.
    pub geometry: GeometryInfo,
    /// The library entry of the revision's material, if any.
    pub material: Option<Material>,
}

pub async fn revision_settings(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<RevisionMass> {
    let rev = revisions::get(pool, actor, revision_id).await?;
    let settings = settings_of(pool, &[rev.id])
        .await?
        .remove(&rev.id)
        .unwrap_or(MassSettings {
            revision_id: rev.id,
            ..MassSettings::default()
        });
    let files = geometry_files_of(pool, &[rev.id]).await?;
    let resolution = resolve_geometry(
        pool,
        &files,
        settings.geometry_file_id,
        &actor.preferred_cad,
        &rev.item_number,
        &rev.name,
    )
    .await?;
    let geometry_file_id = resolution.file().map(|f| f.id);
    let geometry = mass_geometry::info(&resolution, true);
    let material = material_named(pool, &rev.material).await?;
    Ok(RevisionMass {
        settings,
        files,
        geometry_file_id,
        geometry,
        material,
    })
}

async fn material_named(pool: &PgPool, name: &str) -> AppResult<Option<Material>> {
    if name.trim().is_empty() {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, Material>(&format!(
        "SELECT {MATERIAL_COLUMNS} FROM materials WHERE lower(name) = lower($1)"
    ))
    .bind(name.trim())
    .fetch_optional(pool)
    .await?)
}

/// New mass settings of a revision (and of its geometry file).
#[derive(Clone, Debug, Default)]
pub struct MassSettingsUpdate {
    pub density_kg_m3: Option<f64>,
    /// `None` = pick automatically.
    pub geometry_file_id: Option<Uuid>,
    /// Unit of the geometry file; `None` = the format's default.
    pub length_unit: Option<String>,
    /// Units of the item the geometry file shows (1 if not given).
    pub instances: Option<i32>,
    pub measured_mass_kg: Option<f64>,
    pub measured_cg_m: Option<[f64; 3]>,
    pub measured_inertia_kg_m2: Option<[f64; 6]>,
    pub measured_note: String,
}

impl MassSettingsUpdate {
    fn validated(self) -> AppResult<MassSettingsUpdate> {
        let density = self.density_kg_m3.map(validate_density).transpose()?;
        let mass = domain::validate_non_negative("Measured mass", self.measured_mass_kg)?;
        if mass.is_some_and(|m| m > 1e9) {
            return Err(AppError::BadRequest(
                "Measured mass must be at most 10⁹ kg.".into(),
            ));
        }
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite() && x.abs() <= 1e9);
        if let Some(cg) = &self.measured_cg_m {
            if mass.is_none() {
                return Err(AppError::BadRequest(
                    "A measured centre of gravity needs a measured mass.".into(),
                ));
            }
            if !finite(cg) {
                return Err(AppError::BadRequest(
                    "The centre of gravity must be three finite numbers (m).".into(),
                ));
            }
        }
        if let Some(inertia) = &self.measured_inertia_kg_m2 {
            if self.measured_cg_m.is_none() {
                return Err(AppError::BadRequest(
                    "A measured inertia tensor needs the measured centre of gravity it is taken about."
                        .into(),
                ));
            }
            if !finite(inertia) || inertia[0] < 0.0 || inertia[1] < 0.0 || inertia[2] < 0.0 {
                return Err(AppError::BadRequest(
                    "The inertia tensor must be six finite numbers with non-negative Ixx, Iyy, Izz (kg·m²)."
                        .into(),
                ));
            }
            // the moments of a real body satisfy the triangle inequality
            let [a, b, c] = [inertia[0], inertia[1], inertia[2]];
            let slack = 1e-9 * (a + b + c);
            if a > b + c + slack || b > a + c + slack || c > a + b + slack {
                return Err(AppError::BadRequest(
                    "These moments of inertia are impossible for a real body (each must be at most the sum of the other two)."
                        .into(),
                ));
            }
        }
        let unit = match self.length_unit.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(u) => Some(
                LengthUnit::parse(u)
                    .ok_or_else(|| AppError::BadRequest("Length unit must be mm, m or in.".into()))?
                    .as_str()
                    .to_string(),
            ),
        };
        let instances = self.instances.unwrap_or(1);
        if !(1..=100_000).contains(&instances) {
            return Err(AppError::BadRequest(
                "Instances must be a whole number from 1 to 100000.".into(),
            ));
        }
        Ok(MassSettingsUpdate {
            density_kg_m3: density,
            geometry_file_id: self.geometry_file_id,
            length_unit: unit,
            instances: Some(instances),
            measured_mass_kg: mass,
            measured_cg_m: self.measured_cg_m,
            measured_inertia_kg_m2: self.measured_inertia_kg_m2,
            measured_note: domain::limit_text("Note", &self.measured_note, 2000)?,
        })
    }
}

/// Stores the mass settings of a revision: density override, geometry file
/// with its unit and instance count, measured values.
///
/// # Invariants
///
/// * **Permission:** the revision is visible ("not found" otherwise) and
///   editable by the actor (`revisions::ensure_editable`: in work, checked
///   out by the actor). The geometry file must be a 3D file of this revision
///   (400). Values are validated (400).
/// * **Locks:** the revision row (`revisions::lock`).
/// * **Audit:** `revision.mass` with the new values.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update_settings(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    upd: MassSettingsUpdate,
) -> AppResult<RevisionMass> {
    let upd = upd.validated()?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    if !actor.can_see_project(rev.project_id) {
        return Err(AppError::not_found("Revision not found."));
    }
    revisions::ensure_editable(&rev, actor)?;
    // the file whose unit and instance count are set: the chosen one, else
    // the one picked automatically
    let candidates = sqlx::query_as::<_, GeometryFile>(
        "SELECT f.id, f.revision_id, f.filename, f.sha256, f.format, fm.length_unit,
                COALESCE(fm.instances, 1) AS instances
         FROM files f LEFT JOIN file_mass_settings fm ON fm.file_id = f.id
         WHERE f.revision_id = $1 AND f.format = ANY($2)
         ORDER BY f.filename
         FOR UPDATE OF f",
    )
    .bind(rev.id)
    .bind(GEOMETRY_FORMATS.to_vec())
    .fetch_all(&mut *tx)
    .await?;
    if let Some(id) = upd.geometry_file_id {
        if !candidates.iter().any(|f| f.id == id) {
            return Err(AppError::BadRequest(
                "The geometry file must be a 3D file of this revision.".into(),
            ));
        }
    }
    // an ambiguous revision has no file whose unit could be set yet
    let file = mass_geometry::resolve(
        &candidates,
        upd.geometry_file_id,
        &actor.preferred_cad,
        &Hints::default(),
    )
    .file()
    .map(|f| (f.id, f.filename.clone()));
    sqlx::query(
        "INSERT INTO revision_mass (revision_id, density_kg_m3, geometry_file_id, measured_mass_kg,
                                    measured_cg_m, measured_inertia_kg_m2, measured_note,
                                    updated_by, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
         ON CONFLICT (revision_id) DO UPDATE SET
             density_kg_m3 = EXCLUDED.density_kg_m3, geometry_file_id = EXCLUDED.geometry_file_id,
             measured_mass_kg = EXCLUDED.measured_mass_kg, measured_cg_m = EXCLUDED.measured_cg_m,
             measured_inertia_kg_m2 = EXCLUDED.measured_inertia_kg_m2,
             measured_note = EXCLUDED.measured_note, updated_by = EXCLUDED.updated_by,
             updated_at = now()",
    )
    .bind(rev.id)
    .bind(upd.density_kg_m3)
    .bind(upd.geometry_file_id)
    .bind(upd.measured_mass_kg)
    .bind(upd.measured_cg_m.map(|v| v.to_vec()))
    .bind(upd.measured_inertia_kg_m2.map(|v| v.to_vec()))
    .bind(&upd.measured_note)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    if let Some((file_id, _)) = &file {
        sqlx::query(
            "INSERT INTO file_mass_settings (file_id, length_unit, instances) VALUES ($1, $2, $3)
             ON CONFLICT (file_id) DO UPDATE SET length_unit = EXCLUDED.length_unit,
                                                 instances = EXCLUDED.instances",
        )
        .bind(file_id)
        .bind(&upd.length_unit)
        .bind(upd.instances.unwrap_or(1))
        .execute(&mut *tx)
        .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "revision.mass",
        "revision",
        Some(rev.id),
        format!("Updated the mass settings of {}", revisions::label(&rev)),
        json!({
            "item_id": rev.item_id,
            "density_kg_m3": upd.density_kg_m3,
            "geometry_file": file.as_ref().map(|f| &f.1),
            "length_unit": upd.length_unit,
            "instances": upd.instances,
            "measured_mass_kg": upd.measured_mass_kg,
            "measured_cg_m": upd.measured_cg_m,
            "measured_inertia_kg_m2": upd.measured_inertia_kg_m2,
        }),
    )
    .await?;
    tx.commit().await?;
    revision_settings(pool, actor, revision_id).await
}

/// Chooses the geometry file of a revision (`None` = back to automatic),
/// keeping every other mass setting, like the one-click choice on the page.
/// Same rules as [`update_settings`].
pub async fn choose_geometry(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    file_id: Option<Uuid>,
) -> AppResult<RevisionMass> {
    let current = revision_settings(pool, actor, revision_id).await?;
    let s = &current.settings;
    let chosen = file_id.and_then(|id| current.files.iter().find(|f| f.id == id));
    if file_id.is_some() && chosen.is_none() {
        return Err(AppError::BadRequest(
            "The geometry file must be a 3D file of this revision.".into(),
        ));
    }
    // the file keeps the unit and instance count stored for it (for
    // automatic: those of the file that is picked automatically)
    let file = chosen.or_else(|| {
        let auto = current.geometry.choice == "automatic";
        current
            .files
            .iter()
            .find(|f| auto && Some(f.id) == current.geometry_file_id)
    });
    update_settings(
        pool,
        actor,
        revision_id,
        MassSettingsUpdate {
            density_kg_m3: s.density_kg_m3,
            geometry_file_id: file_id,
            length_unit: file.and_then(|f| f.length_unit.clone()),
            instances: file.map(|f| f.instances),
            measured_mass_kg: s.measured_mass_kg,
            measured_cg_m: s.measured_cg_m.as_deref().and_then(v3),
            measured_inertia_kg_m2: s.measured_inertia_kg_m2.as_deref().and_then(sym),
            measured_note: s.measured_note.clone(),
        },
    )
    .await
}

/// Copies the mass settings of `from` to the new revision `to` (*revise*):
/// the geometry file and the file settings are matched by file name.
/// Returns whether settings were copied.
pub(crate) async fn copy_to_revision(
    conn: &mut PgConnection,
    from: Uuid,
    to: Uuid,
) -> AppResult<bool> {
    let copied = sqlx::query(
        "INSERT INTO revision_mass (revision_id, density_kg_m3, geometry_file_id, measured_mass_kg,
                                    measured_cg_m, measured_inertia_kg_m2, measured_note,
                                    updated_by, updated_at)
         SELECT $2, m.density_kg_m3, nf.id, m.measured_mass_kg, m.measured_cg_m,
                m.measured_inertia_kg_m2, m.measured_note, m.updated_by, m.updated_at
         FROM revision_mass m
         LEFT JOIN files ofl ON ofl.id = m.geometry_file_id
         LEFT JOIN LATERAL (
             SELECT f.id FROM files f WHERE f.revision_id = $2 AND f.filename = ofl.filename
             ORDER BY f.id LIMIT 1
         ) nf ON true
         WHERE m.revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    sqlx::query(
        "INSERT INTO file_mass_settings (file_id, length_unit, instances)
         SELECT nf.id, s.length_unit, s.instances
         FROM file_mass_settings s
         JOIN files ofl ON ofl.id = s.file_id AND ofl.revision_id = $1
         JOIN files nf ON nf.revision_id = $2 AND nf.filename = ofl.filename
         ON CONFLICT (file_id) DO NOTHING",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?;
    Ok(copied > 0)
}

// ---------------------------------------------------------------------------
// Roll-up
// ---------------------------------------------------------------------------

/// Where the numbers of a line come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Measured mass (with or without CG / inertia).
    Measured,
    /// Geometry × density.
    Computed,
    /// Geometry with the entered mass spread uniformly.
    EnteredOnGeometry,
    /// The entered mass only: counts in the mass, has no position.
    Entered,
    /// The sum of the BOM children.
    Assembly,
    /// Covered by the measured mass of an assembly above.
    Covered,
    /// No mass at all.
    Missing,
    /// Files of several different parts and none chosen: nothing is added.
    GeometryAmbiguous,
    /// A line the reader may not see.
    Restricted,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Measured => "measured",
            Source::Computed => "computed",
            Source::EnteredOnGeometry => "entered mass on geometry",
            Source::Entered => "entered mass (no position)",
            Source::Assembly => "sum of children",
            Source::Covered => "covered by measured parent",
            Source::Missing => "missing",
            Source::GeometryAmbiguous => "geometry ambiguous",
            Source::Restricted => "restricted",
        }
    }
}

/// What the roll-up knows about one revision.
#[derive(Clone, Debug, Default)]
pub(crate) struct RevisionInput {
    pub material: String,
    pub entered_mass_kg: Option<f64>,
    pub settings: MassSettings,
    /// The geometry file and its integrals (or why there are none).
    pub geometry: Option<(GeometryFile, Result<Geometry, String>)>,
    /// How the geometry file was decided (set for parts that need one).
    pub geometry_info: Option<GeometryInfo>,
    /// Density and where it comes from (`override`, the material name).
    pub density: Option<(f64, String)>,
}

/// One unit of a revision, rolled up.
#[derive(Clone, Debug, Default)]
struct Unit {
    /// Placed mass (with CG and inertia).
    placed: Body,
    /// Mass without position (entered masses, measured without CG).
    unplaced: f64,
    /// Lines below without any mass.
    missing: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct MassLine {
    pub level: usize,
    pub find_number: i32,
    pub item_number: String,
    pub revision_id: Option<Uuid>,
    pub revision_code: Option<String>,
    pub name: String,
    pub quantity: f64,
    /// `quantity / instances`: copies of the source body this line adds.
    pub factor: f64,
    pub source: Source,
    pub source_label: &'static str,
    /// File name, density and material, or the measurement note.
    pub detail: String,
    pub unit_mass_kg: Option<f64>,
    pub total_mass_kg: Option<f64>,
    /// CG of the placed mass of this line (m).
    pub cg_m: Option<V3>,
    pub warnings: Vec<String>,
    /// The geometry file and how it was chosen; the candidates when ambiguous.
    pub geometry: Option<GeometryInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MassReport {
    pub rule: RevisionRule,
    pub root_revision_id: Uuid,
    pub root_label: String,
    pub root_name: String,
    /// Source of the root itself (measured, assembly, computed …).
    pub root_source: Source,
    pub root_detail: String,
    /// The geometry file of the root and how it was chosen (a part).
    pub root_geometry: Option<GeometryInfo>,
    /// The point the second inertia tensor is taken about (m).
    pub reference_m: V3,
    /// Placed plus unplaced mass.
    pub mass_kg: f64,
    pub placed_mass_kg: f64,
    pub unplaced_mass_kg: f64,
    /// CG of the placed mass (m); `None` without placed mass.
    pub cg_m: Option<V3>,
    /// About the CG, tensor form `[xx, yy, zz, xy, yz, xz]` (kg·m²).
    pub inertia_cg: Option<Sym>,
    /// About [`MassReport::reference_m`].
    pub inertia_ref: Option<Sym>,
    /// Principal moments about the CG, largest first.
    pub principal_moments: Option<V3>,
    pub lines: Vec<MassLine>,
    pub line_count: usize,
    pub restricted_lines: usize,
    pub missing_lines: usize,
    /// Every line has a placed mass, nothing restricted, no warnings.
    pub complete: bool,
    /// Warnings about the whole result.
    pub warnings: Vec<String>,
    /// Lines with warnings.
    pub lines_with_warnings: usize,
}

/// Builds the report from the expanded tree and what is known of every
/// revision (pure).
pub(crate) fn report(
    tree: &bom::BomTree,
    root_label: String,
    root_name: String,
    root_has_children: bool,
    inputs: &HashMap<Uuid, RevisionInput>,
    reference: V3,
) -> MassReport {
    let mut lines = Vec::new();
    let empty = RevisionInput::default();
    let root_input = inputs.get(&tree.root.id).unwrap_or(&empty);
    let mut walker = Walker {
        inputs,
        lines: &mut lines,
        restricted: 0,
    };
    let (unit, source, detail, root_warnings) =
        walker.revision(root_input, &tree.children, root_has_children, false);
    let restricted = walker.restricted;
    let total = unit.placed;
    let has_placed = total.mass > 0.0 && total.is_finite();
    let missing_lines = lines
        .iter()
        .filter(|l| matches!(l.source, Source::Missing | Source::GeometryAmbiguous))
        .count();
    let lines_with_warnings = lines.iter().filter(|l| !l.warnings.is_empty()).count();
    let mut warnings = root_warnings;
    if restricted > 0 {
        warnings.push(format!(
            "{restricted} restricted line{} (items of project groups you are not a member of) left out.",
            if restricted == 1 { "" } else { "s" }
        ));
    }
    if missing_lines > 0 || unit.missing > 0 {
        let n = missing_lines.max(unit.missing);
        warnings.push(format!(
            "{n} line{} without any mass.",
            if n == 1 { "" } else { "s" }
        ));
    }
    if unit.unplaced > 0.0 {
        warnings.push(format!(
            "{} kg have no position (entered or measured mass without CG): counted in the mass, not in the CG and inertia.",
            util::format_number(unit.unplaced, 4)
        ));
    }
    if lines_with_warnings > 0 {
        warnings.push(format!(
            "{lines_with_warnings} line{} with warnings (see below).",
            if lines_with_warnings == 1 { "" } else { "s" }
        ));
    }
    let complete = warnings.is_empty();
    MassReport {
        rule: tree.rule,
        root_revision_id: tree.root.id,
        root_label,
        root_name,
        root_source: source,
        root_detail: detail,
        root_geometry: root_input.geometry_info.clone(),
        reference_m: reference,
        mass_kg: total.mass + unit.unplaced,
        placed_mass_kg: total.mass,
        unplaced_mass_kg: unit.unplaced,
        cg_m: has_placed.then_some(total.cg),
        inertia_cg: has_placed.then_some(total.inertia),
        inertia_ref: has_placed.then(|| total.inertia_about(reference)),
        principal_moments: has_placed.then(|| massprops::principal_moments(total.inertia)),
        line_count: lines.len(),
        lines,
        restricted_lines: restricted,
        missing_lines,
        complete,
        warnings,
        lines_with_warnings,
    }
}

struct Walker<'a> {
    inputs: &'a HashMap<Uuid, RevisionInput>,
    lines: &'a mut Vec<MassLine>,
    restricted: usize,
}

impl Walker<'_> {
    /// One unit of `rev` with its BOM `children`; pushes the children's
    /// lines. Returns the unit, its source, a detail text and warnings.
    fn revision(
        &mut self,
        input: &RevisionInput,
        children: &[BomNode],
        has_children: bool,
        covered: bool,
    ) -> (Unit, Source, String, Vec<String>) {
        let mut warnings = Vec::new();
        let s = &input.settings;
        // 1. measured: wins, covers the sub-tree
        if let Some(mass) = s.measured_mass_kg {
            self.children(children, true);
            let note = if s.measured_note.is_empty() {
                String::new()
            } else {
                s.measured_note.clone()
            };
            let cg = s.measured_cg_m.as_deref().and_then(v3);
            let unit = match cg {
                Some(cg) => {
                    let inertia = s.measured_inertia_kg_m2.as_deref().and_then(sym);
                    if inertia.is_none() && mass > 0.0 {
                        warnings.push(
                            "measured without inertia tensor: taken as a point mass at the CG"
                                .into(),
                        );
                    }
                    Unit {
                        placed: Body {
                            mass,
                            cg,
                            inertia: inertia.unwrap_or([0.0; 6]),
                        },
                        unplaced: 0.0,
                        missing: 0,
                    }
                }
                None => {
                    if mass > 0.0 {
                        warnings.push("measured without CG: the mass has no position".into());
                    }
                    Unit {
                        placed: Body::default(),
                        unplaced: mass,
                        missing: 0,
                    }
                }
            };
            return (unit, Source::Measured, note, warnings);
        }
        // 2. an assembly: the sum of its children
        if has_children {
            // its own geometry and entered mass would count the children twice
            let unit = self.children(children, covered);
            return (unit, Source::Assembly, String::new(), warnings);
        }
        // 3. geometry
        let entered = input.entered_mass_kg;
        let ambiguous = input.geometry_info.as_ref().filter(|g| g.is_ambiguous());
        if let Some(info) = ambiguous {
            // files of several parts: no guess; an entered mass still counts
            warnings.push(info.ambiguity_warning());
            if entered.is_none() {
                return (
                    Unit {
                        missing: 1,
                        ..Unit::default()
                    },
                    Source::GeometryAmbiguous,
                    info.note(),
                    warnings,
                );
            }
        }
        let geometry_note = input
            .geometry_info
            .as_ref()
            .map(GeometryInfo::note)
            .filter(|n| !n.is_empty() && ambiguous.is_none())
            .map(|n| format!(" – {n}"))
            .unwrap_or_default();
        if let Some((file, geometry)) = &input.geometry {
            let unit_label = file.unit().as_str();
            match geometry {
                Ok(g) => {
                    warnings.extend(g.warnings());
                    let metres = file.unit().metres();
                    let instances = f64::from(file.instances.max(1));
                    if let Some((density, from)) = &input.density {
                        // the file shows `instances` units: one unit is 1/instances of it
                        let body = g.body(*density, metres).times(1.0 / instances);
                        if let Some(entered) = entered.filter(|m| *m > 0.0) {
                            if (entered - body.mass).abs() > MASS_MISMATCH * body.mass {
                                warnings.push(format!(
                                    "the entered mass {} kg differs from the computed {} kg",
                                    util::format_number(entered, 4),
                                    util::format_number(body.mass, 4)
                                ));
                            }
                        }
                        let detail = format!(
                            "{} ({unit_label}) × {} kg/m³ ({from}){geometry_note}",
                            file.filename,
                            util::format_number(*density, 3)
                        );
                        return (
                            Unit {
                                placed: body,
                                unplaced: 0.0,
                                missing: 0,
                            },
                            Source::Computed,
                            detail,
                            warnings,
                        );
                    }
                    if let Some(mass) = entered.filter(|m| *m > 0.0) {
                        let body = g.body(1.0, metres).times(1.0 / instances);
                        let scaled = if body.mass > 0.0 {
                            body.times(mass / body.mass)
                        } else {
                            body
                        };
                        let detail = format!(
                            "{} ({unit_label}), entered mass spread uniformly{geometry_note}",
                            file.filename
                        );
                        warnings.push(no_density(&input.material));
                        return (
                            Unit {
                                placed: scaled,
                                unplaced: 0.0,
                                missing: 0,
                            },
                            Source::EnteredOnGeometry,
                            detail,
                            warnings,
                        );
                    }
                    warnings.push(no_density(&input.material));
                }
                Err(e) => warnings.push(format!("{}: {e}", file.filename)),
            }
        }
        // 4. the entered mass alone
        if let Some(mass) = entered {
            if input.geometry.is_none() && mass > 0.0 {
                warnings.push(if ambiguous.is_some() {
                    "no geometry file chosen: the entered mass has no position".into()
                } else {
                    "no 3D file: the entered mass has no position".to_string()
                });
            }
            return (
                Unit {
                    placed: Body::default(),
                    unplaced: mass,
                    missing: 0,
                },
                Source::Entered,
                String::new(),
                warnings,
            );
        }
        if input.geometry.is_none() {
            warnings.push("no mass: no 3D file, no measured or entered mass".into());
        }
        (
            Unit {
                missing: 1,
                ..Unit::default()
            },
            Source::Missing,
            String::new(),
            warnings,
        )
    }

    /// The lines below one revision; returns the sum of one unit of it.
    fn children(&mut self, nodes: &[BomNode], covered: bool) -> Unit {
        let empty = RevisionInput::default();
        let mut bodies = Vec::new();
        let mut unplaced = 0.0;
        let mut missing = 0;
        for n in nodes {
            let base = MassLine {
                level: n.level,
                find_number: n.find_number,
                item_number: n.item_number.clone(),
                revision_id: n.revision.as_ref().map(|r| r.id),
                revision_code: n.revision.as_ref().map(|r| r.code.clone()),
                name: n
                    .revision
                    .as_ref()
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
                quantity: n.quantity,
                factor: n.quantity,
                source: Source::Missing,
                source_label: Source::Missing.label(),
                detail: String::new(),
                unit_mass_kg: None,
                total_mass_kg: None,
                cg_m: None,
                warnings: Vec::new(),
                geometry: None,
            };
            if n.restricted {
                self.restricted += 1;
                self.lines.push(MassLine {
                    source: Source::Restricted,
                    source_label: Source::Restricted.label(),
                    ..base
                });
                continue;
            }
            let at = self.lines.len();
            self.lines.push(base);
            let Some(rev) = &n.revision else {
                if let Some(line) = self.lines.get_mut(at) {
                    line.warnings
                        .push("no revision matches the revision rule".into());
                }
                if !covered {
                    missing += 1;
                }
                continue;
            };
            if n.cycle {
                if let Some(line) = self.lines.get_mut(at) {
                    line.warnings.push("the structure loops back here".into());
                }
                if !covered {
                    missing += 1;
                }
                continue;
            }
            let input = self.inputs.get(&rev.id).unwrap_or(&empty);
            let has_children = !n.children.is_empty();
            let (unit, source, detail, mut warnings) =
                self.revision(input, &n.children, has_children, covered);
            if covered {
                if let Some(line) = self.lines.get_mut(at) {
                    line.source = Source::Covered;
                    line.source_label = Source::Covered.label();
                    line.detail = detail;
                }
                continue;
            }
            let instances = match (&input.geometry, source) {
                (Some((f, _)), Source::Computed | Source::EnteredOnGeometry) => {
                    f64::from(f.instances.max(1))
                }
                _ => 1.0,
            };
            // copies of what the source shows (a file with all instances,
            // a measured or summed unit)
            let copies = n.quantity / instances;
            if unit.placed.mass > 0.0 && (copies - 1.0).abs() > 1e-9 {
                warnings.push(format!(
                    "quantity {} with geometry of {} unit{}: {} copies stacked at the same place – export every instance into the file and set its instance count, or check the CG",
                    util::format_number(n.quantity, 6),
                    util::format_number(instances, 0),
                    if instances == 1.0 { "" } else { "s" },
                    util::format_number(copies, 6)
                ));
            }
            let line_body = unit.placed.times(n.quantity);
            let unit_mass = unit.placed.mass + unit.unplaced;
            let factor = n.quantity;
            if let Some(line) = self.lines.get_mut(at) {
                line.factor = copies;
                line.source = source;
                line.source_label = source.label();
                line.detail = detail;
                let massless = matches!(source, Source::Missing | Source::GeometryAmbiguous);
                line.geometry = input.geometry_info.clone();
                line.unit_mass_kg = (!massless).then_some(unit_mass);
                line.total_mass_kg = (!massless).then_some(unit_mass * factor);
                line.cg_m = (line_body.mass > 0.0).then_some(line_body.cg);
                line.warnings = warnings;
            }
            if line_body.mass > 0.0 {
                bodies.push(line_body);
            }
            unplaced += unit.unplaced * factor;
            missing += unit.missing;
        }
        Unit {
            placed: massprops::combine(bodies.iter()),
            unplaced,
            missing,
        }
    }
}

fn no_density(material: &str) -> String {
    if material.trim().is_empty() {
        "no density: no material and no density override".into()
    } else {
        format!("no density: the material '{material}' is not in the material library")
    }
}

fn v3(v: &[f64]) -> Option<V3> {
    match v {
        &[x, y, z] => Some([x, y, z]),
        _ => None,
    }
}

fn sym(v: &[f64]) -> Option<Sym> {
    match v {
        &[a, b, c, d, e, f] => Some([a, b, c, d, e, f]),
        _ => None,
    }
}

/// The mass properties of the structure below a revision the actor may
/// read, under the revision rule; the inertia is also given about
/// `reference` (m). Geometry is computed in the preview process (cached).
pub async fn rollup(
    st: &AppState,
    actor: &Actor,
    root_id: Uuid,
    rule: RevisionRule,
    reference: V3,
) -> AppResult<MassReport> {
    if !reference.iter().all(|v| v.is_finite() && v.abs() <= 1e9) {
        return Err(AppError::BadRequest(
            "The reference point must be three finite numbers (m).".into(),
        ));
    }
    let pool = &st.db;
    let root = revisions::get(pool, actor, root_id).await?;
    let (lines_by_rev, _) = bom::load_structure(pool, actor, root.id, rule).await?;
    let tree = bom::assemble(
        rule,
        ResolvedRevision::from(&root),
        root.item_number.clone(),
        &lines_by_rev,
    )?;
    let mut ids: Vec<Uuid> = bom::tree_revision_ids(&tree);
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    let has_children: HashSet<Uuid> = lines_by_rev
        .iter()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(id, _)| *id)
        .collect();

    let settings = settings_of(pool, &ids).await?;
    let base: Vec<(Uuid, String, Option<f64>, String, String)> = sqlx::query_as(
        "SELECT r.id, r.material, r.mass_kg, i.number, r.name
         FROM revisions r JOIN items i ON i.id = r.item_id WHERE r.id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let library: HashMap<String, Material> = materials(pool)
        .await?
        .into_iter()
        .map(|m| (m.name.to_lowercase(), m))
        .collect();
    let mut files: HashMap<Uuid, Vec<GeometryFile>> = HashMap::new();
    for f in geometry_files_of(pool, &ids).await? {
        files.entry(f.revision_id).or_default().push(f);
    }

    let mut inputs: HashMap<Uuid, RevisionInput> = HashMap::new();
    let mut cache: HashMap<String, Result<Geometry, String>> = HashMap::new();
    for (id, material, entered, number, name) in base {
        let settings = settings.get(&id).cloned().unwrap_or(MassSettings {
            revision_id: id,
            ..MassSettings::default()
        });
        let density = match settings.density_kg_m3 {
            Some(d) => Some((d, "override".to_string())),
            None => library
                .get(&material.trim().to_lowercase())
                .map(|m| (m.density_kg_m3, m.name.clone())),
        };
        // geometry only where it is used: parts without a measured mass
        let needs_geometry = settings.measured_mass_kg.is_none() && !has_children.contains(&id);
        let mut geometry_info = None;
        let geometry = if needs_geometry {
            let resolution = match files.get(&id).filter(|f| !f.is_empty()) {
                Some(f) => Some(
                    resolve_geometry(
                        pool,
                        f,
                        settings.geometry_file_id,
                        &actor.preferred_cad,
                        &number,
                        &name,
                    )
                    .await?,
                ),
                None => None,
            };
            geometry_info = resolution.as_ref().map(|r| mass_geometry::info(r, false));
            match resolution.as_ref().and_then(Resolution::file) {
                Some(file) => {
                    let result = match cache.get(&file.sha256) {
                        Some(r) => r.clone(),
                        None => {
                            let r = if cache.len() >= MAX_GEOMETRY_FILES {
                                Err(format!(
                                    "more than {MAX_GEOMETRY_FILES} geometry files in one roll-up"
                                ))
                            } else {
                                geometry_of(st, file).await
                            };
                            cache.insert(file.sha256.clone(), r.clone());
                            r
                        }
                    };
                    Some((file.clone(), result))
                }
                None => None,
            }
        } else {
            None
        };
        inputs.insert(
            id,
            RevisionInput {
                material,
                entered_mass_kg: entered,
                settings,
                geometry,
                geometry_info,
                density,
            },
        );
    }
    Ok(report(
        &tree,
        revisions::label(&root),
        root.name.clone(),
        has_children.contains(&root.id),
        &inputs,
        reference,
    ))
}

/// The integrals of a geometry file (preview process, cached per content).
async fn geometry_of(st: &AppState, file: &GeometryFile) -> Result<Geometry, String> {
    let Some(format) = PreviewFormat::of_format(&file.format) else {
        return Err(format!("no 3D reader for the format '{}'", file.format));
    };
    st.previews
        .mass_geometry(&st.vault, &file.sha256, format)
        .await
        .map_err(|e| e.public_message())
}

/// Parses `x,y,z` (m); empty = the origin.
pub fn parse_point(raw: Option<&str>) -> AppResult<V3> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok([0.0; 3]);
    }
    let parts: Vec<&str> = raw
        .split([',', ';', ' '])
        .filter(|s| !s.is_empty())
        .collect();
    let bad = || AppError::BadRequest("The reference point must be x,y,z in metres.".into());
    let values: Vec<f64> = parts
        .iter()
        .map(|p| p.parse::<f64>().map_err(|_| bad()))
        .collect::<AppResult<_>>()?;
    match values.as_slice() {
        &[x, y, z] if values.iter().all(|v| v.is_finite() && v.abs() <= 1e9) => Ok([x, y, z]),
        _ => Err(bad()),
    }
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

fn num(v: f64) -> String {
    util::format_number(v, 9)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The report as CSV: one row per line, the totals first as comment-free
/// rows with `level` 0 (`TOTAL`, `INERTIA_CG`, `INERTIA_REF`).
pub fn to_csv(r: &MassReport) -> String {
    let mut out = String::from(
        "level,find_number,item_number,revision,name,quantity,source,unit_mass_kg,total_mass_kg,cg_x_m,cg_y_m,cg_z_m,detail,warnings,geometry_file,geometry_choice\n",
    );
    let cg = r.cg_m.unwrap_or([f64::NAN; 3]);
    let fmt = |v: f64| if v.is_finite() { num(v) } else { String::new() };
    out.push_str(&format!(
        "0,,TOTAL,{},{},1,{},{},{},{},{},{},{},{},{},{}\n",
        csv_field(&r.root_label),
        csv_field(&r.root_name),
        r.root_source.label(),
        num(r.mass_kg),
        num(r.mass_kg),
        fmt(cg[0]),
        fmt(cg[1]),
        fmt(cg[2]),
        csv_field(&format!(
            "placed {} kg, unplaced {} kg",
            num(r.placed_mass_kg),
            num(r.unplaced_mass_kg)
        )),
        csv_field(&r.warnings.join("; ")),
        csv_field(
            r.root_geometry
                .as_ref()
                .and_then(|g| g.file.as_deref())
                .unwrap_or("")
        ),
        r.root_geometry.as_ref().map_or("", |g| g.choice)
    ));
    for (label, tensor, point) in [
        ("INERTIA_CG", r.inertia_cg, r.cg_m),
        ("INERTIA_REF", r.inertia_ref, Some(r.reference_m)),
    ] {
        if let (Some(t), Some(p)) = (tensor, point) {
            out.push_str(&format!(
                "0,,{label},,,,,,,{},{},{},{},,,\n",
                num(p[0]),
                num(p[1]),
                num(p[2]),
                csv_field(&format!(
                    "Ixx={} Iyy={} Izz={} Ixy={} Iyz={} Ixz={} kg·m² (tensor form)",
                    num(t[0]),
                    num(t[1]),
                    num(t[2]),
                    num(t[3]),
                    num(t[4]),
                    num(t[5])
                ))
            ));
        }
    }
    for l in &r.lines {
        let cg = l.cg_m.unwrap_or([f64::NAN; 3]);
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            l.level,
            l.find_number,
            csv_field(&l.item_number),
            l.revision_code.as_deref().unwrap_or(""),
            csv_field(&l.name),
            num(l.quantity),
            l.source_label,
            l.unit_mass_kg.map(num).unwrap_or_default(),
            l.total_mass_kg.map(num).unwrap_or_default(),
            fmt(cg[0]),
            fmt(cg[1]),
            fmt(cg[2]),
            csv_field(&l.detail),
            csv_field(&l.warnings.join("; ")),
            csv_field(
                l.geometry
                    .as_ref()
                    .and_then(|g| g.file.as_deref())
                    .unwrap_or("")
            ),
            l.geometry.as_ref().map_or("", |g| g.choice)
        ));
    }
    out
}

/// The revisions a report read (for the read log).
pub fn report_revision_ids(r: &MassReport) -> Vec<Uuid> {
    let mut ids = vec![r.root_revision_id];
    ids.extend(r.lines.iter().filter_map(|l| l.revision_id));
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(id: Uuid) -> ResolvedRevision {
        ResolvedRevision {
            id,
            code: "A".into(),
            state: "in_work".into(),
            name: format!("rev {id}"),
            mass_kg: None,
            unit_cost: None,
        }
    }

    fn node(rev_id: Option<Uuid>, qty: f64, level: usize, children: Vec<BomNode>) -> BomNode {
        BomNode {
            line_id: Uuid::new_v4(),
            level,
            find_number: 10,
            quantity: qty,
            notes: String::new(),
            item_id: Uuid::new_v4(),
            item_number: "PRT".into(),
            item_type: "part".into(),
            unit: "each".into(),
            revision: rev_id.map(rev),
            unit_mass_kg: 0.0,
            total_mass_kg: 0.0,
            unit_cost_total: 0.0,
            total_cost: 0.0,
            mass_complete: true,
            cost_complete: true,
            cycle: false,
            restricted: false,
            project_id: None,
            children,
        }
    }

    fn tree(root: Uuid, children: Vec<BomNode>) -> bom::BomTree {
        bom::BomTree {
            rule: RevisionRule::Latest,
            root: rev(root),
            root_item_number: "ASM".into(),
            children,
            total_mass_kg: 0.0,
            total_cost: 0.0,
            mass_complete: true,
            cost_complete: true,
            line_count: 0,
            unresolved_count: 0,
            restricted_lines: 0,
            rollup_incomplete: false,
            queries: 0,
        }
    }

    /// A box geometry in mm from `lo` with edges `e`.
    fn boxed(lo: V3, e: V3) -> Geometry {
        let v = e[0] * e[1] * e[2];
        Geometry {
            version: massprops::VERSION,
            volume: v,
            centroid: [lo[0] + e[0] / 2.0, lo[1] + e[1] / 2.0, lo[2] + e[2] / 2.0],
            inertia: [
                v * (e[1] * e[1] + e[2] * e[2]) / 12.0,
                v * (e[0] * e[0] + e[2] * e[2]) / 12.0,
                v * (e[0] * e[0] + e[1] * e[1]) / 12.0,
                0.0,
                0.0,
                0.0,
            ],
            area: 0.0,
            bounds: [0.0; 6],
            triangles: 12,
            closed: true,
            checked: true,
            open_edges: 0,
            non_manifold_edges: 0,
            misoriented_edges: 0,
            degenerate_triangles: 0,
            inverted_fixed: false,
        }
    }

    fn file(name: &str, instances: i32) -> GeometryFile {
        GeometryFile {
            id: Uuid::new_v4(),
            revision_id: Uuid::nil(),
            filename: name.into(),
            sha256: "0".repeat(64),
            format: "stl".into(),
            length_unit: None,
            instances,
        }
    }

    fn part(g: Geometry, density: Option<f64>, instances: i32) -> RevisionInput {
        RevisionInput {
            material: "Aluminium".into(),
            geometry: Some((file("p.stl", instances), Ok(g))),
            density: density.map(|d| (d, "Aluminium".into())),
            ..RevisionInput::default()
        }
    }

    #[test]
    fn cube_and_offset_box_roll_up_by_parallel_axis() {
        let (root, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let t = tree(
            root,
            vec![node(Some(a), 1.0, 1, vec![]), node(Some(b), 1.0, 1, vec![])],
        );
        let mut inputs = HashMap::new();
        // 100 mm aluminium cube at the origin, 200×50×20 mm steel bar at x = 300 mm
        inputs.insert(a, part(boxed([0.0; 3], [100.0; 3]), Some(2700.0), 1));
        inputs.insert(
            b,
            part(
                boxed([300.0, 0.0, 0.0], [200.0, 50.0, 20.0]),
                Some(7850.0),
                1,
            ),
        );
        let r = report(&t, "ASM/A".into(), String::new(), true, &inputs, [0.0; 3]);
        let (ma, mb) = (2.7, 7850.0 * 2e-4);
        assert!((r.mass_kg - (ma + mb)).abs() < 1e-12);
        let cg = r.cg_m.unwrap();
        assert!((cg[0] - (ma * 0.05 + mb * 0.4) / (ma + mb)).abs() < 1e-12);
        assert!(r.complete, "{:?}", r.warnings);
        assert_eq!(r.root_source, Source::Assembly);
        assert_eq!(r.lines.len(), 2);
        assert!(r.lines.iter().all(|l| l.source == Source::Computed));
        // inertia about the origin = Σ about the origin
        let ba = boxed([0.0; 3], [100.0; 3]).body(2700.0, 1e-3);
        let bb = boxed([300.0, 0.0, 0.0], [200.0, 50.0, 20.0]).body(7850.0, 1e-3);
        let want = ba.inertia_about([0.0; 3]);
        let want_b = bb.inertia_about([0.0; 3]);
        for k in 0..6 {
            assert!((r.inertia_ref.unwrap()[k] - (want[k] + want_b[k])).abs() < 1e-12);
        }
        let csv = to_csv(&r);
        assert!(csv.starts_with("level,"));
        assert!(csv.contains("TOTAL") && csv.contains("INERTIA_CG"));
    }

    #[test]
    fn measured_mass_wins_and_covers_the_sub_tree() {
        let (root, sub, a) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let t = tree(
            root,
            vec![node(Some(sub), 1.0, 1, vec![node(Some(a), 1.0, 2, vec![])])],
        );
        let mut inputs = HashMap::new();
        inputs.insert(a, part(boxed([0.0; 3], [100.0; 3]), Some(2700.0), 1));
        inputs.insert(
            sub,
            RevisionInput {
                settings: MassSettings {
                    measured_mass_kg: Some(3.1),
                    measured_cg_m: Some(vec![0.1, 0.2, 0.3]),
                    measured_note: "scale 2026-11-01".into(),
                    ..MassSettings::default()
                },
                ..RevisionInput::default()
            },
        );
        let r = report(&t, "ASM/A".into(), String::new(), true, &inputs, [0.0; 3]);
        assert!((r.mass_kg - 3.1).abs() < 1e-12);
        let cg = r.cg_m.unwrap();
        assert!(cg
            .iter()
            .zip([0.1, 0.2, 0.3])
            .all(|(a, b)| (a - b).abs() < 1e-12));
        assert_eq!(r.lines[0].source, Source::Measured);
        assert_eq!(r.lines[0].detail, "scale 2026-11-01");
        assert_eq!(r.lines[1].source, Source::Covered);
        // point mass: a warning, no inertia about its own CG
        assert!(r.lines[0].warnings.iter().any(|w| w.contains("point mass")));
        assert!(r.inertia_cg.unwrap().iter().all(|v| v.abs() < 1e-12));
    }

    #[test]
    fn missing_density_entered_mass_restricted_and_quantities() {
        let root = Uuid::new_v4();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut hidden = node(None, 1.0, 1, vec![]);
        hidden.restricted = true;
        let t = tree(
            root,
            vec![
                node(Some(a), 1.0, 1, vec![]),
                node(Some(b), 2.0, 1, vec![]),
                node(Some(c), 4.0, 1, vec![]),
                hidden,
                node(None, 1.0, 1, vec![]),
            ],
        );
        let mut inputs = HashMap::new();
        // a: geometry, unknown material, no entered mass → missing density
        let mut ia = part(boxed([0.0; 3], [10.0; 3]), None, 1);
        ia.material = "Unobtainium".into();
        inputs.insert(a, ia);
        // b: entered mass only (bolts)
        inputs.insert(
            b,
            RevisionInput {
                entered_mass_kg: Some(0.01),
                ..RevisionInput::default()
            },
        );
        // c: four fins in one file, quantity 4: no stacking
        inputs.insert(c, part(boxed([0.0; 3], [10.0; 3]), Some(1000.0), 4));
        let r = report(&t, "ASM/A".into(), String::new(), true, &inputs, [0.0; 3]);
        assert!(!r.complete);
        let la = &r.lines[0];
        assert_eq!(la.source, Source::Missing);
        assert!(
            la.warnings.iter().any(|w| w.contains("Unobtainium")),
            "{la:?}"
        );
        let lb = &r.lines[1];
        assert_eq!(lb.source, Source::Entered);
        assert_eq!(lb.total_mass_kg, Some(0.02));
        assert!((r.unplaced_mass_kg - 0.02).abs() < 1e-12);
        // c: 1 g per file, a unit is a quarter, four units = the file
        let lc = &r.lines[2];
        assert!((lc.total_mass_kg.unwrap() - 1e-3).abs() < 1e-12);
        assert!(lc.warnings.is_empty(), "{lc:?}");
        assert!((lc.factor - 1.0).abs() < 1e-12);
        assert_eq!(r.restricted_lines, 1);
        assert_eq!(r.lines[3].source, Source::Restricted);
        assert_eq!(r.missing_lines, 2);
        assert!(r.warnings.iter().any(|w| w.contains("restricted")));
        assert!(r.warnings.iter().any(|w| w.contains("no position")));
        assert!((r.mass_kg - (0.02 + 1e-3)).abs() < 1e-12);
    }

    #[test]
    fn entered_mass_on_geometry_and_open_mesh_warnings() {
        let (root, a) = (Uuid::new_v4(), Uuid::new_v4());
        let t = tree(root, vec![node(Some(a), 1.0, 1, vec![])]);
        let mut g = boxed([0.0; 3], [100.0; 3]);
        g.open_edges = 3;
        g.closed = false;
        let mut input = part(g, None, 1);
        input.entered_mass_kg = Some(5.0);
        let mut inputs = HashMap::new();
        inputs.insert(a, input);
        let r = report(&t, "ASM/A".into(), String::new(), true, &inputs, [0.0; 3]);
        let l = &r.lines[0];
        assert_eq!(l.source, Source::EnteredOnGeometry);
        assert!((r.mass_kg - 5.0).abs() < 1e-12);
        assert_eq!(r.cg_m, Some([0.05, 0.05, 0.05]));
        // 5 kg cube of 0.1 m: m a² / 6
        assert!((r.inertia_cg.unwrap()[0] - 5.0 * 0.01 / 6.0).abs() < 1e-12);
        assert!(l.warnings.iter().any(|w| w.contains("open mesh")));
    }

    #[test]
    fn a_single_part_root_uses_its_own_geometry() {
        let root = Uuid::new_v4();
        let t = tree(root, vec![]);
        let mut inputs = HashMap::new();
        inputs.insert(root, part(boxed([0.0; 3], [100.0; 3]), Some(1000.0), 1));
        let r = report(&t, "PRT/A".into(), String::new(), false, &inputs, [0.0; 3]);
        assert_eq!(r.root_source, Source::Computed);
        assert!((r.mass_kg - 1.0).abs() < 1e-12);
    }

    #[test]
    fn reference_points_parse() {
        assert_eq!(parse_point(None).unwrap(), [0.0; 3]);
        assert_eq!(parse_point(Some(" 1, -2.5;3 ")).unwrap(), [1.0, -2.5, 3.0]);
        assert!(parse_point(Some("1,2")).is_err());
        assert!(parse_point(Some("1,2,x")).is_err());
        assert!(parse_point(Some("1,2,inf")).is_err());
    }

    #[test]
    fn settings_are_validated() {
        let ok = MassSettingsUpdate {
            measured_mass_kg: Some(1.0),
            measured_cg_m: Some([0.0, 0.0, 1.0]),
            measured_inertia_kg_m2: Some([1.0, 1.0, 1.5, 0.0, 0.0, 0.0]),
            length_unit: Some("IN".into()),
            ..MassSettingsUpdate::default()
        };
        let v = ok.clone().validated().unwrap();
        assert_eq!(v.length_unit.as_deref(), Some("in"));
        assert_eq!(v.instances, Some(1));
        let bad = [
            MassSettingsUpdate {
                density_kg_m3: Some(0.0),
                ..MassSettingsUpdate::default()
            },
            MassSettingsUpdate {
                measured_cg_m: Some([0.0; 3]),
                ..MassSettingsUpdate::default()
            },
            MassSettingsUpdate {
                measured_inertia_kg_m2: Some([1.0, 1.0, 3.0, 0.0, 0.0, 0.0]),
                ..ok.clone()
            },
            MassSettingsUpdate {
                length_unit: Some("ft".into()),
                ..MassSettingsUpdate::default()
            },
            MassSettingsUpdate {
                instances: Some(0),
                ..MassSettingsUpdate::default()
            },
            MassSettingsUpdate {
                measured_mass_kg: Some(f64::NAN),
                ..MassSettingsUpdate::default()
            },
        ];
        for b in bad {
            assert!(b.clone().validated().is_err(), "{b:?}");
        }
    }
}
