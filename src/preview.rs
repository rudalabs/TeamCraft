//! 3D preview of CAD files – pure Rust, no browser plug-ins or downloads.
//!
//! The server converts a file once into the compact mesh of [`crate::mesh`]
//! and caches it next to the vault (`previews/ab/<sha256>.v<N>.tcm`); the
//! browser (`shared/js/viewer.js`) and the desktop app only draw it. Meshes,
//! point clouds, STEP B-reps (tessellated with the `truck` kernel) and
//! simulation results (VTK, VTU/VTP, CalculiX FRD with result fields, see
//! [`crate::simmesh`]) are supported, see [`PreviewFormat`].
//!
//! The conversion never blocks the web server: it runs in a separate
//! `teamcraft preview-mesh` process with a time limit (in tests, where there is
//! no such binary, on a blocking thread), at most [`MAX_PARALLEL`] at a time,
//! and concurrent requests for the same file share one conversion. Failures
//! are cached as well, so a broken file is not converted again and again.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};

use crate::{
    error::{AppError, AppResult},
    massprops, shape,
    vault::Vault,
};

/// Bump to invalidate every cached preview (reader fixes, new format).
/// Bump `shape::VERSION` with it: the shape signatures stored in the
/// database are computed from these meshes.
pub const CACHE_VERSION: u32 = 2;

/// Conversions that run at the same time.
pub const MAX_PARALLEL: usize = 2;

/// Thumbnails drawn and assembly previews composed at the same time
/// ([`Previews::render`]): each keeps one CPU busy and holds its meshes in
/// memory.
pub const MAX_RENDERS: usize = 2;

/// Size and memory limits of the preview (`TEAMCRAFT_PREVIEW_MAX_*`).
///
/// A file above the cap of its kind is never opened: no conversion is
/// started, the refusal is cached. Result files (VTK, VTU, VTP, FRD) are the
/// ones that get huge and are not meant to be parsed fully – a preview of
/// them is a courtesy, not a feature the server may die for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewLimits {
    /// Simulation result files.
    pub max_result_bytes: u64,
    /// Meshes and point clouds.
    pub max_mesh_bytes: u64,
    /// STEP B-reps (tessellation needs a multiple of the file size).
    pub max_step_bytes: u64,
    /// Address space of the preview process (0 = unlimited; Linux/macOS).
    pub max_memory_bytes: u64,
}

const MIB: u64 = 1024 * 1024;

impl Default for PreviewLimits {
    fn default() -> Self {
        PreviewLimits {
            max_result_bytes: 64 * MIB,
            max_mesh_bytes: 256 * MIB,
            max_step_bytes: crate::mesh::MAX_STEP_BYTES as u64,
            max_memory_bytes: 1024 * MIB,
        }
    }
}

impl PreviewLimits {
    /// The largest input of a format.
    pub fn max_bytes(&self, format: PreviewFormat) -> u64 {
        if format.is_result() {
            self.max_result_bytes
        } else if format.is_brep() {
            self.max_step_bytes
        } else {
            self.max_mesh_bytes
        }
    }
}

/// Start of every refusal because of the file size (also what is cached).
const TOO_LARGE: &str = "This file is too large for a 3D preview";

/// Why a file of `size` bytes gets no preview.
fn too_large_message(size: u64, limit: u64) -> String {
    format!(
        "{TOO_LARGE} ({} MB, at most {} MB) – attach a rendered picture or a report, \
         or download the file.",
        size.div_ceil(MIB),
        limit / MIB
    )
}

/// Refuses a file above `limit` before it is opened.
fn check_size(input: &Path, limit: u64) -> anyhow::Result<()> {
    let size = std::fs::metadata(input)?.len();
    if size > limit {
        anyhow::bail!("{}", too_large_message(size, limit));
    }
    Ok(())
}

/// Limits the address space of this process (the preview child), so a file
/// that needs more memory fails cleanly instead of taking the server host
/// down. Linux and macOS; elsewhere (Windows) the limit is not enforced.
/// Returns whether a limit is in force.
pub fn limit_memory(bytes: u64) -> bool {
    if bytes == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        rlimit::setrlimit(rlimit::Resource::AS, bytes, bytes).is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The message for a preview process that stopped without an error message
/// (`stderr` = what it printed last): memory exhausted or killed.
fn describe_failure(aborted: bool, stderr: &str, memory_limit: u64) -> String {
    let out_of_memory = aborted
        || stderr.contains("memory allocation")
        || stderr.contains("out of memory")
        || stderr.contains("capacity overflow");
    if out_of_memory && memory_limit > 0 {
        return format!(
            "The preview needs more memory than allowed ({} MB) – attach a rendered picture \
             or a report, or download the file.",
            memory_limit / MIB
        );
    }
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("the converter stopped unexpectedly")
        .trim_start_matches("Error: ")
        .chars()
        .take(500)
        .collect()
}

/// 3D formats the preview understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewFormat {
    Stl,
    Obj,
    Ply,
    Off,
    ThreeMf,
    Amf,
    Gltf,
    Glb,
    ThreeDs,
    Step,
    /// Siemens NX parts: the lightweight (JT) representation they store.
    NxPart,
    /// Standalone JT visualisation files: JT 10 with its scene graph (parts
    /// and assemblies), JT 9 as NX writes it.
    Jt,
    /// SOLIDWORKS parts: the display tessellation they store.
    SwPart,
    /// SOLIDWORKS assemblies: put together from the previews of their parts.
    SwAssembly,
    /// Rhino models: the meshes they store, SubD control nets refined.
    Rhino,
    /// Inventor parts: the display tessellation they store.
    InvPart,
    /// Inventor assemblies: put together from the previews of their parts.
    InvAssembly,
    /// SketchUp models: their faces triangulated, components placed.
    SketchUp,
    Pts,
    Xyz,
    Las,
    /// Simulation results (see `simmesh`): legacy VTK, VTU, VTP, CalculiX FRD.
    VtkLegacy,
    Vtu,
    Vtp,
    Frd,
}

