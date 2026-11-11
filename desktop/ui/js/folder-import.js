// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Import of a CAD folder tree.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, TAURI, api, append, call, dialog, enc, errorText, field, h, label, state, toast } from "./core.js";
import { canEdit, canEditProject, freshProjects, refreshWorklistCount } from "./shell.js";
import { selectView } from "./navigator.js";
import { table } from "./tabs.js";
import { openRevision } from "./revision.js";

// ------------------------------------------- import of a CAD folder tree
// (docs/src/research/cad-folder-import.md)

export var formatKindsPromise = null;
/** File extension → format kind ("part", "assembly", "drawing", …) from the server's format list. */
export function formatKinds() {
  if (!formatKindsPromise) {
    formatKindsPromise = api("GET", "/formats").then(function (r) {
      var kinds = {};
      r.systems.forEach(function (s) {
        s.formats.forEach(function (f) {
          f.extensions.forEach(function (e) { if (!kinds[e]) kinds[e] = f.kind; });
        });
      });
      return kinds;
    }, function (err) { formatKindsPromise = null; throw err; });
  }
  return formatKindsPromise;
}

export var IMPORT_NUMBER = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
/** Kinds that become an item of their own (grouped by base name). */
export var GROUPED_KINDS = ["part", "assembly", "model", "drawing", "exchange", "mesh", "visualization", "electronics"];

export function splitName(name) {
  var dot = name.lastIndexOf(".");
  return dot > 0 ? { stem: name.slice(0, dot), ext: name.slice(dot + 1).toLowerCase() } : { stem: name, ext: "" };
}
export function folderOf(rel) { return rel.split("/").slice(0, -1).join("/"); }

/** Idempotency key of one source item of a folder import: stable for the
 * same group, folder and item key ("folder-import:" + SHA-256 as hex). */
export function importKey(group, path, itemKey) {
  var text = [group, path, itemKey].join("\u0000");
  if (!window.crypto || !crypto.subtle) return Promise.resolve("folder-import:" + encodeURIComponent(text).replace(/%/g, "~").slice(0, 200));
  return crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)).then(function (buf) {
    return "folder-import:" + Array.prototype.map.call(new Uint8Array(buf), function (b) { return ("0" + b.toString(16)).slice(-2); }).join("");
  });
}

/** File name → folder in the project group for the upload of imported
 * files: the scanned folder and the subfolders below it, so the working
 * folder mirrors the original structure (<group>/Rover/Parts/Wheel.ipt). */
export function importFolders(files, top) {
  var out = {};
  files.forEach(function (f) {
    var sub = folderOf(f.rel);
    out[f.name] = sub ? top + "/" + sub : top;
  });
  return out;
}

/** What the import will create: one item per part/assembly file with the
 * drawings and exports of the same base name; the rest in one document item. */
