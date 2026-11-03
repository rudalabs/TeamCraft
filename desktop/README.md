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

## Features

| Area | What the desktop app does |
|---|---|
| Sign-in | **Sign in with the browser** (single sign-on or password, confirmed on a TeamCraft page) or a personal access token; *cancel* stops the sign-in at once |
| Navigator | Project groups (mine first) → items; *My Worklist* (check-outs, reviews, my open changes); change orders by status; full-text search |
| Revision tabs | Overview, Files (versions, restore, open), Structure (multi-level BOM, revision rules, roll-up), Where used, Changes, History, **3D viewer** (STEP, meshes, point clouds – converted by the server), Workspace |
| Check-out / check-in | *Check out* takes the lock and copies the files into `<workspace>/<item>/<revision>/`; the Workspace tab shows *modified* / *new* / *missing* files (SHA-256); *Check in…* uploads the chosen files as new versions and releases the lock in one step. Lock and backup files of the CAD programs are skipped; Creo iterations count as one file |
| Release and changes | *Release…* creates and submits a change order; approve/reject from the worklist |
| Change propagation | *Dependents to check*, *Check needed* banner with **Acknowledge…**, references, *As released* rule, impact analysis |
| Notifications | Bell with unread count and toasts; **Watch** / **Unwatch** |
| Pack and Go, STL | ZIP of a revision and its structure; **Export STL…** from the 3D viewer |
| Skins | The same skins as the web UI |

Keyboard: `Ctrl/Cmd+N` new item, `Ctrl/Cmd+F` search, `F5` refresh,
`Ctrl/Cmd+1/2/3` Home/Worklist/Changes, `Ctrl/Cmd+W` close tab; zoom with
`Ctrl` + mouse wheel or `Ctrl/Cmd` + `+` / `-` / `0`.

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
* checked-out files: `Documents/TeamCraft/<item>/<revision>/` (configurable in *Settings*).

## Layout

`build.rs` copies the assets the app shares with the web UI into `ui/static/`
(git-ignored): `static/skins.css`, the fonts, the logos in `media/` and the 3D
viewer.

```text
src/main.rs        Tauri setup, native menu, state
src/commands.rs    the commands the UI calls (every network and file access)
src/api.rs         REST client (/api/v1, bearer token) – the web view never talks to the server
src/signin.rs      browser sign-in: PKCE, loopback listener on 127.0.0.1
src/workspace.rs   check-out folders, SHA-256 comparison, check-in upload
src/settings.rs    settings.json and the token file
ui/                index.html, app.css (layout), app.js (all views)
capabilities/      Tauri permissions of the window (dialogs only – files and URLs are opened by Rust)
icons/             app icons, rendered from icons/app-icon.svg with `tauri icon`
```

The browser sign-in uses the server endpoints `/desktop/authorize` (web page,
confirmation) and `POST /api/v1/desktop/token` (code + PKCE verifier → token);
see `src/services/desktop.rs` in the server.
