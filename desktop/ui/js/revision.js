// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Revision tab: overview, files, structure, where used.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ICONS, api, append, badge, call, dialog, enc, errorText, fmtDate, fmtNum, fmtSize, h, invoke, label, lockBadge, moreMenu, run, state, svgIcon, toast, tool, toolGroup } from "./core.js";
import { canEdit, canEditProject, savePackage, watchTool } from "./shell.js";
import { typeIcon } from "./navigator.js";
import { openTab, reloadActive, renderActive, renderTabs, table } from "./tabs.js";
import { agentButton } from "./agent.js";
import { groupImpacts, impactGroup, revChanges, revHistory, revReferences } from "./references.js";
import { addFilesDialog, checkinDialog, doCancelCheckout, doCheckout, doObsolete, doRevise, editDialog, filesHint, getStructureDialog, openFile, openWorkspaceFolder, releaseDialog, revViewer, revWorkspace } from "./workspace.js";
import { openChange } from "./change.js";
import { openRequest, requestChangeDialog } from "./requests.js";
import { importBomDialog } from "./bom-import.js";

/** Opens a revision tab; `viewerFile` (a file id) picks the file of its 3D viewer. */
export function openRevision(number, code, sub, viewerFile) {
  openTab({ key: "rev:" + number + ":" + code, kind: "revision", number: number, code: code, title: number + "/" + code, sub: sub || "overview", viewerFile: viewerFile || null });
}

export function loadRevision(tab) {
  if (tab.data) return Promise.resolve(tab.data);
  var base = "/items/" + enc(tab.number);
  return Promise.all([
    api("GET", base + "/revisions/" + enc(tab.code)),
    api("GET", base),
    // references, referrers, CAD file references and impacts (older servers: none)
    api("GET", base + "/revisions/" + enc(tab.code) + "/references").catch(function () { return null; })
  ]).then(function (r) {
    tab.data = {
      rev: r[0].revision, files: r[0].files, bom: r[0].bom, changes: r[0].changes, item: r[1].item, revisions: r[1].revisions,
      requests: r[0].open_requests || [],
      watching: !!r[1].watching,
      refs: r[2] || { outgoing: [], incoming: [], files: [], file_referrers: [], impacts: [] },
      // my check-out: are there local changes? (decides the primary action)
      localChanges: null
    };
    tab.title = tab.data.rev.item_number + "/" + tab.data.rev.code + " " + tab.data.rev.name;
    renderTabs();
    var data = tab.data;
    if (data.rev.checked_out_by !== state.me.id) return data;
    return localChanges(data.rev).then(function (changed) { data.localChanges = changed; data.localChangesFresh = true; return data; });
  });
}

// 3D preview format of a file: the server classifies every file (src/cadformat.rs);
// older servers without format_info fall back to the extension.
export function previewFormat(f) {
  if (f && f.format_info) return f.format_info.preview || null;
  var m = /\.([a-z0-9_]+)$/i.exec((f && f.filename) || "");
  if (!m) return null;
  var ext = m[1].toLowerCase();
  if (ext === "stl") return "stl";
  if (ext === "step" || ext === "stp" || ext === "p21") return "step";
  if (ext === "iges" || ext === "igs") return "iges";
  if (ext === "brep" || ext === "brp") return "brep";
  return null;
}

// Format chip: "SolidWorks assembly", "STEP (ISO 10303)", … or the content type.
export function formatChip(f) {
  var i = f.format_info;
  if (!i) return h("span.dim.small", { text: f.content_type });
  return h("span.fmt.fmt-" + i.kind, { text: i.label, title: i.system_name + " · " + i.kind_label + " · " + f.content_type });
}

export function revisionAbilities(d) {
  var rev = d.rev, me = state.me;
  var inProject = canEditProject(rev.project_id);
  var mine = rev.checked_out_by === me.id;
  var openRevisionExists = d.revisions.some(function (r) { return r.state === "in_work" || r.state === "in_review"; });
  return {
    checkout: inProject && rev.state === "in_work" && !rev.checked_out_by,
    checkin: mine,
    cancel: !!rev.checked_out_by && (mine || me.role === "admin"),
    edit: mine,
    revise: inProject && !openRevisionExists,
    obsolete: inProject && rev.state === "released",
    release: inProject && rev.state === "in_work" && (!rev.checked_out_by || mine) &&
      !d.changes.some(function (c) { return ["draft", "in_review", "rejected"].indexOf(c.status) !== -1; })
  };
}

