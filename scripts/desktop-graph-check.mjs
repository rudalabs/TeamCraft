// SPDX-License-Identifier: MPL-2.0
// Browser check of the desktop Graph sub-tab (desktop/ui/js/graph.js) in a
// real WebKit (WKWebView/WebKitGTK engine) and in Chromium (WebView2 engine),
// plus a load of the whole desktop module graph. Needs Playwright with its
// browsers (`npx playwright install webkit chromium`); Node.js only checks,
// it never ships (ADR 0004).
//
//   node scripts/desktop-graph-check.mjs [screenshot.png]
//
// The real graph.js, app.css and shared/css/skins.css are served with stub
// modules for core.js, navigator.js and tabs.js; the fake tab is an assembly
// with 30 BOM children, 3 references and 3 referrers (rows layout, above
// ARC_MAX). Checks, per browser:
//   * every card's centre is within 3 px of its edge end – before and after a
//     wheel zoom and a drag pan – and no two cards share a position;
//   * the wheel zooms around the cursor;
//   * clicking an assembly card re-centres the graph and the back control
//     says "← Back"; clicking a part opens its overview with returnToGraph;
//   * no page errors.
// Then desktop/ui/index.html is loaded with a stubbed window.__TAURI__ and
// must load every module without an error.
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);
let playwright;
try {
  playwright = require("playwright");
} catch {
  console.error("Playwright is not installed: npm i -D playwright && npx playwright install webkit chromium");
  process.exit(2);
}
const shot = process.argv[2] || path.join(root, "target", "graph-webkit.png");

// ---------------------------------------------------------------------------
// The stub modules and the harness page
// ---------------------------------------------------------------------------

const CORE = `
export function enc(s) { return encodeURIComponent(s); }
export function label(c) { return c; }
export function invoke() { return Promise.reject(new Error("no thumbnails in the harness")); }
export function append(el, children) {
  if (children === null || children === undefined || children === false) return;
  if (!Array.isArray(children)) children = [children];
  children.forEach(function (c) {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) append(el, c);
    else el.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
  });
}
export function h(spec, attrs, children) {
  var parts = spec.split(".");
  var el = document.createElement(parts[0] || "div");
  if (parts.length > 1) el.className = parts.slice(1).join(" ");
  if (attrs) Object.keys(attrs).forEach(function (k) {
    var v = attrs[k];
    if (v === null || v === undefined || v === false) return;
    if (k === "on") Object.keys(v).forEach(function (ev) { el.addEventListener(ev, v[ev]); });
    else if (k === "text") el.textContent = v;
    else if (k === "class") el.className = (el.className ? el.className + " " : "") + v;
    else el.setAttribute(k, v === true ? "" : String(v));
  });
  append(el, children);
  return el;
}
// A fake server: ASM-000004 has 30 BOM lines (every fifth an assembly),
// every other assembly 4.
function bomOf(number, count) {
  var out = [];
  for (var i = 0; i < count; i++) {
    var n = "PRT-" + number.slice(-2) + "-" + String(i).padStart(2, "0");
    if (number === "ASM-000004") n = (i % 5 === 0 ? "ASM-" : "PRT-") + "1-" + String(i).padStart(2, "0");
    out.push({ child_number: n, child_latest_code: "A", child_latest_state: ["in_work", "released", "in_review"][i % 3],
               child_type: i % 5 === 0 && number === "ASM-000004" ? "assembly" : "part", child_name: "Part " + i, quantity: 1 + (i % 3) });
  }
  return out;
}
export function centerData(number) {
  return {
    rev: { item_number: number, code: "A", name: number + " assembly", item_type: "assembly", state: "in_work" },
    files: [], bom: bomOf(number, number === "ASM-000004" ? 30 : 4), changes: [], item: {}, revisions: [],
    refs: {
      outgoing: number === "ASM-000004" ? [0, 1, 2].map(function (i) {
        return { to_number: "DOC-00" + i, to_latest_code: "A", to_latest_state: "released", to_name: "Drawing " + i };
      }) : [],
      incoming: number === "ASM-000004" ? [0, 1].map(function (i) {
        return { from_number: "ASM-9" + i, from_code: "B", from_state: "released", from_name: "Top " + i };
      }) : [],
      files: [],
      file_referrers: number === "ASM-000004" ? [{ item_number: "ASM-99", revision_code: "A", revision_state: "in_review", item_name: "Frame", filename: "frame.iam" }] : [],
      impacts: []
    }
  };
}
export function api(method, p) {
  var m = p.match(/^\\/items\\/([^/]+)(?:\\/revisions\\/([^/]+))?(\\/references)?$/);
  if (!m) return Promise.reject(new Error("unexpected " + p));
  var number = decodeURIComponent(m[1]);
  var d = centerData(number);
  if (m[3]) return Promise.resolve(d.refs);
  if (!m[2]) return Promise.resolve({ item: {}, revisions: [] });
  var asm = number.startsWith("ASM");
  return Promise.resolve({
    revision: { item_number: number, code: "A", name: number, item_type: asm ? "assembly" : "part", state: "in_work" },
    files: [], bom: asm ? bomOf(number, 4) : [], changes: []
  });
}
`;

