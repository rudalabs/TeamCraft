# Running and testing TeamCraft locally

This guide gets TeamCraft running on your machine and walks through a manual
test of every feature. It takes about 30–45 minutes the first time.

---

## 1. Prerequisites

| Tool                            | Why                              | CachyOS / Arch                                                                                                                        |
|---------------------------------|----------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Rust toolchain (stable, ≥ 1.82) | build & tests                    | `sudo pacman -S rustup && rustup default stable`                                                                                      |
| C compiler, make                | builds the `ring` crypto crate   | `sudo pacman -S base-devel`                                                                                                           |
| Docker + Compose v2             | PostgreSQL for development/tests | `sudo pacman -S docker docker-compose && sudo systemctl enable --now docker` (and `sudo usermod -aG docker $USER`, then log in again) |
| A browser                       | manual testing                   | –                                                                                                                                     |
| `curl`, optionally `jq`         | API tests                        | `sudo pacman -S curl jq`                                                                                                              |

Check: `cargo --version`, `docker compose version`.

## 2. First start

All configuration and secrets live in `secrets/` (git-ignored as a whole).
Create the two files once – the defaults are fine for local use:

```sh
cd TeamCraft
mkdir -p secrets
cp .env.example secrets/.env                                # local configuration
printf '%s' 'teamcraft-dev-password' > secrets/db_password  # dev database password
docker compose up -d db             # PostgreSQL 17 on 127.0.0.1:5432
cargo run                           # builds and starts on http://localhost:8080
```

`secrets/db_password` is read by the PostgreSQL container (`POSTGRES_PASSWORD_FILE`)
and by TeamCraft (`TEAMCRAFT_DB_PASSWORD_FILE=secrets/db_password` in
`secrets/.env`). Run `cargo` from the repository root – relative paths in
`secrets/.env` are resolved from there. The password only takes effect when
the database volume is created; to change it later, see §5.

The first build takes a few minutes. On start TeamCraft

* waits for the database (up to 60 s), applies the migrations,
* creates the **bootstrap admin** `admin` / `change-me-please`
  (`TEAMCRAFT_BOOTSTRAP_ADMIN_*` in `secrets/.env`) – only if the database has no users yet,
* serves the UI at <http://localhost:8080> and the API at <http://localhost:8080/api/v1>.

Stop with `Ctrl+C`. Uploaded files end up in `./data/vault` (git-ignored).

Useful variants:

```sh
RUST_LOG=debug cargo run                         # verbose logs
cargo run -- user create --username erika --role engineer   # create a user (prompts for the password)
cargo run -- user list
cargo run -- vault check --verify-hashes         # verify every stored file
cargo run --release                              # optimised build
docker compose up -d --build app                 # run the container image instead (port 8080)
```

> Only run one of `cargo run` and the `app` container at a time – both use port 8080.

## 3. Automated tests

```sh
cargo test                     # unit + integration tests (needs `docker compose up -d db`)
cargo test --lib               # unit tests only, no database needed
cargo fmt --all -- --check     # formatting, as in CI
cargo clippy --all-targets -- -D warnings   # lints, as in CI
```

The integration tests (`tests/`) connect to the database configured in
`secrets/.env` (or to `TEAMCRAFT_TEST_DATABASE_URL`, if set – CI does that).
Every test creates and drops its own temporary database (`teamcraft_test_<uuid>`),
so they never touch your development data. Without any database configuration
they print `skipping database test` and pass.

## 4. Manual test plan

Use **three browser sessions** (e.g. a normal window, a private window and a
second browser) so that you can be signed in as different users at the same
time. Tick off each expected result.

### A. Sign-in and users

1. Open <http://localhost:8080> → you are redirected to **Sign in**.
2. Sign in with a wrong password → *Invalid username or password.*
3. Sign in as `admin` / `change-me-please` → dashboard *Hello, Administrator*.
4. **Users** → *New local account*: create
    * `erika`, role **engineer**, password `erika-password-1`
    * `rolf`, role **engineer**, password `rolf-password-1`
    * `vera`, role **viewer**, password `vera-password-1`
    * try a password with 5 characters → error *at least 10 characters*.
5. Sign in as **erika** in window 2 and as **rolf** in window 3.
6. As admin open `rolf` on the Users page, uncheck *Active*, save → rolf's window is
   signed out on the next click. Re-activate rolf and sign in again.
7. Try to deactivate or demote `admin` itself → *At least one active administrator must remain.*

### B. Projects

1. As admin: **Projects** → create code `nixus`, name `Nixus` → stored as `NIXUS`.
2. Create `NIXUS` again → *already exists*.
3. Edit → uncheck *Active* → project is *archived* and no longer offered when creating items. Re-activate it.

### C. Items and revisions (as erika)

