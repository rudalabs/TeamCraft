// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Navigator views: home, worklist, changes, search.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ICONS, api, append, badge, clear, dialog, enc, errorText, field, fmtDate, h, label, lockIcon, state, svgIcon, toast } from "./core.js";
import { canEdit, checkInAll, refreshWorklistCount, reloadProjects, renderNotifications } from "./shell.js";
import { assistantContextChanged } from "./assistant.js";
import { agent, renderAgentView } from "./agent.js";
import { openRevision } from "./revision.js";
import { impactGroups, relationShort, relationText } from "./references.js";
import { lockAge } from "./workspace.js";
import { openChange } from "./change.js";
import { openRequest } from "./requests.js";
import { newItemDialog } from "./dialogs.js";
import { getProjectsDialog } from "./group-sync.js";
import { importFolderDialog } from "./folder-import.js";
import { renderFiles } from "./browser.js";
import { cutLevel, describe, normalizeMode, pathKey, quantityText, rootNodes, usageBadge } from "./structure.js";

// ======================================================================
// Navigator views
// ======================================================================

export function selectView(view) {
  state.view = view;
  assistantContextChanged();
  document.querySelectorAll(".rail button[data-view]").forEach(function (b) {
    b.classList.toggle("active", b.getAttribute("data-view") === view);
  });
  var nav = $("navigator");
  clear(nav);
  nav.appendChild(h("p.loading", { text: "Loading" }));
  var render = { home: renderHome, projects: renderProjects, worklist: renderWorklist, changes: renderChanges, search: renderSearch, notifications: renderNotifications, agent: renderAgentView }[view] || renderHome;
  Promise.resolve().then(render).catch(function (err) {
    clear(nav);
    nav.appendChild(h("p.notice.error", { text: errorText(err) }));
  });
}

export function navHead(title, actions) {
  return h("div.nav-head", null, [h("h2", { text: title }), h("div.row", null, actions || [])]);
}

export function itemNode(it, opts) {
  opts = opts || {};
  var key = "rev:" + it.number + ":" + it.latest_code;
  var ref = it.number + "/" + it.latest_code;
  // the file browser of Home leads with the name: a long number prefix would
  // cut the real (file) names off (issue #255)
  var label = opts.nameFirst
    ? [h("span.fname", { text: it.name || ref }), h("span.dim.mono", { text: " " + ref })]
    : [h("span.mono", { text: ref }), " ", it.name];
  return h("li", null, h("div.node" + (state.active === key ? ".selected" : ""), {
    title: ref + " – " + it.name,
    on: { click: function () { openRevision(it.number, it.latest_code); } }
  }, [
    h("span.twisty", null, opts.indent ? "" : ""),
    h("span.ico", { text: typeIcon(it.item_type) }),
    h("span.label", null, label.concat([it.matched_file ? h("span.dim", { text: " · " + it.matched_file }) : null])),
    h("span.meta", null, badge(it.latest_state))
  ]));
}

// ---------------------------------------------------------------- the active project

var ACTIVE_PROJECT_KEY = "tc.activeProject";
var SORT_KEY = "tc.homeSort";

/** The project group Home shows (chosen on the Projects page, kept per
 *  computer); null when none is chosen or it is gone. */
export function activeProject() {
  var code = null;
  try { code = localStorage.getItem(ACTIVE_PROJECT_KEY); } catch (e) { /* storage off */ }
  return state.projects.filter(function (p) { return p.code === code && p.active; })[0] || null;
}

export function setActiveProject(code) {
  try { localStorage.setItem(ACTIVE_PROJECT_KEY, code); } catch (e) { /* storage off */ }
}

var SORTS = [
  ["name", "Name"],
  ["number", "Number"],
  ["type", "Type"],
  ["state", "State"]
];

function homeSort() {
  var s = null;
  try { s = localStorage.getItem(SORT_KEY); } catch (e) { /* storage off */ }
  return SORTS.some(function (o) { return o[0] === s; }) ? s : "name";
}

