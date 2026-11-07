// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Settings, sign-out and About.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, SKINS, TAURI, call, dialog, errorText, field, h, label, state, toast } from "./core.js";
import { applySkin } from "./skins.js";
import { checkServerUrl, showConnect } from "./connect.js";
import { checkUpdate } from "./shell.js";
import { resetAssistant } from "./assistant.js";
import { agent, agentSettingsDialog, endAgentPermissions } from "./agent.js";

export function openSettings() {
  var current = document.documentElement.getAttribute("data-skin");
  var ws = h("input", { type: "text", value: state.session.workspace_dir, spellcheck: "false" });
  var updateCheck = h("input", { type: "checkbox", checked: !!state.session.update_check, on: { change: function () {
    var on = updateCheck.checked;
    call("set_update_check", { enabled: on }).then(function () {
      state.session.update_check = on;
      checkUpdate();
    }, function (e) { updateCheck.checked = !on; toast(errorText(e), "error"); });
  } } });
  var serverInsecure = h("p.notice.warn.small.hidden", { text: "This server is reached over plain http:// – the token and all data cross the network unencrypted. Ask your administrator for the https:// address." });
  checkServerUrl(state.session.server_url || "", serverInsecure);
  var skins = h("div.skin-grid", null, SKINS.map(function (s) {
    return h("button.skin-card" + (s.id === current ? ".selected" : ""), {
      type: "button", "data-skin": s.id, title: s.description,
      on: {
        click: function () {
          applySkin(s.id);
          call("set_skin", { skin: s.id }).catch(function (e) { toast(errorText(e), "error"); });
          skins.querySelectorAll(".skin-card").forEach(function (c) { c.classList.toggle("selected", c.getAttribute("data-skin") === s.id); });
        }
      }
    }, [h("img", { src: "/static/media/" + s.logo, alt: "" }), h("strong", { text: s.name }), h("span.small", { text: s.description }),
      h("span.skin-swatch", { "aria-hidden": "true" }, [h("span"), h("span"), h("span"), h("span")])]);
  }));
  var body = h("div.stack", null, [
    h("h3", { text: "Appearance" }),
    h("p.hint", { text: "Choose how TeamCraft Desktop looks. The distribution skins are dark." }),
    skins,
    h("h3", { text: "Workspace" }),
    field("Folder for checked-out files", h("div.row", null, [ws,
      TAURI.dialog ? h("button", { type: "button", on: { click: function () {
        TAURI.dialog.open({ directory: true, defaultPath: ws.value }).then(function (dir) { if (dir) ws.value = dir; });
      } } }, "Choose…") : null])),
    h("h3", { text: "Agents" }),
    h("div.row", null, [
      h("span.hint", { text: "Local AI agents (ACP) that work with TeamCraft's tools – none until you add one." }),
      h("button", { type: "button", on: { click: function () { dlg.close(); agentSettingsDialog(); } } }, "Agents…")
    ]),
    h("h3", { text: "Updates" }),
    h("label.check", null, [updateCheck, "Tell me when a new version is out"]),
    h("p.hint", { text: "Asks GitHub for the latest TeamCraft release at most once a day and shows a link in the status bar. Nothing is downloaded or installed by the app." }),
    h("h3", { text: "Account" }),
    h("p", null, ["Signed in to ", h("strong", { text: state.session.server_url }), " as ", h("strong", { text: state.me.display_name }), " (" + label(state.me.role) + ") on ", h("span", { text: state.session.device })]),
    state.session.token_storage === "file"
      ? h("p.notice.warn.small", { text: "No keychain available: the sign-in token is stored in a file in the app's settings folder that only your user account can read, not in the system keychain. On Linux, start a Secret Service (GNOME Keyring or KWallet) and restart the app – the token then moves into the keychain." })
      : h("p.hint", { text: "The sign-in token is kept in the system keychain." }),
    serverInsecure,
    h("div.row", null, [
      h("button.danger", { type: "button", on: { click: function () { dlg.close(); signOut(); } } }, "Sign out"),
      h("button", { type: "button", on: { click: function () { dlg.close(); signOut(true); } } }, "Change server")
    ])
  ]);
  var dlg = dialog("Settings", body, [
    { label: "Close", onClick: function (d) { d.close(); } },
    {
      label: "Save", primary: true, submit: true, onClick: function (d) {
        if (ws.value.trim() === String(state.session.workspace_dir)) { d.close(); return; }
        call("set_workspace_dir", { dir: ws.value.trim() }).then(function () {
          state.session.workspace_dir = ws.value.trim();
          $("status-ws").textContent = "Workspace: " + state.session.workspace_dir;
          d.close();
          toast("Workspace folder saved");
        }, function (err) { d.fail(err); });
      }
    }
  ], { wide: true });
}

export function signOut(changeServer) {
  call("sign_out").then(function () {
    state.session.signed_in = false;
    state.tabs = [];
    state.active = null;
    state.me = null;
    resetAssistant();
    endAgentPermissions();
    agent.running = null; // signing out stops the agent (src/commands.rs)
    agent.busy = false;
    agent.contextKey = null;
    showConnect(changeServer ? "server" : "signin");
  }, function (e) { toast(errorText(e), "error"); });
}

export function openAbout() {
  dialog("About TeamCraft", h("div.stack", null, [
    h("img.connect-logo.skin-logo-dark", { src: "/static/media/logos/TeamCraft_Logo_White.svg", alt: "TeamCraft" }),
    h("img.connect-logo.skin-logo-light", { src: "/static/media/logos/TeamCraft_Logo_Blue.svg", alt: "TeamCraft" }),
    h("p", { text: "TeamCraft Desktop " + state.session.version + " – the rich client of TeamCraft, the open-source product lifecycle management." }),
    h("p.small", { text: "Pre-release: TeamCraft is a work in progress and has no official release yet. It is provided as is, without warranty of any kind (MPL-2.0, sections 6 and 7). Keep backups of your data." }),
    h("p.small.muted", { text: "TeamCraft is open source under the Mozilla Public License 2.0. Font: Montserrat (SIL Open Font License 1.1), text in the system UI face." }),
    h("p.small.muted", { text: "Server: " + (state.session.server_url || "—") })
  ]), [{ label: "Close", primary: true, onClick: function (d) { d.close(); } }]);
}
