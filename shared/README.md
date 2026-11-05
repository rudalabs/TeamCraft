# shared/ – UI files of both the web UI and TeamCraft Desktop

Everything in this folder is used by **both** user interfaces: the
server-rendered web UI and the desktop app (Tauri). The design behind it is the
research page *Sharing UI code between the web UI and the desktop app*
([`docs/src/research/shared-ui.md`](../docs/src/research/shared-ui.md)): one CSS, one markup contract, two renderers.

```text
css/skins.css   design tokens and skins (colours, radii, spacing, density, fonts)
css/assistant.css  the "Ask TeamCraft" drawer (inside of the <aside>; each shell places it)
fonts/          Montserrat, Oswald (variable WOFF2) and their SIL OFL texts
js/viewer.js    3D preview (WebGL2, TCMESH01), still a classic script
js/assistant.js "Ask TeamCraft" sidebar: ES module, mount(host, {api, getContext, link, …}) – no fetch, no invoke
```

## How the apps get these files

* **Server:** `build.rs` walks this folder and writes the asset table into
  `OUT_DIR`; `src/assets.rs` embeds every file and serves it at
  `/static/shared/<path>`. Stylesheets are linked with `?v=<assets version>`,
  scripts through the versioned path `/static/shared/v/<assets version>/js/…`
  (relative module imports keep the version). No list to maintain by hand.
* **Desktop:** `desktop/build.rs` mirrors this folder into
  `desktop/ui/static/shared/` (git-ignored), so the app loads the same URLs.
* Not served and not copied: this README, `*.test.js`, `test-vectors/` and
  dotfiles.
* Guard tests: `assets::tests::shared_files_are_served` (server) and
  `shared_ui_files_are_mirrored` (`cargo test --manifest-path
  desktop/Cargo.toml`) check that both apps carry exactly these files.

## Rules

1. **Licence header.** Every `.css`, `.js` and `.svg` starts with
   `/* SPDX-License-Identifier: MPL-2.0 */` (SVG: `<!-- … -->`), because the
   files end up in another program's bundle. The fonts keep their OFL texts
   next to them. Checked by `assets::tests::shared_files_carry_spdx_header`.
2. **Plain files, no build step.** No bundler, transpiler or Node.js
   packages; new scripts are plain ES modules. No inline scripts or styles,
   no `innerHTML` with data, no `eval` – both Content Security Policies
   (`src/lib.rs`, `desktop/tauri.conf.json`) enforce it.
3. **No data access.** Shared code never calls `fetch` or Tauri `invoke`; it
   takes data and callbacks (`TeamCraftViewer.mount(canvas, { load })`).
4. **Tokens, not literals.** Colours exist only in `css/skins.css`; component
   styles read the tokens. Density is a token set too: the web UI uses the
   defaults (14/20 px text, 32 px controls and rows), the desktop app sets
   `data-density="compact"` on `<html>` (13/18 px text, 28 px controls and
   rows, 24 px small controls): `--fs-body`, `--lh-body`, `--ctl-h`,
   `--ctl-h-sm`, `--row-h`.
5. **Tolerant of version skew.** The desktop app ships its own copy and talks
   to servers of other versions: unknown values get a generic fallback, never
   an exception.
6. **Syntax check.** CI (`.github/workflows/desktop.yml`, job `ui`) parses
   every script here with `node --input-type=module --check`; Node.js is used
   by CI only.

## The assistant sidebar (`js/assistant.js`, `css/assistant.css`)

The first shared *component*: one module, one stylesheet, two shells.

* **Contract.** The shell provides an empty `<aside>` and the callbacks; the
  module builds the drawer inside it (context chip, quick actions, the
  conversation, the composer). The shell owns the icon that opens it, the
  place of the `<aside>` (web: fixed on the right below the top bar; desktop:
  a grid column beside the tabs) and every data access:
  `api.thread()`, `api.send(body, signal)`, `api.reset()`, `getContext()`.
* **Page context.** The page (web: `data-tc-*` attributes on `<main>`;
  desktop: the active tab) declares a plain object – `kind`, `item`,
  `revision`, `change`, `request`, `project`, `view`, `rule`, `query`,
  `filters`, `selected`, `shown` – described in
  `docs/src/guide/assistant.md` (API section). It is only a hint; the server
  resolves it with the user's rights.
* **State.** Open/closed and the width are kept in `localStorage`
  (`tc.assistant.open`, `tc.assistant.width`), always inside `try/catch`.
* **Answers** are markdown-lite built as DOM nodes (`markdown(text, link)`),
  never as HTML.
