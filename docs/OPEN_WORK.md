# Open work

The to-do list of TeamCraft, in order of priority, then what is done, then details on the open features.
Tick an entry (`[x]`) when it is done and move it to *Done*. Last update: 2026-11-08.

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
5. **Feature demand** – the backlog of what users ask of PLM/PDM systems and TeamCraft lacks; candidates for such
   issues, not commitments.

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

| Version   | Theme                    | Exit criteria                                                                                                                                                                                                                        |
|-----------|--------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **0.2**   | Proofing                 | Sections 1 and 2 of the to-do list mostly done: migration, backup, import and atomicity tests, fuzzing of every parser, `cargo deny`, coverage report, branch protection; the large modules split                                    |
| **0.3**   | Real data                | The CAD-program checks of section 3 with real files (Inventor, Solid Edge, SOLIDWORKS, NX, STEP exports); scale check (#47) with targets met; e-mail/Mattermost against real servers                                                 |
| **0.5**   | Beta – frozen interfaces | Assistant with OpenAI-compatible providers shipped (its config is part of the freeze); API, MCP tools, config variables and CLI reviewed and frozen (deprecations marked); upgrade path tested from 0.2 on; first outside test teams |
| **0.9**   | Release candidate        | No known data-loss or security bug; docs complete; one RC per fix round, two weeks without a blocker                                                                                                                                 |
| **1.0.0** | General availability     | Work-in-progress notices removed (README, docs, web footer, desktop *About*); support policy published                                                                                                                               |

**GA criteria** (all must be ticked before 1.0.0)

*Data safety and upgrades*

- [ ] Schema upgrades from every 0.x release since 0.2 and between all 1.x versions are automatic and tested in CI
  with seeded databases (see *Migrations* in section 1); no edited migrations after 0.5 (`db::EDITED_BEFORE_RELEASE`
  empty and frozen)
- [ ] Backup/restore tested for every release in CI, including restore of an older version's backup; documented
  restore drill; `teamcraft vault check` finds and reports every inconsistency between database and vault
- [ ] All multi-row mutations proven all-or-nothing and safe under concurrency (section 1, *Atomic processes*)
- [x] Upgrade guide in the docs (`docs/src/admin/upgrade.md`): before/after steps, rollback by restore, breaking
  changes per version

*Stable interfaces*

- [ ] REST API `/api/v1` reviewed: consistent names, error shape, pagination and filters on every list; an OpenAPI
  description generated from the routes and checked in CI against the router; deprecation policy (an endpoint lives
  at least one minor version with a `Deprecation` header)
- [ ] MCP tools and prompts: names and argument schemas frozen and versioned, contract tests per tool
- [ ] Configuration: every variable documented and validated at start (unknown `TEAMCRAFT_*` variables warned),
  defaults reviewed; CLI commands and exit codes documented and frozen
- [x] Desktop app ↔ server compatibility: version handshake, the desktop tells the user when the server is too old
  or too new; a supported-versions table – done in #133 (`desktop/src/compat.rs`; table under *Desktop app and
  server versions* in [support policy](src/admin/support-policy.md))

*Assistant*

- [ ] One provider-neutral LLM API layer (`src/llm/`) with the Anthropic Messages backend and an OpenAI-compatible
  Chat Completions backend (OpenAI, Azure OpenAI, Mistral, OpenRouter, Groq, local Ollama/vLLM/LM Studio) – phases 1
  and 2 of [agent-interface](src/research/agent-interface.md): provider, base URL, model and key (`_FILE`) in the
  configuration, `teamcraft assistant check`, the loop tests run for every provider, recorded fixtures per server
- [ ] Per-user daily budget and admin feature switches for the assistant (phase 4), so that an operator can enable
  it with a bought key without surprises

*Security*

