//! Error paths of `teamcraft backup` / `teamcraft restore` that are decided
//! before PostgreSQL is touched: a damaged or incomplete backup directory
//! is refused as a whole, and nothing is written to the vault of the
//! target. No database and no PostgreSQL client tools needed (the success
//! path is `tests/backup_restore.rs`).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use teamcraft::{backup, config::Config};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tc-backup-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A configuration whose database is never reached (port 1) and whose vault
/// is `vault`.
fn config(vault: &Path) -> Config {
    let vault = vault.to_string_lossy().to_string();
    Config::from_lookup(move |key| match key {
        "TEAMCRAFT_DB_HOST" => Some("127.0.0.1".into()),
        "TEAMCRAFT_DB_PORT" => Some("1".into()),
        "TEAMCRAFT_DB_USER" => Some("nobody".into()),
        "TEAMCRAFT_DB_NAME" => Some("nothing".into()),
        "TEAMCRAFT_VAULT_DIR" => Some(vault.clone()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        _ => None,
    })
    .unwrap()
}

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Writes a well-formed backup directory with the given vault objects.
fn make_backup(dir: &Path, dump: &[u8], objects: &[&[u8]]) {
    std::fs::write(dir.join("db.dump"), dump).unwrap();
    for data in objects {
        let hash = sha(data);
        let folder = dir
            .join("vault/objects")
            .join(&hash[0..2])
            .join(&hash[2..4]);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(&hash), data).unwrap();
    }
    std::fs::create_dir_all(dir.join("vault/objects")).unwrap();
    let manifest = serde_json::json!({
        "format": "teamcraft-backup/1",
        "created_at_unix": 1,
        "app_version": "0.1.0",
        "vault_objects": objects.len(),
        "db_dump_sha256": sha(dump),
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

/// The restore is refused with a message containing `needle`, and neither
/// the vault nor anything else was created.
async fn refused(dir: &Path, needle: &str) {
    let vault = scratch("target").join("vault");
    let err = backup::restore(&config(&vault), dir, true)
        .await
        .expect_err("the backup must be refused");
    let text = format!("{err:#}");
    assert!(text.contains(needle), "wanted '{needle}' in: {text}");
    assert!(!vault.exists(), "nothing is written for a refused backup");
}

#[tokio::test]
async fn a_missing_or_unreadable_backup_is_refused() {
    refused(&scratch("empty"), "manifest.json").await;
    refused(&std::env::temp_dir().join("tc-no-such-backup"), "manifest.json").await;
}

#[tokio::test]
async fn an_interrupted_backup_without_manifest_is_refused() {
    // backup() writes the manifest last: a dump and objects without it are
    // what an interrupted run leaves behind
    let dir = scratch("interrupted");
    make_backup(&dir, b"PGDMP", &[b"object"]);
    std::fs::remove_file(dir.join("manifest.json")).unwrap();
    refused(&dir, "manifest.json").await;
}

#[tokio::test]
async fn a_damaged_manifest_is_refused() {
    let dir = scratch("manifest");
    make_backup(&dir, b"PGDMP", &[]);
    let good = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    let cases: [(String, &str); 7] = [
        ("not json".into(), "not valid JSON"),
        ("{}".into(), "no 'format'"),
        (
            good.replace("teamcraft-backup/1", "teamcraft-backup/2"),
            "unsupported backup format",
        ),
        (good.replace("\"app_version\":\"0.1.0\",", ""), "app_version"),
        (good.replace("\"created_at_unix\":1,", ""), "created_at_unix"),
        (
            good.replace(&sha(b"PGDMP"), "ABC"),
            "'db_dump_sha256' is invalid",
        ),
        (
            good.replace("\"vault_objects\":0", "\"vault_objects\":-1"),
            "vault_objects",
        ),
    ];
    for (text, needle) in cases {
        std::fs::write(dir.join("manifest.json"), &text).unwrap();
        refused(&dir, needle).await;
    }
}

#[tokio::test]
async fn a_changed_or_missing_dump_is_refused() {
    let dir = scratch("dump");
    make_backup(&dir, b"PGDMP-original", &[]);
    std::fs::write(dir.join("db.dump"), b"PGDMP-tampered").unwrap();
    refused(&dir, "checksum mismatch").await;
    std::fs::remove_file(dir.join("db.dump")).unwrap();
    refused(&dir, "db.dump").await;
    // a directory where the dump should be
    std::fs::create_dir(dir.join("db.dump")).unwrap();
    refused(&dir, "not a file").await;
}

#[tokio::test]
async fn vault_objects_are_checked_against_their_names_and_the_manifest() {
    let dir = scratch("objects");
    make_backup(&dir, b"PGDMP", &[b"alpha", b"beta"]);

    // an object whose content is not its hash (bit rot, tampering)
    let hash = sha(b"alpha");
    let path = dir
        .join("vault/objects")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(&hash);
    std::fs::write(&path, b"alphA").unwrap();
    refused(&dir, "is corrupt").await;
    std::fs::write(&path, b"alpha").unwrap();

    // an object that the manifest does not know / one that is missing
    let extra = b"gamma";
    let h = sha(extra);
    let folder = dir.join("vault/objects").join(&h[0..2]).join(&h[2..4]);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(&h), extra).unwrap();
    refused(&dir, "manifest says 2").await;
    std::fs::remove_file(folder.join(&h)).unwrap();
    std::fs::remove_file(&path).unwrap();
    refused(&dir, "manifest says 2").await;
    std::fs::write(&path, b"alpha").unwrap();

    // stray files and misplaced objects in the object tree
    std::fs::write(dir.join("vault/objects/readme.txt"), b"x").unwrap();
    refused(&dir, "unexpected file").await;
    std::fs::remove_file(dir.join("vault/objects/readme.txt")).unwrap();
    std::fs::create_dir_all(dir.join("vault/objects/zz")).unwrap();
    refused(&dir, "two-digit hex folder").await;
    std::fs::remove_dir(dir.join("vault/objects/zz")).unwrap();
    let wrong = dir.join("vault/objects/00/00");
    std::fs::create_dir_all(&wrong).unwrap();
    std::fs::write(wrong.join(&hash), b"alpha").unwrap();
    refused(&dir, "content-addressed folder").await;
    std::fs::remove_dir_all(dir.join("vault/objects/00")).unwrap();
    std::fs::write(
        dir.join("vault/objects")
            .join(&hash[0..2])
            .join(&hash[2..4])
            .join("not-a-hash"),
        b"x",
    )
    .unwrap();
    refused(&dir, "unexpected object").await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_in_the_vault_are_not_followed() {
    let dir = scratch("symlink");
    make_backup(&dir, b"PGDMP", &[b"alpha"]);
    let hash = sha(b"alpha");
    let path = dir
        .join("vault/objects")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(&hash);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();
    refused(&dir, "not a regular file").await;
}

#[test]
fn a_failed_backup_leaves_no_manifest_and_refuses_a_used_directory() {
    let vault = scratch("vault");
    let target = scratch("target").join("out");
    // the database is not reachable (or pg_dump is not installed): the
    // run fails, and since the manifest is written last, what is left in the
    // directory is never accepted as a backup
    let err = backup::backup(&config(&vault), &target);
    assert!(err.is_err());
    assert!(!target.join("manifest.json").exists());

    // a directory with content is never used as a target
    let used = scratch("used");
    std::fs::write(used.join("keep.txt"), b"mine").unwrap();
    let err = backup::backup(&config(&vault), &used).unwrap_err();
    assert!(format!("{err:#}").contains("not empty"), "{err:#}");
    assert_eq!(std::fs::read(used.join("keep.txt")).unwrap(), b"mine");

    // a file is not a directory
    let file = used.join("keep.txt");
    let err = backup::backup(&config(&vault), &file).unwrap_err();
    assert!(format!("{err:#}").contains("not a directory"), "{err:#}");
}
