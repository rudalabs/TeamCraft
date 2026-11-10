// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Crash reports. Uncaught errors of the user interface go
// to the local report store (src/crash.rs, next to panics and sessions that
// ended unexpectedly); at the next start the user sees what an earlier session
// left behind and decides: report it on GitHub, copy it, or delete it.
// Nothing is ever sent without that decision.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { call, dialog, h, invoke, state, toast } from "./core.js";

var open = false;

/** Records uncaught JavaScript exceptions. Only real `Error`s count: a
 *  rejected command call (a server's 404 or 403, a cancelled dialog) is no
 *  crash, and "Script error." / ResizeObserver notices carry no error object. */
export function initCrashWatch() {
  window.addEventListener("error", function (e) {
    var err = e.error;
    if (!(err instanceof Error)) return;
    record(err, e.filename ? e.filename.replace(/^.*\/js\//, "js/") + ":" + e.lineno + ":" + e.colno : "");
  });
  window.addEventListener("unhandledrejection", function (e) {
    if (e.reason instanceof Error) record(e.reason, "unhandled promise rejection");
  });
}

function record(err, source) {
  invoke("record_ui_error", {
    message: (err.name || "Error") + ": " + (err.message || ""),
    stack: err.stack ? String(err.stack) : null,
    source: source || null
  }).catch(function () { /* the store is gone: nothing to do */ });
}

/** Shows the crash reports of an earlier session, once, unless the user
 *  switched the question off (Settings → Crash reports). */
export function checkCrashReports() {
  if (open) return;
  call("crash_reports").then(function (r) {
    if (!r || !r.ask || open) return;
    open = true;
    crashDialog(r);
  }, function () { /* no store: nothing to show */ });
}

function summary(kinds) {
  var n = function (k) { return kinds.filter(function (x) { return x === k; }).length; };
  var parts = [];
  if (n("unexpected-exit")) parts.push(n("unexpected-exit") === 1 ? "the last session ended unexpectedly" : n("unexpected-exit") + " sessions ended unexpectedly");
  if (n("panic")) parts.push(n("panic") === 1 ? "the app had an internal error" : "the app had " + n("panic") + " internal errors");
  if (n("ui-error")) parts.push("the user interface ran into errors");
  var text = parts.join(", ");
  return text ? text.charAt(0).toUpperCase() + text.slice(1) + "." : "TeamCraft recorded an error.";
}

function copyText(text, area) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    return navigator.clipboard.writeText(text).catch(function () { return copyBySelection(area); });
  }
  return copyBySelection(area);
}

function copyBySelection(area) {
  area.focus();
  area.select();
  return document.execCommand("copy") ? Promise.resolve() : Promise.reject(new Error("The clipboard is not available – select the text and copy it."));
}

function crashDialog(r) {
  var crashed = r.kinds.some(function (k) { return k === "panic" || k === "unexpected-exit"; });
  var area = h("textarea.crash-text", { readonly: true, rows: "12", spellcheck: "false", "aria-label": "Crash report" });
  area.value = r.text;
  var never = h("input", { type: "checkbox" });
  function finish(d) {
    open = false;
    if (never.checked) {
      call("set_crash_prompt", { enabled: false }).then(function () {
        if (state.session) state.session.crash_prompt = false;
      }, function () { /* asked again next time */ });
    }
    d.close();
  }
  dialog(crashed ? "TeamCraft closed unexpectedly" : "TeamCraft ran into an error", h("div.stack", null, [
    h("p", { text: summary(r.kinds) + " You can share the report with the TeamCraft developers so they can find and fix the cause." }),
    h("p.hint", { text: "Nothing is sent automatically. “Report on GitHub” opens a new issue in your browser with the text below – you review it and submit it yourself (a GitHub account is needed); the full text is also copied to the clipboard. The report holds the app version, the operating system and the error. Your home folder, server addresses and tokens are removed, and no item data or files are included." }),
    area,
    h("label.check", null, [never, "Don't ask again after a crash (Settings → Crash reports)"])
  ]), [
    { label: "Delete report", onClick: function (d) {
      call("crash_reports_discard").catch(function () {});
      finish(d);
    } },
    { label: "Copy", onClick: function (d) {
      copyText(r.text, area).then(function () { toast("Crash report copied to the clipboard"); }, function (e) { d.fail(e); });
    } },
    { label: "Report on GitHub…", primary: true, submit: true, onClick: function (d) {
      copyText(r.text, area).catch(function () { /* the link still carries the text */ })
        .then(function () { return call("crash_reports_open_issue"); })
        .then(function () {
          toast("Opened a new issue in your browser – the full report is also on the clipboard.");
          finish(d);
        }, function (e) { d.fail(e); });
    } }
  ], { wide: true });
  // closing with Esc or a click beside the dialog keeps the reports: asked again next start
  var root = document.getElementById("dialog-root");
  var watch = new MutationObserver(function () {
    if (!root.contains(area)) { open = false; watch.disconnect(); }
  });
  watch.observe(root, { childList: true, subtree: true });
}