/** Why a toolbar action of a revision is disabled – shown as its tooltip. */
export function revisionReasons(d) {
  var rev = d.rev, me = state.me;
  var group = rev.project_code ? "project group " + rev.project_code : "the project group";
  var notEditor = canEditProject(rev.project_id) ? null : "Only CAD editors of " + group + " can do this";
  var stateName = label(rev.state);
  var other = rev.checked_out_by && rev.checked_out_by !== me.id ? rev.checked_out_by_name : null;
  var openChange = d.changes.filter(function (c) { return ["draft", "in_review", "rejected"].indexOf(c.status) !== -1; })[0];
  var openRev = d.revisions.filter(function (r) { return r.state === "in_work" || r.state === "in_review"; })[0];
  return {
    checkout: notEditor || (rev.state !== "in_work" ? "Only revisions in work can be checked out – this one is " + stateName +
      (rev.state === "released" ? "; Revise creates the next revision" : "") : null),
    checkin: other ? "Checked out by " + other + " – only they can check it in" : "Check out first",
    cancel: other ? "Checked out by " + other + " – only they or an administrator can cancel the check-out" : "Not checked out",
    edit: other ? "Checked out by " + other : notEditor ? notEditor : rev.state !== "in_work" ? "Only revisions in work can be edited – this one is " + stateName : "Check out first – the attributes change under your check-out",
    release: notEditor || (rev.state !== "in_work" ? "Only revisions in work can be released – this one is " + stateName :
      other ? "Checked out by " + other + " – they check it in first" :
      openChange ? "Already in change order " + openChange.number + " (" + label(openChange.status) + ")" : null),
    revise: notEditor || (openRev ? "Revision " + openRev.code + " is still " + label(openRev.state) + " – finish it before the next one" : null),
    obsolete: notEditor || (rev.state !== "released" ? "Only released revisions can be made obsolete – this one is " + stateName : null)
  };
}

/**
 * The most likely next step on a revision – drawn as the toolbar's one
 * primary button: check out a free revision in work; check in what was
 * changed locally; release a clean check-out; revise a released revision.
 * Nothing while it is in review, obsolete or locked by someone else.
 */
export function revisionNextStep(d, can) {
  var mine = d.rev.checked_out_by === state.me.id;
  if (mine) return d.localChanges === false && can.release ? "release" : "checkin";
  if (can.checkout) return "checkout";
  if (d.rev.state === "released" && can.revise) return "revise";
  return null;
}

/** Whether the user's working copy of a revision has changes to check in
 * (null when the workspace cannot be read in time). */
export function localChanges(rev) {
  var status = call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }).then(function (ws) {
    return ws.files.some(function (f) { return f.state === "modified" || f.state === "new" || f.state === "conflict"; });
  }, function () { return null; });
  var timeout = new Promise(function (resolve) { setTimeout(function () { resolve(null); }, 1500); });
  return Promise.race([status, timeout]);
}

