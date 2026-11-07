// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – The main window: rail, navigator, status bar, update notice, notifications, long operations.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ICONS, api, badge, call, clear, dialog, enc, errorText, fmtDate, fmtSize, h, invoke, label, run, state, svgIcon, toast, tool } from "./core.js";
import { checkServerVersion, showConnect } from "./connect.js";
import { navHead, selectView } from "./navigator.js";
import { activeTab, closeTab, renderActive, renderTabs } from "./tabs.js";
import { initAssistant } from "./assistant.js";
import { agent, agentProfiles, applyAgentPolicy, localAgentsAllowed, openAgent, schedulePaint, updateAgentControls } from "./agent.js";
import { openRevision } from "./revision.js";
import { impactGroups } from "./references.js";
import { SYNC_LABEL, checkWorkspaceLayout } from "./workspace.js";
import { openChange } from "./change.js";
import { openRequest } from "./requests.js";
import { openSettings } from "./settings.js";

// ======================================================================
// Shell
// ======================================================================

export function canEdit() { return state.me && (state.me.role === "editor" || state.me.role === "admin"); }

export function canEditProject(projectId) {
  var me = state.me;
  if (!me) return false;
  if (me.role === "admin") return true;
  if (me.role !== "editor") return false;
  return !projectId || me.projects.indexOf(projectId) !== -1;
}

export function projectById(id) {
  return state.projects.filter(function (p) { return p.id === id; })[0];
}

/** Fetches the project groups and my account (role, memberships) again, so
 * a group created or joined in the web UI shows without restarting the app. */
export function reloadProjects() {
  return Promise.all([api("GET", "/me"), api("GET", "/projects")]).then(function (r) {
    state.me = r[0];
    state.projects = r[1] || [];
    $("user-name").textContent = state.me.display_name;
    var role = $("user-role");
    role.className = "badge badge-" + state.me.role;
    role.textContent = label(state.me.role);
  });
}

/** For the dialogs that offer the project groups: fresh lists, the cached
 * ones when the server cannot be reached (the dialog's request then fails). */
export function freshProjects() {
  return reloadProjects().catch(function () {});
}

export var brandingImages = {};
/** Blob URL of a replaced logo (cached per version token). The type is needed: an untyped SVG blob is not shown. */
export function brandingImage(kind, token, type) {
  var key = kind + ":" + token;
  if (!brandingImages[key]) {
    brandingImages[key] = invoke("file_bytes", { kind: "branding-" + kind, id: "" })
      .then(function (bytes) {
        return URL.createObjectURL(new Blob([bytes], { type: type || "application/octet-stream" }));
      }, function (err) {
        delete brandingImages[key]; // retry on the next sign-in, not only after a restart
        throw err;
      });
  }
  return brandingImages[key];
}

/** Shows the logos the administrators set: wide in the title bar, square in the rail; built-in ones otherwise. */
export function applyBranding() {
  var brand = document.querySelector(".titlebar .brand"), mark = document.querySelector(".rail-mark");
  if (!mark.dataset.builtin) mark.dataset.builtin = mark.getAttribute("src");
  function builtinWide() {
    brand.classList.remove("has-wide");
    var old = brand.querySelector(".brand-wide");
    if (old) old.remove();
  }
  api("GET", "/branding").then(function (b) {
    if (b.wide) {
      brandingImage("wide", b.wide, b.wide_type).then(function (url) {
        var img = brand.querySelector(".brand-wide");
        if (!img) { img = h("img.brand-wide", { alt: "Logo" }); brand.appendChild(img); }
        img.src = url;
        brand.classList.add("has-wide");
      }, builtinWide);
    } else { builtinWide(); }
    if (b.square) {
      brandingImage("square", b.square, b.square_type).then(function (url) { mark.src = url; }, function () { mark.src = mark.dataset.builtin; });
    } else { mark.src = mark.dataset.builtin; }
  }).catch(function () { /* an older server has no branding: built-in logos */ builtinWide(); mark.src = mark.dataset.builtin; });
}

export function enterShell() {
  return reloadProjects().then(function () {
    $("connect").classList.add("hidden");
    $("shell").classList.remove("hidden");
    $("status-server").textContent = state.session.server_url;
    $("status-ws").textContent = "Workspace: " + state.session.workspace_dir;
    checkServerVersion();
    applyBranding();
    initAssistant();
    selectView(state.view);
    renderTabs();
    renderActive();
    refreshWorklistCount();
    pollNotifications(true);
    // the administrator may have turned local agents off (TEAMCRAFT_LOCAL_AGENTS);
    // otherwise: an agent that kept running while the window reloaded
    applyAgentPolicy().then(function () {
      if (!localAgentsAllowed()) return null;
      return agentProfiles().then(function () { return call("agent_status"); });
    }).then(function (s) {
      if (s && !agent.running) { agent.running = s; agent.busy = s.busy; updateAgentControls(); schedulePaint(true); }
    }, function () {});
    checkWorkspaceLayout();
    checkTokenStorage();
    checkUpdate();
  }).catch(function (err) {
    if (!(err && err.status === 401)) showConnect("signin", errorText(err));
  });
}

