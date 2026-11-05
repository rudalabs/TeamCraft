# TeamCraft Desktop

The rich client of TeamCraft for **Linux, macOS and Windows**: a navigation
rail, a navigator tree of the project groups, object tabs with sub-tabs, a
context toolbar, a status bar – and a **local working folder** for
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
| Navigator | Project groups (mine first) → items, loaded again on *Home* and *Refresh* (a group created in the web UI shows without a restart); *My Worklist* (check-outs, reviews, my open changes); change orders by status; full-text search |
| Toolbar | Below the tab strip, acting on the active tab: grouped buttons that keep their place (revision: Lock · Work · Lifecycle · Collaborate; change: Review · Lifecycle · Collaborate; request: Triage · Requester · Collaborate), the **next step highlighted** (*Check out*, *Check in…* with local changes, *Release…* on a clean check-out, *Revise* when released, *Submit for review* / *Approve change* on a change), greyed-out buttons with a tooltip saying why, rare actions (*Pack and Go…*, *Open in browser*, *Refresh*, *Mark obsolete…*) in **More ▾** – keyboard: `Enter`/`Space`/`↓` open, arrows move, `Esc` closes |
| Revision tabs | Overview (with *At a glance*: the preview image from the CAD file, files, BOM lines, where used, references, change orders, open requests – and a link to the similar parts in the browser), Files (versions, restore, open), Structure (multi-level BOM, revision rules, roll-up), Where used, Changes, History, **3D viewer** (STEP, meshes, point clouds – converted by the server), Workspace |
| Working folder | One folder per project group that **mirrors the folder structure of the files** (`<workspace>/<group>/<folder>/<file>`, the folder is stored with every file on the server), one copy of each file – the revision is metadata in `<workspace>/.teamcraft/state.json` (item, revision, file id, version, SHA-256, checked out). Files you have not checked out are **read-only** on disk; *Check out* takes the lock and makes them writable; *Check in…* uploads the chosen files as new versions, releases the lock in one step and makes them read-only again (files above 32 MiB go first as chunked, resumable uploads with progress and *Cancel*, then join the same check-in transaction). Every check-in names the version each file is based on, so the server refuses (409) to overwrite a version somebody else checked in meanwhile. The Workspace tab shows *up to date* / *modified* / *outdated* / *conflict* / *new* / *missing* / *name clash* per file (from the server's file status) with its path, read-only flag and the lock owner. Two items with the same folder and file name are reported, never overwritten. Lock and backup files of the CAD programs are skipped; Creo iterations count as one file. A tracked revision the server no longer shows (you left a restricted project group) is listed as *No longer accessible*: its local copies stay untouched and are never synced again – delete them yourself if you must |
| Get | *Get…* on a revision: the item with its BOM (revision rule *latest* or *released*) and the parts its CAD files reference, resolved by the server (`…/workset`); only missing and outdated files are downloaded (SHA-256, checked after the download), recorded one by one so an interrupted Get continues where it stopped – also inside a large file: the bytes received stay in `<name>.<sha>.teamcraft-download` and the next Get asks only for the rest (`Range` / `If-Range`, `src/resume.rs`), checks the SHA-256 of the whole file and renames it into place; progress and *Cancel* in the status bar. No browser downloads |
| Old workspaces | Folders of the older layout `<group>/<item>/<revision>/` (and `<item>/<revision>/`) are found after the sign-in; the app offers to move the files into the new layout – never overwriting a file, keeping files whose place is taken and listing them, writing every step to `.teamcraft/migration.log` |
| Check in all | *Check in all* in the worklist (navigator and home page): every check-out without local changes is checked in on the server (lock released, one revision at a time); revisions with modified, new or conflicting files stay checked out and are listed |
| Native CAD previews | The preview image stored in Inventor, Solid Edge and SolidWorks files in the Files tab; on Windows *STEP* in the Workspace tab and *Create STEP exports … first* at check-in export with the installed Inventor or Solid Edge (COM automation through Windows PowerShell; uses your own licence) |
| Whole project groups locally | the download icon in the navigator (*Tools → Get Project Groups…*): copies the files of every item of the chosen groups – latest released or latest revision – into the working folder with their folder structure; unchanged files are skipped, locally modified files kept, name clashes reported. For new team members or a fresh computer |
| Import a CAD folder | the folder icon in the navigator (*Tools → Import CAD Folder…*): scans an existing folder (dry run: items, duplicate file names, skipped lock/backup files), then creates one item per part or assembly with the drawings and exports of the same name, project files in one document item, optional BOM lines from the assembly references, and checks every item in right after its files (assemblies once more after their BOM lines), so an interrupted import never leaves items checked out – a new run of the same folder also releases the check-outs an earlier, interrupted run left behind; a final report lists the new items and files, BOM lines, check-ins and problems (with *Open* for the top assembly); interrupted imports continue where they stopped (remembered per project group, so the same folder can also be imported into another group). Tutorial: *Help → How to Import a CAD Folder* |
| New item | *New* (`Ctrl/Cmd+N`): choose the CAD files first – name and type follow from the first file; they are uploaded right after the item is created |
| Release and changes | *Release…* creates and submits a change order – with the unreleased dependencies (BOM children, drawings) by default; *Release readiness* panel with *Add all*; approve/reject from the worklist |
| Change requests | *Request change…* on a revision tab and *Request…* on a file (Files and Workspace tabs) raise a change request prefilled with item, revision and file name, with an optional screenshot or PDF (native file dialog, uploaded by the Rust side); request tabs with discussion, decision (accept / defer / reject), *Create change order*, attachments; *Requests assigned to me* and *My open requests* in the worklist |
| Change propagation | *Dependents to check* grouped by dependent revision (one worklist line with a count, expandable; on the home page **Open**, **Revise**, **Acknowledge all…** per dependent and **Acknowledge…** per change), *Check needed* on the revision tab, references, *As released* rule, impact analysis |
| Notifications | Bell with unread count and toasts; **Watch** / **Unwatch** |
| Ask TeamCraft | **Ask…** in revision and change tabs (only when the server has the assistant enabled): *Draft release notes*, *Check before release*, *Brief me for this review* or a free question; the server answers with Claude and the user's rights, read-only unless drafts are allowed |
| Agent (ACP) | **Agent** in the rail (`Ctrl/Cmd+4`): a local agent you configure under *Settings → Agents…* (Claude Code through its ACP adapter, Gemini CLI, any [Agent Client Protocol](https://agentclientprotocol.com) agent; none by default). The app starts it in the workspace folder and hands it TeamCraft's MCP server with your token. Streamed conversation, tool calls, plan, permission prompts (*Allow once* / *Reject*), *Cancel*, quick prompts from the server's MCP prompts, the open revision or change as context. Through the app the agent may only read text files in the workspace – no writing, no terminal. See [the guide](../docs/src/guide/assistant.md#acp-agents-in-the-desktop-app) |
| Pack and Go, STL | ZIP of a revision and its structure; **Export STL…** from the 3D viewer |
| Appearance | The same skins as the web UI: *TeamCraft* follows the light or dark appearance of the operating system (and switches with it), *TeamCraft Light* and *TeamCraft Dark* stay fixed, the distribution skins are dark. The window chrome (on Linux the GTK menu bar) asks the system for the matching light or dark variant; a GTK theme without a dark variant keeps its light menu bar (the screenshot job sets `GTK_THEME=Adwaita:dark` for its dark run) |

Keyboard: `Ctrl/Cmd+N` new item, `Ctrl/Cmd+F` search, `F5` refresh,
`Ctrl/Cmd+1/2/3/4` Home/Worklist/Changes/Agent, `Ctrl/Cmd+W` close tab; zoom with
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
cargo test                     # settings, API paths, PKCE + loopback sign-in, working-folder layout, Get and migration planning, read-only files
cargo clippy --all-targets -- -D warnings
```

Windows: WebView2 is part of Windows 10/11. On **Windows ARM64** the build needs
the ARM64 MSVC component and LLVM (`clang`, for the `ring` crate) – setup, a
build in a Parallels VM (about 3 minutes) and a server on another computer are
in [platforms/windows.md](../platforms/windows.md).

On the first start enter the server address (`localhost:8080` for a local
server, `host:port` otherwise) and click **Sign in with the browser**: the browser asks you to
confirm the sign-in for this computer, the app receives a personal access
token (valid 90 days, revocable on your account page in the web UI).

Where the app keeps its data:

* settings and the token: the per-user configuration directory
  (`~/.config/io.github.rudalabs.teamcraft`, `~/Library/Application Support/io.github.rudalabs.teamcraft`,
  `%APPDATA%\io.github.rudalabs.teamcraft`); the token file is readable by the user only;
* the working folder: `Documents/TeamCraft/<project group>/<folder>/<file>` with `Documents/TeamCraft/.teamcraft/state.json` (configurable in *Settings*).

## Layout

`build.rs` copies the assets the app shares with the web UI into `ui/static/`
(git-ignored): `static/skins.css`, the fonts, the logos in `media/` and the 3D
viewer.

```text
src/main.rs        Tauri setup, native menu, state
src/commands.rs    the commands the UI calls (every network and file access)
src/api.rs         REST client (/api/v1, bearer token) – the web view never talks to the server
src/signin.rs      browser sign-in: PKCE, loopback listener on 127.0.0.1
src/layout.rs      working-folder rules (pure): folder mapping, state.json, Get planning, migration planning
src/resume.rs      Get resumes a large file half-way: partial file per content, Range/If-Range, SHA-256 check before the rename
src/workspace.rs   Get, status, read-only handling, check-in, migration of the old layout
src/settings.rs    settings.json (incl. the configured agents) and the token file
src/acp.rs         Agent Client Protocol client: agent subprocess, JSON-RPC over stdio, session, permissions, workspace-only reads
ui/                index.html, app.css (layout), app.js (all views)
capabilities/      Tauri permissions of the window (dialogs only – files and URLs are opened by Rust)
icons/             app icons, rendered from icons/app-icon.svg with `tauri icon`
```

The browser sign-in uses the server endpoints `/desktop/authorize` (web page,
confirmation) and `POST /api/v1/desktop/token` (code + PKCE verifier → token);
see `src/services/desktop.rs` in the server.
