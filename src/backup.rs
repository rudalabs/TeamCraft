//! Backup and restore of the database and the file vault.
//!
//! A backup directory written by [`backup`] contains:
//!
//! ```text
//! db.dump                    pg_dump --format=custom
//! vault/objects/ab/cd/<sha>  content-addressed blobs (without the preview cache)
//! manifest.json              timestamp, version, object count and SHA-256 of db.dump
//! ```
//!
//! `pg_dump` and `pg_restore` are spawned with an argument array, never
//! through a shell. The database password is passed in the child's
//! `PGPASSWORD` environment only; it never appears in an argument or a log
//! message.

use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use crate::{backupcrypt::BackupKey, config::Config};

const MANIFEST_FILE: &str = "manifest.json";
const DUMP_FILE: &str = "db.dump";
const VAULT_DIR: &str = "vault";
const MANIFEST_FORMAT: &str = "teamcraft-backup/1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    created_at_unix: u64,
    app_version: String,
    vault_objects: u64,
    /// SHA-256 of the plain dump (also when the file is encrypted).
    db_dump_sha256: String,
    /// Key derivation and cipher of an encrypted backup (`backupcrypt`);
    /// `None` = plain files.
    encryption: Option<serde_json::Value>,
}

impl Manifest {
    fn to_json(&self) -> String {
        let mut value = json!({
            "format": MANIFEST_FORMAT,
            "created_at_unix": self.created_at_unix,
            "app_version": self.app_version,
            "vault_objects": self.vault_objects,
            "db_dump_sha256": self.db_dump_sha256,
        });
        if let Some(encryption) = &self.encryption {
            value["encryption"] = encryption.clone();
        }
        serde_json::to_string_pretty(&value).expect("the manifest is always serializable")
    }

    fn from_json(text: &str) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_str(text).context("manifest.json is not valid JSON")?;
        let format = value
            .get("format")
            .and_then(serde_json::Value::as_str)
            .context("manifest.json has no 'format'")?;
        if format != MANIFEST_FORMAT {
            bail!("unsupported backup format '{format}' (expected '{MANIFEST_FORMAT}')");
        }
        let string_field = |name: &str| -> Result<String> {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .with_context(|| format!("manifest.json has no '{name}'"))
        };
        let number_field = |name: &str| -> Result<u64> {
            value
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .with_context(|| format!("manifest.json has no '{name}'"))
        };
        let db_dump_sha256 = string_field("db_dump_sha256")?.to_ascii_lowercase();
        if let Err(error) = validate_hash(&db_dump_sha256) {
            return Err(error.context("manifest.json: 'db_dump_sha256' is invalid"));
        }
        Ok(Manifest {
            created_at_unix: number_field("created_at_unix")?,
            app_version: string_field("app_version")?,
            vault_objects: number_field("vault_objects")?,
            db_dump_sha256,
            encryption: value.get("encryption").filter(|v| !v.is_null()).cloned(),
        })
    }
}

/// Result of a successful backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSummary {
    pub db_dump_bytes: u64,
    pub db_dump_sha256: String,
    pub vault_objects: usize,
}

/// Result of a successful restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSummary {
    pub vault_objects: usize,
    pub copied: usize,
    pub skipped: usize,
}

/// Connection parameters for the PostgreSQL client tools. The password is
/// kept separate from the argument list.
struct DatabaseArgs {
    host: String,
    port: u16,
    username: String,
    database: String,
    password: Option<String>,
    ssl_mode: &'static str,
}

impl DatabaseArgs {
    fn new(options: &PgConnectOptions, password: Option<String>) -> Self {
        let username = options.get_username();
        DatabaseArgs {
            host: options.get_host().to_string(),
            port: options.get_port(),
            username: username.to_string(),
            database: options.get_database().unwrap_or(username).to_string(),
            password,
            ssl_mode: ssl_mode_str(options.get_ssl_mode()),
        }
    }

    fn from_config(config: &Config) -> Self {
        Self::new(&config.database, config.database_password.clone())
    }
}

/// Value of `PGSSLMODE` that corresponds to the connection options.
fn ssl_mode_str(mode: PgSslMode) -> &'static str {
    match mode {
        PgSslMode::Disable => "disable",
        PgSslMode::Allow => "allow",
        PgSslMode::Prefer => "prefer",
        PgSslMode::Require => "require",
        PgSslMode::VerifyCa => "verify-ca",
        PgSslMode::VerifyFull => "verify-full",
    }
}

