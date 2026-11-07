//! Content-addressed file vault on the local file system.
//!
//! Layout below the vault root:
//!
//! ```text
//! objects/ab/cd/abcdef0123…   blob named by the SHA-256 of its content
//! tmp/                         uploads in progress (same file system → atomic rename)
//! uploads/<session id>         chunked, resumable uploads (services::uploads)
//! ```
//!
//! Identical files are stored once, no matter how many revisions reference
//! them. Blobs are never modified after they were written. Storing content
//! that is already there renews the blob's modification time, so the time
//! says when the blob was last written or stored again: a blob that the
//! database does not refer to yet (an upload between the vault and its
//! database row) is always younger than the grace period of the removal of
//! unreferenced blobs (`services::retention`).

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
pub struct Vault {
    root: PathBuf,
}

/// Result of a completed upload.
#[derive(Clone, Debug)]
pub struct StoredBlob {
    pub sha256: String,
    pub size: u64,
}

/// A blob found in the vault (see [`Vault::objects_in`]).
#[derive(Clone, Debug)]
pub struct StoredObject {
    pub sha256: String,
    pub size: u64,
    /// When it was written or last stored again.
    pub modified: SystemTime,
}

/// Two lower-case hex digits: a directory level of `objects/`.
fn is_hex_prefix(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Moves a verified file to its content address. When the blob is there
/// already, the file is dropped and the blob's modification time renewed
/// (see the module documentation; the content is not touched). If that
/// fails – the blob was removed a moment ago, or its time cannot be set –
/// the file takes its place.
async fn put_in_place(source: &Path, target: &Path) -> AppResult<()> {
    if fs::try_exists(target).await? {
        let path = target.to_path_buf();
        let renewed = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(SystemTime::now()))
        })
        .await;
        if matches!(renewed, Ok(Ok(()))) {
            fs::remove_file(source).await?;
            return Ok(());
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::rename(source, target).await?;
    Ok(())
}

