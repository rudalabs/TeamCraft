// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Ask TeamCraft (the shared assistant drawer).
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, api, append, call, errorText, h, state, toast } from "./core.js";
import { canEdit, refreshWorklistCount } from "./shell.js";
import { activeTab, table } from "./tabs.js";
import { openRevision } from "./revision.js";

// ======================================================================
// Revision tab
// ======================================================================

// ======================================================================
// Assistant ("Ask TeamCraft") – the server runs Claude with the MCP tools
// and the user's rights; shown only when the server has it enabled.
// ======================================================================

export function assistantEnabled() {
  return !!(state.me && state.me.capabilities && state.me.capabilities.assistant);
}

export var ITEM_NUMBER_IN_TEXT = /\b(?:PRT|ASM|DOC|SW|SIM)-\d{3,}\b/g;

/** Text with generated item numbers as links that open the item. */
export function itemLink(number) {
  return h("button.link", { type: "button", title: "Open " + number, on: { click: function () { openRevision(number, "latest"); } } }, number);
}

export function itemLinks(text) {
  var nodes = [], last = 0, m;
  ITEM_NUMBER_IN_TEXT.lastIndex = 0;
  while ((m = ITEM_NUMBER_IN_TEXT.exec(text)) !== null) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    nodes.push(itemLink(m[0]));
    last = m.index + m[0].length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

/** Inline markdown-lite: `code` and **bold**; everything else is text. */
export function inlineNodes(text) {
  var out = [];
  var parts = text.split("`");
  var ticks = parts.length - 1;
  var paired = ticks - (ticks % 2);
  parts.forEach(function (part, k) {
    if (k % 2 === 1 && k < paired) { out.push(h("code", { text: part })); return; }
    if (k % 2 === 1) out.push("`");
    var pieces = part.split("**"), markers = pieces.length - 1, strong = null;
    pieces.forEach(function (piece, n) {
      if (n > 0) {
        if (markers % 2 === 1 && n === markers) out.push("**");
        else if (strong) { out.push(strong); strong = null; }
        else strong = h("strong");
      }
      if (strong) append(strong, itemLinks(piece));
      else out.push.apply(out, itemLinks(piece));
    });
    if (strong) out.push(strong);
  });
  return out;
}

export function tableCells(line) {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map(function (c) { return c.trim(); });
}

export function isSeparatorRow(line) {
  var cells = tableCells(line);
  return cells.length > 0 && cells.every(function (c) { return /^[\s:]*-[-\s:]*$/.test(c); });
}

/** The assistant's markdown-lite as DOM nodes (never as HTML). */
export function markdownLite(text) {
  var root = h("div.assistant-text");
  var lines = String(text || "").split(/\r?\n/);
  var para = null, list = null;
  function endBlocks() { para = null; list = null; }
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i], t = line.trim(), m;
    if (t.indexOf("```") === 0) {
      endBlocks();
      var code = [];
      for (i++; i < lines.length && lines[i].trim().indexOf("```") !== 0; i++) code.push(lines[i]);
      root.appendChild(h("pre", null, h("code", { text: code.join("\n") })));
      continue;
    }
    if (!t) { endBlocks(); continue; }
    if ((m = /^(#{1,6})\s+(.*)$/.exec(t))) {
      endBlocks();
      root.appendChild(h(m[1].length <= 2 ? "h3" : "h4", null, inlineNodes(m[2])));
      continue;
    }
    if (t.charAt(0) === "|" && i + 1 < lines.length && isSeparatorRow(lines[i + 1])) {
      endBlocks();
      var head = h("tr", null, tableCells(t).map(function (c) { return h("th", null, inlineNodes(c)); }));
      var rows = [];
      for (i += 2; i < lines.length && lines[i].trim().charAt(0) === "|"; i++) {
        rows.push(h("tr", null, tableCells(lines[i]).map(function (c) { return h("td", null, inlineNodes(c)); })));
      }
      i--;
      root.appendChild(h("div.table-wrap", null, h("table.grid", null, [h("thead", null, head), h("tbody", null, rows)])));
      continue;
    }
    if (/^[-*_]{3,}$/.test(t)) { endBlocks(); root.appendChild(h("hr")); continue; }
    var bullet = /^(?:[-*•])\s+(.*)$/.exec(t), numbered = /^\d{1,3}[.)]\s+(.*)$/.exec(t);
    if (bullet || numbered) {
      para = null;
      var tag = bullet ? "ul" : "ol";
      if (!list || list.tagName.toLowerCase() !== tag) { list = h(tag); root.appendChild(list); }
      list.appendChild(h("li", null, inlineNodes((bullet || numbered)[1])));
      continue;
    }
    list = null;
    if (!para) { para = h("p"); root.appendChild(para); } else para.appendChild(h("br"));
    append(para, inlineNodes(t.replace(/^>\s/, "")));
  }
  return root;
}

// ----------------------------------------------------------------------
// The Ask TeamCraft sidebar: the shared drawer (shared/js/assistant.js, the
// same module as the web UI) beside the tabs. Where the user is comes from
// the active tab; the conversation lives on the server (one per user).
// ----------------------------------------------------------------------

