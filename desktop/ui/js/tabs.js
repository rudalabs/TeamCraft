// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Object tabs and the toolbar.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ICONS, append, clear, errorText, fmtDate, fmtSize, h, label, state, svgIcon, tool } from "./core.js";
import { canEdit, checkInAll, refreshWorklistCount } from "./shell.js";
import { selectView } from "./navigator.js";
import { assistantContextChanged } from "./assistant.js";
import { agent, renderAgentTab, schedulePaint } from "./agent.js";
import { loadRevision, openRevision, renderRevision } from "./revision.js";
import { impactGroup, impactGroups } from "./references.js";
import { disposeViewer, lockAge } from "./workspace.js";
import { loadChange, openChange, renderChange } from "./change.js";
import { loadRequest, renderRequest } from "./requests.js";
import { newItemDialog } from "./dialogs.js";

// ======================================================================
// Tabs
// ======================================================================

export function findTab(key) { return state.tabs.filter(function (t) { return t.key === key; })[0]; }

export function openTab(tab) {
  var existing = findTab(tab.key);
  if (!existing) state.tabs.push(tab);
  else if (tab.sub) existing.sub = tab.sub;
  activate(tab.key);
}

export function activate(key) {
  state.active = key;
  var t = findTab(key);
  if (t && (t.kind === "revision" || t.kind === "change")) agent.contextKey = key;
  if (state.view === "agent") schedulePaint(true);
  renderTabs();
  renderActive();
  markSelection();
}

export function markSelection() {
  // re-render cheap navigator selection markers
  document.querySelectorAll("#navigator .node.selected").forEach(function (n) { n.classList.remove("selected"); });
}

export function closeTab(key) {
  var i = state.tabs.findIndex(function (t) { return t.key === key; });
  if (i === -1) return;
  state.tabs.splice(i, 1);
  if (state.active === key) {
    var next = state.tabs[i] || state.tabs[i - 1];
    state.active = next ? next.key : null;
  }
  disposeViewer();
  renderTabs();
  renderActive();
}

export function renderTabs() {
  var bar = $("tabs");
  clear(bar);
  state.tabs.forEach(function (t) {
    bar.appendChild(h("div.tab" + (t.key === state.active ? ".active" : ""), {
      role: "tab", "aria-selected": t.key === state.active ? "true" : "false", title: t.title,
      on: { click: function () { if (state.active !== t.key) activate(t.key); } }
    }, [
      h("span.ico", { text: t.kind === "change" ? "⇄" : t.kind === "request" ? "✎" : t.kind === "agent" ? "✦" : "◆" }),
      h("span.tab-label", { text: t.title }),
      h("button.close", { type: "button", title: "Close (Ctrl+W)", "aria-label": "Close", on: { click: function (e) { e.stopPropagation(); closeTab(t.key); } } }, "×")
    ]));
  });
}

export function activeTab() { return findTab(state.active); }

export function renderActive() {
  disposeViewer();
  assistantContextChanged();
  var body = $("tab-body");
  var toolbar = $("toolbar");
  clear(body);
  clear(toolbar);
  var tab = activeTab();
  if (!tab) {
    renderWelcome(body, toolbar);
    return;
  }
  if (tab.kind === "agent") {
    renderAgentTab(body);
    return;
  }
  body.appendChild(h("p.loading", { text: "Loading " + tab.title }));
  var load = tab.kind === "change" ? loadChange(tab) : tab.kind === "request" ? loadRequest(tab) : loadRevision(tab);
  load.then(function () {
    if (state.active !== tab.key) return;
    clear(body);
    clear(toolbar);
    if (tab.kind === "change") renderChange(tab, body, toolbar);
    else if (tab.kind === "request") renderRequest(tab, body, toolbar);
    else renderRevision(tab, body, toolbar);
    if (state.view === "agent" && agent.contextKey === tab.key) schedulePaint(true);
  }).catch(function (err) {
    // a failed load or a bug while rendering: never leave an empty tab
    if (state.active !== tab.key) return;
    clear(body);
    // a render error may have left half a toolbar whose buttons act on
    // data the tab no longer shows
    clear(toolbar);
    body.appendChild(h("p.notice.error", { text: errorText(err) }));
  });
}

export function reloadActive() {
  var tab = activeTab();
  if (tab) tab.data = null;
  renderActive();
}

