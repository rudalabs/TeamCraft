# Test coverage map

Where the tests are, where they are thin, and what to write next. The page
is the answer to *"which code can break without a test noticing?"* – it is
meant to be updated, not admired. Last measured: 2026-11-06, from the CI
job *coverage* on `main` at `b4cf014` (release 0.2.0; it contains #89–#95
and the proofing tests).

## Method

* **Lines of code** – `wc -l` of every file under `src/` (blank lines and
  comments included) and `desktop/src/`.
* **Unit tests** – the number of `#[test]` / `#[tokio::test]` functions in the
  module's own file (`src/simmesh/tests.rs` belongs to `simmesh.rs`).
* **Integration tests** – `tests/*.rs` talk to the real router (HTTP), so the
  routes and services they reach are mapped by what the test asserts, not by
  imports. The module table names only the test files that use a module
  directly (`teamcraft::module::…`); the area tables list the test files that
  drive the code through the API or the web UI.
* **Measured line coverage** – `cargo llvm-cov` in the CI job *coverage*: the
  complete test suite (unit, integration and database tests) against
  PostgreSQL 16 with the client tools. Its per-file summary is the source of
  the tables below. Locally, without a database, a quick check is:

  ```sh
  cargo install cargo-llvm-cov --locked && rustup component add llvm-tools-preview
  cargo llvm-cov --lib --summary-only            # unit tests only
  cargo llvm-cov --summary-only --no-fail-fast   # every test that runs without PostgreSQL
  cargo llvm-cov --html                          # report in target/llvm-cov/html
  ```

  Without PostgreSQL the database tests skip themselves, so `services/*`,
  `web/*` and `api/*` look uncovered locally although CI exercises them. Inline
  `#[cfg(test)]` code counts as covered lines, so the percentage of a module
  with a large test section is too high – the number of **missed lines** is
  the honest figure.
* **Ranking** – *risk × thinness*. Risk: untrusted input (file parsers, HTTP
  bodies, tokens), data integrity (multi-row mutations, vault, backup,
  migrations, imports, uploads), size of the code. Thinness: missed lines, and
  whether failure paths (refusal, conflict, rollback, damaged input) are
  tested at all and not only the happy path.

## Coverage in CI

The job *coverage* in `.github/workflows/ci.yml` runs `cargo llvm-cov` over
the complete test suite with PostgreSQL 16 and the client tools, prints the
per-file summary into the job summary and uploads the HTML report
(`coverage-html`, 14 days) as an artifact.

**The floor.** Since 2026-11-06 the job fails when the total line coverage
drops below the value in `.github/coverage-floor`. The last runs on `main`
were 91.48 % (6b7c95a), 91.52 % (e513b37), 91.53 % (5a39a85) and 91.65 %
(0fcfa26, b4cf014): the variation between runs is about a tenth of a point,
so the floor is the current value minus half a percentage point – **91.1 %**,
about 280 missed lines more than today. Raise it when the coverage has grown
for good; lowering it is a deliberate change of that file in the pull
request, with the reason in its description.

## Areas

Missed lines / lines and line coverage from CI (`b4cf014`).

