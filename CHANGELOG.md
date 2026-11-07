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

- Need-to-know project groups, step 5 (#151): access review of a restricted
  group (migration 0051, `project_access_reviews`). Page `/projects/{id}/access-review` for managers and
  administrators: members with role, group role, source (manual or identity
  provider), account state and last sign-in (stale after 90 days), the
  administrator policy and elevations, and a "reviewed" confirmation with an
  optional note that is dated and audited (`project.access_review`) and keeps
  the member list as it was. Evidence for auditors as CSV (page and
  `GET /api/v1/projects/{code}/access-review?format=csv`) or JSON, plus
  `POST` to confirm and `GET .../access-review/{id}` for the stored member list.
  The shorter session age for restricted groups (step 2) is not part of this.
- Item owner and hand-over (#152 step 7, migration 0050): `items.owner_id`
  (default: the creator) is shown on the item page and changed by the
  managers of the group and administrators (`item.owner`). Administrators
  hand over a leaving user's item ownerships, draft and rejected change
  orders, open change requests and pending reviews to another user in one
  transaction on *Users* → user → *Hand over work*; a message after the
  redirect gives the counts and the first objects that stayed (restricted
  group, already a reviewer, four-eyes); numbers the acting administrator
  may not read (need-to-know) are shown as "restricted item"; every object
  has its own audit entry.
- Reviewer suggestions show people who changed the affected revisions as
  *knows the item, cannot review* (`cannot_review` in the REST and MCP
  answers, desktop app, change page) instead of hiding them.
- Result values in the item search and on the dashboard: filters
  `result` (`failed`, `passed`, `above_limit`, `below_limit`) and
  `result_name` on the item list, `GET /api/v1/items` and the web filter bar,
  and a dashboard card *Simulations to check* (failed values or out-of-date
  basis). Restricted groups stay out of results and totals. No migration.
- Solver summary per file version, step 3 of `docs/src/research/solver-files.md`
  (migration 0049, nullable `file_versions.solver_summary`): for Fluent transcripts and report files, Nastran `.f06`
  and decks, STAR-CCM+ macros and batch logs, one line with solver, version, how
  the run ended, last iteration and FATAL/WARNING counts of the first and last
  64 KiB. Shown on the file row and the simulation page, in the REST file list and
  the MCP `get_revision` files (marked untrusted). The rules are unverified
  against real files.
- Review rounds and absent reviewers (#152, steps 5 and 6): withdrawing or
  reopening a change after decisions keeps them as an earlier round
  (`change_review_rounds`, `changes.review_round`, migration 0048). The change page and
  `GET /api/v1/changes/{number}` show earlier rounds and flag pending
  reviewers who cannot decide (deactivated, demoted, blind to an item). The
  author or an administrator replaces a pending reviewer while the change is in
  review (`POST /api/v1/changes/{number}/reviewers/{old}/replace`, audited
  `change.replace_reviewer`); other decisions are kept.
- Review policy per project group, step 2 of #152 (migration 0047): minimum approvals (default all), required reviewers and
  an optional role group (e.g. QA) whose member must approve. `submit` refuses a
  change that cannot meet the policy of every affected group; the review outcome
  uses it. Form on the group page, `GET`/`PUT /api/v1/projects/{code}/review-policy`.
  Nothing changes for groups without a policy.
  Review fixes: admins maintain a group hidden by need-to-know, a role group the
  reader cannot see is not named, only the parts of the policy the assigned
  reviewers can meet count after submit, a required person who is the author
  gets a specific message, ungrouped items count as "all reviewers".
- Scoped personal access tokens (threat model F2, #151 step 6, #152 step 4):
  a token gets a scope when it is created (account page, `POST /api/v1/tokens`,
  `teamcraft user create-token --scope`). `full` = read, write and approve
  (what every token had before; existing tokens and the desktop app's token
  stay `full`), `write` = read and write but no approving (reviewing a change
  order, deciding a change request, making a revision obsolete), `read_only` =
  GET/HEAD and the read-only MCP tools, `mcp_only` = only `POST /api/v1/mcp`.
  Refusals are 403 and write nothing; a token cannot mint a token with more
  rights than its own. The desktop app's new tokens now expire after 30 days
  without use (renewed on every use) instead of a fixed 90 days; existing
  desktop tokens keep their fixed expiry.
  Migration `0046`.
- The web *New item* form sends files above 32 MiB in chunks too (resumable, like the revision page); the
  upload sessions may now have no revision and only join the creation of an item. Without JavaScript the form still
  submits all files in one request.
- Need-to-know project groups, step 1 (#151): a restricted group can limit
  administrator access (`project_policies`, migration 0043): `with_reason`
  (time-limited, audited elevation with a written reason; managers are
  notified) or `none` (administrators manage but do not read). Web form on
  the group page and `GET`/`PUT /api/v1/projects/{code}/policy`,
  `POST`/`DELETE /api/v1/projects/{code}/elevations`. Nothing changes for
  groups without a policy. Migration 0044 keeps the elevation history when an
  account is deleted. While a policy is in force an administrator who is no
  manager member cannot loosen it, open the group, link an IdP group or add
  themselves; `/admin/audit` and webhooks honour the rule.

- Wider separation of duties (#152, step 1): a reviewer must not have created
  the change, submitted it (an administrator acting for the author) or changed
  the revisions under review (revision creator, file uploader, logged edits).
  Checked when adding a reviewer, on submit and on every decision. The
  reviewer suggestions never offer such users. An administrator who prepared
  the draft for the author (items, reviewers, edits, submit; read from the
  audit log) cannot review it either. Migrations `0041`/`0042` add
  `changes.submitted_by` and `changes.four_eyes`. Upgrade: changes already in
  review keep working (only the author is refused); the editor and preparer
  checks apply to drafts and to every later submit.

- Company AI instead of shadow AI (data-loss prevention):
  `TEAMCRAFT_LOCAL_AGENTS=off` lets administrators turn off local AI agents
  (the desktop app's *Agent* view, ACP), so that AI use goes through the
  server-side assistant *Ask TeamCraft* with the provider and key the
  organisation configured (Anthropic, OpenAI/Azure, or a local
  OpenAI-compatible server), its per-question audit and its limits; the key
  never reaches a client. `GET /api/v1/me` reports
  `capabilities.local_agents`; the desktop app ends a running agent (it
  asks again when the Agent view opens, every minute and before every
  message) and refuses to start one, and its *Agent* view (`Ctrl/Cmd+4`) runs on the
  company AI automatically, with no local option (or, without a configured
  assistant, shows that AI is switched off by the administrator). When the
  server offers both, *Company AI* / *Local agent* in the Agent toolbar
  switches between them, remembered per server and user;
  `POST /api/v1/mcp` refuses every call with `403`, for every token (also
  the `teamcraft mcp` bridge, agents set up by hand and older desktop apps;
  the built-in assistant calls the tools inside the server and is not
  affected), and `capabilities.mcp` turns `false`. The default `allowed`
  changes nothing. The REST API stays open to a user's own token (scoped
  tokens, F2, are the complement): cooperative enforcement, not a barrier.
- **Admin → AI** (`/admin/ai`): the local-agent setting, the MCP endpoint's
  state and the configured assistant (provider, model, host, limits, whether
  a key is set – never the key).
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

- Desktop: *Ask TeamCraft* and *Agent* are one view. The company AI (the
  server's assistant, with page context, quick actions, the drafts checkbox
  and the conversation kept on the server) is a backend of the *Agent* view
  (`Ctrl/Cmd+4`, the *Agent* button of revision and change tabs); the *Ask*
  rail button, the drawer beside the tabs, *View → Ask TeamCraft*
  (`Ctrl/Cmd+5`) and the *Ask…* toolbar buttons are gone. The web UI keeps
  its sidebar.
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
