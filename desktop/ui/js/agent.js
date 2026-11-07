// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Agent: a local ACP agent.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, api, append, call, clear, dialog, errorText, field, h, label, setStatus, state, toast, tool } from "./core.js";
import { refreshWorklistCount } from "./shell.js";
import { navHead, selectView } from "./navigator.js";
import { closeTab, findTab, openTab } from "./tabs.js";
import { markdownLite } from "./assistant.js";
import { openWorkspaceFolder } from "./workspace.js";

// ======================================================================
// Agent (ACP) – a local agent the user configured (Claude Code through its
// ACP adapter, Gemini CLI …). Rust (src/acp.rs) runs it in the workspace
// folder and hands it TeamCraft's MCP server with the user's sign-in; here:
// the conversation, tool calls, the plan and permission prompts.
// ======================================================================

// The MCP prompts of the server (src/mcp/tools.rs), sent as the message.
export var AGENT_PROMPTS = [
  { name: "release_notes", label: "Release notes", needs: "revision" },
  { name: "release_check", label: "Release check", needs: "revision" },
  { name: "review_change", label: "Review this change", needs: "change" },
  { name: "worklist_triage", label: "Triage my worklist", needs: null }
];

// Longest agent message shown (characters), as MAX_CHUNK in src/acp.rs.
export var AGENT_MAX_TEXT = 200000;

export var TOOL_STATUS = { pending: "Waiting", in_progress: "Running", completed: "Done", failed: "Failed", cancelled: "Cancelled" };

export var agent = {
  profiles: null,   // { agents: [{name, command, args}], selected }
  running: null,    // the session as Rust describes it
  starting: false,
  early: [],        // events that arrived while the session was starting
  busy: false,
  panel: null,      // conversation and composer, kept across renders
  log: null,
  input: null,
  current: null,    // the agent message being streamed
  tools: {},
  toolOrder: [],
  plan: [],
  permissions: {},
  allowed: true,    // the server allows local agents (TEAMCRAFT_LOCAL_AGENTS)
  policyMessage: null,
  contextKey: null, // the revision or change tab the agent is told about
  paint: false,
  navDirty: false,
  dirtyBlocks: []
};

export function agentProfiles() {
  if (agent.profiles) return Promise.resolve(agent.profiles);
  return call("agent_profiles").then(function (p) { agent.profiles = p; return p; });
}

/** Local agents are allowed unless the server says otherwise
 * (`capabilities.local_agents` of GET /me; Rust checks it again on start). */
export function localAgentsAllowed() {
  var caps = state.me && state.me.capabilities;
  return agent.allowed && !(caps && caps.local_agents === false);
}

/** Asks Rust for the server's policy (it also disables View → Agent and
 * ends a running agent when off), then hides the Agent entry points. */
export function applyAgentPolicy() {
  function apply(allowed, message) {
    agent.allowed = allowed;
    agent.policyMessage = allowed ? null : message;
    document.querySelectorAll(".rail button[data-view=agent]").forEach(function (b) { b.classList.toggle("hidden", !allowed); });
    if (!allowed) {
      agent.running = null;
      agent.busy = false;
      if (findTab("agent")) closeTab("agent");
      if (state.view === "agent") selectView("home");
    }
  }
  return call("agent_policy").then(function (p) { apply(p.allowed, p.message); }, function () {
    // server not reachable: what GET /me said at sign-in
    var caps = state.me && state.me.capabilities;
    var allowed = !(caps && caps.local_agents === false);
    // agent_policy did not run, so Rust did not end a running agent: do it
    // here, or the UI would forget a child process that keeps running
    if (!allowed) call("agent_stop").catch(function () {});
    apply(allowed, "Local agents are turned off on this TeamCraft server by the administrator (TEAMCRAFT_LOCAL_AGENTS=off).");
  });
}

