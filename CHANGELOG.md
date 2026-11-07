# Changelog

All notable changes to TeamCraft are recorded in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the version numbers follow [Semantic Versioning](https://semver.org/).
While the version is `0.x`, any release may contain breaking changes. The
policy for supported versions is in the
[support policy](https://rudalabs.github.io/TeamCraft/admin/support-policy.html).

TeamCraft has no stable release yet. The project status is work in progress:
see the notices in the README.

## [Unreleased]

Changes on `main` since the `v0.2.5` tag.

### Added

- HTML reports of FEA/CFD tools (`.html`, `.htm`) are read as text for the
  search, `read_file_text` and `propose_result_values` (#141): table rows as
  lines with ` | ` between the cells, numbers and units as written, entities
  decoded; scripts, styles, comments, embedded images and hidden elements are
  skipped and nothing is fetched. An own bounded reader (`src/htmltext.rs`)
  runs in the isolated `teamcraft text-extract` child (`--format html`) with
  the limits of the PDF reader; new fuzz target `htmltext`.
- Files of ANSYS Fluent, STAR-CCM+ and Simcenter 3D are recognised more
  precisely: NX `.sim`/`.fem`/`.afm` by their `SPLMSSTR` container, text
  `.fem` as OptiStruct deck, `.dat` Nastran decks written by Simcenter 3D,
  `.f04`/`.pch`, Fluent meshes (`.msh`, `.msh.h5`), gzip-compressed and HDF5
  case/data/mesh files (checked by signature), projects (`.flprj`), `.cdat`,
  journals (`.jou`), transcripts (`.trn`) and report files
  (`report-file-*.out`), STAR-CCM+ solution histories (`.simh`) and macros
  (`.java` naming STAR-CCM+), Gmsh meshes. Transcripts, journals, macros and
  Nastran text output are indexed as text; a large plain-text file keeps its
  last 64 KiB in the search text and `read_file_text`.
- Extraction templates: a new method `table` for whitespace-separated tables
  with quoted column names (Fluent report files; migration 0040), and
  `pick = last` reads the last 2 MB of a file larger than 2 MB first. New
  fuzz targets `cadformat` and `resultscan`.

### Changed

- Extraction templates for `.html` files run on the extracted text instead of
  the raw markup.
- Text and CSV files keep their lines and decimal numbers in the stored text,
  so `read_file_text` shows the real text (only ASCII STL, OBJ and KiCad files
  still drop decimal numbers for the search). Files uploaded before keep their
  old text until a new version is uploaded.

## [0.2.5] - 2026-11-07

Changes since the `v0.2.0` tag.

### Added

- The desktop app tells the user when its version does not fit the server
  (version handshake, see the support policy).
- The desktop settings dialog has the notification settings of the web account
  page (what to be told about, e-mail, Mattermost, send a test);
  `GET /api/v1/notifications/settings` also returns the form description.
- The desktop settings have *Change password…* (`POST /api/v1/me/password`;
  hidden for single-sign-on accounts, `GET /api/v1/me` has `auth_source`).
  Wrong current passwords of the own password change, in the web form and over
  REST, count like failed sign-ins (429 with `Retry-After`) and are audited as
  `user.password_failed`.
- Administrators can replace the wide logo (top left) and the square logo
  (bottom left, tab icon) under *Admin → Branding*; the desktop app shows them
  too. See [Branding](https://rudalabs.github.io/TeamCraft/admin/branding.html).
- `GET /metrics` in the Prometheus text format (request counts and latency,
  database pool, outbox backlogs, 3D preview queue), enabled by
  `TEAMCRAFT_METRICS_TOKEN`.
- As-built records: an item type `as_built` for one physical unit (or lot) with
  the design revision it was built to, serial number, as-built BOM and
  deviations from the design with a disposition; REST, MCP and web UI. Units
  are not recorded against obsolete design revisions; build records are not
  part of the Pack and Go ZIP.
- Sign-in, failed sign-in and sign-out are recorded in the audit log, with how
  the user signed in (password, SSO or first-start setup).
- A sign-in throttle slows down repeated failed sign-ins.
- 3D preview of NX parts (`.prt`) from the JT tessellation NX stores in the
  file, refined along its exact surface normals so round faces stay round.
  The thumbnail of an NX part is rendered from it instead of the small JPEG
  stored in the file.
- 3D preview and thumbnail of NX assemblies, put together from the previews
  of their part files (found by file name, sub-assemblies included);
  components on layers the assembly hides are left out, as in NX.
- Desktop app: an update notice. At most once a day the app asks GitHub for
  the latest release and shows a link to it in the status bar; it never
  downloads or installs anything. *Settings → Updates* switches it off.

### Changed

- Accessibility: an automated check runs on every rendered page; keyboard
  flows were completed and the Escape key closes panels.
- The MCP tools are split by area (`src/mcp/tools.rs`); no behaviour change.
- The desktop UI is split into ES modules per view (`desktop/ui/js/`, no build
  step) instead of one 5,000-line `app.js`; CI checks that every module's
  imports are complete (`scripts/check-desktop-modules.mjs`). No behaviour change.
- The service functions for requests are split into smaller modules; no
  behaviour change.
- The invariants of the remaining mutating service functions are documented.

### Fixed

- Desktop app: the assistant answer hung when a whole line looked like an item
  number to the folder import (two variables of the same name), and item
  numbers in answers were never links. Simulation numbers (`SIM-…`) are links
  now as well, in the web UI too.

- The reference scan records the names of STEP exports as a second name, and
  no longer reports noise for JT files.
- The duplicate-files report counts the files of a hidden revision correctly.
- Two administrators can no longer remove each other at the same time.
- The command line commands write their log output to standard error.

### Security

- Web requests that change data are refused when they carry the session cookie
  but neither an `Origin` nor a `Referer` header (cross-site request forgery).
- `Strict-Transport-Security` is sent when the public URL uses `https://`.
- Changing one's own password ends the other sessions of the user.
- The file readers for untrusted uploads (CAD, mesh, point-cloud and BOM
  table files) no longer panic on malformed input.
- The child processes that convert files start with a cleared environment.
- Desktop app: the sign-in token is kept in the OS keychain (Windows
  Credential Manager, macOS Keychain, Secret Service on Linux); the token file
  of earlier versions moves there at the next start. Without a keychain the
  app keeps the private file and shows a warning. Server addresses with plain
  `http://` to another computer get a warning.

[Unreleased]: https://github.com/rudalabs/TeamCraft/compare/v0.2.5...HEAD
[0.2.5]: https://github.com/rudalabs/TeamCraft/compare/v0.2.0...v0.2.5