/** Items of the file browser in the chosen order (name, then number, as the
 *  tie-breaker). */
export function sortItems(items, by) {
  var text = function (v) { return String(v || "").toLowerCase(); };
  var key = {
    name: function (it) { return [text(it.name), it.number]; },
    number: function (it) { return [it.number, ""]; },
    type: function (it) { return [it.item_type, text(it.name)]; },
    state: function (it) { return [it.latest_state, text(it.name)]; }
  }[by] || function (it) { return [text(it.name), it.number]; };
  return items.slice().sort(function (a, b) {
    var x = key(a), y = key(b);
    return x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : x[1] < y[1] ? -1 : x[1] > y[1] ? 1 : 0;
  });
}

export function typeIcon(type) {
  return { part: "◆", assembly: "▣", document: "▤", software: "◇", simulation: "◎", as_built: "◈" }[type] || "•";
}

export function renderHome() {
  // fetched again each time (Home, Refresh, after an import): groups created or joined meanwhile show up
  return reloadProjects().then(drawHome);
}

function myGroups() {
  return state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) !== -1; });
}

function otherGroups() {
  return state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) === -1; });
}

/** Home: a file browser of the active project group (issue #255); the
 *  project is chosen on the Projects page. */
export function drawHome() {
  if (state.view !== "home") return; // switched to another view while loading
  var nav = $("navigator");
  var p = activeProject();
  clear(nav);
  if (!p) {
    nav.appendChild(navHead("Files", []));
    var mine = myGroups();
    nav.appendChild(h("p.empty", { text: "Choose the project group to work in." }));
    nav.appendChild(h("div.row", null, [h("button.primary", { type: "button", on: { click: function () { selectView("projects"); } } }, "Choose a project…")]));
    if (mine.length) {
      nav.appendChild(h("p.section-title", { text: "My project groups" }));
      nav.appendChild(h("ul.tree", null, mine.map(function (g) { return pickNode(g, false); })));
    }
    return;
  }
  nav.appendChild(navHead(p.code, [
    h("button.small", { type: "button", title: "Choose another project group", on: { click: function () { selectView("projects"); } } }, "Change…"),
    canEdit() ? h("button.small", { type: "button", title: "New item (Ctrl+N)", on: { click: function () { newItemDialog(); } } }, [svgIcon(ICONS.plus), "New"]) : null
  ]));
  nav.appendChild(h("p.dim.nav-sub", { text: p.name }));
  nav.appendChild(modeToggle());
  var list = h("ul.tree");
  var count = h("span.count", { text: "" });
  if (navMode() !== "structure") {
    var sort = h("select.small", { "aria-label": "Sort the files by", on: { change: function () {
      try { localStorage.setItem(SORT_KEY, sort.value); } catch (e) { /* storage off */ }
      drawHome();
    } } }, SORTS.map(function (o) { return h("option", { value: o[0], selected: o[0] === homeSort() }, "Sort by " + o[1].toLowerCase()); }));
    nav.appendChild(h("div.row.nav-sort", null, [sort, count]));
  }
  nav.appendChild(list);
  var items = function () { return loadProjectItems(p, list, count, { nameFirst: true, sort: homeSort() }); };
  // Files: the folders and files of the group (browser.js); a server that
  // cannot list them yet gets the items instead
  if (navMode() === "files") renderFiles(p, list, { sort: homeSort(), count: count, olderServer: items });
  else items();
}

/** A project group to choose; `active` marks the current one. */
function pickNode(p, active) {
  return h("li", null, h("div.node.group" + (active ? ".selected" : ""), {
    title: p.name + (p.visibility === "restricted" ? " – restricted: only its members and administrators see it" : ""),
    on: { click: function () { setActiveProject(p.code); selectView("home"); } }
  }, [
    h("span.twisty", { text: active ? "✓" : "" }),
    h("span.ico", { text: "▦" }),
    h("span.label", null, [h("strong", { text: p.code }), h("span.dim", { text: " " + p.name })]),
    h("span.meta", null, p.visibility === "restricted" ? h("span.dim", { text: "restricted" }) : null)
  ]));
}

