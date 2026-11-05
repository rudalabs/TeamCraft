# Test coverage map

Where the tests are, where they are thin, and what to write next. The page
is the answer to *"which code can break without a test noticing?"* – it is
meant to be updated, not admired. Last measured: 2026-11-05.

## Method

* **Lines of code** – `wc -l` of every file under `src/` (blank lines and
  comments included) and `desktop/src/`.
* **Unit tests** – the number of `#[test]` / `#[tokio::test]` functions in the
  module's own file (`src/simmesh/tests.rs` belongs to `simmesh.rs`).
* **Integration tests** – `tests/*.rs` talk to the real router (HTTP), so the
  routes and services they reach are mapped by what the test asserts, not by
  imports. The table below shows only tests that name the module directly
  (`teamcraft::module::…`); the *HTTP* column of the area tables lists the
  test files that drive the code through the API or the web UI.
* **Measured line coverage** – `cargo llvm-cov` (0.9, `llvm-tools-preview`):

  ```sh
  cargo install cargo-llvm-cov --locked && rustup component add llvm-tools-preview
  cargo llvm-cov --lib --summary-only            # unit tests only
  cargo llvm-cov --summary-only --no-fail-fast   # every test that runs without PostgreSQL
  cargo llvm-cov --html                          # report in target/llvm-cov/html
  ```

  Two caveats. (1) Inline `#[cfg(test)]` code counts as covered lines, so the
  percentage of a module with a large test section is too high – the number of
  **missed lines** is the honest figure and is what the tables show.
  (2) On a machine without PostgreSQL the database tests skip themselves, so
  `services/*`, `web/*` and `api/*` show up as uncovered in the local numbers
  although the integration tests exercise them. **The CI job *coverage* runs
  everything against PostgreSQL 16** and uploads the real report (see
  below); the tables here give the local, no-database numbers and name the
  integration tests by hand.
* **Ranking** – *risk × thinness*. Risk: untrusted input (file parsers, HTTP
  bodies, tokens), data integrity (multi-row mutations, vault, backup,
  migrations, imports, uploads), size of the code. Thinness: tests per 100
  lines, missed lines, and whether failure paths (refusal, conflict,
  rollback, damaged input) are tested at all and not only the happy path.

## Coverage in CI

The job *coverage (report only)* in `.github/workflows/ci.yml` runs
`cargo llvm-cov` over the complete test suite with PostgreSQL 16 and the
client tools, prints the per-file summary into the job summary and uploads
the HTML report (`coverage-html`, 14 days) as an artifact. It has **no
threshold yet**: when a few pull requests have shown the normal variation, a
floor that may not drop is added (open item in `docs/OPEN_WORK.md`).

## Areas

Numbers are *missed lines* / lines, from `cargo llvm-cov` without a
database (before this work → after, where tests were added).

