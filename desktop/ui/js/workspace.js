// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – 3D viewer, workspace, check-out/in, edit, release, revise.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, api, badge, call, clear, confirmDialog, dialog, enc, errorText, field, fmtDate, fmtSize, h, invoke, label, lockBadge, run, setStatus, state, toast, tool } from "./core.js";
import { isCancelled, refreshWorklistCount, uploadLine } from "./shell.js";
import { reloadActive, renderActive, table } from "./tabs.js";
import { PROGRAM_LABEL, cadPrograms, dd, exportProgram, exportStep, openRevision, previewFormat, revisionAbilities } from "./revision.js";
import { openChange } from "./change.js";
import { requestChangeDialog } from "./requests.js";
import { refreshStructure } from "./navigator.js";

// ---------------------------------------------------------------- 3D viewer

export function disposeViewer() {
  if (state.viewer) {
    try { state.viewer.dispose(); } catch (e) { /* already gone */ }
    state.viewer = null;
  }
}

export function revViewer(tab, el) {
  var files = tab.data.files.filter(function (f) { return previewFormat(f); });
  if (!files.length) {
    el.appendChild(h("p.empty", { text: "No 3D files (STEP, STL, OBJ, 3MF, glTF, PLY, point clouds, simulation results, …) – native CAD files (SolidWorks, NX, Creo, CATIA, …) are shown in 3D through a STEP export next to them." }));
    return;
  }
  var current = files.filter(function (f) { return f.id === tab.viewerFile; })[0] || files[0];
  tab.viewerFile = current.id;
  var canvas = h("canvas.viewer-canvas", { tabindex: "0", "aria-label": "3D view of " + current.filename });
  var status = h("p.small.muted", { role: "status", text: "Loading" });
  var resetBtn = h("button.small", { type: "button", title: "Back to the initial view (double-click)" }, "Reset view");
  var upBtn = h("button.small", { type: "button", title: "Switch between Z-up and Y-up models" }, "Z up");
  // STL export of STEP/3MF/… models (e.g. for 3D printing); saved through a native dialog in Rust.
  var noExport = ["stl", "pts", "xyz", "las"].indexOf(previewFormat(current)) !== -1;
  var stlBtn = noExport ? null : h("button.small", {
    type: "button", disabled: true, title: "Save the tessellated model as STL (units as in the file, usually mm)",
    on: {
      click: function () {
        var name = current.filename.replace(/\.[^.]+$/, "") + ".stl";
        var data;
        try { data = new Uint8Array(state.viewer.exportStl()); } catch (e) { toast(errorText(e), "error"); return; }
        invoke("save_stl", data, { headers: { "x-file-name": encodeURIComponent(name) } }).then(function (path) {
          if (path) toast("Saved " + path);
        }, function (e) { toast(errorText(e), "error"); });
      }
    }
  }, "Export STL…");
  el.appendChild(h("div.viewer-layout", null, [
    h("div.viewer-files", null, [h("h3", { text: "Models" })].concat(files.map(function (f) {
      return h("button" + (f.id === current.id ? ".active" : ""), {
        type: "button", title: f.filename,
        on: { click: function () { tab.viewerFile = f.id; tab.viewerVersion = null; renderActive(); } }
      }, f.filename + "  v" + f.version);
    }))),
    h("div", null, [canvas, h("div.viewer-bar", null, [status, h("div.row", null, [upBtn, resetBtn, stlBtn])]),
      h("p.hint", { text: "Drag to rotate, wheel to zoom, arrow keys to turn, double-click to reset." })])
  ]));
  // the server converts the file into a mesh (pure Rust) – the app only draws it
  var versionId = tab.viewerVersion;
  state.viewer = window.TeamCraftViewer.mount(canvas, {
    statusEl: status,
    resetBtn: resetBtn,
    upBtn: upBtn,
    onReady: function () { if (stlBtn) stlBtn.disabled = false; },
    load: function () {
      return invoke("file_bytes", versionId ? { kind: "version-mesh", id: versionId } : { kind: "mesh", id: current.id });
    }
  });
}

// ---------------------------------------------------------------- workspace