export function openAgent() {
  if (!localAgentsAllowed()) {
    toast(agent.policyMessage || "Local agents are turned off on this TeamCraft server by the administrator (TEAMCRAFT_LOCAL_AGENTS=off). Use Ask TeamCraft instead.", "error");
    return;
  }
  if (state.view !== "agent") selectView("agent");
  if (state.active !== "agent") openTab({ key: "agent", kind: "agent", title: "Agent" });
}

/** What the user looks at: the last revision or change tab. */
export function agentContext() {
  var tab = agent.contextKey && findTab(agent.contextKey);
  if (!tab || !tab.data) return null;
  if (tab.kind === "change") return { change: tab.data.change.number, title: tab.data.change.title };
  return { item: tab.data.rev.item_number, revision: tab.data.rev.code, title: tab.data.rev.name };
}

export function agentContextLabel() {
  var c = agentContext();
  if (!c) return null;
  return (c.change ? c.change : c.item + "/" + c.revision) + (c.title ? " " + c.title : "");
}

export function ensureAgentPanel() {
  if (agent.panel) return agent.panel;
  agent.log = h("div.agent-log", { "aria-live": "polite" });
  agent.input = h("textarea", {
    rows: "3", maxlength: "20000", placeholder: "Ask the agent – Enter sends, Shift+Enter starts a new line",
    on: {
      keydown: function (e) {
        if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); submitAgent(); }
      }
    }
  });
  var send = h("button.primary", { type: "submit" }, "Send");
  agent.sendButton = send;
  agent.contextEl = h("span.hint.agent-context");
  agent.panel = h("div.agent-panel", null, [
    agent.log,
    h("form.agent-composer", { on: { submit: function (e) { e.preventDefault(); submitAgent(); } } }, [
      agent.input,
      h("div.agent-composer-row", null, [agent.contextEl, send])
    ])
  ]);
  agentEmptyState();
  return agent.panel;
}

export function agentEmptyState() {
  if (agent.log.firstChild) return;
  agent.log.appendChild(h("div.agent-empty", null, [
    h("h3", { text: "Agent" }),
    h("p", { text: "Work with a local AI agent – Claude Code, Gemini CLI or any agent that speaks the Agent Client Protocol. It uses TeamCraft's tools with your rights: search, structures, where-used, change orders, release checks." }),
    h("p.hint", { text: "Start an agent in the navigator on the left. Prompts and the data it looks up go to the agent's AI provider." })
  ]));
}

export function clearAgentEmpty() {
  var empty = agent.log && agent.log.querySelector(".agent-empty");
  if (empty) empty.remove();
}

export function scrollAgent() {
  var log = agent.log;
  if (!log) return;
  // follow the stream only when the user has not scrolled up
  if (log.scrollHeight - log.scrollTop - log.clientHeight < 120) log.scrollTop = log.scrollHeight;
}

export function agentRow(el) {
  ensureAgentPanel();
  clearAgentEmpty();
  var follow = agent.log.scrollHeight - agent.log.scrollTop - agent.log.clientHeight < 120;
  agent.log.appendChild(el);
  if (follow) agent.log.scrollTop = agent.log.scrollHeight;
  return el;
}

export function agentNote(text, kind) {
  agent.current = null;
  agentRow(h("p.agent-note" + (kind ? "." + kind : ""), { text: text }));
}

/** Message and thought chunks: one growing block per message. */
export function agentChunk(kind, text) {
  var cur = agent.current;
  if (!cur || cur.kind !== kind) {
    var body = h("div");
    var el = kind === "thought"
      ? h("details.agent-thought", null, [h("summary", { text: "Thinking" }), body])
      : h("div.agent-msg.agent", null, body);
    cur = agent.current = { kind: kind, text: "", body: body };
    agentRow(el);
  }
  // bounded: every frame redraws the whole message
  if (cur.full) return;
  if (cur.text.length + text.length > AGENT_MAX_TEXT) {
    text = text.slice(0, Math.max(0, AGENT_MAX_TEXT - cur.text.length)) + "\n\n… (the rest of this message is not shown)";
    cur.full = true;
  }
  cur.text += text;
  if (!cur.dirty) { cur.dirty = true; agent.dirtyBlocks.push(cur); }
  schedulePaint();
}