export function renderWelcome(body, toolbar) {
  append(toolbar, [
    canEdit() ? tool("New item", "plus", function () { newItemDialog(); }, { primary: true }) : null,
    tool("Refresh", "refresh", function () { selectView(state.view); refreshWorklistCount(); renderActive(); })
  ]);
  var w = state.worklist;
  var welcome = h("div.welcome", null, [
    h("h1", { text: "Home" }),
    h("p.muted", { text: "Open an item from the navigator, or press " + (/Mac/.test(navigator.platform) ? "⌘F" : "Ctrl+F") + " to search." })
  ]);
  body.appendChild(welcome);
  var fill = function (wl) {
    if (!wl) return;
    var s = wl.stats;
    append(welcome, h("div.stats", null, [
      stat(s.items, "Items"), stat(s.revisions_in_work, "WIP"), stat(s.revisions_in_review, "In Review"),
      stat(s.revisions_released, "Released"), stat(s.open_changes, "Open changes"), stat(fmtSize(s.stored_bytes), s.files + " files in the vault")
    ]));
    // the same cards in the same order as the web dashboard: the review
    // queue, the dependents to check, then the own check-outs
    append(welcome, h("section.panel", null, [h("div.panel-head", null, [panelTitle("Waiting for my review", wl.reviews.length)]),
      wl.reviews.length ? table(["Change", "Title", "Submitted"], wl.reviews.map(function (c) {
        var open = function () { openChange(c.number); };
        return { cells: [recordLink(c.number, open), c.title, fmtDate(c.submitted_at)], open: open };
      })) : h("p.empty", { text: "Nothing to review." })]));
    var groups = impactGroups(wl);
    if (groups.length) {
      append(welcome, h("section.panel", null, [h("div.panel-head", null, [panelTitle("Dependents to check", groups.length)]),
        h("p.hint", { text: "Something these items use or reference was released in a new revision or made obsolete. Acknowledge what needs no change, or revise the item." })
      ].concat(groups.map(function (g) { return impactGroup(g, false); }))));
    }
    append(welcome, h("section.panel", null, [h("div.panel-head", null, [panelTitle("Checked out by me", wl.checkouts.length),
      wl.checkouts.length ? h("button.small", { type: "button", title: "Check in every check-out without local changes (releases the locks)",
        on: { click: function () { checkInAll(); } } }, [svgIcon(ICONS.checkin), " Check in all"]) : null]),
      wl.checkouts.length ? table(["Revision", "Name", "Since"], wl.checkouts.map(function (r) {
        var open = function () { openRevision(r.item_number, r.code); };
        return { cells: [recordLink(r.item_number + "/" + r.code, open), r.name,
          h("span", { title: fmtDate(r.checked_out_at), text: lockAge(r.checked_out_at) })], open: open };
      })) : h("p.empty", { text: "Nothing checked out." })]));
  };
  if (w) fill(w);
  else refreshWorklistCount().then(fill, function () {});
}

/** A Home card title with its count as a muted trailing number, like the
 *  web dashboard's card titles and the worklist navigator. */
export function panelTitle(text, n) {
  return h("h2", null, [text + " ", h("span.count", { text: String(n) })]);
}

/** A record number in a table: a link like in the web UI and the dependents
 *  rows, so every number that opens something looks the same. */
export function recordLink(text, open) {
  return h("button.link.mono", { type: "button", on: { click: function (e) { e.stopPropagation(); open(); } } }, text);
}

export function stat(value, text) {
  return h("div.stat", null, [h("div.value", { text: String(value) }), h("div.label", { text: text })]);
}

/** rows: [{cells: [...], open: fn, cls: "selected"}] */
export function table(headers, rows, opts) {
  opts = opts || {};
  return h("div.table-wrap", null, h("table.grid", null, [
    h("thead", null, h("tr", null, headers.map(function (t, i) {
      return h("th" + (opts.numeric && opts.numeric.indexOf(i) !== -1 ? ".num" : ""), { text: t });
    }))),
    h("tbody", null, rows.map(function (r) {
      return h("tr" + (r.open ? ".clickable" : "") + (r.cls ? "." + r.cls : ""), {
        on: r.open ? { dblclick: r.open, click: r.single ? r.open : null } : null,
        title: r.open ? "Double-click to open" : null
      }, r.cells.map(function (c, i) {
        var td = h("td" + (opts.numeric && opts.numeric.indexOf(i) !== -1 ? ".num" : "") +
          (opts.nowrap && opts.nowrap.indexOf(i) !== -1 ? ".nowrap" : ""));
        append(td, c);
        return td;
      }));
    }))
  ]));
}
