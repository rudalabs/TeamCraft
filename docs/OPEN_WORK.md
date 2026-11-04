# Open work

The to-do list of TeamCraft first – tick an entry (`[x]`) when it is done –,
then the status of the Prio-1 work plan (worked through in this order) and
the known gaps with details. Last update: 2026-11-04.

## To-do

**Fixes**

- [x] Folder import: find out why all imported items stayed in "My check-outs" after the last import and fix it – the
  import checked the new items in only at the very end, after every item, upload and the BOM step, in one promise
  chain: an interrupted import (dialog closed by reloading, app closed, sign-in expired, network gone) or an exception
  in the BOM step skipped the whole check-in phase, and a second run skipped the logged items, so they were never
  checked in. Now every item is checked in right after its files, assemblies are checked out once more for their BOM
  lines and always checked in again, and a run releases the check-outs an earlier run of the same folder left
  behind (request order tested in `tests/concurrency.rs`; still to do: test import against a real server)
- [x] Bulk action "Check in all my check-outs" (desktop and web) – dashboard button and desktop worklist; the desktop
  checks in only revisions without local changes; `POST /api/v1/worklist/checkin-all`
- [ ] SOLIDWORKS 2015+ files: the preview image and the referenced file names sit in compressed segments of the
  container – inflate them before the preview search and the reference scan (gap test in `tests/cad_fixtures.rs`,
  found with the NIST files)
- [ ] Reference scan: false references in real files – earlier file names and binary noise in NX parts, noise in JT
  files (gap test in `tests/cad_fixtures.rs`); the STEP header export name (`FILE_NAME`) is ignored

**General**

- [x] New item with files right away: file field in the web form and in the desktop dialog (an item is normally never
  without a file)
- [x] Previews of Inventor (`.ipt`, `.iam`, `.idw`, `.ipn`) and Solid Edge (`.par`, `.psm`, `.asm`, `.dft`, `.pwd`)
  files: embedded preview image on the server, STEP export through the user's own CAD installation in the desktop app –
  research and decisions in [docs/src/research/cad-previews.md](src/research/cad-previews.md)
- [x] Desktop: tutorial for importing existing CAD file trees (hundreds of files, project files such as Inventor `.ipj`)
- [x] Desktop: import of a CAD folder tree as items with files, BOM from the assembly references (needed for the
  tutorial; feature 8)
- [x] Desktop workspace: one subfolder per project group (alternatively git-style: only the chosen project groups are
  visible locally)
- [x] Desktop: new members download everything of one or more project groups at once
- [ ] Desktop: *Import BOM…* (CSV/Excel) on a checked-out assembly
- [ ] Desktop: cancel long operations (check-in upload, Pack and Go) half-way
- [x] Browser check (no console errors, no overflow at 390 px) of `/import` and `/import/{id}`, with a STEP import
  (2026-11-04)
- [x] STEP assembly import → items + BOM (feature 3c)
- [ ] Try the STEP assembly import with real exports of Inventor, SolidWorks, Solid Edge, Fusion, CATIA and NX
- [ ] 3D preview: test real STEP exports of the CAD programs and assembly placements (`mapped_item`); readers for IGES,
  BREP, Parasolid, JT, … (E57: `e57` crate)
- [x] Configurable numbering schemes `{PROJECT}`, `{TYPE}`, `{YYYY}`, `{SEQ:n}` with preview, per project group (feature 4)
- [ ] Custom attributes per item and file type, classification (standard/purchased parts), attribute search (feature 5)
- [x] Item copy ("Save as" with files and BOM) – done; open: supersede/replace, alternate parts, multi-level where-used
  (feature 6)
- [x] BOM comparison between revisions with Excel export, single level and over all levels (feature 7)
- [x] Backup/restore of the database and vault as CLI commands (`teamcraft backup`, `teamcraft restore`) – see
  [Operations](src/admin/operations.md)
- [ ] Test `teamcraft backup` and `teamcraft restore` once end to end with the real PostgreSQL client tools
- [ ] Chunked resumable uploads (tus) (feature 9)
- [ ] README screenshots/GIFs: web pages captured in CI by `scripts/screenshots.sh` (PNGs still to commit); desktop app and GIFs open
- [x] Enable GitHub Pages for the docs (*Settings → Pages → Source: GitHub Actions*) and check the first docs build
- [x] Check the CI runs (`ci.yml`, `docs.yml`, `desktop.yml`), including the macOS and Windows builds
- [ ] Enable private vulnerability reporting (*Settings → Code security → Private vulnerability reporting*), which
  `SECURITY.md` points to
- [ ] Test e-mail (SMTP) and Mattermost notifications against real servers

**All CAD programs**