/** Redraws the streamed markdown (and, with `nav`, the navigator) at most once per frame. */
export function schedulePaint(nav) {
  if (nav) agent.navDirty = true;
  if (agent.paint) return;
  agent.paint = true;
  requestAnimationFrame(function () {
    agent.paint = false;
    var blocks = agent.dirtyBlocks;
    agent.dirtyBlocks = [];
    blocks.forEach(function (cur) {
      cur.dirty = false;
      clear(cur.body);
      cur.body.appendChild(cur.kind === "thought" ? h("p", { text: cur.text }) : markdownLite(cur.text));
    });
    if (blocks.length) scrollAgent();
    if (agent.navDirty) {
      agent.navDirty = false;
      if (state.view === "agent") renderAgentNav();
    }
  });
}

export function agentTool(p) {
  var t = agent.tools[p.id];
  if (!t) {
    t = agent.tools[p.id] = { title: p.id, kind: "other", status: "pending", text: null };
    agent.toolOrder.push(p.id);
    t.el = h("div.agent-tool");
    agent.current = null;
    agentRow(t.el);
  }
  ["title", "kind", "status", "text"].forEach(function (k) { if (p[k] !== null && p[k] !== undefined) t[k] = p[k]; });
  clear(t.el);
  t.el.className = "agent-tool status-" + t.status;
  append(t.el, [
    h("span.agent-tool-dot", { "aria-hidden": "true" }),
    h("span.agent-tool-title", { text: t.title }),
    h("span.badge", { text: TOOL_STATUS[t.status] || label(t.status) })
  ]);
  if (t.text) t.el.appendChild(h("details.agent-tool-out", null, [h("summary", { text: "Output" }), h("pre", { text: t.text })]));
  schedulePaint(true);
}

export function agentPermission(p) {
  var tool = p.tool_id && agent.tools[p.tool_id];
  var title = tool && tool.title && tool.title !== p.tool_id ? tool.title : p.title;
  var buttons = h("div.row", null, [
    p.can_allow ? h("button.primary", { type: "button", on: { click: function () { answer(true); } } }, "Allow once") : null,
    h("button", { type: "button", on: { click: function () { answer(false); } } }, "Reject")
  ]);
  var card = h("div.agent-permission", { role: "alert" }, [
    h("strong", { text: "The agent asks for permission" }),
    h("p", { text: title }),
    p.can_allow ? null : h("p.hint", { text: "The agent offers no one-time permission – TeamCraft Desktop never grants permanent ones." }),
    buttons
  ]);
  agent.current = null;
  // keys restart with every session: the session is part of the name
  var id = p.session + ":" + p.key;
  agent.permissions[id] = { card: card, buttons: buttons };
  agentRow(card);
  function answer(allow) {
    buttons.querySelectorAll("button").forEach(function (b) { b.disabled = true; });
    call("agent_permission", { session: p.session, key: p.key, allow: allow }).then(function () {
      donePermission(id, allow ? "Allowed once." : "Rejected.");
    }, function (err) { donePermission(id, errorText(err)); });
  }
}

export function donePermission(id, text) {
  var perm = agent.permissions[id];
  if (!perm) return;
  delete agent.permissions[id];
  perm.card.classList.add("answered");
  perm.buttons.replaceWith(h("p.hint", { text: text || "No longer needed." }));
}

/** The session ended: its open permission cards can no longer be answered. */
export function endAgentPermissions() {
  Object.keys(agent.permissions).forEach(function (id) { donePermission(id); });
}