export function renderRevision(tab, body, toolbar) {
  var d = tab.data, rev = d.rev, can = revisionAbilities(d), why = revisionReasons(d);
  var latest = d.revisions[0];
  var next = revisionNextStep(d, can);
  // Buttons keep their place in every state; only the next step is primary.
  function act(id, text, icon, onClick, title) {
    return tool(text, icon, onClick, { primary: next === id, disabled: !can[id], title: can[id] ? title : why[id] });
  }
  var checkinTitle = "Upload the changed files from your workspace and release the lock";
  var noChanges = " (no local changes found)";
  var lock = rev.checked_out_by ? [
    act("checkin", "Check in…", "checkin", function () { checkinDialog(tab); },
      checkinTitle + (d.localChanges === false ? noChanges : "")),
    act("cancel", rev.checked_out_by !== state.me.id ? "Force unlock" : "Cancel check-out", "cancel", function () { doCancelCheckout(tab); },
      rev.checked_out_by !== state.me.id ? "Take the lock away from " + rev.checked_out_by_name + " (administrator)" : "Release the lock without uploading anything")
  ] : [
    act("checkout", "Check out", "checkout", function () { doCheckout(tab); }, "Take the lock and copy the files into your workspace")
  ];
  var releaseButton = act("release", "Release…", "release", function () { releaseDialog(tab); }, "Create a change order with reviewers and submit it");
  // The working copy changes while the tab stays open (the CAD program saves
  // into it): look again when the tab is shown from its cache or the window
  // gets the focus back, so "Release…" is not highlighted over unsaved work.
  tab.recheckLocal = null;
  if (rev.checked_out_by === state.me.id) {
    tab.recheckLocal = function () {
      localChanges(rev).then(function (changed) {
        if (changed === null || changed === d.localChanges || tab.data !== d) return;
        d.localChanges = changed;
        var step = revisionNextStep(d, can);
        lock[0].classList.toggle("primary", step === "checkin");
        releaseButton.classList.toggle("primary", step === "release");
        lock[0].title = checkinTitle + (changed === false ? noChanges : "");
      });
    };
    if (!d.localChangesFresh) tab.recheckLocal();
  }
  d.localChangesFresh = false;
  append(toolbar, [
    toolGroup("Lock", lock),
    toolGroup("Work", [
      act("edit", "Edit", "edit", function () { editDialog(tab); }, "Edit the attributes"),
      tool("Get…", "download", function () { getStructureDialog(rev); }, { title: "Copy this item and everything it consists of (BOM, referenced CAD files) into your working folder" }),
      tool("Open folder", "folder", function () { openWorkspaceFolder(tab); }, { title: "Open the working folder of this revision" })
    ]),
    toolGroup("Lifecycle", [
      releaseButton,
      act("revise", "Revise", "revise", function () { doRevise(tab); }, "Create the next revision")
    ]),
    toolGroup("Collaborate", [
      tool("Request change…" + (d.requests.length ? " (" + d.requests.length + ")" : ""), "request", function () { requestChangeDialog(rev, null); },
        { title: "Report a problem or propose a change to " + rev.item_number + "/" + rev.code + " (change request)" }),
      watchTool(tab, "items", rev.item_number, d.watching),
      agentButton(tab)
    ]),
    moreMenu([
      { label: "Pack and Go…", icon: "package", run: function () { savePackage(rev, null); }, title: "Save this revision with all files of its structure as ZIP (bom.csv, manifest.json) – for sending, not for working" },
      { label: "Add files…", icon: "plus", run: function () { addFilesDialog(tab); }, disabled: !filesHint(d).ok, title: filesHint(d).text },
      { label: "Import BOM…", icon: "download", run: function () { importBomDialog(rev); }, disabled: !can.edit, title: can.edit ? "Add the lines of a BOM exported by the CAD program (CSV, Excel, STEP assembly)" : why.edit },
      { label: "Open in browser", icon: "web", run: function () { call("open_web", { path: "/revisions/" + rev.id }).catch(function (e) { toast(errorText(e), "error"); }); }, title: "Open this revision in the web UI" },
      { label: "Refresh", icon: "refresh", run: reloadActive, shortcut: "F5", title: "Load this revision again" },
      { label: "Mark obsolete…", icon: "obsolete", run: function () { doObsolete(tab); }, disabled: !can.obsolete, danger: true, separated: true, title: can.obsolete ? "Retire this released revision – cannot be undone" : why.obsolete }
    ])
  ]);

  body.appendChild(h("div.object-head", null, [
    h("div", null, [
      h("p.kicker", { text: label(rev.item_type) + (rev.project_code ? " · " + rev.project_code : "") }),
      h("h1", null, [h("span.mono", { text: rev.item_number + "/" + rev.code }), " · " + rev.name]),
      h("div.meta", null, [
        badge(rev.state),
        rev.checked_out_by_name ? lockBadge(rev.checked_out_by_name, rev.checked_out_by === state.me.id) : null,
        h("span", { text: "created " + fmtDate(rev.created_at) + " by " + rev.created_by_name })
      ])
    ])
  ]));
  if (latest && latest.id !== rev.id) {
    body.appendChild(h("p.notice", null, ["This is an older revision. ",
      h("button.link", { type: "button", on: { click: function () { openRevision(rev.item_number, latest.code); } } }, "Open the latest revision " + latest.code)]));
  }
  if (rev.checked_out_by && rev.checked_out_by !== state.me.id) {
    body.appendChild(h("p.notice", { text: "Checked out by " + rev.checked_out_by_name + " since " + fmtDate(rev.checked_out_at) + " – only they can change it right now." }));
  }
  groupImpacts(d.refs.impacts.filter(function (i) { return !i.resolved_at; })).forEach(function (g) {
    body.appendChild(impactGroup(g, true));
  });
  if (d.requests.length) {
    body.appendChild(h("p.notice", null, [d.requests.length + " open change request" + (d.requests.length === 1 ? "" : "s") + " on " + rev.item_number + ": "]
      .concat(d.requests.map(function (r, i) {
        return h("span", null, [i ? " · " : "", h("button.link.mono", { type: "button", on: { click: function () { openRequest(r.number); } } }, r.number),
          " " + r.title + " (" + label(r.status) + ")"]);
      }))));
  }

  var files3d = d.files.filter(function (f) { return previewFormat(f); });
  var subs = [
    { id: "overview", label: "Overview" },
    { id: "files", label: "Files", count: d.files.length },
    { id: "structure", label: "Structure", count: d.bom.length },
    { id: "whereused", label: "Where used" },
    {
      id: "references", label: "References",
      count: d.refs.outgoing.length + d.refs.incoming.length + d.refs.file_referrers.length +
        d.refs.files.reduce(function (n, f) { return n + f.references.length; }, 0)
    },
    { id: "changes", label: "Changes", count: d.changes.length },
    { id: "history", label: "History" },
    { id: "viewer", label: "3D viewer", count: files3d.length, hide: !files3d.length },
    { id: "workspace", label: "Workspace" }
  ];
  var content = h("div.subcontent");
  body.appendChild(h("div.subtabs", { role: "tablist" }, subs.filter(function (s) { return !s.hide; }).map(function (s) {
    return h("button.subtab" + (tab.sub === s.id ? ".active" : ""), {
      type: "button", role: "tab",
      on: { click: function () { tab.sub = s.id; renderActive(); } }
    }, [s.label, s.count !== undefined ? h("span.count", { text: " " + s.count }) : null]);
  })));
  body.appendChild(content);
  var render = {
    overview: revOverview, files: revFiles, structure: revStructure, whereused: revWhereUsed, references: revReferences,
    changes: revChanges, history: revHistory, viewer: revViewer, workspace: revWorkspace
  }[tab.sub] || revOverview;
  Promise.resolve().then(function () { return render(tab, content); }).catch(function (err) {
    content.appendChild(h("p.notice.error", { text: errorText(err) }));
  });
}

