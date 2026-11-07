// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – user interface (plain JavaScript, no build step).
//
// Layout in the style of classic PLM rich clients: a global navigation rail
// (Home, Worklist, Changes, Search), a navigator tree, object tabs with
// sub-tabs (Overview, Files, Structure, Where used, Changes, History,
// 3D viewer, Workspace), a context toolbar and a status bar.
//
// Every server call goes through the Rust commands (src/commands.rs); data
// from the server is only ever inserted as text, never as HTML.
//
// Modules: core.js, skins.js, connect.js, shell.js, navigator.js, tabs.js, assistant.js, agent.js, revision.js, references.js, workspace.js, change.js, requests.js, dialogs.js, bom-import.js, group-sync.js, folder-import.js, settings.js, main.js.

import { $, TAURI, call, errorText, h, invoke, setStatus, state, toast } from "./core.js";
import { applySkin } from "./skins.js";
import { checkServerVersion, initConnect, showConnect } from "./connect.js";
import { enterShell, initShell, onPackageProgress, refreshWorklistCount, uploadLine } from "./shell.js";
import { selectView } from "./navigator.js";
import { activeTab, reloadActive } from "./tabs.js";
import { openAssistant } from "./assistant.js";
import { agent, onAgentEvent, openAgent } from "./agent.js";
import { onGetProgress, openWorkspaceFolder, uploadWatcher } from "./workspace.js";
import { newItemDialog } from "./dialogs.js";
import { importBomDialog } from "./bom-import.js";
import { getProjectsDialog } from "./group-sync.js";
import { importFolderDialog, importTutorial } from "./folder-import.js";
import { openAbout, openSettings, signOut } from "./settings.js";

// ======================================================================
// Zoom: Ctrl + mouse wheel (trackpads: pinch), Ctrl/Cmd + + / - / 0
// ======================================================================

export var ZOOM_MIN = 0.5, ZOOM_MAX = 2.5;

export function setZoom(level) {
  level = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, level));
  if (Math.abs(level - state.zoom) < 0.0005) return;
  state.zoom = level;
  showZoom();
  invoke("set_zoom", { level: level }).catch(function (e) { toast(errorText(e), "error"); });
}

export function showZoom() {
  var el = $("status-zoom");
  el.textContent = Math.round(state.zoom * 100) + " %";
  el.classList.toggle("hidden", Math.abs(state.zoom - 1) < 0.005);
}

export function initZoom() {
  // capture phase: also above the 3D view, which zooms the model with the plain wheel
  window.addEventListener("wheel", function (e) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    e.stopPropagation();
    // one mouse wheel notch ≈ 10 %; the small steps of trackpads zoom smoothly
    var delta = Math.max(-50, Math.min(50, e.deltaY));
    setZoom(state.zoom * Math.exp(-delta * 0.002));
  }, { passive: false, capture: true });
  document.addEventListener("keydown", function (e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    if (e.key === "+" || e.key === "=") setZoom(state.zoom * 1.1);
    else if (e.key === "-" || e.key === "_") setZoom(state.zoom / 1.1);
    else if (e.key === "0") setZoom(1);
    else return;
    e.preventDefault();
  });
  $("status-zoom").addEventListener("click", function () { setZoom(1); });
}

// ======================================================================
// Menu
// ======================================================================

export function handleMenu(id) {
  var shellVisible = !$("shell").classList.contains("hidden");
  switch (id) {
    case "about": case "about-app": openAbout(); return;
    case "zoom-in": setZoom(state.zoom * 1.1); return;
    case "zoom-out": setZoom(state.zoom / 1.1); return;
    case "zoom-reset": setZoom(1); return;
  }
  if (!shellVisible) return;
  switch (id) {
    case "new-item": newItemDialog(); break;
    case "get-projects": getProjectsDialog(); break;
    case "import-folder": importFolderDialog(); break;
    case "import-bom": importBomDialog(null); break;
    case "import-help": importTutorial(); break;
    case "settings": case "settings-app": openSettings(); break;
    case "sign-out": signOut(); break;
    case "refresh": selectView(state.view); reloadActive(); refreshWorklistCount(); break;
    case "search": $("search-input").focus(); $("search-input").select(); break;
    case "go-home": selectView("home"); break;
    case "go-worklist": selectView("worklist"); break;
    case "go-changes": selectView("changes"); break;
    case "go-agent": openAgent(); break;
    case "go-assistant": openAssistant(false); break;
    case "open-workspace": openWorkspaceFolder(activeTab() && activeTab().kind === "revision" ? activeTab() : null); break;
    case "open-web": {
      var t = activeTab();
      var path = !t || !t.data ? "/" : t.kind === "change" ? "/changes/" + t.data.change.id :
        t.kind === "request" ? "/requests/" + t.data.change_request.id : t.data.rev ? "/revisions/" + t.data.rev.id : "/";
      call("open_web", { path: path }).catch(function (e) { toast(errorText(e), "error"); });
      break;
    }
  }
}

// ======================================================================
// Start
// ======================================================================

export function boot() {
  initConnect();
  initShell();
  initZoom();
  TAURI.event.listen("menu", function (e) { handleMenu(e.payload); });
  // large files are sent in resumable chunks: show how far they are
  TAURI.event.listen("upload-progress", function (e) {
    var p = e.payload || {};
    setStatus(uploadLine(p));
    if (uploadWatcher) uploadWatcher(p);
  });
  // Pack and Go: progress dialog with Cancel
  TAURI.event.listen("package-progress", function (e) { onPackageProgress(e.payload || {}); });
  TAURI.event.listen("agent", function (e) { onAgentEvent(e.payload); });
  TAURI.event.listen("workspace-progress", function (e) { onGetProgress(e.payload); });
  $("status-get-cancel").addEventListener("click", function () {
    $("status-get-text").textContent = "Stopping …";
    call("workspace_cancel").catch(function () {});
  });
  call("session").then(function (s) {
    state.session = s;
    state.zoom = s.zoom || 1;
    showZoom();
    applySkin(s.skin);
    if (!s.server_url) showConnect("server");
    else if (!s.signed_in) { showConnect("signin"); checkServerVersion(); }
    else enterShell();
  }, function (err) {
    document.body.appendChild(h("p.notice.error", { text: "TeamCraft could not start: " + errorText(err) }));
  });
}

boot();
