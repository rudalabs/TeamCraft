# TeamCraft Desktop

The rich client of TeamCraft for **Linux, macOS and Windows**: a navigation
rail, a navigator tree of the project groups, object tabs with sub-tabs, a
context toolbar, a status bar – and a **local workspace** for
check-out/check-in.

It is a [Tauri 2](https://tauri.app) application: the user interface (`ui/`,
plain HTML/CSS/JavaScript, no build step) runs in the system web view (WebView2
on Windows, WKWebView on macOS, WebKitGTK on Linux); the Rust side (`src/`)
talks to the TeamCraft server and manages the workspace. The app needs a
TeamCraft server – it is a client, not an offline copy.

> **Work in progress:** there is no official release yet, and the app is
> provided as is, without warranty of any kind. See the
> [project README](../README.md) and [CONTRIBUTING.md](../CONTRIBUTING.md).

## Features

| Area | What the desktop app does |
|---|---|
| Sign-in | **Sign in with the browser** (single sign-on or password, confirmed on a TeamCraft page) or a personal access token; *cancel* stops the sign-in at once |
| Navigator | Project groups (mine first) → items; *My Worklist* (check-outs, reviews, my open changes); change orders by status; full-text search |
| Revision tabs | Overview, Files (versions, restore, open), Structure (multi-level BOM, revision rules, roll-up), Where used, Changes, History, **3D viewer** (STEP, meshes, point clouds – converted by the server), Workspace |
| Check-out / check-in | *Check out* takes the lock and copies the files into `<workspace>/<project group>/<item>/<revision>/` (folders of the older layout without the group folder are moved there on first use); the Workspace tab shows *up to date* / *modified* / *outdated* / *conflict* / *new* / *missing* files (SHA-256) and who holds the lock for how long; *Check in…* uploads the chosen files as new versions and releases the lock in one step. The version every file was copied from is remembered in the hidden `.teamcraft-base.json` of the revision folder and sent along with the check-in, so the server refuses (409) to overwrite a version somebody else checked in meanwhile; *Get files* replaces outdated, unchanged copies and keeps modified ones. Opening an outdated file or one somebody else has checked out asks first. Lock and backup files of the CAD programs are skipped; Creo iterations count as one file |
| Check in all | *Check in all* in the worklist (navigator and home page): every check-out without local changes is checked in on the server (lock released, one revision at a time); revisions with modified, new or conflicting files stay checked out and are listed |
| Native CAD previews | The preview image stored in Inventor, Solid Edge and SolidWorks files in the Files tab; on Windows *STEP* in the Workspace tab and *Create STEP exports … first* at check-in export with the installed Inventor or Solid Edge (COM automation through Windows PowerShell; uses your own licence) |
| Whole project groups locally | *Get…* in the navigator (*Tools → Get Project Groups…*): copies the files of every item of the chosen groups – latest released or latest revision – into the workspace, one folder per group; unchanged files are skipped, locally modified files kept. For new team members or a fresh computer |
| Import a CAD folder | *Import…* in the navigator (*Tools → Import CAD Folder…*): scans an existing folder (dry run: items, duplicate file names, skipped lock/backup files), then creates one item per part or assembly with the drawings and exports of the same name, project files in one document item, optional BOM lines from the assembly references, and checks every item in right after its files (assemblies once more after their BOM lines), so an interrupted import never leaves items checked out – a new run of the same folder also releases the check-outs an earlier, interrupted run left behind; a final report lists the new items and files, BOM lines, check-ins and problems (with *Open* for the top assembly); interrupted imports continue where they stopped. Tutorial: *Help → How to Import a CAD Folder* |
| New item | *New* (`Ctrl/Cmd+N`): choose the CAD files first – name and type follow from the first file; they are uploaded right after the item is created |
| Release and changes | *Release…* creates and submits a change order – with the unreleased dependencies (BOM children, drawings) by default; *Release readiness* panel with *Add all*; approve/reject from the worklist |
| Change requests | *Request change…* on a revision tab and *Request…* on a file (Files and Workspace tabs) raise a change request prefilled with item, revision and file name, with an optional screenshot or PDF (native file dialog, uploaded by the Rust side); request tabs with discussion, decision (accept / defer / reject), *Create change order*, attachments; *Requests assigned to me* and *My open requests* in the worklist |
| Change propagation | *Dependents to check*, *Check needed* banner with **Acknowledge…**, references, *As released* rule, impact analysis |
| Notifications | Bell with unread count and toasts; **Watch** / **Unwatch** |
| Ask TeamCraft | **Ask…** in revision and change tabs (only when the server has the assistant enabled): *Draft release notes*, *Check before release*, *Brief me for this review* or a free question; the server answers with Claude and the user's rights, read-only unless drafts are allowed |
| Pack and Go, STL | ZIP of a revision and its structure; **Export STL…** from the 3D viewer |
| Skins | The same skins as the web UI |

