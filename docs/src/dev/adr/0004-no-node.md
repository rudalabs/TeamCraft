# ADR 0004 – No Node.js and no bundler in the product

**Status:** accepted – a hard rule of the project (`CLAUDE.md`, *Pure Rust*),
restated for the shared UI files in
[Sharing UI code between the web UI and the desktop app](../../research/shared-ui.md)
and `shared/README.md`.

## Context

TeamCraft is a single binary (axum server, REST API, CLI) plus a Tauri desktop
app. Both have user interfaces in HTML, CSS and JavaScript, and both need a
3D viewer and shared styles.

## Decision

* **Plain files, no build step.** The JavaScript of the web UI (`static/`),
  the desktop app (`desktop/ui/`) and the shared files (`shared/`) is written
  as plain scripts and ES modules: no bundler, no transpiler, no Node.js
  packages.
* **Rust embeds and serves them.** `build.rs` walks `shared/` and writes the
  asset table; `src/assets.rs` embeds every file and serves it with cache
  busting (`?v=<assets version>`, versioned script paths). `desktop/build.rs`
  mirrors `shared/` into the desktop app.
* **3D conversion stays in Rust** (`src/mesh.rs`); no downloaded engines.
* **Node.js only checks, never ships.** The CI job `ui` in
  `.github/workflows/desktop.yml` runs `node --check` over the scripts (syntax
  only); the product has no Node.js dependency.

## Consequences

* Everything builds with `cargo` alone (`CONTRIBUTING.md`: "No Node.js, no
  downloaded engines and no C/C++ CAD kernels"); the release builds no
  JavaScript.
* No `innerHTML` with data, no `eval`, no inline scripts or styles – both
  Content Security Policies (`src/lib.rs`, `desktop/tauri.conf.json`) enforce
  it, and plain modules fit `script-src 'self'`.
* Logic that both user interfaces need (labels, dates, markdown-lite) exists
  in Rust and in JavaScript; the research note plans shared test vectors to
  keep the two in step (not in place yet).
* JavaScript libraries from npm cannot be used as they are; what the UI needs
  is written in the repository.

## Alternatives turned down

The reason for the rule itself is not documented beyond the rule: `CLAUDE.md`
and `CONTRIBUTING.md` state it ("everything builds with `cargo` alone")
without a rationale, and the
research note adopts "no bundler, no transpiler, no Node.js packages" for
`shared/` as a given. No alternative (a bundled front-end build, a framework)
is recorded as evaluated.

## Where it is held

| What | Where |
|------|-------|
| The rule | `CLAUDE.md` (*Pure Rust*), `CONTRIBUTING.md`, `shared/README.md` |
| Embedding and serving | `build.rs`, `src/assets.rs` |
| Every shared file served and mirrored, with its licence header | `assets::tests::shared_files_are_served`, `assets::tests::shared_files_carry_spdx_header`, `shared_ui_files_are_mirrored` (desktop) |
| Scripts parse | job `ui` in `.github/workflows/desktop.yml` (`node --check`) |