const NAVIGATOR = `export function typeIcon(t) { return t === "assembly" ? "▣" : "◇"; }`;

const TABS = `
import { centerData } from "./core.js";
import { revGraph, graphBackControl } from "./graph.js";
export var opened = [];
window.__opened = opened;
var current = null;
export function findTab() { return null; }
export function activate() {}
export function openTab(t) { opened.push(t); }
export function renderActive() { render(current); }
// like tabs.js: the tab becomes the other item's tab in place
export function retargetTab(tab, number, code, title, fields) {
  tab.key = "rev:" + number + ":" + code; tab.number = number; tab.code = code; tab.title = title;
  tab.data = null;
  Object.keys(fields || {}).forEach(function (k) { tab[k] = fields[k]; });
  window.__tab = tab;
  render(tab);
}
export function render(tab) {
  current = tab;
  var host = document.getElementById("host");
  host.textContent = "";
  var back = graphBackControl(tab);
  var row = document.createElement("div");
  row.id = "back";
  if (back) row.appendChild(back);
  host.appendChild(row);
  revGraph(tab, host);
}
export function start() {
  var tab = { key: "rev:ASM-000004:A", kind: "revision", number: "ASM-000004", code: "A", sub: "graph",
              data: centerData("ASM-000004") };
  window.__tab = tab;
  render(tab);
}
`;

