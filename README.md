<p align="center">
  <img src="media/logos/TeamCraft_Icon.svg" width="96" height="96" alt="TeamCraft" />
</p>

<h1 align="center">TeamCraft</h1>

<p align="center">
  <strong>Open-source product lifecycle management, built in pure Rust.</strong><br />
  Check your CAD data out and in, keep every version, release with reviews,
  follow changes through assemblies and see STEP, mesh and point-cloud models
  in 3D – in the browser, in the desktop app or through the REST API.
</p>

<div align="center">

[![Linux][badge-linux]][linux]
[![macOS][badge-macos]][macos]
[![Windows][badge-windows]][windows]
[![Documentation][badge-docs]][docs]

</div>

<p align="center">
  <a href="https://github.com/rudalabs/TeamCraft/actions/workflows/ci.yml"><img src="https://github.com/rudalabs/TeamCraft/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI" /></a>
  <a href="https://github.com/rudalabs/TeamCraft/actions/workflows/docs.yml"><img src="https://github.com/rudalabs/TeamCraft/actions/workflows/docs.yml/badge.svg?branch=main" alt="Docs" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/licence-MPL--2.0-blue" alt="Licence: MPL-2.0" /></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white" alt="Built with Rust" />
  <img src="https://img.shields.io/badge/database-PostgreSQL-336791?logo=postgresql&logoColor=white" alt="PostgreSQL" />
</p>

<p align="center">
  <a href="https://rudalabs.github.io/TeamCraft/">Documentation</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="https://rudalabs.github.io/TeamCraft/guide/tour.html">Feature tour</a> ·
  <a href="https://rudalabs.github.io/TeamCraft/reference/formats.html">File formats</a> ·
  <a href="https://rudalabs.github.io/TeamCraft/reference/api.html">REST API</a> ·
  <a href="https://rudalabs.github.io/TeamCraft/api/teamcraft/index.html">Rust API</a>
</p>

## What works

TeamCraft runs end to end today – create a project group, add items, check
files out and in, build the BOM, release through a reviewed change order,
see what has to be checked after a release – and is covered by 93 unit tests
and 26 end-to-end tests against a real PostgreSQL. It has not had a stable
release yet; the database schema is migrated automatically between versions.

- **Check-out / check-in.** Exclusive locks per revision; check-in stores the
  new file versions and releases the lock in one transaction. Cancel
  check-out and *Cancel upload* take effect immediately.
- **Vault explorer.** Project groups as folders, every item with its lock
  owner, files and one-click check-out / check-in in a detail pane.
- **Versions and rollback.** Every upload of a file name is a new version;
  download or restore any version – history is never rewritten.
  Content-addressed storage (SHA-256) stores identical files once.
- **Lifecycle.** WIP → In Review → Released → Obsolete with revision letters
  after ASME Y14.35; released data never changes, the next revision does.
- **Change orders.** Collect revisions, assign reviewers, four-eyes approval,
  all revisions released atomically, impact analysis before approval.
- **Structure.** Multi-level BOM with mass and cost roll-up, revision rules *latest*, *latest released* and *as
  released*, where-used, CSV export and *Pack and Go* ZIP of a whole assembly.
- **BOM import.** Items and multi-level BOMs from the BOM tables of CAD
  programs (Inventor, SolidWorks, Solid Edge, Fusion, Creo, NX, hand-made
  lists) as CSV/TSV, Excel or OpenDocument – English and German columns,
  levels or positions `1.1.2`, units; preview first, then one transaction.
- **Change propagation.** References inside CAD files (SolidWorks, Inventor,
  Solid Edge, CATIA, NX, STEP, KiCad, …) are tracked; a release flags every
  dependent for a check.
- **130+ file formats, 170+ extensions.** Native files of SolidWorks, NX, Creo, CATIA,
  Inventor, Fusion, Solid Edge, AutoCAD, Revit, Navisworks, MicroStation,
  Archicad, Rhino, Blender, Maya, 3ds Max, Houdini, SolidCraft, FieldCraft,
  … plus STEP, IGES, Parasolid, JT, IFC, glTF, USD, E57, LAS and more.
