# Open work

Status of the Prio-1 work plan (worked through in this order) and the known
gaps. Last update: 2026-11-03.

## Done

| # | Feature | Commit |
|---|---------|--------|
| – | File formats of all common CAD programs | `c60bed0` |
| 1 | Assembly/reference tracking with change propagation | `bb4e28b` |
| 2 | Notifications: bell, page, API, e-mail (SMTP), Mattermost, watch lists, per-user opt-out, desktop | `4e428c8` |
| 3a | Pack and Go ZIP (files + `bom.csv` + `manifest.json`), STL export from the 3D viewer | `fdcb626` |
| 3b | CSV/Excel BOM import: reader (`src/bomtable.rs`), service (`src/services/imports.rs`), web UI (upload → preview → import), REST API, unit and integration tests, docs | this commit and `2633664` |

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
  - Inventor *Phantom* components are imported as normal sub-assemblies
    (*Reference* components are skipped);
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
- Research result: `ruststep` 0.4 (Apache-2.0) rejected – outdated
  dependencies (derive_more 0.99, itertools 0.10, thiserror 1, unmaintained
  Inflector) and a heavy proc-macro schema layer; `occt-import-js` is not
  enough (runs in the browser only, gives names but no product structure).

## Open – features 4 to 9 (not started)

4. **Configurable numbering schemes** (`{PROJECT}`, `{TYPE}`, `{YYYY}`,
   `{SEQ:n}`) with preview. Numbers are generated in `items::create_tx`
   (`domain::format_item_number` + counters); the BOM import uses
   `create_tx`, so it picks up the schemes automatically.
5. **Custom attributes** per item type and file type, classification
   (standard/purchased parts), attribute search. The BOM import can then map
   further columns (e.g. Inventor *BOM Structure = Purchased*, vendor) to
   attributes.
6. **Item copy** ("Save as" with files and BOM), supersede/replace, alternate
   parts, multi-level where-used.
7. **BOM comparison** between revisions, Excel export (`rust_xlsxwriter`,
   MIT/Apache-2.0, already a dev-dependency).
8. **Bulk import of a folder tree** (NAS/Drive) as items with files
   (candidate: `walkdir`, MIT/Unlicense).
9. **Chunked resumable uploads** (tus protocol, `tus-js-client`) and
   **backup/restore** of database and vault as CLI commands (`pg_dump` /
   `pg_restore`), with tests and docs.

Rules for all of them: research open source first (licences compatible with
MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0; LGPL only as a separately
loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects
in the README licence table, follow the definition of done in `CLAUDE.md`.

## Known gaps outside the plan

- `DEPLOYMENT.md` was deleted on purpose, but `README.md`, `CLAUDE.md`
  (conventions: "the table in DEPLOYMENT.md §7"), `LOCAL_TESTING.md`,
  `.gitlab-ci.yml` and `src/web/mod.rs` still mention it – the references
  should be removed or pointed to the README (maintainer's decision).
- The macOS build of the desktop app in CI was not checked.
- CAD file reference scanning (`src/cadrefs.rs`) is a heuristic and was not
  tested with real files of the CAD programs.
- E-mail (SMTP) and Mattermost delivery were only tested against fakes, not
  against real servers.