export function onAgentEvent(p) {
  if (agent.starting) { agent.early.push(p); return; }
  if (!agent.running || p.session !== agent.running.session) return;
  switch (p.type) {
    case "message": agentChunk("message", p.text); break;
    case "thought": agentChunk("thought", p.text); break;
    case "tool": agentTool(p); break;
    case "plan": agent.plan = p.entries || []; schedulePaint(true); break;
    case "permission": agentPermission(p); break;
    case "permission_done": donePermission(p.session + ":" + p.key); break;
    case "file_read": if (!p.ok) agentNote("Refused to read " + p.path + " (only the workspace folder is shared).", "warn"); break;
    case "notice": agentNote(p.text, "info"); break;
    case "models": agent.running.models = p.models; schedulePaint(true); break;
    case "exited":
      agentNote(p.text, "error");
      endAgentPermissions();
      agent.running = null;
      agent.busy = false;
      updateAgentControls();
      schedulePaint(true);
      break;
  }
}

export function updateAgentControls() {
  if (!agent.panel) return;
  var ready = !!agent.running && !agent.busy;
  agent.sendButton.disabled = !ready;
  agent.input.disabled = !agent.running;
  var ctx = agentContextLabel();
  agent.contextEl.textContent = agent.running
    ? (agent.busy ? "Working … " : "") + (ctx ? "Context: " + ctx : "No context – open a revision or change to give the agent one")
    : "No agent running";
  if (state.active === "agent") renderAgentToolbar();
}

export function startAgent(name) {
  if (agent.starting) return;
  agent.starting = true;
  agent.early = [];
  schedulePaint(true);
  setStatus("Starting " + name + " – the first start can take a while …");
  call("agent_start", { name: name }).then(function (s) {
    setStatus("");
    agent.starting = false;
    endAgentPermissions(); // the previous session was stopped
    agent.running = s;
    agent.busy = false;
    agent.tools = {};
    agent.toolOrder = [];
    agent.plan = [];
    agent.profiles.selected = name;
    var who = (s.info.agent || name) + (s.info.version ? " " + s.info.version : "");
    agentNote(who + " is ready in " + state.session.workspace_dir + (s.info.teamcraft_tools ? " – with TeamCraft's tools." : "."), "ok");
    agent.early.forEach(onAgentEvent);
    agent.early = [];
    updateAgentControls();
    schedulePaint(true);
    if (agent.input) agent.input.focus();
  }, function (err) {
    setStatus("");
    agent.starting = false;
    agent.early = [];
    agentNote(errorText(err), "error");
    schedulePaint(true);
  });
}

export function stopAgent() {
  call("agent_stop").then(function () {
    if (agent.running) agentNote("The agent was stopped.");
    endAgentPermissions();
    agent.running = null;
    agent.busy = false;
    updateAgentControls();
    schedulePaint(true);
  }, function (e) { toast(errorText(e), "error"); });
}

export function cancelAgent() {
  if (!agent.busy) return;
  call("agent_cancel").catch(function (e) { toast(errorText(e), "error"); });
}

export function submitAgent() {
  var text = agent.input.value.trim();
  if (!text) { agent.input.focus(); return; }
  if (sendAgent(text, null)) agent.input.value = "";
}

/** Sends a message; `shown` replaces the text in the conversation (quick prompts). */
export function sendAgent(text, shown) {
  if (!agent.running || agent.busy) return false;
  agent.current = null;
  agentRow(h("div.agent-msg.user", { text: shown || text }));
  agent.busy = true;
  updateAgentControls();
  schedulePaint(true);
  call("agent_prompt", { text: text, context: agentContext() }).then(function (stop) {
    finishTurn(stop);
  }, function (err) {
    finishTurn(null);
    agentNote(errorText(err), "error");
  });
  return true;
}

export function finishTurn(stop) {
  agent.busy = false;
  agent.current = null;
  var notes = {
    cancelled: "Cancelled.",
    max_tokens: "The agent stopped at its token limit.",
    max_turn_requests: "The agent stopped after too many steps in one turn.",
    refusal: "The agent refused to continue."
  };
  if (stop && notes[stop]) agentNote(notes[stop], stop === "cancelled" ? "" : "warn");
  if (stop === "cancelled") {
    agent.toolOrder.forEach(function (id) {
      var t = agent.tools[id];
      if (t.status === "pending" || t.status === "in_progress") agentTool({ id: id, status: "cancelled" });
    });
  }
  updateAgentControls();
  schedulePaint(true);
  refreshWorklistCount();
}