export function planImport(scan, kinds, opts) {
  var byName = {}, duplicates = [];
  scan.files.forEach(function (f) {
    // Creo: only the newest iteration, under its logical name (bracket.prt.7 → bracket.prt)
    var logical = f.name, iteration = 0;
    var m = /^(.+\.([a-z0-9]+))\.(\d+)$/i.exec(f.name);
    if (m && kinds[m[2].toLowerCase()]) { logical = m[1]; iteration = Number(m[3]); }
    var key = logical.toLowerCase(), seen = byName[key];
    if (!seen) { byName[key] = { file: f, logical: logical, iteration: iteration }; return; }
    if (folderOf(seen.file.rel) === folderOf(f.rel)) {
      if (iteration > seen.iteration) byName[key] = { file: f, logical: logical, iteration: iteration };
    } else {
      duplicates.push(f.rel + " – same name as " + seen.file.rel);
    }
  });
  var entries = Object.keys(byName).map(function (k) { return byName[k]; })
    .sort(function (a, b) { return a.file.rel.localeCompare(b.file.rel); });
  var groups = {}, order = [], extras = [];
  entries.forEach(function (e) {
    var n = splitName(e.logical), kind = kinds[n.ext];
    if (GROUPED_KINDS.indexOf(kind) < 0) return;
    var key = n.stem.toLowerCase();
    if (!groups[key]) { groups[key] = { name: n.stem, files: [], kinds: [] }; order.push(key); }
    groups[key].files.push(e.file);
    groups[key].kinds.push(kind);
  });
  entries.forEach(function (e) {
    var n = splitName(e.logical);
    if (GROUPED_KINDS.indexOf(kinds[n.ext]) >= 0) return;
    var g = groups[n.stem.toLowerCase()];
    if (g) g.files.push(e.file); else extras.push(e.file);
  });
  var items = order.map(function (key) {
    var g = groups[key];
    return {
      key: key, name: g.name, files: g.files,
      type: g.kinds.indexOf("assembly") >= 0 ? "assembly"
        : g.kinds.some(function (k) { return k !== "drawing"; }) ? "part" : "document",
      number: opts.numbersFromNames && IMPORT_NUMBER.test(g.name) ? g.name.toUpperCase() : null
    };
  });
  if (extras.length && opts.extras) {
    items.push({ key: "\u0000project-files", name: "Project files – " + scan.folder_name, type: "document", files: extras, number: null });
  }
  return { items: items, duplicates: duplicates, extras: extras.length, ignored: scan.ignored, files: scan.files.length };
}

/** Items already imported from a folder into a project group (to continue an
 * interrupted import) – per group, so the same folder can go into another group. */
export function importLogKey(project, folder) {
  return "teamcraft.import-log:" + JSON.stringify([state.session.server_url, project, folder]);
}
export function importLog(project, folder) {
  try { return JSON.parse(window.localStorage.getItem(importLogKey(project, folder))) || {}; }
  catch (e) { return {}; }
}
export function saveImportLog(project, folder, log) {
  try { window.localStorage.setItem(importLogKey(project, folder), JSON.stringify(log)); }
  catch (e) { /* the import still works, it just cannot be continued */ }
}
/** An entry of the import log: the item number (done) or
 * `{ number, pending: true }` – created, but its files did not arrive yet;
 * the next run checks it out again and completes it instead of skipping it. */
export function loggedNumber(entry) { return typeof entry === "string" ? entry : entry && entry.number; }
export function loggedPending(entry) { return !!(entry && typeof entry === "object" && entry.pending); }
export function importDone(log, key) { return !!log[key] && !loggedPending(log[key]); }

/** Earlier versions kept one log per folder, without the project group. Such
 * a log is moved to the group of the items it lists (asked for its first
 * item), so an interrupted import still continues instead of creating the
 * items twice; it is kept as it is while that item cannot be read. */
export function adoptFolderImportLog(folder) {
  var oldKey = "teamcraft.import:" + state.session.server_url + ":" + folder, old = null;
  try { old = JSON.parse(window.localStorage.getItem(oldKey)); } catch (e) { /* unreadable: ignored */ }
  var keys = old ? Object.keys(old) : [];
  if (!keys.length) return Promise.resolve();
  return api("GET", "/items/" + enc(loggedNumber(old[keys[0]]))).then(function (r) {
    var project = r.item && r.item.project_code;
    if (!project) return;
    var log = importLog(project, folder);
    keys.forEach(function (k) { if (!log[k]) log[k] = old[k]; });
    saveImportLog(project, folder, log);
    var saved = importLog(project, folder);
    if (keys.every(function (k) { return saved[k]; })) {
      try { window.localStorage.removeItem(oldKey); } catch (e) { /* tried again at the next scan */ }
    }
  }, function () { /* deleted or not visible: the old log stays unused */ });
}

/** File kinds whose references describe the structure (NX: "model"). */
export var REFERENCING_KINDS = ["assembly", "model"];
/** Note of a BOM line whose quantity the CAD file does not tell. */
export var ASSUMED_NOTE = "From the CAD references – check the quantity";
/** Note of a BOM line with the instances counted in the CAD file. */
export var COUNTED_NOTE = "From the CAD references (instances counted in the CAD file)";