export function revWorkspace(tab, el) {
  var rev = tab.data.rev, can = revisionAbilities(tab.data);
  return Promise.all([call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }), cadPrograms()]).then(function (r) {
    var ws = r[0], programs = r[1];
    var changed = ws.files.filter(function (f) { return f.state === "modified" || f.state === "new"; });
    var outdated = ws.files.filter(function (f) { return f.state === "outdated"; });
    var conflicts = ws.files.filter(function (f) { return f.state === "conflict"; });
    var clashes = ws.files.filter(function (f) { return f.state === "clash"; });
    var lockedByOther = rev.checked_out_by && rev.checked_out_by !== state.me.id;
    // STEP for the 3D preview, written by the CAD program installed here
    function stepButton(f) {
      var program = f.state !== "missing" && f.state !== "clash" && exportProgram(f.name, programs);
      if (!program) return null;
      return h("button.small", {
        type: "button", title: "Create a STEP export with " + PROGRAM_LABEL[program] + " next to the file – check it in with the model for the 3D preview",
        on: { click: function () {
          run("Exporting " + f.name + " to STEP with " + PROGRAM_LABEL[program], function () { return exportStep(rev, f.name); })
            .then(function (step) { toast("Created " + step + " – check it in together with " + f.name); renderActive(); }, function () {});
        } }
      }, "STEP");
    }
    el.appendChild(h("section.panel", null, [
      h("div.panel-head", null, [
        h("h2", { text: "Working folder" }),
        h("div.row", null, [
          tool("Open folder", "folder", function () { openWorkspaceFolder(tab); }),
          tool("Get files", "download", function () {
            getting(call("workspace_download", { project: rev.project_code, item: rev.item_number, code: rev.code }))
              .then(function (r) { reportDownload(r); renderActive(); }, function (e) { toast(errorText(e), "error"); });
          }, { title: "Copy the current files of this revision into the working folder without checking out (read-only)" }),
          tool("Get with structure…", "download", function () { getStructureDialog(rev); },
            { title: "Copy this item and everything it consists of (BOM and referenced CAD files) into the working folder" }),
          can.checkin ? tool("Check in…", "checkin", function () { checkinDialog(tab); }, { primary: changed.length > 0 }) : null
        ])
      ]),
      h("p.small.muted", null, ["Folder: ", h("span.mono", { text: ws.dir }),
        " · files you have not checked out are read-only; check out to change them."]),
      rev.checked_out_by ? h("p.small", null, [lockBadge(lockedByOther ? rev.checked_out_by_name : "You", !lockedByOther),
        " checked out " + fmtDate(rev.checked_out_at) + " (" + lockAge(rev.checked_out_at) + ")" +
        (lockedByOther ? " – locked by " + rev.checked_out_by_name + ": only they can check in right now; your local changes stay in the working folder." : "")]) : null,
      !ws.exists ? h("p.empty", { text: "Not in your working folder yet – check out (or “Get files”) to copy the files." }) :
        ws.files.length ? table(["File", "Local state", "Server version", "Size", "On disk", ""], ws.files.map(function (f) {
          var version = f.server_version ? "v" + f.server_version : "—";
          if (f.base_version && f.server_version && f.base_version !== f.server_version) version += " (yours: v" + f.base_version + ")";
          if (f.base_version && !f.server_version && f.state !== "new") version = "removed (yours: v" + f.base_version + ")";
          var stateBadge = badge(f.state, SYNC_LABEL[f.state] || label(f.state));
          if (f.state === "clash") stateBadge.title = "Another file is at this place: " + f.clash_with + " – it is never overwritten.";
          if (lockedByOther && f.state !== "missing") stateBadge = h("span", null, [stateBadge, " ", badge("lock", "Locked by " + rev.checked_out_by_name)]);
          return {
            cls: f.state === "conflict" ? "row-conflict" : f.state === "clash" ? "row-clash" : null,
            cells: [h("span", null, [
              f.name,
              f.local_name ? h("span.dim.small", { text: " ← " + f.local_name, title: "Newest Creo iteration in the working folder; it is checked in as " + f.name }) : null,
              h("span.ws-path.mono", { text: f.path, title: "Place in the working folder" })
            ]),
              stateBadge, version, f.state === "missing" ? "—" : fmtSize(f.size),
              f.state === "missing" ? "—" : h("span.ro-flag", { text: f.read_only ? "read-only" : "writable" }),
              h("span.row", null, [
                f.state !== "missing" && f.state !== "clash" ? h("button.small", { type: "button", on: { click: function () { openLocalFile(tab, f); } } }, "Open") : null,
                f.state !== "missing" && f.state !== "clash" ? stepButton(f) : null,
                h("button.small", { type: "button", title: "Request a change to " + f.name + " (change request)", on: { click: function () { requestChangeDialog(rev, f.name); } } }, "Request…")
              ])]
          };
        }), { numeric: [3] }) : h("p.empty", { text: "The folder is empty." }),
      clashes.length ? h("p.notice.error", { text: clashes.length + " file(s) cannot be placed because another item's file with the same folder and name is there: " +
        clashes.map(function (f) { return f.path + " (" + f.clash_with + ")"; }).join(", ") + ". Nothing was overwritten – give one of them another folder on the revision page." }) : null,
      conflicts.length ? h("p.notice.error", { text: conflicts.length + " file(s) changed here and on the server since you got them: " +
        conflicts.map(function (f) { return f.name; }).join(", ") + ". A check-in would overwrite the newer version, so the server refuses it – rename your copy, get the current files and merge your changes." }) : null,
      outdated.length ? h("p.notice", { text: outdated.length + " file(s) have a newer version on the server: " +
        outdated.map(function (f) { return f.name; }).join(", ") + " – “Get files” replaces them (your unchanged copies)." }) : null,
      changed.length && !can.checkin ? h("p.notice", { text: changed.length + " local change(s), but you have not checked out this revision – " +
        (lockedByOther ? rev.checked_out_by_name + " holds the lock; check in after they released it." : "check out first, then check in.") }) : null
    ]));
  });
}