/** A quick prompt: the server's MCP prompt text, filled with the context. */
export function quickPrompt(p) {
  var c = agentContext() || {};
  var args = p.needs === "revision" ? { item: c.item, revision: c.revision } : p.needs === "change" ? { change: c.change } : {};
  api("POST", "/mcp", { jsonrpc: "2.0", id: 1, method: "prompts/get", params: { name: p.name, arguments: args } }).then(function (r) {
    if (r && r.error) throw new Error(r.error.message);
    var msg = r && r.result && r.result.messages && r.result.messages[0];
    var text = msg && msg.content && msg.content.text;
    if (!text) throw new Error("The server has no prompt " + p.name + ".");
    sendAgent(text, p.label + (p.needs === "revision" ? " – " + c.item + "/" + c.revision : p.needs === "change" ? " – " + c.change : ""));
  }).catch(function (e) { toast(errorText(e), "error"); });
}

export function renderAgentNav() {
  if (state.view !== "agent") return Promise.resolve();
  if (!localAgentsAllowed()) {
    var nav = $("navigator");
    clear(nav);
    nav.appendChild(navHead("Agent", []));
    nav.appendChild(h("div.agent-nav", null, [h("p.hint", { text: agent.policyMessage || "Local agents are turned off on this TeamCraft server by the administrator (TEAMCRAFT_LOCAL_AGENTS=off)." })]));
    return Promise.resolve();
  }
  return agentProfiles().then(function (p) {
    if (state.view !== "agent") return;
    var nav = $("navigator");
    var scroll = nav.scrollTop;
    clear(nav);
    nav.appendChild(navHead("Agent", [
      h("button.small", { type: "button", title: "Configure agents", on: { click: agentSettingsDialog } }, "Agents…")
    ]));
    if (!p.agents.length) {
      nav.appendChild(h("div.agent-nav", null, [
        h("p", { text: "No agent configured yet." }),
        h("p.hint", { text: "Add Claude Code (through its ACP adapter), Gemini CLI or another agent that speaks the Agent Client Protocol. TeamCraft starts nothing on its own." }),
        h("button.primary", { type: "button", on: { click: agentSettingsDialog } }, "Set up an agent…")
      ]));
      return;
    }
    var running = agent.running;
    var pick = h("select", { "aria-label": "Agent", disabled: !!running || agent.starting },
      p.agents.map(function (a) {
        var name = running ? running.name : p.selected;
        return h("option", { value: a.name, selected: a.name === name }, a.name);
      }));
    var start = running
      ? h("button", { type: "button", title: "End the agent process", on: { click: stopAgent } }, "Stop")
      : h("button.primary", { type: "button", disabled: agent.starting, on: { click: function () { startAgent(pick.value); } } }, agent.starting ? "Starting…" : "Start");
    var info = running
      ? [h("span.badge.badge-released", { text: agent.busy ? "working" : "ready" }), " ", (running.info.agent || running.name) + (running.info.version ? " " + running.info.version : ""),
        h("br"), h("span.hint", { text: running.info.teamcraft_tools ? "With TeamCraft's tools (your rights)" : "Without TeamCraft's tools – the agent cannot use MCP over HTTP" })]
      : [h("span.hint", { text: agent.starting ? "Starting …" : "Not running" })];
    var ctx = agentContext();
    var quick = AGENT_PROMPTS.map(function (q) {
      var ok = running && !agent.busy && running.info.teamcraft_tools &&
        (!q.needs || (q.needs === "revision" ? ctx && ctx.item : ctx && ctx.change));
      return h("button.chip", {
        type: "button", disabled: !ok, title: q.needs && !ok ? "Open a " + q.needs + " first" : "Uses the TeamCraft prompt " + q.name,
        on: { click: function () { openAgent(); quickPrompt(q); } }
      }, q.label);
    });
    var plan = agent.plan.length ? h("ol.agent-plan", null, agent.plan.map(function (e) {
      return h("li.status-" + e.status, { text: e.content });
    })) : h("p.empty", { text: "No plan shared yet." });
    var tools = agent.toolOrder.length ? h("ul.tree", null, agent.toolOrder.slice(-40).reverse().map(function (id) {
      var t = agent.tools[id];
      return h("li", null, h("div.node", {
        title: t.title, on: { click: function () { openAgent(); if (t.el) t.el.scrollIntoView({ block: "center" }); } }
      }, [h("span.label", { text: t.title }), h("span.meta", null, h("span.badge", { text: TOOL_STATUS[t.status] || label(t.status) }))]));
    })) : h("p.empty", { text: "No tool calls yet." });
    nav.appendChild(h("div.agent-nav", null, [
      h("div.row", null, [pick, start]),
      h("p.small", null, info),
      agentModelPicker(),
      h("h3", { text: "Context" }),
      h("p.small", { text: agentContextLabel() || "None – open a revision or change order" }),
      h("h3", { text: "Quick prompts" }),
      h("div.assistant-quick", null, quick),
      h("h3", { text: "Plan" }), plan,
      h("h3", { text: "Tool calls" }), tools
    ]));
    nav.scrollTop = scroll;
  });
}

