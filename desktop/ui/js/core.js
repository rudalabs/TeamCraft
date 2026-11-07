// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Helpers: constants, state, DOM and server calls, dialogs, labels.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { showConnect } from "./connect.js";
import { agent } from "./agent.js";
import { projectItems } from "./group-sync.js";

export var TAURI = window.__TAURI__;
export var invoke = TAURI.core.invoke;

// Keep in sync with SKINS in src/domain.rs (a server unit test checks it).
export var SKINS = [
  { id: "teamcraft", name: "TeamCraft", logo: "logos/TeamCraft_Icon.svg", description: "Follows your system: light or dark" },
  { id: "teamcraft-day", name: "TeamCraft Light", logo: "logos/TeamCraft_Icon.svg", description: "House style, light" },
  { id: "teamcraft-night", name: "TeamCraft Dark", logo: "logos/TeamCraft_Icon.svg", description: "House style, dark" },
  { id: "arch", name: "Arch", logo: "logos/TeamCraft_Icon.svg", description: "Blueprint grid" },
  { id: "cachyos", name: "CachyOS", logo: "logos/TeamCraft_Icon.svg", description: "Teal dot matrix" },
  { id: "endeavouros", name: "EndeavourOS", logo: "logos/TeamCraft_Icon.svg", description: "Purple sunset" },
  { id: "kde", name: "KDE", logo: "logos/TeamCraft_Icon.svg", description: "Breeze dark" },
  { id: "kubuntu", name: "Kubuntu", logo: "logos/TeamCraft_Icon.svg", description: "Blue rings" },
  { id: "opensuse", name: "openSUSE", logo: "logos/TeamCraft_Icon.svg", description: "Chameleon scales" },
  { id: "ubuntu", name: "Ubuntu", logo: "logos/TeamCraft_Icon.svg", description: "Circle of friends" }
];

export var ITEM_TYPES = [
  { value: "part", label: "Part" },
  { value: "assembly", label: "Assembly" },
  { value: "document", label: "Document" },
  { value: "software", label: "Software" },
  { value: "simulation", label: "Simulation" },
  { value: "as_built", label: "As-built" }
];

export var state = {
  session: null,
  me: null,
  projects: [],
  view: "home",
  expanded: {},
  projectItems: {},
  changesFilter: "open",
  query: "",
  tabs: [],
  active: null,
  worklist: null,
  viewer: null,
  zoom: 1,
  update: null, // the newer release the status bar tells about: {version, current}
  tokenFileWarned: false // the "no keychain" warning shows once per start
};

// ======================================================================
// Helpers
// ======================================================================

export function $(id) { return document.getElementById(id); }

export function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); }

/** h("tag.class", {attrs}, children) – text children become text nodes. */
export function h(spec, attrs, children) {
  var parts = spec.split(".");
  var el = document.createElement(parts[0] || "div");
  if (parts.length > 1) el.className = parts.slice(1).join(" ");
  if (attrs) {
    Object.keys(attrs).forEach(function (k) {
      var v = attrs[k];
      if (v === null || v === undefined || v === false) return;
      if (k === "on") {
        Object.keys(v).forEach(function (ev) { el.addEventListener(ev, v[ev]); });
      } else if (k === "text") {
        el.textContent = v;
      } else if (k === "class") {
        el.className = (el.className ? el.className + " " : "") + v;
      } else if (k === "value") {
        el.value = v;
      } else if (k === "checked" || k === "disabled" || k === "selected") {
        el[k] = !!v;
      } else {
        el.setAttribute(k, v === true ? "" : String(v));
      }
    });
  }
  append(el, children);
  return el;
}

export function append(el, children) {
  if (children === null || children === undefined || children === false) return;
  if (!Array.isArray(children)) children = [children];
  children.forEach(function (c) {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) append(el, c);
    else el.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
  });
}

export function svgIcon(path) {
  var ns = "http://www.w3.org/2000/svg";
  var svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  path.split("|").forEach(function (d) {
    var p = document.createElementNS(ns, "path");
    p.setAttribute("d", d);
    svg.appendChild(p);
  });
  return svg;
}

