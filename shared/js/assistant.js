/* SPDX-License-Identifier: MPL-2.0 */
// "Ask TeamCraft" sidebar, shared by the web UI and the desktop app.
//
// The drawer knows the page the user is on (a small context object the app
// declares), keeps one conversation per user on the server and shows where
// every question was asked. The apps own the shell (the rail or top bar icon,
// the place of the <aside> host) and all data access: this module never calls
// fetch or Tauri invoke, it gets callbacks (shared/README.md, rule 3).
//
//   import { mount } from ".../assistant.js";
//   const drawer = mount(hostElement, {
//     api: {
//       thread()          → Promise<{ messages, retention_days }>
//       send(body, signal) → Promise<{ messages, context, tools, steps, model, truncated }>
//       reset()           → Promise
//     },
//     getContext() → the page context (see docs/src/user-guide/assistant.md)
//     canEdit      boolean: the draft tools may be offered
//     link(number) → Node | null: how an item number is shown in an answer
//     privacy      string: where the data goes ("Anthropic", "your organisation's model server" …)
//     onToggle(open)
//   });
//   drawer.open(); drawer.close(); drawer.toggle(); drawer.isOpen();
//   drawer.contextChanged();   // the page or tab changed: re-read the context
//   drawer.ask(action, text);  // open and send (quick actions of the apps)
//
// No innerHTML anywhere: answers are built as DOM nodes from markdown-lite.

var STORE_OPEN = "tc.assistant.open";
var STORE_WIDTH = "tc.assistant.width";
var MIN_WIDTH = 320;
var MAX_WIDTH = 720;
var DEFAULT_WIDTH = 400;

// Quick actions per page kind: [action, label]. The server validates the
// action and its context; an unknown action from a newer server is harmless.
var QUICK = {
  dashboard: [["attention", "What needs my attention"]],
  revision: [["release_notes", "Draft release notes"], ["release_check", "Check before release"], ["impact", "What breaks if I change this?"], ["simulation_status", "Are the simulations still valid?"]],
  simulation: [["release_notes", "Draft release notes"], ["release_check", "Check before release"], ["impact", "What breaks if I change this?"], ["simulation_status", "Are the simulations still valid?"], ["result_values", "Fill result values from the report"]],
  item: [["release_notes", "Draft release notes"], ["release_check", "Check before release"], ["impact", "What breaks if I change this?"], ["simulation_status", "Are the simulations still valid?"]],
  bom: [["release_check", "Check before release"], ["impact", "What breaks if I change this?"]],
  where_used: [["impact", "What breaks if I change this?"]],
  change: [["review_brief", "Brief me for this review"]],
  changes: [["attention", "What needs my attention"]]
};

/** Quick actions that fit the page kind (the apps may add their own). */
export function quickActions(kind) {
  return (QUICK[kind] || []).slice();
}

/** "PRT-000009/A · bom (latest released)" – the chip text of a context. */
export function describe(ctx) {
  ctx = ctx || {};
  var parts = [];
  if (ctx.item) parts.push(ctx.item + (ctx.revision ? "/" + ctx.revision : ""));
  if (ctx.change) parts.push(ctx.change);
  if (ctx.request) parts.push(ctx.request);
  if (ctx.project) parts.push("project " + ctx.project);
  if (!parts.length) parts.push(KIND_LABELS[ctx.kind] || (ctx.kind ? "Page" : ""));
  var out = parts.filter(Boolean).join(" · ");
  if (out && ctx.view) out += " · " + String(ctx.view).replace(/[-_]/g, " ");
  if (out && ctx.rule) out += " (" + String(ctx.rule).replace(/[-_]/g, " ") + ")";
  if (out && ctx.query) out += " · “" + String(ctx.query).slice(0, 40) + "”";
  return out;
}

var KIND_LABELS = {
  dashboard: "Dashboard", items: "Items list", item: "Item", revision: "Revision",
  bom: "Bill of materials", where_used: "Where-used", compare: "Comparison", change: "Change order",
  changes: "Change orders list", request: "Change request", requests: "Change requests list",
  simulation: "Simulation results", project: "Project group", projects: "Project groups",
  search: "Search", vault: "Vault explorer", notifications: "Notifications", other: "Page"
};

