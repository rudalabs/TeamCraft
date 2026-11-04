# Open work

The to-do list of TeamCraft first – tick an entry (`[x]`) when it is done –,
then the status of the Prio-1 work plan (worked through in this order) and
the known gaps with details. Last update: 2026-11-04.

## To-do

- [x] New item with files right away: file field in the web form and in the desktop dialog (an item is normally never
  without a file)
- [x] Previews of Inventor (`.ipt`, `.iam`, `.idw`, `.ipn`) and Solid Edge (`.par`, `.psm`, `.asm`, `.dft`, `.pwd`)
  files: embedded preview image on the server, STEP export through the user's own CAD installation in the desktop app –
  research and decisions in [docs/src/research/cad-previews.md](src/research/cad-previews.md)
- [ ] Try the native previews on Windows with real files: preview images of Inventor, Solid Edge and SolidWorks files,
  *STEP* export with Inventor and with Solid Edge (desktop app)
- [ ] Try the CAD folder import with a real Inventor and a real Solid Edge project of hundreds of files (desktop app)
- [x] Desktop: tutorial for importing existing CAD file trees (hundreds of files, project files such as Inventor `.ipj`)
- [x] Desktop: import of a CAD folder tree as items with files, BOM from the assembly references (needed for the
  tutorial; feature 8)
- [x] Desktop workspace: one subfolder per project group (alternatively git-style: only the chosen project groups are
  visible locally)
- [x] Desktop: new members download everything of one or more project groups at once
- [ ] Desktop: *Import BOM…* (CSV/Excel) on a checked-out assembly
- [ ] Desktop: cancel long operations (check-in upload, Pack and Go) half-way
- [ ] Browser check (Playwright, no console errors, no overflow at 390 px) of `/import` and `/import/{id}`
- [ ] STEP assembly import → items + BOM (feature 3c)
- [ ] 3D preview: test real STEP exports of the CAD programs and assembly placements (`mapped_item`); readers for IGES,
  BREP, Parasolid, JT, … (E57: `e57` crate)
- [ ] Configurable numbering schemes `{PROJECT}`, `{TYPE}`, `{YYYY}`, `{SEQ:n}` with preview (feature 4)
- [ ] Custom attributes per item and file type, classification (standard/purchased parts), attribute search (feature 5)
- [ ] Item copy ("Save as" with files and BOM), supersede/replace, alternate parts, multi-level where-used (feature 6)
- [ ] BOM comparison between revisions, Excel export (feature 7)
- [ ] Chunked resumable uploads (tus); backup/restore of database and vault as CLI commands (feature 9)
- [ ] README screenshots/GIFs: dashboard, vault explorer, revision page, 3D viewer, BOM, desktop app
- [x] Enable GitHub Pages for the docs (*Settings → Pages → Source: GitHub Actions*) and check the first docs build
- [x] Check the CI runs (`ci.yml`, `docs.yml`, `desktop.yml`), including the macOS and Windows builds
- [ ] Test the heuristics with real CAD files: CAD reference scan, BOM table columns, embedded previews
- [ ] Test e-mail (SMTP) and Mattermost notifications against real servers

## Done

The history up to the open-source release is squashed into the first commit
`64b4ba7`; later work has its own commits.

| #  | Feature                                                                                                                                                               | Commit    |
|----|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------|
| –  | File formats of all common CAD programs                                                                                                                               | `64b4ba7` |
| 1  | Assembly/reference tracking with change propagation                                                                                                                   | `64b4ba7` |
| 2  | Notifications: bell, page, API, e-mail (SMTP), Mattermost, watch lists, per-user opt-out, desktop                                                                     | `64b4ba7` |
| 3a | Pack and Go ZIP (files + `bom.csv` + `manifest.json`), STL export from the 3D viewer                                                                                  | `64b4ba7` |
| –  | Open-source release: pure-Rust 3D preview (`src/mesh.rs`, truck for STEP), 130+ formats, no WARR branding/seeds/GitLab, self-service project groups                   | `64b4ba7` |
| –  | Vault explorer (`/vault`), immediate cancel buttons, upload progress with cancel, no start-up hangs                                                                   | `64b4ba7` |
| –  | First start without configuration (create the administrator in the browser)                                                                                           | `64b4ba7` |
| –  | mdBook docs with Doxygen-style theme + rustdoc on GitHub Pages, `platforms/` guides, GitHub CI, new README                                                            | `64b4ba7` |
| 3b | CSV/Excel BOM import: reader (`src/bomtable.rs`), service (`src/services/imports.rs`), web UI (upload → preview → import), REST API, unit and integration tests, docs | `64b4ba7` |
| –  | Linux packages: AUR (server, desktop), snap (server), Flatpak (desktop)                                                                                               | `dba645e` |
| –  | Docs in the Doxygen layout with a high-contrast colour scheme                                                                                                         | `f6ec85a` |
| –  | New item together with its files (web form and desktop dialog)                                                                                                        | `2c10492` |
| –  | Preview images of native Inventor, Solid Edge and SolidWorks files                                                                                                    | `1fd120d` |
| –  | Desktop: one workspace folder per project group; import of existing CAD folders                                                                                       | `2c20b2a`, `88f5e68` |
| –  | Skins without distribution logos                                                                                                                                      | `c7b3236` |