/** "Get": an item with everything it consists of – BOM under a revision
 * rule and the CAD files it references – into the working folder. */
export function getStructureDialog(rev) {
  var rule = h("select", null, [
    h("option", { value: "latest", text: "Latest revision of every part, also in work" }),
    h("option", { value: "released", text: "Latest released revision – parts without a release are listed" })
  ]);
  if (rev.state === "released") rule.value = "released";
  dialog("Get " + rev.item_number + "/" + rev.code + " with its structure", h("div.stack", null, [
    h("p", { text: "Copies the files of this revision, of every item in its BOM and of the parts its CAD files reference into your working folder, " +
      "each in its folder of the project group – so the assembly opens in the CAD program. Only missing and outdated files are downloaded; " +
      "files you changed are kept, and nothing is overwritten that belongs to another item." }),
    field("Revision rule", rule),
    h("p.small.muted", { text: "Files you have not checked out are read-only. Cancel stops after the current part of a file – the next Get continues." })
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    { label: "Get files", primary: true, submit: true, onClick: function (d) {
      d.close();
      getting(call("workspace_get", { item: rev.item_number, code: rev.code, rule: rule.value }))
        .then(function (r) { reportDownload(r); renderActive(); }, function (e) { toast(errorText(e), "error"); });
    } }
  ]);
}

/** Shows the progress of a Get in the status bar until it is done. */
export function getting(promise) {
  var box = $("status-get");
  box.classList.remove("hidden");
  $("status-get-text").textContent = "Getting files …";
  $("status-get-bar").value = 0;
  function hide() { box.classList.add("hidden"); }
  return promise.then(function (v) { hide(); return v; }, function (e) { hide(); throw e; });
}

export function onGetProgress(p) {
  var bar = $("status-get-bar");
  bar.max = Math.max(p.bytes_total || 0, 1);
  bar.value = p.bytes_done || 0;
  $("status-get-text").textContent = p.total
    ? "Getting " + Math.min(p.done + (p.current ? 1 : 0), p.total) + " of " + p.total + " files · " +
      fmtSize(p.bytes_done) + " of " + fmtSize(p.bytes_total) + (p.current ? " – " + p.current : "")
    : "Comparing with the working folder …";
}

export function reportDownload(r) {
  var parts = [];
  if (r.cancelled) parts.push("Stopped");
  if (r.downloaded.length) parts.push(r.downloaded.length + " file(s) copied");
  if (r.up_to_date) parts.push(r.up_to_date + " up to date");
  if (r.kept.length) parts.push(r.kept.length + " locally modified file(s) kept");
  if (r.clashes.length) parts.push(r.clashes.length + " name clash(es)");
  if (r.failed.length) parts.push(r.failed.length + " failed");
  if (r.restricted_lines) parts.push(r.restricted_lines + " restricted line(s) skipped – items of project groups you are not a member of");
  var problems = r.clashes.length || r.failed.length || r.unresolved.length || r.missing_references.length;
  if (!problems) {
    toast((parts.length ? parts.join(" · ") : "The working folder is up to date.") + (r.kept.length ? ": " + r.kept.join(", ") : ""), r.kept.length ? "info" : null);
    return;
  }
  function list(title, items) {
    if (!items.length) return null;
    return h("div", null, [h("h3", { text: title }), h("ul.small", null, items.slice(0, 100).map(function (t) { return h("li", { text: t }); })),
      items.length > 100 ? h("p.small.dim", { text: "… and " + (items.length - 100) + " more" }) : null]);
  }
  dialog(r.cancelled ? "Get stopped" : "Get finished with notes", h("div.stack", null, [
    h("p", { text: parts.join(" · ") + "." }),
    h("p.small.muted", null, ["Working folder: ", h("span.mono", { text: r.dir })]),
    list("Name clashes – nothing was overwritten", r.clashes.map(function (c) { return c.path + ": " + c.file + " – the place is taken by " + c.with; })),
    list("Kept (changed locally)", r.kept),
    list("Failed", r.failed),
    list("No revision for the rule", r.unresolved),
    list("Referenced in CAD files, not in TeamCraft", r.missing_references)
  ]), [{ label: "Close", primary: true, submit: true, onClick: function (d) { d.close(); } }], { wide: true });
}

export function openWorkspaceFolder(tab) {
  var rev = tab && tab.data && tab.data.rev;
  call("workspace_open", rev ? { project: rev.project_code, item: rev.item_number, code: rev.code, file: null } : { project: null, item: null, code: null, file: null })
    .catch(function (e) { toast(errorText(e), "error"); });
}

/** Labels of the sync states of working-folder files. */
export var SYNC_LABEL = {
  up_to_date: "Up to date", unchanged: "Up to date", modified: "Modified", outdated: "Outdated", conflict: "Conflict",
  "new": "New", missing: "Not in working folder", clash: "Name clash",
  // the server answers "not found": a restricted project group the user left
  inaccessible: "No longer accessible – local copy only"
};

/** Offers to move a workspace of the old layout <group>/<item>/<revision>/
 * into the working-folder layout (asked after the sign-in). */
export function checkWorkspaceLayout() {
  call("workspace_migration_plan").then(function (plan) {
    if (plan.moves || plan.identical) migrationDialog(plan);
  }, function () { /* offline or no permission: asked again next time */ });
}

export function migrationDialog(plan) {
  function describe(s) {
    if (s.action === "move") return s.from + " → " + s.to + (s.modified ? " (changed locally, moved along)" : "");
    if (s.action === "already_there") return s.from + " – the same file is already at " + s.to + " (old copy removed)";
    return s.from + " – stays: " + s.reason;
  }
  dialog("Move your workspace to the working-folder layout", h("div.stack", null, [
    h("p", { text: "Your workspace still has folders of the old layout <group>/<item>/<revision>/. TeamCraft now keeps one working folder per project group " +
      "that mirrors the folder structure of the files (<group>/<folder>/<file>), so CAD programs find the parts of an assembly." }),
    h("p", { text: plan.moves + " file(s) move, " + plan.identical + " identical cop" + (plan.identical === 1 ? "y is" : "ies are") + " removed, " +
      plan.kept + " stay where they are. Nothing is overwritten – files whose place is taken stay in the old folder and are listed." }),
    h("ul.small", null, plan.steps.slice(0, 200).map(function (s) { return h("li", { text: describe(s) }); })),
    plan.steps.length > 200 ? h("p.small.dim", { text: "… and " + (plan.steps.length - 200) + " more" }) : null,
    h("p.small.muted", null, ["Every step is written to ", h("span.mono", { text: plan.log })])
  ]), [
    { label: "Later", onClick: function (d) { d.close(); } },
    { label: "Move files", primary: true, submit: true, onClick: function (d) {
      d.close();
      run("Moving the workspace", function () { return call("workspace_migrate"); }).then(function (r) {
        var kept = r.steps.filter(function (s) { return s.action === "keep"; });
        dialog("Workspace moved", h("div.stack", null, [
          h("p", { text: r.moves + " file(s) moved, " + r.identical + " identical copies removed, " + r.kept + " kept in the old folders." }),
          kept.length ? h("ul.small", null, kept.slice(0, 200).map(function (s) { return h("li", { text: describe(s) }); })) : null,
          h("p.small.muted", null, ["Log: ", h("span.mono", { text: r.log })])
        ]), [{ label: "Close", primary: true, submit: true, onClick: function (dd) { dd.close(); renderActive(); } }], { wide: true });
      }, function () {});
    } }
  ], { wide: true });
}


/** How long ago a lock was taken: "3 days", "5 h", "12 min", "just now". */
export function lockAge(iso) {
  var s = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (!isFinite(s)) return "—";
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + " min";
  if (s < 86400) return Math.floor(s / 3600) + " h";
  if (s < 172800) return "1 day";
  return Math.floor(s / 86400) + " days";
}

/** Warning before opening a file someone else has checked out, or null. */
export function foreignLockText(rev) {
  if (!rev.checked_out_by || rev.checked_out_by === state.me.id) return null;
  return rev.checked_out_by_name + " has checked out " + rev.item_number + "/" + rev.code + " (for " + lockAge(rev.checked_out_at) +
    ") and may be changing it – you can look at the files, but changes you make cannot be checked in until they release it.";
}

/** Opens a file of the workspace tab – with a warning if it is outdated,
 * in conflict or checked out by somebody else. */
export function openLocalFile(tab, f) {
  var rev = tab.data.rev;
  var lockText = foreignLockText(rev);
  function open() { openFile(tab, f.name, true, true); }
  if (f.state === "outdated") {
    dialog("Newer version on the server", h("div.stack", null, [
      h("p", { text: f.name + " in your workspace is v" + (f.base_version || "?") + "; the server has " +
        (f.server_version ? "v" + f.server_version : "removed it") + ". Get the current files first, so you do not work on an old version." }),
      lockText ? h("p.notice", { text: lockText }) : null
    ]), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      { label: "Open my copy", onClick: function (d) { d.close(); open(); } },
      { label: "Get latest and open", primary: true, submit: true, onClick: function (d) {
        d.close();
        getting(call("workspace_download", { project: rev.project_code, item: rev.item_number, code: rev.code }))
          .then(function (r) { reportDownload(r); open(); renderActive(); }, function (e) { toast(errorText(e), "error"); });
      } }
    ]);
    return;
  }
  var warnings = [];
  if (f.state === "conflict") warnings.push(f.name + " was changed here and on the server since you got it (yours: v" + (f.base_version || "?") +
    ", server: " + (f.server_version ? "v" + f.server_version : "removed") + "). Save your work under another name before you get the current version.");
  if (lockText) warnings.push(lockText);
  if (!warnings.length) { open(); return; }
  dialog(f.state === "conflict" ? "Conflicting file" : "Checked out by " + rev.checked_out_by_name,
    h("div.stack", null, warnings.map(function (w) { return h("p", { text: w }); })), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      { label: "Open anyway", primary: true, submit: true, onClick: function (d) { d.close(); open(); } }
    ]);
}

