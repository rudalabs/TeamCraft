# Sharing UI code between the web UI and the desktop app

TeamCraft has two user interfaces. The web UI renders on the server
(MiniJinja `templates/`, `static/style.css`, `static/app.js` as progressive
enhancement). The desktop app (Tauri 2) renders in the client from REST API
JSON (`desktop/ui/index.html`, `app.css`, `app.js`, about 4,700 lines).
They share only what `desktop/build.rs` copies at build time. This page
records what is duplicated or diverging today, where a top-level `shared/`
folder should draw the line, and how to get there in small,
behaviour-preserving pull requests. Date: 2026-11-05.

## What exists today

| Concern | Web UI | Desktop app | Shared today? |
|---|---|---|---|
| Design tokens, skins | `static/skins.css` | same file, copied by `desktop/build.rs` | **yes** (copy) |
| Fonts (Montserrat, Oswald, OFL texts) | `static/fonts/` | copied | **yes** (copy) |
| Logos, state icons | `media/logos`, `media/icons` | copied | **yes** (copy) |
| 3D viewer (WebGL2, TCMESH01) | `static/viewer.js`, auto-mounts on `#viewer-canvas[data-src]` | same file, `TeamCraftViewer.mount(canvas, {load})` | **yes** (copy), but one classic script with web-only glue at the end |
| Component CSS (badge, menu, table, card/panel, stat, empty, notice, thread, impact group, lifecycle track, assistant text) | `static/style.css` (1,400 lines) | `desktop/ui/app.css` (1,000 lines), re-implemented | no – two copies that drift |
| Density | 14/20 px text; `--ctl-h`, `--row-h` from `skins.css` (32 px, `data-density="compact"` 28 px) | 13/18 px text; own `--d-ctl`, `--d-ctl-sm`, `--d-row` in `app.css` | tokens shared, but the desktop uses its own set and both set text sizes literally |
| State badges | macros `badge`, `glyph_char`, `state_glyph`: text glyph (✓ ○ × – ◐ ▲) plus label | `badge()` in `app.js`: SVG images (`Green_tick.svg` …) for some states, no glyph for `in_work` | no – **diverging** |
| Labels (`in_work` → WIP) | `filter_label` (Rust) | `label()` (JS) | no – two implementations of one table |
| Dates, sizes, numbers | `filter_dt` (UTC, then `app.js` rewrites `<time>` into local time), `filter_age`, `filter_num`, `filter_fixed`, `filter_filesize` | `fmtDate` (local), `fmtSize`, `fmtNum` | no – same rules, two languages |
| Lifecycle stepper | `ol.lifecycle-track` with `done` / `current` / `future` and screen-reader text | `ol.lifecycle-track`, only `current` marked, every state a coloured badge | no – **diverging** |
| BOM tree | server-rendered table, `.lvl`, `.indent-N`, `.tree-guide` | client tree with twisties, expand-all | no |
| Markdown-lite (assistant answers) | `assistant::render_markdown` (Rust, escapes everything) | `markdownLite()` (JS, DOM nodes, never `innerHTML`) | no – two parsers of one dialect |
| Menus and keyboard | `<details class="menu">` + `app.js` (one open, Esc, ↑/↓), `/` and Ctrl+K focus search, ←/→ on tabs | `moreMenu()` with its own keyboard handling, Ctrl+F search | no – same behaviour written twice |
| Line icons | inline SVG in templates and macros (`eye`, `star`, lock) | `ICONS` path table in `app.js` | no |
| Skin list | `domain::SKINS` (Rust) | `SKINS` in `app.js` | checked by a unit test (`src/domain.rs`) |

Guards that already exist and keep working:

- `assets::tests::css_tokens_are_defined` reads `skins.css`, `style.css` and
  `desktop/ui/app.css` and fails on an undefined `var(--x)`.
- `domain` tests: skin ids in `skins.css` and in the desktop `SKINS` list,
  contrast of every token, identical dark-token blocks.
- `.github/workflows/desktop.yml`, job `ui`: `node --check` over
  `desktop/ui/*.js` and `static/*.js`. The runner's Node.js only parses the
  files; the product has no Node.js dependency.
