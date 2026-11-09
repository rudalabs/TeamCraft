// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – the company AI: the Agent view's server backend (the
// shared assistant of the web UI's "Ask TeamCraft" sidebar, inline), and the
// markdown-lite the local agent's answers are rendered with.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

// (`$` only for scripts/check-desktop-modules.mjs, which reads the `$` of the
// regular expressions below as a use)
import { $, api, append, call, errorText, h, toast } from "./core.js";
import { canEdit, refreshWorklistCount } from "./shell.js";
import { openRevision } from "./revision.js";

// ======================================================================
// Answers as DOM nodes (never as HTML), item numbers as links
// ======================================================================

export var ITEM_NUMBER_IN_TEXT = /\b(?:PRT|ASM|DOC|SW|SIM)-\d{3,}\b/g;

/** Text with generated item numbers as links that open the item. */
export function itemLink(number) {
  return h("button.link", { type: "button", title: "Open " + number, on: { click: function () { openRevision(number, "latest"); } } }, number);
}

// item numbers follow the group's numbering scheme: any short token without
// a slash or blank (they become API path segments, encoded)
var ITEM_NUMBER = /^[^\/\s]{1,40}$/;
var REVISION_CODE = /^[^\/\s]{1,10}$/;

/** An "Open 3D preview" link of the company AI (open_3d_preview, the
 * path already checked by the shared module): opens the revision tab on its
 * 3D viewer with that file instead of leaving the app for the web page.
 * Anything that does not look like an item and a revision stays a plain
 * label (null: the shared module would make a web link, which the desktop
 * does not follow). */
export function previewLink(link) {
  var m = /^\/files\/([0-9a-f-]{36})\/preview$/.exec(link.url);
  if (!m || !ITEM_NUMBER.test(link.item) || !REVISION_CODE.test(link.revision)) {
    return h("span.tc-a-open", { text: link.label });
  }
  return h("button.tc-a-open", { type: "button", title: "Open " + link.item + "/" + link.revision + " in the 3D viewer", on: { click: function () { openRevision(link.item, link.revision, "viewer", m[1]); } } }, link.label);
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
// The company AI: the shared assistant (shared/js/assistant.js, the same
// module as the web UI's sidebar) inline in the Agent tab, when the Agent
// view's backend is the server (src/acp.rs, choose_backend). The provider
// key stays on the server; the conversation lives there (one per user).
// ----------------------------------------------------------------------

export var companyAi = {
  host: null,    // the panel's element, kept across renders of the Agent tab
  panel: null,   // the mounted shared module
  ready: null,   // Promise of the panel
  info: null     // GET /assistant: provider, model, host
};

/** What the user looks at: a revision, change or request tab (the last one
 * opened before the Agent tab), else the dashboard. Identifiers only – the
 * server checks every one with the user's rights. */
export function assistantContext(tab) {
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
  return { kind: "dashboard" };
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

/** Where the questions go, for the panel's privacy line and the navigator. */
export function companyAiWhere() {
  var info = companyAi.info || {};
  return info.provider ? info.provider + (info.host ? ", " + info.host : "") : "";
}

/** The company AI panel of the Agent tab, mounted once per sign-in.
 * `getContext` gives the page context, `onBusy(busy)` follows a question. */
export function companyPanel(getContext, onBusy) {
  if (companyAi.host) return companyAi.host;
  var host = companyAi.host = h("section");
  companyAi.ready = Promise.all([
    import("/static/shared/js/assistant.js"),
    api("GET", "/assistant").catch(function () { return null; })
  ]).then(function (r) {
    if (companyAi.host !== host) return null; // signed out meanwhile
    companyAi.info = r[1] || {};
    var where = companyAiWhere();
    companyAi.panel = r[0].mount(host, {
      inline: true,
      title: "Company AI",
      canEdit: canEdit(),
      privacy: "the AI provider configured by your administrator" + (where ? " (" + where + ")" : ""),
      getContext: getContext,
      link: itemLink,
      openLink: previewLink,
      api: {
        thread: function () { return api("GET", "/assistant/thread").catch(function (e) { throw assistantError(e); }); },
        send: assistantSend,
        reset: function () { return api("DELETE", "/assistant/thread").catch(function (e) { throw assistantError(e); }); }
      },
      onBusy: onBusy,
      onDraftsMayHaveChanged: refreshWorklistCount
    });
    return companyAi.panel;
  }, function (err) {
    // the next render of the Agent tab tries again
    if (companyAi.host === host) { companyAi.host = null; companyAi.ready = null; }
    toast(errorText(err), "error");
    return null;
  });
  return host;
}

/** A question to the company AI is running. */
export function companyAiBusy() {
  return !!(companyAi.panel && companyAi.panel.isBusy());
}

/** Signing out: the next user starts with a fresh panel (no conversation of the last one). */
export function resetCompanyAi() {
  companyAi.host = null;
  companyAi.panel = null;
  companyAi.ready = null;
  companyAi.info = null;
}

/** The tab or view changed: the panel re-reads where the user is. */
export function assistantContextChanged() {
  if (companyAi.panel) companyAi.panel.contextChanged();
}