export var ICONS = {
  checkout: "M7 11V7a5 5 0 0 1 10 0v4|M5 11h14v10H5z",
  checkin: "M12 3v12|M7 10l5 5 5-5|M5 21h14",
  cancel: "M6 6l12 12|M18 6L6 18",
  revise: "M4 12a8 8 0 0 1 14-5.3L20 9|M20 4v5h-5|M20 12a8 8 0 0 1-14 5.3L4 15|M4 20v-5h5",
  folder: "M3 7h6l2 2h10v10H3z",
  download: "M12 4v11|M7 10l5 5 5-5|M5 20h14",
  "package": "M3 7.5l9-4.5 9 4.5v9l-9 4.5-9-4.5z|M3 7.5l9 4.5 9-4.5|M12 12v9",
  cube: "M12 3l8 4.5v9L12 21l-8-4.5v-9z|M4 7.5l8 4.5 8-4.5|M12 12v9",
  chevron: "M6 9l6 6 6-6",
  approve: "M5 12.5l4.5 4.5L19 7.5",
  web: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18|M3 12h18|M12 3c3 3.5 3 14.5 0 18|M12 3c-3 3.5-3 14.5 0 18",
  refresh: "M20 11a8 8 0 1 0-2.3 5.7|M20 4v7h-7",
  plus: "M12 5v14|M5 12h14",
  edit: "M4 20h4L19 9l-4-4L4 16z",
  obsolete: "M5 5l14 14|M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18",
  release: "M5 21V4|M5 4h12l-2.5 4 2.5 4H5",
  eye: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z|M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6",
  request: "M4 4h16v12H9l-5 4z|M12 7v4|M12 13.5v.5",
  ask: "M4 5h16v11H9l-5 4z|M8 9h8|M8 12h5",
  agent: "M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z|M19 15l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"
};

// Keep in sync with RequestReason in src/domain.rs.
export var REQUEST_REASONS = [
  { value: "defect", label: "Defect / error" },
  { value: "improvement", label: "Improvement" },
  { value: "cost", label: "Cost reduction" },
  { value: "manufacturing", label: "Manufacturing / assembly" },
  { value: "supplier", label: "Supplier / purchasing" },
  { value: "safety", label: "Safety / compliance" },
  { value: "customer", label: "Customer request" },
  { value: "other", label: "Other" }
];
export var OPEN_REQUEST_STATES = ["new", "in_triage", "accepted", "deferred"];

export function tool(label, icon, onClick, opts) {
  opts = opts || {};
  // Refresh is an icon button (F5 in the View menu does the same)
  var iconOnly = icon === "refresh" && label === "Refresh";
  return h("button.tool" + (opts.primary ? ".primary" : "") + (opts.danger ? ".danger" : "") + (iconOnly ? ".icon" : "") + ".tool-" + icon, {
    type: "button", title: opts.title || (iconOnly ? "Refresh (F5)" : label), "aria-label": iconOnly ? label : null,
    disabled: opts.disabled, on: { click: onClick }
  }, [svgIcon(ICONS[icon] || ICONS.plus), iconOnly ? null : label]);
}

/** A labelled segment of related toolbar buttons (Lock, Work, Lifecycle …). */
export function toolGroup(name, buttons) {
  var list = buttons.filter(function (b) { return b; });
  if (!list.length) return null;
  return h("div.tool-group", { role: "group", "aria-label": name }, list);
}

/**
 * "More ▾": the rare actions of a tab in a menu at the end of the toolbar.
 * items: { label, icon, run, disabled, title, danger, shortcut }.
 * Keyboard: Enter, Space or ↓ open it on the first item, ↑ on the last;
 * ↑/↓/Home/End move, Enter/Space run, Esc or Tab close – the focus goes
 * back to the button. A click outside closes it too.
 */