export function revOverview(tab, el) {
  var d = tab.data, rev = d.rev;
  var props = h("dl.props", null, [
    dt("Item number"), dd(h("span.mono", { text: rev.item_number })),
    dt("Revision"), dd(rev.code),
    dt("Name"), dd(rev.name),
    dt("Type"), dd(label(rev.item_type)),
    dt("Project group"), dd(rev.project_code || "—"),
    dt("Unit"), dd(rev.unit),
    dt("Material"), dd(rev.material || "—"),
    dt("Mass per unit"), dd(rev.mass_kg !== null ? fmtNum(rev.mass_kg, 4) + " kg" : "—"),
    dt("Unit cost"), dd(rev.unit_cost !== null ? fmtNum(rev.unit_cost, 2) + " €" : "—"),
    dt("Description"), dd(h("span.pre", { text: rev.description || "—" })),
    dt("Created"), dd(fmtDate(rev.created_at) + " · " + rev.created_by_name),
    dt("Last modified"), dd(fmtDate(rev.updated_at)),
    dt("Released"), dd(fmtDate(rev.released_at)),
    dt("Similar parts"), dd(d.files.some(function (f) { var p = previewFormat(f); return p && ["pts", "xyz", "las"].indexOf(p) < 0; }) ? h("button.link", { type: "button", title: "Parts with a similar 3D shape (web UI)", on: { click: function () { call("open_web", { path: "/revisions/" + rev.id + "#similar" }).catch(function (e) { toast(errorText(e), "error"); }); } } }, "Compare the shape in the browser") : "— (no STEP or mesh file)")
  ]);
  var states = ["in_work", "in_review", "released", "obsolete"];
  var next = {
    in_work: "Next: In Review – through a change order (Changes) submitted by a CAD editor of the project group.",
    in_review: "Released when every reviewer approves; back to WIP on rejection or withdrawal.",
    released: "Next: Obsolete (CAD editors of the project group). New work happens in a new revision (Revise).",
    obsolete: "Final state."
  }[rev.state];
  append(el, h("div.grid-2", null, [
    h("section.panel", null, [h("h2", { text: "Properties" }), props]),
    h("div", null, [
      revGlance(tab),
      h("section.panel", null, [
        h("h2", { text: "Lifecycle" }),
        h("ol.lifecycle-track", null, states.map(function (s) { return h("li" + (s === rev.state ? ".current" : ""), null, badge(s)); })),
        h("p.small.muted", { text: next })
      ]),
      h("section.panel", null, [
        h("h2", { text: "Revisions" }),
        table(["Rev", "Name", "State", "Created", "Released"], d.revisions.map(function (r) {
          return {
            cells: [h("span.mono", { text: r.code }), r.name, badge(r.state), fmtDate(r.created_at), fmtDate(r.released_at)],
            open: function () { openRevision(rev.item_number, r.code); }, single: true,
            cls: r.id === rev.id ? "selected" : null
          };
        }))
      ])
    ])
  ]));
}

