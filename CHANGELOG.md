# Changelog

All notable changes to TeamCraft are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). TeamCraft is
pre-1.0: while the version is `0.x`, any release may contain breaking changes
(see [CONTRIBUTING.md](CONTRIBUTING.md#releases-and-versions)); they are marked
**Breaking** below. How to upgrade:
[docs/src/admin/upgrade.md](docs/src/admin/upgrade.md).

Entries name the pull request. A change that a user or an administrator
notices gets an entry under *Unreleased* in the same pull request.

## [Unreleased]

### Changed

- **Breaking:** the web UI refuses a form post without `Origin` and `Referer`
  headers (threat model F4). Browsers send them; a script that posts to the
  web UI must send one (the REST API with tokens is not affected). Behind a
  reverse proxy, let the proxy overwrite `X-Forwarded-Host` (#111).

### Security

- Changing one's own password signs out the user's other sessions; personal
  access tokens stay (#110).
- `Strict-Transport-Security` when `TEAMCRAFT_PUBLIC_URL` is https, and the
  file parser child processes start with an empty environment (no database
  URL, no keys) (#111).

### Fixed

- The reference scan cut a file name inside a character when its extension
  held a letter whose lower case is ASCII (the Kelvin sign `K` → `k`) and
  panicked; every reader of untrusted files now refuses `unwrap`, `expect` and
  indexing at compile time (#108).

## [0.2.0] – 2026-11-06

Pre-release. Migrations `0032` to `0035` run on the first start.

### Added

- Ask TeamCraft sidebar in the web UI, with the page as context and one
  conversation per user (#81, #85).
- Assistant: provider-neutral LLM layer (#75), an OpenAI-compatible backend,
  provider configuration and `teamcraft assistant check` (#76); agent tools
  `read_file_text` and `impact_summary`, impact and simulation quick actions
  (#79).
- Result values from an exchange file and per-group extraction templates
  (#88); the agent proposes result values from PDF and text reports as drafts
  (#89).
- Change requests: an e-mail digest of the requests waiting for triage
  (`TEAMCRAFT_REQUEST_DIGEST_HOURS`, #98).
- BOM import: classification and custom attributes from further columns
  (#97).
- BOM page: folded branches, levels, an owner group column and a group filter
  (#102).
- Desktop: *Get* resumes a large file half-way (HTTP Range, #90); a model
  picker for the agent's own models (ACP, #103).
- Preview result picture gallery (#82).
- Documentation: architecture decision records and service invariants (#99).
- Development: test coverage per pull request with a floor (#80, #101),
  rustdoc without warnings in CI (#100), every test binary runs even when one
  fails (#86), CodeQL, `cargo deny`, property tests for the domain rules, fuzz
  targets with a weekly workflow (#94), architecture guard tests (#96), tests
  for migrations (#91), backup and restore (#93), atomic processes (#92), OIDC
  and token lifetimes (#105) and the folder import with the NIST CAD files
  (#107).

### Changed

- **Breaking:** server archives are named by platform:
  `teamcraft-<version>-linux-x86_64.tar.gz`, `-linux-aarch64.tar.gz` and
  `-windows-x64.zip` instead of the Rust target triple (#74). Download scripts
  that build the old names must be changed.
- **Breaking:** `teamcraft backup` stops on a vault object that is corrupt or
  missing instead of writing an incomplete backup; find it with
  `teamcraft vault check --verify-hashes` (#93).
- **Breaking:** previews: result files, meshes and STEP files above their size
  caps are no longer opened (`TEAMCRAFT_PREVIEW_MAX_RESULT_MB`, `…_MESH_MB`,
  `…_STEP_MB`), and the conversion child runs with a memory limit
  (`TEAMCRAFT_PREVIEW_MAX_MEMORY_MB`) (#82).
- The desktop folder import completes items whose files did not arrive in an
  earlier, interrupted run (#107).
- Internal: `domain` and `api` split into modules, no behaviour change (#94).

### Fixed

- OIDC: a login code the provider refuses (already used, expired, PKCE
  mismatch) is a login error, not an HTTP 500 (#105).
- A 3DS vertex or face list chunk of the minimum length at the end of the file
  made the reader panic (#106).
- An NX folder import created no BOM lines (#107).
- BOM page: a group filter with a lower-case group code (`?group=rocket`)
  gave "not found" (#109).

### Security

- `quick-xml` 0.37 → 0.42 (RUSTSEC-2026-0194, RUSTSEC-2026-0195: denial of
  service on XML read from uploaded files – 3MF, AMF, VTK, SOLIDWORKS
  manifest) (#94).

## [0.1.0] – 2026-11-05

First pre-release, tag `v0.1.0` at `439724a` (it pointed to `6f68f9a` first
and was moved the same day). No changelog was kept before 0.2.0; see the
[release notes on GitHub](https://github.com/rudalabs/TeamCraft/releases/tag/v0.1.0).

[Unreleased]: https://github.com/rudalabs/TeamCraft/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/rudalabs/TeamCraft/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rudalabs/TeamCraft/releases/tag/v0.1.0