- Both apps already use the **same URL layout**: the server serves
  `/static/skins.css`, and the desktop's `frontendDist` is `desktop/ui`, so
  `/static/skins.css` there is `desktop/ui/static/skins.css` (git-ignored copy).
  A shared file can keep one URL in both apps.

## How others do it

- **Design-system packages** (GitHub Primer, GOV.UK Frontend): one package of
  tokens plus component CSS with a documented *markup contract*; server
  templates (Rails ViewComponents, Nunjucks macros) and client components
  emit the same class names. The CSS is the shared artefact, not the
  rendering code. GOV.UK Frontend ships its JS as plain ES modules that
  enhance server-rendered markup.
- **Web components / Lit** (used by many Tauri and Electron apps): one
  component implementation for both, but a server-rendered page then needs
  client-side rendering or declarative shadow DOM – a second rendering model
  for the web UI, and shadow DOM fights a shared global stylesheet.
- **Tauri apps that load the server's pages** (remote URL in the webview):
  one UI, but loses offline state, local files and native dialogs, and makes
  the desktop app's CSP depend on the server. Not an option for TeamCraft's
  check-out workspace.

TeamCraft follows the first model: **one CSS, one markup contract, two
renderers**, and shared JS only where the code is the same in both apps
(formatters, widgets that enhance existing markup, the viewer).

## Decision: the boundary

### Shared (`shared/`)

```text
shared/
  README.md            the rules below, the markup contract per component
  css/
    skins.css          tokens and skins (moved from static/, unchanged)
    components.css     badge, state glyph, lock, priority, button, input,
                       card/panel, table, tree rows, lifecycle track, stat,
                       empty, notice/flash, menu pop-up, dialog surface,
                       thread entry, impact group, assistant text, viewer bar
                       and legend
  fonts/               Montserrat, Oswald + OFL texts (moved from static/fonts)
  icons/sprite.svg     line icons as <symbol id="…">, used with <use href>
  js/
    format.js          dt (local), age, num, fixed, filesize, label
    states.js          state → glyph, badge class, tone; priority glyphs
    dom.js             h(), append(), clear() – text only, never innerHTML
    markdown.js        markdown-lite → DOM nodes (from desktop markdownLite)
    menu.js            menu keyboard model: one open, Esc, ↑/↓, outside click
    tabs.js            ←/→ between tabs
    tree.js            expand / collapse / expand-all over tree rows
    viewer.js          WebGL2 TCMESH01 viewer, `export function mount`
  test-vectors/        JSON input → expected output, read by Rust and JS tests
```

Rules for everything in `shared/`:

1. **Plain ES modules**, no bundler, no transpiler, no Node.js packages.
   `<script type="module">` is allowed by `script-src 'self'` in both CSPs
   (server `src/lib.rs`, desktop `tauri.conf.json`). No inline scripts or
   styles, no `innerHTML` with data, no `eval`.
2. **No knowledge of where data comes from.** Shared JS never calls `fetch`
   or Tauri `invoke`; it takes data and callbacks (`viewer.mount(canvas,
   { load })` already works like that). So the web's cookie session and the
   desktop's Rust commands stay app-specific.
3. **Enhance, don't own.** Widgets (`menu.js`, `tabs.js`, `tree.js`) attach to
   markup that exists – server-rendered on the web, built by `dom.js` on the
   desktop – and the web keeps working without JavaScript.
4. **Density by tokens.** Components use the existing `--ctl-h`,
   `--ctl-h-sm`, `--row-h`, `--tree-row-h` plus new `--fs-body` and
   `--lh-body`; the web keeps the defaults (14/20/32), the desktop sets
   `data-density="compact"` and 13/18 px text instead of its own `--d-*`
   tokens. One component CSS, two densities.
5. **Tolerant of version skew.** The desktop app ships its copy of
   `shared/` and talks to servers of other versions: an unknown state gets
   the generic label and no glyph, never an exception.

### Stays in the web UI

Server-rendered pages and the MiniJinja macros that emit the shared markup;
forms with POST → redirect → GET and flash cookies; CSRF origin check;
chunked uploads in `static/app.js`; admin pages, setup, OIDC sign-in, the
desktop authorization page (`templates/desktop/`); the server-rendered
assistant panel; print styles; page layout (top bar, page head, grids).