/**
 * Right column of the overview: the preview image stored in the CAD file
 * (or the way into the 3D viewer) and the key numbers of the revision;
 * each number opens its sub-tab. Only "Where used" costs a request.
 */
export function revGlance(tab) {
  var d = tab.data;
  function go(sub) { return function () { tab.sub = sub; renderActive(); }; }
  function fact(value, text, sub, title) {
    var v = h("span.fact-value", { text: String(value) });
    var body = [v, h("span.fact-label", { text: text })];
    var el = sub ? h("button.fact", { type: "button", title: title || "Open " + text, on: { click: go(sub) } }, body) : h("div.fact", { title: title || null }, body);
    return { el: el, value: v };
  }
  var refCount = d.refs.outgoing.length + d.refs.incoming.length + d.refs.file_referrers.length +
    d.refs.files.reduce(function (n, f) { return n + f.references.length; }, 0);
  var openChanges = d.changes.filter(function (c) { return ["draft", "in_review", "rejected"].indexOf(c.status) !== -1; }).length;
  var whereUsed = fact(d.whereUsed ? d.whereUsed.length : "…", "Where used", "whereused", "Assemblies that use this item");
  var facts = [
    fact(d.files.length, d.files.length === 1 ? "File" : "Files", "files"),
    fact(d.bom.length, d.bom.length === 1 ? "BOM line" : "BOM lines", "structure", "Open the structure"),
    whereUsed,
    fact(refCount, refCount === 1 ? "Reference" : "References", "references"),
    fact(d.changes.length, "Changes", "changes",
      "Change orders with this revision" + (openChanges ? " – " + openChanges + " open" : "")),
    fact(d.requests.length, "Requests", null, "Open change requests on this item – listed above the sub-tabs")
  ];
  if (!d.whereUsed) {
    api("GET", "/items/" + enc(d.rev.item_number) + "/where-used").then(function (list) {
      d.whereUsed = list;
      whereUsed.value.textContent = String(list.length);
    }, function () { whereUsed.value.textContent = "—"; });
  }

  var preview = null, glanceBody = null;
  var withThumb = d.files.filter(function (f) { return f.format_info && f.format_info.thumbnail; })[0];
  var files3d = d.files.filter(function (f) { return previewFormat(f); });
  var viewerButton = files3d.length ? h("button.glance-3d", {
    type: "button", title: "Open the 3D viewer (" + files3d.length + " file" + (files3d.length === 1 ? "" : "s") + ")", on: { click: go("viewer") }
  }, [svgIcon(ICONS.cube), "3D viewer"]) : null;
  if (withThumb) {
    var img = thumbImage(withThumb);
    img.classList.remove("file-thumb");
    img.classList.add("glance-thumb");
    img.title = "Preview image stored in " + withThumb.filename;
    preview = h("figure.glance-preview", null, [img, h("figcaption", null, [h("span.glance-file", { text: withThumb.filename }), viewerButton])]);
    // the file has no image after all: keep only the way into the viewer
    thumbCache[withThumb.sha256].catch(function () {
      if (viewerButton) { img.remove(); return; }
      preview.remove();
      if (glanceBody) glanceBody.classList.add("no-preview");
    });
  } else if (viewerButton) {
    preview = h("div.glance-preview", null, [viewerButton]);
  }
  glanceBody = h("div.glance-body" + (preview ? "" : ".no-preview"), null, [
    preview,
    h("div.facts", null, facts.map(function (f) { return f.el; }))
  ]);
  return h("section.panel.glance", null, [h("h2", { text: "At a glance" }), glanceBody]);
}