const PAGE = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<link rel="stylesheet" href="/skins.css"><link rel="stylesheet" href="/app.css">
<style>body { margin: 0; padding: 16px; width: 1200px; } #host { width: 1100px; }</style>
</head><body><div id="host"></div>
<script type="module">
import { start } from "/js/tabs.js";
start();
</script></body></html>`;

function serve(routes) {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const url = req.url.split("?")[0];
      const found = routes(url);
      if (!found) { res.writeHead(404); res.end(); return; }
      const [type, body] = found;
      res.writeHead(200, { "content-type": type });
      res.end(body);
    });
    server.listen(0, "127.0.0.1", () => resolve(server));
  });
}

const file = (p) => fs.readFileSync(path.join(root, p));
const JS = "text/javascript; charset=utf-8";
const CSS = "text/css; charset=utf-8";

function graphRoutes(url) {
  switch (url) {
    case "/": return ["text/html; charset=utf-8", PAGE];
    case "/app.css": return [CSS, file("desktop/ui/app.css")];
    case "/skins.css": return [CSS, file("shared/css/skins.css")];
    case "/js/graph.js": return [JS, file("desktop/ui/js/graph.js")];
    case "/js/core.js": return [JS, CORE];
    case "/js/navigator.js": return [JS, NAVIGATOR];
    case "/js/tabs.js": return [JS, TABS];
    default: return null;
  }
}

// The whole desktop UI: desktop/ui with the shared files at /static/shared.
function desktopRoutes(url) {
  const types = { ".html": "text/html; charset=utf-8", ".js": JS, ".css": CSS, ".svg": "image/svg+xml",
                  ".woff2": "font/woff2", ".png": "image/png" };
  let p = url === "/" ? "desktop/ui/index.html" : url.startsWith("/static/shared/")
    ? path.join("shared", url.slice("/static/shared/".length))
    : path.join("desktop/ui", url);
  const abs = path.join(root, p);
  if (!abs.startsWith(root) || !fs.existsSync(abs) || fs.statSync(abs).isDirectory()) return null;
  return [types[path.extname(abs)] || "application/octet-stream", fs.readFileSync(abs)];
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

/** Card centres and the edge ends in page coordinates. */
function geometry(page) {
  return page.evaluate(() => {
    const view = document.querySelector(".g-view");
    const ctm = view.getScreenCTM();
    const at = (x, y) => { const p = new DOMPoint(x, y).matrixTransform(ctm); return [p.x, p.y]; };
    const ends = [...document.querySelectorAll(".g-edge")].map((l) => {
      const x1 = +l.getAttribute("x1"), y1 = +l.getAttribute("y1");
      const x2 = +l.getAttribute("x2"), y2 = +l.getAttribute("y2");
      // the centre sits at the scene origin; the other end is the node
      return Math.abs(x1) + Math.abs(y1) < 1e-6 ? at(x2, y2) : at(x1, y1);
    });
    ends.push(at(0, 0)); // the centre card
    const cards = [...document.querySelectorAll(".g-card")].map((c) => {
      const r = c.getBoundingClientRect();
      return [r.left + r.width / 2, r.top + r.height / 2];
    });
    return { ends, cards };
  });
}

function aligned(g, label, fail) {
  if (g.cards.length !== g.ends.length) fail(`${label}: ${g.cards.length} cards for ${g.ends.length} edge ends`);
  const keys = new Set(g.cards.map(([x, y]) => `${Math.round(x)},${Math.round(y)}`));
  if (keys.size !== g.cards.length) fail(`${label}: ${g.cards.length - keys.size} cards share a position`);
  let worst = 0;
  for (const [cx, cy] of g.cards) {
    const d = Math.min(...g.ends.map(([x, y]) => Math.hypot(x - cx, y - cy)));
    worst = Math.max(worst, d);
  }
  if (worst > 3) fail(`${label}: a card is ${worst.toFixed(1)} px away from its edge end`);
  return worst;
}

async function checkGraph(browserType, name) {
  const failures = [];
  const fail = (m) => failures.push(`${name}: ${m}`);
  const server = await serve(graphRoutes);
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await browserType.launch();
  const page = await browser.newPage({ viewport: { width: 1240, height: 980 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  await page.goto(base + "/");
  await page.waitForFunction(() => document.querySelectorAll(".g-card").length === 37, null, { timeout: 15000 });
  await page.waitForTimeout(600); // enrichment and the refit settle
  const count = await page.locator(".g-card").count();
  if (count !== 37) fail(`${count} cards, expected 37 (30 BOM + 3 references + 3 referrers + centre)`);
  const w0 = aligned(await geometry(page), "first paint", fail);
  if (browserType.name() === "webkit") await page.screenshot({ path: shot, fullPage: true });

  // quantity chip: every BOM child shows ×n top-left (the fake BOM has 1 + i % 3);
  // references, referrers and the centre show none
  const qty = await page.evaluate(() => Array.prototype.map.call(document.querySelectorAll(".g-node"), (n) => {
    const chip = n.querySelector(".g-qty");
    const box = chip && chip.getBoundingClientRect(), card = n.getBoundingClientRect();
    return { sub: n.querySelector(".g-sub").textContent, chip: chip ? chip.textContent : null,
             topLeft: !!box && box.left - card.left < 12 && box.top - card.top < 12 };
  }));
  const bomChildren = qty.filter((q) => /^(ASM|PRT)-1-\d\d/.test(q.sub));
  if (bomChildren.length !== 30) fail(`${bomChildren.length} BOM children found, expected 30`);
  bomChildren.forEach((q) => {
    const i = Number(q.sub.match(/-(\d\d)\//)[1]);
    if (q.chip !== "×" + (1 + (i % 3))) fail(`${q.sub}: quantity chip ${q.chip}, expected ×${1 + (i % 3)}`);
    else if (!q.topLeft) fail(`${q.sub}: quantity chip is not in the top-left corner`);
  });
  const others = qty.filter((q) => !/^(ASM|PRT)-1-\d\d/.test(q.sub) && q.chip !== null);
  if (others.length) fail(`quantity chip on a non-BOM node: ${others.map((q) => q.sub).join(", ")}`);

  // wheel zoom around the cursor: the scene point under the cursor stays put
  const pane = await page.locator(".graph-pane").boundingBox();
  const cx = pane.x + pane.width * 0.7, cy = pane.y + pane.height * 0.3;
  const before = await page.evaluate(([x, y]) => {
    const m = document.querySelector(".g-view").getScreenCTM().inverse();
    const p = new DOMPoint(x, y).matrixTransform(m); return [p.x, p.y];
  }, [cx, cy]);
  await page.mouse.move(cx, cy);
  await page.mouse.wheel(0, -400);
  await page.waitForTimeout(200);
  const after = await page.evaluate(([x, y]) => {
    const p = new DOMPoint(x, y).matrixTransform(document.querySelector(".g-view").getScreenCTM());
    return [p.x, p.y];
  }, before);
  if (Math.hypot(after[0] - cx, after[1] - cy) > 2) fail("the wheel did not zoom around the cursor");
  const w1 = aligned(await geometry(page), "after the wheel zoom", fail);

  // drag pan from an empty spot (the top-left corner of the pane)
  const centreBefore = (await geometry(page)).ends.at(-1);
  await page.mouse.move(pane.x + 6, pane.y + 6);
  await page.mouse.down();
  await page.mouse.move(pane.x + 86, pane.y + 46, { steps: 5 });
  await page.mouse.up();
  const g2 = await geometry(page);
  const moved = [g2.ends.at(-1)[0] - centreBefore[0], g2.ends.at(-1)[1] - centreBefore[1]];
  if (Math.abs(moved[0] - 80) > 2 || Math.abs(moved[1] - 40) > 2) fail(`the drag panned by ${moved}, expected 80,40`);
  const w2 = aligned(g2, "after the drag pan", fail);

  // a drag that starts on a card pans the graph like one on the background,
  // and the release does not open or re-centre the card (card 5 is an assembly)
  await page.evaluate(() => { window.__opened.length = 0; });
  const centreTitle = () => page.evaluate(() => document.querySelector(".g-node.is-center").title);
  const titleBefore = await centreTitle();
  const card = await page.locator(".g-card").nth(5).boundingBox();
  await page.mouse.move(card.x + card.width / 2, card.y + card.height / 2);
  await page.mouse.down();
  await page.mouse.move(card.x + card.width / 2 + 60, card.y + card.height / 2 + 30, { steps: 4 });
  await page.mouse.up();
  await page.waitForTimeout(150);
  const g3 = await geometry(page);
  const moved3 = [g3.ends.at(-1)[0] - g2.ends.at(-1)[0], g3.ends.at(-1)[1] - g2.ends.at(-1)[1]];
  if (Math.abs(moved3[0] - 60) > 2 || Math.abs(moved3[1] - 30) > 2) fail(`a drag from a card panned by ${moved3}, expected 60,30`);
  if ((await centreTitle()) !== titleBefore) fail("a drag from an assembly card re-centred the graph");
  if (await page.evaluate(() => window.__opened.length)) fail("a drag from a card opened it");
  aligned(g3, "after a drag from a card", fail);
  // no text got selected and the browser did not start an image drag
  if (await page.evaluate(() => String(window.getSelection()).length)) fail("dragging selected text");

  // a double-click on the background fits the whole graph again
  await page.mouse.dblclick(pane.x + 6, pane.y + 6);
  await page.waitForTimeout(100);
  aligned(await geometry(page), "after the fit", fail);

  // a part opens its overview with the way back to the graph
  await page.evaluate(() => { window.__opened.length = 0; });
  const part = page.locator(".g-node[title^='PRT-1-01/A']");
  await part.click();
  const opened = await page.evaluate(() => window.__opened.map((t) => ({ key: t.key, sub: t.sub, back: !!t.returnToGraph })));
  if (!(opened.length === 1 && opened[0].sub === "overview" && opened[0].back)) {
    fail(`clicking a part opened ${JSON.stringify(opened)}`);
  }

  // the centre opens its own overview and adds no step to go back (#254)
  await page.locator(".g-node.is-center").click();
  const centre = await page.evaluate(() => ({ sub: window.__tab.sub, depth: window.__tab.graph.stack.length }));
  if (!(centre.sub === "overview" && centre.depth === 1)) fail(`clicking the centre gave ${JSON.stringify(centre)}`);
  await page.evaluate(() => { window.__tab.sub = "graph"; });
  await page.evaluate(() => { document.getElementById("host").textContent = ""; });
  await page.evaluate(async () => { (await import("/js/tabs.js")).renderActive(); });
  await page.waitForFunction(() => document.querySelectorAll(".g-card").length > 1, null, { timeout: 15000 });
  if (await page.locator("#back .graph-back").count()) fail("a back control without a step to go back");

  // a sub-assembly becomes the tab's item and the graph centres on it; the
  // back control says "← Back" and returns to the assembly
  await page.locator(".g-node[title^='ASM-1-05/A']").click();
  await page.waitForFunction(() => {
    const c = document.querySelector(".g-node.is-center");
    return c && c.title.startsWith("ASM-1-05/A");
  }, null, { timeout: 10000 });
  const tabItem = await page.evaluate(() => window.__tab.key);
  if (tabItem !== "rev:ASM-1-05:A") fail(`the tab shows ${tabItem} after opening the sub-assembly`);
  const back = await page.locator("#back .graph-back").textContent();
  if (back !== "← Back") fail(`the back control says ${JSON.stringify(back)}`);
  await page.waitForTimeout(400);
  aligned(await geometry(page), "after re-centring", fail);
  await page.locator("#back .graph-back").click();
  await page.waitForFunction(() => {
    const c = document.querySelector(".g-node.is-center");
    return c && c.title.startsWith("ASM-000004/A");
  }, null, { timeout: 10000 });
  const home = await page.evaluate(() => ({ key: window.__tab.key, depth: window.__tab.graph.stack.length }));
  if (!(home.key === "rev:ASM-000004:A" && home.depth === 1)) fail(`back gave ${JSON.stringify(home)}`);

  if (errors.length) fail("page errors: " + errors.join(" | "));
  await browser.close();
  server.close();
  console.log(`${name}: worst card offset ${w0.toFixed(2)} / ${w1.toFixed(2)} / ${w2.toFixed(2)} px (paint / zoom / pan)`);
  return failures;
}

async function checkModules(browserType, name) {
  const server = await serve(desktopRoutes);
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await browserType.launch();
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("requestfailed", (r) => errors.push("failed " + r.url()));
  page.on("response", (r) => { if (r.status() >= 400 && r.url().endsWith(".js")) errors.push(`${r.status()} ${r.url()}`); });
  await page.addInitScript(() => {
    const never = () => new Promise(() => {});
    window.__TAURI__ = {
      core: { invoke: () => never(), convertFileSrc: (p) => p },
      event: { listen: async () => () => {} },
      window: { getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {} }) },
      webview: { getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }) },
      dialog: {}, opener: {}, shell: {}
    };
  });
  await page.goto(base + "/");
  await page.waitForTimeout(1500);
  const loaded = await page.evaluate(() => [...performance.getEntriesByType("resource")]
    .filter((e) => e.name.includes("/js/")).length);
  await browser.close();
  server.close();
  const failures = errors.map((e) => `${name} module graph: ${e}`);
  console.log(`${name}: desktop UI loaded ${loaded} modules`);
  return failures;
}

const failures = [];
for (const name of ["webkit", "chromium"]) {
  try {
    failures.push(...await checkGraph(playwright[name], name));
    failures.push(...await checkModules(playwright[name], name));
  } catch (e) {
    failures.push(`${name}: ${String(e.message || e).split("\n")[0]}`);
  }
}
if (failures.length) {
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log(`graph check passed in WebKit and Chromium; WebKit screenshot: ${shot}`);