/** The BOM children of an assembly from its `GET …/references` answer: the
 * first match of each referenced name that is another item, once per item,
 * with the instance count the server read from the file (`instances`:
 * SOLIDWORKS 2015+ and NX assemblies) or `null` when the file does not tell
 * (Inventor, Solid Edge, …). A part with only instances excluded from the
 * BOM (`instances: 0`) is left out. */
export function referencedChildren(refs, ownNumber) {
  var children = [];
  (refs.files || []).forEach(function (f) {
    if (REFERENCING_KINDS.indexOf(f.format_kind) < 0) return;
    (f.references || []).forEach(function (r) {
      var counted = typeof r.instances === "number" ? r.instances : null;
      if (counted === 0) return;
      var m = (r.matches || []).filter(function (x) { return !x.same_revision; })[0];
      if (!m || m.item_number === ownNumber) return;
      if (children.some(function (c) { return c.child === m.item_number; })) return;
      children.push({ child: m.item_number, instances: counted });
    });
  });
  return children;
}

/** BOM lines for the parts the new items reference, with the instance
 * counts where the CAD file tells them (quantity 1 otherwise). Not only
 * the items planned as assemblies: NX keeps parts and assemblies in the
 * same `.prt` format (kind "model"), so an NX assembly is only recognised by
 * the references the server found in its file. Resolves to
 * `{ lines, assumed }` – lines created, of them with an assumed quantity 1. */
export function bomFromReferences(created, progress, failed) {
  var lines = 0, assumed = 0;
  var assemblies = created.filter(function (c) { return c.item.type !== "document"; });
  return assemblies.reduce(function (chain, c, i) {
    return chain.then(function () {
      progress(i + 1, assemblies.length, "Structure of " + c.rev.item_number);
      var base = "/items/" + enc(c.rev.item_number) + "/revisions/" + enc(c.rev.code);
      return api("GET", base + "/references").then(function (refs) {
        var children = referencedChildren(refs, c.rev.item_number);
        if (!children.length) return;
        return api("POST", base + "/checkout").then(function () {
          var added = children.reduce(function (ch, x) {
            return ch.then(function () {
              var line = x.instances
                ? { child: x.child, quantity: x.instances, notes: COUNTED_NOTE }
                : { child: x.child, quantity: 1, notes: ASSUMED_NOTE };
              return api("POST", base + "/bom", line)
                .then(function () { lines++; if (!x.instances) assumed++; }, function () { /* e.g. it would be circular */ });
            });
          }, Promise.resolve());
          // whatever happened while adding the lines: the lock is released again
          return added.then(function () { return checkInImported(c.rev); }, function (err) {
            return checkInImported(c.rev).then(function () { throw err; });
          });
        }).catch(function (err) { failed.push(c.rev.item_number + ": BOM lines – " + errorText(err)); });
      }, function () { /* no references */ });
    });
  }, Promise.resolve()).then(function () { return { lines: lines, assumed: assumed }; });
}

/** Check-in of an imported item without new files (releases the lock). */
/** An item the server returned for an idempotency key of an earlier run
 * (its answer was lost): still checked out to me → continue as new; checked
 * in with all its files → done; checked in without them → checked out again
 * and completed. */
export function resumeReplayed(rev, it) {
  if (rev.checked_out_by === state.me.id) return rev;
  var base = "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code);
  return api("GET", base).then(function (r) {
    var have = {};
    (r.files || []).forEach(function (f) { have[String(f.filename).toLowerCase()] = true; });
    var complete = it.files.every(function (f) { return have[String(f.name).toLowerCase()]; });
    if (complete) { rev.done = true; return rev; }
    return api("POST", base + "/checkout").then(function () { return rev; });
  });
}

export function checkInImported(rev) {
  return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/checkin");
}

/** Releases the check-outs of items an import of this folder created –
 * also those an earlier run left checked out when it was interrupted. Only
 * the check-out taken when the item was created (same timestamp) counts:
 * an item someone checked out again later to work on it keeps its lock. */