/** The models the running agent offers (its own list – the agent brings
 * provider and sign-in); nothing when it offers no choice. */
export function agentModelPicker() {
  var running = agent.running;
  var models = running && running.models;
  if (!models || !models.options || !models.options.length) return null;
  var current = models.current;
  var options = models.options.map(function (o) {
    return h("option", { value: o.id, selected: o.id === current, title: o.description || o.id }, o.name);
  });
  if (current && !models.options.some(function (o) { return o.id === current; })) {
    options.unshift(h("option", { value: current, selected: true }, current));
  }
  var select = h("select", {
    "aria-label": "Model",
    disabled: agent.busy || agent.switchingModel || models.options.length < 2,
    title: agent.busy ? "Wait until the agent has finished its answer" : "The agent's model – remembered for its next start",
    on: { change: function () { setAgentModel(running.session, select.value); } }
  }, options);
  return h("div.agent-model", null, [h("label.small", null, ["Model ", select])]);
}

export function setAgentModel(session, model) {
  agent.switchingModel = true;
  schedulePaint(true);
  call("agent_set_model", { session: session, model: model }).then(function (models) {
    agent.switchingModel = false;
    if (agent.running && agent.running.session === session) agent.running.models = models;
    var picked = models.options.filter(function (o) { return o.id === models.current; })[0];
    agentNote("Model: " + (picked ? picked.name : models.current || model), "info");
    schedulePaint(true);
  }, function (err) {
    agent.switchingModel = false;
    toast(errorText(err), "error");
    schedulePaint(true); // back to the model in use
  });
}

/** Toolbar button of a revision or change: opens the agent with it as context. */
export function agentButton(tab) {
  if (!localAgentsAllowed()) return null;
  if (!agent.profiles || !agent.profiles.agents.length) return null;
  return tool("Agent", "agent", function () {
    agent.contextKey = tab.key;
    openAgent();
  }, { title: "Work on this with your local agent (" + (agent.running ? agent.running.name : "not started") + ")" });
}

/** The rail's Agent view: controls in the navigator, the conversation as a tab. */
export function renderAgentView() {
  // a restored or remembered view must not reopen what the server turned off
  if (!localAgentsAllowed()) {
    if (findTab("agent")) closeTab("agent");
    selectView("home");
    return Promise.resolve();
  }
  if (!findTab("agent")) openTab({ key: "agent", kind: "agent", title: "Agent" });
  return renderAgentNav();
}

export function renderAgentToolbar() {
  var toolbar = $("toolbar");
  clear(toolbar);
  append(toolbar, [
    tool("Cancel", "cancel", cancelAgent, { disabled: !agent.busy, danger: agent.busy, title: "Stop the running turn (session/cancel)" }),
    tool("Clear", "refresh", function () {
      clear(agent.log); agent.current = null; agentEmptyState();
    }, { title: "Clear the conversation view (the agent keeps its session)" }),
    h("span.sep"),
    tool("Workspace", "folder", function () { openWorkspaceFolder(null); }, { title: "Open the workspace folder – the agent's working directory" }),
    tool("Agents…", "edit", agentSettingsDialog, { title: "Configure agents" })
  ]);
}

