//! 3D preview of CAD files – pure Rust, no browser plug-ins or downloads.
//!
//! The server converts a file once into the compact mesh of [`crate::mesh`]
//! and caches it next to the vault (`previews/ab/<sha256>.v<N>.tcm`); the
//! browser (`static/viewer.js`) and the desktop app only draw it. Meshes,
//! point clouds and STEP B-reps (tessellated with the `truck` kernel) are
//! supported, see [`PreviewFormat`].
//!
//! The conversion never blocks the web server: it runs in a separate
//! `teamcraft preview-mesh` process with a time limit (in tests, where there is
//! no such binary, on a blocking thread), at most [`MAX_PARALLEL`] at a time,
//! and concurrent requests for the same file share one conversion. Failures
//! are cached as well, so a broken file is not converted again and again.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::Semaphore;

use crate::{
    error::{AppError, AppResult},
    shape,
    vault::Vault,
};

/// Bump to invalidate every cached preview (reader fixes, new format).
/// Bump `shape::VERSION` with it: the shape signatures stored in the
/// database are computed from these meshes.
pub const CACHE_VERSION: u32 = 2;

/// Conversions that run at the same time.
pub const MAX_PARALLEL: usize = 2;

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
    Pts,
    Xyz,
    Las,
}

impl PreviewFormat {
    pub const ALL: [PreviewFormat; 13] = [
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
        PreviewFormat::Pts,
        PreviewFormat::Xyz,
        PreviewFormat::Las,
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
            PreviewFormat::Pts => "pts",
            PreviewFormat::Xyz => "xyz",
            PreviewFormat::Las => "las",
        }
    }

    /// B-rep formats are tessellated (slower) – the rest is read as is.
    pub fn is_brep(self) -> bool {
        matches!(self, PreviewFormat::Step)
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
    // a STEP file above the limit is not read into memory just to be refused
    let size = std::fs::metadata(input)?.len();
    let limit = crate::mesh::MAX_STEP_BYTES as u64;
    if format == PreviewFormat::Step && size > limit {
        anyhow::bail!(
            "The STEP file is too large to preview ({} MB, at most {} MB) – download it instead.",
            size / (1024 * 1024),
            limit / (1024 * 1024)
        );
    }
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
/// signature does not fail the conversion – the outcome records why.
/// Returns the mesh (the CLI prints its size).
pub fn run_job(
    format: &str,
    input: &Path,
    output: Option<&Path>,
    signature: Option<&Path>,
) -> anyhow::Result<crate::mesh::Mesh> {
    let mesh = if format == CACHED_MESH {
        let mesh = crate::mesh::Mesh::decode(&std::fs::read(input)?)?;
        mesh.check()?;
        mesh
    } else {
        let format = PreviewFormat::of_format(format)
            .ok_or_else(|| anyhow::anyhow!("no 3D preview for the format '{format}'"))?;
        convert(format, input)?
    };
    if let Some(output) = output {
        std::fs::write(output, mesh.encode())?;
    }
    if let Some(path) = signature {
        std::fs::write(path, serde_json::to_vec(&signature_outcome(&mesh))?)?;
    }
    Ok(mesh)
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

/// Cached, isolated conversions (one per server, part of `AppState`).
pub struct Previews {
    dir: PathBuf,
    /// The `teamcraft` binary for isolated conversions; `None` = in-process.
    worker: Option<PathBuf>,
    slots: Semaphore,
    running: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Previews {
    pub fn new(vault: &Vault) -> Previews {
        // Only the real server binary understands `preview-mesh`; test
        // binaries convert in-process.
        let worker = std::env::current_exe().ok().filter(|exe| {
            exe.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == "teamcraft")
        });
        Previews {
            dir: vault.root().join("previews"),
            worker,
            slots: Semaphore::new(MAX_PARALLEL),
            running: Mutex::new(HashMap::new()),
        }
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

    /// The cached shape signature (or the reason why there is none).
    fn signature_path(&self, sha256: &str) -> AppResult<PathBuf> {
        let (mesh, _) = self.paths(sha256)?;
        Ok(mesh.with_extension(format!("s{}.json", shape::VERSION)))
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
            let _slot = self
                .slots
                .acquire()
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("preview queue closed: {e}")))?;
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
                time_limit: Duration::from_secs(60),
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
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("preview queue closed: {e}")))?;
        let tmp = dir.join(format!("{sha256}.{}.tmp", uuid::Uuid::new_v4().simple()));
        // the shape signature comes with the conversion, from the same mesh
        let sig_tmp = tmp.with_extension("sig.tmp");
        let job = Job {
            format: format.as_str(),
            input,
            output: Some(tmp.clone()),
            signature: (!format.is_point_cloud()).then(|| sig_tmp.clone()),
            time_limit: format.time_limit(),
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

/// One run of the preview process (see [`run_job`]).
struct Job {
    format: &'static str,
    input: PathBuf,
    output: Option<PathBuf>,
    signature: Option<PathBuf>,
    time_limit: Duration,
}

/// `teamcraft preview-mesh` in a child process that is killed at the time limit.
async fn run_worker(exe: &Path, job: &Job) -> Result<(), String> {
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("preview-mesh")
        .arg("--format")
        .arg(job.format)
        .arg("--input")
        .arg(&job.input);
    if let Some(output) = &job.output {
        command.arg("--output").arg(output);
    }
    if let Some(signature) = &job.signature {
        command.arg("--signature").arg(signature);
    }
    let child = command
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
            let stderr = String::from_utf8_lossy(&out.stderr);
            let message = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("the converter stopped unexpectedly")
                .trim_start_matches("Error: ")
                .to_string();
            Err(message.chars().take(500).collect())
        }
    }
}

/// The job on a blocking thread (tests and unusual installations).
async fn run_in_process(job: &Job) -> Result<(), String> {
    let (format, input, output, signature) = (
        job.format,
        job.input.clone(),
        job.output.clone(),
        job.signature.clone(),
    );
    let task = tokio::task::spawn_blocking(move || -> Result<(), String> {
        run_job(format, &input, output.as_deref(), signature.as_deref())
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
        assert_eq!(PreviewFormat::of("Bracket.SLDPRT"), None, "native CAD");
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
        let previews = Previews::new(&vault);
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
        assert!(run_job(CACHED_MESH, &broken, None, Some(&root.join("x.json"))).is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