impl PreviewFormat {
    pub const ALL: [PreviewFormat; 25] = [
        PreviewFormat::Stl,
        PreviewFormat::Obj,
        PreviewFormat::Ply,
        PreviewFormat::Off,
        PreviewFormat::ThreeMf,
        PreviewFormat::Amf,
        PreviewFormat::Gltf,
        PreviewFormat::Glb,
        PreviewFormat::ThreeDs,
        PreviewFormat::Step,
        PreviewFormat::NxPart,
        PreviewFormat::Jt,
        PreviewFormat::SwPart,
        PreviewFormat::SwAssembly,
        PreviewFormat::Rhino,
        PreviewFormat::InvPart,
        PreviewFormat::InvAssembly,
        PreviewFormat::SketchUp,
        PreviewFormat::Pts,
        PreviewFormat::Xyz,
        PreviewFormat::Las,
        PreviewFormat::VtkLegacy,
        PreviewFormat::Vtu,
        PreviewFormat::Vtp,
        PreviewFormat::Frd,
    ];

    /// Detects the format from the file name (see `cadformat`).
    pub fn of(filename: &str) -> Option<PreviewFormat> {
        crate::cadformat::detect(filename.trim(), &[]).and_then(|f| Self::of_format(f.id))
    }

    /// The preview of a stored format id (`files.format`).
    pub fn of_format(format_id: &str) -> Option<PreviewFormat> {
        Self::ALL.into_iter().find(|p| p.as_str() == format_id)
    }

    /// The `cadformat` id of the format.
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewFormat::Stl => "stl",
            PreviewFormat::Obj => "obj",
            PreviewFormat::Ply => "ply",
            PreviewFormat::Off => "off",
            PreviewFormat::ThreeMf => "3mf",
            PreviewFormat::Amf => "amf",
            PreviewFormat::Gltf => "gltf",
            PreviewFormat::Glb => "glb",
            PreviewFormat::ThreeDs => "3ds",
            PreviewFormat::Step => "step",
            PreviewFormat::NxPart => "nx-part",
            PreviewFormat::Jt => "jt",
            PreviewFormat::SwPart => "solidworks-part",
            PreviewFormat::SwAssembly => "solidworks-assembly",
            PreviewFormat::Rhino => "rhino",
            PreviewFormat::InvPart => "inventor-part",
            PreviewFormat::InvAssembly => "inventor-assembly",
            PreviewFormat::SketchUp => "sketchup",
            PreviewFormat::Pts => "pts",
            PreviewFormat::Xyz => "xyz",
            PreviewFormat::Las => "las",
            PreviewFormat::VtkLegacy => "vtk-legacy",
            PreviewFormat::Vtu => "vtk-unstructured",
            PreviewFormat::Vtp => "vtk-polydata",
            PreviewFormat::Frd => "calculix-frd",
        }
    }

    /// Simulation results: the outer surface coloured by result fields.
    pub fn is_result(self) -> bool {
        matches!(
            self,
            PreviewFormat::VtkLegacy | PreviewFormat::Vtu | PreviewFormat::Vtp | PreviewFormat::Frd
        )
    }

    /// B-rep formats are tessellated (slower) – the rest is read as is.
    pub fn is_brep(self) -> bool {
        matches!(self, PreviewFormat::Step)
    }

    /// Assemblies whose preview is put together from their parts' previews
    /// (`services::assemblies`): NX files without a lightweight
    /// representation of their own, SOLIDWORKS and Inventor assemblies.
    pub fn may_be_assembly(self) -> bool {
        matches!(
            self,
            PreviewFormat::NxPart | PreviewFormat::SwAssembly | PreviewFormat::InvAssembly
        )
    }

    pub fn is_point_cloud(self) -> bool {
        matches!(
            self,
            PreviewFormat::Pts | PreviewFormat::Xyz | PreviewFormat::Las
        )
    }

    /// Time limit of one conversion.
    pub fn time_limit(self) -> Duration {
        if self.is_brep() {
            Duration::from_secs(180)
        } else if self.is_result() {
            // large volume meshes: boundary extraction and decompression
            Duration::from_secs(120)
        } else {
            Duration::from_secs(60)
        }
    }
}

/// Converts the file at `input` (tests and tools).
pub fn convert_file(format: PreviewFormat, input: &Path) -> anyhow::Result<Vec<u8>> {
    Ok(convert(format, input)?.encode())
}

/// Reads the file at `input` as a preview mesh.
pub fn convert(format: PreviewFormat, input: &Path) -> anyhow::Result<crate::mesh::Mesh> {
    convert_within(format, input, PreviewLimits::default().max_bytes(format))
}

/// Like [`convert`]; a file above `max_bytes` is refused before it is opened.
pub fn convert_within(
    format: PreviewFormat,
    input: &Path,
    max_bytes: u64,
) -> anyhow::Result<crate::mesh::Mesh> {
    check_size(input, max_bytes)?;
    let data = std::fs::read(input)?;
    crate::mesh::read(format.as_str(), &data)
}

/// `--format` of `teamcraft preview-mesh` for an already converted preview
/// mesh (only its shape signature is computed).
pub const CACHED_MESH: &str = "tcm";

/// One job of the preview process (`teamcraft preview-mesh`): converts
/// `input` into the preview mesh at `output` and/or writes the shape
/// signature (a [`shape::Outcome`] as JSON) to `signature`. With
/// [`CACHED_MESH`] as format, `input` is a cached preview mesh. A failed
/// signature does not fail the conversion – the outcome records why. The
/// mass properties (a [`massprops::Outcome`] as JSON) go to `mass` the same
/// way. Returns the mesh (the CLI prints its size).
pub fn run_job(
    format: &str,
    input: &Path,
    output: Option<&Path>,
    signature: Option<&Path>,
    mass: Option<&Path>,
    max_input_bytes: u64,
) -> anyhow::Result<crate::mesh::Mesh> {
    let mesh = if format == CACHED_MESH {
        check_size(input, max_input_bytes)?;
        let mesh = crate::mesh::Mesh::decode(&std::fs::read(input)?)?;
        mesh.check()?;
        mesh
    } else {
        let format = PreviewFormat::of_format(format)
            .ok_or_else(|| anyhow::anyhow!("no 3D preview for the format '{format}'"))?;
        convert_within(format, input, max_input_bytes)?
    };
    if let Some(output) = output {
        std::fs::write(output, mesh.encode())?;
    }
    if let Some(path) = signature {
        std::fs::write(path, serde_json::to_vec(&signature_outcome(&mesh))?)?;
    }
    if let Some(path) = mass {
        std::fs::write(path, serde_json::to_vec(&mass_outcome(&mesh))?)?;
    }
    Ok(mesh)
}

/// The mass properties of a preview mesh (per unit density, model units),
/// or why there are none.
fn mass_outcome(mesh: &crate::mesh::Mesh) -> massprops::Outcome {
    let computed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| massprops::compute(mesh)));
    match computed {
        Ok(Ok(g)) => massprops::Outcome::Geometry(g),
        Ok(Err(e)) => massprops::Outcome::Error(e.to_string()),
        Err(_) => massprops::Outcome::Error("The mass properties could not be computed.".into()),
    }
}