export function moreMenu(items) {
  items = items.filter(function (i) { return i; });
  var wrap = h("div.more");
  var menuId = "more-menu-" + (++moreMenu.seq);
  var trigger = h("button.tool.more-trigger", {
    type: "button", title: "More actions", "aria-haspopup": "menu", "aria-expanded": "false", "aria-controls": menuId
  }, [h("span", { text: "More" }), svgIcon(ICONS.chevron)]);
  var menu = h("div.menu.hidden", { id: menuId, role: "menu", "aria-label": "More actions" });
  var entries = items.map(function (it) {
    var b = h("button.menu-item" + (it.danger && !it.disabled ? ".danger" : ""), {
      type: "button", role: "menuitem", tabindex: "-1", title: it.title || it.label,
      "aria-disabled": it.disabled ? "true" : null
    }, [svgIcon(ICONS[it.icon] || ICONS.plus), h("span.menu-label", { text: it.label }),
      it.shortcut ? h("kbd", { text: it.shortcut }) : null]);
    b.addEventListener("click", function () {
      if (it.disabled) return;
      close(true);
      it.run();
    });
    if (it.separated) append(menu, h("div.menu-sep", { role: "separator" }));
    append(menu, b);
    return b;
  });
  function isOpen() { return !menu.classList.contains("hidden"); }
  function onDoc(e) { if (!wrap.contains(e.target)) close(false); }
  function open(focusIndex, instant) {
    if (isOpen()) return;
    menu.classList.toggle("instant", !!instant);
    menu.classList.remove("hidden");
    trigger.setAttribute("aria-expanded", "true");
    document.addEventListener("pointerdown", onDoc, true);
    var i = focusIndex < 0 ? entries.length - 1 : focusIndex;
    if (entries[i]) entries[i].focus();
  }
  function close(refocus) {
    if (!isOpen()) return;
    menu.classList.add("hidden");
    trigger.setAttribute("aria-expanded", "false");
    document.removeEventListener("pointerdown", onDoc, true);
    if (refocus) trigger.focus();
  }
  trigger.addEventListener("click", function (e) {
    // detail 0: Enter or Space – open without the entrance fade
    if (isOpen()) close(true); else open(0, e.detail === 0);
  });
  trigger.addEventListener("keydown", function (e) {
    if (e.key === "ArrowDown") { e.preventDefault(); open(0, true); }
    else if (e.key === "ArrowUp") { e.preventDefault(); open(-1, true); }
  });
  menu.addEventListener("keydown", function (e) {
    var i = entries.indexOf(document.activeElement);
    var n = entries.length;
    if (e.key === "ArrowDown") { e.preventDefault(); entries[(i + 1) % n].focus(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); entries[(i - 1 + n) % n].focus(); }
    else if (e.key === "Home") { e.preventDefault(); entries[0].focus(); }
    else if (e.key === "End") { e.preventDefault(); entries[n - 1].focus(); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); }
    else if (e.key === "Tab") { close(false); }
  });
  // a re-render of the toolbar removes the menu: drop the outside-click listener
  wrap.addEventListener("focusout", function (e) {
    if (isOpen() && e.relatedTarget && !wrap.contains(e.relatedTarget)) close(false);
  });
  append(wrap, [trigger, menu]);
  return wrap;
}
moreMenu.seq = 0;

