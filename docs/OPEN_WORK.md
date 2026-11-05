# Open work

The to-do list of TeamCraft: the open entries grouped by area, then what is done, then details on the open features.
Tick an entry (`[x]`) when it is done and move it to *Done*. Last update: 2026-11-05.

## To-do

**Fixes**

- [ ] Reference scan: false references in real files – NX parts fixed (stream container, `src/nxpart.rs`); open: noise
  in JT files (gap test in `tests/cad_fixtures.rs`); the STEP header export name (`FILE_NAME`) is ignored; the scan
  (`src/cadrefs.rs`) is a heuristic that was not tested with real files of the CAD programs yet (see the CAD sections)

**Desktop app**

- [ ] Check #58 in the real app (Tauri build, Windows): project groups load again on *Home*, *Refresh* and in the
  dialogs; the same folder imported into a second project group (so far checked in Chromium with a stubbed Tauri)
- [ ] Decide and document what happens when the same CAD files are imported into two project groups: two independent
  items per file with different numbers, while CAD references are matched by file name (import tutorial, user guide)
- [ ] Check the desktop *Import BOM…* in the real app (native file dialog; so far checked in Chromium with a stubbed
  Tauri against a real server)
- [ ] Cancel long operations (check-in upload, Pack and Go) half-way – today the browser sign-in, Get and the chunked
  upload of large files can be cancelled (in progress: branch `feature/desktop-cancel-operations`)
- [ ] Get: resume a single large file half-way (HTTP range requests; today a cancelled file starts over)
- [ ] Change requests: assign and link a change order from the desktop app (today: in the web UI)
- [ ] Screenshots and short GIFs of the desktop app for the README, and screenshots in the book (the web pages are
  captured in CI by `scripts/screenshots.sh`)

**Web UI and server**

- [ ] Browser check (no console errors, no overflow at 390 px) of `/requests`, `/requests/new` and `/requests/{id}`,
  and the desktop request tab
- [ ] Chunked resumable uploads from the web UI – `static/app.js` still sends one multipart request; the API and the
  desktop app use `/api/v1/uploads` (feature 9; in progress: branch `feature/web-chunked-uploads`)
- [ ] Custom attributes per item type and file type, classification (standard/purchased parts), attribute search;
  then the BOM import can map further columns (e.g. Inventor *BOM Structure = Purchased*, vendor) to attributes
  (feature 5; in progress: branch `feature/custom-attributes`)
- [ ] Supersede/replace, alternate parts, multi-level where-used in the web UI – API and MCP have it (`levels=N`)
  (feature 6; in progress: branch `feature/item-supersede-alternates`)
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
  Creo); CATIA V5 *Bill of Material* text exports are not understood (see *3b*)

**3D preview**

- [ ] Test real STEP exports of the CAD programs, assembly placements (`mapped_item` – parts of an assembly can
  overlap) and length units
- [ ] Readers for IGES, BREP, Parasolid, JT, FBX, DAE, USD and E57 – recognised, not previewed (no pure-Rust readers
  yet; candidate: `e57` crate)

**Simulation management (FEA/CFD)**

- [ ] OpenFOAM case summary (solver, time steps) from `controlDict` – the 3D preview of VTK and CalculiX results is
  done (#43)
- [ ] `.sim` detection: verify the STAR-CCM+ marker with real files (heuristic: "STAR-CCM+" near the start)
- [ ] Read result values from a `teamcraft-results.json`/CSV next to the files at check-in
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
- [ ] Run server and desktop app once on macOS – CI runs the tests on Linux only, macOS and Windows are build checks;
  Windows 11 ARM64 was run by hand (Parallels VM, 2026-11-05, setup in [platforms/windows.md](../platforms/windows.md))

## Done

- [x] Folder import: items no longer stay in "My check-outs" – every item is checked in right after its files,
  assemblies are checked out once more for their BOM lines and always checked in again, and a run releases the
  check-outs an earlier, interrupted run of the same folder left behind
- [x] Desktop: project groups load again on *Home*, *Refresh* and in the dialogs; the import log is kept per project
  group, so the same folder can be imported into a second group (#58)
- [x] Bulk action "Check in all my check-outs" (desktop and web) – `POST /api/v1/worklist/checkin-all`
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
- [x] Item copy ("Save as" with files and BOM), multi-level where-used in the API and MCP (feature 6, part)
- [x] BOM comparison between revisions with Excel export, single level and over all levels (feature 7)
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
5. **Custom attributes:** open (see *To-do*).
6. **Item copy** ("Save as" with files and BOM) and multi-level where-used in the API and MCP (`levels=N`); open:
   supersede/replace, alternate parts, where-used levels in the web UI.
7. **BOM comparison** with Excel export (`rust_xlsxwriter`, MIT/Apache-2.0): direct lines, and over all levels with a
   revision rule (`depth=all`); the expanded BOM itself exports as Excel too.
8. **Bulk import of a folder tree:** done in the desktop app (*Import CAD Folder…*).
9. **Chunked resumable uploads:** a tus-like protocol of our own without new dependencies (`POST /api/v1/uploads`,
   `PATCH` with `Upload-Offset`, 409 on a wrong offset, `HEAD` to resume, `finish` or the `uploads` field of a
   check-in), chunks staged in `<vault>/uploads/`, expired sessions removed hourly; used by the desktop app, not yet
   by the web UI. **Backup/restore** with `pg_dump`/`pg_restore`: `tests/backup_restore.rs` (backup, restore into a
   fresh database and vault, identical row counts, file download and API token on the restored instance, `--force`,
   atomic failure) runs in CI, and the CLI smoke test runs `teamcraft backup`/`restore`. Found and fixed on the way: a
   percent-encoded password in `TEAMCRAFT_DATABASE_URL` reached `pg_dump` undecoded; a failed `restore --force` left a
   half-replaced database – `pg_restore` now runs with `--single-transaction`.

Rules for all of them: research open source first (licences compatible with MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib,
MPL-2.0; LGPL only as a separately loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects in the
README licence table, follow the definition of done in `CLAUDE.md`. The docs are live at
<https://rudalabs.github.io/TeamCraft/>.