export function dt(t) { return h("dt", { text: t }); }
export function dd(v) { var e = h("dd"); append(e, v); return e; }

// ------------------------------------------- previews of native CAD files

export var thumbCache = {};
/** The preview image stored in an Inventor, Solid Edge or SolidWorks file. */
export function thumbImage(f) {
  var img = h("img.file-thumb.hidden", { alt: "", title: "Preview image stored in the file" });
  var url = thumbCache[f.sha256] || (thumbCache[f.sha256] = invoke("file_bytes", { kind: "thumbnail", id: f.id })
    .then(function (bytes) { return URL.createObjectURL(new Blob([bytes])); }));
  url.then(function (u) { img.src = u; img.classList.remove("hidden"); }, function () { /* the file has none */ });
  return img;
}

export var cadProgramsPromise = null;
/** CAD programs on this computer that export STEP: "inventor", "solid-edge" (Windows). */
export function cadPrograms() {
  if (!cadProgramsPromise) cadProgramsPromise = call("cad_programs").catch(function () { return []; });
  return cadProgramsPromise;
}

export var PROGRAM_LABEL = { "inventor": "Inventor", "solid-edge": "Solid Edge" };

/** The local program that can export this file to STEP, or null. */
export function exportProgram(name, programs) {
  var m = /\.([a-z0-9]+)$/i.exec(name || "");
  var ext = m ? m[1].toLowerCase() : "";
  if ((ext === "ipt" || ext === "iam") && programs.indexOf("inventor") >= 0) return "inventor";
  if (["par", "psm", "pwd", "asm"].indexOf(ext) >= 0 && programs.indexOf("solid-edge") >= 0) return "solid-edge";
  return null;
}

/** STEP export of a workspace file with the local CAD program; resolves to the STEP name. */
export function exportStep(rev, name) {
  return call("cad_export_step", { project: rev.project_code, item: rev.item_number, code: rev.code, file: name });
}

export function revFiles(tab, el) {
  var d = tab.data, can = revisionAbilities(d);
  if (!d.files.length) {
    el.appendChild(h("p.empty", { text: "No files attached." }));
    return;
  }
  var rows = d.files.map(function (f) {
    var fmt = previewFormat(f);
    return {
      cells: [
        f.format_info && f.format_info.thumbnail
          ? h("span.file-cell", null, [thumbImage(f), h("span", null, [f.filename, " ", formatChip(f)])])
          : h("span", null, [f.filename, " ", formatChip(f)]),
        h("span.badge", { text: "v" + f.version }),
        fmtSize(f.size_bytes),
        fmtDate(f.uploaded_at) + " · " + f.uploaded_by_name,
        h("span.mono.small.dim", { text: f.sha256.slice(0, 12) + "…", title: f.sha256 }),
        h("span.row", null, [
          fmt ? h("button.small", { type: "button", on: { click: function () { tab.sub = "viewer"; tab.viewerFile = f.id; renderActive(); } } }, "3D") : null,
          h("button.small", { type: "button", title: "Copy into the workspace and open with the default application", on: { click: function () { openFile(tab, f.filename); } } }, "Open"),
          h("button.small", { type: "button", on: { click: function () { showVersions(tab, f, can.edit); } } }, "Versions"),
          h("button.small", { type: "button", title: "Request a change to " + f.filename + " (change request)", on: { click: function () { requestChangeDialog(d.rev, f.filename); } } }, "Request…")
        ])
      ]
    };
  });
  el.appendChild(h("section.panel", null, [
    h("div.panel-head", null, [h("h2", { text: "Files (datasets)" }),
      h("div.row", null, [
        h("span.hint", { text: filesHint(d).text }),
        h("button.small", { type: "button", disabled: !filesHint(d).ok, title: filesHint(d).text, on: { click: function () { addFilesDialog(tab); } } }, "Add files…")])]),
    table(["File", "Version", "Size", "Uploaded", "SHA-256", ""], rows, { numeric: [2] })
  ]));
}