- [ ] (research first) CAD add-ins through the official APIs (Inventor API, Solid Edge API, SolidWorks API, NX Open):
  check out, "save and check in", "get latest" inside the CAD program
- [ ] Check-in takes dependent files along automatically (from the reference scan)
- [ ] Create viewables (STEP) in the background without blocking the check-in
- [x] Status per file: up to date, modified, locked by X, outdated, conflict – desktop Workspace tab and
  `…/file-status`; check-ins are checked against the base version of every file (409 instead of overwriting)
- [x] Report conflicts early, already when a file is opened – the desktop asks before opening an outdated,
  conflicting or foreign-locked file (inside the CAD program still needs the add-ins)
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
  2021 files
- [ ] Test the BOM import with real exports of the BOM editor (structured and parts only)
- [ ] Extend the Inventor part of the tutorial: Pack and Go, `.ipj`, Design Assistant for broken references

**Siemens Solid Edge**

- [ ] Folder import and tutorial: Pack and Go, Design Manager, part families
- [ ] Test preview images and the STEP export (built) with real `.par`/`.asm`/`.psm` files
- [ ] Test the reference scan and the BOM import with real files
- [ ] Use file properties such as the Document Number as item number

**SOLIDWORKS**

- [ ] STEP export through the locally installed SOLIDWORKS (API), like Inventor and Solid Edge
- [ ] Test preview images of SOLIDWORKS 2014 and older (OLE files) with real files – 2015 and newer: see *Fixes*
- [ ] Take configurations and Toolbox parts into account in the import and the reference scan
- [ ] Test the BOM import with real BOM exports

**Siemens NX**

- [ ] (research first) How other PLMs connect NX (native vs. Teamcenter NX Manager), licences (student/academic) and
  what is feasible for an open-source project
- [ ] Folder import for the master model concept: the drawing `*_dwg*.prt` belongs into the item of the model
- [x] `.prt` detection and the reference scan of an assembly verified with the NIST NX 1980 files (false references:
  see *Fixes*)
- [ ] Test the BOM import with real NX BOM exports
- [ ] STEP export through the local NX (NX Open / journal), on Windows and Linux
- [ ] NX load options for the workspace only on request, in the workspace wizard; NX part of the tutorial

**Tests with real data**

- [x] Openly licensed test files in the repository: `tests/fixtures/cad` (NIST: Inventor 2021, NX 1980, SOLIDWORKS
  2018, Creo 10, CATIA V5, STEP AP203/AP242; NX and SOLIDWORKS box assembly), checked by `tests/cad_fixtures.rs`
- [ ] Solid Edge test files: none openly licensed found – create a part, a sheet metal part and an assembly with Solid
  Edge (Community Edition) and publish them under CC0
- [ ] End-to-end test against the server with the fixtures: folder import of `tests/fixtures/cad/nist/mtc-assembly/nx`
  and `…/solidworks` → items, files, references, BOM lines, preview images, STEP preview
- [ ] One real project with hundreds of files each from Inventor, Solid Edge, SOLIDWORKS and NX: folder import, preview
  images, STEP export, BOMs – including the heuristics (CAD reference scan, BOM table columns, embedded previews)

## Done

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
| –  | Work-in-progress / no-warranty notices (README, docs, web footer, desktop), `CONTRIBUTING.md`, `SECURITY.md`                                                          | `5c8eff3`            |

## Open – feature 3 (BOM from CAD)

### 3b – CSV/Excel BOM import, remaining parts

- **Desktop app:** *Import BOM…* on a checked-out assembly revision and for a
  new assembly. Plan: Rust command opens the file dialog, sends the file to
  `POST /api/v1/items/{number}/revisions/{code}/bom/import` (or
  `POST /api/v1/bom-imports`) with `dry_run=true`, the UI shows counts,
  notes and the first rows, a second call imports. Desktop tests and the row
  in `desktop/README.md`.
- **Browser check:** done on 2026-11-04 in a Chromium-based browser (not
  automated): `/import` and `/import/{id}` have no console errors and no
  horizontal overflow at 390 px; wide tables scroll inside `.table-wrap`.
- **Known limitations** (documented in the UI, keep in mind):
    - the column recognition is a heuristic built from the documented export
      layouts; it was **not tested with real files** of Inventor, SolidWorks,
      Solid Edge, Fusion, Creo or NX;
    - CATIA V5 *Bill of Material* text exports (one section per sub-assembly
      plus a *Recapitulation* block) are not understood – use a flat or
      indented export;
    - Inventor *Phantom* components are imported as normal sub-assemblies (*Reference* components are skipped);
    - the notes (warnings) of an import are shown in the preview only; the
      audit entry `import.bom` keeps their number, not their text.

### 3c – STEP assembly import (done, not tested with real exports)