Keyboard: `Ctrl/Cmd+N` new item, `Ctrl/Cmd+F` search, `F5` refresh,
`Ctrl/Cmd+1/2/3` Home/Worklist/Changes, `Ctrl/Cmd+W` close tab; zoom with
`Ctrl` + mouse wheel or `Ctrl/Cmd` + `+` / `-` / `0`.

## Continuous integration

`.github/workflows/desktop.yml` runs on every change to `desktop/`,
`static/` or `media/`: `cargo fmt`, `clippy` and the tests, a build, and a
**start-up smoke test** (the app is started with a fresh profile – on Linux
under a virtual display – and must still be running after 20 seconds) on
Linux x86_64 (Ubuntu 24.04 and 26.04) and ARM64, macOS ARM64 and Windows
x86_64; Windows ARM64 runs as a preview that never blocks. A separate job
parses every script of `ui/` and the web UI (`node --check`; Node.js is only
used by CI, the app has no build step). Nothing is packaged or published.

## Run from source

Installers are not published yet; build and run the app locally. Linux needs
the WebKitGTK development files:

```sh
# Ubuntu / Debian
sudo apt install libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
# Arch / CachyOS
sudo pacman -S webkit2gtk-4.1 libappindicator-gtk3 librsvg xdotool
```

```sh
cd desktop
cargo run                      # start the server first: cargo run in the repository root
cargo test                     # settings, API paths, PKCE + loopback sign-in, workspace comparison
cargo clippy --all-targets -- -D warnings
```

On the first start enter the server address (`localhost:8080` for a local
server) and click **Sign in with the browser**: the browser asks you to
confirm the sign-in for this computer, the app receives a personal access
token (valid 90 days, revocable on your account page in the web UI).

Where the app keeps its data:

* settings and the token: the per-user configuration directory
  (`~/.config/io.github.rudalabs.teamcraft`, `~/Library/Application Support/io.github.rudalabs.teamcraft`,
  `%APPDATA%\io.github.rudalabs.teamcraft`); the token file is readable by the user only;
* checked-out files: `Documents/TeamCraft/<project group>/<item>/<revision>/` (configurable in *Settings*).

## Layout

`build.rs` copies the assets the app shares with the web UI into `ui/static/`
(git-ignored): `static/skins.css`, the fonts, the logos in `media/` and the 3D
viewer.

```text
src/main.rs        Tauri setup, native menu, state
src/commands.rs    the commands the UI calls (every network and file access)
src/api.rs         REST client (/api/v1, bearer token) – the web view never talks to the server
src/signin.rs      browser sign-in: PKCE, loopback listener on 127.0.0.1
src/workspace.rs   check-out folders, SHA-256 comparison with base versions (outdated / conflict), check-in upload
src/settings.rs    settings.json and the token file
ui/                index.html, app.css (layout), app.js (all views)
capabilities/      Tauri permissions of the window (dialogs only – files and URLs are opened by Rust)
icons/             app icons, rendered from icons/app-icon.svg with `tauri icon`
```

The browser sign-in uses the server endpoints `/desktop/authorize` (web page,
confirmation) and `POST /api/v1/desktop/token` (code + PKCE verifier → token);
see `src/services/desktop.rs` in the server.
