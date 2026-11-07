# Contributing to TeamCraft

Thank you for your interest in TeamCraft! TeamCraft is an open-source product
lifecycle management (PLM) system written in pure Rust. It is a **work in
progress** with **no official release yet**, so it helps a lot when people
try it out and say what works and what doesn't.

Everyone who takes part is expected to treat others with respect. Be kind,
assume good intent and criticise the work, not the person. Harassment of any
kind is not tolerated.

## Ways to contribute

* **Try it and report what you find.** Follow the quick start in the
  [README](README.md) or the guide for [Linux](platforms/linux.md),
  [macOS](platforms/macos.md) or [Windows](platforms/windows.md), then
  [open an issue](https://github.com/rudalabs/TeamCraft/issues) for each bug,
  confusing behaviour or gap in the documentation you run into.
* **Test with real CAD data.** Several parts were built from public format
  descriptions without the CAD programs at hand, and they need real files:
  the 3D preview of STEP exports, the scan for references inside CAD files,
  the BOM table import and the embedded preview images of Inventor, Solid
  Edge and SolidWorks files. Tell us the program, its version and what
  happened. Attach a file only if you are allowed to share it.
* **Improve the documentation**, either the book in [`docs/`](docs) (mdBook)
  or the build guides in [`platforms/`](platforms).
* **Write code.** Pick an entry from [docs/OPEN_WORK.md](docs/OPEN_WORK.md) or
  an open issue – preferably from its sections *Proofing* and *Structure*: tests,
  fuzzing and refactoring come before new features for now (see *Direction*
  there). For anything bigger than a bug fix, open an issue first and
  describe your plan. That way we can agree on the approach before you put
  time into it.

Security problems are the exception: **do not open a public issue**. Follow
[SECURITY.md](SECURITY.md) instead.

## Reporting bugs

A good bug report contains:

* the TeamCraft version and commit (`teamcraft --version`,
  `git rev-parse --short HEAD`);
* the operating system, the PostgreSQL version, and the browser or desktop app
  you used;
* what you did, what you expected and what happened instead;
* the relevant lines of the server log, or a screenshot.

**Remove secrets before you post.** That means passwords, API tokens
(`tc_pat_…`), session cookies (`tc_sess_…`), OIDC client secrets, webhook URLs
and private keys.

## Development setup

You need:

* Rust stable, installed with [rustup](https://rustup.rs). The minimum
  supported version is 1.82.
* PostgreSQL 16 or newer. The easiest option is the dev database from
  `docker-compose.yml`, which needs Docker with Compose v2.
* For the desktop app only: the
  [Tauri prerequisites](https://tauri.app/start/prerequisites/) of your
  platform. On Linux these are the WebKitGTK development packages.

```sh
git clone https://github.com/rudalabs/TeamCraft.git && cd TeamCraft
docker compose up -d db     # PostgreSQL on 127.0.0.1:5432, no password needed
cargo run                   # applies the migrations, serves http://localhost:8080
```

You don't need any configuration to work locally. Every setting is optional
and documented in
[docs/src/admin/configuration.md](docs/src/admin/configuration.md). Put local
settings into `secrets/.env`. The whole `secrets/` folder is git-ignored and
must never be committed.

## Checks before a pull request

CI runs these checks on every pull request: the tests against PostgreSQL 16
and 18 on Linux (with coverage on one leg), the server build on Windows, the
desktop app on Linux, macOS and Windows; `main` adds the macOS server build and
more desktop runners (`.github/workflows/`).
Run them locally first:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                  # unit tests and end-to-end tests
```

The end-to-end tests in `tests/` create and drop their own temporary
`teamcraft_test_*` databases. They use the configured database server, or
`TEAMCRAFT_TEST_DATABASE_URL` if it is set. When no database is reachable,
they are skipped; with `TEAMCRAFT_REQUIRE_DB_TESTS=1` set, that counts as a
failure instead. `cargo test --lib` runs only the unit tests and needs no
database. `tests/backup_restore.rs` additionally runs `pg_dump` and
`pg_restore` from `PATH` (major version at least the server's); without them
it is skipped, with `TEAMCRAFT_REQUIRE_PG_TOOLS=1` (CI) that fails.

Each test binary keeps at most 6 test databases open at a time, with 5
connections each (`TEAMCRAFT_TEST_DB_SLOTS` changes the 6). Running several
test binaries or checkouts against one server at the same time can still
exceed PostgreSQL's `max_connections` (100); the harness then waits for
connections instead of failing. On a server whose `lc_messages` is not English (the
Windows installer takes the system language), PostgreSQL sends that refusal
in a non-UTF-8 encoding that sqlx cannot read – `lc_messages = 'C'` in
`postgresql.conf` gives readable errors. A failing test prints the server's
warnings and errors (`RUST_LOG` selects more), so a `500` shows its cause.

| If you changed …  | also run                                                            |
|-------------------|---------------------------------------------------------------------|
| the desktop app   | `cargo fmt/clippy/test --manifest-path desktop/Cargo.toml`          |
| the documentation | `mdbook build docs` (mdBook 0.5) and `scripts/check-doc-links.sh`   |
| the web UI        | check the changed pages in a browser (no CSP errors in the console) |

### Scale check

If you change queries, indexes or pages that list many rows, run the scale
check: an ignored test that seeds a large synthetic vault (~100k items, ~1M
BOM lines, 2M audit rows; see
[Operations → Scale check](docs/src/admin/operations.md#scale-check)) and
times the hot paths through the router.

```sh
docker compose up -d db
cargo test --release --test scale -- --ignored --nocapture
```

The seed (a few minutes, ~1.6 GB) goes into the database `teamcraft_scale` on
the test server and is kept, so the next run only applies new migrations and
measures; `DROP DATABASE teamcraft_scale` starts over. The report lands in
`target/scale-report/` (`report.md`, `plans.md` with the
`EXPLAIN (ANALYZE, BUFFERS)` plan of every statement, `results.json`). Set
`TEAMCRAFT_SCALE_BASELINE=<an earlier results.json>` for a before/after
column and `TEAMCRAFT_SCALE_RUNS` for more timed calls per step. The plans
need a superuser (as in the dev container), because they come from
`auto_explain`. On GitHub, run the **Scale check** workflow by hand
(`gh workflow run scale.yml --ref <branch>`) and download the `scale-report`
artifact; it is not part of the pull request checks.

## Updating the README screenshots

The screenshots in `media/screenshots/` are made by
[`scripts/screenshots.sh`](scripts/screenshots.sh), never by hand, so that they
always show the same demo data. The script needs a running TeamCraft with an
**empty** database that listens on localhost. It creates the administrator
through the first-start page and two editors, the project group ROVER, parts
and assemblies with the NIST files from `tests/fixtures/cad`, a multi-level
BOM, a released and a pending change order, check-outs and notifications. It
then drives headless Chrome through chromedriver's WebDriver API (with `curl`
and `jq`, no Node or Python) and saves the pages as PNGs: 1440 × 900 for the
desktop and 390 pixels wide for the phone, each in the light scheme
(`NAME.png`, the names the README uses) and again in the dark scheme
(`NAME-dark.png`; Chrome emulates `prefers-color-scheme`, the demo user keeps
the System skin). The passwords and API tokens are generated for each run
and never printed.

With `DESKTOP_APP` set to a built TeamCraft Desktop binary (Linux), the script
also captures the desktop app, once with *TeamCraft Light* (`desktop-*.png`)
and once with *TeamCraft Dark* (`desktop-*-dark.png`): it pre-seeds a fresh profile
in a temporary `XDG_CONFIG_HOME` (server address and a personal access token
of the demo administrator, see `desktop/src/settings.rs`), starts the app on
an Xvfb display of 1440 × 900 without a window manager and captures the
screen with ImageMagick. The app has no deep links, so `xdotool` drives it like
a user: the Ctrl+2 shortcut (worklist) and clicks on navigator rows at fixed
screen positions (the ROVER group, the checked-out bracket, the change order
waiting for review). When the desktop layout or the demo data changes, check
that the `desktop-*.png` images still show what their names say and adjust the
coordinates in `desktop_run`.

1. Run the **Screenshots** workflow (`.github/workflows/screenshots.yml`):
   *Actions → Screenshots → Run workflow*, or open a pull request that changes
   the script or the workflow. It builds the release binary and the desktop
   app, starts the server against an empty PostgreSQL 17 and runs the script.
2. Download the `screenshots` artifact of the run (kept for 30 days), for
   example with `gh run download <run-id> -n screenshots -D media/screenshots`.
3. Look at every image, then commit the PNGs in `media/screenshots/`. The
   workflow never commits anything itself.
4. Commit the images **in the same pull request** as the README lines that
   show them. The **Docs** workflow (`.github/workflows/docs.yml`) runs
   [`scripts/check-doc-links.sh`](scripts/check-doc-links.sh) for every pull
   request that touches Markdown or `media/`: it fails and lists every local
   image or link target in `README.md`, the other top-level Markdown files,
   `platforms/`, `desktop/README.md` and `docs/src/` that does not exist. Run it
   locally with `scripts/check-doc-links.sh` before you push.

At the end of a run, `scripts/screenshots.sh` compares its PNGs with the
`media/screenshots/*.png` names in the README and prints a warning (a
workflow annotation on GitHub) for every image the README shows that the run
did not make – the run itself does not fail. Without `DESKTOP_APP` the
`desktop-*.png` images are among them.

To run it locally against a throw-away database and vault (Chrome and a
matching chromedriver installed):

```sh
docker compose exec db createdb -U teamcraft teamcraft_shots
TEAMCRAFT_DATABASE_URL=postgres://teamcraft@localhost:5432/teamcraft_shots \
TEAMCRAFT_VAULT_DIR=/tmp/teamcraft-shots ./target/release/teamcraft serve &
BASE_URL=http://127.0.0.1:8080 OUT_DIR=/tmp/shots scripts/screenshots.sh
```

New pages for the README are added as a `shot` line in the `capture`
function of the script and as an image in the *Screenshots* section of the
README, with alt text that describes what the image shows.

## Code conventions

[Architecture](docs/src/reference/architecture.md) gives an overview of the
code base. These are the most important rules:

* **All writes go through `src/services/`.** A service function does four
  things, all in one transaction:
  * checks permissions (`Actor::require`, `revisions::ensure_editable`);
  * locks the rows it changes (`SELECT … FOR UPDATE`);
  * validates lifecycle transitions (`LifecycleState::can_transition_to`);
  * writes the audit entry (`audit::record`).

  The web handlers (`src/web/`), API handlers (`src/api/`) and MCP tools
  (`src/mcp/`) stay thin and call the same service functions.
* **Reads take the actor.** [Restricted project groups](docs/src/admin/permissions.md)
  hide their data from non-members, so every read service takes `&Actor`
  and answers "not found" for hidden objects. Only `services::access` writes
  the SQL guards (`access::item_visible` and friends); lists put the guard
  into the query that the `COUNT(*)` shares. The `*_unchecked` getters are
  for background and system code – `tests/no_unchecked_in_handlers.rs` fails
  if one is called from `src/web`, `src/api` or `src/mcp`.
* **New routes and MCP tools** are exercised by the leak test
  (`tests/read_permissions.rs`), which reads the router sources: a route with
  a new kind of path parameter needs a placeholder rule in its `fill()`.
* **SQL** uses runtime queries (`sqlx::query_as::<_, T>` with `FromRow`), not
  the `query!` macros, so building needs no database.
* **Architecture guards** (`tests/architecture_guards.rs`, no database) read
  the sources and fail when a rule is broken: `sqlx` in `src/web`, `src/api`
  or `src/mcp`; a public service function that writes without an audit entry
  (itself or through the services it calls – personal data, caches and
  housekeeping go on its `NOT_AUDITED` list with a reason); a template that
  is not in `TEMPLATES` or not rendered by `every_template_renders`; a
  `TEAMCRAFT_*` variable missing from `docs/src/admin/configuration.md`.
* **Multi-row mutations** run in one transaction. `tests/atomic_processes.rs`
  shows how to prove it: `inject` puts a trigger on a table that makes one
  step fail (an error or a lost connection), and the `snapshot` before and
  after must be equal; add a case there for a new multi-step mutation.
* **Schema changes** go into a new numbered file in `migrations/`. Never edit
  a migration that is already on `main`, because existing databases have
  applied it. `tests/migrations.rs` upgrades a v0.1.0 database with data
  (`tests/fixtures/migrations/v0.1.0-seed.sql`) and fails when a migration
  loses rows, changes old values or narrows a `CHECK` list that old rows
  still use – a new enum value is fine, removing one needs a data migration.
* **Enums** are `TEXT` columns with `CHECK` constraints. The Rust enums in
  `src/domain.rs` provide `as_str()` and `parse()`.
* **Errors** use `AppError` (`src/error.rs`). User errors are shown to the
  user; internal errors are logged.
* **Web UI:**
  * Forms follow POST → redirect → GET, with a flash message.
  * Register every new template in `TEMPLATES` (`src/templates.rs`) and in
    the `every_template_renders` test.
  * No inline JavaScript or styles: the Content Security Policy blocks them.
  * Cancel buttons take effect immediately, without a confirmation.
  * **Design tokens:** no literal colours outside `shared/css/skins.css`; every
    component reads the tokens (`--surface`, `--text-muted`, `--primary`,
    `--st-<state>-fg/-bg/-bd`, …). Tokens are opaque `#rrggbb`; the unit tests
    `skins_meet_contrast` and `dark_token_blocks_identical` (`src/domain.rs`)
    check the WCAG contrast and that the two dark blocks match.
  * **Shared UI files** (`shared/`): what both the web UI and the desktop
    app use – skins, fonts, the 3D viewer. The server embeds the folder
    without a hand-written list (`build.rs`), the desktop build copies it;
    every `.css`/`.js`/`.svg` starts with
    `/* SPDX-License-Identifier: MPL-2.0 */`. Rules in `shared/README.md`.
  * **Density by tokens:** sizes of controls, rows and body text come from
    `--ctl-h`, `--ctl-h-sm`, `--row-h`, `--fs-body`, `--lh-body`; the web UI
    uses the defaults, the desktop app `data-density="compact"`. No
    app-specific copies of these tokens.
  * No gradients, glow, texture, glass or uppercase labels in components
    (`grep -n gradient static/style.css` stays empty;
    only the distro skins' `--glow` / `--pattern` behind the page use them).
  * One filled (primary) button per view; the rest is secondary, ghost or in
    a *More* menu.
  * State is colour **and** a glyph (✓ released, ○ in review, ◐ in work,
    × rejected, – obsolete, △ check needed), never colour alone.
  * Check both schemes: the screenshot workflow captures every web page in
    light (`NAME.png`) and dark (`NAME-dark.png`), the desktop app as well
    (`desktop-*.png`, `desktop-*-dark.png`).
* **Pure Rust:** everything builds with `cargo` alone. No Node.js, no
  downloaded engines and no C/C++ CAD kernels; 3D conversion stays in
  `src/mesh.rs`.
* **Configuration:** a new setting goes into `src/config.rs` and
  `docs/src/admin/configuration.md`, and it must stay optional for a local
  test.
* **Tests:** new behaviour comes with tests. Put unit tests next to the code
  and end-to-end tests in `tests/` (`api_flow.rs` for the REST API,
  `web_ui.rs` for the web interface).
* **Documentation:** keep `README.md`, `docs/src/`, `platforms/*.md` and
  `desktop/README.md` in sync with your change.

## Dependencies, research and branding

TeamCraft is licensed under the MPL-2.0, so new dependencies must be
compatible with it:

* **Allowed:** MIT, Apache-2.0, BSD, ISC, Zlib, Unlicense, MPL-2.0.
* **LGPL:** only for a component that is loaded separately.
* **Not allowed:** GPL, AGPL, SSPL and proprietary code.

Add every new third-party component to the licence table in the README.

Before you build a larger feature, look at how existing open-source projects
solve the problem. If you research how other systems handle something, write
the findings down in [`docs/src/research/`](docs/src/research): one page per
topic, listed in its `index.md` and in `docs/src/SUMMARY.md`.

Do not add logos, artwork or branding of other products, and nothing that
imitates the look of another vendor's software. Product names may be used to
describe compatibility, for example "reads SolidWorks files".

## Commits and pull requests

1. Fork the repository and create a branch from `main`.
2. Keep each pull request to one topic, and keep refactoring separate from
   behaviour changes.
3. Write commit messages in English: a short summary line (at most 72
   characters), a blank line, then what changed and why.
4. Update the documentation. If you finish or add a work item, also update
   [docs/OPEN_WORK.md](docs/OPEN_WORK.md).
5. Open the pull request against `main`, describe what changed and how you
   tested it, and link the issue. CI has to pass before the pull request is
   merged.

**Never commit secrets:** no passwords, tokens, private keys, `.env` files,
database dumps or vault data. If it happens anyway, tell the maintainers
right away so the secret can be rotated. Deleting it in a later commit is
not enough, because it stays in the history.

## Licence of contributions

TeamCraft has no contributor licence agreement. By submitting a contribution,
you confirm that you wrote it or have the right to submit it. You also agree
that it is licensed under the [Mozilla Public License 2.0](LICENSE), like the
rest of the project.

## Releases and versions

`main` is the development branch. Version numbers follow
[Semantic Versioning](https://semver.org); while the version is `0.x`, any
release may contain breaking changes.

**Build artifacts.** Every push to `main` runs
`.github/workflows/release.yml`, which builds the server binaries (Linux
x86_64/ARM64, Windows x64) and the desktop installers (AppImage, `.deb`,
`.rpm` and Flatpak bundles for x86_64/aarch64, `.dmg`, and for Windows x64
and ARM64 `.msi`, NSIS `-setup.exe` and a portable `.zip`) and keeps them
as workflow artifacts for 14 days. *Run workflow* on the Actions page builds
any branch the same way.

**Container image.** The same workflow builds `ghcr.io/rudalabs/teamcraft`
from the `Dockerfile` on an amd64 and an arm64 runner and publishes it:
`:edge` for every push to `main`, `:<version>`, `:<major>.<minor>` and
`:latest` for a tag (`latest` also for 0.x, but not for suffixed versions
such as `-rc.1`). A pull request that changes the `Dockerfile` or `deploy/`
builds the image without publishing it. Before anything is published, a
smoke test runs `deploy/install.sh` with the freshly built image against
PostgreSQL and checks `/readyz`, an administrator, the health check, an
upgrade run and the uninstall. Images are built by CI only – please don't
publish your own builds under the project's name. Check `deploy/install.sh`
with `bash -n` and `shellcheck` (CI runs both).

**Cutting a release** (maintainers):

1. Set the same version in `Cargo.toml`, `desktop/Cargo.toml` and
   `desktop/tauri.conf.json` (the workflow refuses a mismatch), run
   `cargo check` and `cargo check --manifest-path desktop/Cargo.toml` so both
   `Cargo.lock` files follow, and commit.
2. Tag and push: `git tag -a v0.2.0 -m "TeamCraft 0.2.0" && git push origin v0.2.0`.
3. The workflow builds everything again from the tag, writes
   `SHA256SUMS.txt` and creates the GitHub Release with generated notes –
   as a **pre-release** for `0.x` and suffixed versions (`1.0.0-rc.1`).

The installers are not code-signed yet (macOS: ad-hoc signature, Windows:
unsigned); signing needs certificates as repository secrets and is a
follow-up. Nothing else is published (no package registries).
