# Open work

The to-do list of TeamCraft, in order of priority, then what is done, then details on the open features.
Tick an entry (`[x]`) when it is done and move it to *Done*. Last update: 2026-11-05.

## Direction

TeamCraft has more features than proof that they hold up. The next effort – of contributors and of AI agents alike –
goes into **proofing and structure, not new features**:

1. **Proofing** – tests that catch real regressions, above all in the processes that must never leave half a state
   behind (migrations, backup/restore, imports, multi-row mutations), fuzzing of every file parser, CI gates.
2. **Structure** – smaller modules with one job each, guard tests for the architecture rules, less duplication
   between web UI, REST API and MCP.
3. **Fixes and verification** – known bugs, checks with real CAD data and real servers.
4. **Parked features** – only finish what is already in progress (custom attributes #60, scale check #47) and fix
   bugs in existing features. A new feature needs an issue with a reason first.

Rules for every change, features included:

- Tests for the failure paths, not only the happy path: refused permission, conflict (409), rollback on error.
- Merge only with every check green – never "fix the tests later" (#65 was merged red, fixed in #67).
- Refactorings keep behaviour: no feature work and no behaviour change in the same PR as a move or split.
- AI agents pick their next task from sections 1 and 2 unless the maintainer asks for something else.

## Road to v1.0.0 (general availability)

**What 1.0 promises.** Teams can run TeamCraft in production and upgrade it without fear: data is never lost or
silently changed, upgrades between 1.x versions are automatic, the REST API, MCP tools, configuration and CLI are
stable (breaking changes only in 2.0), and security issues get fixed releases. 1.0 is a quality milestone, not a
feature milestone – the features in *What works* are enough; what is missing is proof, stability and operations.

**Milestones** – each is a pre-release with release notes; nothing moves to the next one with an open blocker.

| Version | Theme | Exit criteria |
|---|---|---|
| **0.2** | Proofing | Sections 1 and 2 of the to-do list mostly done: migration, backup, import and atomicity tests, fuzzing of every parser, `cargo deny`, coverage report, branch protection; the large modules split |
| **0.3** | Real data | The CAD-program checks of section 3 with real files (Inventor, Solid Edge, SOLIDWORKS, NX, STEP exports); scale check (#47) with targets met; e-mail/Mattermost against real servers |
| **0.5** | Beta – frozen interfaces | Assistant with OpenAI-compatible providers shipped (its config is part of the freeze); API, MCP tools, config variables and CLI reviewed and frozen (deprecations marked); upgrade path tested from 0.2 on; first outside test teams |
| **0.9** | Release candidate | No known data-loss or security bug; docs complete; one RC per fix round, two weeks without a blocker |
| **1.0.0** | General availability | Work-in-progress notices removed (README, docs, web footer, desktop *About*); support policy published |

**GA criteria** (all must be ticked before 1.0.0)

*Data safety and upgrades*

- [ ] Schema upgrades from every 0.x release since 0.2 and between all 1.x versions are automatic and tested in CI
  with seeded databases (see *Migrations* in section 1); no edited migrations after 0.5 (`db::EDITED_BEFORE_RELEASE`
  empty and frozen)
- [ ] Backup/restore tested for every release in CI, including restore of an older version's backup; documented
  restore drill; `teamcraft vault check` finds and reports every inconsistency between database and vault
- [ ] All multi-row mutations proven all-or-nothing and safe under concurrency (section 1, *Atomic processes*)
- [ ] Upgrade guide in the docs (`docs/src/admin/upgrade.md`): before/after steps, rollback by restore, breaking
  changes per version

*Stable interfaces*

- [ ] REST API `/api/v1` reviewed: consistent names, error shape, pagination and filters on every list; an OpenAPI
  description generated from the routes and checked in CI against the router; deprecation policy (an endpoint lives
  at least one minor version with a `Deprecation` header)
- [ ] MCP tools and prompts: names and argument schemas frozen and versioned, contract tests per tool
- [ ] Configuration: every variable documented and validated at start (unknown `TEAMCRAFT_*` variables warned),
  defaults reviewed; CLI commands and exit codes documented and frozen
- [ ] Desktop app ↔ server compatibility: version handshake, the desktop tells the user when the server is too old
  or too new; a supported-versions table

*Assistant*

- [ ] One provider-neutral LLM API layer (`src/llm/`) with the Anthropic Messages backend and an OpenAI-compatible
  Chat Completions backend (OpenAI, Azure OpenAI, Mistral, OpenRouter, Groq, local Ollama/vLLM/LM Studio) – phases 1
  and 2 of [agent-interface](src/research/agent-interface.md): provider, base URL, model and key (`_FILE`) in the
  configuration, `teamcraft assistant check`, the loop tests run for every provider, recorded fixtures per server
- [ ] Per-user daily budget and admin feature switches for the assistant (phase 4), so that an operator can enable
  it with a bought key without surprises

*Security*

- [ ] Threat model in `docs/src/dev/` (vault, sessions, PATs, OIDC, uploads, file parsers, webhooks, MCP/assistant);
  every finding resolved or documented
- [ ] External review or pentest of authentication, read permissions and file parsing; fuzzing of all parsers
  running continuously (section 1)
- [ ] `cargo deny`/`cargo audit` green in CI, SBOM (CycloneDX) attached to each release, release artifacts with
  build provenance (GitHub attestations); private vulnerability reporting enabled (`SECURITY.md`)
- [ ] Session, token and password policies reviewed (expiry, rotation, rate limiting of sign-in and API)

*Operations*

- [ ] Metrics endpoint (Prometheus) for requests, latency, outbox backlogs (notifications, webhooks), preview
  queue, database pool; structured JSON logs; documented alerts
- [ ] Performance targets met and checked by the scale harness (#47): e.g. 1 M files, 100 k items, BOMs of 10 k
  lines, 50 concurrent users – page and API p95 times written down and guarded in a scheduled CI job
- [ ] Production install paths tested end to end per release: Docker Compose (`deploy/`), Linux packages (systemd
  unit), behind a reverse proxy with TLS; documented sizing and PostgreSQL versions (16–18)
- [ ] Data retention and cleanup: old previews, staged uploads, expired sessions, audit log retention setting
- [x] Preview safety: size caps per kind and a memory limit for the preview process (`TEAMCRAFT_PREVIEW_MAX_*`); big result files are stored, never parsed

*Clients and packaging*

- [ ] Flatpak on Flathub with offline cargo sources; AUR packages published; the desktop app's update notice (no
  silent auto-update). Binaries and installers stay **unsigned** (no code signing or notarisation): the checksums
  in `SHA256SUMS.txt` and the build provenance are the verification, and the docs explain the first start past
  SmartScreen and Gatekeeper
- [ ] Desktop app run and tested on Windows x64/ARM64, macOS and Linux for each RC (a short manual test script in
  `docs/src/dev/release-checklist.md`)
- [ ] Accessibility: keyboard use and screen-reader labels on the main web flows checked (WCAG 2.2 AA for contrast
  is already tested)

*Documentation and project*

- [ ] `CHANGELOG.md` (Keep a Changelog) from 0.2 on, written per PR; release notes from it
- [ ] User guide, admin guide and API/MCP reference complete and checked against the 1.0 behaviour; screenshots
  current
- [ ] Support and versioning policy: SemVer from 1.0, which versions get security fixes (e.g. the latest minor and
  the previous one for 6 months), how to report bugs
- [ ] Release checklist (`docs/src/dev/release-checklist.md`): green main, RC soak time, version bump in both
  `Cargo.toml` files, tag, verify assets and checksums, image tags, announcement
- [ ] Governance basics: maintainers, review rule (one approving review, green checks), code of conduct

**Not required for 1.0** – CAD add-ins, more 3D readers, the Jira connector, code-signed binaries, the assistant
features after the budget (threads, streaming, quick actions), the shared UI folder beyond what is already merged, i18n of the UI (English only in 1.0; the
structure should not prevent translations later). They may land in 1.x minor releases.

## To-do

### 1. Proofing

**Tests** (priority: more unit tests)

The feature list is covered broadly since #65 (and #67); the weak spots are the processes that must never leave
half a state behind. Prefer tests that fail on a real regression over tests that restate the code.

- [ ] **Migrations** – today only `db::supported_postgres_versions` is unit-tested. Add: every migration applies in
  order on an empty database and the schema matches a fresh `migrate` (no drift between numbered files); a database
  with data from the previous release (seeded fixture, e.g. v0.1.0) upgrades without loss (row counts, foreign keys,
  CHECK constraints of the enum columns); applying twice is a no-op; a changed checksum of an applied migration is
  refused unless the version is in `db::EDITED_BEFORE_RELEASE`; numbering of `migrations/` has no duplicates and the
  gaps are known (0015)
- [ ] **Backup / restore** – beyond `tests/backup_restore.rs` and the argument/manifest tests in `src/backup.rs`:
  restore of a backup from an older schema version (then migrate), a vault object missing or corrupt during
  backup (refused, nothing written), interrupted backup leaves no directory that `verify` accepts, restore into a
  non-empty target without `--force` is refused, `--force` failure leaves database *and* vault untouched (vault side
  not covered yet), percent-encoded and `_FILE` credentials, large-object and preview-cache handling
- [ ] **Import** – BOM import (`services::imports`, `bomtable.rs`, `stepbom.rs`): the dry-run plan equals what the
  import writes (property test over generated tables), an error in row *n* rolls back rows 1…*n*-1, re-import of the
  same table is idempotent (no duplicate items or BOM lines), cycles and self-references refused, quantity/unit
  parsing edge cases (decimal comma, `1.1.2` positions with gaps, empty levels), malformed STEP (truncated, unknown
  entities, `QUANTIFIED_ASSEMBLY_COMPONENT_USAGE`) never panics (fuzz-style corpus); desktop folder import resumes
  after an interrupted run without duplicates
- [ ] **Atomic processes** – each multi-row mutation is all-or-nothing under an injected failure (a failing
  statement or a closed connection between steps): check-in (file versions + lock release), change order release
  (all revisions or none, impact flags, notifications and webhooks in the same transaction), *Replace in assemblies*,
  Save As with files and BOM, chunked upload `finish`, folder import per item, restricted-group visibility switch;
  concurrent runs of the same mutation (two check-ins, two releases, two numbering requests) give one winner and a
  clean 409, never a duplicate or a lost update
- [ ] Coverage map: keep the feature → tests table of #65 in `CONTRIBUTING.md` (or `docs/src/dev/`) up to date, and
  make CI fail when the integration tests are skipped (`TEAMCRAFT_REQUIRE_DB_TESTS=1`, already set) – #65 was merged
  with two failing integration tests; merge only with green checks

**Parsers and CI gates**

- [ ] Fuzz targets (`cargo fuzz`, own `fuzz/` crate outside the server build) for every reader of untrusted files:
  `mesh` (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, 3DS, point clouds), `simmesh` (VTK, VTU/VTP, FRD), `stepbom`, `swpart`,
  `nxpart`, `thumbnail`, `cadrefs`, `bomtable`, `textindex`; crashes found become regression unit tests with the
  minimised input; a short fuzz run per parser in a scheduled CI job
- [ ] No `unwrap`/`expect`/indexing panics on file input outside tests: audit `swpart`, `mesh`, `simmesh`, `config`,
  then enable `clippy::unwrap_used`/`expect_used` for the parser modules
- [ ] Property tests (`proptest`) for the pure rules in `domain.rs`: revision letter sequence, lifecycle transition
  table, numbering scheme expansion and counters, skin contrast
- [x] Coverage measured per PR (`cargo llvm-cov`, CI job *coverage*, report only: summary and HTML artifact, see [coverage map](src/dev/coverage.md)) – open: a floor that may not drop, once the numbers of a few PRs are known
- [ ] `cargo deny` in CI: licences compatible with MPL-2.0 (see *Rules for all of them* below), security advisories,
  duplicate major versions
- [ ] Branch protection on `main`: required checks (fmt/clippy, tests on PostgreSQL 16 and 18, builds, docs links)

### 2. Structure

- [ ] Split the largest modules by area, without behaviour changes, one PR each: `src/mcp/tools.rs` (~3000 lines →
  one file per area like `services/`), `src/domain.rs` (~2600 → `domain/{roles,lifecycle,revision,numbering,
  skins,validation}.rs`), `src/api/mod.rs` (~2000 → route table + handlers per area, like `src/web/`),
  `src/services/requests.rs` (~1900), `desktop/ui/app.js` (~4700 → ES modules per view, no bundler)
- [ ] Guard tests for the architecture rules, like `tests/no_unchecked_in_handlers.rs`: no `sqlx` in `src/web`,
  `src/api`, `src/mcp` (holds today – keep it that way); every mutation in `services::*` writes an audit entry (list
  of audit actions checked against the mutating functions); every template in `TEMPLATES` and in
  `every_template_renders`; every config variable in `docs/src/admin/configuration.md`
- [ ] One request/response contract for web, REST and MCP where they do the same thing (e.g. BOM, where-used,
  change requests): shared view structs in `services::*` instead of three hand-built JSON/HTML shapes
- [ ] Typed ids (`ItemId`, `RevisionId`, `ChangeId`, … as newtypes over `Uuid`) at the service boundary, so that ids
  cannot be swapped – start with the services that take several ids
- [ ] Document the invariants per service function (permission check, rows locked, audit action, notifications and
  webhooks emitted) in its doc comment; `cargo doc` then is the reference for reviewers
- [ ] Architecture decision records in `docs/src/dev/` for the decisions that keep coming back: access model (deny
  list on the actor, 404 for hidden), outbox for notifications and webhooks, content-addressed vault, no Node.js

### 3. Fixes and verification

**Fixes**

- [ ] Reference scan: false references in real files – NX parts fixed (stream container, `src/nxpart.rs`); open: noise
  in JT files (gap test in `tests/cad_fixtures.rs`); the STEP header export name (`FILE_NAME`) is ignored; the scan
  (`src/cadrefs.rs`) is a heuristic that was not tested with real files of the CAD programs yet (see the CAD sections)

**Tests with real data**

- [ ] Solid Edge test files: none openly licensed found – create a part, a sheet metal part and an assembly with Solid
  Edge (Community Edition) and publish them under CC0
- [ ] End-to-end test against the server with the fixtures: folder import of `tests/fixtures/cad/nist/mtc-assembly/nx`
  and `…/solidworks` → items, files, references, BOM lines, check-ins, preview images, STEP preview (so far only the
  request order of the import is tested, in `tests/concurrency.rs`)
- [ ] One real project with hundreds of files each from Inventor, Solid Edge, SOLIDWORKS and NX: folder import, preview
  images, STEP export, BOMs – including the heuristics (CAD reference scan, BOM table columns, embedded previews)

**Operations and repository**

- [ ] Test e-mail (SMTP) and Mattermost notifications against real servers (so far only against fakes)
- [ ] Enable private vulnerability reporting (*Settings → Code security → Private vulnerability reporting*), which
  `SECURITY.md` points to
- [ ] Run the desktop app once on macOS – CI runs the tests on Linux only, macOS and Windows are build checks (the
  server is no longer shipped for macOS); Windows 11 ARM64 was run by hand (Parallels VM, 2026-11-05, setup in
  [platforms/windows.md](../platforms/windows.md))
- [ ] Install and start the release Flatpak bundles (x86_64, aarch64) and the portable Windows zips (x64, arm64) once –
  built and checked as files only (#66); GNOME runtime 49 may be end of life on Flathub, move to the current one
- [ ] Release v0.1.0 again from the current `main` (re-tag), when asked – the published v0.1.0 is from `6f68f9a` and
  has no Flatpak, Windows ARM64 or portable builds

### 4. Parked features

Only finish what is in progress and fix bugs here; see *Direction*. The CAD-program entries are mostly checks with
real files and belong to section 3 once such files are at hand.

**Desktop app**

- [ ] Check #58 in the real app (Tauri build, Windows): project groups load again on *Home*, *Refresh* and in the
  dialogs; the same folder imported into a second project group (so far checked in Chromium with a stubbed Tauri)
- [ ] Decide and document what happens when the same CAD files are imported into two project groups: two independent
  items per file with different numbers, while CAD references are matched by file name (import tutorial, user guide)
- [ ] Check the desktop *Import BOM…* in the real app (native file dialog; so far checked in Chromium with a stubbed
  Tauri against a real server)
- [ ] Get: resume a single large file half-way (HTTP range requests; today a cancelled file starts over)
- [ ] Change requests: assign and link a change order from the desktop app (today: in the web UI)
- [ ] Screenshots and short GIFs of the desktop app for the README, and screenshots in the book (the web pages are
  captured in CI by `scripts/screenshots.sh`)

**Web UI and server**

- [ ] Browser check (no console errors, no overflow at 390 px) of `/requests`, `/requests/new` and `/requests/{id}`,
  and the desktop request tab
- [ ] Chunked uploads from the *New item* form of the web UI – it still sends its files in one request (the revision
  exists only afterwards); the attach and check-in forms use `/uploads` (feature 9)
- [ ] Custom attributes per file type (feature 5, items are done)
- [ ] Feature 6 leftovers: local substitutes per BOM line, a warning when a superseded item is added to a BOM in the
  desktop app
- [ ] Numbering schemes: a separate scheme per item type within a group, an instance-wide default other than
  `{TYPE}-{SEQ:6}` (feature 4)
- [ ] Change requests: e-mail digest of new requests per project group
- [ ] Jira connector for change requests and change orders, one-way first (TeamCraft → Jira) – design and order of
  work in [docs/src/research/jira-connector.md](src/research/jira-connector.md)

**BOM import from CAD**

- [ ] Try the STEP assembly import with real exports of Inventor, SolidWorks, Solid Edge, Fusion, CATIA and NX; not
  read yet: complex instances, the quantities of `QUANTIFIED_ASSEMBLY_COMPONENT_USAGE`; the geometry of the parts is
  not split into the new items, a STEP file imported *into* a checked-out revision is not attached to it (see *3c*)
- [ ] CSV/Excel BOM import: test the column recognition with real exports (per CAD program below, also Fusion and
  Creo); CATIA V5 *Bill of Material* text exports are not understood (see *3b*); this includes the *BOM structure*
  values and the attribute columns read since feature 5

**3D preview**

- [ ] Test real STEP exports of the CAD programs, assembly placements (`mapped_item` – parts of an assembly can
  overlap) and length units
- [ ] Readers for IGES, BREP, Parasolid, JT, FBX, DAE, USD and E57 – recognised, not previewed (no pure-Rust readers
  yet; candidate: `e57` crate)

**Simulation management (FEA/CFD)**

- [ ] OpenFOAM case summary (solver, time steps) from `controlDict` – the 3D preview of VTK and CalculiX results is
  done (#43)
- [ ] `.sim` detection: verify the STAR-CCM+ marker with real files (heuristic: "STAR-CCM+" near the start)
- [x] Result values at check-in: `teamcraft-results.json|csv` exchange file and per-group extraction templates (bounded, audited, with source); never from big result files
- [ ] Result values in the item search (e.g. failed simulations, values above a limit) and on the dashboard
- [ ] Desktop: *New simulation from this revision…* on a part/assembly (creates the item with its basis), basis files
  checked out read-only into `basis/`, check-in of selected result time steps only
- [ ] Retention for large result files (`files.retention = 'latest'`), now that chunked resumable uploads exist
- [ ] Save As of a simulation: copy basis links and result values

**All CAD programs**

- [ ] (research first) CAD add-ins through the official APIs (Inventor API, Solid Edge API, SolidWorks API, NX Open):
  check out, "save and check in", "get latest" inside the CAD program – also to report conflicts when a file is opened
  inside the CAD program (the desktop app already asks before opening)
- [ ] Check-in takes dependent files along automatically (from the reference scan)
- [ ] Create viewables (STEP) in the background without blocking the check-in
- [ ] Sync file properties with item attributes: part number, name, material, mass
- [ ] Recognise standard and library parts (Content Center, Toolbox, Standard Parts) and treat them as purchased parts
  instead of importing them again and again
- [ ] Workspace setup wizard (first start or when getting a project group): optional CAD settings to tick – Inventor
  `.ipj`, NX load options, Solid Edge search paths; nothing ticked by default, preview of what would be created, never
  created automatically, existing files never overwritten without asking; can be opened again from the settings

**Autodesk Inventor**

- [ ] Project file (`.ipj`) for the workspace only on request, in the workspace wizard: either create a new `.ipj`
  (workspace = group folder, "Using Unique File Names", library paths adjustable) or choose the team's existing `.ipj`
  and only check/show it
- [ ] Folder import: read the paths of an included `.ipj`; recognise Content Center and library folders
- [ ] Test the reference scan with real `.iam` files, also with derived parts, iParts/iAssemblies and Frame Generator
- [ ] Test the STEP export (built) with real files on Windows – the preview images are verified with the NIST Inventor
  2021 files. Inventor 2027 is registered for COM in the Windows 11 ARM64 test VM (`Inventor.Application`,
  `Inventor.exe /Automation`, found by the app's registry check); the export itself was not tried there yet
- [ ] Test the BOM import with real exports of the BOM editor (structured and parts only); *Phantom* components are
  imported as normal sub-assemblies today (*Reference* components are skipped)
- [ ] Extend the Inventor part of the tutorial: Pack and Go, `.ipj`, Design Assistant for broken references

**Siemens Solid Edge**

- [ ] Folder import and tutorial: Pack and Go, Design Manager, part families
- [ ] Test preview images and the STEP export (built) with real `.par`/`.asm`/`.psm` files
- [ ] Test the reference scan and the BOM import with real files
- [ ] Use file properties such as the Document Number as item number

**SOLIDWORKS**

- [ ] STEP export through the locally installed SOLIDWORKS (API), like Inventor and Solid Edge
- [ ] Test preview images of SOLIDWORKS 2014 and older (OLE files) with real files – 2015 and newer: done
- [ ] Test the SOLIDWORKS 2015+ reader with drawings (`.slddrw`, also a real drawing for the document-manager
  fallback), nested sub-assemblies and other versions than 2018
- [ ] Use the instance counts of the component tree (`swReference`, without `swExcludeFromBOM`) for BOM quantities
- [ ] Take configurations and Toolbox parts into account in the import and the reference scan
- [ ] Test the BOM import with real BOM exports

**Siemens NX**

- [ ] (research first) How other PLMs connect NX (native vs. Teamcenter NX Manager), licences (student/academic) and
  what is feasible for an open-source project
- [ ] Folder import for the master model concept: the drawing `*_dwg*.prt` belongs into the item of the model
- [ ] Test the BOM import with real NX BOM exports
- [ ] STEP export through the local NX (NX Open / journal), on Windows and Linux
- [ ] NX load options for the workspace only on request, in the workspace wizard; NX part of the tutorial

## Done

- [x] Tests across the feature list: functional and semantic unit and integration tests per feature (#65, two audit
  test expectations fixed in #67)
- [x] Release pipeline: Flatpak bundles (x86_64, aarch64), desktop installers for Windows ARM64, portable Windows
  zips (x64, arm64), no macOS server binary (#66); first pre-release v0.1.0 with explicit file modes (#49)
- [x] README *What works* as a concise feature list (#64)
- [x] Light and dark web and desktop design (#51, review fixes #53)
- [x] Read access control: restricted project groups on every read path (#52, review fixes #55)
- [x] Event webhooks: signed JSON with outbox, retries and SSRF guard (#46)
- [x] Folder import: items no longer stay in "My check-outs" – every item is checked in right after its files,
  assemblies are checked out once more for their BOM lines and always checked in again, and a run releases the
  check-outs an earlier, interrupted run of the same folder left behind
- [x] Desktop: project groups load again on *Home*, *Refresh* and in the dialogs; the import log is kept per project
  group, so the same folder can be imported into a second group (#58)
- [x] Bulk action "Check in all my check-outs" (desktop and web) – `POST /api/v1/worklist/checkin-all`
- [x] Desktop: cancel long operations (check-in upload, Pack and Go) half-way – *Cancel* in the check-in dialog stops
  before the next STEP export, between two chunks (upload sessions discarded, not resumable after a cancel) or
  half-way through the streamed multipart request: nothing stored, still checked out; Pack and Go has a progress
  dialog with *Cancel* (also while the server packs) and never leaves a half ZIP (`desktop/src/abortable.rs`)
- [x] SOLIDWORKS 2015+ files: preview image and component tree from the compressed sections (`src/swpart.rs`);
  `teamcraft rescan-references` updates assemblies stored before
- [x] NX `.prt` detection and the reference scan of an assembly verified with the NIST NX 1980 files
- [x] New item with files right away (web form and desktop dialog)
- [x] Previews of Inventor and Solid Edge files: embedded preview image on the server, STEP export through the user's
  own CAD installation in the desktop app – [docs/src/research/cad-previews.md](src/research/cad-previews.md)
- [x] Desktop: import of a CAD folder tree as items with files and BOM lines from the assembly references, with
  tutorial (feature 8)
- [x] Desktop workspace: one folder per project group mirroring the folder structure of the files, read-only files
  until check-out, *Get* of a structure, migration of the old layout; new members get whole project groups at once
- [x] Status per file (up to date, modified, locked, outdated, conflict), check-ins against the base version (409
  instead of overwriting); the desktop asks before opening an outdated, conflicting or foreign-locked file
- [x] STEP assembly import → items + BOM (feature 3c); browser check of `/import` and `/import/{id}` (2026-11-04)
- [x] Desktop: *Import BOM…* (CSV/Excel/ODS, STEP assembly) on a checked-out revision (*Structure* tab, *More* menu)
  and *Tools → Import BOM as New Assembly…*: native file dialog, dry run with counts, notes and the first lines,
  then the import through the server's BOM import (`desktop/src/bomimport.rs`, tested with a fake server)
- [x] 3D preview of real-world STEP files: own Part 21 pre-reader, face-by-face fallback, AP242 tessellated geometry,
  "partial preview: N of M faces"
- [x] Configurable numbering schemes per project group with preview (feature 4)
- [x] Custom attributes per item type (text, number, yes/no, choice list; versioned per revision), classification
  in-house/purchased/standard part, attribute search in the item list, REST API, saved searches and MCP (feature 5)
- [x] BOM import fills the classification (*BOM structure* / procurement column: Purchased, Kaufteil, Content Center …)
  and custom attributes (columns named like an attribute) of new items; values that do not fit are skipped with a
  warning (feature 5, second part)
- [x] Item copy ("Save as" with files and BOM), supersede/replace, alternate parts and multi-level where-used in API, MCP
  and web UI (feature 6) – alternates (two-way or one-way) and supersession per item, *Replace in assemblies* from the
  where-used page with a preview (in-work parents changed, released ones listed for a new revision), migration 0030
- [x] BOM comparison between revisions with Excel export, single level and over all levels (feature 7)
- [x] Chunked uploads from the web UI: `static/app.js` sends files above 32 MiB of the attach and check-in forms
  (revision page, vault explorer) in chunks to `/uploads` (the same sessions, signed in with the session cookie and
  covered by the same-origin check, `src/web/uploads.rs`), retries a dropped chunk, resumes after a reload when the
  same file is chosen again, and *Cancel upload* removes the sessions (tested in `tests/uploads.rs` and in Chromium,
  2026-11-05)
- [x] Engineering change requests before the change order (web, API, desktop, attachments, triage, *Create change
  order*); MCP tools `create_change_request`, `comment_change_request` and the `triage_requests` prompt
- [x] Simulation management v1 (item type `simulation`, basis links, out-of-date detection, result values, import,
  comparison, result file formats) and the 3D preview of VTK and CalculiX results (#43)
- [x] Chunked resumable uploads in the API and the desktop app (feature 9, part)
- [x] Backup/restore CLI (`teamcraft backup`, `teamcraft restore`), tested end to end with the real PostgreSQL client
  tools in `tests/backup_restore.rs` (CI, PostgreSQL 16 and 18)
- [x] README screenshots of the web pages, captured in CI by `scripts/screenshots.sh`
- [x] GitHub Pages for the docs; CI runs (`ci.yml`, `docs.yml`, `desktop.yml`) incl. the macOS and Windows builds
- [x] Openly licensed CAD test files in `tests/fixtures/cad` (NIST), checked by `tests/cad_fixtures.rs`

The history up to the open-source release is squashed into the first commit
`bac82b4`; later work has its own commits.

| #  | Feature                                                                                                                                                               | Commit               |
|----|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------|
| –  | File formats of all common CAD programs                                                                                                                               | `bac82b4`            |
| 1  | Assembly/reference tracking with change propagation                                                                                                                   | `bac82b4`            |
| 2  | Notifications: bell, page, API, e-mail (SMTP), Mattermost, watch lists, per-user opt-out, desktop                                                                     | `bac82b4`            |
| 3a | Pack and Go ZIP (files + `bom.csv` + `manifest.json`), STL export from the 3D viewer                                                                                  | `bac82b4`            |
| –  | Open-source release: pure-Rust 3D preview (`src/mesh.rs`, truck for STEP), 130+ formats, vendor-neutral, no GitLab, self-service project groups                       | `bac82b4`            |
| –  | Vault explorer (`/vault`), immediate cancel buttons, upload progress with cancel, no start-up hangs                                                                   | `bac82b4`            |
| –  | First start without configuration (create the administrator in the browser)                                                                                           | `bac82b4`            |
| –  | mdBook docs with Doxygen-style theme + rustdoc on GitHub Pages, `platforms/` guides, GitHub CI, new README                                                            | `bac82b4`            |
| 3b | CSV/Excel BOM import: reader (`src/bomtable.rs`), service (`src/services/imports.rs`), web UI (upload → preview → import), REST API, unit and integration tests, docs | `bac82b4`            |
| –  | Linux packages: AUR (server, desktop), snap (server), Flatpak (desktop)                                                                                               | `3632c0e`            |
| –  | Docs in the Doxygen layout with a high-contrast colour scheme                                                                                                         | `e4d29d4`            |
| –  | New item together with its files (web form and desktop dialog)                                                                                                        | `56c6da2`            |
| –  | Preview images of native Inventor, Solid Edge and SolidWorks files                                                                                                    | `457e03d`            |
| –  | Desktop: one workspace folder per project group; import of existing CAD folders                                                                                       | `8bf8e65`, `9557ffa` |
| –  | Skins without distribution logos                                                                                                                                      | `099737e`            |
| –  | Teamcenter working features: favorites, saved searches, Save As, BOM comparison                                                                                       | `9289aa4`            |
| –  | Release readiness of change orders: unreleased BOM children and references block the submit, *Add all* (web, API, desktop)                                            | `5c5b3fd`            |
| –  | Work-in-progress / no-warranty notices (README, docs, web footer, desktop), `CONTRIBUTING.md`, `SECURITY.md`                                                          | `5c8eff3`            |
| –  | Similar part search by 3D shape signature                                                                                                                             | `03f94dd`            |
| –  | Simulation results in the 3D viewer (VTK, VTU/VTP, CalculiX FRD)                                                                                                      | `0c07a24`            |
| –  | Read access control: restricted project groups                                                                                                                        | `97423bd`, `11127fd` |
| 9  | Backup/restore end to end with the real PostgreSQL client tools                                                                                                       | `995c597`            |
| –  | Desktop: project groups load again; import log per project group                                                                                                      | `8a4b4ca`            |
| 6  | Supersede, replace and alternate parts; multi-level where-used in the web UI                                                                                          | `1274148`            |
| –  | Desktop: cancel the check-in upload and Pack and Go half-way                                                                                                          | `9ba6e52`            |
| 9  | Web UI: chunked, resumable uploads of large files                                                                                                                     | `671f277`            |
| 3b | Desktop: *Import BOM…* from CSV/Excel or a STEP assembly                                                                                                              | `c7a2592`            |
| –  | README feature list; release: Flatpak, Windows ARM64, portable Windows, no macOS server                                                                              | `bb649bf`, `a873920` |
| –  | Tests: functional and semantic coverage across the feature list                                                                                                       | `f8f5322`            |

## Details on the open features

### 3b – CSV/Excel BOM import, known limitations

Documented in the UI, keep in mind:

- the column recognition is a heuristic built from the documented export layouts; it was **not tested with real
  files** of Inventor, SolidWorks, Solid Edge, Fusion, Creo or NX;
- CATIA V5 *Bill of Material* text exports (one section per sub-assembly plus a *Recapitulation* block) are not
  understood – use a flat or indented export;
- Inventor *Phantom* components are imported as normal sub-assemblies (*Reference* components are skipped);
- the notes (warnings) of an import are shown in the preview only; the audit entry `import.bom` keeps their number,
  not their text.

### 3c – STEP assembly import (done, not tested with real exports)

- `src/stepbom.rs`: own minimal ISO 10303-21 (Part 21) reader for `PRODUCT`,
  `PRODUCT_DEFINITION_FORMATION(_WITH_SPECIFIED_SOURCE)`, `PRODUCT_DEFINITION` and `NEXT_ASSEMBLY_USAGE_OCCURRENCE`
  (quantity = number of occurrences below the same parent), turned into a `BomTable`; the plan/run code of
  `services::imports` is reused unchanged. Web form (`/import`) and REST API (`/bom-imports`, `…/bom/import`) take
  `.step`, `.stp` and `.p21` files up to 200 MiB.
- A new top-level assembly keeps the STEP file (3D preview of the whole assembly). Web uploads are staged in the vault
  (`staged_imports.kind = 'step'`, `sha256`); a discarded upload leaves its blob in the vault.
- Research result: `ruststep` 0.4 (Apache-2.0) rejected as a direct dependency – outdated dependencies (derive_more
  0.99, itertools 0.10, thiserror 1, unmaintained Inflector) and a heavy proc-macro schema layer. `truck-stepio`
  (3D preview) gives the shells but no product structure.
- **Known limitations:** tested with hand-written AP214 structures only, not with real exports of the CAD programs
  (the NIST STEP files in `tests/fixtures/cad` are single parts); complex instances and the quantities of
  `QUANTIFIED_ASSEMBLY_COMPONENT_USAGE` are not read; the geometry of the parts is not split into the new items; a
  STEP file imported *into* a checked-out revision is not attached to it.

### Features 4 to 9 – what is done

4. **Numbering schemes:** one scheme per project group (`projects.number_scheme`, `domain::NumberScheme`) with
   `{PROJECT}`, `{TYPE}`, `{YYYY}`, `{YY}`, `{MM}` and `{SEQ:n}`, preview of the next numbers on the group page and
   via `GET /api/v1/projects/{code}/numbering`; counters are named after the expansion without the sequence.
5. **Custom attributes:** done for items – definitions under *Admin → Attributes* (`attribute_defs`: key, label,
   text/number/yes-no/choice list, unit, item types, order, active), values per revision in `revisions.attributes`
   (JSONB; versioned, copied by revise and *Save as*, part of the full-text search), the classification
   in-house/purchased/standard part as item master data (`items.classification`), filters `class`, `attr` + `value`
   (text contains, yes/no, `12`, `>=10`, `10..20`) in the item list, the REST API, saved searches and the MCP search.
   The BOM import fills both for new items: the *BOM structure* / procurement column (Normal, Purchased,
   Phantom, Kaufteil, Normteil, Content Center …) sets the classification, other columns whose header is the key
   or label of an active attribute fill it (`bomtable` keeps unread columns in `extra_columns`); values that do
   not fit are left out with a warning, existing items stay unchanged, the preview lists the attribute columns and
   the columns that are not imported. Open: attributes per file type.
6. **Item copy** ("Save as" with files and BOM), supersede/replace, alternate parts, multi-level where-used: done.
   Alternates (`item_alternates`, two-way by default) and supersession (`item_supersessions`, one successor, no
   circles) are item-level relations (`services::interchange`); *Replace in assemblies* on `/items/{id}/where-used`
   (and `GET/POST /api/v1/items/{number}/replace`, MCP `replace_preview`) changes the parents whose latest revision
   is in work and not checked out by others, and lists the rest with the reason. Research:
   [supersede-alternates](src/research/supersede-alternates.md). Open: local substitutes per BOM line, a warning
   when a superseded item is added to a BOM in the desktop app.
7. **BOM comparison** with Excel export (`rust_xlsxwriter`, MIT/Apache-2.0): direct lines, and over all levels with a
   revision rule (`depth=all`); the expanded BOM itself exports as Excel too.
8. **Bulk import of a folder tree:** done in the desktop app (*Import CAD Folder…*).
9. **Chunked resumable uploads:** a tus-like protocol of our own without new dependencies (`POST /api/v1/uploads`,
   `PATCH` with `Upload-Offset`, 409 on a wrong offset, `HEAD` to resume, `finish` or the `uploads` field of a
   check-in), chunks staged in `<vault>/uploads/`, expired sessions removed hourly; used by the desktop app and the attach
   and check-in forms of the web UI (`/uploads` with the session cookie) for files above 32 MiB; only the *New
   item* form still sends one multipart request. **Backup/restore** with `pg_dump`/`pg_restore`: `tests/backup_restore.rs` (backup, restore into a
   fresh database and vault, identical row counts, file download and API token on the restored instance, `--force`,
   atomic failure) runs in CI, and the CLI smoke test runs `teamcraft backup`/`restore`. Found and fixed on the way: a
   percent-encoded password in `TEAMCRAFT_DATABASE_URL` reached `pg_dump` undecoded; a failed `restore --force` left a
   half-replaced database – `pg_restore` now runs with `--single-transaction`.

Rules for all of them: research open source first (licences compatible with MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib,
MPL-2.0; LGPL only as a separately loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects in the
README licence table, follow the definition of done in `CLAUDE.md`. The docs are live at
<https://rudalabs.github.io/TeamCraft/>.