export var assistantDrawer = null, assistantPending = null;

/** Where the user is: the active tab, else the navigator view. Identifiers
 * only – the server checks every one with the user's rights. */
export function assistantContext() {
  var tab = activeTab();
  if (tab && tab.kind === "revision") {
    var sub = tab.sub || "overview";
    var ctx = {
      kind: sub === "structure" ? "bom" : sub === "whereused" ? "where_used" : "revision",
      item: tab.number, revision: tab.code, view: sub === "whereused" ? "where-used" : sub
    };
    if (sub === "structure") ctx.rule = tab.bomRule || "latest";
    var project = tab.data && tab.data.item && tab.data.item.project_code;
    if (project) ctx.project = project;
    return ctx;
  }
  if (tab && tab.kind === "change") return { kind: "change", change: tab.number };
  if (tab && tab.kind === "request") return { kind: "request", request: tab.number };
  switch (state.view) {
    case "search": return { kind: "search", query: state.query || undefined, shown: state.searchShown || [] };
    case "changes": return { kind: "changes", filters: { status: state.changesFilter } };
    case "notifications": return { kind: "notifications" };
    default: return { kind: "dashboard" };
  }
}

/** Errors reach the shared module as Error objects with a message. */
export function assistantError(err) {
  if (err && err.name === "AbortError") return err;
  return new Error(errorText(err));
}

/** A question in the conversation; Cancel drops the request on the Rust side. */
export function assistantSend(body, signal) {
  var cancelled = false;
  if (signal) {
    signal.addEventListener("abort", function () {
      cancelled = true;
      call("assistant_cancel").catch(function () {});
    });
  }
  return call("assistant_send", { body: Object.assign({ thread: true }, body) }).catch(function (err) {
    if (cancelled) {
      var abort = new Error("Cancelled.");
      abort.name = "AbortError";
      throw abort;
    }
    throw assistantError(err);
  });
}

export function ensureAssistant() {
  if (assistantDrawer) return Promise.resolve(assistantDrawer);
  if (!assistantPending) {
    assistantPending = Promise.all([
      import("/static/shared/js/assistant.js"),
      api("GET", "/assistant").catch(function () { return null; })
    ]).then(function (r) {
      var info = r[1] || {};
      var where = info.provider ? " (" + info.provider + (info.host ? ", " + info.host : "") + ")" : "";
      assistantDrawer = r[0].mount($("assistant-drawer"), {
        opener: $("rail-assistant"),
        canEdit: canEdit(),
        privacy: "the AI provider configured by your administrator" + where,
        getContext: assistantContext,
        link: itemLink,
        api: {
          thread: function () { return api("GET", "/assistant/thread").catch(function (e) { throw assistantError(e); }); },
          send: assistantSend,
          reset: function () { return api("DELETE", "/assistant/thread").catch(function (e) { throw assistantError(e); }); }
        },
        onToggle: function (open) {
          var button = $("rail-assistant");
          button.classList.toggle("active", open);
          button.setAttribute("aria-expanded", String(open));
        },
        onDraftsMayHaveChanged: refreshWorklistCount
      });
      return assistantDrawer;
    }, function (err) { assistantPending = null; throw err; });
  }
  return assistantPending;
}

/** Opens (or toggles) the sidebar; `onlyOpen` never closes it. */
export function openAssistant(onlyOpen) {
  if (!assistantEnabled()) return;
  ensureAssistant().then(function (d) { if (onlyOpen) d.open(); else d.toggle(); },
    function (err) { toast(errorText(err), "error"); });
}

/** The rail icon exists for servers with the assistant; the sidebar stays open across tabs. */
export function initAssistant() {
  var button = $("rail-assistant");
  button.classList.toggle("hidden", !assistantEnabled());
  if (!assistantEnabled()) {
    if (assistantDrawer) assistantDrawer.close(false);
    return;
  }
  if (!button.dataset.ready) {
    button.dataset.ready = "1";
    button.addEventListener("click", function () { openAssistant(false); });
  }
  var wasOpen = false;
  try { wasOpen = window.localStorage.getItem("tc.assistant.open") === "1"; } catch (e) { /* blocked storage */ }
  if (wasOpen) ensureAssistant().then(function (d) { d.open(false); }, function () {});
}

/** Signing out: the next user starts with a fresh drawer (no conversation of the last one). */
export function resetAssistant() {
  var host = $("assistant-drawer");
  var fresh = document.createElement("aside");
  fresh.id = "assistant-drawer";
  fresh.hidden = true;
  host.replaceWith(fresh);
  assistantDrawer = null;
  assistantPending = null;
  document.documentElement.classList.remove("tc-assistant-open");
  var button = $("rail-assistant");
  button.classList.add("hidden");
  button.classList.remove("active");
  button.setAttribute("aria-expanded", "false");
}

/** The tab or view changed: the drawer re-reads where the user is. */
export function assistantContextChanged() {
  if (assistantDrawer) assistantDrawer.contextChanged();
}
