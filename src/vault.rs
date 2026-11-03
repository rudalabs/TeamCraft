//! Content-addressed file vault on the local file system.
//!
//! Layout below the vault root:
//!
//! ```text
//! objects/ab/cd/abcdef0123…   blob named by the SHA-256 of its content
//! tmp/                         uploads in progress (same file system → atomic rename)
//! ```
//!
//! Identical files are stored once, no matter how many revisions reference
//! them. Blobs are never modified after they were written.

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

impl Vault {
    /// Creates the directory layout (if needed) and removes stale temporary files.
    pub async fn open(root: impl Into<PathBuf>) -> std::io::Result<Vault> {
        let vault = Vault { root: root.into() };
        fs::create_dir_all(vault.objects_dir()).await?;
        fs::create_dir_all(vault.tmp_dir()).await?;
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
        if fs::try_exists(&target).await? {
            // Identical content is already stored.
            fs::remove_file(&self.tmp_path).await?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::rename(&self.tmp_path, &target).await?;
        }
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
}
