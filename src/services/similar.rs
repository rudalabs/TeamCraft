//! Similar part search: "do we already have a part like this?".
//!
//! Every 3D file (meshes and STEP, the formats of the 3D preview) gets a
//! shape signature (see `shape`) when its preview is generated – after an
//! upload, when a viewer opens it, or by `teamcraft vault signatures` for
//! older files. Signatures are stored once per blob (`shape_signatures`).
//!
//! [`find`] ranks the latest revisions of other items by
//! [`shape::compare`]: a SQL prefilter on indexed columns (the largest
//! extents within a factor of [`SIZE_WINDOW`], or the proportions within a
//! factor of [`RATIO_WINDOW`] when scale is ignored – wide enough never to
//! drop a part that could reach [`MIN_SCORE`]) keeps at most [`PREFILTER`]
//! files, which are then scored in Rust – the work stays bounded however
//! large the vault is. One hit per item (its best file), scores below
//! [`MIN_SCORE`] are dropped.
//!
//! Visibility is the one of the item search: every signed-in user may read
//! all product data, so candidates are all items except the source itself.

use std::{collections::HashMap, time::Duration};

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    models::{FileRecord, Revision},
    preview::PreviewFormat,
    services::files,
    shape::{self, Signature},
    AppState,
};

/// Candidates below this score (0…1) are not shown.
pub const MIN_SCORE: f64 = 0.5;
/// Most hits in one result.
pub const MAX_LIMIT: i64 = 50;
/// Files scored in Rust after the SQL prefilter.
pub const PREFILTER: i64 = 2000;
/// Largest and middle extent within this factor (with scale). A hit needs
/// `size ≥ MIN_SCORE`, i.e. Σ|ln ratio| over three extents ≤ 1.5·ln 2, so
/// no single extent of a hit is off by more than 2^1.5 ≈ 2.83.
pub const SIZE_WINDOW: f64 = 3.0;
/// Both proportions within this factor (ignoring scale): the two |ln ratio|
/// of a hit sum to at most ln 2.
pub const RATIO_WINDOW: f64 = 2.0;
/// Lengths below this share of the largest count as this share (as in
/// `shape::compare`).
const THIN: f64 = 1e-3;

/// How to search.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub limit: i64,
    /// Compare proportions and shape only (a scaled copy scores 100 %).
    pub ignore_scale: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            limit: 10,
            ignore_scale: false,
        }
    }
}

/// One similar part.
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub item_id: Uuid,
    pub item_number: String,
    pub item_type: String,
    pub project_code: Option<String>,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub state: String,
    pub name: String,
    /// The 3D file that matched.
    pub file_id: Uuid,
    pub filename: String,
    /// Same content as the source file (an exact copy).
    pub identical: bool,
    /// Similarity in percent (0…100), the ranking key.
    pub score: u8,
    /// Shape distribution similarity in percent.
    pub shape: u8,
    /// Size agreement in percent (100 = same extents, or same proportions
    /// when scale is ignored).
    pub size: u8,
    pub extents: [f64; 3],
    pub explanation: String,
    /// A file of the revision with a stored preview image (native CAD).
    pub thumbnail_file_id: Option<Uuid>,
}

/// The source of a search and what is known about its shape.
#[derive(Clone, Debug, Serialize)]
pub struct SourceFile {
    pub file_id: Uuid,
    pub filename: String,
    pub sha256: String,
    pub format: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Searched; `candidates` holds the result (possibly empty).
    Ready,
    /// The shape signature is still being computed.
    Pending,
    /// The revision has no mesh or STEP file.
    NoShapeFile,
    /// No signature can be computed (the reason is in `message`).
    Failed,
}

/// The answer for a revision (web page, REST API, MCP).
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub status: Status,
    pub message: Option<String>,
    pub source: Option<SourceFile>,
    pub signature: Option<SignatureSummary>,
    pub ignore_scale: bool,
    pub candidates: Vec<Candidate>,
}