- `src/stepbom.rs`: own minimal ISO 10303-21 (Part 21) reader for `PRODUCT`,
  `PRODUCT_DEFINITION_FORMATION(_WITH_SPECIFIED_SOURCE)`,
  `PRODUCT_DEFINITION` and `NEXT_ASSEMBLY_USAGE_OCCURRENCE` (quantity = number
  of occurrences below the same parent), turned into a `BomTable`; the
  plan/run code of `services::imports` is reused unchanged. Web form
  (`/import`) and REST API (`/bom-imports`, `…/bom/import`) take `.step`,
  `.stp` and `.p21` files up to 200 MiB.
- A new top-level assembly keeps the STEP file (3D preview of the whole
  assembly). Web uploads are staged in the vault (`staged_imports.kind =
  'step'`, `sha256`); a discarded upload leaves its blob in the vault.
- Research result: `ruststep` 0.4 (Apache-2.0) rejected as a direct
  dependency – outdated dependencies (derive_more 0.99, itertools 0.10,
  thiserror 1, unmaintained Inflector) and a heavy proc-macro schema layer.
  `truck-stepio` (3D preview) gives the shells but no product structure.
- **Known limitations:** tested with hand-written AP214 structures only, not
  with real exports of the CAD programs (the NIST STEP files in
  `tests/fixtures/cad` are single parts); complex instances and the
  quantities of `QUANTIFIED_ASSEMBLY_COMPONENT_USAGE` are not read; the
  geometry of the parts is not split into the new items; a STEP file
  imported *into* a checked-out revision is not attached to it.

## Open – features 4 to 9

4. **Configurable numbering schemes**: done – one scheme per project group
   (`projects.number_scheme`, `domain::NumberScheme`) with `{PROJECT}`,
   `{TYPE}`, `{YYYY}`, `{YY}`, `{MM}` and `{SEQ:n}`, preview of the next
   numbers on the group page and via `GET /api/v1/projects/{code}/numbering`.
   Counters are named after the expansion without the sequence. Open: a
   separate scheme per item type within a group, an instance-wide default
   other than `{TYPE}-{SEQ:6}`.
5. **Custom attributes** per item type and file type, classification (standard/purchased parts), attribute search. The
   BOM import can then map
   further columns (e.g. Inventor *BOM Structure = Purchased*, vendor) to
   attributes.
6. **Item copy** ("Save as" with files and BOM), supersede/replace, alternate
   parts, multi-level where-used.
7. **BOM comparison** between revisions with Excel export (`rust_xlsxwriter`,
   MIT/Apache-2.0): done – direct lines, and over all levels with a revision
   rule (`depth=all`); the expanded BOM itself exports as Excel too.
8. **Bulk import of a folder tree** (NAS/Drive) as items with files (candidate: `walkdir`, MIT/Unlicense).
9. **Chunked resumable uploads** (tus protocol, `tus-js-client`); **backup/restore** of the database and vault is done
   (`teamcraft backup`/`teamcraft restore` with `pg_dump`/`pg_restore`, unit tests for the manifest, argument building,
   hashes and path safety); the end-to-end check with the real client tools is still open (see the to-do list).

Rules for all of them: research open source first (licences compatible with
MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0; LGPL only as a separately
loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects
in the README licence table, follow the definition of done in `CLAUDE.md`.

## Known gaps outside the plan

- **README screenshots:** `scripts/screenshots.sh` and the *Screenshots*
  workflow capture dashboard, vault explorer, revision page, 3D viewer (STEP),
  BOM, change order, notifications and a phone view in CI; the PNGs go into
  `media/screenshots/` (see CONTRIBUTING.md). Still missing: the desktop app
  and GIFs, and the screenshots in the book.
- **CI:** `ci.yml`, `docs.yml` and `desktop.yml` pass (`099737e`), the docs
  are live at <https://rudalabs.github.io/TeamCraft/>. The tests run on Linux
  only; macOS and Windows are build checks – server and desktop app were not
  run there yet.
- **3D preview:** STEP is read with truck (planes, B-splines, cylinders, …);
  real exports of the CAD programs were not tested, and assembly placements (`mapped_item`) may be ignored – parts of an
  assembly can then overlap.
  IGES, BREP, Parasolid, JT, FBX, DAE, USD and E57 are recognised but not
  previewed (no pure-Rust readers yet; candidates: `e57` crate for E57).
- **Desktop app:** long operations (check-in upload, Pack and Go) cannot be
  cancelled half-way yet; only the browser sign-in can.
- CAD file reference scanning (`src/cadrefs.rs`) is a heuristic and was not
  tested with real files of the CAD programs.
- E-mail (SMTP) and Mattermost delivery were only tested against fakes, not
  against real servers.