export function openFile(tab, filename, local, warned) {
  var rev = tab.data.rev;
  var lockText = warned ? null : foreignLockText(rev);
  var asked = lockText ? confirmDialog("Checked out by " + rev.checked_out_by_name, lockText, "Open anyway") : Promise.resolve(true);
  asked.then(function (ok) {
    if (!ok) return;
    var ready = local ? Promise.resolve() :
      getting(call("workspace_download", { project: rev.project_code, item: rev.item_number, code: rev.code }));
    return ready.then(function () {
      return call("workspace_open", { project: rev.project_code, item: rev.item_number, code: rev.code, file: filename });
    });
  }).catch(function (e) { if (e) toast(errorText(e), "error"); });
}

export function doCheckout(tab) {
  var rev = tab.data.rev;
  run("Checking out " + rev.item_number + "/" + rev.code, function () {
    return getting(call("workspace_checkout", { project: rev.project_code, item: rev.item_number, code: rev.code }));
  }).then(function (r) {
    // with structure: the parts came read-only, local files were never overwritten
    toast(r.summary || ("Checked out " + rev.item_number + "/" + rev.code + " – the files in " + r.dir + " are writable now"));
    if (r.kept.length || r.clashes.length || r.failed.length || r.unresolved.length) reportDownload(r);
    refreshStructure();
    tab.sub = "workspace";
    reloadActive();
    refreshWorklistCount();
  }, function () {});
}