/// The signature in the words of a result.
#[derive(Clone, Debug, Serialize)]
pub struct SignatureSummary {
    pub extents: [f64; 3],
    pub area: f64,
    /// `None` when the mesh is not closed (the volume would be unreliable).
    pub volume: Option<f64>,
    pub closed: bool,
    pub triangles: u64,
}

impl From<&Signature> for SignatureSummary {
    fn from(s: &Signature) -> Self {
        SignatureSummary {
            extents: s.extents,
            area: s.area,
            volume: s.closed.then_some(s.volume),
            closed: s.closed,
            triangles: s.triangles,
        }
    }
}

/// The preview format of a file usable for shapes (not point clouds).
pub fn shape_format(file: &FileRecord) -> Option<PreviewFormat> {
    PreviewFormat::of_format(&file.format)
        .or_else(|| PreviewFormat::of(&file.filename))
        .filter(|f| !f.is_point_cloud())
}

/// The file a revision is compared by: `filename` if given, else its STEP
/// model, else its first mesh.
pub fn pick_file<'a>(
    files: &'a [FileRecord],
    filename: Option<&str>,
) -> AppResult<Option<(&'a FileRecord, PreviewFormat)>> {
    if let Some(name) = filename.map(str::trim).filter(|n| !n.is_empty()) {
        let file = files
            .iter()
            .find(|f| f.filename.eq_ignore_ascii_case(name))
            .ok_or_else(|| AppError::not_found(format!("The revision has no file '{name}'.")))?;
        let format = shape_format(file).ok_or_else(|| {
            AppError::BadRequest(format!(
                "'{}' is no 3D model with a preview (meshes such as STL, OBJ, 3MF, glTF or STEP).",
                file.filename
            ))
        })?;
        return Ok(Some((file, format)));
    }
    let mut usable: Vec<(&FileRecord, PreviewFormat)> = files
        .iter()
        .filter_map(|f| shape_format(f).map(|p| (f, p)))
        .collect();
    usable.sort_by_key(|(_, p)| !p.is_brep());
    Ok(usable.into_iter().next())
}

#[derive(sqlx::FromRow)]
struct SignatureRow {
    version: i16,
    extent_1: f64,
    extent_2: f64,
    extent_3: f64,
    area: f64,
    volume: f64,
    closed: bool,
    triangles: i64,
    d2: Vec<f32>,
}

impl SignatureRow {
    fn signature(&self) -> Option<Signature> {
        let s = Signature {
            version: self.version,
            extents: [self.extent_1, self.extent_2, self.extent_3],
            area: self.area,
            volume: self.volume,
            closed: self.closed,
            triangles: u64::try_from(self.triangles).unwrap_or(0),
            d2: self.d2.clone(),
        };
        s.is_valid().then_some(s)
    }
}

const SIGNATURE_COLUMNS: &str =
    "s.version, s.extent_1, s.extent_2, s.extent_3, s.area, s.volume, s.closed, s.triangles, s.d2";

/// The stored signature of a blob (of the current [`shape::VERSION`]).
pub async fn stored(pool: &PgPool, sha256: &str) -> AppResult<Option<Signature>> {
    let row = sqlx::query_as::<_, SignatureRow>(&format!(
        "SELECT {SIGNATURE_COLUMNS} FROM shape_signatures s WHERE s.sha256 = $1 AND s.version = $2"
    ))
    .bind(sha256)
    .bind(shape::VERSION)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.signature()))
}