export function releaseImported(log, failed) {
  var numbers = Object.keys(log).map(function (k) { return loggedNumber(log[k]); });
  if (!numbers.length) return Promise.resolve();
  return refreshWorklistCount().then(function (w) {
    var ids = w.checkouts.filter(function (r) {
      return numbers.indexOf(r.item_number) >= 0 && r.checked_out_at === r.created_at;
    }).map(function (r) { return r.id; });
    if (!ids.length) return;
    return api("POST", "/worklist/checkin-all", { revisions: ids }).then(function (result) {
      (result.revisions || []).forEach(function (o) {
        if (!o.ok) failed.push(o.item_number + ": check-in – " + o.error);
      });
    });
  }).catch(function (err) { failed.push("Releasing the check-outs: " + errorText(err)); });
}

export function importFolderDialog() { freshProjects().then(openImportFolderDialog); }

export function openImportFolderDialog() {
  if (!canEdit()) { toast("Viewers cannot import.", "error"); return; }
  var projects = state.projects.filter(function (p) { return p.active && canEditProject(p.id); });
  if (!projects.length) { toast("You are not a member of any project group – ask an admin to add you.", "error"); return; }
  var folder = h("input", { type: "text", spellcheck: "false", placeholder: "Folder with the CAD files" });
  // another folder: scan again before Import (the plan and the log belong to the scanned folder)
  function forgetScan() { if (!running) { scanned = null; plan = null; scannedPath = ""; report.textContent = ""; } }
  folder.addEventListener("input", forgetScan);
  var pick = TAURI.dialog ? h("button", { type: "button", on: { click: function () {
    TAURI.dialog.open({ directory: true, title: "CAD folder to import" }).then(function (dir) { if (dir && !running) { folder.value = dir; forgetScan(); } });
  } } }, "Choose…") : null;
  var project = h("select", null, projects.map(function (p) { return h("option", { value: p.code, text: p.code + " – " + p.name }); }));
  var fromNames = h("input", { type: "checkbox" });
  var extras = h("input", { type: "checkbox", checked: true });
  var bom = h("input", { type: "checkbox", checked: true });
  var report = h("div.stack");
  var bar = h("progress.hidden", { max: "1", value: "0" });
  var status = h("p.small", { role: "status" });
  var scanned = null, scannedPath = "", kinds = null, plan = null, running = false, stop = false;

  function options() { return { numbersFromNames: fromNames.checked, extras: extras.checked }; }
  function progress(done, total, text) { bar.classList.remove("hidden"); bar.max = Math.max(total, 1); bar.value = done; status.textContent = text; }

  function showPlan() {
    plan = planImport(scanned, kinds, options());
    var log = importLog(project.value, scannedPath);
    var open = plan.items.filter(function (it) { return !importDone(log, it.key); });
    var unfinished = open.filter(function (it) { return loggedPending(log[it.key]); }).length;
    report.textContent = "";
    append(report, [
      h("p", null, [h("strong", { text: plan.files + " files" }), " → " + plan.items.length + " items (" +
        plan.items.filter(function (it) { return it.type === "assembly"; }).length + " assemblies, " +
        plan.items.filter(function (it) { return it.type === "part"; }).length + " parts, " +
        plan.items.filter(function (it) { return it.type === "document"; }).length + " documents)" +
        (plan.items.length - open.length ? " – " + (plan.items.length - open.length) + " already imported, skipped" : "") +
        (unfinished ? " – " + unfinished + " created before without all files, completed now" : "") +
        (plan.ignored ? " · " + plan.ignored + " lock/backup files left out" : "") +
        (plan.extras && !extras.checked ? " · " + plan.extras + " other files not imported" : "")]),
      plan.duplicates.length ? h("div.notice", null, [h("strong", { text: plan.duplicates.length + " duplicate file names – only the first file of each name is imported. Rename them in the CAD program (TeamCraft recognises CAD references by file name):" }),
        h("ul.small", null, plan.duplicates.slice(0, 20).map(function (d) { return h("li", { text: d }); }))]) : null,
      table(["Item", "Type", "Number", "Files"], open.slice(0, 200).map(function (it) {
        return { cells: [it.name, label(it.type), it.number || h("span.dim", { text: "generated" }),
          h("span.small", { text: it.files.map(function (f) { return f.name; }).join(", ") })] };
      })),
      open.length > 200 ? h("p.small.dim", { text: "… and " + (open.length - 200) + " more items" }) : null
    ]);
  }

  function scanNow(d) {
    var path = folder.value.trim();
    if (!path) { d.fail("Choose the folder first."); return; }
    status.textContent = "Scanning …";
    Promise.all([call("scan_folder", { path: path }), formatKinds(), adoptFolderImportLog(path)]).then(function (r) {
      if (folder.value.trim() !== path) { status.textContent = ""; return; } // changed while scanning
      scanned = r[0]; kinds = r[1]; scannedPath = path;
      status.textContent = "Scanned – nothing is uploaded yet. Check the list, then Import.";
      showPlan();
    }, function (err) { status.textContent = ""; d.fail(err); });
  }
  // the project group too: what counts as "already imported" is kept per group
  [fromNames, extras, project].forEach(function (box) { box.addEventListener("change", function () { if (scanned && !running) showPlan(); }); });

  function importNow(d) {
    if (running) return;
    if (!plan) { d.fail("Scan the folder first."); return; }
    var path = scannedPath, group = project.value, log = importLog(group, path);
    var todo = plan.items.filter(function (it) { return !importDone(log, it.key); });
    var created = [], failed = [], files = 0, checkedIn = 0;
    running = true; stop = false;
    todo.reduce(function (chain, it, i) {
      return chain.then(function () {
        if (stop) return;
        progress(i + 1, todo.length, "Item " + (i + 1) + " of " + todo.length + ": " + it.name);
        // An item an earlier run created without all its files: checked out
        // again and completed (its revision A is still in work).
        var pendingNumber = loggedPending(log[it.key]) ? loggedNumber(log[it.key]) : null;
        var start = pendingNumber
          ? api("POST", "/items/" + enc(pendingNumber) + "/revisions/A/checkout").catch(function (err) {
              // checked out to me already (e.g. the earlier run stopped before its check-in)
              return api("GET", "/items/" + enc(pendingNumber) + "/revisions/A").then(function (r) {
                var rev = r.revision || r;
                if (rev.checked_out_by === state.me.id) return rev;
                throw err;
              });
            })
          : importKey(group, path, it.key).then(function (key) {
              // the same key on every run: an item whose answer was lost is
              // returned by the server, not created a second time
              return api("POST", "/items", { item_type: it.type, name: it.name, project: group, number: it.number, idempotency_key: key });
            }).then(function (rev) { return pendingNumber ? rev : resumeReplayed(rev, it); });
        return start.then(function (rev) {
          if (rev.done) {
            // created and checked in completely by an earlier run whose log was lost
            log[it.key] = rev.item_number; saveImportLog(group, path, log);
            created.push({ rev: rev, item: it });
            return;
          }
          if (!pendingNumber) {
            // remembered at once, so a second run never creates it twice –
            // pending until its files are in
            log[it.key] = { number: rev.item_number, pending: true }; saveImportLog(group, path, log);
          }
          created.push({ rev: rev, item: it });
          return call("upload_files", { item: rev.item_number, code: rev.code, paths: it.files.map(function (f) { return f.path; }),
            comment: "Imported from " + scanned.folder_name, folders: importFolders(it.files, scanned.folder_name) })
            .then(function () {
              files += it.files.length;
              log[it.key] = rev.item_number; saveImportLog(group, path, log);
            },
              function (err) { failed.push(rev.item_number + " (" + it.name + "): files not uploaded – " + errorText(err) + " – scan and import again to complete it"); })
            // Checked in right away: an import that is interrupted (app closed,
            // sign-in expired, network gone, an error in a later step) never
            // leaves the items created so far in "My check-outs".
            .then(function () { return checkInImported(rev); })
            .then(function () { checkedIn++; },
              function (err) { failed.push(rev.item_number + ": check-in – " + errorText(err)); });
        }, function (err) { failed.push(it.name + ": " + errorText(err)); });
      });
    }, Promise.resolve()).then(function () {
      // the assemblies are checked out once more for their BOM lines – and always checked in again
      return bom.checked ? bomFromReferences(created, progress, failed) : { lines: 0, assumed: 0 };
    }).catch(function (err) {
      failed.push(errorText(err));
      return { lines: 0, assumed: 0 };
    }).then(function (lines) {
      // Items of this folder that an earlier, interrupted run left checked out.
      return releaseImported(log, failed).then(function () { return lines; });
    }).then(function (lines) {
      running = false;
      bar.classList.add("hidden");
      status.textContent = "";
      if (created.length) { delete state.expanded[group]; if (state.view === "home") selectView("home"); refreshWorklistCount(); }
      d.close();
      importFinishedDialog({
        stopped: stop, created: created, files: files, skipped: plan.items.length - todo.length,
        bom: bom.checked, lines: lines.lines, assumed: lines.assumed, checkedIn: checkedIn, failed: failed,
        folder: scanned.folder_name, project: group
      });
    });
  }

  dialog("Import a CAD folder", h("div.stack", null, [
    h("p", null, ["Brings an existing folder of CAD data into TeamCraft: one item per part or assembly, with the drawings and exports of the same name. The folder itself is not changed. ",
      h("a", { href: "#", on: { click: function (e) { e.preventDefault(); importTutorial(); } } }, "How it works")]),
    field("Folder", h("div.row", null, [folder, pick])),
    field("Project group", project),
    h("label.check", null, [fromNames, "Use file names as item numbers where they are valid – otherwise numbers are generated"]),
    h("label.check", null, [extras, "Put project files (.ipj …) and other documents into one document item"]),
    h("label.check", null, [bom, "Create BOM lines from the references found in the assemblies (instance counts of SOLIDWORKS and NX assemblies, otherwise quantity 1 – check those)"]),
    report, bar, status
  ]), [
    { label: "Close", onClick: function (d) { stop = true; d.close(); } },
    { label: "Scan", onClick: function (d) { if (!running) scanNow(d); } },
    { label: "Import", primary: true, submit: true, onClick: importNow }
  ], { wide: true });
}