// Cancelling one's own check-out takes effect at once (the local files stay
// in the workspace); releasing someone else's lock is confirmed first.
export function doCancelCheckout(tab) {
  var rev = tab.data.rev;
  var other = rev.checked_out_by !== state.me.id;
  function release() {
    run("Releasing the check-out", function () {
      return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/cancel-checkout");
    }).then(function () {
      toast("Released the check-out of " + rev.item_number + "/" + rev.code);
      // the unchanged local files become read-only again
      call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }).catch(function () {}).then(function () {
        reloadActive(); refreshWorklistCount();
      });
    }, function () {});
  }
  if (!other) { release(); return; }
  confirmDialog("Force unlock",
    "Release the check-out of " + rev.item_number + "/" + rev.code + " held by " + rev.checked_out_by_name + "?",
    "Force unlock", true).then(function (ok) { if (ok) release(); });
}

/** Follows the check-in upload of the open check-in dialog (`upload-progress`). */
export var uploadWatcher = null;

export function checkinDialog(tab) {
  var rev = tab.data.rev;
  Promise.all([call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }), cadPrograms()]).then(function (r) {
    var ws = r[0], programs = r[1];
    var candidates = ws.files.filter(function (f) { return f.state === "modified" || f.state === "new" || f.state === "conflict"; });
    // conflicting files are offered, but not ticked: the server refuses them anyway
    var boxes = candidates.map(function (f) {
      return { name: f.name, input: h("input", { type: "checkbox", checked: f.state !== "conflict" }), state: f.state };
    });
    var comment = h("input", { type: "text", maxlength: "2000", placeholder: "What changed, e.g. wall thickness 2 → 2.5 mm" });
    var progressLine = h("p.small.muted.hidden", { role: "status" });
    // Like the Vault add-in or the Teamcenter integration: viewables at check-in.
    var exportable = boxes.filter(function (b) { return exportProgram(b.name, programs); });
    var withStep = h("input", { type: "checkbox", checked: true });
    var stepNote = exportable.length ? h("label.check", null, [withStep,
      "Create STEP exports with " + exportable.map(function (b) { return PROGRAM_LABEL[exportProgram(b.name, programs)]; })
        .filter(function (v, i, a) { return a.indexOf(v) === i; }).join(" / ") +
      " first, for the 3D preview (" + exportable.length + (exportable.length === 1 ? " file)" : " files)")]) : null;
    var body = h("div.stack", null, [
      h("p.small.muted", null, ["From ", h("span.mono", { text: ws.dir })]),
      candidates.length ? h("div.stack", null, boxes.map(function (b) {
        return h("label.check", null, [b.input, b.name, " ", badge(b.state, SYNC_LABEL[b.state])]);
      })) : h("p.empty", { text: "No modified or new files – checking in only releases the lock." }),
      boxes.some(function (b) { return b.state === "conflict"; }) ? h("p.notice.error", { text: "Conflict: these files changed on the server since you got them. " +
        "The server refuses to overwrite the newer version (nothing is stored) – save your copy under another name, get the current files and merge." }) : null,
      stepNote,
      field("Comment", comment, "stored with every new version"),
      progressLine
    ]);
    // While the check-in runs the dialog stays open (no Escape, no backdrop):
    // Cancel stops it before the next STEP export, between two chunks of a
    // large file or half-way through the request with the small files –
    // nothing is stored, the revision stays checked out. Once every byte is
    // sent the server finishes the check-in; the dialog then says so.
    var running = false, cancelled = false;
    var opts = { wide: true };
    function stopped() { return { status: 499, message: "Cancelled." }; }
    dialog("Check in " + rev.item_number + "/" + rev.code, body, [
      { label: "Cancel", onClick: function (d) {
        if (!running) { d.close(); return; }
        if (cancelled) return;
        cancelled = true;
        progressLine.textContent = "Stopping …";
        progressLine.classList.remove("hidden");
        call("cancel_upload").catch(function () {});
      } },
      {
        label: "Check in and release", primary: true, submit: true, onClick: function (d) {
          if (running) return;
          running = true; cancelled = false; opts.modal = true;
          var files = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.name; });
          var toExport = stepNote && withStep.checked ? files.filter(function (n) { return exportProgram(n, programs); }) : [];
          function show(text) { if (!cancelled) { progressLine.textContent = text; progressLine.classList.remove("hidden"); } }
          // one export after the other: the CAD program handles one document at a time
          var exported = toExport.reduce(function (chain, name) {
            return chain.then(function () {
              if (cancelled) throw stopped();
              show("Exporting " + name + " to STEP with " + PROGRAM_LABEL[exportProgram(name, programs)] + " …");
              return exportStep(rev, name).then(function (step) { if (files.indexOf(step) < 0) files.push(step); });
            });
          }, Promise.resolve());
          exported.then(function () {
            if (cancelled) throw stopped();
            show("Checking in …");
            uploadWatcher = function (p) { show(uploadLine(p)); };
            return call("workspace_checkin", { project: rev.project_code, item: rev.item_number, code: rev.code, files: files, comment: comment.value });
          }).then(function () {
            running = false; uploadWatcher = null;
            setStatus("");
            d.close();
            toast("Checked in " + rev.item_number + "/" + rev.code + (files.length ? " with " + files.join(", ") : "") +
              (cancelled ? " – the server already had every file when you cancelled" : ""), cancelled ? "info" : null);
            reloadActive();
            refreshWorklistCount();
          }, function (err) {
            running = false; uploadWatcher = null; opts.modal = false;
            setStatus("");
            if (cancelled || isCancelled(err)) {
              d.close();
              toast("Check-in of " + rev.item_number + "/" + rev.code + " cancelled – nothing was stored; the revision is still checked out to you.", "info");
              reloadActive();
              return;
            }
            progressLine.classList.add("hidden");
            d.fail(err);
          });
        }
      }
    ], opts);
  }, function (err) { toast(errorText(err), "error"); });
}