/// Stores (or replaces) the signature of a blob. Derived data – no audit
/// record, like the text index.
pub async fn store(pool: &PgPool, sha256: &str, s: &Signature) -> AppResult<()> {
    if !s.is_valid() {
        return Err(AppError::BadRequest("Invalid shape signature.".into()));
    }
    let [r2, r3] = s.proportions();
    sqlx::query(
        "INSERT INTO shape_signatures
             (sha256, version, extent_1, extent_2, extent_3, ratio_2, ratio_3, area, volume, closed, triangles, d2)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (sha256) DO UPDATE SET
             version = EXCLUDED.version, extent_1 = EXCLUDED.extent_1, extent_2 = EXCLUDED.extent_2,
             extent_3 = EXCLUDED.extent_3, ratio_2 = EXCLUDED.ratio_2, ratio_3 = EXCLUDED.ratio_3,
             area = EXCLUDED.area, volume = EXCLUDED.volume, closed = EXCLUDED.closed,
             triangles = EXCLUDED.triangles, d2 = EXCLUDED.d2, computed_at = now()",
    )
    .bind(sha256)
    .bind(s.version)
    .bind(s.extents[0])
    .bind(s.extents[1])
    .bind(s.extents[2])
    .bind(r2 as f32)
    .bind(r3 as f32)
    .bind(s.area)
    .bind(s.volume)
    .bind(s.closed)
    .bind(i64::try_from(s.triangles).unwrap_or(i64::MAX))
    .bind(&s.d2)
    .execute(pool)
    .await?;
    Ok(())
}

/// The signature of a blob: stored, or computed in the preview process
/// (converting the file first if needed) and stored.
pub async fn ensure(st: &AppState, sha256: &str, format: PreviewFormat) -> AppResult<Signature> {
    if let Some(s) = stored(&st.db, sha256).await? {
        return Ok(s);
    }
    let s = st.previews.signature(&st.vault, sha256, format).await?;
    store(&st.db, sha256, &s).await?;
    Ok(s)
}

/// [`ensure`] in the background (after uploads); the handle may be awaited
/// with a time limit – dropping it does not stop the work.
pub fn spawn_ensure(
    st: &AppState,
    sha256: &str,
    format: PreviewFormat,
) -> tokio::task::JoinHandle<AppResult<Signature>> {
    let st = st.clone();
    let sha256 = sha256.to_string();
    tokio::spawn(async move {
        let result = ensure(&st, &sha256, format).await;
        if let Err(e) = &result {
            tracing::debug!(sha256, error = %e.public_message(), "no shape signature");
        }
        result
    })
}

#[derive(sqlx::FromRow)]
struct CandidateRow {
    sha256: String,
    version: i16,
    extent_1: f64,
    extent_2: f64,
    extent_3: f64,
    area: f64,
    volume: f64,
    closed: bool,
    triangles: i64,
    d2: Vec<f32>,
    file_id: Uuid,
    filename: String,
    revision_id: Uuid,
    revision_code: String,
    state: String,
    name: String,
    item_id: Uuid,
    item_number: String,
    item_type: String,
    project_code: Option<String>,
}

/// Lengths below this share of the largest count as this share (as in
/// `shape::compare`), so flat parts have a usable window.
fn floor_extents(e: [f64; 3]) -> [f64; 3] {
    let min = (e[0] * THIN).max(1e-12);
    e.map(|x| x.max(min))
}

/// The SQL prefilter of a search: `[lo₁, hi₁, lo₂, hi₂]` of the window and
/// the three targets of its order (see [`find`]). With scale: the largest
/// and the (floored) middle extent; ignoring scale: the two (floored)
/// proportions, a lower bound below [`THIN`] reaching down to 0.
fn prefilter(source: &Signature, ignore_scale: bool) -> ([f64; 4], [f64; 3]) {
    if ignore_scale {
        let [r2, r3] = source.proportions().map(|r| r.max(THIN));
        let w = RATIO_WINDOW;
        let lo = |r: f64| if r / w <= THIN { -1.0 } else { r / w };
        ([lo(r2), r2 * w, lo(r3), r3 * w], [r2, r3, 0.0])
    } else {
        let e = floor_extents(source.extents);
        let w = SIZE_WINDOW;
        ([e[0] / w, e[0] * w, e[1] / w, e[1] * w], e)
    }
}

