// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Navigator views: home, worklist, changes, search.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ICONS, api, append, badge, clear, enc, errorText, field, fmtDate, h, label, lockIcon, state, svgIcon } from "./core.js";
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
  var render = { home: renderHome, worklist: renderWorklist, changes: renderChanges, search: renderSearch, notifications: renderNotifications, agent: renderAgentView }[view] || renderHome;
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
  return h("li", null, h("div.node" + (state.active === key ? ".selected" : ""), {
    title: it.number + "/" + it.latest_code + " – " + it.name,
    on: { click: function () { openRevision(it.number, it.latest_code); } }
  }, [
    h("span.twisty", null, opts.indent ? "" : ""),
    h("span.ico", { text: typeIcon(it.item_type) }),
    h("span.label", null, [h("span.mono", { text: it.number + "/" + it.latest_code }), " ", it.name,
      it.matched_file ? h("span.dim", { text: " · " + it.matched_file }) : null]),
    h("span.meta", null, badge(it.latest_state))
  ]));
}

export function typeIcon(type) {
  return { part: "◆", assembly: "▣", document: "▤", software: "◇", simulation: "◎", as_built: "◈" }[type] || "•";
}

export function renderHome() {
  // fetched again each time (Home, Refresh, after an import): groups created or joined meanwhile show up
  return reloadProjects().then(drawHome);
}

export function drawHome() {
  if (state.view !== "home") return; // switched to another view while loading
  var nav = $("navigator");
  var mine = state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) !== -1; });
  var others = state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) === -1; });
  clear(nav);
  nav.appendChild(navHead("Projects", [
    h("button.small.icon", { type: "button", title: "Get project groups: copy their files into your workspace", "aria-label": "Get project groups…", on: { click: function () { getProjectsDialog(); } } }, svgIcon(ICONS.download)),
    canEdit() ? h("button.small.icon", { type: "button", title: "Import an existing CAD folder (Tools → Import CAD Folder…)", "aria-label": "Import CAD folder…", on: { click: function () { importFolderDialog(); } } }, svgIcon(ICONS.folder)) : null,
    canEdit() ? h("button.small", { type: "button", title: "New item (Ctrl+N)", on: { click: function () { newItemDialog(); } } }, [svgIcon(ICONS.plus), "New"]) : null
  ]));
  function group(title, list) {
    if (!list.length) return null;
    return [h("p.section-title", { text: title }), h("ul.tree", null, list.map(projectNode))];
  }
  nav.appendChild(modeToggle());
  append(nav, group("My project groups", mine));
  append(nav, group(mine.length ? "Other project groups" : "All project groups", others));
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

export function loadProjectItems(p, ul, count) {
  clear(ul);
  ul.appendChild(h("li.loading", { text: "Loading" }));
  if (navMode() === "structure") return loadStructureRoots(p, ul, count);
  return api("GET", "/items?project=" + enc(p.code) + "&limit=500").then(function (r) {
    clear(ul);
    count.textContent = String(r.total);
    if (!r.items.length) {
      ul.appendChild(h("li.empty", { text: "No items yet." }));
      return;
    }
    r.items.forEach(function (it) { ul.appendChild(itemNode(it)); });
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

/** All items (flat list) or Structure; remembered per computer. */
export function navMode() {
  if (!state.navMode) {
    try { state.navMode = normalizeMode(window.localStorage.getItem(MODE_KEY)); } catch (e) { state.navMode = "all"; }
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
  return h("div.nav-filter", null, [["all", "All items"], ["structure", "Structure"]].map(function (m) {
    return h("button.chip" + (m[0] === mode ? ".active" : ""), {
      type: "button", title: m[0] === "all" ? "Every item of a group, alphabetically" : "Assemblies with their parts; parts used nowhere else at the top",
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