## Open – feature 3 (BOM from CAD)

### 3b – CSV/Excel BOM import, remaining parts

- **Desktop app:** *Import BOM…* on a checked-out assembly revision and for a
  new assembly. Plan: Rust command opens the file dialog, sends the file to
  `POST /api/v1/items/{number}/revisions/{code}/bom/import` (or
  `POST /api/v1/bom-imports`) with `dry_run=true`, the UI shows counts,
  notes and the first rows, a second call imports. Desktop tests and the row
  in `desktop/README.md`.
- **Browser check:** the Playwright/Chromium check of `/import` and
  `/import/{id}` (no console errors, no horizontal overflow at 390 px) has not
  been run yet. The pages only use existing styles (cards, tables, `indent-*`).
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

### 3c – STEP assembly import (not started)

- Own minimal ISO 10303-21 (Part 21) reader for `PRODUCT`,
  `PRODUCT_DEFINITION_FORMATION(_WITH_SPECIFIED_SOURCE)`,
  `PRODUCT_DEFINITION` and `NEXT_ASSEMBLY_USAGE_OCCURRENCE` (quantity = number
  of occurrences below the same parent), turned into a `BomTable` so that the
  plan/run code of `services::imports` is reused unchanged.
- The STEP file is attached to the new root revision. Staging for the web
  flow: `staged_imports` already has the `sha256` column and the kind `step`
  (store the upload in the vault, not in the row).
- Research result: `ruststep` 0.4 (Apache-2.0) rejected as a direct
  dependency – outdated dependencies (derive_more 0.99, itertools 0.10,
  thiserror 1, unmaintained Inflector) and a heavy proc-macro schema layer.
  Note: the 3D preview now uses `truck-stepio`, which pulls in `ruststep`
  anyway; its `Table` gives the shells but no product structure, so the own
  Part 21 reader for `PRODUCT` / `NEXT_ASSEMBLY_USAGE_OCCURRENCE` is still
  the plan.

## Open – features 4 to 9 (not started)

4. **Configurable numbering schemes** (`{PROJECT}`, `{TYPE}`, `{YYYY}`,
   `{SEQ:n}`) with preview. Numbers are generated in `items::create_tx`
   (`domain::format_item_number` + counters); the BOM import uses
   `create_tx`, so it picks up the schemes automatically.
5. **Custom attributes** per item type and file type, classification (standard/purchased parts), attribute search. The
   BOM import can then map
   further columns (e.g. Inventor *BOM Structure = Purchased*, vendor) to
   attributes.
6. **Item copy** ("Save as" with files and BOM), supersede/replace, alternate
   parts, multi-level where-used.
7. **BOM comparison** between revisions, Excel export (`rust_xlsxwriter`,
   MIT/Apache-2.0, already a dev-dependency).
8. **Bulk import of a folder tree** (NAS/Drive) as items with files (candidate: `walkdir`, MIT/Unlicense).
9. **Chunked resumable uploads** (tus protocol, `tus-js-client`) and **backup/restore** of database and vault as CLI
   commands (`pg_dump` /
   `pg_restore`), with tests and docs.

Rules for all of them: research open source first (licences compatible with
MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0; LGPL only as a separately
loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects
in the README licence table, follow the definition of done in `CLAUDE.md`.

## Known gaps outside the plan

- **README screenshots / GIFs** are missing: they need a browser session (Claude in Chrome was not connected). Planned:
  dashboard, vault explorer,
  revision page, 3D viewer (STEP), BOM page, desktop app; store them in
  `docs/src/images/` and use them in the README and the book.
- **CI:** `ci.yml`, `docs.yml` and `desktop.yml` pass (`c7b3236`), the docs
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