export function renderProjects() {
  return reloadProjects().then(drawProjects);
}

/** The Projects page: choose the active project group, create one, get
 *  groups into the workspace or import a CAD folder (issue #255). */
export function drawProjects() {
  if (state.view !== "projects") return;
  var nav = $("navigator");
  var current = activeProject();
  var mine = myGroups(), others = otherGroups();
  clear(nav);
  nav.appendChild(navHead("Projects", [
    canEdit() ? h("button.small", { type: "button", title: "Create a project group (you become its manager)", on: { click: function () { newProjectDialog(); } } }, [svgIcon(ICONS.plus), "New"]) : null
  ]));
  nav.appendChild(h("div.row.nav-actions", null, [
    h("button.small", { type: "button", title: "Get project groups: copy their files into your workspace", on: { click: function () { getProjectsDialog(); } } }, [svgIcon(ICONS.download), "Get…"]),
    canEdit() ? h("button.small", { type: "button", title: "Import an existing CAD folder (Tools → Import CAD Folder…)", on: { click: function () { importFolderDialog(); } } }, [svgIcon(ICONS.folder), "Import CAD folder…"]) : null
  ]));
  function group(title, list) {
    if (!list.length) return null;
    return [h("p.section-title", { text: title }), h("ul.tree", null, list.map(function (p) {
      return pickNode(p, !!current && current.code === p.code);
    }))];
  }
  append(nav, group("My project groups", mine));
  append(nav, group(mine.length ? "Other project groups" : "All project groups", others));
  if (!mine.length && !others.length) nav.appendChild(h("p.empty", { text: "No project groups yet." }));
}

/** Creates a project group (CAD editors; the creator becomes member and
 *  manager) and makes it the active one. */