/// The shape signature of a preview mesh, or why there is none.
fn signature_outcome(mesh: &crate::mesh::Mesh) -> shape::Outcome {
    // A partial STEP preview (faces the kernel could not tessellate are
    // missing) gets no signature: area, volume and the D2 distribution of
    // a model with holes would compare it wrongly – a part with half its
    // faces missing would look like a different, open part. The preview
    // itself is still shown; the part is left out of the similar part
    // search (also as a candidate) with this reason instead of a misleading
    // score. When a later kernel shows every face, `CACHE_VERSION` and
    // `shape::VERSION` are bumped and the signature is computed then.
    if let Some((shown, total)) = mesh.faces {
        return shape::Outcome::Error(format!(
            "The 3D preview shows only {shown} of {total} faces, so the shape cannot be compared."
        ));
    }
    // a bug in the signature must never cost the preview itself
    let computed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shape::compute(mesh)));
    match computed {
        Ok(Ok(s)) => shape::Outcome::Signature(s),
        Ok(Err(e)) => shape::Outcome::Error(e.to_string()),
        Err(_) => shape::Outcome::Error("The shape signature could not be computed.".into()),
    }
}

/// The `teamcraft` binary that runs isolated jobs (`preview-mesh`,
/// `text-extract`); `None` in tests and other binaries, which run them
/// in-process.
pub fn worker_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|exe| {
        exe.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == "teamcraft")
    })
}

/// One-at-a-time turns per key ([`Previews::render`]): who asks for a key
/// that is being worked on waits for that work instead of repeating it.
#[derive(Clone, Default)]
struct Turns(Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>);

impl Turns {
    fn place(&self, key: &str) -> Place {
        let lock = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default()
            .clone();
        Place {
            turns: self.clone(),
            key: key.to_string(),
            lock: Some(lock),
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// A place in the line of a key. The key is forgotten when the last place
/// in its line is given up – also by a request that went away while waiting.
struct Place {
    turns: Turns,
    key: String,
    lock: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl Place {
    /// Waits until it is this place's turn.
    async fn turn(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        match &self.lock {
            Some(lock) => Some(lock.clone().lock_owned().await),
            None => None,
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut line = self.turns.0.lock().unwrap_or_else(|e| e.into_inner());
        drop(self.lock.take());
        // only the map holds the lock now: nobody works on or waits for the key
        if line
            .get(&self.key)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            line.remove(&self.key);
        }
    }
}

/// What a render holds while it works, given back in this order: the slot,
/// the turn, then the place in the line (which may forget the key only
/// after the turn is over).
struct Rendering {
    _slot: OwnedSemaphorePermit,
    _turn: Option<tokio::sync::OwnedMutexGuard<()>>,
    _place: Place,
}

/// Cached, isolated conversions (one per server, part of `AppState`).
pub struct Previews {
    dir: PathBuf,
    /// The `teamcraft` binary for isolated conversions; `None` = in-process.
    worker: Option<PathBuf>,
    slots: Semaphore,
    /// Conversions waiting for one of the slots (for `GET /metrics`).
    waiting: AtomicUsize,
    limits: PreviewLimits,
    running: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The slots of [`Previews::render`].
    renders: Arc<Semaphore>,
    /// One render per cache entry at a time.
    turns: Turns,
}

impl Previews {
    pub fn new(vault: &Vault, limits: PreviewLimits) -> Previews {
        // Only the real server binary understands `preview-mesh`; test
        // binaries convert in-process.
        let worker = worker_exe();
        Previews {
            dir: vault.root().join("previews"),
            worker,
            slots: Semaphore::new(MAX_PARALLEL),
            waiting: AtomicUsize::new(0),
            limits,
            running: Mutex::new(HashMap::new()),
            renders: Arc::new(Semaphore::new(MAX_RENDERS)),
            turns: Turns::default(),
        }
    }

    /// Runs CPU-bound `work` on a derived cache entry – drawing a thumbnail,
    /// composing an assembly preview – off the async runtime: at most
    /// [`MAX_RENDERS`] at a time and one at a time per `key` (the path of
    /// the entry). `work` must look in the cache before it does anything, so
    /// requests that waited for the same `key` answer from what the first
    /// one stored instead of doing it again. A request that only reads the
    /// cache does not come here and never waits for a slot.
    ///
    /// The slot and the turn stay taken until `work` returns, also when the
    /// request that started it is gone: a blocking thread cannot be stopped,
    /// and its work is still running.
    pub async fn render<T, F>(&self, key: &str, work: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let place = self.turns.place(key);
        let turn = place.turn().await;
        let slot = self
            .renders
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("render queue closed: {e}")))?;
        let held = Rendering {
            _slot: slot,
            _turn: turn,
            _place: place,
        };
        tokio::task::spawn_blocking(move || {
            let _held = held;
            work()
        })
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("render task failed: {e}")))
    }

    /// The cache directory (`<vault>/previews`), also of the thumbnails.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The preview queue: conversions running now and conversions waiting
    /// for a free slot (at most [`MAX_PARALLEL`] run at a time).
    pub fn queue(&self) -> (usize, usize) {
        let running = MAX_PARALLEL.saturating_sub(self.slots.available_permits());
        (running, self.waiting.load(Ordering::Relaxed))
    }