- **3D preview without plug-ins.** STEP (tessellated with the Rust
  [truck](https://github.com/ricosjp/truck) kernel), STL, OBJ, PLY, OFF, 3MF,
  AMF, glTF/GLB, 3DS and point clouds – converted once on the server, in an
  isolated process with a time limit, and drawn by a small WebGL viewer.
- **Previews of native CAD files.** The preview image Inventor, Solid Edge
  and SolidWorks store in their files is shown next to them; on Windows the
  desktop app creates STEP exports with your own Inventor or Solid Edge at
  check-in for the 3D view (see `docs/src/research/cad-previews.md`).
- **Search.** Ranked full-text search over numbers, names, materials, file
  names and the text inside STEP, IGES and text files; filter by CAD program.
- **Teams.** Viewer, CAD editor and admin roles; self-service project groups
  managed by their creators; single sign-on with any OpenID Connect
  provider and group synchronisation.
- **Notifications.** In-app bell, e-mail and Mattermost; watch items and
  changes.
- **Desktop app.** A Tauri client for Linux, macOS and Windows with a local
  workspace (one folder per project group) for check-out and check-in, the
  download of whole project groups and an import wizard for existing CAD
  folders with a step-by-step tutorial.
- **Audit log.** Every modification with user, time and details.

## Quick start

```sh
git clone https://github.com/rudalabs/TeamCraft.git && cd TeamCraft
docker compose up -d db     # PostgreSQL on 127.0.0.1, no password needed
cargo run --release         # applies the migrations, serves http://localhost:8080
```

Open <http://localhost:8080> and create the administrator on the login page (offered once, while the server listens on
localhost). Then *Projects → New
project group*, *Items → New item* with a STEP file, and open it in 3D.

No configuration is needed for a local test. Step-by-step guides, including
native PostgreSQL and the desktop app:
**[Linux](platforms/linux.md)** · **[macOS](platforms/macos.md)** · **[Windows](platforms/windows.md)**.

## How it fits together

| Layer                |                                                                                                                    |
|----------------------|--------------------------------------------------------------------------------------------------------------------|
| `teamcraft` binary   | web interface, REST API (`/api/v1`) and CLI in one process; templates, migrations and assets are embedded          |
| `domain`, `services` | the rules and the business logic – permissions, row locks, lifecycle checks and the audit entry in one transaction |
| `vault`              | content-addressed file storage on disk, plus the cached 3D previews                                                |
| `mesh`, `preview`    | pure-Rust readers for meshes, point clouds and STEP                                                                |
| PostgreSQL 14+       | the only external service                                                                                          |

Everything is built by `cargo` – no Node.js, no downloaded browser engines,
no C++ CAD kernel. Details: [Architecture](https://rudalabs.github.io/TeamCraft/reference/architecture.html).

## Automate it

```sh
API=http://localhost:8080/api/v1; H="Authorization: Bearer $TOKEN"   # token from Account → API tokens
curl -s -X POST -H "$H" $API/items/PRT-000001/revisions/A/checkout
curl -s -X POST -H "$H" -F file=@hub.step -F comment="wall 2 → 2.5 mm" \
     $API/items/PRT-000001/revisions/A/checkin
curl -s -H "$H" "$API/items?q=hub&system=solidworks"
```

## Works with SolidCraft and FieldCraft

TeamCraft recognises [SolidCraft](https://github.com/tangency-labs/solidcraft)
designs (`.sc`, checked by content) and
[FieldCraft](https://github.com/tangency-labs/fieldcraft) models (`.fieldcraft`), and previews their STEP and 3MF
exports – keep design,
export and analysis together in one revision.
[More](https://rudalabs.github.io/TeamCraft/reference/interop.html).

## Documentation

The documentation is published at **<https://rudalabs.github.io/TeamCraft/>**
(built from [`docs/`](docs) with mdBook, plus the rustdoc API reference):
user guide, administration (configuration, SSO, operations) and reference (file formats, REST API, architecture). A
running server lists its API at
`GET /api/v1` and its formats at `/formats`.

## Contributing

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                  # end-to-end tests need the database: docker compose up -d db
```

CI runs the same checks on Linux, macOS and Windows ([`.github/workflows`](.github/workflows)). Database changes are new
numbered
files in `migrations/`; every mutation goes through `src/services/`. Open
work and known gaps are listed in [docs/OPEN_WORK.md](docs/OPEN_WORK.md).

## Licence

TeamCraft is licensed under the [Mozilla Public License 2.0](LICENSE)
(`MPL-2.0`): modified TeamCraft files stay open, and TeamCraft can be
combined with code under other licences in larger works.

Third-party components (complete list with versions in `Cargo.lock` and
`desktop/Cargo.lock`):

| Component                            | Used for                                 | Licence                                  |
|--------------------------------------|------------------------------------------|------------------------------------------|
| axum, tower, hyper, tokio            | web server, async runtime                | MIT                                      |
| sqlx                                 | PostgreSQL access, migrations            | MIT / Apache-2.0                         |
| MiniJinja                            | HTML templates                           | Apache-2.0                               |
| truck (truck-stepio, truck-meshalgo) | STEP reading and tessellation            | Apache-2.0                               |
| gltf, quick-xml, zip                 | glTF, 3MF/AMF, Pack and Go               | MIT / Apache-2.0                         |
| csv, calamine, encoding_rs           | BOM import: CSV, Excel/ODS, Windows-1252 | MIT-Unlicense / MIT / MIT-Apache + BSD-3 |
| cfb, crc32fast                       | previews stored in CAD files             | MIT / MIT-Apache                         |
| rust_xlsxwriter (tests only)         | Excel files for the import tests         | MIT / Apache-2.0                         |
| reqwest, rustls, lettre              | HTTP client, TLS, e-mail                 | MIT / Apache-2.0                         |
| argon2, sha2                         | password hashing, content hashes         | MIT / Apache-2.0                         |
| Tauri 2                              | desktop app                              | MIT / Apache-2.0                         |
| Montserrat, Oswald                   | fonts                                    | SIL OFL 1.1                              |

The distribution logos in `media/icons/` (Arch Linux, CachyOS, EndeavourOS,
KDE, Kubuntu, openSUSE, Ubuntu) are trademarks of their owners and only
label the matching skins. SolidWorks, NX, Creo, CATIA, Inventor, Fusion,
Revit and the other product names are trademarks of their respective
owners; TeamCraft is not affiliated with them.

[badge-linux]: https://img.shields.io/badge/Linux-Build_from_source-E95420?style=for-the-badge&logo=linux&logoColor=white
[badge-macos]: https://img.shields.io/badge/macOS-Build_from_source-333333?style=for-the-badge&logo=apple&logoColor=white
[badge-windows]: https://img.shields.io/badge/Windows-Build_from_source-0078D6?style=for-the-badge
[badge-docs]: https://img.shields.io/badge/Docs-Read-4b5563?style=for-the-badge&logo=mdbook&logoColor=white
[linux]: platforms/linux.md
[macos]: platforms/macos.md
[windows]: platforms/windows.md
[docs]: https://rudalabs.github.io/TeamCraft/