/** The report line of the BOM lines created from the CAD references. */
export function bomReportLine(lines, assumed) {
  var text = lines + " BOM lines from the CAD references";
  if (!lines) return text;
  if (!assumed) return text + " – quantities counted in the CAD files";
  if (assumed === lines) return text + " – quantity 1, the CAD files do not tell how often a part is used: please check them";
  return text + " – " + (lines - assumed) + " with the quantity counted in the CAD files, " + assumed +
    " with quantity 1 (not known from the CAD file): please check those";
}

/** The report at the end of a folder import (finished, stopped or with
 * problems) – like the upload report of an autoloader. */
export function importFinishedDialog(r) {
  var ok = !r.failed.length && !r.stopped;
  var title = r.stopped ? "Import stopped" : r.failed.length ? "Import finished with problems" : "Import finished";
  function count(type) { return r.created.filter(function (c) { return c.item.type === type; }).length; }
  // the assembly to open: the one named like the folder, else the first new one
  var assemblies = r.created.filter(function (c) { return c.item.type === "assembly"; });
  var top = assemblies.filter(function (c) { return c.item.name.toLowerCase() === String(r.folder).toLowerCase(); })[0] || assemblies[0];
  var body = h("div.stack.import-report", null, [
    h("p.import-report-head" + (ok ? ".ok" : ".warn"), { role: "status", text: (ok ? "✓ " : "⚠ ") + title }),
    h("ul", null, [
      h("li", { text: r.created.length + " new items in " + r.project + " – " + count("assembly") + " assemblies, " +
        count("part") + " parts, " + count("document") + " documents – with " + r.files + " files" }),
      r.skipped ? h("li", { text: r.skipped + " items were imported before and skipped" }) : null,
      r.bom ? h("li", { text: bomReportLine(r.lines, r.assumed) }) : null,
      h("li", { text: r.checkedIn === r.created.length
        ? "Every new item is checked in – nothing is locked"
        : r.checkedIn + " of " + r.created.length + " new items checked in – the others are still checked out to you" }),
      r.stopped ? h("li", { text: "You stopped the import – scan and import the folder again to continue where it stopped." }) : null
    ]),
    r.failed.length ? h("div.notice", null, [h("strong", { text: r.failed.length + (r.failed.length === 1 ? " problem:" : " problems:") }),
      h("ul.small", null, r.failed.map(function (f) { return h("li", { text: f }); }))]) : null,
    h("p.small.dim", { text: "Next: check the structure and the quantities, then release the product with one change order (Help → How to Import a CAD Folder)." })
  ]);
  var actions = [];
  if (top) actions.push({ label: "Open " + top.rev.item_number, onClick: function (d) { d.close(); openRevision(top.rev.item_number, top.rev.code); } });
  actions.push({ label: "Close", primary: true, submit: true, onClick: function (d) { d.close(); } });
  dialog(title, body, actions, { wide: true });
  toast(title + ": " + r.created.length + " items, " + r.files + " files", ok ? null : "error");
}