| Area | Code | Tests | CI: missed / lines | HTTP / integration tests |
|---|---|---|---|---|
| File parsers (`mesh*`, `simmesh`, `stepbom`, `swpart`, `nxpart`, `thumbnail`, `cadrefs`, `bomtable`, `textindex`, `cadformat`, `shape`, `pdftext`, `resultscan`) | 10.1k counted lines | unit tests per module; property tests (#94) | 361 / 10,058 – 96.4 % | `cad_fixtures` (real NIST files), `uploads`, `web_ui`, `parser_robustness`, `parser_corpus`, `preview_limits` |
| `services/*` (business logic, the only writer) | 17.0k counted lines | unit tests in a third of the modules | 1,165 / 16,999 – 93.1 % | `api_flow`, `read_permissions`, `change_orders`, `change_requests`, `concurrency`, `uploads`, `webhooks`, `structure`, `numbering_and_watches`, `release_readiness`, `reviewers`, `audit_and_vault`, `interchange`, `working_folder`, `similar_parts`, `attributes`, `search_listing`, `scale`, `bom_performance`, `import_atomicity`, `import_proofing`, `service_refusals`, `migrations`, `atomic_processes`, `result_values`, `result_proposals`, `assistant_threads`, `agent_tools` |
| `web/*` (HTML handlers) | 5.6k counted lines | – | 891 / 5,613 – 84.1 % | `web_ui`, the feature tests that use the forms, `no_unchecked_in_handlers` |
| `api/*` (REST handlers) | 2.9k counted lines | – | 305 / 2,878 – 89.4 % | `api_flow` and the feature tests, `range_downloads` |
| `domain/*`, `config.rs`, `util.rs`, `error.rs` | 3.1k counted lines | unit and property tests | 87 / 3,068 – 97.2 % | all of them; `domain_and_vault` |
| `auth.rs`, `oidc.rs` | 0.6k counted lines | 10 unit | 191 / 649 – 70.6 % (`oidc.rs` 53.5 %) | `api_flow`, `read_permissions` (tokens, sessions), `oidc_flow` (SSO against a fake provider, session and PAT lifetime; the CI numbers predate it) |
| `vault.rs`, `backup.rs`, `db.rs` | 1.1k counted lines | unit tests in each | 101 / 1,148 – 91.2 % | `audit_and_vault`, `domain_and_vault`, `uploads`, `backup_restore`, `backup_errors`, `backup_proofing`, `migrations` |
| `webhooks.rs`, `delivery.rs`, `notify.rs` | 1.0k counted lines | unit tests in each | 129 / 971 – 86.7 % (`notify.rs` 26 %) | `webhooks`, `webhook_ssrf`, `api_flow` (e-mail outbox), `atomic_processes`; no test against a real SMTP/Mattermost server |
| `assistant.rs`, `llm/*`, `mcp/*` | 5.2k counted lines | unit tests in each | 306 / 5,236 – 94.2 % | `assistant`, `assistant_threads`, `mcp`, `agent_tools`, `read_permissions` |
| `main.rs`, `lib.rs` (CLI, router) | 0.8k counted lines | 3 unit | 533 / 756 – 29.5 % | the CLI runs only in the CLI smoke test of the *tests* job, which the coverage job does not measure |
| Desktop (`desktop/src`, own crate, not measured) | 9.1k lines | 72 unit | – | none (CI runs the desktop tests on Linux only) |

## Top 15: the thinnest high-risk spots

Ranked by risk × thinness on 2026-11-05; the last two columns are today's
state.

| # | Spot | Why it is risky | Tests that pin it | CI missed / lines | Open |
|---|---|---|---|---|---|
| 1 | `services/imports.rs` + `bomtable`/`stepbom` import path | Multi-row write from untrusted tables; one transaction must hold | `import_atomicity` (refusals, injected failure, retry, no duplicates), `import_proofing` (7), damage tests | 62 / 860 | – |
| 2 | `db.rs` + `migrations/` | A bad upgrade destroys every installation | `migrations` (8): numbering and the known gap, the release lists against their tags, every migration one at a time ending at the fresh schema (constraints, indexes, triggers, functions), twice a no-op, checksum rule, a failed migration blocks, a database of the published v0.1.0 and one of the first `v0.1.0` tag with data upgrade without loss, every table and enum value of the old schema in the seed (#91) | 16 / 105 | – |
| 3 | `backup.rs` | A restore is the last line of defence | `backup_errors` (7), `backup_proofing` (5, #93): corrupt or missing objects refuse the backup and nothing is left, a backup of the published v0.1.0 is restored and migrated, a failing `--force` leaves database and vault untouched, credentials, large objects, preview cache | 55 / 719 | – |
| 4 | `services/changes.rs` | Release of a change order flips many revisions at once | `atomic_processes` (#92): an injected failure (error or lost connection) at the second revision, the notifications or the webhook deliveries leaves nothing; two releases at once release once | 85 / 1,223 | – |
| 5 | `services/files.rs`, `services/uploads.rs` | Check-in, versions, chunked uploads: data loss on races | `atomic_processes`: failure between the versions and the lock release, upload `finish` repeatable; two check-ins at once give one winner; `concurrency`, `uploads`, `range_downloads` | 45 / 935, 30 / 457 | – |
| 6 | `services/items.rs`, `revisions.rs` | Creation, numbering, lifecycle | `service_refusals`, `domain_and_vault`, `atomic_processes` (12 parallel creations, the same number twice) | 54 / 724, 4 / 490 | – |
| 7 | `services/bom.rs` | Cycles and quantities corrupt every roll-up | 10 unit, `structure`, `service_refusals` | 14 / 1,004 | – |
| 8 | `services/checkouts.rs` | The lock is the integrity mechanism of the vault | `concurrency` | 10 / 240 | – |
| 9 | `services/users.rs`, `auth.rs`, `oidc.rs` | Sessions, tokens, SSO – untrusted input | unit tests, `api_flow`, `read_permissions`, `oidc_flow` (8, fake identity provider in the test process): a good login provisions the account with the mapped role; a missing, forged or expired `state`, a replayed callback, an error from the provider, a wrong `nonce`, an old `exp`, another `aud` or `iss`, no subject sign nobody in; a PKCE mismatch and a used code are a login error (fixed, below), a wrong client secret stays an internal error; ID token signatures are not checked by design (pinned); sessions and PATs end on expiry, sign-out / revocation (owner or admin, audited) and deactivation | 187 / 690, 11 / 262, 180 / 387 (before `oidc_flow`) | – (a plain-HTTP issuer, below) |
| 10 | `webhooks.rs` + `services/webhooks.rs` (SSRF guard, signing) | Server-side requests to configurable URLs | unit tests of the address rules (`internal_addresses`, `default_policy_blocks_internal_targets`, `allow_list_restricts_and_trusts_hosts`), HMAC vectors, `webhooks`, `atomic_processes`, `webhook_ssrf` (4) and `every_connection_checks_the_answer_it_uses`: a name that passes when saved and resolves to loopback, cloud metadata, an IPv4-mapped or unique-local address when used (DNS rebinding) is refused at connect time and every attempt resolves anew; mixed answers keep only public addresses; `localhost` via the system resolver and a stored IP literal are refused; a listed host is delivered (resolution injectable: `Dispatcher::with_lookup`) | 43 / 582, 25 / 679 | – |
| 11 | `mesh.rs` readers | Untrusted bytes, big allocations | `parser_robustness`, `parser_corpus`, fuzz targets | 108 / 1,535 | – |
| 12 | `simmesh.rs` (VTK/VTU/FRD) | Untrusted, hand-written parsing | 19 unit, damage tests | 81 / 1,465 | **an FRD sample in the damage tests** |
| 13 | `swpart.rs`, `nxpart.rs`, `thumbnail.rs` | Offsets, lengths, decompression bombs | unit tests, NIST files, damage tests | 8 / 657, 3 / 262, 15 / 543 | **a SOLIDWORKS container built by hand** |
| 14 | `stepbom.rs`, `bomtable.rs`, `cadrefs.rs`, `textindex.rs`, `cadformat.rs` | Untrusted text and spreadsheets; heuristic | unit tests, damage tests | 5 / 655, 10 / 747, 2 / 442, 2 / 200, 4 / 771 | – |
| 15 | `vault.rs` | Content-addressed storage: the bytes of every revision | `domain_and_vault`; since #93 every backup hashes every object and requires every referenced one | 30 / 324 | – |

Thin, outside the top 15: `notify.rs` (48 of 65 lines missed),
`services/insights.rs` (83 of 187), `services/requests.rs` (162 of 1,587),
`web/requests.rs`, `web/simulations.rs`, `web/attributes.rs`, `web/admin.rs`
and `web/webhooks.rs` (a quarter of their lines missed each),
`desktop/src/commands.rs`.

### Found while writing the tests

* **Fixed** (2026-11-05) – `mesh::read("gltf", …)` panicked (index out of
  bounds inside `gltf-json`) for a primitive whose `POSITION` points past the
  accessor list. The library call is now wrapped in `catch_unwind` and yields
  an error; regression test `gltf_accessor_index_out_of_range_is_an_error`.
* **Fixed** (#93) – `backup::backup` copied vault objects without checking
  their hash and did not require the objects the database refers to; a
  corrupt or missing object was found only at restore time. The backup now
  refuses both and removes what it wrote.
* **No bug** (#92) – every multi-row mutation covered by `atomic_processes`
  already ran in one transaction; a mutation check (a check-in split into two
  transactions) makes those tests fail.
* **Fixed** – a login code the identity provider refused (`invalid_grant`:
  a code already used or expired, or a PKCE verifier that does not match –
  an intercepted code) ended in a 500 page and an incident report. It is a
  failed login attempt, not a server fault: it is now the login page with a
  400 and "the identity provider refused the login code"; other token
  endpoint errors (`invalid_client`, a 5xx, no connection) stay internal
  errors because the administrator has to act. Regression tests
  `a_pkce_mismatch_or_a_used_code_is_refused_by_the_provider` and
  `a_misconfigured_client_stays_a_server_error`.
* **Observation, decision left open** – ID token signatures are not
  verified; that rests on the token coming straight from the token endpoint
  over TLS (OIDC Core 3.1.3.7 step 6). `TEAMCRAFT_OIDC_ISSUER_URL` also
  accepts `http://` (an internal identity provider without TLS); then
  nothing protects the token on its way. Refusing `http://` outside of
  `localhost`, or verifying the signature against the provider's JWKS when
  the issuer is plain HTTP, would close that – not changed here because it
  can break existing installations.
* **No bug** – the SSRF check at connect time: the resolver filters every
  answer for each new connection; a mutation that drops the filter fails the
  four resolver-based tests.

### What to write next

1. `services/insights.rs` (BOM health check, duplicates) and `notify.rs`.
2. FRD sample and a SOLIDWORKS container built by hand in the damage tests.
3. The CLI (`main.rs`, 511 of 643 lines missed): run the commands in-process in
   a test instead of only in the smoke test, so the coverage job sees them.
4. Done: atomicity under an injected failure (#92), a floor for the CI
   coverage job (`.github/coverage-floor`), OIDC failure paths and the SSRF
   check at connect time (`oidc_flow`, `webhook_ssrf`), `cargo fuzz` targets (see
   [Fuzzing the file readers](fuzzing.md)).

## Per module

From the CI job *coverage* on `b4cf014`. *Integration* names the test files
that use the module by name (`teamcraft::…`); the HTTP-driven coverage of
`services/*`, `web/*` and `api/*` is in the area table above.

| Module | LOC | Unit tests | CI missed / lines | Line coverage | Integration (direct use) |
|---|---|---|---|---|---|
| `api/bom_api.rs` | 261 | 0 | 7 of 191 | 96.3 % | – |
| `api/change_requests.rs` | 425 | 0 | 46 of 306 | 85.0 % | – |
| `api/changes_api.rs` | 228 | 0 | 18 of 179 | 89.9 % | – |
| `api/concurrency_api.rs` | 64 | 0 | 1 of 36 | 97.2 % | – |
| `api/files_api.rs` | 139 | 0 | 3 of 114 | 97.4 % | – |
| `api/interchange_api.rs` | 156 | 0 | 2 of 105 | 98.1 % | – |
| `api/items_api.rs` | 468 | 0 | 3 of 342 | 99.1 % | – |
| `api/mod.rs` | 699 | 0 | 157 of 530 | 70.4 % | – |
| `api/notifications_api.rs` | 142 | 0 | 1 of 109 | 99.1 % | – |
| `api/personal.rs` | 222 | 0 | 17 of 143 | 88.1 % | – |
| `api/projects_api.rs` | 135 | 0 | 1 of 91 | 98.9 % | – |
| `api/references_api.rs` | 149 | 0 | 0 of 103 | 100.0 % | – |
| `api/simulations.rs` | 406 | 0 | 28 of 301 | 90.7 % | – |
| `api/uploads_api.rs` | 256 | 0 | 17 of 189 | 91.0 % | – |
| `api/webhooks.rs` | 201 | 0 | 4 of 139 | 97.1 % | – |
| `assets.rs` | 413 | 8 | 2 of 280 | 99.3 % | web_ui |
| `assistant.rs` | 2094 | 12 | 71 of 1608 | 95.6 % | agent_tools, read_permissions, result_proposals |
| `auth.rs` | 410 | 6 | 11 of 262 | 95.8 % | api_flow, bom_performance |
| `backup.rs` | 961 | 9 | 55 of 719 | 92.4 % | backup_errors, backup_proofing, backup_restore |
| `bomtable.rs` | 978 | 19 | 10 of 747 | 98.7 % | api_flow, import_proofing, parser_robustness |
| `cadformat.rs` | 1372 | 19 | 4 of 771 | 99.5 % | cad_fixtures, parser_robustness |
| `cadrefs.rs` | 629 | 18 | 2 of 442 | 99.5 % | cad_fixtures, parser_robustness |
| `config.rs` | 1527 | 16 | 47 of 1108 | 95.8 % | api_flow, backup_errors, backup_proofing +3 |
| `db.rs` | 185 | 1 | 16 of 105 | 84.8 % | api_flow, backup_proofing, migrations |
| `delivery.rs` | 477 | 4 | 38 of 324 | 88.3 % | api_flow, read_permissions |
| `domain/attributes.rs` | 289 | 0 | 8 of 184 | 95.7 % | – |
| `domain/changes.rs` | 336 | 0 | 4 of 147 | 97.3 % | – |
| `domain/concurrency.rs` | 173 | 0 | 6 of 97 | 93.8 % | – |
| `domain/items.rs` | 324 | 0 | 0 of 209 | 100.0 % | – |
| `domain/lifecycle.rs` | 155 | 0 | 0 of 66 | 100.0 % | – |
| `domain/mod.rs` | 1021 | 38 | 7 of 663 | 98.9 % | api_flow, assistant, assistant_threads +28 |
| `domain/notifications.rs` | 156 | 0 | 1 of 64 | 98.4 % | – |
| `domain/references.rs` | 88 | 0 | 0 of 32 | 100.0 % | – |
| `domain/roles.rs` | 96 | 0 | 5 of 39 | 87.2 % | – |
| `domain/simulations.rs` | 130 | 0 | 0 of 74 | 100.0 % | – |
| `domain/skins.rs` | 177 | 0 | 1 of 57 | 98.2 % | – |
| `domain/validation.rs` | 220 | 0 | 1 of 167 | 99.4 % | – |
| `error.rs` | 133 | 0 | 6 of 57 | 89.5 % | change_orders, preview_limits, read_permissions |
| `fuzz_harness.rs` | 157 | 2 | 10 of 63 | 84.1 % | parser_corpus |
| `httprange.rs` | 176 | 5 | 0 of 91 | 100.0 % | – |
| `lib.rs` | 233 | 0 | 22 of 113 | 80.5 % | – |
| `llm/anthropic.rs` | 389 | 5 | 4 of 289 | 98.6 % | – |
| `llm/http.rs` | 204 | 3 | 4 of 153 | 97.4 % | – |
| `llm/mod.rs` | 302 | 0 | 7 of 93 | 92.5 % | – |
| `llm/openai.rs` | 550 | 8 | 11 of 425 | 97.4 % | – |
| `main.rs` | 1032 | 3 | 511 of 643 | 20.5 % | – |
| `mcp/mod.rs` | 291 | 2 | 7 of 167 | 95.8 % | read_permissions |
| `mcp/tools.rs` | 3395 | 14 | 202 of 2501 | 91.9 % | read_permissions |
| `mesh.rs` | 1933 | 12 | 108 of 1535 | 93.0 % | cad_fixtures, parser_robustness, web_ui |
| `mesh/p21.rs` | 672 | 5 | 26 of 459 | 94.3 % | – |
| `mesh/step.rs` | 529 | 4 | 17 of 351 | 95.2 % | – |
| `mesh/tessellated.rs` | 438 | 6 | 9 of 359 | 97.5 % | – |
| `models.rs` | 868 | 10 | 0 of 313 | 100.0 % | – |
| `notify.rs` | 116 | 2 | 48 of 65 | 26.2 % | – |
| `nxpart.rs` | 397 | 7 | 3 of 262 | 98.9 % | parser_robustness |
| `oidc.rs` | 492 | 4 | 180 of 387 | 53.5 % | – |
| `pagecontext.rs` | 400 | 5 | 4 of 272 | 98.5 % | – |
| `pdftext.rs` | 321 | 5 | 36 of 241 | 85.1 % | – |
| `preview.rs` | 941 | 5 | 111 of 651 | 82.9 % | cad_fixtures, preview_limits |
| `resultscan.rs` | 863 | 9 | 15 of 600 | 97.5 % | – |
| `services/access.rs` | 159 | 1 | 0 of 61 | 100.0 % | – |
| `services/assistant.rs` | 470 | 0 | 53 of 359 | 85.2 % | – |
| `services/assistant_threads.rs` | 264 | 2 | 1 of 175 | 99.4 % | assistant_threads |
| `services/attributes.rs` | 307 | 0 | 23 of 251 | 90.8 % | – |
| `services/audit.rs` | 322 | 0 | 2 of 242 | 99.2 % | read_permissions |
| `services/bom.rs` | 1435 | 10 | 14 of 1004 | 98.6 % | – |
| `services/bom_compare.rs` | 601 | 4 | 8 of 385 | 97.9 % | – |
| `services/changes.rs` | 1393 | 0 | 85 of 1223 | 93.0 % | api_flow, change_orders, read_permissions |
| `services/checkouts.rs` | 376 | 2 | 10 of 240 | 95.8 % | concurrency |
| `services/desktop.rs` | 234 | 2 | 1 of 146 | 99.3 % | web_ui |
| `services/explorer.rs` | 164 | 0 | 0 of 80 | 100.0 % | – |
| `services/files.rs` | 1219 | 3 | 45 of 935 | 95.2 % | api_flow, backup_proofing |
| `services/impacts.rs` | 1173 | 3 | 24 of 786 | 96.9 % | – |
| `services/imports.rs` | 1197 | 6 | 62 of 860 | 92.8 % | – |
| `services/insights.rs` | 276 | 0 | 83 of 187 | 55.6 % | – |
| `services/interchange.rs` | 708 | 0 | 20 of 496 | 96.0 % | – |
| `services/items.rs` | 1023 | 0 | 54 of 724 | 92.5 % | api_flow, bom_performance, read_permissions |
| `services/mod.rs` | 89 | 0 | 0 of 27 | 100.0 % | api_flow, assistant_threads, backup_proofing +7 |
| `services/notifications.rs` | 838 | 2 | 3 of 544 | 99.4 % | read_permissions |
| `services/package.rs` | 181 | 0 | 4 of 121 | 96.7 % | – |
| `services/personal.rs` | 169 | 1 | 3 of 122 | 97.5 % | – |
| `services/projects.rs` | 688 | 0 | 53 of 558 | 90.5 % | api_flow, read_permissions |
| `services/readiness.rs` | 627 | 6 | 4 of 423 | 99.1 % | – |
| `services/references.rs` | 506 | 0 | 24 of 352 | 93.2 % | – |
| `services/requests.rs` | 1942 | 5 | 162 of 1587 | 89.8 % | read_permissions |
| `services/result_proposals.rs` | 424 | 1 | 16 of 303 | 94.7 % | – |
| `services/result_templates.rs` | 234 | 0 | 15 of 196 | 92.3 % | – |
| `services/reviewers.rs` | 446 | 2 | 12 of 188 | 93.6 % | – |
| `services/revisions.rs` | 594 | 0 | 4 of 490 | 99.2 % | – |
| `services/save_as.rs` | 173 | 0 | 3 of 130 | 97.7 % | – |
| `services/similar.rs` | 677 | 1 | 43 of 434 | 90.1 % | – |
| `services/simulations.rs` | 1769 | 5 | 85 of 1326 | 93.6 % | – |
| `services/stats.rs` | 44 | 0 | 0 of 26 | 100.0 % | – |
| `services/uploads.rs` | 644 | 5 | 30 of 457 | 93.4 % | uploads |
| `services/users.rs` | 836 | 2 | 187 of 690 | 72.9 % | api_flow, backup_proofing, migrations +2 |
| `services/webhooks.rs` | 915 | 4 | 25 of 679 | 96.3 % | – |
| `services/workset.rs` | 304 | 0 | 7 of 192 | 96.4 % | – |
| `shape.rs` | 1031 | 9 | 20 of 771 | 97.4 % | cad_fixtures |
| `simmesh.rs` | 1968 | 19 | 81 of 1465 | 94.5 % | – |
| `stepbom.rs` | 865 | 13 | 5 of 655 | 99.2 % | import_proofing, parser_robustness |
| `swpart.rs` | 978 | 14 | 8 of 657 | 98.8 % | parser_robustness |
| `templates.rs` | 1615 | 3 | 6 of 1263 | 99.5 % | – |
| `textindex.rs` | 302 | 9 | 2 of 200 | 99.0 % | parser_robustness |
| `thumbnail.rs` | 723 | 12 | 15 of 543 | 97.2 % | cad_fixtures, parser_robustness |
| `util.rs` | 156 | 5 | 1 of 104 | 99.0 % | api_flow, web_ui |
| `vault.rs` | 434 | 4 | 30 of 324 | 90.7 % | – |
| `web/account.rs` | 192 | 0 | 24 of 145 | 83.4 % | – |
| `web/admin.rs` | 585 | 0 | 110 of 428 | 74.3 % | – |
| `web/assistant.rs` | 193 | 0 | 23 of 132 | 82.6 % | – |
| `web/attributes.rs` | 170 | 0 | 34 of 136 | 75.0 % | – |
| `web/auth_pages.rs` | 285 | 0 | 55 of 197 | 72.1 % | – |
| `web/bom.rs` | 286 | 0 | 51 of 214 | 76.2 % | – |
| `web/changes.rs` | 484 | 0 | 93 of 400 | 76.8 % | – |
| `web/dashboard.rs` | 55 | 0 | 3 of 39 | 92.3 % | – |
| `web/desktop.rs` | 85 | 0 | 1 of 40 | 97.5 % | – |
| `web/explorer.rs` | 118 | 0 | 10 of 75 | 86.7 % | – |
| `web/formats.rs` | 22 | 0 | 0 of 16 | 100.0 % | – |
| `web/imports.rs` | 242 | 0 | 13 of 187 | 93.0 % | – |
| `web/interchange.rs` | 241 | 0 | 20 of 168 | 88.1 % | – |
| `web/items.rs` | 404 | 0 | 28 of 289 | 90.3 % | – |
| `web/mod.rs` | 889 | 2 | 18 of 639 | 97.2 % | – |
| `web/notifications.rs` | 176 | 0 | 5 of 148 | 96.6 % | – |
| `web/personal.rs` | 324 | 0 | 39 of 202 | 80.7 % | – |
| `web/references.rs` | 143 | 0 | 15 of 96 | 84.4 % | – |
| `web/requests.rs` | 672 | 0 | 131 of 527 | 75.1 % | – |
| `web/revisions.rs` | 1194 | 0 | 105 of 1014 | 89.6 % | – |
| `web/simulations.rs` | 330 | 0 | 65 of 256 | 74.6 % | – |
| `web/uploads.rs` | 90 | 0 | 0 of 49 | 100.0 % | – |
| `web/webhooks.rs` | 271 | 0 | 48 of 216 | 77.8 % | – |
| `webhooks.rs` | 930 | 8 | 43 of 582 | 92.6 % | read_permissions |
