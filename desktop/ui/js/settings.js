// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Settings, sign-out and About.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, SKINS, TAURI, api, call, dialog, errorText, field, h, label, state, toast } from "./core.js";
import { applySkin } from "./skins.js";
import { checkServerUrl, showConnect } from "./connect.js";
import { checkUpdate } from "./shell.js";
import { resetCompanyAi } from "./assistant.js";
import { agent, agentSettingsDialog, endAgentPermissions, localAgentsAllowed, localAgentsOffText } from "./agent.js";

export function openSettings() {
  var current = document.documentElement.getAttribute("data-skin");
  var ws = h("input", { type: "text", value: state.session.workspace_dir, spellcheck: "false" });
  var withParts = h("input", { type: "checkbox", checked: state.session.checkout_parts !== false, on: { change: function () {
    var on = withParts.checked;
    call("set_checkout_parts", { enabled: on }).then(function () { state.session.checkout_parts = on; },
      function (e) { withParts.checked = !on; toast(errorText(e), "error"); });
  } } });
  var notifyBox = h("div.stack");
  var notify = notificationSettings(notifyBox);
  var updateCheck = h("input", { type: "checkbox", checked: !!state.session.update_check, on: { change: function () {
    var on = updateCheck.checked;
    call("set_update_check", { enabled: on }).then(function () {
      state.session.update_check = on;
      checkUpdate();
    }, function (e) { updateCheck.checked = !on; toast(errorText(e), "error"); });
  } } });
  var crashPrompt = h("input", { type: "checkbox", checked: state.session.crash_prompt !== false, on: { change: function () {
    var on = crashPrompt.checked;
    call("set_crash_prompt", { enabled: on }).then(function () { state.session.crash_prompt = on; },
      function (e) { crashPrompt.checked = !on; toast(errorText(e), "error"); });
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
    h("label.check", null, [withParts, "When checking out an assembly, also get its parts"]),
    h("p.hint", { text: "Only the assembly is locked. Its parts are copied read-only into your working folder, so it opens in the CAD program; parts you changed locally are never overwritten and are listed in the report. Switch off to copy only the assembly." }),
    // the administrator may have turned local agents off (TEAMCRAFT_LOCAL_AGENTS):
    // then only why, and where the company AI is
    h("h3", { text: "Agents" }),
    !localAgentsAllowed() ? h("p.hint", { text: localAgentsOffText() }) : null,
    localAgentsAllowed() ? h("div.row", null, [
      h("span.hint", { text: "Local AI agents (ACP) that work with TeamCraft's tools – none until you add one." }),
      h("button", { type: "button", on: { click: function () { dlg.close(); agentSettingsDialog(); } } }, "Agents…")
    ]) : null,
    h("h3", { text: "Notifications" }),
    notifyBox,
    h("h3", { text: "Updates" }),
    h("label.check", null, [updateCheck, "Tell me when a new version is out"]),
    h("p.hint", { text: "Asks GitHub for the latest TeamCraft release at most once a day and shows a link in the status bar. Nothing is downloaded or installed by the app." }),
    h("h3", { text: "Crash reports" }),
    h("label.check", null, [crashPrompt, "After a crash, ask at the next start whether to share the report"]),
    h("p.hint", { text: "Crashes and uncaught errors are written to a report on this computer (in the app's settings folder, without your home folder, server addresses or tokens). It is only shared when you open the GitHub issue yourself. Switched off, the reports stay on this computer and are never shown." }),
    h("h3", { text: "Account" }),
    h("p", null, ["Signed in to ", h("strong", { text: state.session.server_url }), " as ", h("strong", { text: state.me.display_name }), " (" + label(state.me.role) + ") on ", h("span", { text: state.session.device })]),
    state.session.token_storage === "file"
      ? h("p.notice.warn.small", { text: "No keychain available: the sign-in token is stored in a file in the app's settings folder that only your user account can read, not in the system keychain. On Linux, start a Secret Service (GNOME Keyring or KWallet) and restart the app – the token then moves into the keychain." })
      : h("p.hint", { text: "The sign-in token is kept in the system keychain." }),
    serverInsecure,
    h("div.row", null, [
      h("button.danger", { type: "button", on: { click: function () { dlg.close(); signOut(); } } }, "Sign out"),
      // single-sign-on accounts have no password here (older servers send no auth_source)
      state.me.auth_source === "oidc" ? null : h("button", { type: "button", on: { click: function () { dlg.close(); passwordDialog(); } } }, "Change password…"),
      h("button", { type: "button", on: { click: function () { dlg.close(); signOut(true); } } }, "Change server")
    ])
  ]);
  var dlg = dialog("Settings", body, [
    { label: "Close", onClick: function (d) { d.close(); } },
    {
      label: "Save", primary: true, submit: true, onClick: function (d) {
        var workspaceChanged = ws.value.trim() !== String(state.session.workspace_dir);
        var saveWorkspace = workspaceChanged
          ? call("set_workspace_dir", { dir: ws.value.trim() }).then(function () {
            state.session.workspace_dir = ws.value.trim();
            $("status-ws").textContent = "Workspace: " + state.session.workspace_dir;
          })
          : Promise.resolve();
        saveWorkspace.then(function () { return notify.save(); }).then(function (saved) {
          d.close();
          if (workspaceChanged || saved) toast("Settings saved");
        }, function (err) { d.fail(err); });
      }
    }
  ], { wide: true });
  notify.load();
}

/**
 * The notification settings of the account (same as the web account page:
 * what to be told about, e-mail, Mattermost) in the settings dialog.
 * `save()` resolves to true when something was sent to the server.
 */
export function notificationSettings(box) {
  var model = null, boxes = [], email, mm, mmUser, dirty = false;
  function touch() { dirty = true; }
  function check(text, on, onChange) {
    var input = h("input", { type: "checkbox", on: { change: function () { touch(); if (onChange) onChange(); } } });
    input.checked = !!on;
    return { input: input, node: h("label.check", null, [input, " " + text]) };
  }
  function draw() {
    boxes = model.kinds.map(function (k) { var c = check(k.label, k.on); c.value = k.value; return c; });
    email = check("by e-mail" + (model.email_address ? " to " + model.email_address : ""), model.email);
    mm = check("to Mattermost" + (model.mattermost_direct ? " (direct message)" : " (mention in the TeamCraft channel)"), model.mattermost);
    mmUser = h("input", { type: "text", value: model.mattermost_username || "", maxlength: "64", spellcheck: "false", placeholder: state.me.username, on: { input: touch } });
    var notes = [];
    if (!model.email_enabled) notes.push(h("p.hint", { text: "E-mail is not configured on this server." }));
    else if (!model.email_address) notes.push(h("p.hint", { text: "Your account has no e-mail address." }));
    if (!model.mattermost_enabled) notes.push(h("p.hint", { text: "Mattermost is not configured on this server." }));
    box.replaceChildren(
      h("strong", { text: "Tell me about" }),
      h("div.stack", null, boxes.map(function (c) { return c.node; })),
      h("strong", { text: "Also send it" }),
      email.node, mm.node,
      field("Mattermost username", mmUser, "empty = your login"),
      h("div.stack", null, notes),
      h("div.row", null, [h("button", { type: "button", title: "Creates a notification for you and sends it through every channel you use", on: { click: function () {
        call("api", { method: "POST", path: "/notifications/test", body: null }).then(function () { toast("Test notification sent"); }, function (e) { toast(errorText(e), "error"); });
      } } }, "Send a test")])
    );
  }
  return {
    load: function () {
      box.replaceChildren(h("p.hint", { text: "Loading …" }));
      api("GET", "/notifications/settings").then(function (m) { model = m; draw(); }, function (e) {
        box.replaceChildren(h("p.hint", { text: "Not available: " + errorText(e) }));
      });
    },
    save: function () {
      if (!model || !dirty) return Promise.resolve(false);
      var muted = boxes.filter(function (c) { return !c.input.checked; }).map(function (c) { return c.value; });
      var user = mmUser.value.trim();
      return api("PUT", "/notifications/settings", {
        email: email.input.checked, mattermost: mm.input.checked,
        mattermost_username: user === "" ? null : user, muted: muted
      }).then(function () { dirty = false; return true; });
    }
  };
}

/** Changes the password of the account (`POST /me/password`; accounts of a single sign-on have none). */
export function passwordDialog() {
  var current = h("input", { type: "password", autocomplete: "current-password", required: "required" });
  var next = h("input", { type: "password", autocomplete: "new-password", minlength: "10", required: "required" });
  var again = h("input", { type: "password", autocomplete: "new-password", minlength: "10", required: "required" });
  dialog("Change password", h("div.stack", null, [
    field("Current password", current),
    field("New password", next, "at least 10 characters"),
    field("Repeat the new password", again)
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    { label: "Change password", primary: true, submit: true, onClick: function (d) {
      if (next.value !== again.value) { d.fail("The new passwords do not match."); return; }
      api("POST", "/me/password", { current_password: current.value, new_password: next.value }).then(function () {
        d.close();
        toast("Password changed");
      }, function (err) { d.fail(err); });
    } }
  ]);
}

export function signOut(changeServer) {
  call("sign_out").then(function () {
    state.session.signed_in = false;
    state.tabs = [];
    state.active = null;
    state.me = null;
    resetCompanyAi();
    endAgentPermissions();
    agent.running = null; // signing out stops the agent (src/commands.rs)
    agent.busy = false;
    agent.contextKey = null;
    agent.backend = null; // the next sign-in asks the server again (agent_policy)
    agent.available = [];
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