export function showVersions(tab, f, canRestore) {
  api("GET", "/files/" + f.id + "/versions").then(function (versions) {
    var body = table(["Version", "Comment", "Size", "Uploaded", ""], versions.map(function (v) {
      return {
        cells: [
          h("span", null, [h("span.badge", { text: "v" + v.version }), v.version === f.version ? h("span.badge.badge-released", { text: "current" }) : null]),
          v.comment || "—", fmtSize(v.size_bytes), fmtDate(v.uploaded_at) + " · " + v.uploaded_by_name,
          h("span.row", null, [
            previewFormat(f) ? h("button.small", { type: "button", on: { click: function () { dlg.close(); tab.sub = "viewer"; tab.viewerFile = f.id; tab.viewerVersion = v.id; renderActive(); } } }, "3D") : null,
            canRestore && v.version !== f.version ? h("button.small", {
              type: "button", on: {
                click: function () {
                  run("Restoring v" + v.version, function () {
                    return api("POST", "/files/" + f.id + "/restore", { version: v.version });
                  }).then(function (nf) { dlg.close(); toast("Restored " + f.filename + " – it is now v" + nf.version); reloadActive(); }, function () {});
                }
              }
            }, "Restore") : null
          ])
        ]
      };
    }), { numeric: [2] });
    var dlg = dialog("Versions of " + f.filename, body, [{ label: "Close", primary: true, onClick: function (dd) { dd.close(); } }], { wide: true });
  }, function (err) { toast(errorText(err), "error"); });
}