impl Vault {
    /// Creates the directory layout (if needed) and removes stale temporary files.
    pub async fn open(root: impl Into<PathBuf>) -> std::io::Result<Vault> {
        let vault = Vault { root: root.into() };
        fs::create_dir_all(vault.objects_dir()).await?;
        fs::create_dir_all(vault.tmp_dir()).await?;
        fs::create_dir_all(vault.uploads_dir()).await?;
        vault.clean_tmp(Duration::from_secs(24 * 3600)).await?;
        Ok(vault)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Path of the blob with the given hash. The hash must be validated hex.
    pub fn object_path(&self, sha256: &str) -> AppResult<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("invalid content hash".into()));
        }
        let sha = sha256.to_ascii_lowercase();
        Ok(self
            .objects_dir()
            .join(&sha[0..2])
            .join(&sha[2..4])
            .join(&sha))
    }

    /// Starts a streaming upload limited to `max_bytes`.
    pub async fn begin_upload(&self, max_bytes: u64) -> AppResult<Upload> {
        let tmp_path = self.tmp_dir().join(format!("upload-{}", Uuid::new_v4()));
        let file = fs::File::create(&tmp_path).await?;
        Ok(Upload {
            vault: self.clone(),
            tmp_path,
            file: Some(file),
            hasher: Sha256::new(),
            size: 0,
            max_bytes,
            finished: false,
        })
    }

    /// Stores a complete in-memory buffer (used by tests and small imports).
    pub async fn store_bytes(&self, data: &[u8]) -> AppResult<StoredBlob> {
        let mut upload = self.begin_upload(u64::MAX).await?;
        upload.write(data).await?;
        upload.finish().await
    }

    pub async fn open_blob(&self, sha256: &str) -> AppResult<fs::File> {
        let path = self.object_path(sha256)?;
        fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                tracing::error!(sha256, "blob missing from vault");
                AppError::NotFound(
                    "The file content is missing from the vault. Please contact an administrator."
                        .into(),
                )
            } else {
                AppError::Io(e)
            }
        })
    }

    pub async fn exists(&self, sha256: &str) -> bool {
        match self.object_path(sha256) {
            Ok(p) => fs::try_exists(p).await.unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Re-hashes a blob and compares it with its name.
    pub async fn verify(&self, sha256: &str) -> AppResult<bool> {
        use tokio::io::AsyncReadExt;
        let mut file = self.open_blob(sha256).await?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()) == sha256.to_ascii_lowercase())
    }

    /// Checks that the vault directory is writable (used by `/readyz`).
    pub async fn check_writable(&self) -> AppResult<()> {
        let probe = self.tmp_dir().join(format!(".probe-{}", Uuid::new_v4()));
        fs::write(&probe, b"ok").await?;
        fs::remove_file(&probe).await?;
        Ok(())
    }

    /// Directory of the chunked uploads in progress. It is on the vault's
    /// file system, so a completed upload moves into `objects/` atomically.
    pub fn uploads_dir(&self) -> PathBuf {
        self.root.join("uploads")
    }

    fn staging_path(&self, session: Uuid) -> PathBuf {
        self.uploads_dir().join(session.simple().to_string())
    }

    /// Creates the (empty) staging file of a chunked upload.
    pub async fn create_staging(&self, session: Uuid) -> AppResult<()> {
        fs::File::create(self.staging_path(session)).await?;
        Ok(())
    }

    /// Writes a chunk at `offset` of a staging file and flushes it to disk.
    /// Anything behind `offset` (the rest of an interrupted earlier chunk
    /// that was never confirmed) is cut off first, so the file always
    /// equals the confirmed bytes plus this chunk.
    pub async fn write_staged(&self, session: Uuid, offset: u64, chunk: &[u8]) -> AppResult<()> {
        use tokio::io::AsyncSeekExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(self.staging_path(session))
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    AppError::Conflict(
                        "The data of this upload is gone – start the upload again.".into(),
                    )
                } else {
                    AppError::Io(e)
                }
            })?;
        let len = file.metadata().await?.len();
        if len < offset {
            return Err(AppError::Conflict(
                "The data of this upload is incomplete – start the upload again.".into(),
            ));
        }
        file.set_len(offset).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(chunk).await?;
        file.flush().await?;
        file.sync_data().await?;
        Ok(())
    }

    /// SHA-256 and length of a staging file (read once, from disk – the
    /// chunks may have arrived across server restarts).
    pub async fn hash_staged(&self, session: Uuid) -> AppResult<StoredBlob> {
        use tokio::io::AsyncReadExt;
        let mut file = fs::File::open(self.staging_path(session)).await?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut size = 0u64;
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            size += n as u64;
            hasher.update(&buf[..n]);
        }
        Ok(StoredBlob {
            sha256: hex::encode(hasher.finalize()),
            size,
        })
    }

    /// Moves a verified staging file to its content address (or drops it if
    /// identical content is stored already).
    pub async fn adopt_staged(&self, session: Uuid, sha256: &str) -> AppResult<()> {
        let source = self.staging_path(session);
        let target = self.object_path(sha256)?;
        put_in_place(&source, &target).await
    }

    /// Removes the staging file of a session (no error if there is none).
    pub async fn remove_staged(&self, session: Uuid) -> AppResult<()> {
        match fs::remove_file(self.staging_path(session)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    /// Staging files not changed for `older_than`, with their session id.
    pub async fn stale_staged(&self, older_than: Duration) -> AppResult<Vec<Uuid>> {
        let mut stale = Vec::new();
        let mut entries = fs::read_dir(self.uploads_dir()).await?;
        let now = SystemTime::now();
        while let Some(entry) = entries.next_entry().await? {
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|n| Uuid::try_parse(n).ok())
            else {
                continue;
            };
            let old = entry
                .metadata()
                .await
                .and_then(|m| m.modified())
                .map(|modified| now.duration_since(modified).unwrap_or_default() > older_than)
                .unwrap_or(false);
            if old {
                stale.push(id);
            }
        }
        Ok(stale)
    }

    /// The two-character directories below `objects/` (`ab` of
    /// `objects/ab/cd/abcd…`), sorted – the pages of [`Vault::objects_in`].
    pub async fn object_prefixes(&self) -> AppResult<Vec<String>> {
        let mut prefixes = Vec::new();
        let mut entries = fs::read_dir(self.objects_dir()).await?;
        while let Some(entry) = entries.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                if is_hex_prefix(name) && entry.file_type().await?.is_dir() {
                    prefixes.push(name.to_string());
                }
            }
        }
        prefixes.sort();
        Ok(prefixes)
    }

    /// The blobs below `objects/<prefix>/`, with size and modification time.
    /// Anything that is not a blob at its content address (another name, a
    /// file in the wrong directory) is skipped – and never removed.
    pub async fn objects_in(&self, prefix: &str) -> AppResult<Vec<StoredObject>> {
        let mut found = Vec::new();
        if !is_hex_prefix(prefix) {
            return Ok(found);
        }
        let top = self.objects_dir().join(prefix);
        let mut dirs = fs::read_dir(&top).await?;
        while let Some(dir) = dirs.next_entry().await? {
            let Some(second) = dir.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !is_hex_prefix(&second) || !dir.file_type().await?.is_dir() {
                continue;
            }
            let mut files = fs::read_dir(dir.path()).await?;
            while let Some(file) = files.next_entry().await? {
                let Some(name) = file.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let at_address = name.len() == 64
                    && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    && name[0..2] == *prefix
                    && name[2..4] == second;
                if !at_address {
                    continue;
                }
                let meta = file.metadata().await?;
                if meta.is_file() {
                    found.push(StoredObject {
                        sha256: name,
                        size: meta.len(),
                        modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    });
                }
            }
        }
        Ok(found)
    }

    /// How long ago a blob was written or last stored again (see
    /// [`Upload::finish`]); `None` if there is no such blob.
    pub async fn object_age(&self, sha256: &str) -> AppResult<Option<Duration>> {
        match fs::metadata(self.object_path(sha256)?).await {
            Ok(meta) => {
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                Ok(Some(
                    SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or_default(),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    /// Removes a blob if it was not written or stored again within `grace`.
    /// Only `services::retention` calls this, after it made sure that the
    /// database refers to the blob nowhere. Returns whether it was removed.
    pub async fn remove_object_older_than(&self, sha256: &str, grace: Duration) -> AppResult<bool> {
        match self.object_age(sha256).await? {
            Some(age) if age >= grace => {}
            _ => return Ok(false),
        }
        match fs::remove_file(self.object_path(sha256)?).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    async fn clean_tmp(&self, older_than: Duration) -> std::io::Result<()> {
        let mut entries = fs::read_dir(self.tmp_dir()).await?;
        let now = SystemTime::now();
        while let Some(entry) = entries.next_entry().await? {
            let stale = entry
                .metadata()
                .await
                .and_then(|m| m.modified())
                .map(|modified| now.duration_since(modified).unwrap_or_default() > older_than)
                .unwrap_or(false);
            if stale {
                let _ = fs::remove_file(entry.path()).await;
            }
        }
        Ok(())
    }
}

/// An upload in progress. Dropping it without calling [`Upload::finish`]
/// deletes the temporary file.
pub struct Upload {
    vault: Vault,
    tmp_path: PathBuf,
    file: Option<fs::File>,
    hasher: Sha256,
    size: u64,
    max_bytes: u64,
    finished: bool,
}

impl Upload {
    pub async fn write(&mut self, chunk: &[u8]) -> AppResult<()> {
        self.size += chunk.len() as u64;
        if self.size > self.max_bytes {
            return Err(AppError::PayloadTooLarge(format!(
                "The file exceeds the upload limit of {} MiB.",
                self.max_bytes / (1024 * 1024)
            )));
        }
        self.hasher.update(chunk);
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("upload already finished")))?;
        file.write_all(chunk).await?;
        Ok(())
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Flushes the data to disk and moves it to its content address.
    pub async fn finish(&mut self) -> AppResult<StoredBlob> {
        let mut file = self
            .file
            .take()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("upload already finished")))?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let sha256 = hex::encode(std::mem::take(&mut self.hasher).finalize());
        let target = self.vault.object_path(&sha256)?;
        // identical content may be stored already
        put_in_place(&self.tmp_path, &target).await?;
        self.finished = true;
        Ok(StoredBlob {
            sha256,
            size: self.size,
        })
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.tmp_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("teamcraft-vault-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn stores_content_addressed_and_deduplicates() {
        let root = temp_root();
        let vault = Vault::open(&root).await.unwrap();
        let a = vault.store_bytes(b"hello vault").await.unwrap();
        let b = vault.store_bytes(b"hello vault").await.unwrap();
        assert_eq!(a.sha256, b.sha256);
        assert_eq!(a.size, 11);
        assert_eq!(a.sha256, hex::encode(Sha256::digest(b"hello vault")));
        assert!(vault.exists(&a.sha256).await);
        assert!(vault.verify(&a.sha256).await.unwrap());
        // the temporary directory is empty again
        let mut tmp = fs::read_dir(root.join("tmp")).await.unwrap();
        assert!(tmp.next_entry().await.unwrap().is_none());
        fs::remove_dir_all(root).await.ok();
    }

    #[tokio::test]
    async fn enforces_size_limit_and_cleans_up() {
        let root = temp_root();
        let vault = Vault::open(&root).await.unwrap();
        {
            let mut upload = vault.begin_upload(4).await.unwrap();
            upload.write(b"1234").await.unwrap();
            let err = upload.write(b"5").await.unwrap_err();
            assert!(matches!(err, AppError::PayloadTooLarge(_)));
        }
        let mut tmp = fs::read_dir(root.join("tmp")).await.unwrap();
        assert!(
            tmp.next_entry().await.unwrap().is_none(),
            "temp file removed on drop"
        );
        fs::remove_dir_all(root).await.ok();
    }

    #[tokio::test]
    async fn staged_chunks_resume_and_move_into_the_vault() {
        let root = temp_root();
        let vault = Vault::open(&root).await.unwrap();
        let id = Uuid::new_v4();
        vault.create_staging(id).await.unwrap();
        vault.write_staged(id, 0, b"hello ").await.unwrap();
        // an interrupted chunk left bytes behind that were never confirmed …
        vault.write_staged(id, 6, b"wor").await.unwrap();
        // … the resent chunk at the confirmed offset replaces them
        vault.write_staged(id, 6, b"vault").await.unwrap();
        assert!(matches!(
            vault.write_staged(id, 99, b"gap").await,
            Err(AppError::Conflict(_))
        ));
        let blob = vault.hash_staged(id).await.unwrap();
        assert_eq!(blob.size, 11);
        assert_eq!(blob.sha256, hex::encode(Sha256::digest(b"hello vault")));
        vault.adopt_staged(id, &blob.sha256).await.unwrap();
        assert!(vault.verify(&blob.sha256).await.unwrap());
        // identical content again: the staging file is dropped, the blob stays
        let twin = Uuid::new_v4();
        vault.create_staging(twin).await.unwrap();
        vault.write_staged(twin, 0, b"hello vault").await.unwrap();
        vault.adopt_staged(twin, &blob.sha256).await.unwrap();
        assert!(vault.stale_staged(Duration::ZERO).await.unwrap().is_empty());
        vault.remove_staged(twin).await.unwrap();
        fs::remove_dir_all(root).await.ok();
    }

    #[test]
    fn rejects_bad_hashes() {
        let vault = Vault {
            root: PathBuf::from("/nonexistent"),
        };
        assert!(vault.object_path("../../etc/passwd").is_err());
        assert!(vault.object_path(&"g".repeat(64)).is_err());
        let ok = vault.object_path(&"ab".repeat(32)).unwrap();
        assert!(ok.ends_with(format!("objects/ab/ab/{}", "ab".repeat(32))));
    }

    /// Sets the modification time of a file `ago` into the past.
    fn age(path: &Path, ago: Duration) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    #[tokio::test]
    async fn lists_blobs_and_removes_only_old_ones() {
        let root = temp_root();
        let vault = Vault::open(&root).await.unwrap();
        let a = vault.store_bytes(b"first").await.unwrap();
        let b = vault.store_bytes(b"second").await.unwrap();
        // not blobs at their address: never listed, never removed
        let a_dir = vault.object_path(&a.sha256).unwrap();
        let a_dir = a_dir.parent().unwrap();
        std::fs::write(a_dir.join("notes.txt"), b"x").unwrap();
        let misplaced = if a.sha256.starts_with("ee") {
            "dd"
        } else {
            "ee"
        }
        .repeat(32);
        std::fs::write(a_dir.join(&misplaced), b"misplaced").unwrap();
        std::fs::create_dir_all(root.join("objects/zz")).unwrap();

        let mut listed = Vec::new();
        for prefix in vault.object_prefixes().await.unwrap() {
            listed.extend(vault.objects_in(&prefix).await.unwrap());
        }
        let mut names: Vec<_> = listed.iter().map(|o| o.sha256.clone()).collect();
        names.sort();
        let mut expected = vec![a.sha256.clone(), b.sha256.clone()];
        expected.sort();
        assert_eq!(names, expected);
        assert!(vault.objects_in("../..").await.unwrap().is_empty());

        // young blobs stay; old ones go
        let hour = Duration::from_secs(3600);
        assert!(!vault
            .remove_object_older_than(&a.sha256, hour)
            .await
            .unwrap());
        age(&vault.object_path(&a.sha256).unwrap(), 2 * hour);
        assert!(vault.object_age(&a.sha256).await.unwrap().unwrap() >= hour);
        // storing the same content again renews the time …
        vault.store_bytes(b"first").await.unwrap();
        assert!(vault.object_age(&a.sha256).await.unwrap().unwrap() < hour);
        assert!(!vault
            .remove_object_older_than(&a.sha256, hour)
            .await
            .unwrap());
        // … and an old, unrenewed blob is removed once
        age(&vault.object_path(&a.sha256).unwrap(), 2 * hour);
        assert!(vault
            .remove_object_older_than(&a.sha256, hour)
            .await
            .unwrap());
        assert!(!vault.exists(&a.sha256).await);
        assert!(vault.object_age(&a.sha256).await.unwrap().is_none());
        assert!(!vault
            .remove_object_older_than(&a.sha256, hour)
            .await
            .unwrap());
        // a chunked upload of the same content renews the time as well
        age(&vault.object_path(&b.sha256).unwrap(), 2 * hour);
        let id = Uuid::new_v4();
        vault.create_staging(id).await.unwrap();
        vault.write_staged(id, 0, b"second").await.unwrap();
        vault.adopt_staged(id, &b.sha256).await.unwrap();
        assert!(vault.object_age(&b.sha256).await.unwrap().unwrap() < hour);
        assert!(vault.verify(&b.sha256).await.unwrap());
        fs::remove_dir_all(root).await.ok();
    }
}