    /// Waits for a conversion slot; counted in [`Previews::queue`] while
    /// waiting, also when the waiting request is dropped.
    async fn slot(&self) -> AppResult<SemaphorePermit<'_>> {
        struct Waiting<'a>(&'a AtomicUsize);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        self.waiting.fetch_add(1, Ordering::Relaxed);
        let _waiting = Waiting(&self.waiting);
        self.slots
            .acquire()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("preview queue closed: {e}")))
    }

    fn paths(&self, sha256: &str) -> AppResult<(PathBuf, PathBuf)> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("Invalid content hash.".into()));
        }
        let base = self.dir.join(&sha256[..2]);
        Ok((
            base.join(format!("{sha256}.v{CACHE_VERSION}.tcm")),
            base.join(format!("{sha256}.v{CACHE_VERSION}.err")),
        ))
    }

    /// Where the composed preview of an assembly is cached: next to the
    /// blob's own preview, named by the components it was built from
    /// (`key`, hex), so a changed component gives a new file.
    pub fn assembly_path(&self, sha256: &str, key: &str) -> AppResult<PathBuf> {
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("Invalid assembly key.".into()));
        }
        let (mesh, _) = self.paths(sha256)?;
        Ok(mesh.with_file_name(format!("{sha256}.asm-{key}.v{CACHE_VERSION}.tcm")))
    }

    /// The cached shape signature (or the reason why there is none).
    fn signature_path(&self, sha256: &str) -> AppResult<PathBuf> {
        let (mesh, _) = self.paths(sha256)?;
        Ok(mesh.with_extension(format!("s{}.json", shape::VERSION)))
    }

    /// The cached mass properties (or the reason why there are none).
    fn mass_path(&self, sha256: &str) -> AppResult<PathBuf> {
        let (mesh, _) = self.paths(sha256)?;
        Ok(mesh.with_extension(format!("m{}.json", massprops::VERSION)))
    }

    /// The mass properties of a blob per unit density in model units (see
    /// `massprops`): from the cache, or computed from the cached preview
    /// mesh in the isolated preview process (converting the file first if
    /// needed). Failures are cached like failed previews.
    pub async fn mass_geometry(
        &self,
        vault: &Vault,
        sha256: &str,
        format: PreviewFormat,
    ) -> AppResult<massprops::Geometry> {
        if format.is_point_cloud() || format.is_result() {
            return Err(AppError::BadRequest(
                "Point clouds and result files have no volume.".into(),
            ));
        }
        let path = self.mass_path(sha256)?;
        if let Some(found) = read_mass(&path).await {
            return found;
        }
        let mesh_path = self.mesh(vault, sha256, format).await?;
        self.exclusive(format!("{sha256}.mass"), async {
            if let Some(found) = read_mass(&path).await {
                return found;
            }
            let _slot = self.slot().await?;
            let dir = path.parent().unwrap_or(&self.dir);
            let tmp = dir.join(format!(
                "{sha256}.{}.mass.tmp",
                uuid::Uuid::new_v4().simple()
            ));
            let job = Job {
                format: CACHED_MESH,
                input: mesh_path.clone(),
                output: None,
                signature: None,
                mass: Some(tmp.clone()),
                time_limit: Duration::from_secs(60),
                max_input_bytes: u64::MAX,
                memory_bytes: self.limits.max_memory_bytes,
            };
            let outcome = match &self.worker {
                Some(exe) => run_worker(exe, &job).await,
                None => run_in_process(&job).await,
            };
            match outcome {
                Ok(()) => {
                    tokio::fs::rename(&tmp, &path).await.map_err(|e| {
                        AppError::Other(anyhow::anyhow!("cannot store the mass properties: {e}"))
                    })?;
                }
                Err(message) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    let failed = serde_json::to_vec(&massprops::Outcome::Error(message.clone()))
                        .unwrap_or_default();
                    let _ = tokio::fs::write(&path, failed).await;
                    return Err(AppError::BadRequest(message));
                }
            }
            read_mass(&path).await.unwrap_or_else(|| {
                Err(AppError::Other(anyhow::anyhow!(
                    "the mass properties could not be read back"
                )))
            })
        })
        .await
    }

    /// Whether the preview mesh of a blob is already cached.
    pub async fn has_mesh(&self, sha256: &str) -> bool {
        match self.paths(sha256) {
            Ok((mesh, _)) => tokio::fs::metadata(mesh).await.is_ok(),
            Err(_) => false,
        }
    }

    /// Whether the conversion of a blob is behind us: its preview mesh or
    /// the reason why there is none is cached.
    pub async fn is_converted(&self, sha256: &str) -> bool {
        match self.paths(sha256) {
            Ok((mesh, err)) => {
                tokio::fs::metadata(mesh).await.is_ok() || tokio::fs::metadata(err).await.is_ok()
            }
            Err(_) => false,
        }
    }

    /// One conversion or signature per key at a time, however many wait.
    async fn exclusive<T, F>(&self, key: String, job: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let lock = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.entry(key.clone()).or_default().clone()
        };
        let guard = lock.lock().await;
        let result = job.await;
        drop(guard);
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        // only the last waiter removes the entry
        if running.get(&key).is_some_and(|l| Arc::strong_count(l) <= 2) {
            running.remove(&key);
        }
        result
    }

    /// The shape signature of a blob (see `shape`): from the cache, written
    /// by the conversion, or computed from the cached preview mesh in the
    /// isolated preview process. Failures are cached like failed previews.
    pub async fn signature(
        &self,
        vault: &Vault,
        sha256: &str,
        format: PreviewFormat,
    ) -> AppResult<shape::Signature> {
        if format.is_point_cloud() {
            return Err(AppError::BadRequest(
                "Point clouds have no shape signature.".into(),
            ));
        }
        let sig_path = self.signature_path(sha256)?;
        if let Some(found) = read_signature(&sig_path).await {
            return found;
        }
        let mesh_path = self.mesh(vault, sha256, format).await?;
        self.exclusive(format!("{sha256}.signature"), async {
            if let Some(found) = read_signature(&sig_path).await {
                return found;
            }
            let _slot = self.slot().await?;
            let dir = sig_path.parent().unwrap_or(&self.dir);
            let tmp = dir.join(format!(
                "{sha256}.{}.sig.tmp",
                uuid::Uuid::new_v4().simple()
            ));
            let job = Job {
                format: CACHED_MESH,
                input: mesh_path.clone(),
                output: None,
                signature: Some(tmp.clone()),
                mass: None,
                time_limit: Duration::from_secs(60),
                max_input_bytes: u64::MAX,
                memory_bytes: self.limits.max_memory_bytes,
            };
            let outcome = match &self.worker {
                Some(exe) => run_worker(exe, &job).await,
                None => run_in_process(&job).await,
            };
            match outcome {
                Ok(()) => {
                    tokio::fs::rename(&tmp, &sig_path).await.map_err(|e| {
                        AppError::Other(anyhow::anyhow!("cannot store the shape signature: {e}"))
                    })?;
                }
                Err(message) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    let failed = serde_json::to_vec(&shape::Outcome::Error(message.clone()))
                        .unwrap_or_default();
                    let _ = tokio::fs::write(&sig_path, failed).await;
                    return Err(AppError::BadRequest(message));
                }
            }
            read_signature(&sig_path).await.unwrap_or_else(|| {
                Err(AppError::Other(anyhow::anyhow!(
                    "the shape signature could not be read back"
                )))
            })
        })
        .await
    }

    /// The cached mesh of a blob, converting it first if necessary.
    pub async fn mesh(
        &self,
        vault: &Vault,
        sha256: &str,
        format: PreviewFormat,
    ) -> AppResult<PathBuf> {
        let (mesh_path, err_path) = self.paths(sha256)?;
        self.check_size(vault, sha256, format, &err_path).await?;
        if let Some(found) = cached(&mesh_path, &err_path).await? {
            return Ok(found);
        }
        // one conversion per blob, however many viewers wait for it
        let lock = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.entry(sha256.to_string()).or_default().clone()
        };
        let guard = lock.lock().await;
        let result = match cached(&mesh_path, &err_path).await {
            Ok(Some(found)) => Ok(found),
            Err(e) => Err(e),
            Ok(None) => self
                .convert(vault, sha256, format, &mesh_path, &err_path)
                .await
                .map(|_| mesh_path.clone()),
        };
        drop(guard);
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sha256);
        result
    }

    /// A file above the size cap of its kind is refused before anything opens
    /// it; the refusal is cached like a failed conversion. The cap is checked
    /// on every call (one `stat`), so raising it takes effect at once.
    async fn check_size(
        &self,
        vault: &Vault,
        sha256: &str,
        format: PreviewFormat,
        err_path: &Path,
    ) -> AppResult<()> {
        let Ok(size) = tokio::fs::metadata(vault.object_path(sha256)?)
            .await
            .map(|m| m.len())
        else {
            return Ok(());
        };
        let limit = self.limits.max_bytes(format);
        if size > limit {
            let message = too_large_message(size, limit);
            if let Some(dir) = err_path.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            let _ = tokio::fs::write(err_path, &message).await;
            return Err(AppError::BadRequest(message));
        }
        // a refusal from before the cap was raised
        if let Ok(old) = tokio::fs::read_to_string(err_path).await {
            if old.starts_with(TOO_LARGE) {
                let _ = tokio::fs::remove_file(err_path).await;
            }
        }
        Ok(())
    }

    /// Starts the conversion in the background (after an upload), so the
    /// first viewer does not have to wait.
    pub fn warm(self: &Arc<Self>, vault: &Vault, sha256: &str, format: PreviewFormat) {
        let previews = self.clone();
        let vault = vault.clone();
        let sha256 = sha256.to_string();
        tokio::spawn(async move {
            if let Err(e) = previews.mesh(&vault, &sha256, format).await {
                tracing::debug!(sha256, error = %e.public_message(), "preview not generated");
            }
        });
    }

    async fn convert(
        &self,
        vault: &Vault,
        sha256: &str,
        format: PreviewFormat,
        mesh_path: &Path,
        err_path: &Path,
    ) -> AppResult<()> {
        let input = vault.object_path(sha256)?;
        if !vault.exists(sha256).await {
            return Err(AppError::not_found(
                "The file content is missing in the vault.",
            ));
        }
        let dir = mesh_path.parent().unwrap_or(&self.dir).to_path_buf();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            AppError::Other(anyhow::anyhow!("cannot create {}: {e}", dir.display()))
        })?;
        let _slot = self.slot().await?;
        let tmp = dir.join(format!("{sha256}.{}.tmp", uuid::Uuid::new_v4().simple()));
        // the shape signature comes with the conversion, from the same mesh
        let sig_tmp = tmp.with_extension("sig.tmp");
        let job = Job {
            format: format.as_str(),
            input,
            output: Some(tmp.clone()),
            signature: (!format.is_point_cloud()).then(|| sig_tmp.clone()),
            mass: None,
            time_limit: format.time_limit(),
            max_input_bytes: self.limits.max_bytes(format),
            memory_bytes: self.limits.max_memory_bytes,
        };
        let started = std::time::Instant::now();
        let outcome = match &self.worker {
            Some(exe) => run_worker(exe, &job).await,
            None => run_in_process(&job).await,
        };
        match outcome {
            Ok(()) => {
                if job.signature.is_some() {
                    let sig_path = self.signature_path(sha256)?;
                    if tokio::fs::rename(&sig_tmp, &sig_path).await.is_err() {
                        let _ = tokio::fs::remove_file(&sig_tmp).await;
                    }
                }
                tokio::fs::rename(&tmp, mesh_path).await.map_err(|e| {
                    AppError::Other(anyhow::anyhow!("cannot store the preview: {e}"))
                })?;
                tracing::info!(
                    sha256,
                    format = format.as_str(),
                    ms = started.elapsed().as_millis() as u64,
                    "preview generated"
                );
                Ok(())
            }
            Err(message) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                let _ = tokio::fs::remove_file(&sig_tmp).await;
                let _ = tokio::fs::write(err_path, &message).await;
                tracing::info!(sha256, format = format.as_str(), %message, "no preview");
                Err(AppError::BadRequest(message))
            }
        }
    }
}