export function fmtDate(iso) {
  if (!iso) return "—";
  var d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export function fmtSize(bytes) {
  if (bytes === null || bytes === undefined) return "—";
  var units = ["B", "KiB", "MiB", "GiB"];
  var v = bytes, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (i === 0 ? v : v.toFixed(1)) + " " + units[i];
}

export function fmtNum(v, digits) {
  if (v === null || v === undefined) return "—";
  return Number(v).toLocaleString(undefined, { maximumFractionDigits: digits === undefined ? 3 : digits });
}

export var LABELS = { in_work: "WIP", in_review: "In Review", editor: "CAD editor" };
export function label(code) {
  if (!code) return "";
  if (LABELS[code]) return LABELS[code];
  return String(code).split("_").map(function (w) { return w.charAt(0).toUpperCase() + w.slice(1); }).join(" ");
}

export function badge(value, text) {
  var icon = null;
  if (value === "released" || value === "approved" || value === "unchanged" || value === "up_to_date") icon = "Green_tick.svg";
  else if (value === "in_review" || value === "pending" || value === "modified" || value === "outdated") icon = "Yellow_circle.svg";
  else if (value === "rejected" || value === "obsolete" || value === "cancelled" || value === "conflict" || value === "clash") icon = "Red_cross.svg";
  return h("span.badge.badge-" + value, null, [
    icon ? h("img", { src: "/static/media/icons/" + icon, alt: "" }) : null,
    text || label(value)
  ]);
}

// A priority as the web titleblock shows it: plain text, high and critical
// with their own glyph (▲, ▲▲) in the impact and danger ink – never a state pill.
export function priorityBadge(p) {
  if (!p) return null;
  return h("span.prio.prio-" + p, { title: "Priority " + label(p) }, label(p));
}

// The lock holder with the line lock icon (the web UI's lock macro):
// muted for your own lock, amber for someone else's.
export function lockIcon(mine) {
  var svg = svgIcon(ICONS.checkout);
  svg.setAttribute("class", "lock-icon" + (mine ? " lock-mine" : ""));
  return svg;
}
export function lockBadge(name, mine) {
  return h("span.badge.badge-lock", { title: "Checked out by " + (mine ? "you" : name) }, [lockIcon(mine), name, mine ? h("span.dim", { text: " (you)" }) : null]);
}

export function errorText(err) {
  if (!err) return "Unknown error";
  if (typeof err === "string") return err;
  return err.message || JSON.stringify(err);
}

export function setStatus(text) { $("status-msg").textContent = text || ""; }

export function toast(text, kind) {
  var el = h("div.toast" + (kind ? "." + kind : ""), { role: kind === "error" ? "alert" : "status", text: text });
  $("toasts").appendChild(el);
  setTimeout(function () {
    el.classList.add("leaving"); // app.css fades it out in 150 ms
    setTimeout(function () { el.remove(); }, 160);
  }, kind === "error" ? 9000 : 4500);
}

/** Calls a Rust command; a 401 sends the user back to the sign-in. */
export function call(cmd, args) {
  return invoke(cmd, args || {}).catch(function (err) {
    if (err && err.status === 401 && cmd !== "sign_in_token") {
      state.session.signed_in = false;
      showConnect("signin", "Your sign-in has expired or was revoked – please sign in again.");
    }
    throw err;
  });
}

export function api(method, path, body) {
  return call("api", { method: method, path: path, body: body === undefined ? null : body });
}

export function enc(s) { return encodeURIComponent(s); }

/** Runs an action with status text and error toast; returns its promise. */
export function run(statusText, fn) {
  setStatus(statusText + " …");
  return Promise.resolve().then(fn).then(function (v) {
    setStatus("");
    return v;
  }, function (err) {
    setStatus("");
    toast(errorText(err), "error");
    throw err;
  });
}

// ---------------------------------------------------------------- dialogs

export function dialog(title, body, actions, opts) {
  opts = opts || {};
  var root = $("dialog-root");
  var errorEl = h("p.error-text.hidden", { role: "alert" });
  var buttons = (actions || []).map(function (a) {
    return h("button" + (a.primary ? ".primary" : "") + (a.danger ? ".danger" : ""), {
      type: a.submit ? "submit" : "button",
      on: a.submit ? null : { click: function () { a.onClick(handle); } }
    }, a.label);
  });
  var form = h("form.dialog" + (opts.wide ? ".wide" : ""), {
    role: "dialog", "aria-modal": "true", "aria-label": title,
    on: {
      submit: function (e) {
        e.preventDefault();
        var submit = (actions || []).filter(function (a) { return a.submit; })[0];
        if (submit) submit.onClick(handle);
      }
    }
  }, [h("h2", { text: title }), body, errorEl, h("div.dialog-actions", null, buttons)]);
  var backdrop = h("div.dialog-backdrop", { on: { mousedown: function (e) { if (e.target === backdrop && !opts.modal) close(); } } }, form);
  function close() { backdrop.remove(); document.removeEventListener("keydown", onKey); }
  function onKey(e) { if (e.key === "Escape" && !opts.modal) close(); }
  function fail(err) { errorEl.textContent = errorText(err); errorEl.classList.remove("hidden"); }
  var handle = { close: close, fail: fail, form: form };
  document.addEventListener("keydown", onKey);
  root.appendChild(backdrop);
  var first = form.querySelector("input, select, textarea, button.primary");
  if (first) first.focus();
  return handle;
}

export function confirmDialog(title, text, okLabel, danger) {
  return new Promise(function (resolve) {
    dialog(title, h("p", { text: text }), [
      { label: "Cancel", onClick: function (d) { d.close(); resolve(false); } },
      { label: okLabel || "OK", primary: !danger, danger: danger, submit: true, onClick: function (d) { d.close(); resolve(true); } }
    ]);
  });
}

export function field(labelText, input, hint) {
  return h("label", null, [labelText, hint ? h("span.hint", { text: " " + hint }) : null, input]);
}