| Area | Code | Tests | Measured (no DB) | HTTP / integration tests |
|---|---|---|---|---|
| File parsers (`mesh*`, `simmesh`, `stepbom`, `swpart`, `nxpart`, `thumbnail`, `cadrefs`, `bomtable`, `textindex`, `cadformat`, `shape`) | 13.7k lines | 166 unit (incl. 19 in `simmesh/tests.rs`); `cad_fixtures`, `parser_robustness` | 4–130 missed per module, 91–99 % | `cad_fixtures` (real NIST files), `uploads`, `web_ui`, `parser_robustness` (new) |
| `services/*` (business logic, the only writer) | 21.9k lines | 63 unit | not measurable without PostgreSQL (6–67 % of lines run in unit tests) | `api_flow`, `read_permissions`, `change_orders`, `change_requests`, `concurrency`, `uploads`, `webhooks`, `structure`, `numbering_and_watches`, `release_readiness`, `reviewers`, `audit_and_vault`, `interchange`, `working_folder`, `similar_parts`, `attributes`, `search_listing`, `scale`, `bom_performance`, `import_atomicity` (new), `service_refusals` (new), `migrations` (new) |
| `web/*`, `api/*` (thin handlers) | 10.9k lines | 2 unit | – | `web_ui`, `api_flow`, `no_unchecked_in_handlers` (architecture guard) |
| `domain.rs`, `config.rs`, `util.rs`, `error.rs` | 4.4k lines | 46 unit | 5–167 missed | all of them; `domain_and_vault` (new) |
| `auth.rs`, `oidc.rs` | 0.9k lines | 10 unit | 97 / 180 missed (63 % / 53 %) | `api_flow`, `read_permissions` (tokens, sessions); OIDC has no integration test |
| `vault.rs`, `backup.rs`, `db.rs` | 1.5k lines | 14 unit | vault 62 → 53, backup 142 → 113, db 85 of 105 missed | `audit_and_vault`, `uploads`, `backup_restore`, `backup_errors` (new), `migrations` (new) |
| `webhooks.rs`, `delivery.rs`, `notify.rs` | 1.5k lines | 14 unit | 242 / 252 / 48 missed | `webhooks`, `api_flow` (e-mail outbox), no test against a real SMTP/Mattermost server |
| `assistant.rs`, `llm/*`, `mcp/*` | 5.6k lines | 27 unit | mcp/tools 1197 of 2246 missed | `assistant`, `mcp`, `read_permissions` (owned by other work in progress, not touched here) |
| Desktop (`desktop/src`, own crate, not measured: needs WebKitGTK) | 8.6k lines | 66 unit | estimate: pure helpers (`layout`, `abortable`, `api`, `settings`) tested, Tauri commands (`commands.rs` 1.2k lines, 3 tests), `workspace.rs` (1.5k, 6) and `acp.rs` (2k, 21) thin | none (CI runs the desktop tests on Linux only) |

## Top 15: the thinnest high-risk spots

Ranked by risk × thinness; *tests added* is what this change brought.

