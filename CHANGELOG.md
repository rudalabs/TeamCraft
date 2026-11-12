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

### Added

- 3D preview of Autodesk Fusion designs (`.f3d`) from the display mesh
  Fusion stores in the file, refined along its exact normals like NX parts;
  the thumbnail is rendered from it (the PNG Fusion stored is the fallback).
- Offboarding (#173, step 8): **Season handover** of a project group
  (`/projects/{id}/season`, managers and administrators): tick the members
  who leave – or let the page tick those inactive since a date or past their
  new *member until* date – and run one plan: make the new lead a manager,
  release check-outs with a reason, set *member until*, remove members and
  demote the old managers. Every step is checked and audited on its own.
  Migration 0070 (`project_members.expires_at`, a flag only).
- Offboarding (#173, step 7): **groups without an active manager** – or
  whose managers have neither signed in nor used a token for 90 days – are
  listed for administrators on *Project groups* and at
  `GET /api/v1/projects/manager-gaps`; the hourly housekeeping notifies the
  administrators once per gap (audited as `project.manager_gap_alert`).
- Offboarding (#173, step 6): a group manager can release the check-out of
  an **active** member with a reason (*Release with reason* on the revision
  page, `{"reason": …}` on `cancel-checkout`). The reason is audited and told
  to the holder; without one the release is still refused.
- Offboarding (#173, step 4): **Offboard and deactivate** on the admin user
  page, `POST /api/v1/users/{id}/offboard` and `teamcraft user deactivate`
  deactivate a leaver in one step – hand over the work and the manager roles
  to a successor, delete the API tokens (so a reactivation does not bring
  them back), release the check-outs. Deactivating the only active manager of
  a group without a successor needs a confirmation; every step is audited.
  Audit entries of the command line are recorded as `system` without a user.
- Desktop app, *Home* (#255): a **Files** view, the one Home starts with –
  the folders of the active project group and the files in them, as in its
  working folder: folders first, then the files by name as a file manager
  orders them (`Part2` before `Part10`), the file name first in every row.
  One folder is loaded at a time (`GET /api/v1/projects/{code}/files`: the
  sub-folders with their file counts and the files of the latest revision of
  every item, by name, 500 per request), so a group with more than 500 items
  is listed completely – *All items* shows the first 500 by number. Items
  without files follow the files at the top of the group. *New* and *Import
  CAD Folder…* start with the active group. With an older server Home shows
  *All items*.
- Desktop app (#255): a **Projects** page (rail, `Ctrl/Cmd+5`) to choose the
  active project group, create one, get groups into the workspace and import
  a CAD folder. **Home** becomes a file browser of the active group: names
  first with the number dimmed after them (long number prefixes no longer cut
  the file names off), sorted by name by default, or by number, type or
  state.
- Offboarding (#173, step 1): **what a user still holds**, in one place –
  check-outs with their age, open change orders they wrote, reviews waiting
  for them, change requests assigned to and raised by them, owned items,
  memberships (flagging groups where they are the only active manager) and
  API tokens. *Users → user → Everything … still holds*
  (`/users/{id}/holdings`), `GET /api/v1/users/{id}/holdings` and
  `teamcraft user holdings --username`. Filtered by the reader's rights, not
  the leaver's; group managers see the same report for the groups they
  manage (*holds…* in the member list).
- BOM quantities from Inventor assemblies (#247): the reference scan reads
  how often an `.iam` places each referenced file from the file descriptor
  table of its `UFRxDoc` stream (`src/invasm.rs`, checked against Inventor's
  own API on 32 real assemblies: 190 of 190 files), so the folder import,
  *Quantities from CAD…* and the BOM-from-references page get real
  quantities instead of 1. Suppressed occurrences are left out, as in
  Inventor's BOM (from the occurrence records in the same stream); a part
  with only suppressed occurrences counts 0 – it stays referenced but gets
  no BOM line. When the records do not add up, the counts include the
  suppressed ones instead of guessing.
  *Quantities from CAD…* reads the counts from the stored file, so it works
  for assemblies imported before; `teamcraft rescan-references --format
  inventor-assembly` also refreshes the stored counts of those files.

- Two optional rules of a project group's review policy (#152 step 9, both
  off by default): *Requesters do not decide their own change requests*, and
  *A reviewer confirms each "use as is" deviation* – a reviewer of the change
  order confirms every concession of an as-built revision on its own
  (`POST /api/v1/as-built-deviations/{id}/confirm`) before approving.
  Migration 0062.
- The Pack and Go manifest names, for every released revision, the change
  order that released it and its approvers (`released_by`), and
  administrators export the audit log as CSV or JSON (*Admin → Audit log*,
  `GET /api/v1/audit`); each export is recorded (#152 step 8).
- Sign-off of review decisions (#152 step 4): `TEAMCRAFT_APPROVAL_REAUTH`
  (`off` by default, `release`, `every_decision`) asks for the password – or
  a fresh single sign-on (`prompt=login`, *Sign in again*) – to decide in the
  browser. Every decision records how it was signed off and an optional
  meaning ("approved as design owner"). Migration 0063.
- The read log of need-to-know groups (#151, step 4) also records the read
  tools of MCP clients and the built-in assistant that name an item (channel
  `mcp` / `assistant`, the tool as detail) and the desktop app's Get (channel
  `desktop`, kind `workset`); the assistant's own taint comparisons are not
  logged. *The whole log, with filters* (`/projects/{id}/access-log`)
  filters by user, kind, channel, object and dates and pages through the log;
  `GET /api/v1/projects/{code}/access-log` takes the same filters and pages
  with `next` / `before`. Migration 0066.
- Periodic access review of need-to-know groups (#151, step 5): *Review the
  access every (days)* flags a group "access review due" (group list, group
  page, review page, evidence report) when no review was confirmed within the
  interval, and the hourly housekeeping reminds the managers once it is due
  and again after each further interval (audited). Migration 0067.
- Conditions on membership of need-to-know groups (#151, step 2): required
  identity-provider claims (from the last single sign-on; only the claims a
  condition names are kept), allowed networks (client address ranges) and a
  maximum sign-in age. A reader who does not meet them – administrators
  included – sees the group as "not found"; a refused member is audited once
  per session; a manager cannot set conditions that would hide the group from
  themselves; administrators limited by the policy may tighten but not
  loosen. The access review shows the conditions and whether each member met
  them. The assistant applies the request's conditions (and, for a token, the
  `api_tokens` switch) at every tool step. Migration 0068.

### Fixed

- Desktop, *Graph* tab (#254): a click on a sub-assembly now turns the tab
  into that sub-assembly – tab title, header, toolbar and graph are its own –
  instead of re-centring the graph inside the tab of the assembly above it;
  a part opens its overview the same way. *← Back* leads back one step per
  click, and a click on the assembly one came from is that same step. A click
  on the item in the middle opens its overview and no longer adds a step:
  each click used to add one, so the back control appeared although nothing
  had changed. `Ctrl`/`⌘`+click opens a node in a tab of its own.
- Every web page starts with a *Skip to the main content* link (visible
  while it has the keyboard focus), and the 17 empty column headers (action
  columns, the corner of the inertia tensor) have a name for screen readers.
  The page accessibility test now requires both.
- An administrator revoking somebody else's API token is recorded in the
  token owner's history ("Revoked API token 'x' of lena"), no longer in the
  administrator's own (#173).
- CI: a unit test killed at its 1 s limit because the runner stalled no
  longer fails the run – failed unit tests get two more attempts (nextest
  `retries`); a hang still fails every attempt, a stall is reported as
  `FLAKY`. The 8-million-triangle limit test runs alone, and the vault tests
  (a disk flush per stored file) get 10 s on Windows.

## [0.2.7] - 2026-11-10

Changes since the `v0.2.6` tag.

### Added

- The *Material* field (*New item*, *Edit revision*, desktop app) suggests the
  materials of the library with their density while you type (a `<datalist>`,
  also without JavaScript); a name that is not in the library gets a hint.
  Free text stays allowed.
- Desktop: rearrange the object tabs by dragging (insertion marker, scrolling
  at the edge of the strip) or with `Ctrl+Shift+PageUp/PageDown`; switch tabs
  with `Ctrl+PageUp/PageDown` or `Ctrl+Tab`; arrow keys between tabs (WAI-ARIA
  tabs); right-click menu (move to start/end, close, close others, close tabs
  to the right).
- Desktop: a *Graph* tab on the revision view, a navigable graph of the
  item's neighbourhood (BOM children and references below, the assemblies
  that use it above). Colour shows the lifecycle state, size the item's own
  BOM complexity; every node carries the preview image and the file name.
  The view is kept while you move through the graph and it scales to large
  assemblies.
- Mattermost notifications as direct messages of a bot account, never into a
  channel (#230): `TEAMCRAFT_MATTERMOST_URL` and `TEAMCRAFT_MATTERMOST_BOT_TOKEN`
  (also `…_FILE`). TeamCraft looks the user up by username (cached), opens the
  direct channel and posts through the Mattermost REST API, with the full text
  also for restricted project groups (unless the group switched notification
  content off). An unknown or deactivated Mattermost user fails the chat
  delivery at once with a reason shown under *Notifications*
  (`chat_error` in `GET /api/v1/notifications`); there is no channel fallback.
  Outbox, retries and backoff are unchanged. The notification settings
  response has `mattermost_legacy_webhook`. No migration.

### Deprecated

- `TEAMCRAFT_MATTERMOST_WEBHOOK` and `TEAMCRAFT_MATTERMOST_DIRECT` for
  notifications (channel mentions leak personal notifications to everybody in
  the channel). They keep working when no bot is configured, log a warning at
  start-up and the account page says so; switch as described in
  [Upgrading](docs/src/admin/upgrade.md). `MATTERMOST_ERROR_WEBHOOK` is
  unchanged.

### Changed

- Mass properties (#227): the geometry file of a revision is picked
  automatically only when its 3D files are one model (same normalised name,
  e.g. `box.stl`, `box.prt`, `Box_rev-B.SLDPRT`), and the page, the exports
  and the MCP tool say so. Files of several different parts (an assembly
  attached as one item) are no longer reduced to the alphabetically first
  part: the revision gets the new source *geometry ambiguous*, adds nothing
  and flags the totals incomplete until a file is chosen. The likely whole
  assembly (root of the file references, or a name matching the item) is
  suggested with a one-click *Use …*; the file selector in *Mass settings*
  is grouped by model. REST, JSON, MCP (`geometry`, `root_geometry`) and the
  CSV (`geometry_file`, `geometry_choice`) carry the candidates and whether a
  file was chosen automatically or by hand. The preferred CAD program breaks
  ties between native formats.

## [0.2.6] - 2026-11-08

Changes since the `v0.2.5` tag.

### Added

- Mass properties of assemblies: total mass, centre of gravity and inertia
  tensor (about the CG and a reference point, principal moments) from the
  exported 3D files of the parts in the common assembly frame, the densities
  of an admin-managed material library or a per-revision override, and
  measured masses (with optional CG and inertia) that win over everything
  computed. Per-file length unit (mm, m, in) and number of units shown; open,
  non-manifold and inverted meshes are reported (fully inverted ones fixed).
  The mesh integrals run in the isolated preview process and are cached per
  content. Page per revision with CSV and JSON export,
  `GET /api/v1/items/{number}/revisions/{code}/mass-properties`,
  `GET /api/v1/materials` and the read-only MCP tool `get_mass_properties`.
  Migration 0061.
- Preferred CAD program (*Account → Preferred CAD program*): when a revision
  has files of the chosen program (e.g. NX), the native files of other CAD
  programs are listed in a collapsed group on the revision page, in the vault
  explorer and in the desktop app's Files tab. Neutral formats (STEP, JT, STL,
  PDF, …) always stay visible; display only. `GET /api/v1/me` returns
  `preferred_cad`, file formats carry `format_info.family`.
- 3D preview of Rhino models (`.3dm`, Rhino 1 to 8) from the meshes Rhino
  stores (mesh objects, render meshes of polysurfaces and extrusions, block
  instances placed); SubD objects are refined from their control net
  (Catmull-Clark with creases). Hidden objects and layers are left out. The
  thumbnail is rendered from the 3D preview; models saved without meshes
  show the picture Rhino stored.
- Branding (#184): administrators rename the header name and subtitle ("TeamCraft PLM") under *Admin → Branding*;
  effective at once in the web
  header, page titles, sign-in page, notification e-mails and the desktop
  title bar. `GET /api/v1/branding` returns `display_name` and `subtitle`,
  `PUT /api/v1/branding/name` sets them (admins). The footer, About, version
  and licence notices stay TeamCraft. `TEAMCRAFT_INSTANCE_NAME` is now only
  the default.

- Need-to-know project groups, step 5 (#151): access review of a restricted
  group (migration 0053, `project_access_reviews`). Page `/projects/{id}/access-review` for managers and
  administrators: members with role, group role, source (manual or identity
  provider), account state and last sign-in (stale after 90 days), the
  administrator policy and elevations, and a "reviewed" confirmation with an
  optional note that is dated and audited (`project.access_review`) and keeps
  the member list as it was. Evidence for auditors as CSV (page and
  `GET /api/v1/projects/{code}/access-review?format=csv`) or JSON, plus
  `POST` to confirm and `GET .../access-review/{id}` for the stored member list.
  The shorter session age for restricted groups (step 2) is not part of this.
- Data retention and cleanup: the hourly housekeeping now also removes the
  blobs of expired staged imports and upload sessions, trims the preview
  cache (`TEAMCRAFT_PREVIEW_CACHE_MAX_MB`, default no limit) and applies an
  optional audit log retention (`TEAMCRAFT_AUDIT_RETENTION_DAYS`, default
  keep for ever; one `audit.purge` entry per removal). New command
  `teamcraft vault gc` (dry run unless `--apply`) lists and removes vault
  blobs nothing refers to.
- Retention for large result files: a file can be set to *keep only the
  latest version* (`files.retention = 'latest'`, migration 0052; revision
  page menu, `PATCH /api/v1/files/{id}` `{"retention"}`, audit
  `file.retention`). Superseded versions are removed after
  `TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS` (default 7) by the housekeeping and
  by `teamcraft vault purge-versions` (dry run unless `--apply`) – never the
  newest version and never anything of a revision that is not in work;
  audited as `file.purge_versions`. The default stays `all`.

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
- Reviewer suggestions show people who changed the affected revisions as *knows the item, cannot review*
  (`cannot_review` in the REST and MCP
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
  reopening a change after decisions keeps them as an earlier round (`change_review_rounds`, `changes.review_round`,
  migration 0048). The change page and
  `GET /api/v1/changes/{number}` show earlier rounds and flag pending
  reviewers who cannot decide (deactivated, demoted, blind to an item). The
  author or an administrator replaces a pending reviewer while the change is in
  review (`POST /api/v1/changes/{number}/reviewers/{old}/replace`, audited
  `change.replace_reviewer`); other decisions are kept.
- Review policy per project group, step 2 of #152 (migration 0047): minimum approvals (default all), required reviewers
  and
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
  `teamcraft user create-token --scope`). `full` = read, write and approve (what every token had before; existing tokens
  and the desktop app's token
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
- Need-to-know project groups, steps 3 and 4 (#151; migration 0051): interface switches per restricted group
  (`api_tokens`, `mcp`,
  `assistant`, `webhooks`, `packages`; a switched-off interface sees the group
  as non-existent), a read and download log (`access_log`: views, file
  downloads, Pack and Go, BOM exports with channel and client address, one
  entry per object and user within five minutes) with a retention per group,
  `teamcraft access-log purge` (audited), `GET /api/v1/projects/{code}/access-log`
  and a form and table on the group page. `tests/interface_switches.rs` fails
  when an interface has no switch or no code applies it. Everything is allowed
  and nothing is logged by default.

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
  `TEAMCRAFT_LOCAL_AGENTS=off` lets administrators turn off local AI agents (the desktop app's *Agent* view, ACP), so
  that AI use goes through the
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
  journals (`.jou`), transcripts (`.trn`) and report files (`report-file-*.out`), STAR-CCM+ solution histories (`.simh`)
  and macros (`.java` naming STAR-CCM+), Gmsh meshes. Transcripts, journals, macros and
  Nastran text output are indexed as text; a large plain-text file keeps its
  last 64 KiB in the search text and `read_file_text`.
- Extraction templates: a new method `table` for whitespace-separated tables
  with quoted column names (Fluent report files; migration 0040), and
  `pick = last` reads the last 2 MB of a file larger than 2 MB first. New
  fuzz targets `cadformat` and `resultscan`.
- 3D preview of SOLIDWORKS parts (`.sldprt`, 2015 and later) from the
  display tessellation SOLIDWORKS stores in the file, refined along its
  exact surface normals like NX parts. SOLIDWORKS assemblies are put
  together from the previews of their part files, with the placements of
  their component tree; suppressed and hidden components are left out. The
  thumbnails of SOLIDWORKS parts and assemblies are rendered from the 3D
  preview.
- 3D preview of standalone JT files (JT 9 with topologically compressed
  triangle LODs; other versions report their version), graphical revision
  comparison (*Compare with …* lays another version or revision over the
  model) and 3D notes pinned to a picked point of a file version (#200).
- Storage quotas per user and for the whole vault (a refused
  upload gets 413 and stores nothing), optional malware scanning with ClamAV
  (`TEAMCRAFT_CLAMD_ADDR`; infected files are refused and never stored),
  likely duplicates by name (`GET /api/v1/items/duplicates`, a hint after
  creating an item), bulk revise and obsolete (`POST /api/v1/items/bulk`, up to
  200 items, one transaction each), and a section plane and point-to-point
  measuring in the 3D viewer (web and desktop) (#199).
- One-click copy of item numbers, printable QR labels of a revision (the QR
  code of the short link `/i/<number>`), and a warning before removing a
  file that CAD files of other items still reference (#202).
- New item form: chunked, resumable uploads of large files, as on the
  revision page (#164).
- Create a BOM from the CAD references the server found (#180): a preview with
  instance counts as quantities (NX placed instances, SOLIDWORKS component
  tree; otherwise 1, flagged as assumed), unmatched names and hidden items
  counted, then the chosen lines in one audited transaction. Web UI and
  `GET`/`POST /api/v1/items/{n}/revisions/{code}/bom/from-references`.
- Desktop: a structure view of assemblies in the navigator, and checking out
  an assembly gets its parts – missing parts are downloaded read-only, parts
  changed locally are never overwritten and are listed (#183).
- Adding files to an existing item (#181): the Files card says how (check out
  and add, locked by someone else, in review, frozen – revise), offers *Check
  out and add files* and takes dropped files; the desktop app has *Add files…*
  (copies into the working folder, checks out first if needed).
- New simulation from an item (#178): the form is prefilled from the item and
  the item is linked as the basis in the same transaction; `POST /items`
  takes `basis`.
- Simulation results: *Fill with AI…* on the Results card with a box to say
  which numbers to extract and which limits apply; the assistant proposes
  values that a person accepts (#177).
- Offboarding (#173, minimum fix): managers release the check-outs of
  deactivated members (also in bulk), demote managers (at least one active
  manager stays), take over the change orders of a deactivated author; a
  change with a deactivated reviewer cannot be submitted; runbook *When
  someone leaves* in the admin guide.
- Obsolete and supersede through a change order (#152 step 3): a group
  setting `obsolete_requires_change`, change order lines with the action
  `obsolete` or `supersede` applied on release, and watchers are notified of
  a supersede.
- Need-to-know groups (#151): the switches `notification_content`,
  `external_llm` (with `TEAMCRAFT_ASSISTANT_INTERNAL_URLS`) and
  `desktop_sync`.
- Mutation testing with cargo-mutants (#170): configuration, a report-only
  check on pull requests and a weekly run; the gaps of the first
  whole-repository run are closed with unit tests (#207).

### Changed

- Desktop app, local agents: calls of TeamCraft's own read-only MCP tools
  (by the server's `tools/list` annotations) are allowed once without a
  permission prompt and marked *Allowed automatically (read-only)* in the
  tool list; the server still answers with the user's rights only. Write
  tools, other servers' tools, the agent's own tools and every request the
  app cannot identify for certain still ask. *Settings → Agents… → Ask
  before read-only TeamCraft tools* restores a prompt for every call.
- Desktop: *Ask TeamCraft* and *Agent* are one view. The company AI (the
  server's assistant, with page context, quick actions, the drafts checkbox
  and the conversation kept on the server) is a backend of the *Agent* view (`Ctrl/Cmd+4`, the *Agent* button of
  revision and change tabs); the *Ask*
  rail button, the drawer beside the tabs, *View → Ask TeamCraft*
  (`Ctrl/Cmd+5`) and the *Ask…* toolbar buttons are gone. The web UI keeps
  its sidebar.
- Extraction templates for `.html` files run on the extracted text instead of
  the raw markup.
- Text and CSV files keep their lines and decimal numbers in the stored text,
  so `read_file_text` shows the real text (only ASCII STL, OBJ and KiCad files
  still drop decimal numbers for the search). Files uploaded before keep their
  old text until a new version is uploaded.
- Admin → Attributes: the field "Order" is called "Position in forms" (#179).
- CI runs the tests with `cargo nextest`, and a unit test fails after 1 s
  unless it is slow on purpose (#209).

### Fixed

- The sign-in and error pages showed the logo twice in dark mode (#182).
- After a refused migration (changed checksum, dirty version) the next start
  no longer waits forever for the migration lock (#209).
- `POST /api/v1/change-requests` checks the 100-item limit before looking up
  the items (#163).
- Tests and CI on `main` after merges: audit guard exemptions, the
  `file_versions_solver_summary_object` check is validated,
  unique migration numbers with a guard test, rustdoc links and test
  fixtures (#196, #204, #208).

## [0.2.5] - 2026-11-07

Changes since the `v0.2.0` tag.

### Added

- The desktop app tells the user when its version does not fit the server (version handshake, see the support policy).
- The desktop settings dialog has the notification settings of the web account
  page (what to be told about, e-mail, Mattermost, send a test);
  `GET /api/v1/notifications/settings` also returns the form description.
- The desktop settings have *Change password…* (`POST /api/v1/me/password`;
  hidden for single-sign-on accounts, `GET /api/v1/me` has `auth_source`).
  Wrong current passwords of the own password change, in the web form and over
  REST, count like failed sign-ins (429 with `Retry-After`) and are audited as
  `user.password_failed`.
- Administrators can replace the wide logo (top left) and the square logo (bottom left, tab icon) under *Admin →
  Branding*; the desktop app shows them
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

[Unreleased]: https://github.com/rudalabs/TeamCraft/compare/v0.2.7...HEAD
[0.2.7]: https://github.com/rudalabs/TeamCraft/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/rudalabs/TeamCraft/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/rudalabs/TeamCraft/compare/v0.2.0...v0.2.5