/// The cached mesh, or the cached reason why there is none.
async fn cached(mesh_path: &Path, err_path: &Path) -> AppResult<Option<PathBuf>> {
    if tokio::fs::metadata(mesh_path).await.is_ok() {
        return Ok(Some(mesh_path.to_path_buf()));
    }
    match tokio::fs::read_to_string(err_path).await {
        Ok(message) => Err(AppError::BadRequest(message)),
        Err(_) => Ok(None),
    }
}

/// The cached shape signature: `None` if not computed yet (or unreadable).
async fn read_signature(path: &Path) -> Option<AppResult<shape::Signature>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    match serde_json::from_slice::<shape::Outcome>(&bytes).ok()? {
        shape::Outcome::Signature(s) if s.is_valid() => Some(Ok(s)),
        shape::Outcome::Signature(_) => None,
        shape::Outcome::Error(message) => Some(Err(AppError::BadRequest(message))),
    }
}

/// The cached mass properties: `None` if not computed yet (or unreadable).
async fn read_mass(path: &Path) -> Option<AppResult<massprops::Geometry>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    match serde_json::from_slice::<massprops::Outcome>(&bytes).ok()? {
        massprops::Outcome::Geometry(g) if g.is_valid() => Some(Ok(g)),
        massprops::Outcome::Geometry(_) => None,
        massprops::Outcome::Error(message) => Some(Err(AppError::BadRequest(message))),
    }
}

/// One run of the preview process (see [`run_job`]).
struct Job {
    format: &'static str,
    input: PathBuf,
    output: Option<PathBuf>,
    signature: Option<PathBuf>,
    mass: Option<PathBuf>,
    time_limit: Duration,
    /// Largest input the process opens (the process checks it again).
    max_input_bytes: u64,
    /// Address space of the process, 0 = unlimited.
    memory_bytes: u64,
}