export function editDialog(tab) {
  var rev = tab.data.rev;
  var name = h("input", { type: "text", value: rev.name, maxlength: "200", required: true });
  var material = h("input", { type: "text", value: rev.material, maxlength: "200" });
  var mass = h("input", { type: "text", inputmode: "decimal", value: rev.mass_kg === null ? "" : String(rev.mass_kg) });
  var cost = h("input", { type: "text", inputmode: "decimal", value: rev.unit_cost === null ? "" : String(rev.unit_cost) });
  var desc = h("textarea", { maxlength: "10000" });
  desc.value = rev.description || "";
  function num(input) {
    var v = input.value.trim().replace(",", ".");
    if (!v) return null;
    var n = Number(v);
    if (!isFinite(n)) throw new Error("'" + input.value + "' is not a number.");
    return n;
  }
  dialog("Edit " + rev.item_number + "/" + rev.code, h("div.stack", null, [
    field("Name", name), field("Material", material),
    h("div.grid-2", null, [field("Mass per unit (kg)", mass), field("Unit cost (€)", cost)]),
    field("Description", desc)
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    {
      label: "Save", primary: true, submit: true, onClick: function (d) {
        var body;
        try {
          body = { name: name.value, material: material.value, description: desc.value, mass_kg: num(mass), unit_cost: num(cost) };
        } catch (e) { d.fail(e.message); return; }
        api("PATCH", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code), body).then(function () {
          d.close(); toast("Saved " + rev.item_number + "/" + rev.code); reloadActive();
        }, function (err) { d.fail(err); });
      }
    }
  ], { wide: true });
}