/** Where the sign-in landed: without an OS keychain the token is in a file
 * (src/settings.rs) – say so once per start; Settings shows it too. */
export function checkTokenStorage() {
  invoke("session").then(function (s) {
    state.session.token_storage = s.token_storage;
    if (s.token_storage === "file" && !state.tokenFileWarned) {
      state.tokenFileWarned = true;
      toast("No keychain available: your sign-in is stored in a file only you can read. Details in Settings.", "error");
    }
  }, function () {});
}

// ======================================================================
// Update notice: a newer release on GitHub (src/update.rs) – a link to the
// release page in the status bar, never a download or an installation
// ======================================================================

export function checkUpdate() {
  if (!state.session.update_check) { showUpdate(null); return; }
  invoke("check_update").then(showUpdate, function () {});
}

export function showUpdate(notice) {
  state.update = notice || null;
  $("status-update").classList.toggle("hidden", !notice);
  if (notice) $("status-update-open").textContent = "TeamCraft " + notice.version + " is available (you have " + notice.current + ")";
}

export function initUpdateNotice() {
  $("status-update-open").addEventListener("click", function () {
    call("open_release_page").catch(function (e) { toast(errorText(e), "error"); });
  });
  $("status-update-dismiss").addEventListener("click", function () {
    var notice = state.update;
    showUpdate(null);
    if (notice) call("dismiss_update", { version: notice.version }).catch(function (e) { toast(errorText(e), "error"); });
  });
  // the app may run for days: look again now and then (Rust asks GitHub
  // at most once a day)
  setInterval(function () { if (state.me) checkUpdate(); }, 6 * 3600 * 1000);
}

export function initShell() {
  initUpdateNotice();
  if (/Mac/.test(navigator.platform)) $("search-kbd").textContent = "⌘F"; // the View → Search shortcut
  document.querySelectorAll(".rail button[data-view]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var view = btn.getAttribute("data-view");
      if (view === "agent") openAgent();
      else selectView(view);
    });
  });
  $("search-form").addEventListener("submit", function (e) {
    e.preventDefault();
    state.query = $("search-input").value.trim();
    selectView("search");
  });
  $("open-settings").addEventListener("click", openSettings);
  $("open-notifications").addEventListener("click", function () { selectView("notifications"); });
  // New notifications: bell count and a toast, every minute while signed in.
  setInterval(function () { if (state.me) pollNotifications(false); }, 60000);
  initSplitter();
  // back from the CAD program: is the next step of my check-out still right?
  window.addEventListener("focus", function () {
    var t = activeTab();
    if (t && t.data && t.recheckLocal) t.recheckLocal();
  });
  document.addEventListener("keydown", function (e) {
    var mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "w" && state.active) {
      e.preventDefault();
      closeTab(state.active);
    }
  });
}

export function initSplitter() {
  var splitter = $("splitter");
  var startX = 0, startW = 0;
  function move(e) {
    var w = Math.max(220, Math.min(640, startW + e.clientX - startX));
    document.documentElement.style.setProperty("--nav-width", w + "px");
  }
  function up() {
    splitter.classList.remove("dragging");
    document.removeEventListener("pointermove", move);
    document.removeEventListener("pointerup", up);
  }
  splitter.addEventListener("pointerdown", function (e) {
    startX = e.clientX;
    startW = $("navigator").getBoundingClientRect().width;
    splitter.classList.add("dragging");
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", up);
  });
}

// ---------------------------------------------------------------- notifications

/** Updates the bell; on later polls new unread notifications show as a toast. */
export function pollNotifications(first) {
  return api("GET", "/notifications?unread=true&limit=20").then(function (r) {
    var el = $("bell-count");
    el.textContent = r.unread > 99 ? "99+" : String(r.unread);
    el.classList.toggle("hidden", !r.unread);
    var bell = $("open-notifications");
    bell.classList.toggle("has-unread", r.unread > 0);
    // aria-label replaces the content, so it carries the count itself
    var bellText = r.unread ? "Notifications, " + r.unread + " unread" : "Notifications";
    bell.setAttribute("aria-label", bellText);
    bell.setAttribute("title", bellText);
    var seen = state.seenNotifications || {};
    var fresh = r.notifications.filter(function (n) { return !seen[n.id]; });
    r.notifications.forEach(function (n) { seen[n.id] = true; });
    state.seenNotifications = seen;
    if (!first && fresh.length) {
      toast(fresh.length === 1 ? fresh[0].title : fresh.length + " new notifications – " + fresh[0].title, "info");
      if (state.view === "notifications") selectView("notifications");
    }
    return r;
  }).catch(function () { return null; });
}