/// Ranks the latest revisions of all other items by their similarity to
/// `source` (see the module documentation).
pub async fn find(
    pool: &PgPool,
    actor: &Actor,
    source_item_id: Uuid,
    source_sha256: &str,
    source: &Signature,
    options: Options,
) -> AppResult<Vec<Candidate>> {
    // the item search's visibility: every signed-in user reads everything
    actor.require(Role::Viewer)?;
    let limit = options.limit.clamp(1, MAX_LIMIT) as usize;
    // $1 version, $2 source item, $3…$6 window, $7…$9 targets, $10 limit;
    // the order is the size distance of `shape::compare` (floored lengths)
    let (window, order) = if options.ignore_scale {
        (
            "s.ratio_2 BETWEEN $3 AND $4 AND s.ratio_3 BETWEEN $5 AND $6",
            "abs(ln(GREATEST(s.ratio_2::float8, 1e-3) / $7)) \
             + abs(ln(GREATEST(s.ratio_3::float8, 1e-3) / $8)) + 0 * $9",
        )
    } else {
        (
            "s.extent_1 BETWEEN $3 AND $4 \
             AND GREATEST(s.extent_2, s.extent_1 * 1e-3) BETWEEN $5 AND $6",
            "abs(ln(s.extent_1 / $7)) + abs(ln(GREATEST(s.extent_2, s.extent_1 * 1e-3) / $8)) \
             + abs(ln(GREATEST(s.extent_3, s.extent_1 * 1e-3) / $9))",
        )
    };
    let ([lo1, hi1, lo2, hi2], [t1, t2, t3]) = prefilter(source, options.ignore_scale);
    let rows = sqlx::query_as::<_, CandidateRow>(&format!(
        "SELECT s.sha256, {SIGNATURE_COLUMNS},
                f.id AS file_id, f.filename, r.id AS revision_id, r.code AS revision_code, r.state, r.name,
                i.id AS item_id, i.number AS item_number, i.item_type, p.code AS project_code
         FROM shape_signatures s
         JOIN files f ON f.sha256 = s.sha256
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN projects p ON p.id = i.project_id
         WHERE s.version = $1 AND i.id <> $2 AND {window}
           AND r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)
         ORDER BY {order}, i.number, f.filename
         LIMIT $10"
    ))
    .bind(shape::VERSION)
    .bind(source_item_id)
    .bind(lo1)
    .bind(hi1)
    .bind(lo2)
    .bind(hi2)
    .bind(t1)
    .bind(t2)
    .bind(t3)
    .bind(PREFILTER)
    .fetch_all(pool)
    .await?;

    // best file per item
    let mut best: HashMap<Uuid, (shape::Similarity, CandidateRow, Signature)> = HashMap::new();
    for row in rows {
        let sig = SignatureRow {
            version: row.version,
            extent_1: row.extent_1,
            extent_2: row.extent_2,
            extent_3: row.extent_3,
            area: row.area,
            volume: row.volume,
            closed: row.closed,
            triangles: row.triangles,
            d2: row.d2.clone(),
        }
        .signature();
        let Some(sig) = sig else { continue };
        let sim = shape::compare(source, &sig, options.ignore_scale);
        if sim.score < MIN_SCORE {
            continue;
        }
        match best.get(&row.item_id) {
            Some((b, _, _)) if b.score >= sim.score => {}
            _ => {
                best.insert(row.item_id, (sim, row, sig));
            }
        }
    }
    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.0.score
            .total_cmp(&a.0.score)
            .then_with(|| a.1.item_number.cmp(&b.1.item_number))
    });
    ranked.truncate(limit);

    // preview images of native CAD files of the hits
    let revision_ids: Vec<Uuid> = ranked.iter().map(|r| r.1.revision_id).collect();
    let thumbs = thumbnails(pool, &revision_ids).await?;
    let percent = |x: f64| (x * 100.0).floor().clamp(0.0, 100.0) as u8;
    Ok(ranked
        .into_iter()
        .map(|(sim, row, sig)| Candidate {
            explanation: if row.sha256 == source_sha256 {
                "identical file content".to_string()
            } else {
                shape::explain(source, &sig, options.ignore_scale)
            },
            identical: row.sha256 == source_sha256,
            thumbnail_file_id: thumbs.get(&row.revision_id).copied(),
            item_id: row.item_id,
            item_number: row.item_number,
            item_type: row.item_type,
            project_code: row.project_code,
            revision_id: row.revision_id,
            revision_code: row.revision_code,
            state: row.state,
            name: row.name,
            file_id: row.file_id,
            filename: row.filename,
            score: percent(sim.score),
            shape: percent(sim.shape),
            size: percent(sim.size),
            extents: sig.extents,
        })
        .collect())
}