/// Writes a backup of the configured database and vault into `dir`.
///
/// Every vault object is checked against its name while it is copied, and
/// every file content the database refers to must be in the vault: a
/// corrupt or missing object refuses the backup. A failed backup removes
/// what it wrote (the directory, if it created it), so nothing is left that
/// looks like a backup; the manifest is written last and renamed into place.
pub async fn backup(config: &Config, dir: &Path) -> Result<BackupSummary> {
    backup_with(config, dir, None).await
}

/// Like [`backup`], with every file encrypted with a key derived from
/// `passphrase` (see [`crate::backupcrypt`]). The plain dump exists only
/// for the moment between `pg_dump` and its encryption, inside `dir`.
pub async fn backup_encrypted(
    config: &Config,
    dir: &Path,
    passphrase: &str,
) -> Result<BackupSummary> {
    let key = BackupKey::new(passphrase)?;
    backup_with(config, dir, Some(&key)).await
}

async fn backup_with(
    config: &Config,
    dir: &Path,
    key: Option<&BackupKey>,
) -> Result<BackupSummary> {
    let created = prepare_target(dir)?;
    match write_backup(config, dir, key).await {
        Ok(summary) => Ok(summary),
        Err(error) => {
            discard_partial(dir, created);
            Err(error.context("no backup was written"))
        }
    }
}