/** Opens what a notification is about (and marks it as read). */
export function openNotification(n) {
  var read = n.read_at ? Promise.resolve() : api("POST", "/notifications/read", { ids: [n.id] });
  read.then(function () { pollNotifications(true); }, function () {});
  if (n.subject_type === "change" && n.subject_ref) {
    openChange(n.subject_ref);
  } else if (n.subject_type === "change_request" && n.subject_ref) {
    openRequest(n.subject_ref);
  } else if (n.subject_type === "revision" && n.subject_ref.indexOf("/") > 0) {
    var parts = n.subject_ref.split("/");
    openRevision(parts[0], parts[1], n.kind === "impact" ? "references" : undefined);
  } else if (n.link) {
    call("open_web", { path: n.link }).catch(function (e) { toast(errorText(e), "error"); });
  }
  if (state.view === "notifications") setTimeout(function () { selectView("notifications"); }, 300);
}

export function renderNotifications() {
  return api("GET", "/notifications?limit=100").then(function (r) {
    var nav = $("navigator");
    clear(nav);
    nav.appendChild(navHead("Notifications", [
      h("button.small", { type: "button", disabled: !r.unread, title: "Mark all as read", on: {
        click: function () { api("POST", "/notifications/read").then(function () { pollNotifications(true); selectView("notifications"); }, function (e) { toast(errorText(e), "error"); }); }
      } }, "All read"),
      h("button.small", { type: "button", title: "Notification settings in the web UI", on: {
        click: function () { call("open_web", { path: "/account#notifications" }).catch(function (e) { toast(errorText(e), "error"); }); }
      } }, svgIcon(ICONS.web))
    ]));
    if (!r.notifications.length) {
      nav.appendChild(h("p.empty", { text: "No notifications yet: review requests, decisions, releases, checks needed on your items and news about what you watch appear here." }));
      return;
    }
    nav.appendChild(h("ul.notifications", null, r.notifications.map(function (n) {
      return h("li" + (n.read_at ? "" : ".unread"), null, h("button", {
        type: "button", title: n.body || n.title, on: { click: function () { openNotification(n); } }
      }, [
        h("span.n-title", { text: n.title }),
        n.body ? h("span.n-body", { text: n.body }) : null,
        h("span.n-meta", { text: n.actor_name + " · " + fmtDate(n.created_at) })
      ]));
    })));
  });
}

/** "Pack and Go": ZIP of a revision and its structure, saved where the user
 * chooses. After the save dialog a progress dialog follows the download; its
 * Cancel stops it (also while the server is still packing) – the partial ZIP
 * is removed, nothing is written. */
export function savePackage(rev, rule) {
  if (packing) { toast("Pack and Go of " + packing.label + " is still running.", "info"); return; }
  packing = { label: rev.item_number + "/" + rev.code, dialog: null };
  function done() { if (packing && packing.dialog) packing.dialog.close(); packing = null; }
  call("save_package", { item: rev.item_number, code: rev.code, rule: rule }).then(function (path) {
    done();
    if (path) toast("Saved " + path);
  }, function (err) {
    done();
    toast(errorText(err), isCancelled(err) ? "info" : "error");
  });
}

// ------------------------------------------------ stopping long operations

/** An operation the user cancelled (status 499 from the Rust side) – a note, not an error. */
export function isCancelled(err) { return !!err && err.status === 499; }

/** The running Pack and Go: its label and progress dialog (opened by the
 * first `package-progress` event, i.e. after the save dialog). */
export var packing = null;

/** A progress dialog with a bar and Cancel, which asks the Rust side to stop
 * (`cancelCommand`); Escape and the backdrop do not close it. The caller
 * closes it when the operation has ended. */
export function progressDialog(title, cancelCommand) {
  var bar = h("progress", { max: "1", value: "0" });
  var text = h("p.small", { role: "status", text: "Starting …" });
  var stopping = false;
  var d = dialog(title, h("div.stack", null, [text, bar]), [
    { label: "Cancel", onClick: function () {
      if (stopping) return;
      stopping = true;
      text.textContent = "Stopping …";
      call(cancelCommand).catch(function () {});
    } }
  ], { modal: true });
  return {
    close: d.close,
    update: function (done, total, line) {
      if (stopping) return;
      if (total) { bar.max = total; bar.value = Math.min(done, total); } else { bar.removeAttribute("value"); }
      text.textContent = line;
    }
  };
}

export function onPackageProgress(p) {
  if (!packing) return;
  if (!packing.dialog) packing.dialog = progressDialog("Pack and Go " + packing.label, "cancel_package");
  var received = p.received || 0;
  packing.dialog.update(received, p.total || 0, received
    ? "Received " + fmtSize(received) + (p.total ? " of " + fmtSize(p.total) : "") + " …"
    : "The server is packing the files of the structure …");
}