### Stays in the desktop app

The shell (rail, navigator, object tabs, toolbar, status bar, splitter);
workspace, check-out / check-in, local files and the migration dialog;
native file dialogs and progress for packages; the ACP agent panel; offline
and connection state, server switching and token sign-in; Tauri commands
(`invoke`); dialogs with their own focus management.

### Rendering model

The web UI keeps server templates; the desktop app keeps rendering from
JSON. Neither is forced onto the other. What makes them look identical is
the markup contract: `shared/README.md` lists, per component, the DOM the
CSS expects (for example `span.badge.badge-<state> > span.glyph[aria-hidden]`
+ label). A MiniJinja macro produces it on the web, a small builder in the
desktop (`dom.js` + `states.js`) produces it on the desktop.

Rejected alternatives: desktop views rendered by the server (loses offline
work and the desktop CSP), web pages rendered client-side from the API
(breaks the no-JavaScript web UI and doubles the API surface the web
depends on), web components with shadow DOM (needs the second rendering
model on the web and isolates the shared stylesheet).

### Rust and JS side by side

Some rules have to exist twice, because the server renders without JS and
the desktop renders without the server: labels, glyphs, number and date
formats, markdown-lite. They are not merged; they are **pinned to the same
test vectors** in `shared/test-vectors/*.json` (for example
`{"label": [["in_work", "WIP"], ["in_review", "In Review"], …]}`):

- Rust unit tests (`include_str!`) assert `filter_label`, `glyph_char`,
  `format_bytes`, `render_markdown` against the vectors, in `cargo test --lib`.
- The CI job that already runs `node --check` also runs
  `node --test shared/js/*.test.js` with the same vectors. The test files
  are not served and not copied into the desktop app.

The skin list already follows this pattern (a Rust test reads
`desktop/ui/app.js`); it moves to `shared/js/states.js`-style data checked
the same way.

## Serving and cache busting

**Server.** `src/assets.rs` embeds `shared/` under `/static/shared/…`. To
avoid a hand-written list going stale, the server `build.rs` (which already
watches `static` and `media`) walks `shared/` and writes the asset table for
it into `OUT_DIR`; `assets.rs` includes it next to the explicit `static/`
entries. Test files (`*.test.js`), `test-vectors/` and `README.md` are
skipped.

Cache busting today appends `?v=<asset hash>` in the templates. ES modules
import each other with relative URLs (`import { label } from "./format.js"`),
which drop the query string. An inline import map would need a CSP hash.
So shared modules are served under a **versioned path**:
`/static/shared/v/<asset version>/js/viewer.js`. Relative imports resolve
inside the same versioned directory and are cacheable as `immutable`; a
request for any other version is served as today with `no-cache` + `ETag`,
so an outdated page still works. CSS and the sprite keep `?v=`.

**Desktop.** `desktop/build.rs` copies `shared/` recursively to
`desktop/ui/static/shared/` (already git-ignored as part of
`desktop/ui/static/`), skipping the same test files. No versioning – the
files are bundled. `index.html` links `/static/shared/css/…` and loads
`app.js` as a module.

**Workflows.** `ci.yml` builds on any non-doc change, so it covers
`shared/**`; `desktop.yml` adds `shared/**` to its `paths` filters.

## Tests that guard the split

- **Same files in both apps:** a server unit test walks `shared/` and
  asserts every servable file is an asset at `/static/shared/…` and nothing
  else is; a desktop test (`cargo test --manifest-path desktop/Cargo.toml`)
  asserts `ui/static/shared/` mirrors `shared/` byte for byte after the
  build script ran.
- **Tokens:** `css_tokens_are_defined` gets `shared/css/*.css` added, and a
  new check that `components.css` uses no literal colours or sizes outside
  the density tokens.
- **Markup contract:** a Rust test renders each macro in `macros.html` with
  the vectors and checks the class structure the contract lists; the JS
  test does the same for the builders with a string-building `h()`.
- **Parity of rules:** the test vectors above, run by `cargo test` and
  `node --test`.
- **Syntax:** `node --check` over `shared/js/*.js`. Node treats `.js` as
  CommonJS without a `package.json`, so the check runs as
  `node --input-type=module --check < "$f"` (to be confirmed in the first
  PR; `.mjs` is the fallback, which then needs the `text/javascript` type in
  `assets.rs`).