/** The tutorial: how to bring existing CAD data into TeamCraft. */
export function importTutorial() {
  function step(title, points) {
    return h("section.stack", null, [h("h3", { text: title }), h("ul", null, points.map(function (p) { return h("li", { text: p }); }))]);
  }
  dialog("How to import a CAD folder", h("div.stack.tutorial", null, [
    h("p", { text: "Like Autodesk Vault's Autoloader or the data preparation of Teamcenter: prepare the data, scan it (nothing is uploaded), then import. Hundreds of files are fine." }),
    step("1 · Prepare the data in the CAD program", [
      "Collect each product into one clean folder with Pack and Go – Inventor: File → Save As → Pack and Go (with the project file .ipj); Solid Edge: Pack and Go in Design Manager or Revision Manager; SolidWorks: File → Pack and Go.",
      "Open the top assembly once and fix missing references – broken links stay broken after the import.",
      "Give every file a unique name. TeamCraft, like Teamcenter, recognises CAD references by file name: two different Bracket.ipt in two subfolders are reported as duplicates and only the first one is imported.",
      "Optional: export STEP files next to the models for the 3D preview – or let the app create them later at check-in (Windows, with Inventor or Solid Edge)."
    ]),
    step("2 · Scan", [
      "Tools → Import CAD Folder…: choose the folder and the project group, then Scan. The report lists the items that will be created, duplicate names and the files left out (lock and backup files, OldVersions folders).",
      "One item per part or assembly file; its drawing and exports (STEP, STL, PDF …) with the same name go into the same item. Project files (.ipj …) and other documents go into one document item.",
      "Item numbers are generated (PRT-…, ASM-…) – or taken from the file names when they are valid item numbers."
    ]),
    step("3 · Import", [
      "Import creates the items and uploads the files, item by item, with progress. The source folder is only read.",
      "Stop finishes the current item. The app remembers what was imported into each project group: scan and import again to continue. The same folder can also be imported into another project group.",
      "At the end every new item is checked in, so nothing stays locked.",
      "When it is done, a report shows the new items and files, the BOM lines and any problems – with a button that opens the top assembly."
    ]),
    step("4 · Check and release", [
      "Structure: assemblies get BOM lines for the parts they reference. SOLIDWORKS (2015 and newer) and NX assemblies give the quantity – how often the part is placed; for other formats (Inventor, Solid Edge, …) the quantity is 1 and has to be checked. For exact quantities export the structured BOM from the CAD program (CSV or Excel) and use Import BOM on the assembly.",
      "Look at the preview images and 3D views, fix what is missing, then release the product with one change order.",
      "Team members get the files with Get… (Tools → Get Project Groups…) – one folder per project group."
    ])
  ]), [{ label: "Close", primary: true, onClick: function (d) { d.close(); } }], { wide: true });
}