/// Per revision the first file with an embedded preview image.
async fn thumbnails(pool: &PgPool, revision_ids: &[Uuid]) -> AppResult<HashMap<Uuid, Uuid>> {
    if revision_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, (Uuid, Uuid, Option<String>)>(
        "SELECT revision_id, id, format FROM files
         WHERE revision_id = ANY($1) ORDER BY lower(filename)",
    )
    .bind(revision_ids)
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::new();
    for (revision_id, file_id, format) in rows {
        if format.as_deref().is_some_and(crate::thumbnail::supports) {
            out.entry(revision_id).or_insert(file_id);
        }
    }
    Ok(out)
}

/// Similar parts of a revision: picks its 3D file (or `filename`), gets
/// the signature – waiting at most `wait` for a computation, which goes on
/// in the background – and searches.
pub async fn for_revision(
    st: &AppState,
    actor: &Actor,
    rev: &Revision,
    filename: Option<&str>,
    options: Options,
    wait: Duration,
) -> AppResult<Report> {
    let files = files::list(&st.db, rev.id).await?;
    for_files(st, actor, rev, &files, filename, options, wait).await
}

/// [`for_revision`] with the revision's files already at hand.
pub async fn for_files(
    st: &AppState,
    actor: &Actor,
    rev: &Revision,
    files: &[FileRecord],
    filename: Option<&str>,
    options: Options,
    wait: Duration,
) -> AppResult<Report> {
    let mut report = Report {
        status: Status::NoShapeFile,
        message: None,
        source: None,
        signature: None,
        ignore_scale: options.ignore_scale,
        candidates: Vec::new(),
    };
    let Some((file, format)) = pick_file(files, filename)? else {
        report.message = Some(format!(
            "{}/{} has no 3D model to compare (meshes such as STL, OBJ, 3MF, glTF or STEP); \
             attach a STEP export of native CAD files.",
            rev.item_number, rev.code
        ));
        return Ok(report);
    };
    report.source = Some(SourceFile {
        file_id: file.id,
        filename: file.filename.clone(),
        sha256: file.sha256.clone(),
        format: format.as_str(),
    });
    let signature = match stored(&st.db, &file.sha256).await? {
        Some(s) => s,
        None => {
            let task = spawn_ensure(st, &file.sha256, format);
            match tokio::time::timeout(wait, task).await {
                Err(_) => {
                    report.status = Status::Pending;
                    report.message = Some(
                        "The shape signature is being computed – try again in a moment.".into(),
                    );
                    return Ok(report);
                }
                Ok(Err(e)) => return Err(AppError::Other(anyhow::anyhow!("signature task: {e}"))),
                Ok(Ok(Err(e @ AppError::BadRequest(_))))
                | Ok(Ok(Err(e @ AppError::NotFound(_)))) => {
                    report.status = Status::Failed;
                    report.message = Some(e.public_message());
                    return Ok(report);
                }
                Ok(Ok(Err(e))) => return Err(e),
                Ok(Ok(Ok(s))) => s,
            }
        }
    };
    report.candidates = find(
        &st.db,
        actor,
        rev.item_id,
        &file.sha256,
        &signature,
        options,
    )
    .await?;
    report.signature = Some(SignatureSummary::from(&signature));
    report.status = Status::Ready;
    Ok(report)
}

