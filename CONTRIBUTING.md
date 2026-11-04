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
  an open issue. For anything bigger than a bug fix, open an issue first and
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

CI runs these checks on every pull request, on Linux, macOS and Windows.
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
database.

| If you changed …  | also run                                                            |
|-------------------|---------------------------------------------------------------------|
| the desktop app   | `cargo fmt/clippy/test --manifest-path desktop/Cargo.toml`          |
| the documentation | `mdbook build docs` (mdBook 0.5)                                    |
| the web UI        | check the changed pages in a browser (no CSP errors in the console) |

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
desktop and 390 pixels wide for the phone. The passwords and API tokens are
generated for each run and never printed.

1. Run the **Screenshots** workflow (`.github/workflows/screenshots.yml`):
   *Actions → Screenshots → Run workflow*, or open a pull request that changes
   the script or the workflow. It builds the release binary, starts it against
   an empty PostgreSQL 17 and runs the script.
2. Download the `screenshots` artifact of the run (kept for 30 days), for
   example with `gh run download <run-id> -n screenshots -D media/screenshots`.
3. Look at every image, then commit the PNGs in `media/screenshots/`. The
   workflow never commits anything itself.

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

  The web handlers (`src/web/`) and API handlers (`src/api/`) stay thin and
  call the same service functions.
* **SQL** uses runtime queries (`sqlx::query_as::<_, T>` with `FromRow`), not
  the `query!` macros, so building needs no database.
* **Schema changes** go into a new numbered file in `migrations/`. Never edit
  a migration that is already on `main`, because existing databases have
  applied it.
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
x86_64/ARM64, macOS ARM64, Windows x86_64) and the desktop installers
(AppImage, `.deb`, `.rpm`, `.dmg`, `.msi`, NSIS `-setup.exe`) and keeps them
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