export function renderAgentTab(body) {
  body.appendChild(ensureAgentPanel());
  renderAgentToolbar();
  updateAgentControls();
  scrollAgent();
  if (agent.running && !agent.busy) agent.input.focus();
}

export var AGENT_PRESETS = [
  { name: "Claude Code", command: "npx", args: "-y @zed-industries/claude-code-acp", hint: "Needs Node.js; npx installs the ACP adapter from npm on the first start. Sign in once with `claude` in a terminal or set ANTHROPIC_API_KEY." },
  { name: "Gemini CLI", command: "gemini", args: "--experimental-acp", hint: "Needs the Gemini CLI installed (npm install -g @google/gemini-cli) and signed in." }
];

export function agentSettingsDialog() {
  call("agent_profiles").then(function (p) {
    var windows = /Windows/i.test(navigator.userAgent);
    var rows = h("div.stack");
    function addRow(a, hint) {
      var name = h("input", { type: "text", value: a.name, maxlength: "60", placeholder: "Claude Code" });
      var cmd = h("input", { type: "text", value: a.command, spellcheck: "false", placeholder: windows ? "npx.cmd or C:\\…\\agent.exe" : "npx or /usr/local/bin/gemini" });
      var args = h("input", { type: "text", value: a.args, spellcheck: "false", placeholder: "--experimental-acp" });
      var row = h("div.agent-row", null, [
        h("div.agent-row-fields", null, [field("Name", name), field("Program", cmd), field("Arguments", args)]),
        hint ? h("p.hint", { text: hint }) : null,
        h("button.small", { type: "button", on: { click: function () { row.remove(); } } }, "Remove")
      ]);
      row.agentEntry = function () { return { name: name.value, command: cmd.value, args: args.value }; };
      rows.appendChild(row);
      return name;
    }
    p.agents.forEach(function (a) { addRow(a); });
    var presets = AGENT_PRESETS.map(function (pr) {
      return h("button.chip", { type: "button", on: { click: function () {
        addRow({ name: pr.name, command: windows && pr.command === "npx" ? "npx.cmd" : pr.command, args: pr.args }, pr.hint).focus();
      } } }, "+ " + pr.name);
    }).concat([h("button.chip", { type: "button", on: { click: function () { addRow({ name: "", command: "", args: "" }).focus(); } } }, "+ Other agent")]);
    var body = h("div.stack", null, [
      h("p.hint", { text: "An agent is a program on this computer that speaks the Agent Client Protocol (ACP). TeamCraft starts it only when you press Start, in your workspace folder, and gives it TeamCraft's tools with your sign-in – it can do in TeamCraft what you can do, but never submit, approve or release." }),
      h("p.hint", { text: "Through TeamCraft it may only read files in the workspace folder – no writing, no terminal. The agent is still a program running as you: its own tools ask for permission here, answer them with care. Desktop apps often do not see your shell's PATH – an absolute program path is safest." }),
      rows,
      h("div.assistant-quick", null, presets)
    ]);
    dialog("Agents", body, [
      { label: "Close", onClick: function (d) { d.close(); } },
      {
        label: "Save", primary: true, submit: true, onClick: function (d) {
          var entries = Array.prototype.map.call(rows.children, function (r) { return r.agentEntry(); });
          call("set_agent_profiles", { agents: entries }).then(function (saved) {
            agent.profiles = saved;
            d.close();
            toast(saved.agents.length === 1 ? "1 agent saved" : saved.agents.length + " agents saved");
            if (state.view === "agent") renderAgentNav();
          }, function (err) { d.fail(err); });
        }
      }
    ], { wide: true });
  }, function (e) { toast(errorText(e), "error"); });
}