function el(tag, attrs, children) {
  var n = document.createElement(tag);
  Object.keys(attrs || {}).forEach(function (k) {
    var v = attrs[k];
    if (v === null || v === undefined || v === false) return;
    if (k === "class") n.className = v;
    else if (k === "text") n.textContent = v;
    else if (k === "on") Object.keys(v).forEach(function (e) { n.addEventListener(e, v[e]); });
    else n.setAttribute(k, v === true ? "" : v);
  });
  [].concat(children === undefined ? [] : children).forEach(function (c) {
    if (c === null || c === undefined || c === false) return;
    n.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
  });
  return n;
}

function store(key, value) {
  try {
    if (value === undefined) return window.localStorage.getItem(key);
    window.localStorage.setItem(key, value);
  } catch (e) { /* private mode, blocked storage: the drawer still works */ }
  return null;
}

// ---------------------------------------------------------------------
// markdown-lite as DOM nodes (the server's render_markdown does the same as
// HTML for the full page; here nothing is ever parsed as HTML)
// ---------------------------------------------------------------------

var ITEM_NUMBER = /\b(?:PRT|ASM|DOC|SW)-\d{3,}\b/g;

function itemNodes(text, link) {
  if (!link) return [text];
  var out = [], last = 0, m;
  ITEM_NUMBER.lastIndex = 0;
  while ((m = ITEM_NUMBER.exec(text)) !== null) {
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(link(m[0]) || m[0]);
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function inlineNodes(text, link) {
  var out = [];
  var parts = text.split("`");
  var ticks = parts.length - 1;
  var paired = ticks - (ticks % 2);
  parts.forEach(function (part, k) {
    if (k % 2 === 1 && k < paired) { out.push(el("code", { text: part })); return; }
    if (k % 2 === 1) out.push("`");
    var pieces = part.split("**"), markers = pieces.length - 1, strong = null;
    pieces.forEach(function (piece, n) {
      if (n > 0) {
        if (markers % 2 === 1 && n === markers) out.push("**");
        else if (strong) { out.push(strong); strong = null; }
        else strong = el("strong");
      }
      var nodes = itemNodes(piece, link);
      nodes.forEach(function (c) {
        var node = typeof c === "string" ? document.createTextNode(c) : c;
        if (strong) strong.appendChild(node); else out.push(node);
      });
    });
    if (strong) out.push(strong);
  });
  return out.map(function (n) { return typeof n === "string" ? document.createTextNode(n) : n; });
}

function cells(line) {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map(function (c) { return c.trim(); });
}

function isSeparator(line) {
  var c = cells(line);
  return c.length > 0 && c.every(function (x) { return /^[\s:]*-[-\s:]*$/.test(x); });
}

/** The assistant's markdown-lite as a <div class="tc-md"> of DOM nodes. */
export function markdown(text, link) {
  var root = el("div", { class: "tc-md assistant-text" });
  var lines = String(text || "").split(/\r?\n/);
  var para = null, list = null;
  function end() { para = null; list = null; }
  function add(parent, nodes) { nodes.forEach(function (n) { parent.appendChild(n); }); }
  for (var i = 0; i < lines.length; i++) {
    var t = lines[i].trim(), m;
    if (t.indexOf("```") === 0) {
      end();
      var code = [];
      for (i++; i < lines.length && lines[i].trim().indexOf("```") !== 0; i++) code.push(lines[i]);
      root.appendChild(el("pre", null, el("code", { text: code.join("\n") })));
      continue;
    }
    if (!t) { end(); continue; }
    if ((m = /^(#{1,6})\s+(.*)$/.exec(t))) {
      end();
      var head = el(m[1].length <= 2 ? "h3" : "h4");
      add(head, inlineNodes(m[2], link));
      root.appendChild(head);
      continue;
    }
    if (t.charAt(0) === "|" && i + 1 < lines.length && isSeparator(lines[i + 1])) {
      end();
      var tr = el("tr");
      cells(t).forEach(function (c) { var th = el("th"); add(th, inlineNodes(c, link)); tr.appendChild(th); });
      var body = el("tbody");
      for (i += 2; i < lines.length && lines[i].trim().charAt(0) === "|"; i++) {
        var row = el("tr");
        cells(lines[i]).forEach(function (c) { var td = el("td"); add(td, inlineNodes(c, link)); row.appendChild(td); });
        body.appendChild(row);
      }
      i--;
      root.appendChild(el("div", { class: "table-wrap" }, el("table", { class: "grid" }, [el("thead", null, tr), body])));
      continue;
    }
    if (/^[-*_]{3,}$/.test(t)) { end(); root.appendChild(el("hr")); continue; }
    var bullet = /^(?:[-*•])\s+(.*)$/.exec(t), numbered = /^\d{1,3}[.)]\s+(.*)$/.exec(t);
    if (bullet || numbered) {
      para = null;
      var tag = bullet ? "ul" : "ol";
      if (!list || list.tagName.toLowerCase() !== tag) { list = el(tag); root.appendChild(list); }
      var li = el("li");
      add(li, inlineNodes((bullet || numbered)[1], link));
      list.appendChild(li);
      continue;
    }
    list = null;
    if (!para) { para = el("p"); root.appendChild(para); } else para.appendChild(el("br"));
    add(para, inlineNodes(t.replace(/^>\s/, ""), link));
  }
  return root;
}

// ---------------------------------------------------------------------
// The drawer
// ---------------------------------------------------------------------

export function mount(host, o) {
  o = o || {};
  var api = o.api;
  var isOpen = false;
  var busy = false;
  var loaded = false;
  var controller = null;
  var contextOff = false;
  var lastKey = "";
  var width = parseInt(store(STORE_WIDTH), 10) || DEFAULT_WIDTH;
  width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, width));
  var retention = 30;

  host.classList.add("tc-assistant");
  host.setAttribute("role", "complementary");
  host.setAttribute("aria-label", "Ask TeamCraft");
  host.setAttribute("tabindex", "-1");
  host.hidden = true;
  host.style.setProperty("--tc-assistant-w", width + "px");

  var title = el("h2", { class: "tc-a-title", text: "Ask TeamCraft" });
  var resetBtn = el("button", { type: "button", class: "tc-a-btn", title: "Start a new conversation – the current one is deleted", text: "New conversation", on: { click: reset } });
  var closeBtn = el("button", { type: "button", class: "tc-a-btn tc-a-close", "aria-label": "Close the assistant", title: "Close (Esc)", text: "×", on: { click: function () { close(true); } } });
  var handle = el("div", { class: "tc-a-handle", role: "separator", "aria-orientation": "vertical", tabindex: "0", title: "Drag or use the arrow keys to resize", "aria-label": "Resize the assistant", "aria-valuemin": String(MIN_WIDTH), "aria-valuemax": String(MAX_WIDTH) });
  var chipLabel = el("span", { class: "tc-a-chip-label" });
  var chipRemove = el("button", { type: "button", class: "tc-a-chip-x", "aria-label": "Do not send the page context", title: "Do not send the page context", text: "×", on: { click: function () { contextOff = true; renderContext(); } } });
  var chipAdd = el("button", { type: "button", class: "tc-a-link", text: "add it back", on: { click: function () { contextOff = false; renderContext(); } } });
  var chip = el("div", { class: "tc-a-context" });
  var quick = el("div", { class: "tc-a-quick" });
  var log = el("div", { class: "tc-a-log", role: "log", "aria-live": "polite", "aria-label": "Conversation", tabindex: "0" });
  var status = el("p", { class: "tc-a-status", role: "status" });
  var input = el("textarea", { class: "tc-a-input", rows: "3", maxlength: "4000", placeholder: "Ask about this page, items, structures, changes … (Enter sends, Shift+Enter: new line)", "aria-label": "Your question" });
  var drafts = o.canEdit ? el("input", { type: "checkbox" }) : null;
  var sendBtn = el("button", { type: "submit", class: "tc-a-send", text: "Ask" });
  var cancelBtn = el("button", { type: "button", class: "tc-a-btn tc-a-cancel", text: "Cancel", on: { click: cancel } });
  cancelBtn.hidden = true;
  var form = el("form", { class: "tc-a-form" }, [
    input,
    drafts ? el("label", { class: "tc-a-check" }, [drafts, "Allow drafting change orders, change requests and comments"]) : null,
    el("div", { class: "tc-a-actions" }, [sendBtn, cancelBtn]),
    status,
    el("p", { class: "tc-a-hint", text: "Read-only unless you allow drafts; it never submits, approves or releases. Your question, the page context and the data the tools return are sent to " + (o.privacy || "the model provider") + "." })
  ]);
  form.addEventListener("submit", function (e) { e.preventDefault(); send("ask", input.value.trim()); });
  input.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); form.requestSubmit(); }
  });

  host.appendChild(handle);
  host.appendChild(el("div", { class: "tc-a-head" }, [title, resetBtn, closeBtn]));
  host.appendChild(chip);
  host.appendChild(quick);
  host.appendChild(log);
  host.appendChild(form);

  host.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && isOpen) { e.preventDefault(); e.stopPropagation(); close(true); }
  });

  // --- resize ---------------------------------------------------------

  function setWidth(w, save) {
    width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));
    host.style.setProperty("--tc-assistant-w", width + "px");
    // the page makes room for the drawer (web) or the grid column follows (desktop)
    document.documentElement.style.setProperty("--tc-assistant-w", width + "px");
    handle.setAttribute("aria-valuenow", String(width));
    if (save) store(STORE_WIDTH, String(width));
  }
  setWidth(width, false);
  handle.addEventListener("pointerdown", function (e) {
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    function move(ev) { setWidth(window.innerWidth - ev.clientX, false); }
    function up() {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      store(STORE_WIDTH, String(width));
    }
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  });
  handle.addEventListener("keydown", function (e) {
    if (e.key === "ArrowLeft") { e.preventDefault(); setWidth(width + 24, true); }
    else if (e.key === "ArrowRight") { e.preventDefault(); setWidth(width - 24, true); }
  });

  // --- context chip and quick actions ---------------------------------

  function context() {
    var c = {};
    try { c = (o.getContext && o.getContext()) || {}; } catch (e) { c = {}; }
    return c;
  }

  function renderContext() {
    var ctx = context();
    var text = describe(ctx);
    var key = JSON.stringify([ctx.kind, ctx.item, ctx.revision, ctx.change, ctx.request, ctx.project, ctx.view]);
    if (key !== lastKey) { lastKey = key; contextOff = false; }
    chip.textContent = "";
    if (!text) {
      chip.hidden = true;
    } else {
      chip.hidden = false;
      chip.appendChild(el("span", { class: "tc-a-chip-title", text: "Context:" }));
      if (contextOff) {
        chip.appendChild(el("span", { class: "tc-a-muted", text: "none (page not shared) · " }));
        chip.appendChild(chipAdd);
      } else {
        chipLabel.textContent = text;
        chip.appendChild(el("span", { class: "tc-a-chip" }, [chipLabel, chipRemove]));
      }
    }
    quick.textContent = "";
    var actions = contextOff ? [] : (o.quickActions ? o.quickActions(ctx) : quickActions(ctx.kind));
    actions.forEach(function (a) {
      quick.appendChild(el("button", { type: "button", class: "tc-a-quickbtn", text: a[1], disabled: busy, on: { click: function () { send(a[0], input.value.trim(), a[1]); } } }));
    });
    quick.hidden = !actions.length;
  }

  // --- messages -------------------------------------------------------

  function scrollDown() { log.scrollTop = log.scrollHeight; }

  function renderMessage(m, extras) {
    if (m.role === "user") {
      return el("div", { class: "tc-a-msg tc-a-user" }, [
        el("p", { class: "tc-a-text", text: m.text }),
        m.context_label ? el("p", { class: "tc-a-where", text: "asked on " + m.context_label }) : null
      ]);
    }
    var node = el("div", { class: "tc-a-msg tc-a-assistant" }, [markdown(m.text, o.link)]);
    if (extras && extras.truncated) node.appendChild(el("p", { class: "tc-a-note", text: "The answer was cut off at the length limit – ask a narrower question for the rest." }));
    if (extras && extras.tools) {
      var tools = extras.tools.map(function (t) {
        return el("span", { class: "badge" + (t.ok ? "" : " badge-rejected"), title: t.ok ? "tool call" : "tool call failed", text: t.name });
      });
      node.appendChild(el("p", { class: "tc-a-meta" }, [tools.length ? "Looked up: " : "No tools used. "].concat(tools, [
        " · " + extras.steps + (extras.steps === 1 ? " step" : " steps") + " · " + extras.model + (extras.allow_drafts ? " · drafts allowed" : " · read-only")
      ])));
    }
    return node;
  }

  function renderThread(messages) {
    log.textContent = "";
    if (!messages.length) {
      log.appendChild(el("p", { class: "tc-a-empty", text: "Ask about the page you are on or anything in TeamCraft. The conversation is kept for " + retention + " days, so you can come back to it; “New conversation” deletes it." }));
    }
    messages.forEach(function (m) { log.appendChild(renderMessage(m)); });
    if (messages.length) log.appendChild(el("p", { class: "tc-a-disclaimer", text: "Generated by an AI model from the PDM data – check numbers and states before you rely on them." }));
    scrollDown();
  }

  function showError(text) {
    var old = log.querySelector(".tc-a-error");
    if (old) old.remove();
    log.appendChild(el("p", { class: "tc-a-error", role: "alert", text: text }));
    scrollDown();
  }

  function load() {
    if (loaded || !api) return;
    loaded = true;
    status.textContent = "Loading the conversation …";
    api.thread().then(function (t) {
      retention = (t && t.retention_days) || retention;
      status.textContent = "";
      renderThread((t && t.messages) || []);
    }, function (err) {
      loaded = false;
      status.textContent = "";
      showError(String((err && err.message) || err || "The conversation could not be loaded."));
    });
  }

  // --- actions --------------------------------------------------------

  function setBusy(on) {
    busy = on;
    host.classList.toggle("is-busy", on);
    [input, sendBtn, resetBtn, drafts].forEach(function (c) { if (c) c.disabled = on; });
    quick.querySelectorAll("button").forEach(function (b) { b.disabled = on; });
    cancelBtn.hidden = !on;
    status.textContent = on ? "Looking it up in TeamCraft … this can take a minute." : "";
  }

  function send(action, text, label) {
    if (busy || !api) return;
    if (action === "ask" && !text) { status.textContent = "Type a question first."; input.focus(); return; }
    var empty = log.querySelector(".tc-a-empty");
    if (empty) empty.remove();
    var shared = contextOff ? {} : context();
    var here = describe(shared);
    var shown = text || label || action;
    log.appendChild(renderMessage({ role: "user", text: shown, context_label: here }));
    scrollDown();
    setBusy(true);
    controller = typeof AbortController === "function" ? new AbortController() : null;
    api.send({ action: action, question: text || null, context: shared, allow_drafts: !!(drafts && drafts.checked) }, controller && controller.signal).then(function (run) {
      setBusy(false);
      controller = null;
      input.value = "";
      // the server's stored messages are the truth (labels resolved with the user's rights)
      loaded = false;
      api.thread().then(function (t) {
        loaded = true;
        renderThread((t && t.messages) || []);
        var last = log.querySelectorAll(".tc-a-assistant");
        var tail = last[last.length - 1];
        if (tail && run) {
          var fresh = renderMessage({ role: "assistant", text: (run.answer && run.answer.text) || "" }, { truncated: run.answer && run.answer.truncated, tools: run.tools, steps: run.steps, model: run.model, allow_drafts: run.allow_drafts });
          tail.replaceWith(fresh);
        }
        scrollDown();
      }, function () { /* the answer is stored; it shows on the next open */ });
      if (run && run.allow_drafts && o.onDraftsMayHaveChanged) o.onDraftsMayHaveChanged();
    }, function (err) {
      setBusy(false);
      controller = null;
      if (err && err.name === "AbortError") {
        // the server stops the run when the request is dropped; nothing was stored
        loaded = false;
        status.textContent = "Cancelled.";
        load();
        return;
      }
      loaded = false;
      load();
      setTimeout(function () { showError(String((err && err.message) || err || "The assistant could not answer.")); }, 0);
    });
  }

  function cancel() {
    if (controller) controller.abort();
    else if (o.cancel) o.cancel();
  }

  function reset() {
    if (busy || !api) return;
    api.reset().then(function () {
      loaded = true;
      renderThread([]);
      input.focus();
    }, function (err) { showError(String((err && err.message) || err)); });
  }

  // --- open / close ---------------------------------------------------

  function open(focus) {
    if (isOpen) { if (focus) input.focus(); return; }
    isOpen = true;
    host.hidden = false;
    document.documentElement.classList.add("tc-assistant-open");
    store(STORE_OPEN, "1");
    renderContext();
    load();
    if (o.onToggle) o.onToggle(true);
    if (focus !== false) setTimeout(function () { input.focus(); }, 0);
  }

  function close(restoreFocus) {
    if (!isOpen) return;
    isOpen = false;
    host.hidden = true;
    document.documentElement.classList.remove("tc-assistant-open");
    store(STORE_OPEN, "0");
    if (o.onToggle) o.onToggle(false);
    if (restoreFocus && o.opener) o.opener.focus();
  }

  var api2 = {
    open: open,
    close: close,
    toggle: function () { if (isOpen) close(true); else open(); },
    isOpen: function () { return isOpen; },
    contextChanged: function () { if (isOpen) renderContext(); },
    ask: function (action, text, label) { open(false); send(action, text || "", label); },
    /** The stored open/closed state of this browser. */
    wasOpen: function () { return store(STORE_OPEN) === "1"; }
  };
  return api2;
}