/// `teamcraft preview-mesh` in a child process that is killed at the time limit.
async fn run_worker(exe: &Path, job: &Job) -> Result<(), String> {
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("preview-mesh")
        .arg("--format")
        .arg(job.format)
        .arg("--input")
        .arg(&job.input)
        .arg("--max-input-bytes")
        .arg(job.max_input_bytes.to_string())
        .arg("--max-memory-bytes")
        .arg(job.memory_bytes.to_string());
    if let Some(output) = &job.output {
        command.arg("--output").arg(output);
    }
    if let Some(signature) = &job.signature {
        command.arg("--signature").arg(signature);
    }
    if let Some(mass) = &job.mass {
        command.arg("--mass-properties").arg(mass);
    }
    let child = command
        .env_clear()
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("The preview converter could not be started: {e}"))?;
    match tokio::time::timeout(job.time_limit, child.wait_with_output()).await {
        Err(_) => Err(format!(
            "The model is too complex to preview (no result within {} s) – download it instead.",
            job.time_limit.as_secs()
        )),
        Ok(Err(e)) => Err(format!("The preview converter failed: {e}")),
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => {
            #[cfg(unix)]
            let aborted = std::os::unix::process::ExitStatusExt::signal(&out.status) == Some(6);
            #[cfg(not(unix))]
            let aborted = false;
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(describe_failure(aborted, &stderr, job.memory_bytes))
        }
    }
}

/// The job on a blocking thread (tests and unusual installations).
async fn run_in_process(job: &Job) -> Result<(), String> {
    let (format, input, output, signature, mass, max_input) = (
        job.format,
        job.input.clone(),
        job.output.clone(),
        job.signature.clone(),
        job.mass.clone(),
        job.max_input_bytes,
    );
    let task = tokio::task::spawn_blocking(move || -> Result<(), String> {
        run_job(
            format,
            &input,
            output.as_deref(),
            signature.as_deref(),
            mass.as_deref(),
            max_input,
        )
        .map(drop)
        .map_err(|e| e.to_string())
    });
    match tokio::time::timeout(job.time_limit, task).await {
        Err(_) => Err(format!(
            "The model is too complex to preview (no result within {} s).",
            job.time_limit.as_secs()
        )),
        Ok(Err(e)) => Err(format!("The preview converter failed: {e}")),
        Ok(Ok(result)) => result,
    }
}

/// What a file in the preview cache (`previews/ab/…`) is, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheEntry<'a> {
    /// A cache file of the current versions, for the blob with this SHA-256.
    Current(&'a str),
    /// A cache file of an older cache, mesh or thumbnail version.
    Stale,
    /// A file being written (or left behind by an interrupted conversion).
    Temporary,
    /// Not a name the cache writes: left alone.
    Unknown,
}