- **No inline code:** the existing CSP stays the test – any inline script
  or style in shared markup fails in both apps.

## Licence headers

The repository has no per-file headers today; `LICENSE` covers it. Files in
`shared/` start with a one-line `/* SPDX-License-Identifier: MPL-2.0 */`
because they are copied into another program's bundle, where the origin is
otherwise lost. The fonts keep their OFL texts next to them
(`shared/fonts/OFL-*.txt`), which the desktop *About* dialog already cites.
A unit test checks the SPDX line on every `.css`, `.js` and `.svg` in
`shared/`.

## Migration plan

Small pull requests, each behaviour-preserving unless it says otherwise,
each with before/after screenshots of both apps in the description.

1. **Folder and plumbing.** Create `shared/` with `README.md`; move
   `skins.css`, `fonts/` and `viewer.js` (still a classic script) into it.
   Server serves `/static/shared/…`, build-script asset table,
   `desktop/build.rs` copies the tree, CI paths, the "same files" tests,
   SPDX test. The `@font-face` URLs in `skins.css` become
   `/static/shared/fonts/…`; old `/static/skins.css` URLs are no longer
   linked anywhere. No visual change.
2. **Density tokens.** Add `--fs-body` and `--lh-body`, replace the
   desktop's `--d-ctl` / `--d-ctl-sm` / `--d-row` with the shared
   `--ctl-h` / `--ctl-h-sm` / `--row-h` under `data-density="compact"`, and
   replace literal sizes in both app stylesheets. No visual change.
3. **`components.css`, one family per PR:** badge, state glyph, lock and
   priority first; then tables and tree rows; card/panel, stat, empty,
   notice; menu pop-up; lifecycle track; thread entry, impact group,
   assistant text, viewer bar and legend. Each PR deletes the duplicate from
   `style.css` and `app.css`. The badge PR is the one **visible** change:
   the desktop adopts the web's text glyphs instead of the SVG state icons,
   so colour plus glyph means the same in both apps; the lifecycle-track PR
   gives the desktop the `done` / `current` / `future` steps.
4. **Formatters and states.** `shared/js/format.js`, `states.js`, `dom.js`,
   test vectors with the Rust tests and `node --test`. The desktop `app.js`
   becomes a module and imports them; the web `app.js` uses `format.js` for
   the local-time rewrite of `<time>`.
5. **Viewer as a module.** `viewer.js` exports `mount`; the web auto-mount
   glue moves to `static/app.js`; versioned module path on the server.
6. **Markdown-lite.** The desktop `markdownLite` moves to
   `shared/js/markdown.js`; Rust `render_markdown` and the JS version share
   `test-vectors/markdown.json`.
7. **Widgets and icons.** `menu.js` (one keyboard model for `<details
   class="menu">` and the desktop "More" menu), `tabs.js`, `tree.js` (the
   web BOM page gains collapse as an enhancement), `icons/sprite.svg`
   replacing inline icon paths in templates and the desktop `ICONS` table.

Not planned: sharing page layouts, the desktop shell, forms, or any data
access code.

## Open questions

- The desktop's SVG state icons (`media/icons/Green_tick.svg` …): drop them
  for the text glyphs (recommended, step 3) or keep them as a desktop-only
  option?
- Should a living component gallery exist (a docs page or a hidden `/ui`
  page rendering every component through the macros), so reviewers compare
  both apps without starting the desktop app?

## Sources

- Repository state at `f8f5322`: `src/assets.rs`, `src/templates.rs`,
  `templates/macros.html`, `static/*`, `desktop/build.rs`,
  `desktop/tauri.conf.json`, `desktop/ui/*`, `.github/workflows/desktop.yml`.
- GOV.UK Frontend – Nunjucks macros and ES modules over one component CSS:
  <https://frontend.design-system.service.gov.uk/>
- Primer (GitHub) – shared CSS for Rails view components and React:
  <https://primer.style/>
- MDN – JavaScript modules, `<script type="module">`, import maps and CSP:
  <https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide/Modules>
- Tauri 2 – `frontendDist` and the CSP configuration:
  <https://v2.tauri.app/security/csp/>