/// Stored files (current versions) with a 3D format and no signature yet,
/// for `teamcraft vault signatures`: distinct blobs after `after` (keyset
/// paging by hash) with their format.
pub async fn missing(pool: &PgPool, after: &str, limit: i64) -> AppResult<Vec<(String, String)>> {
    let formats: Vec<&str> = PreviewFormat::ALL
        .iter()
        .filter(|f| !f.is_point_cloud())
        .map(|f| f.as_str())
        .collect();
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT ON (f.sha256) f.sha256, f.format FROM files f
         WHERE f.format = ANY($1) AND f.sha256 > $4
           AND NOT EXISTS (SELECT 1 FROM shape_signatures s
                           WHERE s.sha256 = f.sha256 AND s.version = $2)
         ORDER BY f.sha256
         LIMIT $3",
    )
    .bind(&formats)
    .bind(shape::VERSION)
    .bind(limit)
    .bind(after)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(extents: [f64; 3]) -> Signature {
        Signature {
            version: shape::VERSION,
            extents,
            area: 1.0,
            volume: 0.0,
            closed: false,
            triangles: 12,
            d2: vec![1.0 / shape::D2_BINS as f32; shape::D2_BINS],
        }
    }

    /// The SQL window of [`find`], evaluated in Rust on stored values.
    fn passes(window: [f64; 4], c: &Signature, ignore_scale: bool) -> bool {
        let inside = |v: f64, lo: f64, hi: f64| lo <= v && v <= hi;
        if ignore_scale {
            let [r2, r3] = c.proportions().map(|r| r as f32 as f64);
            inside(r2, window[0], window[1]) && inside(r3, window[2], window[3])
        } else {
            let e = c.extents;
            inside(e[0], window[0], window[1])
                && inside(e[1].max(e[0] * 1e-3), window[2], window[3])
        }
    }

    /// The prefilter never drops a part that `shape::compare` would show:
    /// every candidate whose size agreement reaches MIN_SCORE (the score
    /// can be no higher) lies inside the window – flat parts included.
    #[test]
    fn prefilter_keeps_every_possible_hit() {
        let lengths = [0.4, 1.0, 2.6, 2.9, 7.0, 25.0];
        let shares = [
            0.0, 0.0004, 0.001, 0.0015, 0.003, 0.1, 0.3, 0.45, 0.5, 0.7, 0.95, 1.0,
        ];
        let mut all = Vec::new();
        for &l in &lengths {
            for &f in &shares {
                for &g in &shares {
                    all.push(signature([l, l * f, l * f * g]));
                }
            }
        }
        let mut checked = 0;
        for source in all.iter().step_by(7) {
            for ignore_scale in [false, true] {
                let (window, _) = prefilter(source, ignore_scale);
                for c in &all {
                    let sim = shape::compare(source, c, ignore_scale);
                    if sim.size >= MIN_SCORE + 1e-9 {
                        checked += 1;
                        assert!(
                            passes(window, c, ignore_scale),
                            "ignore_scale={ignore_scale}: {:?} vs {:?} (size {}) outside {window:?}",
                            source.extents,
                            c.extents,
                            sim.size
                        );
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked}");
        // and the window does filter: much larger, or other proportions
        let plate = signature([120.0, 40.0, 8.0]);
        let (window, _) = prefilter(&plate, false);
        assert!(!passes(window, &signature([400.0, 40.0, 8.0]), false));
        let (window, _) = prefilter(&plate, true);
        assert!(passes(window, &signature([240.0, 80.0, 16.0]), true));
        assert!(!passes(window, &signature([50.0, 50.0, 50.0]), true));
    }
}
