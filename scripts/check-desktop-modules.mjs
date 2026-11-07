// SPDX-License-Identifier: MPL-2.0
// Checks the ES modules of the desktop UI (desktop/ui/js/) without running
// them – plain Node.js, no packages (ADR 0004: Node.js only checks, never
// ships). Run by the CI job `ui` in .github/workflows/desktop.yml:
//
//   node scripts/check-desktop-modules.mjs
//
// A name used from another module without an import is a ReferenceError only
// when that code runs, so this catches it before: every import names an
// export of its module, every top-level name of another module that a module
// uses is imported (or declared locally), no top-level name is declared in two
// modules, and every module is reachable from js/main.js.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const dir = path.join(root, "desktop/ui/js");
const files = fs.readdirSync(dir).filter((f) => f.endsWith(".js")).sort();
const errors = [];
const ident = "[A-Za-z_$][\\w$]*";

/** Source without comments and string/regex contents (rough, line-preserving). */
function code(text) {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|`(?:\\.|[^`\\])*`/g, (m) => '""' + " ".repeat(Math.max(0, m.length - 2)))
    .replace(/\/\/[^\n]*/g, "");
}

const mods = {};
for (const f of files) {
  const text = fs.readFileSync(path.join(dir, f), "utf8");
  const exports = new Set();
  for (const m of text.matchAll(new RegExp(`^export (?:function|var|let|const) (${ident})`, "gm"))) exports.add(m[1]);
  for (const m of text.matchAll(new RegExp(`^export var [^\\n]*?,\\s*(${ident})\\s*=`, "gm"))) exports.add(m[1]);
  const imports = new Map(); // name → module
  for (const m of text.matchAll(/^import \{ ([^}]*) \} from "\.\/([\w-]+\.js)";$/gm)) {
    for (const n of m[1].split(",").map((s) => s.trim()).filter(Boolean)) imports.set(n, m[2]);
  }
  for (const m of text.matchAll(/^import\s+(?!\{ [^}]* \} from "\.\/[\w-]+\.js";$)[^\n]*$/gm)) {
    errors.push(`${f}: unsupported import form (use: import { a, b } from "./x.js";): ${m[0]}`);
  }
  mods[f] = { text, body: code(text), exports, imports };
}

// imports resolve
for (const [f, m] of Object.entries(mods)) {
  for (const [n, from] of m.imports) {
    if (!mods[from]) errors.push(`${f}: imports from a missing module ./${from}`);
    else if (!mods[from].exports.has(n)) errors.push(`${f}: ${n} is not exported by ${from}`);
  }
}

// one owner per top-level name
const owner = new Map();
for (const [f, m] of Object.entries(mods)) {
  for (const n of m.exports) {
    if (owner.has(n)) errors.push(`${n} is declared in ${owner.get(n)} and ${f}`);
    owner.set(n, f);
  }
}

// names of other modules are imported or declared locally
for (const [f, m] of Object.entries(mods)) {
  const local = new Set();
  for (const d of m.body.matchAll(new RegExp(`\\b(?:var|let|const|function)\\s+(${ident})`, "g"))) local.add(d[1]);
  for (const p of m.body.matchAll(/\bfunction\s*[\w$]*\s*\(([^)]*)\)/g)) {
    for (const n of p[1].split(",").map((s) => s.trim().replace(/\s*=.*$/, ""))) if (n) local.add(n);
  }
  for (const c of m.body.matchAll(new RegExp(`\\bcatch\\s*\\((${ident})\\)`, "g"))) local.add(c[1]);
  for (const v of m.body.matchAll(new RegExp(`\\bvar\\s+[^;]*?,\\s*(${ident})\\s*=`, "g"))) local.add(v[1]);
  const used = new Set();
  for (const u of m.body.matchAll(new RegExp(`(^|[^.\\w$])(${ident})`, "gm"))) used.add(u[2]);
  for (const n of used) {
    const o = owner.get(n);
    if (!o || o === f || m.imports.has(n) || local.has(n)) continue;
    // an object key ({ name: … }) is not a use
    if (new RegExp(`(^|[^.\\w$])${n.replace(/\$/g, "\\$")}(?!\\s*:)`, "m").test(m.body)) {
      errors.push(`${f}: uses ${n} of ${o} without importing it`);
    }
  }
}

// every module is loaded
const seen = new Set();
(function visit(f) {
  if (seen.has(f) || !mods[f]) return;
  seen.add(f);
  for (const from of new Set(mods[f].imports.values())) visit(from);
})("main.js");
for (const f of files) if (!seen.has(f)) errors.push(`${f}: not reachable from main.js`);
const html = fs.readFileSync(path.join(root, "desktop/ui/index.html"), "utf8");
if (!html.includes('<script type="module" src="js/main.js"></script>')) errors.push("desktop/ui/index.html does not load js/main.js as a module");

if (errors.length) {
  console.error(errors.join("\n"));
  process.exit(1);
}
console.log(`desktop/ui/js: ${files.length} modules, imports complete`);