export function revStructure(tab, el) {
  var rule = tab.bomRule || "latest";
  var d = tab.data, rev = d.rev;
  var head = h("div.panel-head", null, [
    h("h2", { text: "Product structure" }),
    h("div.row", null, [
      h("span.hint", { text: "Revision rule" }),
      h("button.chip" + (rule === "latest" ? ".active" : ""), { type: "button", on: { click: function () { tab.bomRule = "latest"; renderActive(); } } }, "Latest"),
      h("button.chip" + (rule === "released" ? ".active" : ""), { type: "button", on: { click: function () { tab.bomRule = "released"; renderActive(); } } }, "Latest released"),
      h("button.chip" + (rule === "as_released" ? ".active" : ""), {
        type: "button", title: "Released assemblies show exactly the child revisions they were released with",
        on: { click: function () { tab.bomRule = "as_released"; renderActive(); } }
      }, "As released"),
      h("button.small", {
        type: "button", title: "Save the structure with this revision rule and all its files as ZIP",
        on: { click: function () { savePackage(rev, rule); } }
      }, "Pack and Go…"),
      canEdit() ? h("button.small", {
        type: "button", disabled: rev.checked_out_by !== state.me.id,
        title: rev.checked_out_by === state.me.id ? "Add the lines of a BOM exported by the CAD program (CSV, Excel, STEP assembly)"
          : rev.checked_out_by ? "Checked out by " + rev.checked_out_by_name : "Check out first – the BOM is imported under your check-out",
        on: { click: function () { importBomDialog(rev); } }
      }, "Import BOM…") : null,
      h("button.small", { type: "button", on: { click: function () { setExpandAll(tab, true); renderActive(); } } }, "Expand all"),
      h("button.small", { type: "button", on: { click: function () { setExpandAll(tab, false); renderActive(); } } }, "Collapse all")
    ])
  ]);
  var panel = h("section.panel", null, [head, h("p.loading", { text: "Loading structure" })]);
  el.appendChild(panel);
  return api("GET", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/bom?expand=true&rule=" + rule).then(function (tree) {
    panel.removeChild(panel.lastChild);
    tab.bomCollapsed = tab.bomCollapsed || {};
    tab.bomTree = tree;
    var rows = [{
      cells: [
        h("span", null, [h("span.ico", { text: typeIcon(rev.item_type) + " " }), h("strong.mono", { text: tree.root_item_number + "/" + tree.root.code })]),
        tree.root.name, badge(tree.root.state), "", "",
        fmtNum(tree.total_mass_kg, 3) + " kg" + (tree.mass_complete ? "" : " ⚠"),
        fmtNum(tree.total_cost, 2) + " €" + (tree.cost_complete ? "" : " ⚠")
      ],
      cls: "selected"
    }];
    function walk(nodes, depth) {
      nodes.forEach(function (n) {
        var hasChildren = n.children && n.children.length;
        var collapsed = !!tab.bomCollapsed[n.line_id];
        var twisty = hasChildren ? h("button.twisty-btn", {
          type: "button", title: collapsed ? "Expand" : "Collapse",
          on: { click: function (e) { e.stopPropagation(); tab.bomCollapsed[n.line_id] = !collapsed; renderActive(); } }
        }, collapsed ? "▸" : "▾") : h("span.twisty-btn", null, "");
        var indent = h("span.indent");
        indent.style.width = (depth * 1.2) + "rem";
        var r = n.revision;
        rows.push({
          cells: [
            h("span", null, [indent, twisty, h("span.mono", { text: String(n.find_number) + "  " }), h("span.mono", { text: n.item_number + (r ? "/" + r.code : "") })]),
            n.restricted ? h("span.muted", { text: "Restricted item – in a project group you are not a member of" })
              : r ? r.name : h("span.warn-text", { text: "no " + (rule === "released" ? "released " : "") + "revision" }),
            r ? badge(r.state) : "",
            fmtNum(n.quantity, 6) + " " + n.unit,
            n.notes || "",
            fmtNum(n.total_mass_kg, 3) + " kg" + (n.mass_complete ? "" : " ⚠"),
            fmtNum(n.total_cost, 2) + " €" + (n.cost_complete ? "" : " ⚠")
          ],
          open: r ? function () { openRevision(n.item_number, r.code); } : null
        });
        if (hasChildren && !collapsed) walk(n.children, depth + 1);
      });
    }
    walk(tree.children, 1);
    panel.appendChild(table(["Find no. / item", "Name", "State", "Qty", "Notes", "Mass", "Cost"], rows, { numeric: [3, 5, 6] }));
    panel.appendChild(h("p.hint", { text: tree.line_count + " lines" + (tree.unresolved_count ? " · " + tree.unresolved_count + " without a matching revision" : "") +
      (tree.restricted_lines ? " · mass and cost exclude " + tree.restricted_lines + " restricted line" + (tree.restricted_lines === 1 ? "" : "s") : "") +
      " · double-click a line to open it · ⚠ = incomplete mass or cost" }));
  });
}

export function setExpandAll(tab, expand) {
  tab.bomCollapsed = {};
  if (expand || !tab.bomTree) return;
  (function walk(nodes) {
    nodes.forEach(function (n) {
      if (n.children && n.children.length) { tab.bomCollapsed[n.line_id] = true; walk(n.children); }
    });
  })(tab.bomTree.children);
}

export function revWhereUsed(tab, el) {
  var rev = tab.data.rev;
  return api("GET", "/items/" + enc(rev.item_number) + "/where-used").then(function (list) {
    el.appendChild(h("section.panel", null, [h("h2", { text: "Where used" }),
      list.length ? table(["Parent", "Name", "State", "Find no.", "Qty"], list.map(function (w) {
        return { cells: [h("span.mono", { text: w.parent_number + "/" + w.parent_code }), w.parent_name, badge(w.parent_state), String(w.find_number), fmtNum(w.quantity, 6)], open: function () { openRevision(w.parent_number, w.parent_code); } };
      }), { numeric: [3, 4] }) : h("p.empty", { text: "Not used in any structure." })]));
  });
}