- Open findings of the [threat model](src/dev/threat-model.md) (F1–F19 there):
    - [x] F1: failed password sign-ins throttled per username and client address (`TEAMCRAFT_SIGNIN_MAX_FAILURES`,
      429 with `Retry-After`)
    - [x] F1 (rest): wrong current passwords of the own password change (web form and `POST /api/v1/me/password`)
      count like failed sign-ins (429 with `Retry-After`, audited as `user.password_failed`)
    - [x] F2: scoped PATs (`full` = read+write+approve, `write` = no approving, `read_only`, `mcp_only`;
      existing and desktop tokens stay `full`; a token cannot mint a wider one). Left: a narrower desktop token by
      default,
      per-group scopes, sign-off re-authentication (#152 step 4)
      (also the complement of `TEAMCRAFT_LOCAL_AGENTS=off`, which closes the MCP endpoint for every token but
      not the REST API: a read-only token would narrow what an agent holding a user's own token can do there)
    - [x] F3: sign-in, failed sign-in and sign-out in the audit log (`user.sign_in`, `user.sign_in_failed`,
      `user.sign_out`; no password, token or unknown typed name)
    - [x] F4: web posts without `Origin` and `Referer` are refused; install guide says the proxy must overwrite
      `X-Forwarded-Host`
    - [x] F5: `Strict-Transport-Security` when the public URL is https
    - [ ] F6: OIDC with a plain `http://` issuer: refuse it outside `localhost`, or verify ID token signatures (JWKS)
      for it – a decision, since it can break installations with an internal IdP
    - [x] F7: end TeamCraft sessions of users removed at the IdP – OpenID Connect back-channel logout
      (`/auth/oidc/backchannel-logout`, #173 step 9). Left: shorter SSO sessions for providers without back-channel
      logout (`TEAMCRAFT_OIDC_SESSION_HOURS`, unpublished `ga/oidc-hardening` work)
    - [ ] F8: re-check the hash of a blob while it is downloaded (or periodically), not only in `vault check`
    - [x] F9: disk quota per user and in total for uploads (`services::quotas`, `teamcraft user quota` /
      `teamcraft vault quota`, `tests/quotas.rs`)
    - [ ] F10: serve downloads with a Content-Type derived from the format (the download headers are pinned in
      `tests/web_security.rs`)
    - [ ] F11: a memory limit for the preview and text children on Windows (job object); a smaller cap for 3MF entries
    - [x] F12: start the parser children with a cleared environment (no database URL, no keys)
    - [ ] F13: a cap on the unpacked size of XLSX/ODS tables, or read them in the isolated child
    - [ ] F14: fuzz targets for `simmesh` and `pdftext` (`resultscan` and `cadformat` have theirs since #156; the
      no-panic lints and the 3DS and Kelvin fixes are in `main` since #106/#108)
    - [ ] F15: prompt injection: confirmation of assistant drafts per write, or taint for MCP sessions as well
    - [ ] F16: per-user quota for the assistant and MCP (with the daily budget in section 2, *Assistant*)
    - [ ] F17: `teamcraft mcp`: no redirects, a warning (or refusal) for `http://` servers other than `localhost`
    - [x] F18: desktop token in the OS keychain (Windows Credential Manager, macOS Keychain, Secret Service); warn on
      `http://` server URLs – file fallback with a warning where there is no keychain
    - [ ] F19: backups written with mode 0600/0700, a signed manifest (optional encryption done:
      `teamcraft backup --encrypt`, `src/backupcrypt.rs`)
- [ ] External review or pentest of authentication, read permissions and file parsing; fuzzing of all parsers
  running continuously (section 1)
- [ ] `cargo deny`/`cargo audit` green in CI, SBOM (CycloneDX) attached to each release, release artifacts with
  build provenance (GitHub attestations); private vulnerability reporting enabled (`SECURITY.md`)
- [ ] Session, token and password policies reviewed (expiry, rotation, rate limiting of sign-in and API)

*Operations*

- [~] Metrics endpoint (Prometheus) for requests, latency, outbox backlogs (notifications, webhooks), preview
  queue, database pool; structured JSON logs; documented alerts
- [ ] Performance targets met and checked by the scale harness (#47): e.g. 1 M files, 100 k items, BOMs of 10 k
  lines, 50 concurrent users – page and API p95 times written down and guarded in a scheduled CI job
- [ ] Production install paths tested end to end per release: Docker Compose (`deploy/`), Linux packages (systemd
  unit), behind a reverse proxy with TLS; documented sizing and PostgreSQL versions (16–18)
- [x] Data retention and cleanup: old previews, staged uploads, expired sessions, audit log retention setting
- [x] Preview safety: size caps per kind and a memory limit for the preview process (`TEAMCRAFT_PREVIEW_MAX_*`); big
  result files are stored, never parsed

*Clients and packaging*

- [ ] Flatpak on Flathub with offline cargo sources; AUR packages published. Done: the desktop app's update notice
  (daily GitHub release check, opt-out, a link only – no silent auto-update). Binaries and installers stay **unsigned**
  (no code signing or notarisation): the checksums
  in `SHA256SUMS.txt` and the build provenance are the verification, and the docs explain the first start past
  SmartScreen and Gatekeeper
- [ ] Desktop app run and tested on Windows x64/ARM64, macOS and Linux for each RC (a short manual test script in
  `docs/src/dev/release-checklist.md`)
- [x] Accessibility: keyboard use and screen-reader labels on the main web flows checked (WCAG 2.2 AA for contrast
  is already tested) – `every_rendered_page_is_accessible` checks labels, accessible names, `alt`, `tabindex`,
  `lang`, heading levels and `<th scope>` on every rendered page; sign-in, new item, check-in, BOM and new change
  request done with the keyboard in Chromium; toggled panels now close with Escape ([docs](src/dev/accessibility.md))
- [x] Accessibility: a skip link to the main content (eight stops come before it on every page) – checked by
  `every_rendered_page_is_accessible`
- [ ] Accessibility: the HTML built by JavaScript (assistant sidebar, BOM tree controls, 3D viewer, upload progress)
  and the desktop app's UI under the same checks as the templates
- [x] Accessibility: a text (visually hidden) for the empty action column headers (`<th scope="col"></th>`) – 17
  found and fixed; every `<th>` needs a name now
- [ ] Accessibility: the main flows once with a screen reader (NVDA or VoiceOver)

*Documentation and project*

- [x] `CHANGELOG.md` (Keep a Changelog) from 0.2 on, written per PR; release notes from it
- [ ] User guide, admin guide and API/MCP reference complete and checked against the 1.0 behaviour; screenshots
  current
- [x] Support and versioning policy: SemVer from 1.0, which versions get security fixes (e.g. the latest minor and
  the previous one for 6 months), how to report bugs
- [x] Release checklist (`docs/src/dev/release-checklist.md`): green main, RC soak time, version bump in both
  `Cargo.toml` files, tag, verify assets and checksums, image tags, announcement
- [ ] Governance basics: maintainers, review rule (one approving review, green checks), code of conduct

**Not required for 1.0** – CAD add-ins, more 3D readers, the Jira connector, code-signed binaries, the assistant
features after the budget (threads, streaming, quick actions), the shared UI folder beyond what is already merged, i18n
of the UI (English only in 1.0; the
structure should not prevent translations later). They may land in 1.x minor releases.

## To-do

### 1. Proofing

**Tests** (priority: more unit tests)

The feature list is covered broadly since #65 (and #67); the weak spots are the processes that must never leave
half a state behind. Prefer tests that fail on a real regression over tests that restate the code.

- [x] **Import** – BOM import (`services::imports`, `bomtable.rs`, `stepbom.rs`): dry-run plan = import over generated
  tables, rollback, idempotent re-import, cycles, quantity/unit/position edge cases, malformed STEP
  (`tests/import_proofing.rs`, `tests/import_atomicity.rs`); desktop folder import per item and resumed after an
  interruption (`tests/folder_import.rs`, the requests of `desktop/ui/app.js` against the server): an item whose
  files did not arrive is completed by the next run instead of being skipped for good (fixed), none is created
  twice. Open: an item whose *creation* answer was lost (network gone between the server's commit and the app's
  log) is created again on the next run – fixed in code with an idempotency key for `POST /items`
  (`tests/item_idempotency.rs`) – **database tests written, not yet run against a real database**
- [ ] BOM import time grows faster than the number of lines: 1 000 lines 3.9 s, 10 000 lines 101 s on a developer
  machine (2026-11-12, debug build, `ten_thousand_lines_import_in_one_transaction`), about n^1.4. Find the step that
  grows (plan lookups, per-row statements, counters) and make it linear; then lower `MAX_TEN_FOLD_GROWTH` in
  `tests/import_scale_and_concurrency.rs` from 60 towards 20
- [x] **Atomic processes** – the folder import per item is covered too (see *Import*); the rest is done, see *Done*
- [ ] Coverage map: keep the feature → tests table of #65 in `CONTRIBUTING.md` (or `docs/src/dev/`) up to date, and
  make CI fail when the integration tests are skipped (`TEAMCRAFT_REQUIRE_DB_TESTS=1`, already set) – #65 was merged
  with two failing integration tests; merge only with green checks

**Parsers and CI gates**

- [x] Fuzz targets (`cargo fuzz`, own `fuzz/` crate outside the server build) for every reader of untrusted files:
  `mesh` (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, 3DS, point clouds), `simmesh` (VTK, VTU/VTP, FRD), `stepbom`, `swpart`,
  `nxpart`, `thumbnail`, `cadrefs`, `bomtable`, `textindex` – entry points in `src/fuzz_harness.rs`, the stable corpus
  test `tests/parser_corpus.rs` and the weekly *Fuzzing* workflow (`fuzz.yml`), merged with #94; crashes become
  regression unit tests with the minimised input ([docs](src/dev/fuzzing.md)) – open: watch the first scheduled runs
- [x] No `unwrap`/`expect`/indexing panics on file input outside tests: every reader of untrusted files – `swpart`,
  `mesh` (with `mesh::step`, `p21`, `tessellated`), `simmesh`, `stepbom`, `nxpart`, `thumbnail`, `cadrefs`,
  `bomtable`, `textindex`, `resultscan` – and `config` deny `clippy::unwrap_used`, `expect_used`, `indexing_slicing`,
  `panic` and `unreachable` (`#![deny]` in each module; `clippy.toml` allows them in tests); reads go through bounded
  helpers (`mesh::bytes_at`, `u32_le`, `range`, …) or `get`/slice patterns, also for `&str` cuts the lint does not
  see; one reviewed `#[allow]` (`mesh::compose`, fixed arrays). **Two bugs found and fixed**: a 3DS vertex or face
  list chunk of the minimum length at the end of the file read its count past the last byte (now skipped, test
  `three_ds_list_chunks_without_their_count`); the reference scan cut a name inside a character when the extension
  held a letter whose lower case is ASCII (the Kelvin sign `K` → `k`, test
  `a_non_ascii_letter_in_the_extension_does_not_panic`)
- [x] Property tests (`proptest`) for the pure rules in `domain.rs` (`domain::property_tests`): revision letters,
  lifecycle transitions, numbering scheme parsing, formatting and reading back – open: skin contrast
- [x] Coverage measured per PR (`cargo llvm-cov`, PostgreSQL 18 leg of the CI job *tests*, before 2026-11-07 its own job
  *coverage*: summary and HTML artifact, see [coverage map](src/dev/coverage.md)) with a floor that may not drop: the
  job fails below `.github/coverage-floor` (91.1 % of lines; `main` was at 91.48–91.65 % over four runs); the coverage
  map is refreshed from the CI numbers of `b4cf014`
- [ ] Mutation testing
  (`cargo mutants`, [research](src/research/mutation-testing.md), [how to](src/dev/mutation-testing.md)):
  `.cargo/mutants.toml` examines the pure modules with `cargo test --lib` (no PostgreSQL); workflow `mutants.yml`
  runs `--in-diff` on pull requests (report-only) and a weekly sharded full run. Done: `httprange` (0 missed) and
  `util` (3 missed → 0, `byte_sizes_stop_at_tib`), plus the field mutants of `resultscan::TemplateInput::validated`
  (7 missed → 0, `template_input_is_stored_normalised`). Next, in this order: `pagecontext` (58 mutants, 5 tests),
  `src/domain/**` (~500, the rules – lifecycle, roles, revisions, validation; the unit tests sit in `domain/mod.rs`,
  several submodules have none of their own), `cadformat` (287), `htmltext` (263), `resultscan` (193), `cadrefs`
  (189), `bomtable` (166), `stepbom` (155) – read the first weekly report, fix or document every missed mutant per
  module. Then: make the PR job blocking once the examined modules are clean; add `llm/anthropic.rs`,
  `llm/openai.rs`, `mesh/*` readers and `simmesh`; decide whether services with database tests are worth a second,
  PostgreSQL-backed run (`--test-package`/integration tests – hours per run); speed up the slowest lib tests (18 s of
  each mutant's ~30 s is the lib test run)
- [x] `cargo deny` in CI (`deny.toml`, job *dependencies*): permissive licences only, security advisories, unknown
  registries and git sources; duplicate versions only warn. Three transitive "unmaintained" notices are ignored with
  reasons in `deny.toml` (cgmath, proc-macro-error, ttf-parser) – open: replace them when the parent crates move
- [ ] Branch protection on `main` (**maintainer action**, needs admin rights): a ruleset with required checks – fmt and
  clippy, tests on PostgreSQL 16 and 18, build (windows-latest), cargo deny (Cargo.toml), local links and images.
  None exists today (the API answers 404 for the protection and `[]` for rulesets); that is how #65 and #137 were
  merged with red database tests
- [x] CI review (2026-11-07, every job checked against its run history, each cut challenged twice): runner minutes
  are free for this public repository, the scarce resource is concurrency (20 jobs, 5 macOS; PR jobs waited up to
  29 min). Done: `timeout-minutes` on every CI job (two runs held a runner for the 6 h default); coverage runs in the
  PostgreSQL 18 test leg instead of its own job (PostgreSQL 16 stays the plain leg with the CLI smoke test); the
  macOS server build runs on `main` only (no macOS server is shipped), Windows on every PR but without linking the
  63 integration-test binaries (`cargo check --all-targets`); PR runs read the Rust caches of `main` but no longer
  write their own (the cache was at 10.95 GB of 10 GB); `cargo deny` also checks `desktop/Cargo.lock` (report-only
  for now) and runs weekly; desktop PRs run one leg per release architecture (Ubuntu 24.04 x64 and ARM64, macOS,
  Windows x64 and ARM64 – the last report-only while the runner is a preview), `main`, a weekly run and manual runs
  add Ubuntu 26.04, and `cargo fmt` runs once
  instead of six times; screenshot-only commits no longer start desktop builds; superseded PR runs of the docs and
  CodeQL are cancelled, CodeQL skips docs-only PRs
- [x] `cargo deny` for `desktop/Cargo.lock`: the first run found three "unmaintained" notices (zvariant,
  zbus_names, zvariant_derive – merged into zbus 6, pulled in by keyring and Tauri through zbus 4/5), ignored with a
  reason in `deny.toml`; the job blocks like the server one
- [ ] Flaky desktop test on macOS: `resume::tests::a_wrong_part_is_started_over_and_a_complete_one_only_renamed`
  (`desktop/src/resume.rs`, the assertion on the recorded `Range` headers) fails in about 2 of 15 runs – record the
  header before the test server answers, or compare the set of ranges instead of their order
- [x] Release builds on `main` (#275): every push to `main` built all 14 release jobs (~110–170 runner-min) and
  a newer push cancelled the running build, so about half of the commits on `main` showed a red ✗ without a real
  failure. Now nightly (skipped when only documentation changed since the last good build), on `v*` tags and by
  hand; `:edge` follows `main` within a day. A pull request that changes `packaging/**` or
  `desktop/tauri.conf.json` builds everything before it is merged. CI, CodeQL and the desktop workflow no longer
  cancel or replace runs on `main` either
- [ ] Run `scale.yml` once by hand – its schedule has never fired yet – and make a failing scheduled run open an
  issue instead of only an e-mail
- [x] `fuzz.yml` had never built a single target: the prebuilt `cargo-fuzz` from `taiki-e/install-action` is a musl
  binary, so `cargo fuzz` defaulted `--target` to `x86_64-unknown-linux-musl` (not installed; its static libc the
  sanitizer refuses), and every target failed before fuzzing. The first scheduled run (2026-11-08) failed for this,
  not for a crash. Fixed by building for the GNU host triple (`fix/fuzz-build-target`); the targets themselves compile
  (`cargo +nightly check` on `fuzz/`). Still to do: make a failing scheduled run open an issue, not only an e-mail
- [x] Triage the open CodeQL alerts (2026-11-07, 32 alerts, each checked twice; `fix/codeql-triage`). One real
  finding, the rest false positives, all answered in code or configuration, none by a model row:
    - `rust/uncontrolled-allocation-size` #38 (`POST /api/v1/change-requests`) **real, low risk**: the handler
      resolved every listed item before the 100-item limit was checked, and that limit counted distinct rows, so a
      2 MB body of one repeated item ran ~150 000 lookups and inserts in one transaction and succeeded. Fixed:
      `requests::check_item_count` in the handler before any lookup and again in `requests::create`; test
      `too_many_items_are_rejected_before_any_is_resolved`; threat model row. #39 (`simulations::import_results`)
      was already bounded by `MAX_RESULTS` one line earlier – the pre-sized `Vec` is gone so the bound is plain.
    - `rust/hard-coded-cryptographic-value` #1–#5 (`src/auth.rs`): the timing-equaliser `DUMMY_HASH` and the round
      trip test now hash random values instead of literals; #49/#50 (`src/domain/mod.rs`): the password policy unit
      test is generated at the `MIN_PASSWORD_LEN` boundary; #8–#14 (`src/oidc.rs`, nonce literals in unit tests)
      and #15/#53/#54 (`tests/`): test fixtures – the OIDC unit tests moved to `src/oidc/tests.rs` and
      `.github/codeql/codeql-config.yml` leaves `tests/**` and `src/**/tests.rs` out of the analysis (Rust runs with
      build-mode none, where analysis `paths-ignore` applies).
    - `rust/cleartext-storage-database` #16 (`src/delivery.rs`) and #17 (`tests/`): the source was only the name of
      the SQL builder `notifications::confidential_now` (sensitive-name heuristic; it returns constant SQL text) –
      renamed `restricted_now_sql`; #52 (`tests/common`): test-only, excluded.
    - `rust/cleartext-logging` #21–#23 (`teamcraft user create|set-password|list`): the operator's own stdout shows
      account names, never a password. The report is now written with `writeln!` (a closed pipe such as
      `user list | head` is an error, not a panic); #18–#20, #41, #51: assertion messages in `tests/`, excluded.
- [ ] After the first CodeQL run of `fix/codeql-triage` on `main`, check that all 32 alerts closed. Not verifiable
  locally (no CodeQL CLI): whether the `writeln!` output still counts as a logging sink (#21–#23 – if it does,
  dismiss them in the GitHub UI as false positive, "admin CLI echoes the account names the operator asked for") and
  whether the path-injection barrier model (#160/#161) closed its 16 alerts
- [ ] Dependabot (or Renovate) for Cargo and GitHub Actions, and a `Cargo.lock` for `fuzz/`

### 2. Structure

- [x] Split the largest modules by area, without behaviour changes, one PR each: `src/mcp/tools.rs` (~3000 lines →
  one file per area like `services/`) – mcp done (`src/mcp/tools/*.rs`; tool table, results and dispatch stay in
  `tools.rs`; `tests/mcp_contract.rs` snapshots tool list, schemas and prompts), `src/domain.rs` (~2600 → `domain/{roles,lifecycle,revision,numbering,
  skins,validation}.rs`) – domain done (`src/domain/*.rs`, re-exported), `src/api/mod.rs` (~2000 → route table +
  handlers per area, like `src/web/`) – api done (`src/api/*_api.rs`, route table stays in `mod.rs`),
  `src/services/requests.rs` (~1900) – requests done (`src/services/requests/*.rs`, re-exported;
  `tests/change_request_paths.rs` pins the REST paths), `desktop/ui/app.js` (~5000 → ES modules per view, no bundler) –
  desktop done (`desktop/ui/js/*.js`, `js/main.js` starts the app; `scripts/check-desktop-modules.mjs` in the CI job
  `ui` checks that every import resolves and every name of another module is imported)
- [ ] One request/response contract for web, REST and MCP where they do the same thing (e.g. BOM, where-used,
  change requests): shared view structs in `services::*` instead of three hand-built JSON/HTML shapes
- [ ] Typed ids (`ItemId`, `RevisionId`, `ChangeId`, … as newtypes over `Uuid`) at the service boundary, so that ids
  cannot be swapped – start with the services that take several ids
- [ ] Document the invariants per service function (permission check, rows locked, audit action, notifications and
  webhooks emitted) in its doc comment; `cargo doc` then is the reference for reviewers – started with an
  `# Invariants` section on `files::check_in`, `changes::review`, `interchange::replace`, `save_as::save_as`,
  `uploads::finish` and `projects::set_visibility`, then on every other function in `src/services/` that writes in
  a transaction or an audit record; still open: `services::requests` and `src/mcp/` (wait for their splits)
- [x] Architecture decision records in `docs/src/dev/adr/`: access model (deny list on the actor, 404 for hidden),
  outbox for notifications and webhooks, content-addressed vault, no Node.js – each with context, decision,
  consequences, alternatives (or "not documented") and the code and tests that hold it

### 3. Fixes and verification

**Fixes**

- [ ] Reference scan: false references in real files – NX parts fixed (stream container, `src/nxpart.rs`); noise in
  JT files fixed (binary bytes around a name, `tests/cad_fixtures.rs`); the STEP header export name (`FILE_NAME`) is
  a second name of the file for references of other files, used only where no file has the name itself (migration
  0036, `tests/step_export_names.rs`; impacts, reviewers, readiness and the desktop working set still match the file
  name only); open: the scan (`src/cadrefs.rs`) is a heuristic that was not tested with real files of the CAD programs
  yet (see the CAD sections)

**Tests with real data**

- [ ] Solid Edge test files: none openly licensed found – create a part, a sheet metal part and an assembly with Solid
  Edge (Community Edition) and publish them under CC0
- [x] End-to-end test against the server with the fixtures: folder import of `tests/fixtures/cad/nist/mtc-assembly/nx`
  and `…/solidworks` → items, files and their folder, references, BOM lines, check-ins (`tests/folder_import.rs`);
  found and fixed: an NX folder got no BOM lines (NX assemblies are `.prt` files of the kind "model"); an NX
  assembly is still imported as an item of type part
- [ ] The same fixtures: preview images and the STEP preview of the imported files
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

**Need-to-know project groups (#151)** – done: step 1 (administrator access: policy, reason-based elevation,
`none`), step 2 (conditions on membership: IdP claims, allowed networks, maximum sign-in age;
`services::membership_conditions`, `tests/membership_conditions.rs`), step 3 (switches `api_tokens`, `mcp`,
`assistant`, `webhooks`, `packages`, `notification_content`, `external_llm`, `desktop_sync`), step 4 (read and
download log incl. MCP and assistant tool reads and desktop Get, log page with filters and paging, retention and
`teamcraft access-log purge`), step 5 (access review, evidence, review schedule with reminders and the review-due
flag), step 6 (token scopes). Left, optional: a PDF/image watermark of downloads; claims for local accounts (an
administrator-maintained attribute) – local accounts never meet a claim condition today. Open question 1 is
answered for now: `none` is allowed, but needs a member who is a manager.

Only finish what is in progress and fix bugs here; see *Direction*. The CAD-program entries are mostly checks with
real files and belong to section 3 once such files are at hand.

**Ownership and approval rules ([#152](https://github.com/rudalabs/TeamCraft/issues/152))**

- [x] Step 1: wider separation of duties (editors and the acting admin cannot review; suggester excludes them).
- [x] Step 2: review policy per project group (min approvals, required reviewers, role group). Left: show the policy on
  the change page, notify when a role group has no eligible reviewer. (After submit only the parts the assigned
  reviewers can meet count, instead of a frozen snapshot.)
- [x] Step 3: obsolete and supersede through a change order (group setting `obsolete_requires_change`, obsolete and
  supersede lines on a change order applied on release, supersede notifies watchers). Left: an
  `obsolete_requires_change` toggle in the desktop app; lines from MCP and the desktop app.
- [x] Step 4: token scopes (`approve`: done, scope `write` cannot approve) and sign-off re-authentication
  (`TEAMCRAFT_APPROVAL_REAUTH` = `release` | `every_decision`: password or a fresh SSO sign-in with `prompt=login`,
  method and meaning stored per review; `tests/approval_sign_off.rs`, `tests/oidc_flow.rs`). The meaning is free
  text (open question 7: no fixed list yet).
- [x] Step 5: keep review rounds (`change_review_rounds`, `changes.review_round`; change page and API show earlier
  rounds).
- [x] Step 6: flag and replace absent reviewers (substitutes per user: still open).
- [x] Step 7: item owner and admin handover of a leaving user's work (owner on the item page; notifying the new owner
  and a CLI command are not done).
- [x] Step 8: approvals in the package manifest (`released_by`), audit log export (CSV/JSON, admin page and
  `GET /api/v1/audit`, `tests/approval_records.rs`). Hash chain: not done – open question 6 answered with the
  database grant (`REVOKE UPDATE, DELETE` on `audit_log`, documented in admin/operations.md).
- [x] Step 9: requester cannot decide own request; separate approval of *use as is* deviations (group settings
  `requester_cannot_decide`, `concessions_need_review`; `tests/review_rules.rs`).
- [x] Follow-up of step 1: show editors in the suggester as "knows the item, cannot review".

**Offboarding for high-turnover teams ([#173](https://github.com/rudalabs/TeamCraft/issues/173))**

- [x] Step 0: runbook "When someone leaves" in `docs/src/admin/users.md`.
- [x] Step 1: holdings report per user (`services::offboarding::holdings`: page `/users/{id}/holdings` for admins and
  group managers, `GET /api/v1/users/{id}/holdings`, `teamcraft user holdings`), filtered by the viewer's rights. Left:
  show it as a confirmation list when an administrator unticks *Active* (with step 4).
- [x] Step 2: inactive lock holders count as gone, only active managers count, a deactivated user's IdP membership can
  be removed, submit refuses a deactivated reviewer, *Demote* manager.
- [x] Step 3: bulk release of another user's check-outs (`checkouts::release_held`; admin user page, group member list).
- [x] Step 4: deactivation as an offboarding step (`services::deactivation`: hand-over incl. manager roles, token
  revocation, release of check-outs; *Offboard and deactivate* on the user page, `POST /api/v1/users/{id}/offboard`,
  `teamcraft user deactivate`).
- [x] Step 5: managers of every affected group withdraw, reopen, cancel or take over a deactivated author's change.
- [x] Step 6: a manager releases an active member's check-out with a reason (`revisions::cancel_checkout` with
  `reason`: audited, told to the holder; *Release with reason* on the revision page, `{reason}` in REST). Lock age is
  shown in the explorer, on the revision page and in the holdings report. Open: a timed stale-lock rule
  (`TEAMCRAFT_STALE_CHECKOUT_DAYS`), issue question 3.
- [x] Step 7: groups without an active manager, or whose managers showed no activity for 90 days
  (`services::manager_gaps`: *Groups without an active manager* on the project overview of administrators,
  `GET /api/v1/projects/manager-gaps`; the hourly housekeeping tells the administrators once per gap).
- [x] Step 8: season handover per group (`services::season`, *Season handover* at `/projects/{id}/season` for managers
  and admins): tick members (or those inactive since a date / past "member until"), then one run makes the new lead a
  manager, releases check-outs with a reason, sets "member until" (`project_members.expires_at`, flag only), removes
  members and demotes the old managers – each step through its own service and audited. Left: the access review page
  for internal groups, ownership hand-over from the season page (today: *Hand over work* on the user page).
- [x] Step 9: IdP-driven offboarding – (a) *SSO users without sign-in* on the admin *Users* page
  (`services::offboarding::idle_sso_users`: no sign-in or token use for 90 days, still holding memberships, tokens or
  check-outs); (b) `TEAMCRAFT_OIDC_TOKEN_IDLE_DAYS` caps new tokens of SSO users (`users::token_lifetime_for`);
  (c) OpenID Connect Back-Channel Logout (`oidc::verify_logout_token`, `users::idp_logout`, migration 0071), tests
  in `tests/oidc_flow.rs` with a signed logout token of the fake provider; (d) SCIM: research page only
  (`docs/src/research/scim-deprovisioning.md`, not now). Left: a logout token with only `sid` (refused today), an
  API route for the idle report.
- [x] Step 10: desktop working folder – *Check in all* and *Sign out* list the check-outs with local changes
  (`layout::local_changes`) and offer to check them in first (sign-out also to park them); *Park…* uploads work in
  progress as new versions and keeps the lock (`POST …/files`, `files::attach`); after a forced release the
  Workspace tab says *Released by X; your local changes are kept and need a new check-out* (`released` of
  `…/file-status`, `checkouts::released_from`, read from the audit log).

**Desktop app**

- [x] #183: assemblies in the desktop – *Structure* view in the navigator (`GET …/structure`), check-out of an
  assembly gets its parts read-only and never overwrites local files. Open: size prompt for very large assemblies,
  the *No BOM – N parts referenced* hint node, the web vault explorer toggle, check in the real app
- [ ] Check #58 in the real app (Tauri build, Windows): project groups load again on *Home*, *Refresh* and in the
  dialogs; the same folder imported into a second project group (so far checked in Chromium with a stubbed Tauri)
- [ ] Decide and document what happens when the same CAD files are imported into two project groups: two independent
  items per file with different numbers, while CAD references are matched by file name (import tutorial, user guide)
- [ ] Check the desktop *Import BOM…* in the real app (native file dialog; so far checked in Chromium with a stubbed
  Tauri against a real server)
- [ ] Change requests: assign and link a change order from the desktop app (today: in the web UI)
- [ ] Screenshots and short GIFs of the desktop app for the README, and screenshots in the book (the web pages are
  captured in CI by `scripts/screenshots.sh`)

**Web UI and server**

- [ ] Browser check (no console errors, no overflow at 390 px) of `/requests`, `/requests/new` and `/requests/{id}`,
  and the desktop request tab
- [x] Chunked uploads from the *New item* form of the web UI: sessions without a revision (migration 0045) that only
  join the creation of an item; the attach and check-in forms use `/uploads` (feature 9)
- [ ] Custom attributes per file type (feature 5, items are done)
- [ ] Feature 6 leftovers: local substitutes per BOM line, a warning when a superseded item is added to a BOM in the
  desktop app
- [ ] Numbering schemes: a separate scheme per item type within a group, an instance-wide default other than
  `{TYPE}-{SEQ:6}` (feature 4)
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
- [ ] Readers for IGES, BREP, Parasolid, FBX, DAE, USD and E57 – recognised, not previewed (no pure-Rust readers
  yet; candidate: `e57` crate). JT 9 and 10 are read with their scene graph (`src/mesh/jt10.rs`); open there: JT 8, shattered
  JT files (parts in other `.jt` files), a real JT 9.5 assembly as a test file (the tests build synthetic ones) – no
  openly licensed test files yet
- [x] CATIA V5 products from their parts (`src/catprod.rs`, components by document UUID or part number). Open: hidden
  components (`VPGlobal`?), components without a document of their own, V5-6R20xx products and an openly licensed
  product as a test file (the tests build synthetic ones)

**Simulation management (FEA/CFD)**

- [ ] OpenFOAM case summary (solver, time steps) from `controlDict` – the 3D preview of VTK and CalculiX results is
  done (#43)
- [ ] Fluent, STAR-CCM+ and Simcenter 3D files, plan of the research page *Fluent, STAR-CCM+ and Simcenter 3D files*
  ([#153](https://github.com/rudalabs/TeamCraft/pull/153), `docs/src/research/solver-files.md`). Every byte-level
  rule is a hypothesis until it is checked with real files – the heads (1–4 KB) of STAR-CCM+ `.sim` files of two or
  three versions, NX 1980+ and older `.sim`/`.fem`/`.afm`, a Fluent `report-file-0.out` and `.trn`, a Simcenter 3D
  Nastran `.dat` and `.f06`, openly licensed or self-made, as fixtures
    - [x] Step 1 – detection (`src/cadformat.rs`, #156): `SPLMSSTR` as positive NX test for `.sim`/`.fem`/`.afm`,
      text `.fem` = OptiStruct deck, `.dat` with Nastran markers = Nastran deck, `.f04`/`.pch`, text `.op2` refused,
      Fluent `.msh`/`.msh.h5`/`.cas.gz`/`.dat.gz`/`.msh.gz` (HDF5 signature at 0 or 512, gzip magic), `.flprj`,
      `.cdat`, `.jou`, `.trn`, report files (`.out` with quoted title and header), STAR-CCM+ `.simh` and macros (`.java`
      naming STAR-CCM+), Gmsh `.msh`; fuzz target `cadformat`
    - [ ] Step 1, open question: label a `.sim` with neither marker as "uncertain" in the UI instead of the NX fallback
    - [ ] Files stored before #156 keep their old format and search text: a one-time re-detection of the
      changed extensions (`.sim` `.fem` `.afm` `.dat` `.msh` `.out` `.java` `.jou` `.trn` `.f04` `.pch`, the Fluent
      `.h5`/`.gz` names) and re-indexing at startup – not in a migration, `files.format` of v0.1.0 data must stay as it
      is
    - [x] Step 2 – text (#156): `trn`, `f06`, `f04`, `pch`, `jou`, `java` scannable and plain text (search,
      `read_file_text` – with the last 64 KiB of a larger file after a marker); templates with `pick = last` read the
      last 2 MB of a larger file first; new template method `table` (Fluent report files, migration 0040); fuzz target
      `resultscan`
    - [x] Step 3 (this PR, rules unverified against real files) – solver summary per file version: pure
      `src/solversummary.rs` over head and tail (64 KiB each):
      Fluent banner/convergence/last iteration, report file columns and last row, `.f06` version, `END OF JOB` and
      FATAL/WARNING window counts, Nastran `SOL`, STAR-CCM+ macro version and stopping criterion; a nullable JSONB
      column on file versions, one line on the file row and the simulation page, data (untrusted) for the MCP tools
    - [ ] Step 4 – references and basis proposals: `nxpart`'s `ExternalReferences` reader on `nx-simulation` files
      (first verify the streams exist in CAE parts); `cadrefs` for Fluent journals (`/file/read-case "x"`, …) and
      STAR-CCM+ macros (`resolvePath("x.sim")`); propose (never set) a *Geometry* basis along `.sim` → `.fem` →
      `_i.prt` → master `.prt`
    - [ ] Step 5 – desktop folder import: ignore `cleanup-fluent-*` and Nastran scratch files (`.MASTER`, `.DBALL`,
      `.SCRATCH`, …, verify the list), group the `.sim`/`.fem` chain, the Fluent case stem and the STAR-CCM+ `.sim`
      stem into one proposed simulation item, untick results above 64 MB and autosaves except the last step, hint
      for STAR-CCM+ *Clear Solution*; needs the retention rule (`files.retention = 'latest'`)
    - [ ] Step 6 – docs: tested snippets that write `teamcraft-results.csv` from a STAR-CCM+ Java macro, a Fluent
      journal/PyFluent script and a pyNastran post-script (in the simulation guide)
    - Not planned: parsing `.op2`, `.h5`, `.dat.h5`, `.cdat`, `.sim`, `.simh`; an HDF5 reader (revisit only in the
      isolated child); vendor tools on the server (later, desktop only: `starccm+ -info` with the user's licence)
- [x] Result values at check-in: `teamcraft-results.json|csv` exchange file and per-group extraction templates (bounded,
  audited, with source); never from big result files
- [x] Result values proposed by the assistant from PDF/text reports (`propose_result_values`, drafts with a quote,
  accepted or rejected by a CAD editor); PDF text layer in `read_file_text`
- [x] "Fill with AI…" on the Results card: a box with what to extract, limits and report files; drafts only, rights and
  the `assistant` switch respected (#177; open: desktop button, reusable requests per group, *Accept all*)
- [x] `read_file_text` of plain-text files shows the real text with its lines and decimal numbers (bounded; only
  geometry as text – ASCII STL, OBJ, KiCad – drops decimals for the search), so text and CSV reports work like PDFs
  (#141)
- [ ] Scanned PDF reports (OCR) – not planned; attach a PDF with a text layer or the numbers by hand
- [x] HTML reports of commercial FEA/CFD tools: own bounded HTML-to-text reader (`src/htmltext.rs`) in the isolated
  `text-extract` child, table rows as lines with ` | ` separators, scripts/styles/comments/hidden elements/embedded
  images skipped, nothing fetched; search, `read_file_text`, `propose_result_values` and the extraction templates use
  the extracted text ([#141](https://github.com/rudalabs/TeamCraft/issues/141))
- [ ] HTML reports: MHTML (`.mht`) and ZIP-packed multi-file reports (unpacking needs its own caps, see F13); text
  hidden
  by a style sheet (not inline) is not detected
- [x] Result values in the item search (failed, passed, above/below limit, by name) and a dashboard card for failing or
  out-of-date simulations; left: an *out of date* search filter, the `result` filter in the MCP `search_items` tool and
  in saved searches
- [x] New simulation from a part/assembly revision in the web UI and `POST /items` with `basis`: prefilled form, basis
  linked in the same transaction ([#178](https://github.com/rudalabs/TeamCraft/issues/178)); left: several bases at once
- [ ] Desktop: *New simulation from this revision…* on a part/assembly (creates the item with its basis), basis files
  checked out read-only into `basis/`, check-in of selected result time steps only
- [x] Retention for large result files (`files.retention = 'latest'`): per file, old versions removed after a grace
  period, never for released revisions (`teamcraft vault purge-versions`); left: a size-based default for new files
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
- [x] Use the instance counts of the component tree (`swReference`, without `swExcludeFromBOM`) for BOM quantities
  (#180: Create BOM from CAD references)
- [ ] Take configurations and Toolbox parts into account in the import and the reference scan
- [ ] Test the BOM import with real BOM exports

**Siemens NX**

- [ ] (research first) How other PLMs connect NX (native vs. Teamcenter NX Manager), licences (student/academic) and
  what is feasible for an open-source project
- [ ] Folder import for the master model concept: the drawing `*_dwg*.prt` belongs into the item of the model
- [ ] Test the BOM import with real NX BOM exports
- [ ] STEP export through the local NX (NX Open / journal), on Windows and Linux
- [ ] NX load options for the workspace only on request, in the workspace wizard; NX part of the tutorial

### 5. Feature demand (backlog)

What users ask of PLM/PDM systems (reviews of Teamcenter, Vistapoint, Dokuly, OdooPLM and others, Siemens community
enhancement requests), reduced to what TeamCraft does not have yet; what is present or done is left out. Entries
that need the CAD programs themselves (add-ins, generated drawings) are listed too – they need a Windows machine
with licensed CAD software to build and test. Like section 4, these are candidates, not commitments: a feature
needs an issue with a reason first (see *Direction*).

#### Core data, CAD and vault

- [ ] Modern, quieter UI: fewer clicks, modal dialogs and legacy patterns on the common paths
- [ ] Responsive pages on large data (1M files, 100k items, 10k-line BOMs – see the scale targets)
- [ ] Faster opening of large CAD files and assemblies
- [ ] Configurable lifecycle states and site-specific allowed transitions instead of one built-in state machine
- [ ] Electrical routing and harness data in the product definition
- [ ] Repair CAD references after a file is renamed or moved
- [ ] Central conversion queue for previews and derivatives with retries and a failure view
- [ ] 3D preview of more native formats; IFC geometry in the viewer
- [ ] PMI in the 3D preview, also PMI authored on assemblies
- [ ] Native CAD add-ins (see the research entry in section 4): check in/out, search, revise, BOM and references inside
  SolidWorks, Inventor, Solid Edge (occurrence semantics), Creo, CATIA, AutoCAD (xrefs) and Revit/BIM; NX comparable
  to a managed mode (Save As, revise, structure, numbering)
- [ ] ECAD/EPLAN integration: electrical design files and their BOM connectivity as first-class data, not
  attachments
- [ ] Derivatives generated on check-in or release – STEP, drawing PDF, DXF, Parasolid – through the CAD programs (the
  desktop app's Inventor/Solid Edge STEP export is a first step)
- [ ] Restore drills: back up and restore the data of each release in CI, not only the current one
- [ ] Mass properties, follow-ups of the first version (`services::mass_properties`, research page *Mass properties from
  exported meshes*): placements per BOM line instance (4×4 transforms, e.g. from the NX /
  SOLIDWORKS assembly placements the 3D preview reads) for parts exported in their own frame; a REST/MCP write
  for the mass settings and the material library; measured masses on released revisions (as-built units);
  the unit a 3MF or STEP file declares as the default unit; desktop view

#### Search, metadata and usability

- [ ] Search: proven fast at scale, Google-like for a known part, facets (type, state, group, owner, class), column
  filters in result grids, folder filters over all subfolders
- [ ] Personal search presets (filters, columns, defaults); configurable result columns with per-user layouts,
  classification attributes as columns
- [ ] Search across classification attributes; important metadata on the summary page; full long descriptions
- [ ] Richer custom attribute types (date, unit, reference, formula) with validation; full regex naming rules (negative
  lookahead)
- [ ] Hierarchical numbering (from parent or project), document numbering apart from parts, custom document types,
  generated cover sheets
- [ ] Administrator-configurable forms (hide unused fields), role-specific views, low-code validation rules
- [ ] Same search, commands and navigation in web UI and desktop app; one-click actions (revise, approve, open
  related objects)
- [ ] Keyboard-first operation, screen-reader checks, skip links and complete table headers; translated UI (i18n)
- [ ] Native mobile app
- [ ] Finer notification rules per event and channel, digests for all notifications, Microsoft Teams and Slack
  delivery

#### BOM, configuration, change and release

- [ ] BOM: round-trip Excel edit and re-import that keeps identities, CAD BOM synchronisation, graphical BOM compare
- [ ] Multi-BOM: eBOM → mBOM with traceability, service BOM, several views on the same data without copies
- [ ] Effectivity by date, serial number and lot; variants with option rules and a 150 % BOM; user-defined revision
  rules
- [ ] Assembly flags: "newer revision exists", missing drawings
- [ ] Occurrence properties: per-line metadata, recursive reports and their export, "do not show at higher level",
  BOMs that include hidden or reference-only content, workflow expansion filters
- [ ] Dissolve/promote a subassembly, pack identical lines, substitutes per occurrence, supersession propagated to all
  assemblies, reference designators in electrical searches; proofing of quantity/unit edge cases
- [ ] Sourcing: supplier price breaks, currency conversion, inventory, distributor search and metadata import (DigiKey,
  Nexar)
- [ ] Workflows: NPI process, parallel and conditional routing, dynamic participants, notify earlier reviewers,
  target vs. reference attachments at submission, editable task names, templates, a no-code designer, versioned
  definitions, delegation, due dates and escalation
- [ ] Changes linked to quality documents (control plans, PFMEA, work instructions, inspection plans) and to the
  requirements that drove them

#### Requirements, manufacturing, quality, projects

- [ ] Requirements: hierarchy, derived and superseded requirements, links to parts and documents, references in
  Markdown; verification records and physical test management beyond simulation
- [ ] Manufacturing: process planning, bill of process, work instructions, line balancing, scheduling links, work
  packages traceable to the BOM
- [ ] As-maintained/service history, serial-number and lot traceability
- [ ] Quality: QMS links, CAPA, nonconformance, deviations/waivers with expiry, control-plan/PFMEA/inspection-plan
  links
- [ ] Materials: material records and levels, coatings, RoHS/REACH declarations, sustainability and carbon footprint
- [ ] Projects: task assignment with due dates, milestones, portfolio and KPI dashboards, change cycle-time and reuse
  metrics, self-service and ad-hoc reports, audit-ready report packages
- [ ] Electronic signatures with meaning; re-authentication at sign-off
- [ ] Suppliers: portal, collaboration with restricted external access, RFQs, approved manufacturer list, end-of-life
  alerts for purchased parts
- [ ] Connectors: ERP/SAP, MES, Jira and other issue trackers

#### Administration, security, platform

- [ ] Managed/SaaS and hybrid deployment (on-premises vault, cloud collaboration)
- [ ] Permissions per object, per attribute and per lifecycle state; external (supplier) users
- [ ] SAML, SCIM provisioning (research: *Deprovisioning through the identity provider*, not now), built-in MFA
- [ ] Token scopes per project group (F2), project API keys
- [ ] OpenAPI description, a stable versioned API, an event bus connector (Kafka/AMQP)
- [ ] AI: per-user budgets (F16), prompt-injection protection for writes, AI-assisted impact analysis, semantic search
- [ ] Code-signed installers, Flathub and AUR packages
- [ ] Operations: complete metrics, outbox/queue observability, documented alerts, dependency updates, SBOM and build
  provenance, fuzzing of the remaining parsers, preview limits (F11), hash re-check on download (F8)
- [ ] Upgrades: proven upgrade chain per release, restore-based rollback guide, a plugin/extension layer
- [ ] CAD clone workflows: clone an assembly with selective reuse, Save As / revise per component over a whole
  structure, shown as an indented tree
- [ ] Interactive onboarding and contextual help
- [ ] Migration tooling from Teamcenter, Windchill, Aras and SOLIDWORKS PDM
- [ ] GA proofs: production readiness, migration and concurrency safety, scale validation, external security review,
  support and versioning policy

#### Product and community

- [ ] Staged adoption: start as a plain vault with versions and add changes, workflows and configuration later –
  simple defaults, enterprise depth switched on per installation or group; CAD-centric wording over database terms
- [ ] Documentation for every feature (user, admin, API, MCP), kept complete as features change
- [ ] Feature requests in the open with voting and a visible status (GitHub Discussions or issue reactions, linked
  from this roadmap), and a shorter path from request to release
- [ ] A commercial or priority support option for companies that need one (outside the code; a decision of the
  maintainers)

## Done

- [x] **From the feature demand list (#199):** storage quotas per user and for the vault (F9, `teamcraft user quota` /
  `vault quota`), malware scanning of uploads with ClamAV (`TEAMCRAFT_CLAMD_ADDR`), likely duplicates by name
  (`GET /items/duplicates`, hint after creating an item), bulk revise/obsolete (`POST /items/bulk`), section plane and
  point-to-point measuring with selectable decimals in the 3D viewer, encrypted backups (`teamcraft backup --encrypt`).
  In review (#200): standalone JT preview, graphical comparison of revisions/versions, 3D notes.
- [x] **Threat model** (`docs/src/dev/threat-model.md`): assets, attackers, trust boundaries and per threat the
  mitigation with code and test or *open*; vault, sessions, PATs, OIDC, uploads, file parsers, webhooks,
  MCP/assistant, desktop ↔ server, backup/restore. 19 open findings listed in section 2, *Security*; one fixed on
  the way: changing one's own password now signs out the user's other sessions
- [x] **BOM tree view** for large structures (web UI only): lines with children fold and unfold (progressive
  enhancement in `static/app.js`, all expanded without JavaScript), `?depth=n` shows the first n levels (a line at the
  cut counts what is hidden below it), a *Group* column links each line's project group (restricted placeholders show
  none), `?group=CODE` keeps the lines of a group with the path to them; roll-ups and totals always cover the whole
  structure, CSV/Excel exports stay complete. Pure `bom::view_rows` with unit tests, `tests/bom_tree_view.rs`.
  Not done: the desktop app's structure tab (own renderer in `desktop/ui/app.js`) has none of this yet
- [x] CI gate for the API reference: the `fmt and clippy` job also runs `cargo doc --no-deps` with
  `RUSTDOCFLAGS=-D warnings`; the 8 broken or private intra-doc links it found are fixed (doc comments only)
- [x] Guard tests for the architecture rules (`tests/architecture_guards.rs`, no database, walk directories so module
  splits do not break them): no `sqlx` in `src/web`, `src/api`, `src/mcp`; every public service function that
  writes (itself or through the services it calls) records an audit entry, exceptions listed with a reason
  (`NOT_AUDITED`: personal data, caches, sessions, staging, housekeeping); every template in `TEMPLATES` and rendered by
  `every_template_renders`; every `TEAMCRAFT_*` variable in `docs/src/admin/configuration.md` (found:
  `TEAMCRAFT_ENV_FILE`
  and the build-time `TEAMCRAFT_SYSTEM_CONFIG` were undocumented). A mutation check per rule made each guard fail once.
- [x] **Backup / restore** proofed (`tests/backup_proofing.rs`, `tests/backup_errors.rs`): a corrupt or missing vault
  object refuses the backup and nothing is left (two fixes: the backup now checks every object against its name and
  every content the database refers to, and a failed backup removes what it wrote; the manifest is renamed into
  place); a v0.1.0 backup with data restores into an empty database and migrates; a failing `restore --force`
  leaves database and vault untouched; credentials from a fully percent-encoded URL and from `_FILE`; large objects
  travel, the preview cache stays out and a target's cache is left alone; restore into a non-empty database needs
  `--force` (`tests/backup_restore.rs`)
- [x] **Atomic processes** proofed (`tests/atomic_processes.rs`): a trigger in the test database makes one step fail
  – with an error or by ending the connection (`pg_terminate_backend`) – and nothing is left (rows, counters, audit,
  notifications, webhook deliveries, check-out, review decision, visibility) for check-in, change order release,
  *Replace in assemblies*, Save As with files and BOM, chunked upload `finish` (repeatable afterwards) and the
  restricted-group visibility switch; two check-ins, two releases, two replacements and parallel numbering (also the
  same explicit number) give one winner and a 409. No atomicity bug found; a mutation check (check-in split into two
  transactions) makes the tests fail
- [x] **Migrations** proofed (`tests/migrations.rs`): numbering without duplicates and only the known gap (0015);
  every migration applied one at a time on an empty database, each step a version to upgrade from, ending at the
  schema of a fresh `migrate` (columns with type, nullability and default, constraints, indexes, triggers,
  functions); twice is a no-op; a changed checksum is refused unless in `db::EDITED_BEFORE_RELEASE`; a failed
  migration blocks the upgrade; a v0.1.0 database with data (`tests/fixtures/migrations/v0.1.0-seed.sql`: every
  v0.1.0 table, every enum value of v0.1.0) upgrades without loss – same row counts, same values in the old columns,
  every constraint validated and every CHECK true for every row, the API reads the old data; 0021 (not in v0.1.0)
  is applied after 0022
- [x] Desktop Get: resume a single large file half-way – the server answers `Range` / `If-Range` on file downloads
  (ETag = SHA-256, 206 / 416, `src/httprange.rs`); the desktop keeps the bytes received in
  `<name>.<sha>.teamcraft-download`, asks only for the rest and checks the SHA-256 of the whole file before the rename
  (`desktop/src/resume.rs`)
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
- [x] Item copy ("Save as" with files and BOM), supersede/replace, alternate parts and multi-level where-used in API,
  MCP
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
- [x] Change requests: e-mail digest of the new requests waiting for triage per project group – to its CAD editors
  every `TEAMCRAFT_REQUEST_DIGEST_HOURS` (default 24), muted per user like every kind (`request_digest`), only when
  something new came in, no second digest of a period after a restart (`request_digests`, migration 0035), checked
  again at sending time (restricted groups); tested in `tests/request_digests.rs` with a recording mailer
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
| –  | README feature list; release: Flatpak, Windows ARM64, portable Windows, no macOS server                                                                               | `bb649bf`, `a873920` |
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
- **Known limitations:** tested with hand-written AP214 structures only, not with real exports of the CAD programs (the
  NIST STEP files in `tests/fixtures/cad` are single parts); complex instances and the quantities of
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
   check-in), chunks staged in `<vault>/uploads/`, expired sessions removed hourly; used by the desktop app and the
   attach
   and check-in forms of the web UI (`/uploads` with the session cookie) for files above 32 MiB; only the *New
   item* form still sends one multipart request. **Backup/restore** with `pg_dump`/`pg_restore`:
   `tests/backup_restore.rs` (backup, restore into a
   fresh database and vault, identical row counts, file download and API token on the restored instance, `--force`,
   atomic failure) runs in CI, and the CLI smoke test runs `teamcraft backup`/`restore`. Found and fixed on the way: a
   percent-encoded password in `TEAMCRAFT_DATABASE_URL` reached `pg_dump` undecoded; a failed `restore --force` left a
   half-replaced database – `pg_restore` now runs with `--single-transaction`.

Rules for all of them: research open source first (licences compatible with MPL-2.0: MIT, Apache-2.0, BSD, ISC, Zlib,
MPL-2.0; LGPL only as a separately loaded component; no GPL/AGPL/SSPL, nothing proprietary), list used projects in the
README licence table, follow the definition of done in `CLAUDE.md`. The docs are live at
<https://rudalabs.github.io/TeamCraft/>.

## Pending verification (2026-11-07)

- The database tests of #122–#126 got their first real runs in CI: #137 and #138 fixed the failures (test bugs, plus
  one desktop bug – a folder import resumed after a lost creation answer re-uploaded files of an item that was already
  checked in). Tick the idempotency key and the upgrade tests above once `main` is green.
- #144 (as-built records, metrics endpoint, desktop notification settings and password change, desktop UI as ES
  modules – the squashed #132, #134, #135, #136, #143): its new database tests (`tests/as_built.rs`,
  `tests/api_password.rs`, `tests/notification_settings_api.rs`, the metrics tests) run for the first time in its CI.
- GA items started but not yet in a pull request (work kept in local worktrees): assistant budget (phase 4, F16),
  scoped tokens (F2), download hardening (F8, F10), retention and cleanup, parser hardening (F11,
  F13, F14), MCP hardening (F15, F17), backup hardening (F19: file modes, signed manifest), OIDC hardening (F6, F7
  session hours – back-channel logout is done),
  OpenAPI description; not
  started: MCP versioning, CLI and configuration freeze, accessibility leftovers, SBOM and attestations, upgrade
  proofs per release, scale targets, JSON logs.