1. **Items → New item**: type *Part*, number empty, name `Bolt M6x20`, project NIXUS,
   mass `0,01` (comma works), unit cost `0.25` → redirected to **PRT-000001/A**, state *In Work*.
2. New item: *Assembly*, number `nx-eng-001`, name `Engine mount`, mass `0.5`, cost `20`
   → number normalised to **NX-ENG-001**.
3. New item with an empty name or mass `abc` → the form is shown again with an error and your input kept.
4. New item with number `NX-ENG-001` → *already in use*.
5. **Items** list: search `mount`, filter by type/project/state, check pagination appears with > 50 items (optional).
6. Open the short link <http://localhost:8080/i/nx-eng-001> → lands on the latest revision.

### D. Check-out / check-in

1. erika on NX-ENG-001/A: **Check out** → 🔒 badge with her name.
2. rolf opens the same revision: no *Edit* button, a notice says it is checked out by erika.
3. erika: **Edit attributes** → material `AlMg3` → saved, history shows the change.
4. admin opens the revision → **Check in** asks for confirmation (force release of erika's lock) → lock gone, history
   shows *Released check-out … held by erika*.

### E. Files

1. erika on NX-ENG-001/A → *Files*: upload one or more files (e.g. a PDF and a STEP file).
2. Download → identical file, browser saves it with the original name.
3. Upload the same file again → stored only once in `./data/vault/objects` (same SHA-256 prefix shown).
4. **Remove** a file → gone from the revision (the blob stays, older revisions may use it).
5. Optional: set `TEAMCRAFT_MAX_UPLOAD_MB=1` in `secrets/.env`, restart, upload a larger file → *exceeds the upload limit*.

### F. Bill of materials

1. NX-ENG-001/A → *Add component*: `PRT-000001`, quantity `4` → find number **10** assigned automatically.
2. Edit the line inline (quantity `6`, notes `M6 A2-70`) → **Save**.
3. Open PRT-000001/A → *Add component* `NX-ENG-001` → *would create a circular BOM structure*.
4. Same for the assembly itself → rejected.
5. **Multi-level view & roll-up** on NX-ENG-001/A → total mass `0.5 + 6 × 0.01 = 0.56 kg`, cost
   `20 + 6 × 0.25 = 21.5 €`.
6. Create a part without mass, add it to the assembly → total marked *incomplete*, ⚠ on the line.
7. Switch the revision rule to **Latest released** → lines show *no released revision* (nothing released yet).
8. **CSV** export opens in a spreadsheet program; formulas are not executed.
9. PRT-000001/A → *Where used* lists NX-ENG-001/A.

### G. Change order and release (four-eyes principle)

1. erika on NX-ENG-001/A → **Release via new change…** → title `Initial release engine mount`,
   priority *High* → change **CHG-000001** (*Draft*) with the revision already listed.
2. Add `PRT-000001` via *Add its in-work revision*.
3. **Submit for review** is not offered yet (no reviewer). The reviewer list does not contain erika (authors cannot
   review).
4. Add reviewer **rolf** → **Submit for review** → status *In Review*, both revisions *In Review*.
5. erika: the revisions can no longer be edited (no *Edit* buttons; check-out impossible).
6. rolf: dashboard → *Waiting for my review* lists CHG-000001.
7. rolf: **Reject** without comment → *Please explain why …*; with comment `Drawing missing` → change *Rejected*,
   revisions back to *In Work*.
8. erika: fix something, **Reopen as draft** → **Submit for review** again.
9. rolf: **Approve** with comment → change *Released*, both revisions *Released* with a release date.
10. Try to edit or change the BOM of a released revision → not possible.
11. Optional: create another change, submit, then **Withdraw** (back to Draft), then **Cancel**.

### H. New revisions and revision rules

1. NX-ENG-001/A (released) → **Revise** → **NX-ENG-001/B** *In Work* with copied attributes, files and BOM.
2. Revise A again → *already has the open revision B*.
3. PRT-000001/A → Revise → PRT-000001/B.
4. NX-ENG-001/B → multi-level view: rule *Latest* shows the bolt at **B**, rule *Latest released* at **A**.
5. NX-ENG-001/A → notice *You are looking at an older revision*.
6. PRT-000001/A → **Mark obsolete** → state *Obsolete* (only released revisions can become obsolete).

### I. Viewer permissions (as vera)

1. vera sees dashboard, items, revisions, BOMs, changes, downloads files.
2. No *New item*, *Edit*, *Check out*, upload or BOM forms; no *Users*/ *Audit log* in the menu.
3. Open <http://localhost:8080/items/new> directly → *403 · Forbidden*.

### J. Account and REST API

1. erika → click her name (top right) → **Create token** `curl test`, 90 days → token shown once; copy it.
2. In a terminal:

   ```sh
   export TOKEN=tc_pat_…        # the copied token
   API=http://localhost:8080/api/v1
   curl -s $API -H "Authorization: Bearer $TOKEN" | jq .endpoints        # endpoint list
   curl -s $API/me -H "Authorization: Bearer $TOKEN" | jq
   curl -s "$API/items?q=bolt" -H "Authorization: Bearer $TOKEN" | jq
   curl -s $API/items -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        -d '{"item_type":"part","name":"Washer M6","mass_kg":0.001,"project":"NIXUS"}' | jq
   curl -s "$API/items/NX-ENG-001/revisions/B/bom?expand=true&rule=released" -H "Authorization: Bearer $TOKEN" | jq .total_mass_kg
   curl -s -F file=@README.md $API/items/NX-ENG-001/revisions/B/files -H "Authorization: Bearer $TOKEN" | jq
   curl -s $API/me -H "Authorization: Bearer wrong" | jq      # → 401 unauthorized
   ```
3. Revoke the token on the account page → the same `curl` returns 401.
4. Change erika's password on the account page; sign out and in with the new password.

### K. Audit log and history

1. admin → **Audit log**: all actions above are listed; filter by entity type `change`, user `rolf`, action
   `change.released`.
2. Every revision and change page shows its own *History*.

### L. Deleting mistakes

1. Create an item `Test delete` → **Delete item** (only offered while it has a single, never released revision that is
   unused) → gone.
2. Items used in a BOM, part of a change or ever released cannot be deleted.

### M. Operations endpoints

```sh
curl -i http://localhost:8080/healthz          # 200 ok (liveness)
curl -s http://localhost:8080/readyz | jq      # database + vault status (readiness)
curl -sI http://localhost:8080/ | grep -iE 'content-security|x-frame|x-content-type'
cargo run -- healthcheck                       # exit code 0 while the server runs
```

Stop PostgreSQL (`docker compose stop db`) → `/readyz` returns 503 and pages show an
error page; `docker compose start db` → back to normal without restarting TeamCraft.

### N. Container image (optional)

Stop a running `cargo run` first (port 8080), then:

```sh
docker compose up -d --build app   # builds the image, reads secrets/.env + secrets/db_password
docker compose ps                  # app becomes "healthy" after a few seconds
docker compose logs -f app
docker compose stop app
```

### O. Single sign-on (optional, needs an Authentik application)

1. Ask IT for a test application in Authentik (see DEPLOYMENT.md §2) with redirect URI
   `http://localhost:8080/auth/oidc/callback`, and add yourself to `teamcraft-engineers`.
2. In `secrets/.env` set `TEAMCRAFT_OIDC_ISSUER_URL`, `TEAMCRAFT_OIDC_CLIENT_ID`,
   `TEAMCRAFT_OIDC_CLIENT_SECRET` and keep `TEAMCRAFT_LOCAL_LOGIN=true`; restart.
3. The login page shows **Sign in with WARR SSO** → after login you are an *engineer*;
   removing you from the group and signing in again makes you a *viewer*.

## 5. Resetting local data

```sh
docker compose down -v      # deletes the PostgreSQL volume (all data!)
rm -rf data/                # deletes uploaded files
```

To change the dev database password: write the new value to
`secrets/db_password` and reset the volume as above (PostgreSQL only reads the
password file when it initialises an empty volume).

## 6. Troubleshooting

| Symptom                                                                                 | Fix                                                                                                                                                            |
|-----------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `cannot connect to PostgreSQL … Connection refused`                                     | `docker compose up -d db`; check `docker compose ps`                                                                                                           |
| `password authentication failed`                                                        | the database volume was created with another password → restore the old value in `secrets/db_password`, or reset the volume (§5, deletes data)                 |
| `cannot read TEAMCRAFT_DB_PASSWORD_FILE`                                                | `secrets/db_password` is missing, or `cargo` was not started from the repository root                                                                          |
| `warning: secrets/.env could not be loaded completely`                                  | quote values with spaces: `NAME="WARR SSO"`                                                                                                                    |
| `Address already in use`                                                                | another process (or the `app` container) uses port 8080 → stop it or set `TEAMCRAFT_BIND_ADDR=127.0.0.1:8081` and `TEAMCRAFT_PUBLIC_URL=http://localhost:8081` |
| *Cross-site request blocked*                                                            | you posted a form from another origin (e.g. a browser extension or an HTML file); reload the page from TeamCraft                                               |
| Bootstrap admin is not created                                                          | it is only created when the database has **no** users; use `cargo run -- user create --role admin …`                                                           |
| Integration tests are *skipped*                                                         | no database configured: `secrets/.env` is missing (or `TEAMCRAFT_TEST_DATABASE_URL` is not set)                                                                |
| Integration tests fail with `connect to the test database server: … Connection refused` | a database is configured but PostgreSQL is not running → `docker compose up -d db`                                                                             |