/** The progress line of a check-in: bytes sent, then waiting for the server. */
export function uploadLine(p) {
  if (p.total && p.sent >= p.total) return "Sent " + p.file + " – the server is storing it (too late to cancel) …";
  var pct = p.total ? Math.floor(100 * p.sent / p.total) : 0;
  return "Uploading " + p.file + " – " + fmtSize(p.sent) + " of " + fmtSize(p.total) + " (" + pct + " %) …";
}

/** Watch / Unwatch button for the toolbar of a revision or change tab. */
export function watchTool(tab, kind, number, watching) {
  return tool(watching ? "Unwatch" : "Watch", "eye", function () {
    run(watching ? "Unwatching" : "Watching", function () {
      return api(watching ? "DELETE" : "POST", "/" + kind + "/" + enc(number) + "/watch");
    }).then(function (r) {
      toast(r.watching ? "You watch " + number + " now – changes show up under the bell" : "You no longer watch " + number);
      tab.data = null;
      renderActive();
    }, function () {});
  }, { title: watching ? "Stop the notifications about " + number : "Get notified about " + number });
}

/** "Check in all my check-outs": every revision without local changes in
 * the workspace is checked in on the server (lock released, one by one);
 * revisions with modified, new or conflicting files are left checked out
 * and listed – check them in one at a time with their files. */
export function checkInAll() {
  var checkouts = (state.worklist && state.worklist.checkouts) || [];
  var clean = [], skipped = [], unreadable = [];
  run("Comparing " + checkouts.length + " check-outs with the workspace", function () {
    return checkouts.reduce(function (chain, r) {
      return chain.then(function () {
        return call("workspace_status", { project: r.project_code, item: r.item_number, code: r.code }).then(function (ws) {
          var local = ws.files.filter(function (f) { return f.state === "modified" || f.state === "new" || f.state === "conflict"; });
          if (local.length) skipped.push({ rev: r, files: local });
          else clean.push(r);
        }, function (err) { unreadable.push(r.item_number + "/" + r.code + ": " + errorText(err)); });
      });
    }, Promise.resolve()).then(function () {
      if (!clean.length) return { revisions: [], checked_in: 0 };
      return api("POST", "/worklist/checkin-all", { revisions: clean.map(function (r) { return r.id; }) }).then(function (result) {
        // the released files become read-only in the working folder again
        return clean.reduce(function (chain, r) {
          return chain.then(function () {
            return call("workspace_status", { project: r.project_code, item: r.item_number, code: r.code }).catch(function () {});
          });
        }, Promise.resolve()).then(function () { return result; });
      });
    });
  }).then(function (result) {
    var failed = (result.revisions || []).filter(function (o) { return !o.ok; });
    var body = h("div.stack", null, [
      h("p", { text: result.checked_in + " of " + checkouts.length + " check-outs checked in – their locks are released." }),
      skipped.length ? h("div.stack", null, [
        h("p", { text: skipped.length + " kept checked out because of local changes – check them in one by one with their files:" }),
        h("ul.small", null, skipped.map(function (s) {
          return h("li", null, [h("a", { href: "#", on: { click: function (e) { e.preventDefault(); openRevision(s.rev.item_number, s.rev.code, "workspace"); } } },
            s.rev.item_number + "/" + s.rev.code), " – " + s.files.map(function (f) { return f.name + " (" + (SYNC_LABEL[f.state] || f.state) + ")"; }).join(", ")]);
        }))
      ]) : null,
      failed.length || unreadable.length ? h("div.notice.error", null, [h("strong", { text: "Problems:" }),
        h("ul.small", null, failed.map(function (o) { return h("li", { text: o.item_number + "/" + o.code + ": " + o.error }); })
          .concat(unreadable.map(function (t) { return h("li", { text: t }); })))]) : null
    ]);
    dialog("Check in all my check-outs", body, [{ label: "Close", primary: true, submit: true, onClick: function (d) { d.close(); } }]);
    refreshWorklistCount().then(function () { if (state.view === "worklist") selectView("worklist"); renderActive(); }, function () {});
  }, function () {});
}

export function refreshWorklistCount() {
  return api("GET", "/worklist").then(function (w) {
    state.worklist = w;
    // dependents count once, however many changes they have to be checked against
    var n = w.checkouts.length + w.reviews.length + impactGroups(w).length + (w.requests_assigned || []).length;
    var el = $("worklist-count");
    el.textContent = String(n);
    el.classList.toggle("hidden", n === 0);
    var rail = el.closest("button");
    if (rail) {
      rail.setAttribute("aria-label", n ? "Worklist, " + n + " open" : "Worklist");
    }
    return w;
  });
}
