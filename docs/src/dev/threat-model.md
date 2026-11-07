# Threat model

What TeamCraft protects, from whom, where the trust boundaries are, and – per
threat – what the code does about it and which test pins it. Every claim
names the code (`file`, function) and the test; a threat without both is
marked **open** and has an entry in `docs/OPEN_WORK.md` (section 2,
*Security*). Written against TeamCraft 0.2.0 (`main` after #108); keep it in
step with the code – a change to a mitigation changes its row.

Status words: **mitigated** (code and test), **partial** (code, but a gap
named in the row), **open** (no mitigation), **accepted** (a deliberate
decision, documented). STRIDE letters: **S**poofing, **T**ampering,
**R**epudiation, **I**nformation disclosure, **D**enial of service,
**E**levation of privilege.

## System and trust boundaries

```text
   browser ──HTTPS (reverse proxy)──┐                 ┌── identity provider (OIDC)
   desktop app ──HTTPS + PAT────────┤                 ├── LLM provider (assistant)
   MCP client / `teamcraft mcp` ────┤   teamcraft     ├── SMTP, Mattermost
   REST client (PAT) ───────────────┘   server  ──────┼── webhook receivers
                                          │   │       │
                     PostgreSQL ──────────┘   │       └── (outbound, server-initiated)
                     vault directory ─────────┤
                     preview / text-extract child process (untrusted file bytes)
```

* **B1 – network ↔ server.** Everything from browsers, the desktop app, MCP
  and REST clients is untrusted until authenticated. TLS ends at the reverse
  proxy (the server speaks HTTP); deployments without TLS are out of scope
  (`SECURITY.md`).
* **B2 – authenticated user ↔ data of other project groups.** A signed-in
  user is trusted with their role, not with restricted groups they are not
  a member of.
* **B3 – file bytes ↔ server.** Uploaded files are hostile input to the
  parsers (CAD, mesh, point cloud, BOM tables, PDF, simulation results).
* **B4 – text in the database ↔ the LLM.** File text, descriptions and
  comments reach the model provider and can carry instructions.
* **B5 – server ↔ outbound targets.** The server makes requests to URLs
  partly chosen by administrators (webhooks, LLM, IdP).
* **B6 – server ↔ host.** Administrators, the file system, the database and
  backups are trusted; attacks that need them are out of scope
  (`SECURITY.md`).

## Assets

| Asset | Where | Why it matters |
|---|---|---|
| Files of every revision (CAD, drawings) | vault `objects/ab/cd/<sha256>` (`src/vault.rs`) | the product data itself; often confidential |
| Items, BOMs, changes, requests, audit log | PostgreSQL | engineering record; release decisions |
| Restricted project groups | `projects.visibility`, `Actor.hidden_projects` | confidentiality between teams or customers |
| Credentials | `users.password_hash` (Argon2), `sessions.token_hash`, `api_tokens.token_hash` (SHA-256), `webhooks.secret` (plain, needed for HMAC), OIDC client secret and LLM key (configuration, `_FILE`) | takeover of accounts or integrations |
| Availability | server process, disk, database | engineering work stops |
| Desktop working folders | the user's disk | files written by the desktop app |

## Attackers

| | Attacker | Can |
|---|---|---|
| A1 | anonymous network attacker | reach the login page, the API and `/auth/*` |
| A2 | signed-in user (viewer or editor) | everything their role allows; tries to read restricted groups or act beyond the role |
| A3 | author of a hostile file | gets a file uploaded (a supplier's STEP, a colleague's spreadsheet) |
| A4 | author of hostile text | writes a description, comment or file that the assistant or an MCP client later reads |
| A5 | network attacker between server and IdP / LLM / webhook target | reads or changes outbound traffic |
| A6 | administrator, host or database access | out of scope (B6) |

## Authentication: passwords, sessions, tokens

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Password database stolen | I | Argon2id with a random salt (`src/auth.rs` `hash_password`); minimum 10 characters (`src/domain/validation.rs` `validate_password`) | `src/auth.rs` `password_hash_round_trip` | mitigated |
| User enumeration at sign-in | I | one message for unknown user, wrong password, OIDC-only and inactive accounts; a dummy Argon2 verify for unknown users (`src/services/users.rs` `authenticate_local`, `src/auth.rs` `DUMMY_HASH`) | `tests/web_ui.rs` `wrong_password_is_rejected` (message); no timing test | mitigated |
| Password guessing (A1) | S | failed sign-ins counted per username and per client address for 15 minutes (`TEAMCRAFT_SIGNIN_MAX_FAILURES`, 10 per name, 50 per address); then 429 with `Retry-After` without checking the password; the address is the peer or, behind an internal proxy, the proxy's `X-Forwarded-For` entry; in memory per process (`src/auth.rs` `SignInThrottle`, `client_address`; `src/web/auth_pages.rs` `login_submit`). The own password change (web form `/account/password` and `POST /api/v1/me/password` of the desktop app) shares the per-username counter: a wrong current password counts, a full counter refuses even the right one with 429 (`src/services/users.rs` `change_own_password`); so whoever holds a stolen session or token can lock the user's web sign-in for 15 minutes, as anyone can at the sign-in form. Not throttled: the desktop token exchange (its one-time code is deleted on the first attempt) | `src/auth.rs` `sign_in_throttle_counts_per_name_and_address`, `client_addresses_behind_a_proxy`; `tests/web_security.rs` `failed_sign_ins_are_throttled_per_name_and_address`; `tests/api_password.rs` `wrong_current_passwords_are_throttled` | mitigated (F1 fixed) |
| Session token guessed or stolen from the database | S, I | 256-bit random tokens, only SHA-256 hashes stored (`src/auth.rs` `generate_token`, `hash_token`; `src/services/users.rs` `create_session`) | `src/auth.rs` `tokens_are_random_and_hashed` | mitigated |
| Session cookie read by script or sent cross-site | I | `HttpOnly; SameSite=Lax`, `Secure` when the public URL is https (`src/auth.rs` `cookie_attributes`, `src/config.rs` `TEAMCRAFT_COOKIE_SECURE`) | – (attributes not asserted by a test) | partial: no test |
| Session outlives its purpose | S | absolute lifetime `TEAMCRAFT_SESSION_TTL_HOURS` (72 h); the lookup requires `expires_at > now()` and an active user (`actor_for_session`); sign-out deletes the row; deactivation and an admin password reset delete all sessions | `tests/oidc_flow.rs` `sessions_expire_end_with_sign_out_and_with_deactivation` | mitigated |
| A stolen session survives the victim's password change | S | fixed in #110: `users::change_own_password` ends every other session of the user (the one that made the change stays) and records how many in the audit entry | `tests/oidc_flow.rs` `changing_the_own_password_ends_the_other_sessions` | mitigated |
| Role or membership changed, old rights stay | E | role and memberships are read from the database on every request (`actor_for_session`, `actor_for_api_token`) | `tests/read_permissions.rs` `visibility_is_managed_and_audited` | mitigated |
| Personal access token (PAT) stolen | S, I | 256-bit random, SHA-256 hash stored, shown once, optional expiry, revocation by the owner or an admin (audited `token.revoke`), dead with a deactivated account (`src/services/users.rs` `create_token`, `revoke_token`, `actor_for_api_token`) | `tests/oidc_flow.rs` `personal_access_tokens_expire_are_revoked_and_die_with_the_account`; `tests/api_flow.rs` `api_requires_a_valid_token` | mitigated |
| A PAT has more rights than its client needs | E | none: a PAT carries all rights of its user (REST and MCP, writes included); the desktop app's token lives 90 days. The MCP reference advises one token per client with an expiry (`docs/src/reference/mcp.md`) | – | **open** (F2) |
| Sign-in events cannot be traced | R | token creation and revocation, password changes, role changes, OIDC provisioning and desktop authorisation are audited, and so are sign-in (`user.sign_in` with the method: password, SSO, setup), failed password sign-in (`user.sign_in_failed`; an unknown typed username is not kept), a wrong current password at the own password change (`user.password_failed`) and sign-out (`user.sign_out`) – never a password or token (`src/services/users.rs` `create_session`, `delete_session`, `record_failed_sign_in`) | `tests/web_security.rs` `sign_in_and_sign_out_are_audited`; `tests/architecture_guards.rs` | mitigated (F3 fixed) |

## Web: CSRF, XSS, headers, redirects

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Cross-site form post (CSRF) | E | `csrf_guard` on every non-GET web request compares `Origin` (or `Referer`) with the host and the public URL; `Origin: null` is refused; `SameSite=Lax` as second layer (`src/web/mod.rs` `csrf_guard`, `same_origin`). A request without both headers is refused (#111); `X-Forwarded-Host` is trusted as sent by the proxy (documented in the install guide) | `src/web/mod.rs` `origin_checks`; `tests/web_security.rs` `a_form_post_without_origin_and_referer_is_refused`, `cookie_requests_without_origin_or_referer_are_refused` (nothing written, sign-out refused); `tests/web_ui.rs` `cross_site_posts_are_blocked`; `tests/assistant_threads.rs`, `tests/uploads.rs` (cross-site cases) | mitigated (F4) |
| Cookies used against the API | E | the API accepts only `Authorization: Bearer` (`src/auth.rs` `ApiUser`); no cookie code in `src/api` | – (no test sends a session cookie to `/api/v1`) | partial: no test |
| Script injection (XSS) | E, I | MiniJinja templates; CSP `default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'`, no inline JS or styles (`src/lib.rs`) | `tests/web_security.rs` `security_headers_and_hsts_for_https` (the full policy, no `unsafe-inline`/`unsafe-eval`) | mitigated |
| Clickjacking | T | `frame-ancestors 'none'` and `X-Frame-Options: DENY` (`src/lib.rs`) | `tests/web_security.rs` `security_headers_and_hsts_for_https` | mitigated |
| Downgrade to HTTP | I | `Strict-Transport-Security: max-age=31536000` on every response when the public URL is https (follows `TEAMCRAFT_COOKIE_SECURE`, `src/lib.rs`; #111); the install guide covers the proxy | `tests/web_security.rs` `hsts_follows_the_public_url`, `security_headers_and_hsts_for_https` (pages, API, `/healthz`) | mitigated (F5) |
| Open redirect after sign-in | S | `util::safe_redirect_target` accepts only local paths (no `//`, no `\`); applied to `next` and to the OIDC return target | `src/util.rs` `redirect_targets`; `tests/oidc_flow.rs` (return to `next`) | mitigated |
| First-start setup taken over | E | `/setup` only with local sign-in, a loopback bind address and no user yet, under an advisory lock (`src/web/auth_pages.rs` `setup_open`) | `tests/web_ui.rs` `first_start_creates_the_administrator` | mitigated |

## Authorization and restricted project groups

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| A non-member reads a restricted group (A2) | I | reads go through the SQL guards of `src/services/access.rs` (`item_visible`, `project_visible`, `change_visible`, `request_visible`, …) with the actor's `hidden_projects`; a hidden object answers like a missing one (`ensure_visible`); design in [ADR 0001](adr/0001-access-model.md) | `tests/read_permissions.rs` `outsiders_see_nothing_of_a_restricted_group` and `every_route_has_a_placeholder_rule` (every web, REST and MCP route), `texts_naming_restricted_objects_are_redacted`, the review-round tests; `tests/no_unchecked_in_handlers.rs` | mitigated; residual oracles listed in `SECURITY.md` |
| An administrator who is not on the named list reads a restricted group (#151) | I | need-to-know policy per group: `with_reason` (time-limited, audited elevation with a written reason, managers notified) or `none`; the rule fills `Actor::hidden_projects` for administrators (`access::hidden_of_user`), so all guards apply | `tests/need_to_know.rs` | step 1 mitigated for groups with a policy; claims, interface switches, read log and token scopes (F2) open |
| A viewer or editor writes beyond the role | E | `Actor::require`, `can_edit_project`, `revisions::ensure_editable` in the services (`src/auth.rs`, `src/services/*`) | `tests/api_flow.rs`, `tests/service_refusals.rs`, `tests/read_permissions.rs` | partial: by convention; no route sweep for writes as there is for reads |
| A mutation leaves no trace | R | `audit::record` in the transaction of every service mutation; exceptions listed with a reason | `tests/architecture_guards.rs` `every_service_mutation_writes_an_audit_entry` (#96) | mitigated |
| One request makes the server resolve an unbounded list (A2) | D | list lengths are checked before the list is resolved: the affected items of a change request at most `requests::MAX_ITEMS` (100) in the REST API, the MCP tool and again in `requests::create` (repeats included – the per-row limit in `insert_item` counted distinct rows, so a 2 MB body of one repeated item ran ~150 000 lookups and inserts in one transaction until the CodeQL triage fixed it); simulation results at most `MAX_RESULTS` (2000) per import; JSON bodies are capped by axum's default limit (2 MB) | `tests/change_requests.rs` `too_many_items_are_rejected_before_any_is_resolved` | mitigated for the lists found by CodeQL; no general audit of list fields |

## Single sign-on (OIDC)

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Login CSRF / injected code | S | `state` (stored, 15 minutes, used once: `DELETE … RETURNING`), `nonce` in the ID token, PKCE S256 (`src/oidc.rs` `start_login`, `finish_login`) | `tests/oidc_flow.rs` `broken_state_never_signs_in`, `a_good_login_signs_in_and_maps_the_groups` (replay), `broken_id_tokens_never_sign_in` (nonce) | mitigated |
| Intercepted or replayed authorization code | S | PKCE verifier and single-use codes at the provider; a refused code (`invalid_grant`) is a login error (fixed in #105) | `tests/oidc_flow.rs` `a_pkce_mismatch_or_a_used_code_is_refused_by_the_provider` | mitigated |
| Forged ID token | S | `iss`, `aud`, `exp` (120 s skew), `nonce` and `sub` are checked (`validate_id_token_claims`); the issuer of the discovery document must match the configured one. The **signature is not verified**: the token comes straight from the token endpoint over the back channel, which OpenID Connect Core 1.0, 3.1.3.7 step 6 allows **only when that channel is TLS**. `TEAMCRAFT_OIDC_ISSUER_URL` also accepts `http://` (`src/config.rs`), and then nothing protects the token against A5 | `tests/oidc_flow.rs` `broken_id_tokens_never_sign_in`, `id_token_signatures_are_not_checked_by_design` | **open** for plain-HTTP issuers (F6) |
| Takeover of a local account by an SSO name | S, E | accounts are linked by the provider's `sub` only; a username that is taken gets a suffix (`src/services/users.rs` `upsert_oidc_user`) | `tests/oidc_flow.rs` `a_good_login_signs_in_and_maps_the_groups` | mitigated |
| Everyone at the IdP gets in | E | without `TEAMCRAFT_OIDC_VIEWER_GROUP` every account of the provider signs in as viewer; with it, only members (`src/oidc.rs` `role_for_groups`; documented in `docs/src/admin/users.md`) | `src/oidc/tests.rs` `group_mapping` | accepted (documented) |
| Removed at the IdP, still signed in | E | the role follows the groups at the next SSO sign-in; a running session stays until it expires (72 h by default); no back-channel logout | – | **open** (F7) |
| Provider misconfiguration hidden | – | `invalid_client`, 5xx and network errors stay internal errors (reported) | `tests/oidc_flow.rs` `a_misconfigured_client_stays_a_server_error` | mitigated |

## Vault, uploads and downloads

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Path traversal into or out of the object store | T, I | object paths only from 64 hex characters (`src/vault.rs` `object_path`); staging names are UUIDs; file names are sanitised (`util::sanitize_filename`: no `/`, `\`, control characters or leading dots) | `tests/domain_and_vault.rs` `hashes_cannot_escape_the_object_tree`; `src/vault.rs` `rejects_bad_hashes`; `src/util.rs` `filenames`; `tests/backup_errors.rs` `symlinks_in_the_vault_are_not_followed` | mitigated |
| Content does not match its name | T | the hash is computed while storing, chunked uploads are re-hashed against the announced size and SHA-256 (`src/vault.rs` `Upload::write`, `hash_staged`; `src/services/uploads.rs` `verify_content`) | `tests/uploads.rs`; `src/services/uploads.rs` `announced_hashes_are_validated` | mitigated |
| A blob changed on disk is served | T | downloads stream the blob without re-hashing; `teamcraft vault check --verify-hashes` and every backup find it (`Vault::verify`, `backup::copy_checked_objects`) | `tests/domain_and_vault.rs` `a_blob_that_changed_on_disk_fails_verification`; `tests/backup_proofing.rs` `a_corrupt_or_missing_vault_object_refuses_the_backup` | partial (needs host access, B6; noted as F8) |
| Another user's upload session used | S, T | sessions are loaded only for their owner (others get 404), expire after 24 h, appends are serialised and need the confirmed offset (409 otherwise) (`src/services/uploads.rs` `load`, `append`) | `tests/uploads.rs` `sessions_are_private_and_checked`, `concurrent_chunks_at_one_offset_are_serialised`, `expired_sessions_are_removed` | mitigated |
| Disk or memory exhausted by uploads | D | `TEAMCRAFT_MAX_UPLOAD_MB` (512) as body limit and in `Upload::write`; chunks up to `TEAMCRAFT_UPLOAD_CHUNK_MB` (16); at most 16 open upload sessions per user; expired sessions and stale staging files are removed | `tests/domain_and_vault.rs` `an_upload_over_the_limit_is_refused_and_leaves_no_blob`; `src/services/uploads.rs` `chunks_stay_within_the_announced_size` | partial: no quota per user or in total (F9) |
| Download of a restricted file | I | every download path goes through `files::get` / `get_version` with `access::ensure_visible`; Range and If-Range in `src/httprange.rs` | `tests/read_permissions.rs` `outsiders_see_nothing_of_a_restricted_group`; `tests/range_downloads.rs` `downloads_resume_with_range_and_if_range` | mitigated |
| An uploaded HTML or SVG file runs script | E | downloads are always `Content-Disposition: attachment` (`util::content_disposition`), `X-Content-Type-Options: nosniff` and the CSP on every response; request attachments additionally `default-src 'none'; sandbox`. The stored Content-Type is the one the client sent (`files::clean_content_type` checks only its form) | – `tests/web_security.rs` pins attachment, nosniff and CSP | partial (F10) |

## File parsers

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| A crafted mesh, STEP, PDF or result file crashes or hangs the server (A3) | D | 3D conversion (`teamcraft preview-mesh`) and the text of PDF and HTML reports (`teamcraft text-extract --format pdf` or `html`) run in a child process with a wall-time limit, `kill_on_drop`, size caps per format and two at a time (`src/preview.rs` `run_worker`, `src/pdftext.rs`; HTML: the first 16 MB are read, at most 256 open elements are tracked, 200 000 characters out, `src/htmltext.rs`); a failure is cached, not retried | `src/preview.rs` `failures_of_the_conversion_process_are_explained`; `tests/parser_robustness.rs`; `tests/preview_limits.rs` `the_html_text_process_is_bounded` | mitigated on Linux and macOS |
| Memory exhaustion in the child | D | `RLIMIT_AS` 1 GiB on Unix (`src/preview.rs` `limit_memory`); **no memory limit on Windows**; a 3MF package entry may claim up to 2 GiB (`src/mesh.rs` `zip_entry`) | – | partial (F11) |
| A compromised child reads the server's secrets | I | the children start with a cleared environment (`env_clear` in `preview.rs`, `pdftext.rs`); no sandbox beyond the process boundary | – | mitigated (F12) |
| Parsers that run inside the server process | D | CAD reference scan (`src/cadrefs.rs`, with `nxpart`, `swpart`), plain text index, thumbnails, BOM import (`stepbom`, `bomtable`) run on blocking threads with their own caps: scan 32 MiB, text 4 MiB / 200 000 characters plus a 64 KiB tail of a larger plain-text file (at most a quarter of the characters, only after a text head, no NUL), result-template scan 2 MB head plus 2 MB tail of every scannable upload above 2 MB (up to 4 MB held per file during a check-in, also for files no template reads), thumbnail stream 8 MiB, tables 20 MiB, STEP 200 MiB, Deflate sections bounded and CRC-checked (`src/swpart.rs`). A panic ends only the blocking task. **XLSX and ODS are unpacked by calamine in the server process without a cap on the unpacked size** (`src/bomtable.rs`), so a ZIP bomb under 20 MiB can exhaust memory | `tests/parser_robustness.rs` `damaged_bom_tables_never_panic`, `damaged_containers_never_panic`, `thumbnail_extraction_survives_damaged_files`, `claimed_sizes_beyond_the_file_are_refused` | partial (F13) |
| Panics on hostile bytes | D | `catch_unwind` around third-party readers (truck STEP, glTF, pdf_oxide); twelve `cargo fuzz` targets run weekly (`fuzz/`, `.github/workflows/fuzz.yml`, [Fuzzing](fuzzing.md)); the seed and fixture corpus runs in every CI run. `#![deny(clippy::unwrap_used, expect_used, indexing_slicing, panic, unreachable)]` on the readers of untrusted files (`mesh`, `simmesh`, `swpart`, `nxpart`, `stepbom`, `bomtable`, `cadrefs`, `textindex`, `htmltext`, `thumbnail`, `resultscan`, `cadformat`; #106, #108), which fixed a 3DS and a reference-scan panic. Not fuzzed: `simmesh`, `pdftext`; `pdftext` is not under the lint | `tests/parser_corpus.rs` `the_corpus_never_panics_a_reader`; `tests/parser_robustness.rs`; `src/mesh.rs` `three_ds_list_chunks_without_their_count`; `src/cadrefs.rs` `a_non_ascii_letter_in_the_extension_does_not_panic` | partial (F14) |
| XML entity expansion / external entities | D, I | quick-xml (3MF, AMF, VTU/VTP, SOLIDWORKS component tree) neither loads DTDs nor expands custom entities | `tests/parser_robustness.rs` `component_trees_in_broken_xml_never_panic` (no DOCTYPE payload) | mitigated; no explicit test |
| Names inside CAD files used as paths | T | references are normalised names stored in `file_references` and matched in SQL only, never opened (`src/cadrefs.rs`, `src/services/references.rs`) | `src/cadrefs.rs` `scan_file_reads_stored_files` | mitigated |

## Webhooks (server-side requests)

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| A webhook reaches internal services or cloud metadata (SSRF) | I, E | only administrators manage webhooks (`src/services/webhooks.rs`, `Actor::require(Role::Admin)`); `http`/`https` without credentials; IP literals checked when saved and again when sent (`TargetPolicy::check_url`); every resolved address checked when the connection is made (`GuardedResolver`), loopback, private, link-local, shared, unique-local, NAT64/6to4 forms refused unless the host is listed; no redirects, no proxy (`src/webhooks.rs`) | `src/webhooks.rs` `internal_addresses`, `default_policy_blocks_internal_targets`, `allow_list_restricts_and_trusts_hosts`, `every_connection_checks_the_answer_it_uses`; `tests/webhook_ssrf.rs` (DNS rebinding, `localhost`, stored IP literal) | mitigated |
| Forged webhook deliveries | S | `X-TeamCraft-Signature` HMAC-SHA256 over the body with a per-webhook secret, shown once, rotatable (audited) | `src/webhooks.rs` `hmac_sha256_matches_rfc_4231`, `signature_header`; `tests/webhooks.rs` `release_events_are_signed_and_delivered` | mitigated |
| A webhook leaks restricted data | I | a project webhook sees only its group; deliveries of groups restricted after queueing are withheld | `tests/webhooks.rs` `project_webhooks_see_only_their_revisions` | mitigated |
| Webhook secrets read from the database | I | stored in plain text (the HMAC needs them); part of every backup | – | accepted (B6) |
| Tokens in webhook URLs end up in logs | I | delivery errors are recorded without the URL (`describe`, `without_url`) | `tests/webhooks.rs` `failed_deliveries_back_off_and_give_up` | mitigated |

## MCP server and assistant

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Unauthenticated tool calls | S | `POST /api/v1/mcp` and `/api/v1/assistant` take bearer PATs only; the web sidebar uses the session with the CSRF guard (`src/mcp/mod.rs`, `src/api/mod.rs`) | `tests/mcp.rs` `mcp_protocol_and_tools` (401 without token) | mitigated |
| A tool reads beyond the user's rights | I | every tool calls the same services with the caller's actor; the assistant reloads the actor before each tool call; page context ids are hints, filtered by the access guards (`src/assistant.rs`, `src/services/assistant.rs`) | `tests/read_permissions.rs` (MCP routes), `tests/agent_tools.rs` `read_file_text_of_a_restricted_group_is_hidden_from_outsiders` | mitigated |
| Prompt injection through file text or descriptions makes the model act (A4) | T, E | five write tools only, all drafts or comments (no submit, approve, release); the assistant offers them only when the user ticks *Allow drafting* and may edit; `read_file_text` marks its output as untrusted; the HTML report reader skips comments, scripts and hidden elements (`hidden`, `aria-hidden`, inline `display:none`/`visibility:hidden` – not text styled invisible by a style sheet or colour) (`src/htmltext.rs`); the system prompt says tool results are data; after reading something an outsider could not read, the four text-writing tools refuse (taint) (`src/mcp/tools.rs`, `src/assistant.rs`) | `src/mcp/tools.rs` `tool_definitions_are_complete`; `tests/agent_tools.rs`; `tests/assistant_threads.rs` `a_follow_up_reads_with_the_rights_of_now` | partial: the untrusted marker is advice to the model; no taint for external MCP clients (F15) |
| Exfiltration through the rendered answer | I | markdown-lite escapes everything, has no images and no free links (only `/i/<item>` links); the CSP blocks foreign images (`src/assistant.rs` `render_markdown`) | `src/assistant.rs` `markdown_is_rendered_safely` | mitigated |
| Data sent to the model provider | I | off unless an administrator configures a key; the dialogs name provider and host; only question and answer text are stored, threads expire (30 days) and are owner-only; audit entries carry counts, not content (`src/services/assistant_threads.rs`, `src/services/assistant.rs`) | `tests/assistant_threads.rs` | accepted (documented in `docs/src/guide/assistant.md`, `SECURITY.md`) |
| The LLM client is turned against internal hosts | I | the base URL is administrator configuration; no redirects, key in a sensitive header, errors without URL (`src/llm/http.rs`) | `src/llm/http.rs` `redirects_are_not_followed_and_the_key_stays_on_the_host`, `network_errors_do_not_carry_the_url` | accepted (B6) |
| Cost or capacity exhausted by one user | D | 12 steps, 16 000 output tokens per call, 800 000 tokens per run, 240 s, four runs at a time; no per-user quota or rate limit | – | **open** (F16; OPEN_WORK *per-user daily budget*) |
| `teamcraft mcp` bridge leaks its token | I | token from a file or `TEAMCRAFT_TOKEN`, never printed; it follows redirects with the default policy (reqwest drops `Authorization` when host or port change) and accepts `http://` servers (`src/main.rs`) | `src/main.rs` `mcp_endpoint_accepts_base_or_full_url` | partial (F17) |

## Desktop app ↔ server

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Sign-in code intercepted | S | browser flow to a loopback callback with PKCE S256 and state; one-time code (hashed, 5 minutes, deleted on any exchange attempt); the result is a PAT (90 days), audited `desktop.authorize` (`src/services/desktop.rs`, `desktop/src/signin.rs`) | `tests/web_ui.rs` `desktop_sign_in_through_the_browser`; `src/services/desktop.rs` `pkce_s256`, `requests_are_validated`; `desktop/src/signin.rs` `pkce_matches_rfc_7636`, `loopback_round_trip` | mitigated |
| Token read from the user's disk | I | the OS keychain (Windows Credential Manager, macOS Keychain, Secret Service), written and read back before it counts; a token file of older versions moves into it at the next start. Without a keychain: the file `token` in the app's config folder (mode 0600 on Unix, the profile folder's permissions on Windows) and a warning at the start and in *Settings* (`desktop/src/settings.rs`) | `desktop/src/settings.rs` `settings_and_keychain_token_round_trip`, `without_a_keychain_the_token_falls_back_to_a_private_file` (mode 0600), `a_token_file_moves_into_the_keychain`, `a_keychain_that_loses_the_token_counts_as_none`, `sign_out_fails_when_the_keychain_keeps_the_token` | mitigated (F18) |
| Token sent in clear text | I | rustls with certificate validation; API paths are checked so the token goes only to the configured server (`desktop/src/api.rs` `check_path`); an `http://` server URL to another computer is accepted but warned about in the connect dialog, on the sign-in step and in *Settings* (`desktop/src/settings.rs` `insecure_http`) | `desktop/src/settings.rs` `insecure_http_spares_https_and_loopback` | mitigated (F18): warned, the user decides |
| Update notice abused | T / I | the daily release check (opt-out) asks only `api.github.com` for the latest release, without redirects and with a 10 s timeout; a version from the answer is used only if it is a plain `x.y.z`; the notice opens a fixed release page – the app downloads and installs nothing. GitHub sees the address and app version once a day (`desktop/src/update.rs`) | `desktop/src/update.rs` `newer_versions`, `tags`, `at_most_daily` | mitigated |
| Server writes outside the working folder | T | every server-supplied name goes through `safe_name` / `normalize_folder` (no `..`, no separators, reserved Windows names) before `join_rel` (`desktop/src/layout.rs` `local_rel`) | `desktop/src/layout.rs` `names`, `folders_like_the_server`, `layout_mapping` | mitigated |
| Old desktop app against a newer server | T | none: no version handshake | – | **open** (OPEN_WORK *desktop ↔ server compatibility*) |

## Backup and restore

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Backup taken by an unauthorised user | I | command line only (`teamcraft backup`, `restore`); no web or API route (`src/main.rs`) | – | mitigated (B6) |
| A corrupt or incomplete backup | T | every object is re-hashed while copied, every object the database refers to must exist; a failed backup removes what it wrote; the manifest is renamed into place last (`src/backup.rs`) | `tests/backup_proofing.rs`, `tests/backup_errors.rs` | mitigated |
| Restore of a tampered or broken backup | T | manifest, dump checksum, object count and every object hash are checked before anything is written; a non-empty database needs `--force`; `pg_restore` runs in one transaction (`src/backup.rs` `verify_backup`, `ensure_database_empty`) | `tests/backup_errors.rs`, `tests/backup_proofing.rs` `a_failing_forced_restore_leaves_database_and_vault_untouched`, `tests/backup_restore.rs` | mitigated; the manifest is not signed (whoever can write the backup can rewrite both) |
| The backup leaks credentials | I | the dump holds password, session and token hashes and the webhook secrets; files are written with the process umask (no restrictive mode), not encrypted; the database password goes only through `PGPASSWORD` (`src/backup.rs`) | `src/backup.rs` `commands_keep_the_password_out_of_the_arguments` | partial (F19) |

## Branding uploads

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| An administrator's SVG logo runs script in the origin of other users | E, I | the type is decided from the content; SVG with scripts, event handlers, animations, `foreignObject`, entity declarations or `href`/`src`/`url(…)` references other than `#id` and `data:image/…` is refused; logos are shown only through `<img>` (no script runs there) and served with `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; sandbox` and `nosniff`; 512 KiB and 4096 px caps, bitmaps whose size cannot be read (PNG, JPEG, WebP) are refused (`src/services/branding.rs` `validate`, `src/web/branding.rs` `image`) | `src/services/branding.rs` unit tests (`refuses_active_svg_content`, `refuses_external_references_in_an_inkscape_svg`, `huge_webp_and_bitmaps_without_a_readable_size_are_refused`, …); `tests/branding.rs` | mitigated; the SVG check is a deny list, the sandbox and `<img>` are the real barrier |
| A non-administrator replaces the logos | E | `Actor::require(Admin)` in `branding::set` and `reset`; every change audited (`branding.set`, `branding.reset`) | `tests/branding.rs` `bad_logos_and_other_users_are_refused` | mitigated |
| The public logo route leaks data | I | `GET /branding/{wide,square}` serves only the logo images (no sign-in, so the sign-in page can show them) | `tests/branding.rs` | accepted: logos are public by design |

## Metrics endpoint

| Threat | STRIDE | Mitigation | Tests | Status |
|---|---|---|---|---|
| Metrics read by outsiders | I | off unless `TEAMCRAFT_METRICS_TOKEN` (at least 16 characters) is set; bearer token compared without early exit; no label comes from a request (no paths, no users) (`src/metrics.rs`, `src/lib.rs` `metrics_endpoint`) | `src/metrics.rs` `token_comparison`; `tests/metrics.rs` | mitigated; a reverse proxy should not expose `/metrics` publicly |

## Open findings

Each open one has an entry in `docs/OPEN_WORK.md` (section 2, *Security*);
fixed ones are struck through and name the change.

| | Finding | Area |
|---|---|---|
| F1 | ~~No rate limit or lockout for sign-in~~ – fixed (`fix/security-web-auth`): per username and client address, 429; the own password change (web form and REST) counts on the same per-username counter; the desktop token exchange is not throttled (see the table above) | authentication |
| F2 | PATs have no scopes (read-only, MCP-only); the desktop token has full rights for 90 days | tokens |
| F3 | ~~Sign-in, failed sign-in and sign-out are not in the audit log~~ – fixed (`fix/security-web-auth`) | audit |
| F4 | ~~CSRF: a request without `Origin` and `Referer` passes~~ fixed (#111); `X-Forwarded-Host` is trusted as sent (documented) | web |
| F5 | ~~No HSTS~~ fixed (#111): sent when the public URL is https | web |
| F6 | ID token signatures are not verified, yet the issuer may be plain `http://` | OIDC |
| F7 | A user removed at the IdP keeps a running session until it expires | OIDC |
| F8 | Downloads do not re-check the blob hash (tampering on disk shows only in `vault check` or a backup) | vault |
| F9 | No disk quota per user or in total | uploads |
| F10 | Served Content-Type is the client's (the download headers are now pinned by a test) | downloads |
| F11 | No memory limit for the preview and text children on Windows; 3MF entries up to 2 GiB | parsers |
| F12 | ~~The parser children inherit the server's environment~~ fixed: cleared | parsers |
| F13 | XLSX/ODS are unpacked in the server process without a cap on the unpacked size | parsers |
| F14 | `simmesh`, `pdftext`, `resultscan` not fuzzed (the no-panic lints and the 3DS fix are in `main`, #106/#108) | parsers |
| F15 | Prompt injection: the untrusted marker is advisory; no taint for external MCP clients | assistant / MCP |
| F16 | No per-user quota or rate limit for the assistant and MCP | assistant / MCP |
| F17 | The `teamcraft mcp` bridge follows redirects and accepts `http://` | MCP |
| F18 | ~~Desktop token in a plain file; `http://` server URLs accepted~~ – fixed (`ga/desktop-security`): OS keychain with a warned file fallback, a warning for `http://` servers | desktop |
| F19 | Backups are written with the process umask, unencrypted, with an unsigned manifest | backup |

Fixed while writing this model: a password change kept the user's other
sessions alive (`users::change_own_password`, test
`changing_the_own_password_ends_the_other_sessions`).