async fn write_backup(
    config: &Config,
    dir: &Path,
    key: Option<&BackupKey>,
) -> Result<BackupSummary> {
    let database = DatabaseArgs::from_config(config);
    let dump = dir.join(DUMP_FILE);
    let plain_dump = match key {
        Some(_) => dir.join(format!("{DUMP_FILE}.plain")),
        None => dump.clone(),
    };
    run(dump_command(&database, &plain_dump), "pg_dump")?;
    if !plain_dump.is_file() {
        bail!("pg_dump did not create '{}'", plain_dump.display());
    }
    let (db_dump_sha256, db_dump_bytes) = match key {
        Some(key) => {
            let result = crate::backupcrypt::encrypt_file(key, &plain_dump, &dump);
            let _ = fs::remove_file(&plain_dump);
            result?
        }
        None => {
            let bytes = fs::metadata(&dump)
                .with_context(|| format!("cannot read '{}'", dump.display()))?
                .len();
            (sha256_file(&dump)?, bytes)
        }
    };

    // Read after the dump: a content is stored in the vault before the row
    // that names it is committed, so everything the dump refers to is listed.
    let referenced = referenced_hashes(config).await?;
    let target_objects = dir.join(VAULT_DIR).join("objects");
    fs::create_dir_all(&target_objects)
        .with_context(|| format!("cannot create '{}'", target_objects.display()))?;
    let copied = copy_checked_objects(&config.vault_dir.join("objects"), &target_objects, key)?;
    let missing: Vec<&String> = referenced
        .iter()
        .filter(|hash| !copied.contains(hash.as_str()))
        .collect();
    if let Some(first) = missing.first() {
        bail!(
            "{} file content(s) the database refers to are missing from the vault (e.g. {first}); \
             check the vault with `teamcraft vault check`",
            missing.len()
        );
    }

    let manifest = Manifest {
        created_at_unix: now_unix(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        vault_objects: copied.len() as u64,
        db_dump_sha256: db_dump_sha256.clone(),
        encryption: key.map(BackupKey::manifest_entry),
    };
    let manifest_path = dir.join(MANIFEST_FILE);
    let partial = dir.join(format!("{MANIFEST_FILE}.partial"));
    fs::write(&partial, manifest.to_json())
        .with_context(|| format!("cannot write '{}'", partial.display()))?;
    fs::rename(&partial, &manifest_path)
        .with_context(|| format!("cannot write '{}'", manifest_path.display()))?;

    Ok(BackupSummary {
        db_dump_bytes,
        db_dump_sha256,
        vault_objects: copied.len(),
    })
}

/// The file contents the database refers to (as `teamcraft vault check`).
async fn referenced_hashes(config: &Config) -> Result<Vec<String>> {
    let pool = crate::db::connect(config)
        .await
        .context("cannot connect to the database to list the referenced file contents")?;
    let hashes = crate::services::files::all_hashes(&pool).await;
    pool.close().await;
    hashes.map_err(|e| anyhow::anyhow!("cannot list the referenced file contents: {e}"))
}

/// Copies every object like [`copy_objects`] and checks its content against
/// its name; returns the hashes copied.
fn copy_checked_objects(
    source_objects: &Path,
    target_objects: &Path,
    key: Option<&BackupKey>,
) -> Result<std::collections::HashSet<String>> {
    let mut copied = std::collections::HashSet::new();
    for object in collect_objects(source_objects)? {
        let target = object_path_under(target_objects, &object.hash)?;
        if fs::symlink_metadata(&target).is_ok() {
            bail!(
                "backup target already contains the object '{}'",
                object.hash
            );
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create '{}'", parent.display()))?;
        }
        let actual = match key {
            // encrypted on the way; the hash is of the plain content read
            Some(key) => crate::backupcrypt::encrypt_file(key, &object.path, &target)?.0,
            None => {
                fs::copy(&object.path, &target).with_context(|| {
                    format!(
                        "cannot copy vault object '{}' to '{}'",
                        object.path.display(),
                        target.display()
                    )
                })?;
                sha256_file(&target)?
            }
        };
        if actual != object.hash {
            bail!(
                "vault object {} is corrupt (its content is {actual}); restore it from an \
                 older backup or check the vault with `teamcraft vault check --verify-hashes`",
                object.hash
            );
        }
        copied.insert(object.hash);
    }
    Ok(copied)
}

/// Removes what a failed backup wrote: the whole directory if the backup
/// created it, otherwise its content (it was empty before).
fn discard_partial(dir: &Path, created: bool) {
    if created {
        let _ = fs::remove_dir_all(dir);
        return;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// Verifies a backup directory and restores it into the configured database
/// and vault. A database that already contains tables requires `force`.
pub async fn restore(config: &Config, dir: &Path, force: bool) -> Result<RestoreSummary> {
    restore_with(config, dir, force, None).await
}

/// Like [`restore`]; an encrypted backup needs its `passphrase`. The dump is
/// decrypted to a temporary file in the vault's `tmp/` for `pg_restore` and
/// removed afterwards; objects are decrypted straight into the vault.
pub async fn restore_with(
    config: &Config,
    dir: &Path,
    force: bool,
    passphrase: Option<&str>,
) -> Result<RestoreSummary> {
    let (manifest, key) = verify_backup_with(dir, passphrase)?;
    ensure_database_empty(config, force).await?;
    let database = DatabaseArgs::from_config(config);
    let mut dump = dir.join(DUMP_FILE);
    let mut temporary = None;
    if let Some(key) = &key {
        let tmp = config.vault_dir.join("tmp");
        fs::create_dir_all(&tmp).with_context(|| format!("cannot create '{}'", tmp.display()))?;
        let plain = tmp.join(format!("restore-{}.dump", std::process::id()));
        let _ = fs::remove_file(&plain);
        let mut out = std::io::BufWriter::new(
            fs::File::create_new(&plain)
                .with_context(|| format!("cannot create '{}'", plain.display()))?,
        );
        if let Err(e) = crate::backupcrypt::decrypt_file(key, &dump, &mut out) {
            drop(out);
            let _ = fs::remove_file(&plain);
            return Err(e);
        }
        drop(out);
        dump = plain.clone();
        temporary = Some(plain);
    }
    let restored = run(restore_command(&database, &dump), "pg_restore");
    if let Some(plain) = &temporary {
        let _ = fs::remove_file(plain);
    }
    if let Err(error) = restored {
        bail!(
            "{error:#} – the database is unchanged (the restore runs in one transaction); \
             restore a backup that is older than the database (e.g. from before an upgrade) \
             into a fresh, empty database"
        );
    }

    let target_objects = config.vault_dir.join("objects");
    fs::create_dir_all(&target_objects)
        .with_context(|| format!("cannot create '{}'", target_objects.display()))?;
    let stats = copy_objects_with(
        &dir.join(VAULT_DIR).join("objects"),
        &target_objects,
        true,
        key.as_ref(),
    )?;

    Ok(RestoreSummary {
        vault_objects: manifest.vault_objects as usize,
        copied: stats.copied,
        skipped: stats.skipped,
    })
}

async fn ensure_database_empty(config: &Config, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let pool = match crate::db::connect(config).await {
        Ok(pool) => pool,
        Err(error) => bail!(
            "cannot connect to the database to check whether it is empty (does it exist?): {error:#}"
        ),
    };
    let tables = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pg_tables
         WHERE schemaname NOT IN ('pg_catalog', 'information_schema')",
    )
    .fetch_one(&pool)
    .await
    .context("cannot inspect the database");
    pool.close().await;
    let tables = tables?;
    if tables > 0 {
        bail!(
            "the database already contains {tables} table(s); run the restore with --force to replace them"
        );
    }
    Ok(())
}

/// Checks that `dir` is an empty directory or does not exist (then it is
/// created); returns whether it was created.
fn prepare_target(dir: &Path) -> Result<bool> {
    if dir.exists() {
        if !dir.is_dir() {
            bail!("backup target '{}' is not a directory", dir.display());
        }
        let mut entries = fs::read_dir(dir)
            .with_context(|| format!("cannot read backup target '{}'", dir.display()))?;
        if entries
            .next()
            .transpose()
            .with_context(|| format!("cannot read backup target '{}'", dir.display()))?
            .is_some()
        {
            bail!("backup target '{}' is not empty", dir.display());
        }
        Ok(false)
    } else {
        fs::create_dir_all(dir)
            .with_context(|| format!("cannot create backup target '{}'", dir.display()))?;
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectFile {
    hash: String,
    path: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CopyStats {
    copied: usize,
    skipped: usize,
}

fn read_sorted(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("cannot read '{}'", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("cannot read '{}'", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn hex_folder(entry: &fs::DirEntry) -> Result<String> {
    let name = entry.file_name();
    let name = name
        .to_str()
        .with_context(|| format!("non-UTF-8 name in the vault: '{}'", entry.path().display()))?;
    if name.len() != 2 || !name.bytes().all(is_lower_hex) {
        bail!(
            "unexpected entry '{}': not a two-digit hex folder",
            entry.path().display()
        );
    }
    Ok(name.to_string())
}

fn collect_objects(objects_root: &Path) -> Result<Vec<ObjectFile>> {
    if !objects_root.exists() {
        return Ok(Vec::new());
    }
    if !objects_root.is_dir() {
        bail!(
            "vault objects path '{}' is not a directory",
            objects_root.display()
        );
    }
    let mut objects = Vec::new();
    for first in read_sorted(objects_root)? {
        if !first.file_type()?.is_dir() {
            bail!(
                "unexpected file '{}' in the vault objects directory",
                first.path().display()
            );
        }
        let first_prefix = hex_folder(&first)?;
        for second in read_sorted(&first.path())? {
            if !second.file_type()?.is_dir() {
                bail!(
                    "unexpected file '{}' in the vault objects directory",
                    second.path().display()
                );
            }
            let second_prefix = hex_folder(&second)?;
            for entry in read_sorted(&second.path())? {
                if !entry.file_type()?.is_file() {
                    bail!(
                        "'{}' is not a regular file; refusing to copy it",
                        entry.path().display()
                    );
                }
                let name = entry.file_name();
                let hash = name
                    .to_str()
                    .with_context(|| {
                        format!("non-UTF-8 object name in '{}'", entry.path().display())
                    })?
                    .to_string();
                if let Err(error) = validate_hash(&hash) {
                    return Err(
                        error.context(format!("unexpected object '{}'", entry.path().display()))
                    );
                }
                let expected_prefix = format!("{first_prefix}{second_prefix}");
                if !hash.starts_with(expected_prefix.as_str()) {
                    bail!(
                        "object '{}' is not stored in its content-addressed folder",
                        entry.path().display()
                    );
                }
                objects.push(ObjectFile {
                    hash,
                    path: entry.path(),
                });
            }
        }
    }
    Ok(objects)
}

#[cfg(test)]
fn copy_objects(
    source_objects: &Path,
    target_objects: &Path,
    skip_existing: bool,
) -> Result<CopyStats> {
    copy_objects_with(source_objects, target_objects, skip_existing, None)
}

/// Copies (with a key: decrypts) the objects of a backup into a vault.
fn copy_objects_with(
    source_objects: &Path,
    target_objects: &Path,
    skip_existing: bool,
    key: Option<&BackupKey>,
) -> Result<CopyStats> {
    let mut stats = CopyStats::default();
    for object in collect_objects(source_objects)? {
        let target = object_path_under(target_objects, &object.hash)?;
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_file() => {
                if skip_existing {
                    stats.skipped += 1;
                    continue;
                }
                bail!(
                    "backup target already contains the object '{}'",
                    object.hash
                );
            }
            Ok(_) => bail!("'{}' exists and is not a regular file", target.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot inspect '{}'", target.display()));
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create '{}'", parent.display()))?;
        }
        if let Some(key) = key {
            // decrypted next to its place and moved in once its content is checked
            let partial = target.with_extension("partial");
            let _ = fs::remove_file(&partial);
            let mut out = std::io::BufWriter::new(
                fs::File::create_new(&partial)
                    .with_context(|| format!("cannot create '{}'", partial.display()))?,
            );
            let sha = crate::backupcrypt::decrypt_file(key, &object.path, &mut out);
            drop(out);
            match sha {
                Ok(sha) if sha == object.hash => fs::rename(&partial, &target)
                    .with_context(|| format!("cannot write '{}'", target.display()))?,
                Ok(sha) => {
                    let _ = fs::remove_file(&partial);
                    bail!(
                        "vault object {} is corrupt (its content is {sha})",
                        object.hash
                    );
                }
                Err(e) => {
                    let _ = fs::remove_file(&partial);
                    return Err(e);
                }
            }
        } else {
            fs::copy(&object.path, &target).with_context(|| {
                format!(
                    "cannot copy vault object '{}' to '{}'",
                    object.path.display(),
                    target.display()
                )
            })?;
        }
        stats.copied += 1;
    }
    Ok(stats)
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64 || !hash.bytes().all(is_lower_hex) {
        bail!("'{hash}' is not a lower-case SHA-256 hash");
    }
    Ok(())
}

fn is_lower_hex(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn object_path_under(objects_root: &Path, hash: &str) -> Result<PathBuf> {
    validate_hash(hash)?;
    Ok(objects_root.join(&hash[0..2]).join(&hash[2..4]).join(hash))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot read '{}'", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot read '{}'", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Whether the backup in `dir` is encrypted (from its manifest).
pub fn is_encrypted(dir: &Path) -> Result<bool> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read '{}'", manifest_path.display()))?;
    Ok(Manifest::from_json(&text)?.encryption.is_some())
}

#[cfg(test)]
fn verify_backup(dir: &Path) -> Result<Manifest> {
    verify_backup_with(dir, None).map(|(manifest, _)| manifest)
}

/// SHA-256 of a backup file's plain content (decrypted on the way when the
/// backup is encrypted; authentication failures are errors).
fn backup_file_sha(path: &Path, key: Option<&BackupKey>) -> Result<String> {
    match key {
        Some(key) => crate::backupcrypt::decrypt_file(key, path, &mut std::io::sink()),
        None => sha256_file(path),
    }
}

/// Checks a backup directory completely (manifest, dump, every object) and
/// returns its manifest and – for an encrypted backup – the key.
fn verify_backup_with(
    dir: &Path,
    passphrase: Option<&str>,
) -> Result<(Manifest, Option<BackupKey>)> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read '{}'", manifest_path.display()))?;
    let manifest = Manifest::from_json(&text)?;
    let key = match (&manifest.encryption, passphrase) {
        (Some(entry), Some(passphrase)) => Some(BackupKey::from_manifest(passphrase, entry)?),
        (Some(_), None) => bail!(
            "the backup is encrypted: set TEAMCRAFT_BACKUP_PASSPHRASE (or _FILE) or enter the passphrase"
        ),
        (None, _) => None,
    };

    let dump = dir.join(DUMP_FILE);
    let metadata =
        fs::metadata(&dump).with_context(|| format!("cannot read '{}'", dump.display()))?;
    if !metadata.is_file() {
        bail!("'{}' is not a file", dump.display());
    }
    let actual = backup_file_sha(&dump, key.as_ref())?;
    if actual != manifest.db_dump_sha256 {
        bail!(
            "database dump checksum mismatch: manifest {}, file {}",
            manifest.db_dump_sha256,
            actual
        );
    }

    let objects_root = dir.join(VAULT_DIR).join("objects");
    let objects = collect_objects(&objects_root)?;
    if objects.len() as u64 != manifest.vault_objects {
        bail!(
            "backup contains {} vault object(s), the manifest says {}",
            objects.len(),
            manifest.vault_objects
        );
    }
    for object in &objects {
        let actual = backup_file_sha(&object.path, key.as_ref())?;
        if actual != object.hash {
            bail!(
                "vault object {} is corrupt (file content is {})",
                object.hash,
                actual
            );
        }
    }
    Ok((manifest, key))
}

fn connection_args(database: &DatabaseArgs) -> Vec<OsString> {
    vec![
        OsString::from("--host"),
        OsString::from(database.host.as_str()),
        OsString::from("--port"),
        OsString::from(database.port.to_string()),
        OsString::from("--username"),
        OsString::from(database.username.as_str()),
        OsString::from("--dbname"),
        OsString::from(database.database.as_str()),
    ]
}

fn dump_args(database: &DatabaseArgs, target: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--format=custom"),
        OsString::from("--no-password"),
    ];
    args.extend(connection_args(database));
    args.push(OsString::from("--file"));
    args.push(target.as_os_str().to_os_string());
    args
}

fn restore_args(database: &DatabaseArgs, dump: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--clean"),
        OsString::from("--if-exists"),
        OsString::from("--no-owner"),
        // All or nothing: a failed restore (e.g. an older backup over a
        // database with newer tables that depend on the dropped ones) must
        // not leave a half-replaced database behind.
        OsString::from("--single-transaction"),
        OsString::from("--no-password"),
    ];
    args.extend(connection_args(database));
    args.push(dump.as_os_str().to_os_string());
    args
}

fn apply_connection_env(command: &mut Command, database: &DatabaseArgs) {
    command.env("PGSSLMODE", database.ssl_mode);
    if let Some(password) = &database.password {
        command.env("PGPASSWORD", password.as_str());
    }
}

fn dump_command(database: &DatabaseArgs, target: &Path) -> Command {
    let mut command = Command::new("pg_dump");
    command.args(dump_args(database, target));
    apply_connection_env(&mut command, database);
    command
}

fn restore_command(database: &DatabaseArgs, dump: &Path) -> Command {
    let mut command = Command::new("pg_restore");
    command.args(restore_args(database, dump));
    apply_connection_env(&mut command, database);
    command
}

fn run(mut command: Command, name: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("cannot run {name}: is it installed and on PATH?"))?;
    if !status.success() {
        bail!("{name} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("teamcraft-backup-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn write_object(objects_root: &Path, content: &[u8]) -> String {
        let hash = sha256_bytes(content);
        let path = object_path_under(objects_root, &hash).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        hash
    }

    fn database_args() -> DatabaseArgs {
        DatabaseArgs {
            host: "db.example.org".into(),
            port: 5433,
            username: "teamcraft".into(),
            database: "plm".into(),
            password: Some("pw-secret".into()),
            ssl_mode: "require",
        }
    }

    fn arg_strings(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn env_value<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .and_then(|(_, value)| value)
    }

    #[test]
    fn manifest_round_trip_and_validation() {
        let manifest = Manifest {
            created_at_unix: 1_762_000_000,
            app_version: "0.1.0".into(),
            vault_objects: 3,
            db_dump_sha256: sha256_bytes(b"dump"),
            encryption: None,
        };
        assert_eq!(Manifest::from_json(&manifest.to_json()).unwrap(), manifest);
        assert!(Manifest::from_json("{").is_err());
        let invalid_hash = format!(
            r#"{{"format":"{MANIFEST_FORMAT}","created_at_unix":1,"app_version":"0.1.0","vault_objects":0,"db_dump_sha256":"../escape"}}"#
        );
        assert!(Manifest::from_json(&invalid_hash).is_err());
        let other_format = manifest.to_json().replace(MANIFEST_FORMAT, "other/1");
        assert!(Manifest::from_json(&other_format).is_err());
        let no_count = format!(
            r#"{{"format":"{MANIFEST_FORMAT}","created_at_unix":1,"app_version":"0.1.0","db_dump_sha256":"{}"}}"#,
            sha256_bytes(b"dump")
        );
        assert!(Manifest::from_json(&no_count).is_err());
    }

    #[test]
    fn database_options_are_split_into_arguments() {
        let options = PgConnectOptions::new()
            .host("db.example.org")
            .port(5433)
            .username("teamcraft")
            .password("pw-secret")
            .database("plm")
            .ssl_mode(PgSslMode::VerifyFull);
        let database = DatabaseArgs::new(&options, Some("pw-secret".into()));
        assert_eq!(database.host.as_str(), "db.example.org");
        assert_eq!(database.port, 5433);
        assert_eq!(database.username.as_str(), "teamcraft");
        assert_eq!(database.database.as_str(), "plm");
        assert_eq!(database.password.as_deref(), Some("pw-secret"));
        assert_eq!(database.ssl_mode, "verify-full");
    }

    #[test]
    fn commands_keep_the_password_out_of_the_arguments() {
        let database = database_args();
        let dump = Path::new("/backup/db.dump");
        let command = dump_command(&database, dump);
        assert_eq!(command.get_program().to_string_lossy(), "pg_dump");
        let args = arg_strings(&command);
        for expected in [
            "--format=custom",
            "--no-password",
            "--host",
            "db.example.org",
            "--port",
            "5433",
            "--username",
            "teamcraft",
            "--dbname",
            "plm",
            "--file",
            "/backup/db.dump",
        ] {
            assert!(
                args.iter().any(|arg| arg.as_str() == expected),
                "missing {expected}: {args:?}"
            );
        }
        assert!(!args.iter().any(|arg| arg.contains("pw-secret")));
        assert_eq!(
            env_value(&command, "PGPASSWORD"),
            Some(OsStr::new("pw-secret"))
        );
        assert_eq!(
            env_value(&command, "PGSSLMODE"),
            Some(OsStr::new("require"))
        );

        let command = restore_command(&database, dump);
        assert_eq!(command.get_program().to_string_lossy(), "pg_restore");
        let args = arg_strings(&command);
        for expected in [
            "--clean",
            "--if-exists",
            "--no-owner",
            "--single-transaction",
            "--no-password",
            "--host",
            "db.example.org",
            "--dbname",
            "plm",
            "/backup/db.dump",
        ] {
            assert!(
                args.iter().any(|arg| arg.as_str() == expected),
                "missing {expected}: {args:?}"
            );
        }
        assert!(!args.iter().any(|arg| arg.contains("pw-secret")));
    }

    #[test]
    fn hashes_files() {
        let dir = temp_dir("hash");
        let file = dir.join("db.dump");
        fs::write(&file, b"hello backup").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), sha256_bytes(b"hello backup"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn verify_checks_manifest_dump_and_object_count() {
        let dir = temp_dir("verify");
        let dump = b"database dump";
        fs::write(dir.join(DUMP_FILE), dump).unwrap();
        let objects = dir.join(VAULT_DIR).join("objects");
        write_object(&objects, b"file one");
        fs::write(
            dir.join(MANIFEST_FILE),
            Manifest {
                created_at_unix: 1,
                app_version: "0.1.0".into(),
                vault_objects: 1,
                db_dump_sha256: sha256_bytes(dump),
                encryption: None,
            }
            .to_json(),
        )
        .unwrap();
        assert_eq!(verify_backup(&dir).unwrap().vault_objects, 1);

        fs::write(dir.join(DUMP_FILE), b"tampered").unwrap();
        let error = verify_backup(&dir).unwrap_err().to_string();
        assert!(error.contains("checksum mismatch"), "{error}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn encrypted_backups_verify_and_restore_only_with_the_passphrase() {
        let pass = "correct horse battery staple";
        let key = BackupKey::new(pass).unwrap();
        let source = temp_dir("enc-source");
        let dir = temp_dir("enc-backup");
        let plain_dump = source.join("db.dump");
        fs::write(&plain_dump, b"database dump").unwrap();
        crate::backupcrypt::encrypt_file(&key, &plain_dump, &dir.join(DUMP_FILE)).unwrap();
        let vault_objects = source.join("objects");
        let hash = write_object(&vault_objects, b"file one");
        let backup_objects = dir.join(VAULT_DIR).join("objects");
        let copied = copy_checked_objects(&vault_objects, &backup_objects, Some(&key)).unwrap();
        assert!(copied.contains(&hash));
        let stored = object_path_under(&backup_objects, &hash).unwrap();
        assert_ne!(fs::read(&stored).unwrap(), b"file one", "stored encrypted");
        fs::write(
            dir.join(MANIFEST_FILE),
            Manifest {
                created_at_unix: 1,
                app_version: "0.1.0".into(),
                vault_objects: 1,
                db_dump_sha256: sha256_bytes(b"database dump"),
                encryption: Some(key.manifest_entry()),
            }
            .to_json(),
        )
        .unwrap();
        assert!(is_encrypted(&dir).unwrap());

        let (manifest, found) = verify_backup_with(&dir, Some(pass)).unwrap();
        assert_eq!(manifest.vault_objects, 1);
        assert!(found.is_some());
        let error = verify_backup_with(&dir, None).unwrap_err().to_string();
        assert!(error.contains("encrypted"), "{error}");
        let error = verify_backup_with(&dir, Some("a wrong passphrase"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("wrong backup passphrase"), "{error}");

        // decrypted into a vault, checked against the name
        let target = temp_dir("enc-target");
        let key = found.unwrap();
        let stats = copy_objects_with(&backup_objects, &target, true, Some(&key)).unwrap();
        assert_eq!(stats.copied, 1);
        assert_eq!(
            fs::read(object_path_under(&target, &hash).unwrap()).unwrap(),
            b"file one"
        );

        // a changed byte is caught by the authentication
        let mut bytes = fs::read(&stored).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&stored, bytes).unwrap();
        assert!(verify_backup_with(&dir, Some(pass)).is_err());
        for d in [source, dir, target] {
            fs::remove_dir_all(d).ok();
        }
    }

    #[test]
    fn verify_detects_a_corrupt_object_and_a_wrong_count() {
        let dir = temp_dir("verify-object");
        let dump = b"dump";
        fs::write(dir.join(DUMP_FILE), dump).unwrap();
        let objects = dir.join(VAULT_DIR).join("objects");
        let hash = write_object(&objects, b"content");
        let manifest = Manifest {
            created_at_unix: 1,
            app_version: "0.1.0".into(),
            vault_objects: 1,
            db_dump_sha256: sha256_bytes(dump),
            encryption: None,
        };
        fs::write(dir.join(MANIFEST_FILE), manifest.to_json()).unwrap();

        fs::write(object_path_under(&objects, &hash).unwrap(), b"changed").unwrap();
        let error = verify_backup(&dir).unwrap_err().to_string();
        assert!(error.contains("corrupt"), "{error}");

        fs::write(
            dir.join(MANIFEST_FILE),
            Manifest {
                vault_objects: 2,
                ..manifest
            }
            .to_json(),
        )
        .unwrap();
        let error = verify_backup(&dir).unwrap_err().to_string();
        assert!(error.contains("manifest says 2"), "{error}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn object_paths_are_validated() {
        let root = Path::new("/vault/objects");
        assert!(object_path_under(root, "../../etc/passwd").is_err());
        assert!(object_path_under(root, &"g".repeat(64)).is_err());
        assert!(object_path_under(root, &"AB".repeat(32)).is_err());
        let hash = "ab".repeat(32);
        assert_eq!(
            object_path_under(root, &hash).unwrap(),
            root.join("ab").join("ab").join(&hash)
        );

        let dir = temp_dir("paths");
        let objects = dir.join("objects");
        fs::create_dir_all(objects.join("ab").join("cd")).unwrap();
        fs::write(objects.join("ab").join("cd").join("not-a-hash"), b"x").unwrap();
        assert!(collect_objects(&objects).is_err());
        fs::remove_dir_all(dir).ok();

        let dir = temp_dir("paths-misplaced");
        let objects = dir.join("objects");
        let hash = "ab".repeat(32);
        let wrong = objects.join("00").join("00");
        fs::create_dir_all(&wrong).unwrap();
        fs::write(wrong.join(&hash), b"content").unwrap();
        assert!(collect_objects(&objects).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn copy_skips_existing_objects() {
        let source_root = temp_dir("copy-source");
        let target_root = temp_dir("copy-target");
        let source = source_root.join("objects");
        let target = target_root.join("objects");
        let hash = write_object(&source, b"blob");
        let stats = copy_objects(&source, &target, true).unwrap();
        assert_eq!(
            stats,
            CopyStats {
                copied: 1,
                skipped: 0
            }
        );

        let target_file = object_path_under(&target, &hash).unwrap();
        fs::write(&target_file, b"keep me").unwrap();
        let stats = copy_objects(&source, &target, true).unwrap();
        assert_eq!(
            stats,
            CopyStats {
                copied: 0,
                skipped: 1
            }
        );
        assert_eq!(
            fs::read(&target_file).unwrap(),
            b"keep me",
            "existing objects are not overwritten"
        );
        assert_eq!(
            copy_objects(&source, &target, false)
                .unwrap_err()
                .to_string(),
            format!("backup target already contains the object '{hash}'")
        );
        fs::remove_dir_all(source_root).ok();
        fs::remove_dir_all(target_root).ok();
    }

    #[test]
    fn backup_target_must_exist_empty_or_not_at_all() {
        let root = temp_dir("target");
        let target = root.join("backup");
        prepare_target(&target).unwrap();
        prepare_target(&target).unwrap();
        fs::write(target.join("partial.dump"), b"x").unwrap();
        assert!(prepare_target(&target).is_err());
        let file = root.join("plain-file");
        fs::write(&file, b"x").unwrap();
        assert!(prepare_target(&file).is_err());
        fs::remove_dir_all(root).ok();
    }
}