/**
 * Release workflow: creates a change order for this revision, assigns the
 * reviewers (CAD editors of the project group are offered) and submits it.
 */
export function releaseDialog(tab) {
  var rev = tab.data.rev;
  var mine = rev.checked_out_by === state.me.id;
  var title = h("input", { type: "text", maxlength: "200", required: true, value: "Release " + rev.item_number + "/" + rev.code + " – " + rev.name });
  var priority = h("select", null, ["low", "normal", "high", "critical"].map(function (p) {
    return h("option", { value: p, text: label(p), selected: p === "normal" });
  }));
  var desc = h("textarea", { maxlength: "20000", placeholder: "What is released and why" });
  var others = h("input", { type: "text", placeholder: "user names, comma-separated" });
  var submitNow = h("input", { type: "checkbox", checked: true });
  // A change releases a whole product: unreleased BOM children, drawings and
  // specifications have to go with it (the server blocks the submit otherwise).
  var withDeps = h("input", { type: "checkbox", checked: true });
  var reviewerBox = h("div.stack", null, h("p.loading", { text: "Loading the members of " + (rev.project_code || "the project group") }));
  var boxes = [];
  if (rev.project_code) {
    api("GET", "/projects/" + enc(rev.project_code) + "/members").then(function (members) {
      clear(reviewerBox);
      var candidates = members.filter(function (m) {
        return m.active && m.user_id !== state.me.id && (m.role === "editor" || m.role === "admin");
      });
      if (!candidates.length) {
        reviewerBox.appendChild(h("p.hint", { text: "No other CAD editors in " + rev.project_code + " – name reviewers below." }));
      }
      candidates.forEach(function (m) {
        var input = h("input", { type: "checkbox" });
        boxes.push({ username: m.username, input: input });
        reviewerBox.appendChild(h("label.check", null, [input, m.display_name, h("span.dim", { text: " " + m.username + " · " + label(m.role) })]));
      });
    }, function (err) { clear(reviewerBox); reviewerBox.appendChild(h("p.notice.error", { text: errorText(err) })); });
  } else {
    clear(reviewerBox);
  }
  var body = h("div.stack", null, [
    mine ? h("p.notice", { text: "You still hold the check-out. Check in your workspace changes first – submitting releases the lock without uploading anything." }) : null,
    field("Title", title),
    h("div.grid-2", null, [field("Priority", priority), h("label.check", null, [submitNow, "Submit for review now"])]),
    h("label.check", null, [withDeps, "Include unreleased dependencies", h("span.dim", { text: " – BOM children, drawings and specifications in work, level by level" })]),
    field("Description", desc),
    h("h3", { text: "Reviewers" }),
    h("p.hint", { text: "All reviewers must approve; you cannot review your own change (four-eyes principle)." }),
    reviewerBox,
    field("Other reviewers", others, "optional")
  ]);
  dialog("Release " + rev.item_number + "/" + rev.code, body, [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    {
      label: "Create change", primary: true, submit: true, onClick: function (d) {
        var reviewers = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.username; })
          .concat(others.value.split(",").map(function (s) { return s.trim(); }).filter(Boolean));
        if (submitNow.checked && !reviewers.length) { d.fail("Choose at least one reviewer to submit the change."); return; }
        var number = null;
        setStatus("Creating the change …");
        api("POST", "/changes", { title: title.value, description: desc.value, priority: priority.value }).then(function (c) {
          number = c.number;
          return api("POST", "/changes/" + enc(number) + "/items", { item: rev.item_number, revision: rev.code });
        }).then(function () {
          return withDeps.checked ? api("POST", "/changes/" + enc(number) + "/dependencies") : null;
        }).then(function () {
          return reviewers.reduce(function (p, username) {
            return p.then(function () { return api("POST", "/changes/" + enc(number) + "/reviewers", { username: username }); });
          }, Promise.resolve());
        }).then(function () {
          return submitNow.checked ? api("POST", "/changes/" + enc(number) + "/submit") : null;
        }).then(function () {
          setStatus("");
          d.close();
          toast(number + (submitNow.checked ? " submitted for review – " + rev.item_number + "/" + rev.code + " is now In Review" : " created as draft"));
          tab.data = null;
          refreshWorklistCount();
          openChange(number);
        }, function (err) {
          setStatus("");
          d.fail(number ? errorText(err) + " – " + number + " exists as a draft; finish it in its tab." : err);
          if (number) { tab.data = null; openChange(number); }
        });
      }
    }
  ], { wide: true });
}

export function doRevise(tab) {
  reviseRevision(tab.data.rev.item_number, tab.data.rev.code);
}

export function reviseRevision(number, code) {
  confirmDialog("Revise", "Create a new revision of " + number + " based on " + code + "? It is checked out to you.", "Revise").then(function (ok) {
    if (!ok) return;
    run("Creating the next revision", function () {
      return api("POST", "/items/" + enc(number) + "/revisions/" + enc(code) + "/revise");
    }).then(function (nr) {
      toast("Created " + nr.item_number + "/" + nr.code + " (in work, checked out to you)");
      openRevision(nr.item_number, nr.code, "workspace");
      refreshWorklistCount();
    }, function () {});
  });
}

export function doObsolete(tab) {
  var rev = tab.data.rev;
  confirmDialog("Mark obsolete", "Mark " + rev.item_number + "/" + rev.code + " as obsolete? This cannot be undone.", "Mark obsolete", true).then(function (ok) {
    if (!ok) return;
    run("Marking obsolete", function () {
      return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/obsolete");
    }).then(function () { toast(rev.item_number + "/" + rev.code + " is now obsolete"); reloadActive(); }, function () {});
  });
}