export function newProjectDialog() {
  var code = h("input", { type: "text", maxlength: "20", required: true, placeholder: "CHASSIS", autocomplete: "off" });
  var name = h("input", { type: "text", maxlength: "120", required: true });
  var desc = h("textarea", { rows: "2" });
  var restricted = h("input", { type: "checkbox" });
  dialog("New project group", h("div.stack", null, [
    h("div.grid-2", null, [field("Code", code, "short, unique, e.g. CHASSIS"), field("Name", name)]),
    field("Description", desc, "optional"),
    h("label.check", null, [restricted, " Restricted – only its members and administrators see the group and its items"])
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    {
      label: "Create", primary: true, submit: true, onClick: function (d) {
        api("POST", "/projects", {
          code: code.value.trim(), name: name.value.trim(), description: desc.value,
          visibility: restricted.checked ? "restricted" : "internal"
        }).then(function (p) {
          d.close();
          setActiveProject(p.code);
          toast("Created the project group " + p.code + " – you manage it");
          selectView("home");
        }, function (err) { d.fail(err); });
      }
    }
  ]);
}

export function projectNode(p) {
  var open = !!state.expanded[p.code];
  var children = h("ul.tree" + (open ? "" : ".hidden"));
  var node = h("div.node.group", {
    title: p.name + (p.visibility === "restricted" ? " – restricted: only its members and administrators see it" : ""),
    on: {
      click: function () {
        state.expanded[p.code] = !state.expanded[p.code];
        twisty.textContent = state.expanded[p.code] ? "▾" : "▸";
        children.classList.toggle("hidden", !state.expanded[p.code]);
        if (state.expanded[p.code]) loadProjectItems(p, children, count);
      }
    }
  });
  var twisty = h("span.twisty", { text: open ? "▾" : "▸" });
  var count = h("span.count", { text: "" });
  append(node, [twisty, h("span.ico", { text: "▦" }), h("span.label", null, [h("strong", { text: p.code }), h("span.dim", { text: " " + p.name })]), h("span.meta", null, count)]);
  if (open) loadProjectItems(p, children, count);
  return h("li", null, [node, children]);
}

export function loadProjectItems(p, ul, count, opts) {
  opts = opts || {};
  clear(ul);
  ul.appendChild(h("li.loading", { text: "Loading" }));
  if (navMode() === "structure") return loadStructureRoots(p, ul, count);
  return api("GET", "/items?project=" + enc(p.code) + "&limit=500").then(function (r) {
    clear(ul);
    count.textContent = r.total > r.items.length ? r.items.length + " of " + r.total : String(r.total);
    if (!r.items.length) {
      ul.appendChild(h("li.empty", { text: "No items yet." }));
      return;
    }
    var items = opts.sort ? sortItems(r.items, opts.sort) : r.items;
    items.forEach(function (it) { ul.appendChild(itemNode(it, opts)); });
  }, function (err) {
    clear(ul);
    ul.appendChild(h("li.notice.error", { text: errorText(err) }));
  });
}

/** A worklist heading with its count as a muted trailing number (.count),
 *  the same pattern as the card titles in both apps. */
export function sectionTitle(text, n) {
  return h("p.section-title", null, [text + " ", h("span.count", { text: String(n) })]);
}

export function renderWorklist() {
  return refreshWorklistCount().then(function (w) {
    var nav = $("navigator");
    clear(nav);
    nav.appendChild(navHead("My worklist", [
      w.checkouts.length ? h("button.small", { type: "button", title: "Check in every check-out without local changes (releases the locks)",
        on: { click: function () { checkInAll(); } } }, [svgIcon(ICONS.checkin), " Check in all"]) : null,
      h("button.icon", { type: "button", title: "Refresh", "aria-label": "Refresh", on: { click: function () { selectView("worklist"); } } }, svgIcon(ICONS.refresh))
    ]));
    nav.appendChild(sectionTitle("Checked out by me", w.checkouts.length));
    nav.appendChild(w.checkouts.length ? h("ul.tree", null, w.checkouts.map(function (r) {
      return h("li", null, h("div.node", { title: "Checked out " + fmtDate(r.checked_out_at), on: { click: function () { openRevision(r.item_number, r.code); } } }, [
        h("span.twisty"), h("span.ico", null, lockIcon(true)),
        h("span.label", null, [h("span.mono", { text: r.item_number + "/" + r.code }), " ", r.name]),
        h("span.meta.small.dim", { text: lockAge(r.checked_out_at) })
      ]));
    })) : h("p.empty", { text: "Nothing checked out." }));
    nav.appendChild(sectionTitle("Waiting for my review", w.reviews.length));
    nav.appendChild(w.reviews.length ? h("ul.tree", null, w.reviews.map(changeNode)) : h("p.empty", { text: "Nothing to review." }));
    var groups = impactGroups(w);
    nav.appendChild(sectionTitle("Dependents to check", groups.length));
    nav.appendChild(groups.length ? h("ul.tree", null, groups.map(dependentNode)) : h("p.empty", { text: "Nothing to check." }));
    nav.appendChild(sectionTitle("My open changes", w.my_changes.length));
    nav.appendChild(w.my_changes.length ? h("ul.tree", null, w.my_changes.map(changeNode)) : h("p.empty", { text: "No open changes." }));
    // change requests (older servers: none)
    var assigned = w.requests_assigned || [], mine = w.my_requests || [];
    nav.appendChild(sectionTitle("Requests assigned to me", assigned.length));
    nav.appendChild(assigned.length ? h("ul.tree", null, assigned.map(requestNode)) : h("p.empty", { text: "No change requests assigned to you." }));
    nav.appendChild(sectionTitle("My open requests", mine.length));
    nav.appendChild(mine.length ? h("ul.tree", null, mine.map(requestNode)) : h("p.empty", { text: "Found a problem? Use “Request change…” on a revision or a workspace file." }));
  });
}

/** Worklist: one line per dependent with the number of changes to check;
 *  the twisty lists them (a click on a source opens it). */
export function dependentNode(g) {
  var key = "dep:" + g.revision_id;
  var open = !!state.expanded[key];
  var n = g.impacts.length;
  var children = h("ul.tree" + (open ? "" : ".hidden"), null, g.impacts.map(function (i) {
    return h("li", null, h("div.node", {
      title: g.item_number + " " + relationText(i.relation, i.source_number) + (i.detail ? " (" + i.detail + ")" : "") +
        " – " + i.source_number + "/" + i.source_code + " " + (i.event === "released" ? "was released" : "is obsolete"),
      on: { click: function () { openRevision(i.source_number, i.source_code); } }
    }, [
      h("span.twisty"), h("span.ico.dim", { text: "↳" }),
      h("span.label", null, [h("span.mono", { text: i.source_number + "/" + i.source_code }), " ", h("span.dim", { text: relationShort(i.relation) })]),
      h("span.meta.small.dim", { text: i.event === "released" ? "released" : "obsolete" })
    ]));
  }));
  var twisty = h("button.twisty-btn", {
    type: "button", "aria-expanded": String(open), "aria-label": (open ? "Hide" : "Show") + " the " + n + " changes",
    on: {
      click: function (ev) {
        ev.stopPropagation();
        state.expanded[key] = !state.expanded[key];
        twisty.textContent = state.expanded[key] ? "▾" : "▸";
        twisty.setAttribute("aria-expanded", String(!!state.expanded[key]));
        children.classList.toggle("hidden", !state.expanded[key]);
      }
    }
  }, open ? "▾" : "▸");
  var node = h("div.node.dependent", {
    title: g.item_number + "/" + g.revision_code + " " + g.item_name + " – " + n + " change" + (n === 1 ? "" : "s") + " to check",
    on: { click: function () { openRevision(g.item_number, g.revision_code, "references"); } }
  }, [
    twisty, h("span.ico.warn", { text: "△" }),
    h("span.label", null, [h("span.mono", { text: g.item_number + "/" + g.revision_code }), " ", g.item_name]),
    h("span.meta", null, h("span.count-badge", { text: String(n) }))
  ]);
  return h("li", null, [node, children]);
}

export function requestNode(r) {
  var key = "ecr:" + r.number;
  return h("li", null, h("div.node" + (state.active === key ? ".selected" : ""), {
    title: r.number + " – " + r.title + (r.due_date ? " · due " + r.due_date : ""),
    on: { click: function () { openRequest(r.number); } }
  }, [
    h("span.twisty"), h("span.ico", { text: "✎" }),
    h("span.label", null, [h("span.mono", { text: r.number }), " ", r.title]),
    h("span.meta", null, badge(r.status))
  ]));
}

export function changeNode(c) {
  var key = "chg:" + c.number;
  return h("li", null, h("div.node" + (state.active === key ? ".selected" : ""), {
    title: c.number + " – " + c.title,
    on: { click: function () { openChange(c.number); } }
  }, [
    h("span.twisty"), h("span.ico", { text: "⇄" }),
    h("span.label", null, [h("span.mono", { text: c.number }), " ", c.title]),
    h("span.meta", null, badge(c.status))
  ]));
}

export var CHANGE_FILTERS = [
  { id: "open", label: "Open" }, { id: "draft", label: "Draft" }, { id: "in_review", label: "In Review" },
  { id: "rejected", label: "Rejected" }, { id: "released", label: "Released" }, { id: "all", label: "All" }
];

export function renderChanges() {
  var f = state.changesFilter;
  var query = "?limit=500" + ((f === "open" || f === "all") ? "" : "&status=" + enc(f));
  return api("GET", "/changes" + query).then(function (r) {
    var list = Array.isArray(r) ? r : (r.changes || []);
    if (f === "open") list = list.filter(function (c) { return ["draft", "in_review", "rejected"].indexOf(c.status) !== -1; });
    var nav = $("navigator");
    clear(nav);
    nav.appendChild(navHead("Change orders"));
    nav.appendChild(h("div.nav-filter", null, CHANGE_FILTERS.map(function (cf) {
      return h("button.chip" + (cf.id === f ? ".active" : ""), {
        type: "button", on: { click: function () { state.changesFilter = cf.id; selectView("changes"); } }
      }, cf.label);
    })));
    nav.appendChild(list.length ? h("ul.tree", null, list.map(changeNode)) : h("p.empty", { text: "No changes." }));
  });
}

export function renderSearch() {
  var nav = $("navigator");
  var q = state.query;
  if (!q) {
    clear(nav);
    nav.appendChild(navHead("Search"));
    nav.appendChild(h("p.hint", { text: "Type into the search field above: item numbers, names, materials, descriptions, file names and text inside STEP/IGES files are searched." }));
    $("search-input").focus();
    return null;
  }
  return api("GET", "/items?q=" + enc(q) + "&limit=200").then(function (r) {
    clear(nav);
    state.searchShown = r.items.slice(0, 25).map(function (it) { return it.number; });
    assistantContextChanged();
    nav.appendChild(navHead("Search"));
    nav.appendChild(h("p.small.muted", { text: r.total + " result" + (r.total === 1 ? "" : "s") + " for “" + q + "”" }));
    nav.appendChild(r.items.length ? h("ul.tree", null, r.items.map(function (it) { return itemNode(it); })) : h("p.empty", { text: "Nothing found." }));
  });
}

// ---------------------------------------------------------------- structure view

var MODE_KEY = "teamcraft.navigator.mode";

/** Files (folders and files), All items (flat list) or Structure; remembered per computer. */
export function navMode() {
  if (!state.navMode) {
    try { state.navMode = normalizeMode(window.localStorage.getItem(MODE_KEY)); } catch (e) { state.navMode = normalizeMode(null); }
  }
  return state.navMode;
}

export function setNavMode(mode) {
  state.navMode = normalizeMode(mode);
  try { window.localStorage.setItem(MODE_KEY, state.navMode); } catch (e) { /* not remembered */ }
  if (state.view === "home") selectView("home");
}

/** The toggle in the head of the project list. */
function modeToggle() {
  var mode = navMode();
  var modes = [
    ["files", "Files", "The folders and files of the group, as in its working folder"],
    ["all", "All items", "Every item of the group in one list"],
    ["structure", "Structure", "Assemblies with their parts; parts used nowhere else at the top"]
  ];
  return h("div.nav-filter", null, modes.map(function (m) {
    return h("button.chip" + (m[0] === mode ? ".active" : ""), {
      type: "button", title: m[2],
      "aria-pressed": String(m[0] === mode), on: { click: function () { setNavMode(m[0]); } }
    }, m[1]);
  }));
}

/** Redraws the project list after a lock changed: every row of a shared part
 *  shows the new state at once. */
export function refreshStructure() {
  if (state.view === "home" && navMode() === "structure") selectView("home");
}

function structureRule() {
  return state.structureRule === "released" ? "released" : "latest";
}

/** Top-level items of a group; every assembly expands into its parts. */
function loadStructureRoots(p, ul, count) {
  return api("GET", "/projects/" + enc(p.code) + "/structure?limit=500").then(function (r) {
    clear(ul);
    var top = rootNodes(r);
    count.textContent = String(top.total);
    if (!top.nodes.length) {
      ul.appendChild(h("li.empty", { text: "No items yet." }));
      return;
    }
    top.nodes.forEach(function (n) { ul.appendChild(structureNode(n, "grp:" + p.code)); });
    var cut = cutLevel(top.nodes, top.total, true);
    if (cut.hidden) ul.appendChild(h("li.empty", { text: "… and " + cut.hidden + " more – use the search or All items" }));
  }, function (err) {
    clear(ul);
    ul.appendChild(h("li.notice.error", { text: errorText(err) }));
  });
}

/** One line of the structure view (a BOM line below an assembly, or a top-level item). */
export function structureNode(n, parentKey) {
  var d = describe(n, structureRule());
  var key = pathKey(parentKey, n.item_number || ("line" + n.find_number));
  var open = d.expandable && !!state.expanded[key];
  var children = h("ul.tree" + (open ? "" : ".hidden"));
  var twisty = d.expandable ? h("button.twisty-btn", {
    type: "button", "aria-expanded": String(open), "aria-label": (open ? "Hide" : "Show") + " the parts of " + d.label,
    on: { click: function (ev) {
      ev.stopPropagation();
      state.expanded[key] = !state.expanded[key];
      twisty.textContent = state.expanded[key] ? "▾" : "▸";
      twisty.setAttribute("aria-expanded", String(!!state.expanded[key]));
      children.classList.toggle("hidden", !state.expanded[key]);
      if (state.expanded[key]) loadStructureChildren(n, key, children, false);
    } }
  }, open ? "▾" : "▸") : h("span.twisty");
  var meta = [];
  var qty = quantityText(n);
  if (qty) meta.push(h("span.dim", { text: qty }));
  if (n.code) meta.push(badge(n.state));
  if (n.checked_out_by_name) {
    var mine = state.me && n.checked_out_by_name === state.me.display_name;
    meta.push(h("span.lock-owner", { title: "Checked out by " + n.checked_out_by_name }, [lockIcon(mine), " " + n.checked_out_by_name]));
  }
  var usage = usageBadge(n);
  if (usage) {
    meta.push(h("button.link.small", {
      type: "button", title: "Used in " + n.used_in + " assemblies – open the Where used tab",
      on: { click: function (ev) { ev.stopPropagation(); openRevision(n.item_number, n.code, "whereused"); } }
    }, usage));
  }
  var node = h("div.node" + (state.active === "rev:" + n.item_number + ":" + n.code ? ".selected" : "") + (d.kind === "restricted" ? ".restricted" : ""), {
    title: d.openable ? n.item_number + "/" + n.code + " – " + n.name : d.placeholder,
    "data-item": n.item_number || "",
    on: { click: function () {
      if (!d.openable) return;
      markTwins(n.item_number);
      openRevision(n.item_number, n.code);
    } }
  }, [
    twisty, h("span.ico", { text: n.item_type ? typeIcon(n.item_type) : "•" }),
    d.kind === "revision"
      ? h("span.label", null, [h("span.mono", { text: d.label }), " ", n.name])
      : h("span.label.dim", { text: d.kind === "restricted" ? d.placeholder : d.label + " – " + d.placeholder }),
    h("span.meta", null, meta)
  ]);
  if (open) loadStructureChildren(n, key, children, false);
  return h("li", null, [node, children]);
}

/** Highlights the other appearances of a shared part in the expanded tree. */
function markTwins(number) {
  document.querySelectorAll("#navigator .node.twin").forEach(function (el) { el.classList.remove("twin"); });
  document.querySelectorAll("#navigator .node[data-item]").forEach(function (el) {
    if (el.getAttribute("data-item") === number) el.classList.add("twin");
  });
}

/** The parts of an assembly, one level, loaded when it is expanded. */
function loadStructureChildren(n, key, ul, showAll) {
  clear(ul);
  ul.appendChild(h("li.loading", { text: "Loading" }));
  return api("GET", "/items/" + enc(n.item_number) + "/revisions/" + enc(n.code) + "/structure?rule=" + structureRule()).then(function (r) {
    clear(ul);
    var cut = cutLevel(r.nodes, r.total, showAll);
    if (!r.nodes.length) {
      ul.appendChild(h("li.empty", { text: "No BOM lines." }));
      return;
    }
    cut.shown.forEach(function (c) { ul.appendChild(structureNode(c, key)); });
    if (cut.hidden) {
      ul.appendChild(h("li", null, h("button.link.small", { type: "button", on: { click: function () { loadStructureChildren(n, key, ul, true); } } },
        "show all " + r.total + " lines")));
    }
  }, function (err) {
    clear(ul);
    ul.appendChild(h("li.notice.error", { text: errorText(err) }));
  });
}
