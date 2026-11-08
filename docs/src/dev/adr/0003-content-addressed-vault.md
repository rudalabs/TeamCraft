# ADR 0003 – Files live in a content-addressed vault on the local file system

**Status:** accepted – part of the first version; backup checks added in #93.

## Context

Revisions carry files – CAD models, drawings, exports, simulation results –
that are large, often identical across revisions (a revision copies the files
it does not change) and must never change once a version is checked in. The
database needs to name a file version; the bytes need somewhere to live.

## Decision

* **Named by content.** `src/vault.rs` stores every blob under
  `objects/ab/cd/<sha256>`, named by the SHA-256 of its content. The database
  stores the hash (`files.sha256`, `file_versions.sha256`,
  `change_request_attachments.sha256`).
* **Write once.** An upload goes to `tmp/` on the same file system and is
  renamed into place when complete (atomic); a blob is never modified after
  it was written. Identical files are stored once, however many revisions
  reference them.
* **Chunked uploads** are staged in `uploads/<session>` until they are finished (`services::uploads`).
* **Derived data is separate:** the preview cache (`previews/`) is not part of
  the vault's content and is rebuilt when missing.
* **Backups check the vault.** Since #93, `teamcraft backup` hashes every object
  against its name while copying it and requires every content the database
  refers to (`services::files::all_hashes`, the same list `teamcraft vault
  check` uses); a corrupt or missing object refuses the backup, and a failed
  backup removes what it wrote. `teamcraft restore` verifies every object
  before anything is restored and copies the vault only after the database.

## Consequences

* Integrity is checkable at any time: `teamcraft vault check
  [--verify-hashes]` compares the referenced hashes with the vault.
* De-duplication is free; downloads can carry the hash as a strong ETag and
  resume half-way (`src/httprange.rs`).
* The code has no function that deletes an object: content of deleted items
  or failed check-ins stays in the vault as unreferenced blobs. There is no
  garbage collection.
* The vault is a local directory; several servers would need a shared file
  system.

## Alternatives turned down

Not documented in the repository: no research note, commit message or
`OPEN_WORK.md` entry records whether storing files in the database (large
objects / `bytea`) or in an object store (S3-compatible) was considered.

## Where it is held

| What                                                                                       | Where                                                   |
|--------------------------------------------------------------------------------------------|---------------------------------------------------------|
| Layout, atomic write, verify                                                               | `src/vault.rs`                                          |
| Consistency check                                                                          | `teamcraft vault check` in `src/main.rs`                |
| Backup and restore with object checks                                                      | `src/backup.rs`                                         |
| Corrupt or missing objects refuse the backup; a failing restore leaves the vault untouched | `tests/backup_proofing.rs`, `tests/backup_errors.rs`    |
| Backup and restore end to end                                                              | `tests/backup_restore.rs`                               |
| Vault and audit trail                                                                      | `tests/audit_and_vault.rs`, `tests/domain_and_vault.rs` |