| # | Spot | Why it is risky | Before | Tests added | What the new tests pin |
|---|---|---|---|---|---|
| 1 | `services/imports.rs` (1.2k lines) + `bomtable`/`stepbom` import path | Multi-row write from untrusted tables; one transaction must hold | 6 unit, one happy-path HTTP flow | 4 DB tests in `tests/import_atomicity.rs` (+ the damage tests of #14) | refused import writes no row, counter step or audit entry (permissions, unreadable tables, unknown group); an injected database failure (trigger) while the third part is written rolls back parts, assembly, lines, counters and audit entries, and the retry succeeds; re-import creates no duplicate parts; dry-run plan equals what the import writes |
| 2 | `db.rs` + `migrations/` | A bad upgrade destroys every installation | 1 unit test (server version) | 4 in `tests/migrations.rs` | numbering without duplicates, the known gap 0015; all files apply on an empty database; second run is a no-op and the schema equals the harness schema; a changed checksum stops the upgrade unless the version is in `EDITED_BEFORE_RELEASE`; a half-applied migration blocks |
| 3 | `backup.rs` restore path | A restore is the last line of defence | 9 unit, one happy-path e2e, 142 missed lines | 7 in `tests/backup_errors.rs` (no database needed) | missing/interrupted backup (no manifest), damaged manifest fields, changed or missing dump, corrupt/extra/missing/misplaced vault objects, symlinks – every refusal happens before anything is written; a failed backup leaves no manifest and never reuses a non-empty directory |
| 4 | `services/changes.rs` (1.4k lines, 0 unit) | Release of a change order flips many revisions at once | integration only | – | **open**: atomicity under an injected failure (see below) |
| 5 | `services/files.rs` (1.1k lines, 3 unit), `services/uploads.rs` | Check-in, versions, chunked uploads: data loss on races | integration only (`concurrency`, `uploads`) | – | **open**: failure between file versions and lock release |
| 6 | `services/items.rs`, `revisions.rs` (1.6k lines, 0 unit) | Creation, numbering, lifecycle | integration only | 3 DB tests in `tests/service_refusals.rs`, 7 DB-free in `tests/domain_and_vault.rs` | refused creation leaves no row/audit entry (duplicate number 409, read-only role, other group, bad type/group/name); concurrent creation with one number → one 201 and one 409; concurrent numbering gives distinct numbers; lifecycle table is exactly the documented four transitions; revision letters round-trip and skip I O Q S X Z; name/number/user/project/password boundaries |
| 7 | `services/bom.rs` (1.4k lines) | Cycles and quantities corrupt every roll-up | 10 unit, `structure` | 2 DB tests in `service_refusals` | cycles (direct and via a sub-assembly), zero/negative quantity, unknown child, BOM on a part: all refused, nothing written; one winner for two lines with the same position |
| 8 | `services/checkouts.rs` | The lock is the integrity mechanism of the vault | 2 unit, `concurrency` | 1 DB test | three concurrent check-outs → one 200, two 409, one holder, audited |
| 9 | `services/users.rs`, `auth.rs`, `oidc.rs` | Sessions, tokens, SSO – untrusted input, 6.7 % / 63 % / 53 % lines run in unit tests | 2 / 6 / 4 unit | – | **open**: OIDC callback state/nonce/PKCE failures, token expiry and revocation |
| 10 | `webhooks.rs` + `services/webhooks.rs` (SSRF guard, signing) | Server-side requests to configurable URLs | 8 + 4 unit, `webhooks` | – | **open**: guard against private/link-local/IPv6-mapped targets and DNS rebinding |
| 11 | `mesh.rs` readers (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, 3DS, points, LAS) | Untrusted bytes, big allocations | 12 unit + fixtures, 130 missed | 7 in `tests/parser_robustness.rs` | every prefix and 400 corruptions of each sample never panic and yield only consistent meshes (indices in range, finite, survives the cache encoding); claimed sizes (4 billion triangles, 10⁹ vertices) are refused; random bodies behind the magic numbers; **bug found and fixed**: a glTF primitive naming a non-existent accessor panicked inside the glTF library |
| 12 | `simmesh.rs` (2k lines, VTK/VTU/FRD) | Untrusted, 1.5k lines of hand-written parsing | 19 unit in `simmesh/tests.rs` | covered by the damage tests (VTK legacy, VTU) | prefixes and corruptions of VTK legacy and XML samples; FRD has no hand-made sample yet |
| 13 | `swpart.rs`, `nxpart.rs`, `thumbnail.rs` (containers with offsets and Deflate) | Offsets, lengths, decompression bombs | 14 / 7 / 12 unit + NIST files | 3 tests | every prefix and corruption of an NX container, random bytes behind SOLIDWORKS/OLE/NX/PNG magics for the thumbnail reader, hostile component XML, `inflate` with a lying size |
| 14 | `stepbom.rs`, `bomtable.rs`, `cadrefs.rs`, `textindex.rs`, `cadformat.rs` | Untrusted text/spreadsheets; heuristic | 13 / 19 / 18 / 9 / 19 unit | 6 tests | damaged CSV/TSV/XLSX/ODS and STEP structure (dangling and self references) never panic and keep quantities finite and levels ≥ 1; names found by the reference scan are bare file names; odd file names and heads in format detection |
| 15 | `vault.rs` | Content-addressed storage: the bytes of every revision | 4 unit, 62 missed | 3 tests in `tests/domain_and_vault.rs` | a blob changed on disk fails `verify`, a missing blob is `NotFound`; hashes cannot escape the object tree; an upload over its limit is refused and leaves no object |

Not in the top 15 but thin: `delivery.rs` (252 of 324 lines not run without a
server), `services/requests.rs` (1.9k lines, 5 unit), `services/simulations.rs`,
`services/notifications.rs`, `desktop/src/commands.rs`, `config.rs` error paths.

### Found while writing the tests

* **Fixed** – `mesh::read("gltf", …)` panicked (index out of bounds inside
  `gltf-json`) for a primitive whose `POSITION` points past the accessor list.
  The preview runs in a child process, so the effect was a failed preview and a
  log entry, but a library panic must not be reachable from an upload. The
  library call is now wrapped in `catch_unwind` and yields an error;
  regression test `gltf_accessor_index_out_of_range_is_an_error`.
* **Observation, not a bug of its own** – `backup::backup` copies vault
  objects without checking their hash; a corrupt object is found only when
  the backup is verified (`restore`). `docs/OPEN_WORK.md` asks for "a vault
  object missing or corrupt during backup (refused, nothing written)": that
  needs `pg_dump` and a database, so it is left for a test in
  `tests/backup_restore.rs`.

### What to write next

1. Atomicity under an injected failure for check-in, change-order release,
   *Replace in assemblies*, Save As and chunked-upload `finish` (needs a
   failure hook in the services or a trigger that raises in the test
   database – `CREATE TRIGGER … RAISE EXCEPTION` on the last table written is
   the cheap way).
2. OIDC callback failure paths with a fake provider; token expiry/revocation.
3. SSRF guard table test for webhooks (pure function, no database).
4. FRD sample and a SOLIDWORKS container built by hand in the damage tests.
5. `cargo fuzz` targets for the parsers (`docs/OPEN_WORK.md`), fed with the
   samples of `tests/parser_robustness.rs` as seed corpus.
6. A floor for the CI coverage job once its numbers are stable.

## Per module

`Missed` is *unit tests only / all tests that run without PostgreSQL* of the
lines `cargo llvm-cov` counts (not the file length). *Integration* names the
test files that use the module by name; the HTTP-driven coverage of
`services/*` is in the area table above. `main.rs` is the CLI and is covered
by the CLI smoke test in CI only.

| Module | LOC | Unit tests | Missed / counted lines | Integration (direct use) |
|---|---|---|---|---|
| `assets.rs` | 413 | 8 | 2 / 2 of 280 | web_ui |
| `assistant.rs` | 1498 | 8 | 167 / 167 of 1024 | read_permissions |
| `auth.rs` | 410 | 6 | 97 / 97 of 262 | api_flow,bom_performance |
| `backup.rs` | 857 | 9 | 142 / 113 of 643 | backup_errors,backup_restore |
| `bomtable.rs` | 912 | 19 | 8 / 8 of 699 | api_flow,parser_robustness |
| `cadformat.rs` | 1372 | 19 | 4 / 4 of 771 | cad_fixtures,parser_robustness |
| `cadrefs.rs` | 629 | 18 | 7 / 4 of 442 | cad_fixtures,parser_robustness |
| `config.rs` | 1079 | 13 | 49 / 38 of 769 | api_flow,backup_errors,read_permissions +2 |
| `db.rs` | 185 | 1 | 85 / 85 of 105 | api_flow,migrations,scale |
| `delivery.rs` | 477 | 4 | 252 / 252 of 324 | api_flow,read_permissions |
| `domain.rs` | 2980 | 28 | 167 / 162 of 1764 | api_flow,assistant,attributes +23 |
| `error.rs` | 133 | 0 | 47 / 47 of 57 | change_orders,domain_and_vault,read_permissions |
| `lib.rs` | 212 | 0 | 98 / 98 of 98 | – |
| `llm/anthropic.rs` | 410 | 5 | 7 / 7 of 311 | – |
| `llm/http.rs` | 84 | 0 | 58 / 58 of 58 | api_flow,assistant,attributes +22 |
| `llm/mod.rs` | 227 | 0 | 7 / 7 of 47 | – |
| `main.rs` | 902 | 3 | – | – |
| `mcp/mod.rs` | 290 | 2 | 88 / 88 of 167 | – |
| `mcp/tools.rs` | 3055 | 12 | 1197 / 1197 of 2246 | read_permissions |
| `mesh.rs` | 1929 | 12 | 130 / 112 of 1526 | cad_fixtures,parser_robustness,web_ui |
| `mesh/p21.rs` | 672 | 5 | 27 / 27 of 459 | – |
| `mesh/step.rs` | 529 | 4 | 17 / 17 of 351 | – |
| `mesh/tessellated.rs` | 438 | 6 | 11 / 9 of 359 | – |
| `models.rs` | 586 | 0 | 79 / 79 of 79 | – |
| `notify.rs` | 116 | 2 | 48 / 48 of 65 | – |
| `nxpart.rs` | 397 | 7 | 4 / 3 of 262 | parser_robustness |
| `oidc.rs` | 492 | 4 | 180 / 180 of 387 | – |
| `preview.rs` | 722 | 3 | 118 / 118 of 515 | cad_fixtures |
| `services/access.rs` | 159 | 1 | 30 / 30 of 61 | no_unchecked_in_handlers |
| `services/assistant.rs` | 191 | 0 | 129 / 129 of 129 | read_permissions |
| `services/attributes.rs` | 307 | 0 | 251 / 251 of 251 | – |
| `services/audit.rs` | 322 | 0 | 242 / 242 of 242 | read_permissions |
| `services/bom.rs` | 1435 | 10 | 388 / 388 of 1004 | bom_performance |
| `services/bom_compare.rs` | 601 | 4 | 28 / 28 of 385 | – |
| `services/changes.rs` | 1393 | 0 | 1223 / 1223 of 1223 | api_flow,change_orders,read_permissions |
| `services/checkouts.rs` | 376 | 2 | 159 / 159 of 240 | concurrency |
| `services/desktop.rs` | 234 | 2 | 69 / 69 of 146 | web_ui |
| `services/explorer.rs` | 164 | 0 | 80 / 80 of 80 | – |
| `services/files.rs` | 1128 | 3 | 763 / 763 of 874 | api_flow |
| `services/impacts.rs` | 1173 | 3 | 655 / 655 of 786 | – |
| `services/imports.rs` | 1197 | 6 | 721 / 721 of 860 | – |
| `services/insights.rs` | 276 | 0 | 187 / 187 of 187 | – |
| `services/interchange.rs` | 708 | 0 | 496 / 496 of 496 | – |
| `services/items.rs` | 1023 | 0 | 724 / 724 of 724 | api_flow,bom_performance,read_permissions |
| `services/mod.rs` | 86 | 0 | 27 / 27 of 27 | – |
| `services/notifications.rs` | 838 | 2 | 430 / 430 of 544 | read_permissions |
| `services/package.rs` | 181 | 0 | 121 / 121 of 121 | – |
| `services/personal.rs` | 169 | 1 | 97 / 97 of 122 | – |
| `services/projects.rs` | 688 | 0 | 558 / 558 of 558 | api_flow,read_permissions |
| `services/readiness.rs` | 627 | 6 | 139 / 139 of 423 | – |
| `services/references.rs` | 506 | 0 | 352 / 352 of 352 | – |
| `services/requests.rs` | 1942 | 5 | 1426 / 1426 of 1587 | read_permissions |
| `services/reviewers.rs` | 446 | 2 | 49 / 49 of 188 | – |
| `services/revisions.rs` | 594 | 0 | 490 / 490 of 490 | – |
| `services/save_as.rs` | 173 | 0 | 130 / 130 of 130 | – |
| `services/similar.rs` | 677 | 1 | 365 / 365 of 434 | – |
| `services/simulations.rs` | 1558 | 4 | 725 / 725 of 1176 | – |
| `services/stats.rs` | 44 | 0 | 26 / 26 of 26 | – |
| `services/uploads.rs` | 644 | 5 | 328 / 328 of 457 | uploads |
| `services/users.rs` | 836 | 2 | 644 / 644 of 690 | api_flow,read_permissions,scale +1 |
| `services/webhooks.rs` | 915 | 4 | 492 / 492 of 679 | read_permissions,webhooks |
| `services/workset.rs` | 304 | 0 | 192 / 192 of 192 | bom_performance |
| `shape.rs` | 1031 | 9 | 20 / 20 of 771 | cad_fixtures |
| `simmesh.rs` | 1962 | 0 | 81 / 81 of 1455 | – |
| `simmesh/tests.rs` | 981 | 19 | – | – |
| `stepbom.rs` | 865 | 13 | 8 / 5 of 655 | parser_robustness |
| `swpart.rs` | 974 | 14 | 8 / 8 of 653 | parser_robustness |
| `templates.rs` | 1576 | 3 | 6 / 6 of 1230 | – |
| `textindex.rs` | 293 | 9 | 1 / 1 of 195 | parser_robustness |
| `thumbnail.rs` | 719 | 12 | 23 / 16 of 541 | cad_fixtures,parser_robustness |
| `util.rs` | 156 | 5 | 1 / 1 of 104 | api_flow,web_ui |
| `vault.rs` | 434 | 4 | 62 / 53 of 324 | domain_and_vault |
| `webhooks.rs` | 930 | 8 | 242 / 242 of 582 | read_permissions,webhooks |

`web/*` (22 files, 7.1k lines, plus `web/mod.rs`) and `api/*` (7 files, 3.7k lines) are left out: they are thin handlers over `services::*`, reached only through HTTP by `web_ui`, `api_flow` and the feature tests; without a database none of their lines run (0 %).