/// Classifies a file name of the preview cache: the meshes and failures of
/// this module (`<sha256>.v<N>.tcm|err`), their shape signatures
/// (`<sha256>.v<N>.s<M>.json`) and the thumbnails of [`crate::thumbnail`]
/// (`<sha256>.thumb.v<N>.png|bmp|jpg|none`); every temporary name ends in
/// `.tmp`.
pub fn cache_entry(name: &str) -> CacheEntry<'_> {
    let Some((sha256, rest)) = name.split_at_checked(64) else {
        return CacheEntry::Unknown;
    };
    let Some(rest) = rest.strip_prefix('.') else {
        return CacheEntry::Unknown;
    };
    if !sha256
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return CacheEntry::Unknown;
    }
    if rest.ends_with(".tmp") {
        return CacheEntry::Temporary;
    }
    // composed assembly previews: `asm-<hex key>.v<N>.tcm`
    if let Some((key, tail)) = rest.strip_prefix("asm-").and_then(|r| r.split_once('.')) {
        if !key.is_empty() && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            if let Some(n) = tail.strip_prefix('v').and_then(|t| t.strip_suffix(".tcm")) {
                if n == CACHE_VERSION.to_string() {
                    return CacheEntry::Current(sha256);
                }
                if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
                    return CacheEntry::Stale;
                }
            }
        }
    }
    let mesh = format!("v{CACHE_VERSION}.");
    let thumb = format!("thumb.v{}.", crate::thumbnail::CACHE_VERSION);
    let current = match rest.strip_prefix(&mesh) {
        Some(kind) => {
            kind == "tcm"
                || kind == "err"
                || kind == format!("s{}.json", shape::VERSION)
                || kind == format!("m{}.json", massprops::VERSION)
        }
        None => rest
            .strip_prefix(&thumb)
            .is_some_and(|ext| matches!(ext, "png" | "bmp" | "jpg" | "none")),
    };
    if current {
        return CacheEntry::Current(sha256);
    }
    // an older version of one of the names above
    let version = |s: &str| {
        s.strip_prefix('v')
            .and_then(|s| s.split_once('.'))
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if version(rest) || rest.strip_prefix("thumb.").is_some_and(version) {
        CacheEntry::Stale
    } else {
        CacheEntry::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_previews_get_no_shape_signature() {
        let mut mesh = crate::mesh::Mesh {
            positions: vec![0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 3.0, 0.0],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        assert!(matches!(
            signature_outcome(&mesh),
            shape::Outcome::Signature(_)
        ));
        mesh.faces = Some((5, 6));
        match signature_outcome(&mesh) {
            shape::Outcome::Error(message) => assert!(message.contains("5 of 6 faces")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn failures_of_the_conversion_process_are_explained() {
        let limit = 1024 * MIB;
        let m = describe_failure(true, "", limit);
        assert!(m.contains("more memory than allowed (1024 MB)"), "{m}");
        let m = describe_failure(false, "memory allocation of 99 bytes failed", limit);
        assert!(m.contains("more memory"), "{m}");
        // without a limit an abort is not blamed on it
        let m = describe_failure(true, "Error: broken file\n", 0);
        assert_eq!(m, "broken file");
        assert_eq!(
            describe_failure(false, "x\nError: no triangles\n\n", limit),
            "no triangles"
        );
        assert_eq!(
            describe_failure(false, "", limit),
            "the converter stopped unexpectedly"
        );
    }

    #[test]
    fn caps_by_kind() {
        let l = PreviewLimits::default();
        assert_eq!(l.max_bytes(PreviewFormat::Vtu), 64 * MIB);
        assert_eq!(l.max_bytes(PreviewFormat::Frd), 64 * MIB);
        assert_eq!(l.max_bytes(PreviewFormat::Stl), 256 * MIB);
        assert_eq!(l.max_bytes(PreviewFormat::Las), 256 * MIB);
        assert_eq!(l.max_bytes(PreviewFormat::Step), 256 * MIB);
        let dir = std::env::temp_dir().join(format!("tc-cap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.vtu");
        std::fs::File::create(&file)
            .unwrap()
            .set_len(5 * MIB)
            .unwrap();
        let e = convert_within(PreviewFormat::Vtu, &file, MIB).unwrap_err();
        assert!(e.to_string().contains("too large for a 3D preview"), "{e}");
        assert!(e.to_string().contains("5 MB, at most 1 MB"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn formats_by_extension() {
        assert_eq!(PreviewFormat::of("bracket.STL"), Some(PreviewFormat::Stl));
        assert_eq!(PreviewFormat::of("engine.step"), Some(PreviewFormat::Step));
        assert_eq!(
            PreviewFormat::of("engine.v2.stp"),
            Some(PreviewFormat::Step)
        );
        assert_eq!(PreviewFormat::of("engine.p21"), Some(PreviewFormat::Step));
        assert_eq!(PreviewFormat::of("print.3mf"), Some(PreviewFormat::ThreeMf));
        assert_eq!(PreviewFormat::of("scene.GLB"), Some(PreviewFormat::Glb));
        assert_eq!(PreviewFormat::of("scan.las"), Some(PreviewFormat::Las));
        assert_eq!(
            PreviewFormat::of("Bracket.SLDPRT"),
            Some(PreviewFormat::SwPart)
        );
        assert_eq!(
            PreviewFormat::of("Rover.sldasm"),
            Some(PreviewFormat::SwAssembly)
        );
        assert_eq!(
            PreviewFormat::of("Bracket.ipt"),
            Some(PreviewFormat::InvPart)
        );
        assert_eq!(
            PreviewFormat::of("Pump.IAM"),
            Some(PreviewFormat::InvAssembly)
        );
        assert_eq!(PreviewFormat::of("Bracket.par"), None, "native CAD");
        assert_eq!(
            PreviewFormat::of("plate.iges"),
            None,
            "no B-rep reader for IGES"
        );
        assert_eq!(PreviewFormat::of("drawing.pdf"), None);
        assert_eq!(PreviewFormat::of("stl"), None);
        assert_eq!(PreviewFormat::of(".stl"), None);
        assert!(PreviewFormat::Step.is_brep() && !PreviewFormat::Stl.is_brep());
        assert!(PreviewFormat::Las.is_point_cloud());
        assert_eq!(PreviewFormat::of("chair.3dm"), Some(PreviewFormat::Rhino));
        assert_eq!(PreviewFormat::of("beam.vtu"), Some(PreviewFormat::Vtu));
        assert_eq!(
            PreviewFormat::of("beam.VTK"),
            Some(PreviewFormat::VtkLegacy)
        );
        assert_eq!(PreviewFormat::of("skin.vtp"), Some(PreviewFormat::Vtp));
        assert_eq!(PreviewFormat::of("job.frd"), Some(PreviewFormat::Frd));
        assert_eq!(PreviewFormat::of("grid.vts"), None, "structured grids");
        assert!(PreviewFormat::Frd.is_result() && !PreviewFormat::Stl.is_result());
        for p in PreviewFormat::ALL {
            assert_eq!(PreviewFormat::of_format(p.as_str()), Some(p));
            assert!(
                crate::cadformat::by_id(p.as_str()).is_some(),
                "{} is a registered format",
                p.as_str()
            );
        }
    }

    #[tokio::test]
    async fn conversions_are_cached_including_failures() {
        let root = std::env::temp_dir().join(format!("teamcraft-preview-{}", uuid::Uuid::new_v4()));
        let vault = Vault::open(&root).await.unwrap();
        let previews = Previews::new(&vault, PreviewLimits::default());
        assert!(previews.worker.is_none(), "tests convert in-process");
        let ok = vault
            .store_bytes(b"solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n")
            .await
            .unwrap();
        let path = previews
            .mesh(&vault, &ok.sha256, PreviewFormat::Stl)
            .await
            .unwrap();
        let mesh = crate::mesh::Mesh::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        // second call: from the cache
        assert_eq!(
            previews
                .mesh(&vault, &ok.sha256, PreviewFormat::Stl)
                .await
                .unwrap(),
            path
        );
        let bad = vault.store_bytes(b"this is not a mesh").await.unwrap();
        let first = previews.mesh(&vault, &bad.sha256, PreviewFormat::Stl).await;
        assert!(matches!(first, Err(AppError::BadRequest(_))));
        let (_, err_path) = previews.paths(&bad.sha256).unwrap();
        assert!(err_path.exists(), "the failure is remembered");
        assert!(previews
            .mesh(&vault, &bad.sha256, PreviewFormat::Stl)
            .await
            .is_err());
        assert!(previews.paths("../../etc/passwd").is_err());

        // The conversion wrote the shape signature as well.
        let sig_path = previews.signature_path(&ok.sha256).unwrap();
        assert!(sig_path.exists(), "signature written with the preview");
        let sig = previews
            .signature(&vault, &ok.sha256, PreviewFormat::Stl)
            .await
            .unwrap();
        assert_eq!(sig.triangles, 1);
        assert!(!sig.closed);
        // Previews from before signatures: computed from the cached mesh.
        std::fs::remove_file(&sig_path).unwrap();
        assert!(previews.has_mesh(&ok.sha256).await);
        // converted: a mesh or a cached failure, not yet: neither
        assert!(previews.is_converted(&ok.sha256).await);
        assert!(previews.is_converted(&bad.sha256).await);
        assert!(!previews.is_converted(&"0".repeat(64)).await);
        let again = previews
            .signature(&vault, &ok.sha256, PreviewFormat::Stl)
            .await
            .unwrap();
        assert_eq!(again, sig);
        assert!(previews
            .signature(&vault, &bad.sha256, PreviewFormat::Stl)
            .await
            .is_err());
        assert!(previews
            .signature(&vault, &ok.sha256, PreviewFormat::Xyz)
            .await
            .is_err());
        // A broken cached mesh is an error, not a panic.
        let broken = root.join("broken.tcm");
        std::fs::write(&broken, b"TCMESH01\xff\xff\xff\xff").unwrap();
        assert!(run_job(
            CACHED_MESH,
            &broken,
            None,
            Some(&root.join("x.json")),
            None,
            u64::MAX
        )
        .is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cache_names_are_classified() {
        let sha = "ab".repeat(32);
        let previews = Previews {
            dir: PathBuf::from("/vault/previews"),
            worker: None,
            slots: Semaphore::new(1),
            waiting: AtomicUsize::new(0),
            limits: PreviewLimits::default(),
            running: Mutex::new(HashMap::new()),
            renders: Arc::new(Semaphore::new(1)),
            turns: Turns::default(),
        };
        let (mesh, err) = previews.paths(&sha).unwrap();
        let asm = previews.assembly_path(&sha, "0123abcd").unwrap();
        let sig = previews.signature_path(&sha).unwrap();
        let mass = previews.mass_path(&sha).unwrap();
        for path in [&mesh, &err, &sig, &asm, &mass] {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert_eq!(cache_entry(name), CacheEntry::Current(&sha), "{name}");
        }
        for ext in ["png", "bmp", "jpg", "none"] {
            let name = format!("{sha}.thumb.v{}.{ext}", crate::thumbnail::CACHE_VERSION);
            assert_eq!(cache_entry(&name), CacheEntry::Current(&sha), "{name}");
        }
        let old = CACHE_VERSION + 7;
        for name in [
            format!("{sha}.v{old}.tcm"),
            format!("{sha}.v{old}.err"),
            format!("{sha}.v{CACHE_VERSION}.s{}.json", shape::VERSION + 7),
            format!("{sha}.v{CACHE_VERSION}.m{}.json", massprops::VERSION + 7),
            format!("{sha}.thumb.v{old}.png"),
            format!("{sha}.asm-0123abcd.v{old}.tcm"),
        ] {
            assert_eq!(cache_entry(&name), CacheEntry::Stale, "{name}");
        }
        for name in [
            format!("{sha}.0123abcd.tmp"),
            format!("{sha}.0123abcd.sig.tmp"),
            format!("{sha}.thumb.v{CACHE_VERSION}.png.4711.tmp"),
        ] {
            assert_eq!(cache_entry(&name), CacheEntry::Temporary, "{name}");
        }
        for name in [
            "README".to_string(),
            format!("{}.v2.tcm", "AB".repeat(32)),
            format!("{sha}v2.tcm"),
            format!("{sha}.notes.txt"),
            format!("{}.v2.tcm", "ab".repeat(31)),
            "é".repeat(40),
        ] {
            assert_eq!(cache_entry(&name), CacheEntry::Unknown, "{name}");
        }
    }

    #[tokio::test]
    async fn the_queue_counts_running_and_waiting_conversions() {
        let root = std::env::temp_dir().join(format!("teamcraft-preview-{}", uuid::Uuid::new_v4()));
        let vault = Vault::open(&root).await.unwrap();
        let previews = Arc::new(Previews::new(&vault, PreviewLimits::default()));
        assert_eq!(previews.queue(), (0, 0));
        let held: Vec<_> = take_every_slot(&previews).await;
        assert_eq!(previews.queue(), (MAX_PARALLEL, 0));
        let waiter = {
            let previews = previews.clone();
            tokio::spawn(async move {
                let _slot = previews.slot().await.unwrap();
            })
        };
        while previews.queue().1 == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(previews.queue(), (MAX_PARALLEL, 1));
        // a dropped waiter leaves the queue as well
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(previews.queue(), (MAX_PARALLEL, 0));
        drop(held);
        assert_eq!(previews.queue(), (0, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn renders_are_bounded_and_done_once_per_key() {
        let root = std::env::temp_dir().join(format!("teamcraft-preview-{}", uuid::Uuid::new_v4()));
        let vault = Vault::open(&root).await.unwrap();
        let previews = Arc::new(Previews::new(&vault, PreviewLimits::default()));
        let cache = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
        let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut requests = Vec::new();
        // three requests for each of six entries, all at once
        for n in 0..18 {
            let (previews, cache) = (previews.clone(), cache.clone());
            let (running, peak) = (running.clone(), peak.clone());
            requests.push(tokio::spawn(async move {
                let key = format!("entry-{}", n % 6);
                let work_key = key.clone();
                previews
                    .render(&key, move || {
                        if cache.lock().unwrap().contains(&work_key) {
                            return false;
                        }
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        cache.lock().unwrap().insert(work_key);
                        running.fetch_sub(1, Ordering::SeqCst);
                        true
                    })
                    .await
                    .unwrap()
            }));
        }
        let mut rendered = 0;
        for request in requests {
            rendered += usize::from(request.await.unwrap());
        }
        // one render per entry, the other requests found it in the cache
        assert_eq!(rendered, 6);
        let peak = peak.load(Ordering::SeqCst);
        assert!((1..=MAX_RENDERS).contains(&peak), "{peak} at once");
        assert_eq!(previews.renders.available_permits(), MAX_RENDERS);
        assert_eq!(previews.turns.len(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_render_keeps_its_slot_and_turn_when_the_request_is_gone() {
        let root = std::env::temp_dir().join(format!("teamcraft-preview-{}", uuid::Uuid::new_v4()));
        let vault = Vault::open(&root).await.unwrap();
        let previews = Arc::new(Previews::new(&vault, PreviewLimits::default()));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel::<()>();
        let request = {
            let previews = previews.clone();
            tokio::spawn(async move {
                previews
                    .render("entry", move || {
                        started_tx.send(()).unwrap();
                        finish_rx.recv().unwrap();
                    })
                    .await
            })
        };
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        // a second request for the entry waits in line …
        let waiter = {
            let previews = previews.clone();
            tokio::spawn(async move { previews.render("entry", || ()).await })
        };
        request.abort();
        let _ = request.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // … and the work goes on without its request: slot and turn are taken
        assert_eq!(previews.renders.available_permits(), MAX_RENDERS - 1);
        assert_eq!(previews.turns.len(), 1);
        assert!(!waiter.is_finished());
        // a waiter that goes away leaves the line, the entry stays known
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(previews.turns.len(), 1);
        finish_tx.send(()).unwrap();
        for _ in 0..200 {
            if previews.turns.len() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(previews.renders.available_permits(), MAX_RENDERS);
        assert_eq!(previews.turns.len(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    async fn take_every_slot(previews: &Previews) -> Vec<SemaphorePermit<'_>> {
        let mut held = Vec::new();
        for _ in 0..MAX_PARALLEL {
            held.push(previews.slot().await.unwrap());
        }
        held
    }
}
