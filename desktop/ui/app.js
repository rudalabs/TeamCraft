// TeamCraft Desktop – user interface (plain JavaScript, no build step).
//
// Layout in the style of classic PLM rich clients: a global navigation rail
// (Home, Worklist, Changes, Search), a navigator tree, object tabs with
// sub-tabs (Overview, Files, Structure, Where used, Changes, History,
// 3D viewer, Workspace), a context toolbar and a status bar.
//
// Every server call goes through the Rust commands (src/commands.rs); data
// from the server is only ever inserted as text, never as HTML.
(function () {
  "use strict";

  var TAURI = window.__TAURI__;
  var invoke = TAURI.core.invoke;

  // Keep in sync with SKINS in src/domain.rs (a server unit test checks it).
  var SKINS = [
    { id: "teamcraft", name: "TeamCraft", logo: "logos/TeamCraft_Icon.svg", description: "House style, night" },
    { id: "teamcraft-day", name: "TeamCraft Day", logo: "logos/TeamCraft_Icon.svg", description: "House style, light" },
    { id: "arch", name: "Arch", logo: "logos/TeamCraft_Icon.svg", description: "Blueprint grid" },
    { id: "cachyos", name: "CachyOS", logo: "logos/TeamCraft_Icon.svg", description: "Teal dot matrix" },
    { id: "endeavouros", name: "EndeavourOS", logo: "logos/TeamCraft_Icon.svg", description: "Purple sunset" },
    { id: "kde", name: "KDE", logo: "logos/TeamCraft_Icon.svg", description: "Breeze dark" },
    { id: "kubuntu", name: "Kubuntu", logo: "logos/TeamCraft_Icon.svg", description: "Blue rings" },
    { id: "opensuse", name: "openSUSE", logo: "logos/TeamCraft_Icon.svg", description: "Chameleon scales" },
    { id: "ubuntu", name: "Ubuntu", logo: "logos/TeamCraft_Icon.svg", description: "Circle of friends" }
  ];

  var ITEM_TYPES = [
    { value: "part", label: "Part" },
    { value: "assembly", label: "Assembly" },
    { value: "document", label: "Document" },
    { value: "software", label: "Software" }
  ];

  var state = {
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
    zoom: 1
  };

  // ======================================================================
  // Helpers
  // ======================================================================

  function $(id) { return document.getElementById(id); }

  function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); }

  /** h("tag.class", {attrs}, children) – text children become text nodes. */
  function h(spec, attrs, children) {
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

  function append(el, children) {
    if (children === null || children === undefined || children === false) return;
    if (!Array.isArray(children)) children = [children];
    children.forEach(function (c) {
      if (c === null || c === undefined || c === false) return;
      if (Array.isArray(c)) append(el, c);
      else el.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
    });
  }

  function svgIcon(path) {
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

  var ICONS = {
    checkout: "M7 11V7a5 5 0 0 1 10 0v4|M5 11h14v10H5z",
    checkin: "M12 3v12|M7 10l5 5 5-5|M5 21h14",
    cancel: "M6 6l12 12|M18 6L6 18",
    revise: "M4 12a8 8 0 0 1 14-5.3L20 9|M20 4v5h-5|M20 12a8 8 0 0 1-14 5.3L4 15|M4 20v-5h5",
    folder: "M3 7h6l2 2h10v10H3z",
    download: "M12 4v11|M7 10l5 5 5-5|M5 20h14",
    web: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18|M3 12h18|M12 3c3 3.5 3 14.5 0 18|M12 3c-3 3.5-3 14.5 0 18",
    refresh: "M20 11a8 8 0 1 0-2.3 5.7|M20 4v7h-7",
    plus: "M12 5v14|M5 12h14",
    edit: "M4 20h4L19 9l-4-4L4 16z",
    obsolete: "M5 5l14 14|M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18",
    release: "M5 21V4|M5 4h12l-2.5 4 2.5 4H5",
    eye: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z|M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6",
    request: "M4 4h16v12H9l-5 4z|M12 7v4|M12 13.5v.5"
  };

  // Keep in sync with RequestReason in src/domain.rs.
  var REQUEST_REASONS = [
    { value: "defect", label: "Defect / error" },
    { value: "improvement", label: "Improvement" },
    { value: "cost", label: "Cost reduction" },
    { value: "manufacturing", label: "Manufacturing / assembly" },
    { value: "supplier", label: "Supplier / purchasing" },
    { value: "safety", label: "Safety / compliance" },
    { value: "customer", label: "Customer request" },
    { value: "other", label: "Other" }
  ];
  var OPEN_REQUEST_STATES = ["new", "in_triage", "accepted", "deferred"];

  function tool(label, icon, onClick, opts) {
    opts = opts || {};
    return h("button.tool" + (opts.primary ? ".primary" : "") + (opts.danger ? ".danger" : ""), {
      type: "button", title: opts.title || label, disabled: opts.disabled, on: { click: onClick }
    }, [svgIcon(ICONS[icon] || ICONS.plus), label]);
  }

  function fmtDate(iso) {
    if (!iso) return "—";
    var d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
  }

  function fmtSize(bytes) {
    if (bytes === null || bytes === undefined) return "—";
    var units = ["B", "KiB", "MiB", "GiB"];
    var v = bytes, i = 0;
    while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return (i === 0 ? v : v.toFixed(1)) + " " + units[i];
  }

  function fmtNum(v, digits) {
    if (v === null || v === undefined) return "—";
    return Number(v).toLocaleString(undefined, { maximumFractionDigits: digits === undefined ? 3 : digits });
  }

  var LABELS = { in_work: "WIP", in_review: "In Review", editor: "CAD editor" };
  function label(code) {
    if (!code) return "";
    if (LABELS[code]) return LABELS[code];
    return String(code).split("_").map(function (w) { return w.charAt(0).toUpperCase() + w.slice(1); }).join(" ");
  }

  function badge(value, text) {
    var icon = null;
    if (value === "released" || value === "approved" || value === "unchanged") icon = "Green_tick.svg";
    else if (value === "in_review" || value === "pending" || value === "modified") icon = "Yellow_circle.svg";
    else if (value === "rejected" || value === "obsolete" || value === "cancelled") icon = "Red_cross.svg";
    return h("span.badge.badge-" + value, null, [
      icon ? h("img", { src: "/static/media/icons/" + icon, alt: "" }) : null,
      text || label(value)
    ]);
  }

  function errorText(err) {
    if (!err) return "Unknown error";
    if (typeof err === "string") return err;
    return err.message || JSON.stringify(err);
  }

  function setStatus(text) { $("status-msg").textContent = text || ""; }

  function toast(text, kind) {
    var el = h("div.toast" + (kind ? "." + kind : ""), { role: kind === "error" ? "alert" : "status", text: text });
    $("toasts").appendChild(el);
    setTimeout(function () { el.remove(); }, kind === "error" ? 9000 : 4500);
  }

  /** Calls a Rust command; a 401 sends the user back to the sign-in. */
  function call(cmd, args) {
    return invoke(cmd, args || {}).catch(function (err) {
      if (err && err.status === 401 && cmd !== "sign_in_token") {
        state.session.signed_in = false;
        showConnect("signin", "Your sign-in has expired or was revoked – please sign in again.");
      }
      throw err;
    });
  }

  function api(method, path, body) {
    return call("api", { method: method, path: path, body: body === undefined ? null : body });
  }

  function enc(s) { return encodeURIComponent(s); }

  /** Runs an action with status text and error toast; returns its promise. */
  function run(statusText, fn) {
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

  function dialog(title, body, actions, opts) {
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

  function confirmDialog(title, text, okLabel, danger) {
    return new Promise(function (resolve) {
      dialog(title, h("p", { text: text }), [
        { label: "Cancel", onClick: function (d) { d.close(); resolve(false); } },
        { label: okLabel || "OK", primary: !danger, danger: danger, submit: true, onClick: function (d) { d.close(); resolve(true); } }
      ]);
    });
  }

  function field(labelText, input, hint) {
    return h("label", null, [labelText, hint ? h("span.hint", { text: " " + hint }) : null, input]);
  }

  // ======================================================================
  // Skins
  // ======================================================================

  function applySkin(id) {
    var root = document.documentElement;
    root.setAttribute("data-skin", id || "teamcraft");
    var light = getComputedStyle(root).colorScheme.indexOf("light") !== -1;
    root.classList.toggle("skin-light", light);
    if (state.viewer) renderActive(); // the 3D view takes its colours from the skin
  }

  // ======================================================================
  // Connect / sign in
  // ======================================================================

  function showConnect(step, error) {
    $("shell").classList.add("hidden");
    $("connect").classList.remove("hidden");
    $("server-form").classList.toggle("hidden", step !== "server");
    $("signin-step").classList.toggle("hidden", step !== "signin");
    $("signin-wait").classList.add("hidden");
    $("signin-browser").disabled = false;
    if (!$("server-url").value) $("server-url").value = state.session.server_url || "";
    $("signin-server").textContent = state.session.server_url || "";
    var err = $("connect-error");
    err.textContent = error || "";
    err.classList.toggle("hidden", !error);
    if (step === "server") $("server-url").focus();
  }

  function initConnect() {
    $("server-form").addEventListener("submit", function (e) {
      e.preventDefault();
      var url = $("server-url").value;
      $("connect-error").classList.add("hidden");
      call("set_server", { url: url }).then(function (base) {
        state.session.server_url = base;
        state.session.signed_in = false;
        showConnect("signin");
      }, function (err) { showConnect("server", errorText(err)); });
    });
    $("change-server").addEventListener("click", function () { showConnect("server"); });
    // Each attempt has a number; "cancel" resets the screen at once and makes
    // the pending attempt's late result irrelevant.
    var attempt = 0;
    $("signin-browser").addEventListener("click", function () {
      var mine = ++attempt;
      $("signin-browser").disabled = true;
      $("signin-wait").classList.remove("hidden");
      $("connect-error").classList.add("hidden");
      call("sign_in_browser").then(function () {
        if (mine !== attempt) return;
        state.session.signed_in = true;
        enterShell();
      }, function (err) {
        if (mine === attempt) showConnect("signin", errorText(err));
      });
    });
    $("signin-cancel").addEventListener("click", function () {
      attempt++;
      showConnect("signin");
      call("cancel_sign_in").catch(function () {});
    });
    $("token-form").addEventListener("submit", function (e) {
      e.preventDefault();
      call("sign_in_token", { token: $("token-input").value }).then(function () {
        $("token-input").value = "";
        state.session.signed_in = true;
        enterShell();
      }, function (err) { showConnect("signin", errorText(err)); });
    });
  }

  // ======================================================================
  // Shell
  // ======================================================================

  function canEdit() { return state.me && (state.me.role === "editor" || state.me.role === "admin"); }

  function canEditProject(projectId) {
    var me = state.me;
    if (!me) return false;
    if (me.role === "admin") return true;
    if (me.role !== "editor") return false;
    return !projectId || me.projects.indexOf(projectId) !== -1;
  }

  function projectById(id) {
    return state.projects.filter(function (p) { return p.id === id; })[0];
  }

  function enterShell() {
    return Promise.all([api("GET", "/me"), api("GET", "/projects")]).then(function (r) {
      state.me = r[0];
      state.projects = r[1] || [];
      $("connect").classList.add("hidden");
      $("shell").classList.remove("hidden");
      $("user-name").textContent = state.me.display_name;
      var role = $("user-role");
      role.className = "badge badge-" + state.me.role;
      role.textContent = label(state.me.role);
      $("status-server").textContent = state.session.server_url;
      $("status-ws").textContent = "Workspace: " + state.session.workspace_dir;
      selectView(state.view);
      renderTabs();
      renderActive();
      refreshWorklistCount();
      pollNotifications(true);
    }).catch(function (err) {
      if (!(err && err.status === 401)) showConnect("signin", errorText(err));
    });
  }

  function initShell() {
    document.querySelectorAll(".rail button[data-view]").forEach(function (btn) {
      btn.addEventListener("click", function () { selectView(btn.getAttribute("data-view")); });
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
    document.addEventListener("keydown", function (e) {
      var mod = e.ctrlKey || e.metaKey;
      if (mod && e.key.toLowerCase() === "w" && state.active) {
        e.preventDefault();
        closeTab(state.active);
      }
    });
  }

  function initSplitter() {
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
  function pollNotifications(first) {
    return api("GET", "/notifications?unread=true&limit=20").then(function (r) {
      var el = $("bell-count");
      el.textContent = r.unread > 99 ? "99+" : String(r.unread);
      el.classList.toggle("hidden", !r.unread);
      $("open-notifications").classList.toggle("has-unread", r.unread > 0);
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
  function openNotification(n) {
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

  function renderNotifications() {
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

  /** "Pack and Go": ZIP of a revision and its structure, saved where the user chooses. */
  function savePackage(rev, rule) {
    run("Packing " + rev.item_number + "/" + rev.code, function () {
      return call("save_package", { item: rev.item_number, code: rev.code, rule: rule });
    }).then(function (path) { if (path) toast("Saved " + path); }, function () {});
  }

  /** Watch / Unwatch button for the toolbar of a revision or change tab. */
  function watchTool(tab, kind, number, watching) {
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

  function refreshWorklistCount() {
    return api("GET", "/worklist").then(function (w) {
      state.worklist = w;
      var n = w.checkouts.length + w.reviews.length + (w.impacts || []).length + (w.requests_assigned || []).length;
      var el = $("worklist-count");
      el.textContent = String(n);
      el.classList.toggle("hidden", n === 0);
      return w;
    });
  }

  // ======================================================================
  // Navigator views
  // ======================================================================

  function selectView(view) {
    state.view = view;
    document.querySelectorAll(".rail button[data-view]").forEach(function (b) {
      b.classList.toggle("active", b.getAttribute("data-view") === view);
    });
    var nav = $("navigator");
    clear(nav);
    nav.appendChild(h("p.loading", { text: "Loading" }));
    var render = { home: renderHome, worklist: renderWorklist, changes: renderChanges, search: renderSearch, notifications: renderNotifications }[view] || renderHome;
    Promise.resolve().then(render).catch(function (err) {
      clear(nav);
      nav.appendChild(h("p.notice.error", { text: errorText(err) }));
    });
  }

  function navHead(title, actions) {
    return h("div.nav-head", null, [h("h2", { text: title }), h("div.row", null, actions || [])]);
  }

  function itemNode(it, opts) {
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

  function typeIcon(type) {
    return { part: "◆", assembly: "▣", document: "▤", software: "◇" }[type] || "•";
  }

  function renderHome() {
    var nav = $("navigator");
    var mine = state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) !== -1; });
    var others = state.projects.filter(function (p) { return p.active && state.me.projects.indexOf(p.id) === -1; });
    clear(nav);
    nav.appendChild(navHead("Project groups", [
      h("button.small", { type: "button", title: "Copy the files of whole project groups into your workspace", on: { click: function () { getProjectsDialog(); } } }, "Get…"),
      canEdit() ? h("button.small", { type: "button", title: "Import an existing CAD folder (Tools → Import CAD Folder…)", on: { click: function () { importFolderDialog(); } } }, "Import…") : null,
      canEdit() ? h("button.small", { type: "button", title: "New item (Ctrl+N)", on: { click: function () { newItemDialog(); } } }, [svgIcon(ICONS.plus), "New"]) : null
    ]));
    function group(title, list) {
      if (!list.length) return null;
      return [h("p.section-title", { text: title }), h("ul.tree", null, list.map(projectNode))];
    }
    append(nav, group("My project groups", mine));
    append(nav, group(mine.length ? "Other project groups" : "All project groups", others));
  }

  function projectNode(p) {
    var open = !!state.expanded[p.code];
    var children = h("ul.tree" + (open ? "" : ".hidden"));
    var node = h("div.node.group", {
      title: p.name,
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

  function loadProjectItems(p, ul, count) {
    clear(ul);
    ul.appendChild(h("li.loading", { text: "Loading" }));
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

  function renderWorklist() {
    return refreshWorklistCount().then(function (w) {
      var nav = $("navigator");
      clear(nav);
      nav.appendChild(navHead("My worklist", [
        h("button.small", { type: "button", title: "Refresh", on: { click: function () { selectView("worklist"); } } }, svgIcon(ICONS.refresh))
      ]));
      nav.appendChild(h("p.section-title", { text: "Checked out by me (" + w.checkouts.length + ")" }));
      nav.appendChild(w.checkouts.length ? h("ul.tree", null, w.checkouts.map(function (r) {
        return h("li", null, h("div.node", { on: { click: function () { openRevision(r.item_number, r.code); } } }, [
          h("span.twisty"), h("span.ico", { text: "🔒" }),
          h("span.label", null, [h("span.mono", { text: r.item_number + "/" + r.code }), " ", r.name]),
          h("span.meta", null, badge(r.state))
        ]));
      })) : h("p.empty", { text: "Nothing checked out." }));
      nav.appendChild(h("p.section-title", { text: "Waiting for my review (" + w.reviews.length + ")" }));
      nav.appendChild(w.reviews.length ? h("ul.tree", null, w.reviews.map(changeNode)) : h("p.empty", { text: "Nothing to review." }));
      var impacts = w.impacts || [];
      nav.appendChild(h("p.section-title", { text: "Dependents to check (" + impacts.length + ")" }));
      nav.appendChild(impacts.length ? h("ul.tree", null, impacts.map(function (i) {
        return h("li", null, h("div.node", {
          title: i.item_number + " " + relationText(i.relation, i.source_number) + " – " + i.source_number + "/" + i.source_code + " " + i.event,
          on: { click: function () { openRevision(i.item_number, i.revision_code, "references"); } }
        }, [
          h("span.twisty"), h("span.ico", { text: "⚠" }),
          h("span.label", null, [h("span.mono", { text: i.item_number + "/" + i.revision_code }), " ", i.item_name]),
          h("span.meta.mono", { text: i.source_number + "/" + i.source_code })
        ]));
      })) : h("p.empty", { text: "Nothing to check." }));
      nav.appendChild(h("p.section-title", { text: "My open changes (" + w.my_changes.length + ")" }));
      nav.appendChild(w.my_changes.length ? h("ul.tree", null, w.my_changes.map(changeNode)) : h("p.empty", { text: "No open changes." }));
      // change requests (older servers: none)
      var assigned = w.requests_assigned || [], mine = w.my_requests || [];
      nav.appendChild(h("p.section-title", { text: "Requests assigned to me (" + assigned.length + ")" }));
      nav.appendChild(assigned.length ? h("ul.tree", null, assigned.map(requestNode)) : h("p.empty", { text: "No change requests assigned to you." }));
      nav.appendChild(h("p.section-title", { text: "My open requests (" + mine.length + ")" }));
      nav.appendChild(mine.length ? h("ul.tree", null, mine.map(requestNode)) : h("p.empty", { text: "Found a problem? Use “Request change…” on a revision or a workspace file." }));
    });
  }

  function requestNode(r) {
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

  function changeNode(c) {
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

  var CHANGE_FILTERS = [
    { id: "open", label: "Open" }, { id: "draft", label: "Draft" }, { id: "in_review", label: "In Review" },
    { id: "rejected", label: "Rejected" }, { id: "released", label: "Released" }, { id: "all", label: "All" }
  ];

  function renderChanges() {
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

  function renderSearch() {
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
      nav.appendChild(navHead("Search"));
      nav.appendChild(h("p.small.muted", { text: r.total + " result" + (r.total === 1 ? "" : "s") + " for “" + q + "”" }));
      nav.appendChild(r.items.length ? h("ul.tree", null, r.items.map(function (it) { return itemNode(it); })) : h("p.empty", { text: "Nothing found." }));
    });
  }

  // ======================================================================
  // Tabs
  // ======================================================================

  function findTab(key) { return state.tabs.filter(function (t) { return t.key === key; })[0]; }

  function openTab(tab) {
    var existing = findTab(tab.key);
    if (!existing) state.tabs.push(tab);
    else if (tab.sub) existing.sub = tab.sub;
    activate(tab.key);
  }

  function activate(key) {
    state.active = key;
    renderTabs();
    renderActive();
    markSelection();
  }

  function markSelection() {
    // re-render cheap navigator selection markers
    document.querySelectorAll("#navigator .node.selected").forEach(function (n) { n.classList.remove("selected"); });
  }

  function closeTab(key) {
    var i = state.tabs.findIndex(function (t) { return t.key === key; });
    if (i === -1) return;
    state.tabs.splice(i, 1);
    if (state.active === key) {
      var next = state.tabs[i] || state.tabs[i - 1];
      state.active = next ? next.key : null;
    }
    disposeViewer();
    renderTabs();
    renderActive();
  }

  function renderTabs() {
    var bar = $("tabs");
    clear(bar);
    state.tabs.forEach(function (t) {
      bar.appendChild(h("div.tab" + (t.key === state.active ? ".active" : ""), {
        role: "tab", "aria-selected": t.key === state.active ? "true" : "false", title: t.title,
        on: { click: function () { if (state.active !== t.key) activate(t.key); } }
      }, [
        h("span.ico", { text: t.kind === "change" ? "⇄" : t.kind === "request" ? "✎" : "◆" }),
        h("span.tab-label", { text: t.title }),
        h("button.close", { type: "button", title: "Close (Ctrl+W)", "aria-label": "Close", on: { click: function (e) { e.stopPropagation(); closeTab(t.key); } } }, "×")
      ]));
    });
  }

  function activeTab() { return findTab(state.active); }

  function renderActive() {
    disposeViewer();
    var body = $("tab-body");
    var toolbar = $("toolbar");
    clear(body);
    clear(toolbar);
    var tab = activeTab();
    if (!tab) {
      renderWelcome(body, toolbar);
      return;
    }
    body.appendChild(h("p.loading", { text: "Loading " + tab.title }));
    var load = tab.kind === "change" ? loadChange(tab) : tab.kind === "request" ? loadRequest(tab) : loadRevision(tab);
    load.then(function () {
      if (state.active !== tab.key) return;
      clear(body);
      clear(toolbar);
      if (tab.kind === "change") renderChange(tab, body, toolbar);
      else if (tab.kind === "request") renderRequest(tab, body, toolbar);
      else renderRevision(tab, body, toolbar);
    }, function (err) {
      if (state.active !== tab.key) return;
      clear(body);
      body.appendChild(h("p.notice.error", { text: errorText(err) }));
    });
  }

  function reloadActive() {
    var tab = activeTab();
    if (tab) tab.data = null;
    renderActive();
  }

  function renderWelcome(body, toolbar) {
    append(toolbar, [
      canEdit() ? tool("New item", "plus", function () { newItemDialog(); }, { primary: true }) : null,
      tool("Refresh", "refresh", function () { selectView(state.view); refreshWorklistCount(); renderActive(); })
    ]);
    var w = state.worklist;
    var welcome = h("div.welcome", null, [
      h("h1", { text: "Hello, " + (state.me ? state.me.display_name : "") }),
      h("p.muted", { text: "Open a project group in the navigator, search above or pick something from your worklist." })
    ]);
    body.appendChild(welcome);
    var fill = function (wl) {
      if (!wl) return;
      var s = wl.stats;
      append(welcome, h("div.stats", null, [
        stat(s.items, "Items"), stat(s.revisions_in_work, "Revisions in work"), stat(s.revisions_in_review, "In review"),
        stat(s.revisions_released, "Released revisions"), stat(s.open_changes, "Open changes"), stat(fmtSize(s.stored_bytes), s.files + " files in the vault")
      ]));
      if ((wl.impacts || []).length) {
        append(welcome, h("section.panel", null, [h("h2", { text: "Dependents to check" }),
          h("p.hint", { text: "These items use or reference something that was released in a new revision or made obsolete: acknowledge (no change needed) or revise them." })
        ].concat(wl.impacts.map(function (i) { return impactBanner(i, false); }))));
      }
      if (wl.checkouts.length || wl.reviews.length) {
        append(welcome, h("div.grid-2", null, [
          h("section.panel", null, [h("h2", { text: "Checked out by me" }),
            wl.checkouts.length ? table(["Revision", "Name", "Since"], wl.checkouts.map(function (r) {
              return { cells: [h("span.mono", { text: r.item_number + "/" + r.code }), r.name, fmtDate(r.checked_out_at)], open: function () { openRevision(r.item_number, r.code); } };
            })) : h("p.empty", { text: "Nothing checked out." })]),
          h("section.panel", null, [h("h2", { text: "Waiting for my review" }),
            wl.reviews.length ? table(["Change", "Title", "Submitted"], wl.reviews.map(function (c) {
              return { cells: [h("span.mono", { text: c.number }), c.title, fmtDate(c.submitted_at)], open: function () { openChange(c.number); } };
            })) : h("p.empty", { text: "Nothing to review." })])
        ]));
      }
    };
    if (w) fill(w);
    else refreshWorklistCount().then(fill, function () {});
  }

  function stat(value, text) {
    return h("div.stat", null, [h("div.value", { text: String(value) }), h("div.label", { text: text })]);
  }

  /** rows: [{cells: [...], open: fn, cls: "selected"}] */
  function table(headers, rows, opts) {
    opts = opts || {};
    return h("div.table-wrap", null, h("table.grid", null, [
      h("thead", null, h("tr", null, headers.map(function (t, i) {
        return h("th" + (opts.numeric && opts.numeric.indexOf(i) !== -1 ? ".num" : ""), { text: t });
      }))),
      h("tbody", null, rows.map(function (r) {
        return h("tr" + (r.open ? ".clickable" : "") + (r.cls ? "." + r.cls : ""), {
          on: r.open ? { dblclick: r.open, click: r.single ? r.open : null } : null,
          title: r.open ? "Double-click to open" : null
        }, r.cells.map(function (c, i) {
          var td = h("td" + (opts.numeric && opts.numeric.indexOf(i) !== -1 ? ".num" : ""));
          append(td, c);
          return td;
        }));
      }))
    ]));
  }

  // ======================================================================
  // Revision tab
  // ======================================================================

  function openRevision(number, code, sub) {
    openTab({ key: "rev:" + number + ":" + code, kind: "revision", number: number, code: code, title: number + "/" + code, sub: sub || "overview" });
  }

  function loadRevision(tab) {
    if (tab.data) return Promise.resolve(tab.data);
    var base = "/items/" + enc(tab.number);
    return Promise.all([
      api("GET", base + "/revisions/" + enc(tab.code)),
      api("GET", base),
      // references, referrers, CAD file references and impacts (older servers: none)
      api("GET", base + "/revisions/" + enc(tab.code) + "/references").catch(function () { return null; })
    ]).then(function (r) {
      tab.data = {
        rev: r[0].revision, files: r[0].files, bom: r[0].bom, changes: r[0].changes, item: r[1].item, revisions: r[1].revisions,
        requests: r[0].open_requests || [],
        watching: !!r[1].watching,
        refs: r[2] || { outgoing: [], incoming: [], files: [], file_referrers: [], impacts: [] }
      };
      tab.title = tab.data.rev.item_number + "/" + tab.data.rev.code + " " + tab.data.rev.name;
      renderTabs();
      return tab.data;
    });
  }

  // 3D preview format of a file: the server classifies every file (src/cadformat.rs);
  // older servers without format_info fall back to the extension.
  function previewFormat(f) {
    if (f && f.format_info) return f.format_info.preview || null;
    var m = /\.([a-z0-9_]+)$/i.exec((f && f.filename) || "");
    if (!m) return null;
    var ext = m[1].toLowerCase();
    if (ext === "stl") return "stl";
    if (ext === "step" || ext === "stp" || ext === "p21") return "step";
    if (ext === "iges" || ext === "igs") return "iges";
    if (ext === "brep" || ext === "brp") return "brep";
    return null;
  }

  // Format chip: "SolidWorks assembly", "STEP (ISO 10303)", … or the content type.
  function formatChip(f) {
    var i = f.format_info;
    if (!i) return h("span.dim.small", { text: f.content_type });
    return h("span.fmt.fmt-" + i.kind, { text: i.label, title: i.system_name + " · " + i.kind_label + " · " + f.content_type });
  }

  function revisionAbilities(d) {
    var rev = d.rev, me = state.me;
    var inProject = canEditProject(rev.project_id);
    var mine = rev.checked_out_by === me.id;
    var openRevisionExists = d.revisions.some(function (r) { return r.state === "in_work" || r.state === "in_review"; });
    return {
      checkout: inProject && rev.state === "in_work" && !rev.checked_out_by,
      checkin: mine,
      cancel: !!rev.checked_out_by && (mine || me.role === "admin"),
      edit: mine,
      revise: inProject && !openRevisionExists,
      obsolete: inProject && rev.state === "released",
      release: inProject && rev.state === "in_work" && (!rev.checked_out_by || mine) &&
        !d.changes.some(function (c) { return ["draft", "in_review", "rejected"].indexOf(c.status) !== -1; })
    };
  }

  function renderRevision(tab, body, toolbar) {
    var d = tab.data, rev = d.rev, can = revisionAbilities(d);
    var latest = d.revisions[0];
    append(toolbar, [
      tool("Check out", "checkout", function () { doCheckout(tab); }, { primary: can.checkout, disabled: !can.checkout, title: "Take the lock and copy the files into your workspace" }),
      tool("Check in…", "checkin", function () { checkinDialog(tab); }, { disabled: !can.checkin, title: "Upload changed files from the workspace and release the lock" }),
      tool(rev.checked_out_by && rev.checked_out_by !== me.id ? "Force unlock" : "Cancel check-out", "cancel", function () { doCancelCheckout(tab); }, { disabled: !can.cancel }),
      h("span.sep"),
      tool("Edit", "edit", function () { editDialog(tab); }, { disabled: !can.edit, title: "Edit the attributes (requires the check-out)" }),
      tool("Release…", "release", function () { releaseDialog(tab); }, { disabled: !can.release, title: "Create a change order with reviewers and submit it" }),
      tool("Revise", "revise", function () { doRevise(tab); }, { disabled: !can.revise, title: "Create the next revision" }),
      tool("Obsolete", "obsolete", function () { doObsolete(tab); }, { disabled: !can.obsolete, danger: can.obsolete }),
      h("span.sep"),
      tool("Request change…" + (d.requests.length ? " (" + d.requests.length + ")" : ""), "request", function () { requestChangeDialog(rev, null); },
        { title: "Report a problem or propose a change to " + rev.item_number + "/" + rev.code + " (change request)" }),
      watchTool(tab, "items", rev.item_number, d.watching),
      tool("Pack and Go…", "download", function () { savePackage(rev, null); }, { title: "Save this revision with all files of its structure as ZIP (bom.csv, manifest.json)" }),
      tool("Open folder", "folder", function () { openWorkspaceFolder(tab); }, { title: "Open the workspace folder of this revision" }),
      tool("Browser", "web", function () { call("open_web", { path: "/revisions/" + rev.id }).catch(function (e) { toast(errorText(e), "error"); }); }, { title: "Open this revision in the web UI" }),
      tool("Refresh", "refresh", reloadActive),
      h("span.context", { text: rev.project_code ? "Project group " + rev.project_code : "" })
    ]);

    body.appendChild(h("div.object-head", null, [
      h("div", null, [
        h("p.kicker", { text: label(rev.item_type) + (rev.project_code ? " · " + rev.project_code : "") }),
        h("h1", null, [h("span.mono", { text: rev.item_number + "/" + rev.code }), " · " + rev.name]),
        h("div.meta", null, [
          badge(rev.state),
          rev.checked_out_by_name ? h("span.badge.badge-lock", { text: "🔒 " + rev.checked_out_by_name }) : null,
          h("span", { text: "created " + fmtDate(rev.created_at) + " by " + rev.created_by_name })
        ])
      ])
    ]));
    if (latest && latest.id !== rev.id) {
      body.appendChild(h("p.notice", null, ["This is an older revision. ",
        h("button.link", { type: "button", on: { click: function () { openRevision(rev.item_number, latest.code); } } }, "Open the latest revision " + latest.code)]));
    }
    if (rev.checked_out_by && rev.checked_out_by !== state.me.id) {
      body.appendChild(h("p.notice", { text: "Checked out by " + rev.checked_out_by_name + " since " + fmtDate(rev.checked_out_at) + " – only they can change it right now." }));
    }
    d.refs.impacts.filter(function (i) { return !i.resolved_at; }).forEach(function (i) {
      body.appendChild(impactBanner(i, true));
    });
    if (d.requests.length) {
      body.appendChild(h("p.notice", null, [d.requests.length + " open change request" + (d.requests.length === 1 ? "" : "s") + " on " + rev.item_number + ": "]
        .concat(d.requests.map(function (r, i) {
          return h("span", null, [i ? " · " : "", h("button.link.mono", { type: "button", on: { click: function () { openRequest(r.number); } } }, r.number),
            " " + r.title + " (" + label(r.status) + ")"]);
        }))));
    }

    var files3d = d.files.filter(function (f) { return previewFormat(f); });
    var subs = [
      { id: "overview", label: "Overview" },
      { id: "files", label: "Files", count: d.files.length },
      { id: "structure", label: "Structure", count: d.bom.length },
      { id: "whereused", label: "Where used" },
      {
        id: "references", label: "References",
        count: d.refs.outgoing.length + d.refs.incoming.length + d.refs.file_referrers.length +
          d.refs.files.reduce(function (n, f) { return n + f.references.length; }, 0)
      },
      { id: "changes", label: "Changes", count: d.changes.length },
      { id: "history", label: "History" },
      { id: "viewer", label: "3D viewer", count: files3d.length, hide: !files3d.length },
      { id: "workspace", label: "Workspace" }
    ];
    var content = h("div.subcontent");
    body.appendChild(h("div.subtabs", { role: "tablist" }, subs.filter(function (s) { return !s.hide; }).map(function (s) {
      return h("button.subtab" + (tab.sub === s.id ? ".active" : ""), {
        type: "button", role: "tab",
        on: { click: function () { tab.sub = s.id; renderActive(); } }
      }, [s.label, s.count !== undefined ? h("span.count", { text: " " + s.count }) : null]);
    })));
    body.appendChild(content);
    var render = {
      overview: revOverview, files: revFiles, structure: revStructure, whereused: revWhereUsed, references: revReferences,
      changes: revChanges, history: revHistory, viewer: revViewer, workspace: revWorkspace
    }[tab.sub] || revOverview;
    Promise.resolve().then(function () { return render(tab, content); }).catch(function (err) {
      content.appendChild(h("p.notice.error", { text: errorText(err) }));
    });
  }

  function revOverview(tab, el) {
    var d = tab.data, rev = d.rev;
    var props = h("dl.props", null, [
      dt("Item number"), dd(h("span.mono", { text: rev.item_number })),
      dt("Revision"), dd(rev.code),
      dt("Name"), dd(rev.name),
      dt("Type"), dd(label(rev.item_type)),
      dt("Project group"), dd(rev.project_code || "—"),
      dt("Unit"), dd(rev.unit),
      dt("Material"), dd(rev.material || "—"),
      dt("Mass per unit"), dd(rev.mass_kg !== null ? fmtNum(rev.mass_kg, 4) + " kg" : "—"),
      dt("Unit cost"), dd(rev.unit_cost !== null ? fmtNum(rev.unit_cost, 2) + " €" : "—"),
      dt("Description"), dd(h("span.pre", { text: rev.description || "—" })),
      dt("Created"), dd(fmtDate(rev.created_at) + " · " + rev.created_by_name),
      dt("Last modified"), dd(fmtDate(rev.updated_at)),
      dt("Released"), dd(fmtDate(rev.released_at))
    ]);
    var states = ["in_work", "in_review", "released", "obsolete"];
    var next = {
      in_work: "Next: In Review – through a change order (Changes) submitted by a CAD editor of the project group.",
      in_review: "Released when every reviewer approves; back to WIP on rejection or withdrawal.",
      released: "Next: Obsolete (CAD editors of the project group). New work happens in a new revision (Revise).",
      obsolete: "Final state."
    }[rev.state];
    append(el, h("div.grid-2", null, [
      h("section.panel", null, [h("h2", { text: "Properties" }), props]),
      h("div", null, [
        h("section.panel", null, [
          h("h2", { text: "Lifecycle" }),
          h("ol.lifecycle-track", null, states.map(function (s) { return h("li" + (s === rev.state ? ".current" : ""), null, badge(s)); })),
          h("p.small.muted", { text: next })
        ]),
        h("section.panel", null, [
          h("h2", { text: "Revisions" }),
          table(["Rev", "Name", "State", "Created", "Released"], d.revisions.map(function (r) {
            return {
              cells: [h("span.mono", { text: r.code }), r.name, badge(r.state), fmtDate(r.created_at), fmtDate(r.released_at)],
              open: function () { openRevision(rev.item_number, r.code); }, single: true,
              cls: r.id === rev.id ? "selected" : null
            };
          }))
        ])
      ])
    ]));
  }

  function dt(t) { return h("dt", { text: t }); }
  function dd(v) { var e = h("dd"); append(e, v); return e; }

  // ------------------------------------------- previews of native CAD files

  var thumbCache = {};
  /** The preview image stored in an Inventor, Solid Edge or SolidWorks file. */
  function thumbImage(f) {
    var img = h("img.file-thumb.hidden", { alt: "", title: "Preview image stored in the file" });
    var url = thumbCache[f.sha256] || (thumbCache[f.sha256] = invoke("file_bytes", { kind: "thumbnail", id: f.id })
      .then(function (bytes) { return URL.createObjectURL(new Blob([bytes])); }));
    url.then(function (u) { img.src = u; img.classList.remove("hidden"); }, function () { /* the file has none */ });
    return img;
  }

  var cadProgramsPromise = null;
  /** CAD programs on this computer that export STEP: "inventor", "solid-edge" (Windows). */
  function cadPrograms() {
    if (!cadProgramsPromise) cadProgramsPromise = call("cad_programs").catch(function () { return []; });
    return cadProgramsPromise;
  }

  var PROGRAM_LABEL = { "inventor": "Inventor", "solid-edge": "Solid Edge" };

  /** The local program that can export this file to STEP, or null. */
  function exportProgram(name, programs) {
    var m = /\.([a-z0-9]+)$/i.exec(name || "");
    var ext = m ? m[1].toLowerCase() : "";
    if ((ext === "ipt" || ext === "iam") && programs.indexOf("inventor") >= 0) return "inventor";
    if (["par", "psm", "pwd", "asm"].indexOf(ext) >= 0 && programs.indexOf("solid-edge") >= 0) return "solid-edge";
    return null;
  }

  /** STEP export of a workspace file with the local CAD program; resolves to the STEP name. */
  function exportStep(rev, name) {
    return call("cad_export_step", { project: rev.project_code, item: rev.item_number, code: rev.code, file: name });
  }

  function revFiles(tab, el) {
    var d = tab.data, can = revisionAbilities(d);
    if (!d.files.length) {
      el.appendChild(h("p.empty", { text: "No files attached." }));
      return;
    }
    var rows = d.files.map(function (f) {
      var fmt = previewFormat(f);
      return {
        cells: [
          f.format_info && f.format_info.thumbnail
            ? h("span.file-cell", null, [thumbImage(f), h("span", null, [f.filename, " ", formatChip(f)])])
            : h("span", null, [f.filename, " ", formatChip(f)]),
          h("span.badge", { text: "v" + f.version }),
          fmtSize(f.size_bytes),
          fmtDate(f.uploaded_at) + " · " + f.uploaded_by_name,
          h("span.mono.small.dim", { text: f.sha256.slice(0, 12) + "…", title: f.sha256 }),
          h("span.row", null, [
            fmt ? h("button.small", { type: "button", on: { click: function () { tab.sub = "viewer"; tab.viewerFile = f.id; renderActive(); } } }, "3D") : null,
            h("button.small", { type: "button", title: "Copy into the workspace and open with the default application", on: { click: function () { openFile(tab, f.filename); } } }, "Open"),
            h("button.small", { type: "button", on: { click: function () { showVersions(tab, f, can.edit); } } }, "Versions"),
            h("button.small", { type: "button", title: "Request a change to " + f.filename + " (change request)", on: { click: function () { requestChangeDialog(d.rev, f.filename); } } }, "Request…")
          ])
        ]
      };
    });
    el.appendChild(h("section.panel", null, [
      h("div.panel-head", null, [h("h2", { text: "Files (datasets)" }),
        h("span.hint", { text: can.checkin ? "Edit the files in your workspace, then check in." : "Check out to change files." })]),
      table(["File", "Version", "Size", "Uploaded", "SHA-256", ""], rows, { numeric: [2] })
    ]));
  }

  function showVersions(tab, f, canRestore) {
    api("GET", "/files/" + f.id + "/versions").then(function (versions) {
      var body = table(["Version", "Comment", "Size", "Uploaded", ""], versions.map(function (v) {
        return {
          cells: [
            h("span", null, [h("span.badge", { text: "v" + v.version }), v.version === f.version ? h("span.badge.badge-released", { text: "current" }) : null]),
            v.comment || "—", fmtSize(v.size_bytes), fmtDate(v.uploaded_at) + " · " + v.uploaded_by_name,
            h("span.row", null, [
              previewFormat(f) ? h("button.small", { type: "button", on: { click: function () { dlg.close(); tab.sub = "viewer"; tab.viewerFile = f.id; tab.viewerVersion = v.id; renderActive(); } } }, "3D") : null,
              canRestore && v.version !== f.version ? h("button.small", {
                type: "button", on: {
                  click: function () {
                    run("Restoring v" + v.version, function () {
                      return api("POST", "/files/" + f.id + "/restore", { version: v.version });
                    }).then(function (nf) { dlg.close(); toast("Restored " + f.filename + " – it is now v" + nf.version); reloadActive(); }, function () {});
                  }
                }
              }, "Restore") : null
            ])
          ]
        };
      }), { numeric: [2] });
      var dlg = dialog("Versions of " + f.filename, body, [{ label: "Close", primary: true, onClick: function (dd) { dd.close(); } }], { wide: true });
    }, function (err) { toast(errorText(err), "error"); });
  }

  function revStructure(tab, el) {
    var rule = tab.bomRule || "latest";
    var d = tab.data, rev = d.rev;
    var head = h("div.panel-head", null, [
      h("h2", { text: "Product structure" }),
      h("div.row", null, [
        h("span.hint", { text: "Revision rule" }),
        h("button.chip" + (rule === "latest" ? ".active" : ""), { type: "button", on: { click: function () { tab.bomRule = "latest"; renderActive(); } } }, "Latest"),
        h("button.chip" + (rule === "released" ? ".active" : ""), { type: "button", on: { click: function () { tab.bomRule = "released"; renderActive(); } } }, "Latest released"),
        h("button.chip" + (rule === "as_released" ? ".active" : ""), {
          type: "button", title: "Released assemblies show exactly the child revisions they were released with",
          on: { click: function () { tab.bomRule = "as_released"; renderActive(); } }
        }, "As released"),
        h("button.small", {
          type: "button", title: "Save the structure with this revision rule and all its files as ZIP",
          on: { click: function () { savePackage(rev, rule); } }
        }, "Pack and Go…"),
        h("button.small", { type: "button", on: { click: function () { setExpandAll(tab, true); renderActive(); } } }, "Expand all"),
        h("button.small", { type: "button", on: { click: function () { setExpandAll(tab, false); renderActive(); } } }, "Collapse all")
      ])
    ]);
    var panel = h("section.panel", null, [head, h("p.loading", { text: "Loading structure" })]);
    el.appendChild(panel);
    return api("GET", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/bom?expand=true&rule=" + rule).then(function (tree) {
      panel.removeChild(panel.lastChild);
      tab.bomCollapsed = tab.bomCollapsed || {};
      tab.bomTree = tree;
      var rows = [{
        cells: [
          h("span", null, [h("span.ico", { text: typeIcon(rev.item_type) + " " }), h("strong.mono", { text: tree.root_item_number + "/" + tree.root.code })]),
          tree.root.name, badge(tree.root.state), "", "",
          fmtNum(tree.total_mass_kg, 3) + " kg" + (tree.mass_complete ? "" : " ⚠"),
          fmtNum(tree.total_cost, 2) + " €" + (tree.cost_complete ? "" : " ⚠")
        ],
        cls: "selected"
      }];
      function walk(nodes, depth) {
        nodes.forEach(function (n) {
          var hasChildren = n.children && n.children.length;
          var collapsed = !!tab.bomCollapsed[n.line_id];
          var twisty = hasChildren ? h("button.twisty-btn", {
            type: "button", title: collapsed ? "Expand" : "Collapse",
            on: { click: function (e) { e.stopPropagation(); tab.bomCollapsed[n.line_id] = !collapsed; renderActive(); } }
          }, collapsed ? "▸" : "▾") : h("span.twisty-btn", null, "");
          var indent = h("span.indent");
          indent.style.width = (depth * 1.2) + "rem";
          var r = n.revision;
          rows.push({
            cells: [
              h("span", null, [indent, twisty, h("span.mono", { text: String(n.find_number) + "  " }), h("span.mono", { text: n.item_number + (r ? "/" + r.code : "") })]),
              r ? r.name : h("span.warn-text", { text: "no " + (rule === "released" ? "released " : "") + "revision" }),
              r ? badge(r.state) : "",
              fmtNum(n.quantity, 6) + " " + n.unit,
              n.notes || "",
              fmtNum(n.total_mass_kg, 3) + " kg" + (n.mass_complete ? "" : " ⚠"),
              fmtNum(n.total_cost, 2) + " €" + (n.cost_complete ? "" : " ⚠")
            ],
            open: r ? function () { openRevision(n.item_number, r.code); } : null
          });
          if (hasChildren && !collapsed) walk(n.children, depth + 1);
        });
      }
      walk(tree.children, 1);
      panel.appendChild(table(["Find no. / item", "Name", "State", "Qty", "Notes", "Mass", "Cost"], rows, { numeric: [3, 5, 6] }));
      panel.appendChild(h("p.hint", { text: tree.line_count + " lines" + (tree.unresolved_count ? " · " + tree.unresolved_count + " without a matching revision" : "") + " · double-click a line to open it · ⚠ = incomplete mass or cost" }));
    });
  }

  function setExpandAll(tab, expand) {
    tab.bomCollapsed = {};
    if (expand || !tab.bomTree) return;
    (function walk(nodes) {
      nodes.forEach(function (n) {
        if (n.children && n.children.length) { tab.bomCollapsed[n.line_id] = true; walk(n.children); }
      });
    })(tab.bomTree.children);
  }

  function revWhereUsed(tab, el) {
    var rev = tab.data.rev;
    return api("GET", "/items/" + enc(rev.item_number) + "/where-used").then(function (list) {
      el.appendChild(h("section.panel", null, [h("h2", { text: "Where used" }),
        list.length ? table(["Parent", "Name", "State", "Find no.", "Qty"], list.map(function (w) {
          return { cells: [h("span.mono", { text: w.parent_number + "/" + w.parent_code }), w.parent_name, badge(w.parent_state), String(w.find_number), fmtNum(w.quantity, 6)], open: function () { openRevision(w.parent_number, w.parent_code); } };
        }), { numeric: [3, 4] }) : h("p.empty", { text: "Not used in any structure." })]));
    });
  }

  // ---------------------------------------------------------------- references and change propagation

  var REFERENCE_KINDS = [
    { id: "drawing", label: "Drawing of" },
    { id: "specification", label: "Specification for" },
    { id: "reference", label: "References" }
  ];

  function kindLabel(kind) {
    var k = REFERENCE_KINDS.filter(function (x) { return x.id === kind; })[0];
    return k ? k.label : kind;
  }

  /** How a dependent depends on the item that changed ("uses it in its BOM"). */
  function relationText(r, target) {
    target = target || "it";
    switch (r) {
      case "bom": return "uses " + target + " in its BOM";
      case "drawing": return "is a drawing of " + target;
      case "specification": return "is a specification for " + target;
      case "reference": return "references " + target;
      case "file": return "has a CAD file that references " + (target === "it" ? "its files" : "files of " + target);
      default: return "depends on " + target;
    }
  }

  /** An open impact: the dependent has to be checked. */
  function impactBanner(i, inRevision) {
    var can = canEditProject(i.project_id);
    return h("div.impact", { role: "status" }, [
      h("p", null, [h("strong", { text: "Check needed: " }),
        h("button.link.mono", { type: "button", on: { click: function () { openRevision(i.source_number, i.source_code); } } }, i.source_number + "/" + i.source_code),
        " " + i.source_name + (i.event === "released" ? " was released" : " is obsolete and has to be replaced") +
        (i.change_number ? " (" + i.change_number + ")" : "") + " – " + i.item_number + " " + relationText(i.relation) +
        (i.detail ? " (" + i.detail + ")" : "") + "."]),
      h("div.row", null, [
        can ? h("button.small", { type: "button", on: { click: function () { acknowledgeDialog(i); } } }, "Acknowledge…") : null,
        inRevision ? null : h("button.small", { type: "button", on: { click: function () { openRevision(i.item_number, i.revision_code, "references"); } } }, "Open " + i.item_number),
        h("span.hint", { text: "Revising and releasing " + i.item_number + " resolves it as well." })
      ])
    ]);
  }

  function acknowledgeDialog(i) {
    var note = h("textarea", { maxlength: "2000", required: true, placeholder: "e.g. form, fit and function unchanged" });
    dialog("Acknowledge for " + i.item_number, h("div.stack", null, [
      h("p", { text: i.source_number + "/" + i.source_code + " " + (i.event === "released" ? "was released" : "is obsolete") + "; " +
        i.item_number + " " + relationText(i.relation) + ". Why does " + i.item_number + " need no change?" }),
      field("Note", note)
    ]), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Acknowledge", primary: true, submit: true, onClick: function (d) {
          api("POST", "/impacts/" + i.id + "/acknowledge", { note: note.value }).then(function () {
            d.close(); toast(i.item_number + " checked against " + i.source_number + "/" + i.source_code);
            refreshWorklistCount().catch(function () {});
            if (state.view === "worklist") selectView("worklist");
            reloadActive();
          }, function (err) { d.fail(err); });
        }
      }
    ]);
  }

  function addReferenceDialog(tab) {
    var rev = tab.data.rev;
    var kind = h("select", null, REFERENCE_KINDS.map(function (k) { return h("option", { value: k.id }, k.label); }));
    var item = h("input", { type: "text", required: true, placeholder: "Item number, e.g. PRT-000012", spellcheck: "false" });
    var note = h("input", { type: "text", maxlength: "500", placeholder: "optional" });
    dialog("Add a reference to " + rev.item_number + "/" + rev.code, h("div.stack", null, [
      field("Kind", kind, rev.item_number + " is a … of the item"), field("Item", item), field("Note", note)
    ]), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Add", primary: true, submit: true, onClick: function (d) {
          api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/references",
            { item: item.value, kind: kind.value, note: note.value }).then(function (r) {
            d.close(); toast("Added: " + kindLabel(r.kind) + " " + r.to_number); reloadActive();
          }, function (err) { d.fail(err); });
        }
      }
    ]);
  }

  function revReferences(tab, el) {
    var d = tab.data, rev = d.rev, refs = d.refs, can = revisionAbilities(d);
    el.appendChild(h("section.panel", null, [
      h("div.panel-head", null, [h("h2", { text: rev.item_number + "/" + rev.code + " refers to" }),
        h("button.small", { type: "button", disabled: !can.edit, title: can.edit ? "" : "Check out to change the references", on: { click: function () { addReferenceDialog(tab); } } }, "Add reference…")]),
      refs.outgoing.length ? table(["Kind", "Item", "Name", "Latest", "Released with", ""], refs.outgoing.map(function (r) {
        return {
          cells: [kindLabel(r.kind), h("span.mono", { text: r.to_number }), r.to_name + (r.note ? " · " + r.note : ""),
            h("span", null, [h("span.mono", { text: r.to_latest_code + " " }), badge(r.to_latest_state)]), r.released_to_code || "—",
            can.edit ? h("button.small.danger", {
              type: "button", on: {
                click: function () {
                  confirmDialog("Remove reference", "Remove the reference to " + r.to_number + "?", "Remove", true).then(function (ok) {
                    if (!ok) return;
                    run("Removing", function () { return api("DELETE", "/references/" + r.id); }).then(reloadActive, function () {});
                  });
                }
              }
            }, "Remove") : ""],
          open: function () { openRevision(r.to_number, r.to_latest_code); }
        };
      })) : h("p.empty", { text: "No references besides the BOM." })
    ]));
    var referrers = refs.incoming.map(function (r) {
      return { cells: [h("span.mono", { text: r.from_number + "/" + r.from_code }), r.from_name, kindLabel(r.kind).toLowerCase() + " " + rev.item_number, badge(r.from_state)],
        open: function () { openRevision(r.from_number, r.from_code); } };
    }).concat(refs.file_referrers.map(function (f) {
      return { cells: [h("span.mono", { text: f.item_number + "/" + f.revision_code }), f.item_name, f.filename + " references " + f.referenced, badge(f.revision_state)],
        open: function () { openRevision(f.item_number, f.revision_code); } };
    }));
    el.appendChild(h("section.panel", null, [h("h2", { text: "Referenced by" }),
      referrers.length ? table(["Revision", "Name", "How", "State"], referrers) : h("p.empty", { text: "No other item refers to " + rev.item_number + "." })]));
    if (refs.files.length) {
      el.appendChild(h("section.panel", null, [h("h2", { text: "Found in the CAD files" }),
        h("p.hint", { text: "File names inside the assemblies and drawings of this revision, matched against the vault. Heuristic: TeamCraft searches the files for CAD file names; compressed parts of proprietary formats are not read, and names a CAD program keeps after a component was removed are found too." })
      ].concat(refs.files.map(function (f) {
        return h("div", null, [h("p.small", null, [h("strong", { text: f.filename }), " references:"]),
          h("ul.plain", null, f.references.map(function (n) {
            return h("li", null, [h("span.mono.small", { text: n.name }), " → "].concat(n.matches.length ? n.matches.map(function (m) {
              return m.same_revision ? h("span.small", { text: "in this revision " }) :
                h("button.link.mono", { type: "button", on: { click: function () { openRevision(m.item_number, m.revision_code); } } }, m.item_number + "/" + m.revision_code + " ");
            }) : [h("span.badge.badge-in_review", { text: "not in TeamCraft" })]));
          }))]);
      }))));
    }
    var resolved = refs.impacts.filter(function (i) { return i.resolved_at; });
    if (resolved.length) {
      el.appendChild(h("section.panel", null, [h("h2", { text: "Earlier checks" }),
        table(["Changed", "Event", "Resolved", "Note"], resolved.map(function (i) {
          return { cells: [h("span.mono", { text: i.source_number + "/" + i.source_code }), i.event,
            (i.resolution === "acknowledged" ? "acknowledged by " + i.resolved_by_name : i.resolution) + " · " + fmtDate(i.resolved_at), i.note || "—"] };
        }))]));
    }
  }

  function revChanges(tab, el) {
    var list = tab.data.changes;
    el.appendChild(h("section.panel", null, [h("h2", { text: "Change orders" }),
      list.length ? table(["Change", "Title", "Status", "Created"], list.map(function (c) {
        return { cells: [h("span.mono", { text: c.number }), c.title, badge(c.status), fmtDate(c.created_at)], open: function () { openChange(c.number); } };
      })) : h("p.empty", { text: "Not part of any change order yet – “Release…” in the toolbar creates one." })]));
  }

  function revHistory(tab, el) {
    var rev = tab.data.rev;
    return api("GET", "/items/" + enc(rev.item_number) + "/history?limit=200").then(function (list) {
      el.appendChild(h("section.panel", null, [h("h2", { text: "History" }),
        list.length ? table(["When", "Who", "What"], list.map(function (e) {
          return { cells: [fmtDate(e.at), e.username, e.summary] };
        })) : h("p.empty", { text: "No activity yet." })]));
    });
  }

  // ---------------------------------------------------------------- 3D viewer

  function disposeViewer() {
    if (state.viewer) {
      try { state.viewer.dispose(); } catch (e) { /* already gone */ }
      state.viewer = null;
    }
  }

  function revViewer(tab, el) {
    var files = tab.data.files.filter(function (f) { return previewFormat(f); });
    if (!files.length) {
      el.appendChild(h("p.empty", { text: "No 3D files (STEP, STL, OBJ, 3MF, glTF, PLY, point clouds, …) – native CAD files (SolidWorks, NX, Creo, CATIA, …) are shown in 3D through a STEP export next to them." }));
      return;
    }
    var current = files.filter(function (f) { return f.id === tab.viewerFile; })[0] || files[0];
    tab.viewerFile = current.id;
    var canvas = h("canvas.viewer-canvas", { tabindex: "0", "aria-label": "3D view of " + current.filename });
    var status = h("p.small.muted", { role: "status", text: "Loading" });
    var resetBtn = h("button.small", { type: "button", title: "Back to the initial view (double-click)" }, "Reset view");
    var upBtn = h("button.small", { type: "button", title: "Switch between Z-up and Y-up models" }, "Z up");
    // STL export of STEP/3MF/… models (e.g. for 3D printing); saved through a native dialog in Rust.
    var noExport = ["stl", "pts", "xyz", "las"].indexOf(previewFormat(current)) !== -1;
    var stlBtn = noExport ? null : h("button.small", {
      type: "button", disabled: true, title: "Save the tessellated model as STL (units as in the file, usually mm)",
      on: {
        click: function () {
          var name = current.filename.replace(/\.[^.]+$/, "") + ".stl";
          var data;
          try { data = new Uint8Array(state.viewer.exportStl()); } catch (e) { toast(errorText(e), "error"); return; }
          invoke("save_stl", data, { headers: { "x-file-name": encodeURIComponent(name) } }).then(function (path) {
            if (path) toast("Saved " + path);
          }, function (e) { toast(errorText(e), "error"); });
        }
      }
    }, "Export STL…");
    el.appendChild(h("div.viewer-layout", null, [
      h("div.viewer-files", null, [h("h3", { text: "Models" })].concat(files.map(function (f) {
        return h("button" + (f.id === current.id ? ".active" : ""), {
          type: "button", title: f.filename,
          on: { click: function () { tab.viewerFile = f.id; tab.viewerVersion = null; renderActive(); } }
        }, f.filename + "  v" + f.version);
      }))),
      h("div", null, [canvas, h("div.viewer-bar", null, [status, h("div.row", null, [upBtn, resetBtn, stlBtn])]),
        h("p.hint", { text: "Drag to rotate, wheel to zoom, arrow keys to turn, double-click to reset." })])
    ]));
    // the server converts the file into a mesh (pure Rust) – the app only draws it
    var versionId = tab.viewerVersion;
    state.viewer = window.TeamCraftViewer.mount(canvas, {
      statusEl: status,
      resetBtn: resetBtn,
      upBtn: upBtn,
      onReady: function () { if (stlBtn) stlBtn.disabled = false; },
      load: function () {
        return invoke("file_bytes", versionId ? { kind: "version-mesh", id: versionId } : { kind: "mesh", id: current.id });
      }
    });
  }

  // ---------------------------------------------------------------- workspace

  function revWorkspace(tab, el) {
    var rev = tab.data.rev, can = revisionAbilities(tab.data);
    return Promise.all([call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }), cadPrograms()]).then(function (r) {
      var ws = r[0], programs = r[1];
      var changed = ws.files.filter(function (f) { return f.state === "modified" || f.state === "new"; });
      // STEP for the 3D preview, written by the CAD program installed here
      function stepButton(f) {
        var program = f.state !== "missing" && exportProgram(f.name, programs);
        if (!program) return null;
        return h("button.small", {
          type: "button", title: "Create a STEP export with " + PROGRAM_LABEL[program] + " next to the file – check it in with the model for the 3D preview",
          on: { click: function () {
            run("Exporting " + f.name + " to STEP with " + PROGRAM_LABEL[program], function () { return exportStep(rev, f.name); })
              .then(function (step) { toast("Created " + step + " – check it in together with " + f.name); renderActive(); }, function () {});
          } }
        }, "STEP");
      }
      el.appendChild(h("section.panel", null, [
        h("div.panel-head", null, [
          h("h2", { text: "Local workspace" }),
          h("div.row", null, [
            tool("Open folder", "folder", function () { openWorkspaceFolder(tab); }),
            tool("Get files", "download", function () {
              run("Copying files into the workspace", function () { return call("workspace_download", { project: rev.project_code, item: rev.item_number, code: rev.code }); })
                .then(function (r) { reportDownload(r); renderActive(); }, function () {});
            }, { title: "Copy the current files into the workspace without checking out" }),
            can.checkin ? tool("Check in…", "checkin", function () { checkinDialog(tab); }, { primary: changed.length > 0 }) : null
          ])
        ]),
        h("p.small.muted", null, ["Folder: ", h("span.mono", { text: ws.dir })]),
        !ws.exists ? h("p.empty", { text: "Not in your workspace yet – check out (or “Get files”) to copy the files." }) :
          ws.files.length ? table(["File", "Local state", "Server version", "Size", ""], ws.files.map(function (f) {
            return {
              cells: [f.local_name ? h("span", null, [f.name, " ", h("span.dim.small", { text: "← " + f.local_name, title: "Newest Creo iteration in the workspace; it is checked in as " + f.name })]) : f.name, badge(f.state), f.server_version ? "v" + f.server_version : "—", f.state === "missing" ? "—" : fmtSize(f.size),
                h("span.row", null, [
                  f.state !== "missing" ? h("button.small", { type: "button", on: { click: function () { openFile(tab, f.name, true); } } }, "Open") : null,
                  f.state !== "missing" ? stepButton(f) : null,
                  h("button.small", { type: "button", title: "Request a change to " + f.name + " (change request)", on: { click: function () { requestChangeDialog(rev, f.name); } } }, "Request…")
                ])]
            };
          }), { numeric: [3] }) : h("p.empty", { text: "The folder is empty." }),
        changed.length && !can.checkin ? h("p.notice", { text: changed.length + " local change(s), but you have not checked out this revision – check out first, then check in." }) : null
      ]));
    });
  }

  function reportDownload(r) {
    var parts = [];
    if (r.downloaded.length) parts.push(r.downloaded.length + " file(s) copied");
    if (r.kept.length) parts.push(r.kept.length + " locally modified file(s) kept: " + r.kept.join(", "));
    toast(parts.length ? parts.join(" · ") : "The workspace is up to date.", r.kept.length ? "info" : null);
  }

  function openWorkspaceFolder(tab) {
    var rev = tab && tab.data && tab.data.rev;
    call("workspace_open", rev ? { project: rev.project_code, item: rev.item_number, code: rev.code, file: null } : { project: null, item: null, code: null, file: null })
      .catch(function (e) { toast(errorText(e), "error"); });
  }

  function openFile(tab, filename, local) {
    var rev = tab.data.rev;
    var ready = local ? Promise.resolve() : run("Copying " + filename + " into the workspace", function () {
      return call("workspace_download", { project: rev.project_code, item: rev.item_number, code: rev.code });
    });
    ready.then(function () {
      return call("workspace_open", { project: rev.project_code, item: rev.item_number, code: rev.code, file: filename });
    }).catch(function (e) { if (e) toast(errorText(e), "error"); });
  }

  function doCheckout(tab) {
    var rev = tab.data.rev;
    run("Checking out " + rev.item_number + "/" + rev.code, function () {
      return call("workspace_checkout", { project: rev.project_code, item: rev.item_number, code: rev.code });
    }).then(function (r) {
      toast("Checked out " + rev.item_number + "/" + rev.code + " – files are in " + r.dir);
      if (r.kept.length) reportDownload(r);
      tab.sub = "workspace";
      reloadActive();
      refreshWorklistCount();
    }, function () {});
  }

  // Cancelling one's own check-out takes effect at once (the local files stay
  // in the workspace); releasing someone else's lock is confirmed first.
  function doCancelCheckout(tab) {
    var rev = tab.data.rev;
    var other = rev.checked_out_by !== state.me.id;
    function release() {
      run("Releasing the check-out", function () {
        return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/cancel-checkout");
      }).then(function () { toast("Released the check-out of " + rev.item_number + "/" + rev.code); reloadActive(); refreshWorklistCount(); }, function () {});
    }
    if (!other) { release(); return; }
    confirmDialog("Force unlock",
      "Release the check-out of " + rev.item_number + "/" + rev.code + " held by " + rev.checked_out_by_name + "?",
      "Force unlock", true).then(function (ok) { if (ok) release(); });
  }

  function checkinDialog(tab) {
    var rev = tab.data.rev;
    Promise.all([call("workspace_status", { project: rev.project_code, item: rev.item_number, code: rev.code }), cadPrograms()]).then(function (r) {
      var ws = r[0], programs = r[1];
      var candidates = ws.files.filter(function (f) { return f.state === "modified" || f.state === "new"; });
      var boxes = candidates.map(function (f) {
        return { name: f.name, input: h("input", { type: "checkbox", checked: true }), state: f.state };
      });
      var comment = h("input", { type: "text", maxlength: "2000", placeholder: "What changed, e.g. wall thickness 2 → 2.5 mm" });
      // Like the Vault add-in or the Teamcenter integration: viewables at check-in.
      var exportable = boxes.filter(function (b) { return exportProgram(b.name, programs); });
      var withStep = h("input", { type: "checkbox", checked: true });
      var stepNote = exportable.length ? h("label.check", null, [withStep,
        "Create STEP exports with " + exportable.map(function (b) { return PROGRAM_LABEL[exportProgram(b.name, programs)]; })
          .filter(function (v, i, a) { return a.indexOf(v) === i; }).join(" / ") +
        " first, for the 3D preview (" + exportable.length + (exportable.length === 1 ? " file)" : " files)")]) : null;
      var body = h("div.stack", null, [
        h("p.small.muted", null, ["From ", h("span.mono", { text: ws.dir })]),
        candidates.length ? h("div.stack", null, boxes.map(function (b) {
          return h("label.check", null, [b.input, b.name, " ", badge(b.state)]);
        })) : h("p.empty", { text: "No modified or new files – checking in only releases the lock." }),
        stepNote,
        field("Comment", comment, "stored with every new version")
      ]);
      dialog("Check in " + rev.item_number + "/" + rev.code, body, [
        { label: "Cancel", onClick: function (d) { d.close(); } },
        {
          label: "Check in and release", primary: true, submit: true, onClick: function (d) {
            var files = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.name; });
            var toExport = stepNote && withStep.checked ? files.filter(function (n) { return exportProgram(n, programs); }) : [];
            // one export after the other: the CAD program handles one document at a time
            var exported = toExport.reduce(function (chain, name) {
              return chain.then(function () {
                setStatus("Exporting " + name + " to STEP with " + PROGRAM_LABEL[exportProgram(name, programs)] + " …");
                return exportStep(rev, name).then(function (step) { if (files.indexOf(step) < 0) files.push(step); });
              });
            }, Promise.resolve());
            exported.then(function () {
              setStatus("Checking in …");
              return call("workspace_checkin", { project: rev.project_code, item: rev.item_number, code: rev.code, files: files, comment: comment.value });
            }).then(function () {
              setStatus("");
              d.close();
              toast("Checked in " + rev.item_number + "/" + rev.code + (files.length ? " with " + files.join(", ") : ""));
              reloadActive();
              refreshWorklistCount();
            }, function (err) { setStatus(""); d.fail(err); });
          }
        }
      ], { wide: true });
    }, function (err) { toast(errorText(err), "error"); });
  }

  function editDialog(tab) {
    var rev = tab.data.rev;
    var name = h("input", { type: "text", value: rev.name, maxlength: "200", required: true });
    var material = h("input", { type: "text", value: rev.material, maxlength: "200" });
    var mass = h("input", { type: "text", inputmode: "decimal", value: rev.mass_kg === null ? "" : String(rev.mass_kg) });
    var cost = h("input", { type: "text", inputmode: "decimal", value: rev.unit_cost === null ? "" : String(rev.unit_cost) });
    var desc = h("textarea", { maxlength: "10000" });
    desc.value = rev.description || "";
    function num(input) {
      var v = input.value.trim().replace(",", ".");
      if (!v) return null;
      var n = Number(v);
      if (!isFinite(n)) throw new Error("'" + input.value + "' is not a number.");
      return n;
    }
    dialog("Edit " + rev.item_number + "/" + rev.code, h("div.stack", null, [
      field("Name", name), field("Material", material),
      h("div.grid-2", null, [field("Mass per unit (kg)", mass), field("Unit cost (€)", cost)]),
      field("Description", desc)
    ]), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Save", primary: true, submit: true, onClick: function (d) {
          var body;
          try {
            body = { name: name.value, material: material.value, description: desc.value, mass_kg: num(mass), unit_cost: num(cost) };
          } catch (e) { d.fail(e.message); return; }
          api("PATCH", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code), body).then(function () {
            d.close(); toast("Saved " + rev.item_number + "/" + rev.code); reloadActive();
          }, function (err) { d.fail(err); });
        }
      }
    ], { wide: true });
  }

  /**
   * Release workflow: creates a change order for this revision, assigns the
   * reviewers (CAD editors of the project group are offered) and submits it.
   */
  function releaseDialog(tab) {
    var rev = tab.data.rev;
    var mine = rev.checked_out_by === state.me.id;
    var title = h("input", { type: "text", maxlength: "200", required: true, value: "Release " + rev.item_number + "/" + rev.code + " – " + rev.name });
    var priority = h("select", null, ["low", "normal", "high", "critical"].map(function (p) {
      return h("option", { value: p, text: label(p), selected: p === "normal" });
    }));
    var desc = h("textarea", { maxlength: "20000", placeholder: "What is released and why" });
    var others = h("input", { type: "text", placeholder: "user names, comma-separated" });
    var submitNow = h("input", { type: "checkbox", checked: true });
    var reviewerBox = h("div.stack", null, h("p.loading", { text: "Loading the members of " + (rev.project_code || "the project group") }));
    var boxes = [];
    if (rev.project_code) {
      api("GET", "/projects/" + enc(rev.project_code) + "/members").then(function (members) {
        clear(reviewerBox);
        var candidates = members.filter(function (m) {
          return m.active && m.user_id !== state.me.id && (m.role === "editor" || m.role === "admin");
        });
        if (!candidates.length) {
          reviewerBox.appendChild(h("p.hint", { text: "No other CAD editors in " + rev.project_code + " – name reviewers below." }));
        }
        candidates.forEach(function (m) {
          var input = h("input", { type: "checkbox" });
          boxes.push({ username: m.username, input: input });
          reviewerBox.appendChild(h("label.check", null, [input, m.display_name, h("span.dim", { text: " " + m.username + " · " + label(m.role) })]));
        });
      }, function (err) { clear(reviewerBox); reviewerBox.appendChild(h("p.notice.error", { text: errorText(err) })); });
    } else {
      clear(reviewerBox);
    }
    var body = h("div.stack", null, [
      mine ? h("p.notice", { text: "You still hold the check-out. Check in your workspace changes first – submitting releases the lock without uploading anything." }) : null,
      field("Title", title),
      h("div.grid-2", null, [field("Priority", priority), h("label.check", null, [submitNow, "Submit for review now"])]),
      field("Description", desc),
      h("h3", { text: "Reviewers" }),
      h("p.hint", { text: "All reviewers must approve; you cannot review your own change (four-eyes principle)." }),
      reviewerBox,
      field("Other reviewers", others, "optional")
    ]);
    dialog("Release " + rev.item_number + "/" + rev.code, body, [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Create change", primary: true, submit: true, onClick: function (d) {
          var reviewers = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.username; })
            .concat(others.value.split(",").map(function (s) { return s.trim(); }).filter(Boolean));
          if (submitNow.checked && !reviewers.length) { d.fail("Choose at least one reviewer to submit the change."); return; }
          var number = null;
          setStatus("Creating the change …");
          api("POST", "/changes", { title: title.value, description: desc.value, priority: priority.value }).then(function (c) {
            number = c.number;
            return api("POST", "/changes/" + enc(number) + "/items", { item: rev.item_number, revision: rev.code });
          }).then(function () {
            return reviewers.reduce(function (p, username) {
              return p.then(function () { return api("POST", "/changes/" + enc(number) + "/reviewers", { username: username }); });
            }, Promise.resolve());
          }).then(function () {
            return submitNow.checked ? api("POST", "/changes/" + enc(number) + "/submit") : null;
          }).then(function () {
            setStatus("");
            d.close();
            toast(number + (submitNow.checked ? " submitted for review – " + rev.item_number + "/" + rev.code + " is now In Review" : " created as draft"));
            tab.data = null;
            refreshWorklistCount();
            openChange(number);
          }, function (err) {
            setStatus("");
            d.fail(number ? errorText(err) + " – " + number + " exists as a draft; finish it in its tab." : err);
            if (number) { tab.data = null; openChange(number); }
          });
        }
      }
    ], { wide: true });
  }

  function doRevise(tab) {
    var rev = tab.data.rev;
    confirmDialog("Revise", "Create a new revision of " + rev.item_number + " based on " + rev.code + "? It is checked out to you.", "Revise").then(function (ok) {
      if (!ok) return;
      run("Creating the next revision", function () {
        return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/revise");
      }).then(function (nr) {
        toast("Created " + nr.item_number + "/" + nr.code + " (in work, checked out to you)");
        openRevision(nr.item_number, nr.code, "workspace");
        refreshWorklistCount();
      }, function () {});
    });
  }

  function doObsolete(tab) {
    var rev = tab.data.rev;
    confirmDialog("Mark obsolete", "Mark " + rev.item_number + "/" + rev.code + " as obsolete? This cannot be undone.", "Mark obsolete", true).then(function (ok) {
      if (!ok) return;
      run("Marking obsolete", function () {
        return api("POST", "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/obsolete");
      }).then(function () { toast(rev.item_number + "/" + rev.code + " is now obsolete"); reloadActive(); }, function () {});
    });
  }

  // ======================================================================
  // Change tab
  // ======================================================================

  function openChange(number) {
    openTab({ key: "chg:" + number, kind: "change", number: number, title: number });
  }

  function loadChange(tab) {
    if (tab.data) return Promise.resolve(tab.data);
    return api("GET", "/changes/" + enc(tab.number)).then(function (r) {
      tab.data = r;
      tab.title = r.change.number + " " + r.change.title;
      renderTabs();
      return r;
    });
  }

  function renderChange(tab, body, toolbar) {
    var c = tab.data.change, me = state.me;
    var owner = canEdit() && (c.created_by === me.id || me.role === "admin");
    var myReview = tab.data.reviews.filter(function (r) { return r.reviewer_id === me.id; })[0];
    var canReview = c.status === "in_review" && myReview && myReview.decision === "pending";
    function action(path, text) {
      run(text, function () { return api("POST", "/changes/" + enc(c.number) + "/" + path); })
        .then(function () { toast(text + " – done"); reloadActive(); refreshWorklistCount(); }, function () {});
    }
    append(toolbar, [
      tool("Submit for review", "checkin", function () { action("submit", "Submitting " + c.number); },
        { primary: owner && c.status === "draft", disabled: !(owner && c.status === "draft") }),
      tool("Withdraw", "cancel", function () { action("withdraw", "Withdrawing " + c.number); }, { disabled: !(owner && c.status === "in_review") }),
      h("span.sep"),
      watchTool(tab, "changes", c.number, !!tab.data.watching),
      tool("Browser", "web", function () { call("open_web", { path: "/changes/" + c.id }).catch(function (e) { toast(errorText(e), "error"); }); }, { title: "Open this change in the web UI (reviewers, items, cancel …)" }),
      tool("Refresh", "refresh", reloadActive)
    ]);
    body.appendChild(h("div.object-head", null, [h("div", null, [
      h("p.kicker", { text: "Change order · priority " + label(c.priority) }),
      h("h1", null, [h("span.mono", { text: c.number }), " · " + c.title]),
      h("div.meta", null, [badge(c.status), h("span", { text: "created " + fmtDate(c.created_at) + " by " + c.created_by_name })])
    ])]));
    var hints = {
      draft: "Draft: add the revisions to release and at least one reviewer, then submit.",
      in_review: "In review: all reviewers must approve; a single rejection sends the revisions back to WIP.",
      rejected: "Rejected: fix the revisions, then reopen and resubmit in the web UI – or cancel the change.",
      released: "Released: all revisions of this change are released."
    };
    if (hints[c.status]) body.appendChild(h("p.notice.info", { text: hints[c.status] }));

    if (canReview) {
      var comment = h("textarea", { maxlength: "4000", placeholder: "Comment – required when rejecting" });
      var decide = function (decision) {
        run(decision === "approve" ? "Approving" : "Rejecting", function () {
          return api("POST", "/changes/" + enc(c.number) + "/review", { decision: decision, comment: comment.value });
        }).then(function (r) {
          toast(c.number + " – " + (decision === "approve" ? "approved" : "rejected") + "; status " + label(r.change.status));
          reloadActive(); refreshWorklistCount();
        }, function () {});
      };
      body.appendChild(h("section.panel", null, [h("h2", { text: "Your review" }), comment,
        h("div.row", null, [
          h("button.primary", { type: "button", on: { click: function () { decide("approve"); } } }, "Approve"),
          h("button.danger", { type: "button", on: { click: function () { decide("reject"); } } }, "Reject")
        ])]));
    }

    append(body, h("div.grid-2", null, [
      h("section.panel", null, [h("h2", { text: "Details" }), h("dl.props", null, [
        dt("Description"), dd(h("span.pre", { text: c.description || "—" })),
        dt("Priority"), dd(label(c.priority)),
        dt("Submitted"), dd(fmtDate(c.submitted_at)),
        dt("Closed"), dd(fmtDate(c.closed_at)),
        dt("Last update"), dd(fmtDate(c.updated_at))
      ])]),
      h("section.panel", null, [h("h2", { text: "Reviewers" }),
        tab.data.reviews.length ? table(["Reviewer", "Decision", "Comment"], tab.data.reviews.map(function (r) {
          return { cells: [r.reviewer_name, badge(r.decision), h("span.pre", { text: r.comment || "" })] };
        })) : h("p.empty", { text: "No reviewers assigned yet." })])
    ]));
    body.appendChild(h("section.panel", null, [h("h2", { text: "Affected revisions" }),
      tab.data.items.length ? table(["Revision", "Name", "State"], tab.data.items.map(function (i) {
        return { cells: [h("span.mono", { text: i.item_number + "/" + i.code }), i.name, h("span", null, [badge(i.state), i.checked_out_by_name ? h("span.badge.badge-lock", { text: "🔒 " + i.checked_out_by_name }) : null])], open: function () { openRevision(i.item_number, i.code); } };
      })) : h("p.empty", { text: "No revisions yet." })]));
    // Impact analysis: the items that depend on the revisions of this change.
    var impactPanel = h("section.panel", null, [h("h2", { text: "Impact analysis" }), h("p.muted.small", { text: "Loading …" })]);
    body.appendChild(impactPanel);
    api("GET", "/changes/" + enc(c.number) + "/impact").then(function (list) {
      clear(impactPanel);
      append(impactPanel, [h("h2", { text: "Impact analysis" }),
        list.length ? h("p.hint", { text: c.status === "released"
          ? "These items depended on the released revisions; the ones not released with this change were flagged for a check."
          : "These items depend on the revisions of this change; on approval the ones not in it are flagged for a check." }) : null,
        list.length ? table(["Level", "Revision", "Name", "Depends on", "State"], list.map(function (dep) {
          return {
            cells: [String(dep.level), h("span.mono", { text: dep.item_number + "/" + dep.revision_code }),
              h("span", null, [dep.item_name, dep.in_change ? h("span.badge.badge-member", { text: " in this change" }) : null]),
              relationText(dep.relation, dep.via_number), badge(dep.revision_state)],
            open: function () { openRevision(dep.item_number, dep.revision_code); }
          };
        }), { numeric: [0] }) : h("p.empty", { text: "No other item uses or references the revisions of this change." })]);
    }, function () {
      clear(impactPanel);
      append(impactPanel, [h("h2", { text: "Impact analysis" }), h("p.empty", { text: "Not available on this server." })]);
    });
  }

  // ======================================================================
  // Change requests (ECR)
  // ======================================================================

  function reasonLabel(value) {
    var r = REQUEST_REASONS.filter(function (x) { return x.value === value; })[0];
    return r ? r.label : label(value);
  }

  /** Lets the user choose a screenshot, mark-up or PDF; resolves to a path or null. */
  function chooseAttachment() {
    if (!TAURI.dialog) return Promise.resolve(null);
    return TAURI.dialog.open({
      multiple: false, title: "Screenshot, mark-up or PDF",
      filters: [{ name: "Images and PDF", extensions: ["png", "jpg", "jpeg", "gif", "webp", "pdf"] }]
    }).then(function (chosen) { return Array.isArray(chosen) ? chosen[0] || null : chosen || null; });
  }

  /**
   * "Request change…" from a revision tab or a workspace file: raises a
   * change request on the revision (prefilled with the file name) and
   * attaches an optional screenshot or PDF.
   */
  function requestChangeDialog(rev, filename) {
    var title = h("input", { type: "text", maxlength: "200", required: true, placeholder: "What is wrong, what should change?" });
    var reason = h("select", null, REQUEST_REASONS.map(function (r) { return h("option", { value: r.value, text: r.label }); }));
    var priority = h("select", null, ["low", "normal", "high", "critical"].map(function (p) {
      return h("option", { value: p, text: label(p), selected: p === "normal" });
    }));
    var desc = h("textarea", { maxlength: "20000", rows: "7", placeholder: "What you observed, where, how often – and what you suggest" });
    if (filename) desc.value = "Found in " + filename + ":\n\n";
    var due = h("input", { type: "date" });
    var shot = null;
    var shotLabel = h("span.dim.small", { text: "none" });
    var pick = TAURI.dialog ? h("button", { type: "button", on: { click: function () {
      chooseAttachment().then(function (p) { if (p) { shot = p; shotLabel.textContent = baseName(p); } }, function () {});
    } } }, "Choose…") : null;
    var body = h("div.stack", null, [
      h("p.notice.info", { text: "About " + rev.item_number + "/" + rev.code + " – " + rev.name + (filename ? " · file " + filename : "") +
        ". The CAD editors of " + (rev.project_code || "the item") + " triage it; an accepted request becomes a change order." }),
      field("Title", title),
      field("Problem and proposed solution", desc),
      h("div.grid-2", null, [field("Reason", reason), field("Priority", priority)]),
      h("div.grid-2", null, [
        field("Needed by", due, "optional"),
        // not a <label>: a click anywhere in it would open the file dialog
        pick ? h("div", null, ["Screenshot / PDF", h("span.hint", { text: " optional, up to 10 MiB" }), h("div.row", null, [pick, shotLabel])]) : null
      ])
    ]);
    dialog("Request a change", body, [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Raise request", primary: true, submit: true, onClick: function (d) {
          var number = null;
          setStatus("Raising the change request …");
          api("POST", "/change-requests", {
            title: title.value, description: desc.value, reason: reason.value, priority: priority.value,
            due_date: due.value || null,
            items: [{ item: rev.item_number, revision: rev.code, note: filename ? "File: " + filename : "" }]
          }).then(function (r) {
            number = r.change_request.number;
            return shot ? call("upload_request_attachment", { number: number, path: shot }) : null;
          }).then(function () {
            setStatus("");
            d.close();
            toast(number + " raised – the CAD editors of " + (rev.project_code || "the item") + " are notified");
            refreshWorklistCount().catch(function () {});
            openRequest(number);
          }, function (err) {
            setStatus("");
            if (!number) { d.fail(err); return; }
            d.close();
            toast(number + " was raised, but the attachment failed: " + errorText(err), "error");
            openRequest(number);
          });
        }
      }
    ], { wide: true });
  }

  function openRequest(number) {
    openTab({ key: "ecr:" + number, kind: "request", number: number, title: number });
  }

  function loadRequest(tab) {
    if (tab.data) return Promise.resolve(tab.data);
    return api("GET", "/change-requests/" + enc(tab.number)).then(function (r) {
      tab.data = r;
      tab.title = r.change_request.number + " " + r.change_request.title;
      renderTabs();
      return r;
    });
  }

  var attachmentCache = {};

  function attachmentImage(a) {
    var img = h("img.attachment-thumb.hidden", { alt: a.filename, title: a.filename });
    var url = attachmentCache[a.id] || (attachmentCache[a.id] = invoke("file_bytes", { kind: "request-attachment", id: a.id })
      .then(function (bytes) { return URL.createObjectURL(new Blob([bytes], { type: a.content_type })); }));
    url.then(function (u) { img.src = u; img.classList.remove("hidden"); }, function () { /* not available */ });
    return img;
  }

  function attachToRequest(r) {
    chooseAttachment().then(function (path) {
      if (!path) return;
      run("Attaching " + baseName(path), function () { return call("upload_request_attachment", { number: r.number, path: path }); })
        .then(function (a) { toast("Attached " + a.filename + " to " + r.number); reloadActive(); }, function () {});
    }, function (e) { toast(errorText(e), "error"); });
  }

  function createChangeFromRequest(tab) {
    var r = tab.data.change_request;
    var inWork = tab.data.items.filter(function (i) { return i.latest_state === "in_work"; });
    confirmDialog("Create change order", "Create a draft change order for " + r.number + (inWork.length
      ? " with " + inWork.map(function (i) { return i.item_number + "/" + i.latest_code; }).join(", ") + "?"
      : "? No affected item has a revision in work yet – revise them first or add the revisions later."), "Create").then(function (ok) {
      if (!ok) return;
      run("Creating the change order for " + r.number, function () {
        return api("POST", "/change-requests/" + enc(r.number) + "/create-change", {});
      }).then(function (c) {
        toast("Created " + c.change.number + (c.skipped.length ? " – not added: " + c.skipped.join("; ") : " – add reviewers and submit it"), c.skipped.length ? "info" : undefined);
        tab.data = null;
        refreshWorklistCount().catch(function () {});
        openChange(c.change.number);
      }, function () {});
    });
  }

  function renderRequest(tab, body, toolbar) {
    var d = tab.data, r = d.change_request, can = d.can || {};
    function action(path, text, payload) {
      return run(text, function () { return api("POST", "/change-requests/" + enc(r.number) + "/" + path, payload === undefined ? null : payload); })
        .then(function () { toast(text + " – done"); reloadActive(); refreshWorklistCount().catch(function () {}); }, function () {});
    }
    append(toolbar, [
      tool("Start triage", "checkout", function () { action("triage", "Starting the triage of " + r.number); },
        { primary: !!can.start_triage, disabled: !can.start_triage, title: "Take it up (in triage)" + (r.assignee ? "" : ", assigned to you") }),
      tool("Take up again", "revise", function () { action("reopen", "Taking up " + r.number); }, { disabled: !can.reopen }),
      tool("Create change order", "release", function () { createChangeFromRequest(tab); },
        { primary: !!can.implement, disabled: !can.implement, title: "Draft change order with the in-work revisions of the affected items" }),
      tool("Withdraw", "cancel", function () { action("withdraw", "Withdrawing " + r.number); }, { disabled: !can.withdraw }),
      tool("Attach…", "plus", function () { attachToRequest(r); }, { disabled: !can.attach || !TAURI.dialog, title: "Attach a screenshot, mark-up or PDF" }),
      h("span.sep"),
      watchTool(tab, "change-requests", r.number, !!d.watching),
      tool("Browser", "web", function () { call("open_web", { path: "/requests/" + r.id }).catch(function (e) { toast(errorText(e), "error"); }); },
        { title: "Open this change request in the web UI (assign, link a change order, edit …)" }),
      tool("Refresh", "refresh", reloadActive)
    ]);
    body.appendChild(h("div.object-head", null, [h("div", null, [
      h("p.kicker", { text: "Change request · " + reasonLabel(r.reason) + " · priority " + label(r.priority) }),
      h("h1", null, [h("span.mono", { text: r.number }), " · " + r.title]),
      h("div.meta", null, [badge(r.status), h("span", { text: "requested " + fmtDate(r.created_at) + " by " + r.requested_by_name })])
    ])]));
    var hints = {
      "new": "New: waiting for the CAD editors of the affected project groups to triage it.",
      in_triage: "In triage" + (r.assignee_name ? " with " + r.assignee_name : "") + ": accept, defer or reject.",
      accepted: d.changes.length ? "Accepted – implemented by the change order(s) below; done when one of them is released." : "Accepted – next: create its change order.",
      deferred: "Deferred: " + r.decision_comment,
      rejected: "Rejected: " + r.decision_comment,
      implemented: "Implemented – its change order was released.",
      withdrawn: "Withdrawn by the requester."
    };
    if (hints[r.status]) body.appendChild(h("p.notice.info", { text: hints[r.status] }));

    if (can.decide) {
      var comment = h("textarea", { maxlength: "4000", placeholder: "Comment – required when deferring or rejecting" });
      var decide = function (decision) {
        action("decide", { accept: "Accepting ", defer: "Deferring ", reject: "Rejecting " }[decision] + r.number, { decision: decision, comment: comment.value });
      };
      body.appendChild(h("section.panel", null, [h("h2", { text: "Decision" }), comment,
        h("div.row", null, [
          h("button.primary", { type: "button", on: { click: function () { decide("accept"); } } }, "Accept"),
          r.status === "deferred" ? null : h("button", { type: "button", on: { click: function () { decide("defer"); } } }, "Defer"),
          h("button.danger", { type: "button", on: { click: function () { decide("reject"); } } }, "Reject")
        ])]));
    }

    append(body, h("div.grid-2", null, [
      h("section.panel", null, [h("h2", { text: "Details" }), h("dl.props", null, [
        dt("Problem and proposed solution"), dd(h("span.pre", { text: r.description || "—" })),
        dt("Reason"), dd(reasonLabel(r.reason)),
        dt("Priority"), dd(label(r.priority)),
        dt("Needed by"), dd(r.due_date || "—"),
        dt("Assignee"), dd(r.assignee_name || "—"),
        dt("Decision"), dd(r.decided_at ? r.decided_by_name + " · " + fmtDate(r.decided_at) + (r.decision_comment ? " – " + r.decision_comment : "") : "—")
      ])]),
      h("section.panel", null, [h("h2", { text: "Affected items" }),
        d.items.length ? table(["Item", "Name", "Latest", "Note"], d.items.map(function (i) {
          return {
            cells: [h("span.mono", { text: i.item_number + (i.revision_code ? "/" + i.revision_code : "") }), i.name,
              h("span", null, [h("span.mono", { text: i.latest_code + " " }), badge(i.latest_state)]), i.note],
            open: function () { openRevision(i.item_number, i.latest_code); }
          };
        })) : h("p.empty", { text: "No items named." })])
    ]));
    if (d.changes.length) {
      body.appendChild(h("section.panel", null, [h("h2", { text: "Change orders" }),
        table(["Change", "Title", "Status"], d.changes.map(function (c) {
          return { cells: [h("span.mono", { text: c.number }), c.title, badge(c.status)], open: function () { openChange(c.number); }, single: true };
        }))]));
    }

    var text = h("textarea", { maxlength: "10000", placeholder: "Findings, measurements, questions, a proposal …" });
    body.appendChild(h("section.panel", null, [h("h2", { text: "Discussion" }),
      d.comments.length ? h("ol.thread", null, d.comments.map(function (c) {
        return h("li.thread-entry", null, [
          h("div.thread-meta", { text: c.author_name + (c.author_id === r.requested_by ? " (requester)" : "") + " · " + fmtDate(c.created_at) }),
          h("div.pre", { text: c.body })
        ]);
      })) : h("p.empty", { text: "No comments yet." }),
      text,
      h("div.row", null, [h("button", { type: "button", on: { click: function () {
        if (!text.value.trim()) { text.focus(); return; }
        run("Adding the comment", function () { return api("POST", "/change-requests/" + enc(r.number) + "/comments", { body: text.value }); })
          .then(function () { reloadActive(); }, function () {});
      } } }, "Add comment")])
    ]));

    var images = d.attachments.filter(function (a) { return a.content_type.indexOf("image/") === 0; });
    body.appendChild(h("section.panel", null, [h("h2", { text: "Attachments" }),
      images.length ? h("div.attachment-thumbs", null, images.map(attachmentImage)) : null,
      d.attachments.length ? table(["File", "Size", "Uploaded"], d.attachments.map(function (a) {
        return {
          cells: [a.filename, fmtSize(a.size_bytes), a.uploaded_by_name + " · " + fmtDate(a.uploaded_at)],
          open: function () { call("open_web", { path: "/request-attachments/" + a.id + "?inline=1" }).catch(function (e) { toast(errorText(e), "error"); }); }
        };
      })) : h("p.empty", { text: "No attachments." })
    ]));
    body.appendChild(h("section.panel", null, [h("h2", { text: "Status history" }),
      d.history.length ? table(["When", "Who", "What"], d.history.map(function (e) {
        return { cells: [fmtDate(e.at), e.username, e.summary] };
      })) : h("p.empty", { text: "No history." })]));
  }

  // ======================================================================
  // Dialogs: new item, settings, about
  // ======================================================================

  /** `C:\cad\Bracket.ipt` → `Bracket.ipt`. */
  function baseName(path) {
    return String(path).split(/[\\/]/).pop();
  }

  var ASSEMBLY_EXT = ["iam", "asm", "sldasm", "catproduct", "f3z"];
  var DRAWING_EXT = ["idw", "dwg", "dxf", "dft", "slddrw", "catdrawing", "drw", "ipn", "pdf"];

  /** Name and item type suggested by a CAD file (Creo iterations ignored). */
  function guessFromFile(path) {
    var base = baseName(path).replace(/\.\d+$/, "");
    var dot = base.lastIndexOf(".");
    var ext = dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
    return {
      name: dot > 0 ? base.slice(0, dot) : base,
      type: ASSEMBLY_EXT.indexOf(ext) >= 0 ? "assembly" : DRAWING_EXT.indexOf(ext) >= 0 ? "document" : "part"
    };
  }

  function newItemDialog() {
    if (!canEdit()) { toast("Viewers cannot create items.", "error"); return; }
    var projects = state.projects.filter(function (p) { return p.active && canEditProject(p.id); });
    if (!projects.length) { toast("You are not a member of any project group – ask an admin to add you.", "error"); return; }
    var type = h("select", null, ITEM_TYPES.map(function (t) { return h("option", { value: t.value, text: t.label }); }));
    var project = h("select", null, projects.map(function (p) { return h("option", { value: p.code, text: p.code + " – " + p.name }); }));
    var name = h("input", { type: "text", maxlength: "200", required: true, placeholder: "from the first file" });
    var number = h("input", { type: "text", maxlength: "64", placeholder: "automatic (PRT-000042 …)" });
    var material = h("input", { type: "text", maxlength: "200" });
    var mass = h("input", { type: "text", inputmode: "decimal" });
    var desc = h("textarea", { maxlength: "10000" });
    // The item's files: chosen first, name and type follow from the first one.
    var paths = [];
    var typeChosen = false;
    type.addEventListener("change", function () { typeChosen = true; });
    var fileList = h("ul.file-picks");
    function showFiles() {
      fileList.textContent = "";
      paths.forEach(function (p, i) {
        fileList.appendChild(h("li", null, [h("span.mono", { text: baseName(p) }),
          h("button.small", { type: "button", title: "Remove", on: { click: function () { paths.splice(i, 1); showFiles(); } } }, "×")]));
      });
    }
    var pick = TAURI.dialog ? h("button", { type: "button", on: { click: function () {
      TAURI.dialog.open({ multiple: true, title: "Files of the new item" }).then(function (chosen) {
        if (!chosen) return;
        (Array.isArray(chosen) ? chosen : [chosen]).forEach(function (p) { if (paths.indexOf(p) < 0) paths.push(p); });
        showFiles();
        var guess = guessFromFile(paths[0]);
        if (!name.value.trim()) name.value = guess.name;
        if (!typeChosen) type.value = guess.type;
      });
    } } }, "Choose files…") : null;
    dialog("New item", h("div.stack", null, [
      field("Files", h("div.stack", null, [pick, fileList]), "the CAD model with its drawing and a STEP export for the 3D preview – uploaded right after the item is created"),
      h("div.grid-2", null, [field("Type", type), field("Project group", project)]),
      field("Name", name), field("Number", number, "optional"),
      h("div.grid-2", null, [field("Material", material), field("Mass per unit (kg)", mass)]),
      field("Description", desc)
    ]), [
      { label: "Cancel", onClick: function (d) { d.close(); } },
      {
        label: "Create", primary: true, submit: true, onClick: function (d) {
          var m = mass.value.trim().replace(",", ".");
          var body = {
            item_type: type.value, name: name.value, project: project.value,
            number: number.value.trim() || null, material: material.value, description: desc.value,
            mass_kg: m ? Number(m) : null
          };
          if (m && !isFinite(body.mass_kg)) { d.fail("Mass: '" + mass.value + "' is not a number."); return; }
          api("POST", "/items", body).then(function (rev) {
            if (!paths.length) return rev;
            setStatus("Uploading " + paths.length + (paths.length === 1 ? " file" : " files") + " …");
            return call("upload_files", { item: rev.item_number, code: rev.code, paths: paths, comment: null }).then(function () {
              setStatus("");
              return rev;
            }, function (err) {
              setStatus("");
              toast("Created " + rev.item_number + "/" + rev.code + ", but the files could not be uploaded: " + errorText(err), "error");
              return rev;
            });
          }).then(function (rev) {
            d.close();
            toast("Created " + rev.item_number + "/" + rev.code + (paths.length ? " with " + paths.length + (paths.length === 1 ? " file" : " files") : "") + " – it is checked out to you");
            openRevision(rev.item_number, rev.code);
            delete state.expanded[body.project];
            if (state.view === "home") selectView("home");
            refreshWorklistCount();
          }, function (err) { d.fail(err); });
        }
      }
    ], { wide: true });
  }

  // ------------------------------------------- whole project groups locally

  /** Every item of a project group (all pages of the item list). */
  function projectItems(code) {
    var all = [];
    function page(offset) {
      return api("GET", "/items?project=" + enc(code) + "&limit=200&offset=" + offset).then(function (r) {
        all = all.concat(r.items);
        return r.items.length && all.length < r.total ? page(all.length) : all;
      });
    }
    return page(0);
  }

  /** For new team members or a fresh computer: copies the files of every item
   * of the chosen project groups into the workspace, one folder per group. */
  function getProjectsDialog() {
    function member(p) { return state.me.projects.indexOf(p.id) !== -1; }
    var groups = state.projects.filter(function (p) { return p.active; }).sort(function (a, b) {
      return (member(b) - member(a)) || a.code.localeCompare(b.code);
    });
    var boxes = groups.map(function (p) {
      return { code: p.code, input: h("input", { type: "checkbox", checked: member(p) }), label: p.code + " – " + p.name + (member(p) ? " (member)" : "") };
    });
    var rule = h("select", null, [
      h("option", { value: "released", text: "Latest released revision – items without a release are skipped" }),
      h("option", { value: "latest", text: "Latest revision, also in work" })
    ]);
    var bar = h("progress.hidden", { max: "1", value: "0" });
    var report = h("p.small", { role: "status" });
    var stop = false, running = false;
    dialog("Get project groups", h("div.stack", null, [
      h("p", { text: "Copies the files of every item of the chosen project groups into your workspace – one folder per group: " +
        state.session.workspace_dir + "/<group>/<item>/<revision>/. Unchanged files are skipped, files you changed locally are kept." }),
      groups.length ? h("div.stack", null, boxes.map(function (b) { return h("label.check", null, [b.input, b.label]); }))
        : h("p.empty", { text: "There are no project groups yet." }),
      field("Revision", rule),
      bar, report
    ]), [
      { label: "Close", onClick: function (d) { stop = true; d.close(); } },
      {
        label: "Get files", primary: true, submit: true, onClick: function (d) {
          if (running) return;
          var codes = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.code; });
          if (!codes.length) { d.fail("Choose at least one project group."); return; }
          running = true; stop = false;
          var done = { items: 0, files: 0, kept: 0, skipped: 0, errors: [] };
          report.textContent = "Listing the items …";
          Promise.all(codes.map(function (code) {
            return projectItems(code).then(function (items) { return items.map(function (it) { return { project: code, item: it }; }); });
          })).then(function (lists) {
            var jobs = [].concat.apply([], lists);
            bar.max = Math.max(jobs.length, 1); bar.value = 0; bar.classList.remove("hidden");
            return jobs.reduce(function (chain, job, i) {
              return chain.then(function () {
                if (stop) return;
                var code = rule.value === "released" ? job.item.released_code : job.item.latest_code;
                bar.value = i + 1;
                report.textContent = "Item " + (i + 1) + " of " + jobs.length + ": " + job.project + " / " + job.item.number;
                if (!code) { done.skipped++; return; }
                return call("workspace_download", { project: job.project, item: job.item.number, code: code }).then(function (r) {
                  done.items++; done.files += r.downloaded.length; done.kept += r.kept.length;
                }, function (err) { done.errors.push(job.item.number + ": " + errorText(err)); });
              });
            }, Promise.resolve());
          }).then(function () {
            running = false;
            var text = (stop ? "Stopped. " : "Done. ") + done.items + " items, " + done.files + " files copied" +
              (done.kept ? ", " + done.kept + " locally modified files kept" : "") +
              (done.skipped ? ", " + done.skipped + " items without a released revision skipped" : "") +
              (done.errors.length ? ", " + done.errors.length + " errors: " + done.errors.slice(0, 5).join("; ") : "") + ".";
            report.textContent = text;
            toast(text, done.errors.length ? "error" : null);
          }, function (err) { running = false; d.fail(err); });
        }
      }
    ], { wide: true });
  }

  // ------------------------------------------- import of a CAD folder tree
  // (docs/src/research/cad-folder-import.md)

  var formatKindsPromise = null;
  /** File extension → format kind ("part", "assembly", "drawing", …) from the server's format list. */
  function formatKinds() {
    if (!formatKindsPromise) {
      formatKindsPromise = api("GET", "/formats").then(function (r) {
        var kinds = {};
        r.systems.forEach(function (s) {
          s.formats.forEach(function (f) {
            f.extensions.forEach(function (e) { if (!kinds[e]) kinds[e] = f.kind; });
          });
        });
        return kinds;
      }, function (err) { formatKindsPromise = null; throw err; });
    }
    return formatKindsPromise;
  }

  var ITEM_NUMBER = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
  /** Kinds that become an item of their own (grouped by base name). */
  var GROUPED_KINDS = ["part", "assembly", "model", "drawing", "exchange", "mesh", "visualization", "electronics"];

  function splitName(name) {
    var dot = name.lastIndexOf(".");
    return dot > 0 ? { stem: name.slice(0, dot), ext: name.slice(dot + 1).toLowerCase() } : { stem: name, ext: "" };
  }
  function folderOf(rel) { return rel.split("/").slice(0, -1).join("/"); }

  /** What the import will create: one item per part/assembly file with the
   * drawings and exports of the same base name; the rest in one document item. */
  function planImport(scan, kinds, opts) {
    var byName = {}, duplicates = [];
    scan.files.forEach(function (f) {
      // Creo: only the newest iteration, under its logical name (bracket.prt.7 → bracket.prt)
      var logical = f.name, iteration = 0;
      var m = /^(.+\.([a-z0-9]+))\.(\d+)$/i.exec(f.name);
      if (m && kinds[m[2].toLowerCase()]) { logical = m[1]; iteration = Number(m[3]); }
      var key = logical.toLowerCase(), seen = byName[key];
      if (!seen) { byName[key] = { file: f, logical: logical, iteration: iteration }; return; }
      if (folderOf(seen.file.rel) === folderOf(f.rel)) {
        if (iteration > seen.iteration) byName[key] = { file: f, logical: logical, iteration: iteration };
      } else {
        duplicates.push(f.rel + " – same name as " + seen.file.rel);
      }
    });
    var entries = Object.keys(byName).map(function (k) { return byName[k]; })
      .sort(function (a, b) { return a.file.rel.localeCompare(b.file.rel); });
    var groups = {}, order = [], extras = [];
    entries.forEach(function (e) {
      var n = splitName(e.logical), kind = kinds[n.ext];
      if (GROUPED_KINDS.indexOf(kind) < 0) return;
      var key = n.stem.toLowerCase();
      if (!groups[key]) { groups[key] = { name: n.stem, files: [], kinds: [] }; order.push(key); }
      groups[key].files.push(e.file);
      groups[key].kinds.push(kind);
    });
    entries.forEach(function (e) {
      var n = splitName(e.logical);
      if (GROUPED_KINDS.indexOf(kinds[n.ext]) >= 0) return;
      var g = groups[n.stem.toLowerCase()];
      if (g) g.files.push(e.file); else extras.push(e.file);
    });
    var items = order.map(function (key) {
      var g = groups[key];
      return {
        key: key, name: g.name, files: g.files,
        type: g.kinds.indexOf("assembly") >= 0 ? "assembly"
          : g.kinds.some(function (k) { return k !== "drawing"; }) ? "part" : "document",
        number: opts.numbersFromNames && ITEM_NUMBER.test(g.name) ? g.name.toUpperCase() : null
      };
    });
    if (extras.length && opts.extras) {
      items.push({ key: "\u0000project-files", name: "Project files – " + scan.folder_name, type: "document", files: extras, number: null });
    }
    return { items: items, duplicates: duplicates, extras: extras.length, ignored: scan.ignored, files: scan.files.length };
  }

  /** Items already imported from a folder (to continue an interrupted import). */
  function importLog(folder) {
    try { return JSON.parse(window.localStorage.getItem("teamcraft.import:" + state.session.server_url + ":" + folder)) || {}; }
    catch (e) { return {}; }
  }
  function saveImportLog(folder, log) {
    try { window.localStorage.setItem("teamcraft.import:" + state.session.server_url + ":" + folder, JSON.stringify(log)); }
    catch (e) { /* the import still works, it just cannot be continued */ }
  }

  /** BOM lines for the parts the new assemblies reference (quantity 1). */
  function bomFromReferences(created, progress) {
    var lines = 0;
    var assemblies = created.filter(function (c) { return c.item.type === "assembly"; });
    return assemblies.reduce(function (chain, c, i) {
      return chain.then(function () {
        progress(i + 1, assemblies.length, "Structure of " + c.rev.item_number);
        var base = "/items/" + enc(c.rev.item_number) + "/revisions/" + enc(c.rev.code);
        return api("GET", base + "/references").then(function (refs) {
          var children = [];
          (refs.files || []).forEach(function (f) {
            if (f.format_kind !== "assembly") return;
            f.references.forEach(function (r) {
              var m = r.matches.filter(function (x) { return !x.same_revision; })[0];
              if (m && m.item_number !== c.rev.item_number && children.indexOf(m.item_number) < 0) children.push(m.item_number);
            });
          });
          return children.reduce(function (ch, child) {
            return ch.then(function () {
              return api("POST", base + "/bom", { child: child, quantity: 1, notes: "From the CAD references – check the quantity" })
                .then(function () { lines++; }, function () { /* e.g. it would be circular */ });
            });
          }, Promise.resolve());
        }, function () { /* no references */ });
      });
    }, Promise.resolve()).then(function () { return lines; });
  }

  function importFolderDialog() {
    if (!canEdit()) { toast("Viewers cannot import.", "error"); return; }
    var projects = state.projects.filter(function (p) { return p.active && canEditProject(p.id); });
    if (!projects.length) { toast("You are not a member of any project group – ask an admin to add you.", "error"); return; }
    var folder = h("input", { type: "text", spellcheck: "false", placeholder: "Folder with the CAD files" });
    var pick = TAURI.dialog ? h("button", { type: "button", on: { click: function () {
      TAURI.dialog.open({ directory: true, title: "CAD folder to import" }).then(function (dir) { if (dir) { folder.value = dir; scanned = null; report.textContent = ""; } });
    } } }, "Choose…") : null;
    var project = h("select", null, projects.map(function (p) { return h("option", { value: p.code, text: p.code + " – " + p.name }); }));
    var fromNames = h("input", { type: "checkbox" });
    var extras = h("input", { type: "checkbox", checked: true });
    var bom = h("input", { type: "checkbox", checked: true });
    var report = h("div.stack");
    var bar = h("progress.hidden", { max: "1", value: "0" });
    var status = h("p.small", { role: "status" });
    var scanned = null, kinds = null, plan = null, running = false, stop = false;

    function options() { return { numbersFromNames: fromNames.checked, extras: extras.checked }; }
    function progress(done, total, text) { bar.classList.remove("hidden"); bar.max = Math.max(total, 1); bar.value = done; status.textContent = text; }

    function showPlan() {
      plan = planImport(scanned, kinds, options());
      var log = importLog(folder.value.trim());
      var open = plan.items.filter(function (it) { return !log[it.key]; });
      report.textContent = "";
      append(report, [
        h("p", null, [h("strong", { text: plan.files + " files" }), " → " + plan.items.length + " items (" +
          plan.items.filter(function (it) { return it.type === "assembly"; }).length + " assemblies, " +
          plan.items.filter(function (it) { return it.type === "part"; }).length + " parts, " +
          plan.items.filter(function (it) { return it.type === "document"; }).length + " documents)" +
          (plan.items.length - open.length ? " – " + (plan.items.length - open.length) + " already imported, skipped" : "") +
          (plan.ignored ? " · " + plan.ignored + " lock/backup files left out" : "") +
          (plan.extras && !extras.checked ? " · " + plan.extras + " other files not imported" : "")]),
        plan.duplicates.length ? h("div.notice", null, [h("strong", { text: plan.duplicates.length + " duplicate file names – only the first file of each name is imported. Rename them in the CAD program (TeamCraft recognises CAD references by file name):" }),
          h("ul.small", null, plan.duplicates.slice(0, 20).map(function (d) { return h("li", { text: d }); }))]) : null,
        table(["Item", "Type", "Number", "Files"], open.slice(0, 200).map(function (it) {
          return { cells: [it.name, label(it.type), it.number || h("span.dim", { text: "generated" }),
            h("span.small", { text: it.files.map(function (f) { return f.name; }).join(", ") })] };
        })),
        open.length > 200 ? h("p.small.dim", { text: "… and " + (open.length - 200) + " more items" }) : null
      ]);
    }

    function scanNow(d) {
      var path = folder.value.trim();
      if (!path) { d.fail("Choose the folder first."); return; }
      status.textContent = "Scanning …";
      Promise.all([call("scan_folder", { path: path }), formatKinds()]).then(function (r) {
        scanned = r[0]; kinds = r[1];
        status.textContent = "Scanned – nothing is uploaded yet. Check the list, then Import.";
        showPlan();
      }, function (err) { status.textContent = ""; d.fail(err); });
    }
    [fromNames, extras].forEach(function (box) { box.addEventListener("change", function () { if (scanned && !running) showPlan(); }); });

    function importNow(d) {
      if (running) return;
      if (!plan) { d.fail("Scan the folder first."); return; }
      var path = folder.value.trim(), log = importLog(path);
      var todo = plan.items.filter(function (it) { return !log[it.key]; });
      var created = [], failed = [], files = 0, checkedIn = 0;
      running = true; stop = false;
      todo.reduce(function (chain, it, i) {
        return chain.then(function () {
          if (stop) return;
          progress(i + 1, todo.length, "Item " + (i + 1) + " of " + todo.length + ": " + it.name);
          return api("POST", "/items", { item_type: it.type, name: it.name, project: project.value, number: it.number }).then(function (rev) {
            // remembered at once, so a second run never creates it twice
            log[it.key] = rev.item_number; saveImportLog(path, log);
            created.push({ rev: rev, item: it });
            return call("upload_files", { item: rev.item_number, code: rev.code, paths: it.files.map(function (f) { return f.path; }), comment: "Imported from " + scanned.folder_name })
              .then(function () { files += it.files.length; },
                function (err) { failed.push(rev.item_number + " (" + it.name + "): files not uploaded – " + errorText(err)); });
          }, function (err) { failed.push(it.name + ": " + errorText(err)); });
        });
      }, Promise.resolve()).then(function () {
        return bom.checked ? bomFromReferences(created, progress) : 0;
      }).then(function (lines) {
        // nothing stays locked: every new item is checked in
        return created.reduce(function (chain, c, i) {
          return chain.then(function () {
            progress(i + 1, created.length, "Checking in " + c.rev.item_number);
            return api("POST", "/items/" + enc(c.rev.item_number) + "/revisions/" + enc(c.rev.code) + "/checkin")
              .then(function () { checkedIn++; },
                function (err) { failed.push(c.rev.item_number + ": check-in – " + errorText(err)); });
          });
        }, Promise.resolve()).then(function () { return lines; });
      }).catch(function (err) {
        failed.push(errorText(err));
        return 0;
      }).then(function (lines) {
        running = false;
        bar.classList.add("hidden");
        status.textContent = "";
        if (created.length) { delete state.expanded[project.value]; if (state.view === "home") selectView("home"); refreshWorklistCount(); }
        d.close();
        importFinishedDialog({
          stopped: stop, created: created, files: files, skipped: plan.items.length - todo.length,
          bom: bom.checked, lines: lines, checkedIn: checkedIn, failed: failed,
          folder: scanned.folder_name, project: project.value
        });
      });
    }

    dialog("Import a CAD folder", h("div.stack", null, [
      h("p", null, ["Brings an existing folder of CAD data into TeamCraft: one item per part or assembly, with the drawings and exports of the same name. The folder itself is not changed. ",
        h("a", { href: "#", on: { click: function (e) { e.preventDefault(); importTutorial(); } } }, "How it works")]),
      field("Folder", h("div.row", null, [folder, pick])),
      field("Project group", project),
      h("label.check", null, [fromNames, "Use file names as item numbers where they are valid – otherwise numbers are generated"]),
      h("label.check", null, [extras, "Put project files (.ipj …) and other documents into one document item"]),
      h("label.check", null, [bom, "Create BOM lines from the references found in the assemblies (quantity 1 – check them)"]),
      report, bar, status
    ]), [
      { label: "Close", onClick: function (d) { stop = true; d.close(); } },
      { label: "Scan", onClick: function (d) { if (!running) scanNow(d); } },
      { label: "Import", primary: true, submit: true, onClick: importNow }
    ], { wide: true });
  }

  /** The report at the end of a folder import (finished, stopped or with
   * problems) – like the upload report of an autoloader. */
  function importFinishedDialog(r) {
    var ok = !r.failed.length && !r.stopped;
    var title = r.stopped ? "Import stopped" : r.failed.length ? "Import finished with problems" : "Import finished";
    function count(type) { return r.created.filter(function (c) { return c.item.type === type; }).length; }
    // the assembly to open: the one named like the folder, else the first new one
    var assemblies = r.created.filter(function (c) { return c.item.type === "assembly"; });
    var top = assemblies.filter(function (c) { return c.item.name.toLowerCase() === String(r.folder).toLowerCase(); })[0] || assemblies[0];
    var body = h("div.stack.import-report", null, [
      h("p.import-report-head" + (ok ? ".ok" : ".warn"), { role: "status", text: (ok ? "✓ " : "⚠ ") + title }),
      h("ul", null, [
        h("li", { text: r.created.length + " new items in " + r.project + " – " + count("assembly") + " assemblies, " +
          count("part") + " parts, " + count("document") + " documents – with " + r.files + " files" }),
        r.skipped ? h("li", { text: r.skipped + " items were imported before and skipped" }) : null,
        r.bom ? h("li", { text: r.lines + " BOM lines from the CAD references – quantity 1, please check them" }) : null,
        h("li", { text: r.checkedIn === r.created.length
          ? "Every new item is checked in – nothing is locked"
          : r.checkedIn + " of " + r.created.length + " new items checked in – the others are still checked out to you" }),
        r.stopped ? h("li", { text: "You stopped the import – scan and import the folder again to continue where it stopped." }) : null
      ]),
      r.failed.length ? h("div.notice", null, [h("strong", { text: r.failed.length + (r.failed.length === 1 ? " problem:" : " problems:") }),
        h("ul.small", null, r.failed.map(function (f) { return h("li", { text: f }); }))]) : null,
      h("p.small.dim", { text: "Next: check the structure and the quantities, then release the product with one change order (Help → How to Import a CAD Folder)." })
    ]);
    var actions = [];
    if (top) actions.push({ label: "Open " + top.rev.item_number, onClick: function (d) { d.close(); openRevision(top.rev.item_number, top.rev.code); } });
    actions.push({ label: "Close", primary: true, submit: true, onClick: function (d) { d.close(); } });
    dialog(title, body, actions, { wide: true });
    toast(title + ": " + r.created.length + " items, " + r.files + " files", ok ? null : "error");
  }

  /** The tutorial: how to bring existing CAD data into TeamCraft. */
  function importTutorial() {
    function step(title, points) {
      return h("section.stack", null, [h("h3", { text: title }), h("ul", null, points.map(function (p) { return h("li", { text: p }); }))]);
    }
    dialog("How to import a CAD folder", h("div.stack.tutorial", null, [
      h("p", { text: "Like Autodesk Vault's Autoloader or the data preparation of Teamcenter: prepare the data, scan it (nothing is uploaded), then import. Hundreds of files are fine." }),
      step("1 · Prepare the data in the CAD program", [
        "Collect each product into one clean folder with Pack and Go – Inventor: File → Save As → Pack and Go (with the project file .ipj); Solid Edge: Pack and Go in Design Manager or Revision Manager; SolidWorks: File → Pack and Go.",
        "Open the top assembly once and fix missing references – broken links stay broken after the import.",
        "Give every file a unique name. TeamCraft, like Teamcenter, recognises CAD references by file name: two different Bracket.ipt in two subfolders are reported as duplicates and only the first one is imported.",
        "Optional: export STEP files next to the models for the 3D preview – or let the app create them later at check-in (Windows, with Inventor or Solid Edge)."
      ]),
      step("2 · Scan", [
        "Tools → Import CAD Folder…: choose the folder and the project group, then Scan. The report lists the items that will be created, duplicate names and the files left out (lock and backup files, OldVersions folders).",
        "One item per part or assembly file; its drawing and exports (STEP, STL, PDF …) with the same name go into the same item. Project files (.ipj …) and other documents go into one document item.",
        "Item numbers are generated (PRT-…, ASM-…) – or taken from the file names when they are valid item numbers."
      ]),
      step("3 · Import", [
        "Import creates the items and uploads the files, item by item, with progress. The source folder is only read.",
        "Stop finishes the current item. The app remembers what was imported: scan and import again to continue.",
        "At the end every new item is checked in, so nothing stays locked.",
        "When it is done, a report shows the new items and files, the BOM lines and any problems – with a button that opens the top assembly."
      ]),
      step("4 · Check and release", [
        "Structure: assemblies get BOM lines for the parts they reference, with quantity 1. For exact quantities export the structured BOM from the CAD program (CSV or Excel) and use Import BOM on the assembly.",
        "Look at the preview images and 3D views, fix what is missing, then release the product with one change order.",
        "Team members get the files with Get… (Tools → Get Project Groups…) – one folder per project group."
      ])
    ]), [{ label: "Close", primary: true, onClick: function (d) { d.close(); } }], { wide: true });
  }

  function openSettings() {
    var current = document.documentElement.getAttribute("data-skin");
    var ws = h("input", { type: "text", value: state.session.workspace_dir, spellcheck: "false" });
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
      h("h3", { text: "Skin" }), skins,
      h("h3", { text: "Workspace" }),
      field("Folder for checked-out files", h("div.row", null, [ws,
        TAURI.dialog ? h("button", { type: "button", on: { click: function () {
          TAURI.dialog.open({ directory: true, defaultPath: ws.value }).then(function (dir) { if (dir) ws.value = dir; });
        } } }, "Choose…") : null])),
      h("h3", { text: "Account" }),
      h("p", null, ["Signed in to ", h("strong", { text: state.session.server_url }), " as ", h("strong", { text: state.me.display_name }), " (" + label(state.me.role) + ") on ", h("span", { text: state.session.device })]),
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

  function signOut(changeServer) {
    call("sign_out").then(function () {
      state.session.signed_in = false;
      state.tabs = [];
      state.active = null;
      state.me = null;
      showConnect(changeServer ? "server" : "signin");
    }, function (e) { toast(errorText(e), "error"); });
  }

  function openAbout() {
    dialog("About TeamCraft", h("div.stack", null, [
      h("img.connect-logo.skin-logo-dark", { src: "/static/media/logos/TeamCraft_Logo_White.svg", alt: "TeamCraft" }),
      h("img.connect-logo.skin-logo-light", { src: "/static/media/logos/TeamCraft_Logo_Blue.svg", alt: "TeamCraft" }),
      h("p", { text: "TeamCraft Desktop " + state.session.version + " – the rich client of TeamCraft, the open-source product lifecycle management." }),
      h("p.small", { text: "Pre-release: TeamCraft is a work in progress and has no official release yet. It is provided as is, without warranty of any kind (MPL-2.0, sections 6 and 7). Keep backups of your data." }),
      h("p.small.muted", { text: "TeamCraft is open source under the Mozilla Public License 2.0. Fonts: Montserrat and Oswald (SIL Open Font License 1.1)." }),
      h("p.small.muted", { text: "Server: " + (state.session.server_url || "—") })
    ]), [{ label: "Close", primary: true, onClick: function (d) { d.close(); } }]);
  }

  // ======================================================================
  // Zoom: Ctrl + mouse wheel (trackpads: pinch), Ctrl/Cmd + + / - / 0
  // ======================================================================

  var ZOOM_MIN = 0.5, ZOOM_MAX = 2.5;

  function setZoom(level) {
    level = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, level));
    if (Math.abs(level - state.zoom) < 0.0005) return;
    state.zoom = level;
    showZoom();
    invoke("set_zoom", { level: level }).catch(function (e) { toast(errorText(e), "error"); });
  }

  function showZoom() {
    var el = $("status-zoom");
    el.textContent = Math.round(state.zoom * 100) + " %";
    el.classList.toggle("hidden", Math.abs(state.zoom - 1) < 0.005);
  }

  function initZoom() {
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

  function handleMenu(id) {
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
      case "import-help": importTutorial(); break;
      case "settings": case "settings-app": openSettings(); break;
      case "sign-out": signOut(); break;
      case "refresh": selectView(state.view); reloadActive(); refreshWorklistCount(); break;
      case "search": $("search-input").focus(); $("search-input").select(); break;
      case "go-home": selectView("home"); break;
      case "go-worklist": selectView("worklist"); break;
      case "go-changes": selectView("changes"); break;
      case "open-workspace": openWorkspaceFolder(activeTab() && activeTab().kind === "revision" ? activeTab() : null); break;
      case "open-web": {
        var t = activeTab();
        var path = !t || !t.data ? "/" : t.kind === "change" ? "/changes/" + t.data.change.id : "/revisions/" + t.data.rev.id;
        call("open_web", { path: path }).catch(function (e) { toast(errorText(e), "error"); });
        break;
      }
    }
  }

  // ======================================================================
  // Start
  // ======================================================================

  function boot() {
    initConnect();
    initShell();
    initZoom();
    TAURI.event.listen("menu", function (e) { handleMenu(e.payload); });
    call("session").then(function (s) {
      state.session = s;
      state.zoom = s.zoom || 1;
      showZoom();
      applySkin(s.skin);
      if (!s.server_url) showConnect("server");
      else if (!s.signed_in) showConnect("signin");
      else enterShell();
    }, function (err) {
      document.body.appendChild(h("p.notice.error", { text: "TeamCraft could not start: " + errorText(err) }));
    });
  }

  boot();
})();
