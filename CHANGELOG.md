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

Nothing merged since 0.2.0 yet.

## [0.2.0] – 2026-11-06

Pre-release. Migrations `0021` to `0035` run on the first start.

### Added

- Restricted project groups: read access per group, placeholders for hidden
  items in BOMs and references (#52, review fixes #55). See
  [Read permissions → Upgrading](docs/src/admin/permissions.md#upgrading).
- Similar part search by 3D shape signature (#42).
- Simulation results in the 3D viewer: VTK, VTU/VTP and CalculiX FRD (#43).
- Supersede, replace and alternate parts; multi-level where-used in the web
  UI (#59).
- Custom attributes and classification of items (#60); the BOM import fills
  them from further columns (#97).
- Web UI: chunked, resumable uploads of large files (#62).
- Web UI: light and dark title-block design (#51, #53); BOM page with folded
  branches, levels, an owner group column and a group filter (#102).
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
- Desktop: *Import BOM…* from CSV, Excel or a STEP assembly (#63); cancel the
  check-in upload and Pack and Go half-way (#61); *Get* resumes a large file
  half-way (HTTP Range, #90); project groups reload without a restart and an
  import log per group (#58); state-aware toolbar, *More* menu and *At a
  glance* overview (#41); a model picker for the agent's own models (ACP,
  #103).
- Release builds: Flatpak bundles (x86_64, aarch64), Windows ARM64 installers
  and portable Windows zips (#66).
- Preview result picture gallery (#82).
- Documentation: architecture decision records and service invariants (#99),
  the road to 1.0 in `docs/OPEN_WORK.md` (#68, #73), research pages (#50,
  #69, #70).
- Development: test coverage per pull request with a floor (#80, #101),
  rustdoc without warnings in CI (#100), CodeQL, `cargo deny`, property tests
  for the domain rules, fuzz targets with a weekly workflow (#94),
  architecture guard tests (#96), tests for migrations (#91), backup and
  restore (#93), atomic processes (#92), OIDC and token lifetimes (#105) and
  the folder import with the NIST CAD files (#107); a scale check (#47).

### Changed

- **Breaking:** server archives are named by platform:
  `teamcraft-<version>-linux-x86_64.tar.gz`, `-linux-aarch64.tar.gz` and
  `-windows-x64.zip` instead of the Rust target triple (#74). Download scripts
  that build the old names must be changed.
- **Breaking:** `teamcraft backup` stops on a vault object that is corrupt or
  missing instead of writing an incomplete backup; find it with
  `teamcraft vault check --verify-hashes` (#93).
- Previews: result files, meshes and STEP files above their size caps are no
  longer opened (`TEAMCRAFT_PREVIEW_MAX_RESULT_MB`, `…_MESH_MB`, `…_STEP_MB`),
  and the conversion child runs with a memory limit
  (`TEAMCRAFT_PREVIEW_MAX_MEMORY_MB`) (#82).
- Faster on large vaults: set-based search, trigram indexes for the substring
  search (migration `0023`), level-wise where-used and impact analysis; BOM and
  comparison pages are bounded (#47).
- The desktop folder import completes items whose files did not arrive in an
  earlier, interrupted run (#107).
- Shared UI files for the web UI and the desktop app in `shared/` (#71).
- Internal: `domain` and `api` split into modules, no behaviour change (#94).

### Removed

- **Breaking:** no macOS server binary any more; the desktop app is still
  built for macOS (#66). Run the server in Docker or on Linux or Windows.

### Fixed

- OIDC: a login code the provider refuses (already used, expired, PKCE
  mismatch) is a login error, not an HTTP 500 (#105).
- A 3DS vertex or face list chunk of the minimum length at the end of the file
  made the reader panic (#106).
- An NX folder import created no BOM lines (#107).
- BOM page: a group filter with a lower-case group code (`?group=rocket`)
  gave "not found" (#109).
- Review fixes for the STEP import, the all-levels BOM compare and numbering
  schemes (#56); MCP change requests and simulations (#37).
- Backup and restore: two fixes found by the end-to-end test with the real
  client tools (#57).

### Security

- `quick-xml` 0.37 → 0.42 (RUSTSEC-2026-0194, RUSTSEC-2026-0195: denial of
  service on XML read from uploaded files – 3MF, AMF, VTK, SOLIDWORKS
  manifest) (#94).

## [0.1.0] – 2026-11-05

First pre-release (`6f68f9a`). No changelog was kept before 0.2.0; see the
[release notes on GitHub](https://github.com/rudalabs/TeamCraft/releases/tag/v0.1.0).

[Unreleased]: https://github.com/rudalabs/TeamCraft/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/rudalabs/TeamCraft/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rudalabs/TeamCraft/releases/tag/v0.1.0
