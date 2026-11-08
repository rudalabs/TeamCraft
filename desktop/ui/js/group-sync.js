// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Whole project groups locally.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { api, call, dialog, enc, errorText, field, h, label, state, toast } from "./core.js";
import { freshProjects } from "./shell.js";

// ------------------------------------------- whole project groups locally

/** Every item of a project group (all pages of the item list). */
export function projectItems(code) {
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
export function getProjectsDialog() { freshProjects().then(openGetProjectsDialog); }

export function openGetProjectsDialog() {
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
    h("p", { text: "Copies the files of every item of the chosen project groups into your working folder – one folder per group with the folder structure of the files: " +
      state.session.workspace_dir + "/<group>/<folder>/<file>. Unchanged files are skipped, files you changed locally are kept, " +
      "two items with the same folder and file name are reported (never overwritten). Files you have not checked out are read-only." }),
    groups.length ? h("div.stack", null, boxes.map(function (b) { return h("label.check", null, [b.input, b.label]); }))
      : h("p.empty", { text: "There are no project groups yet." }),
    field("Revision", rule),
    bar, report
  ]), [
    { label: "Close", onClick: function (d) { stop = true; if (running) call("workspace_cancel").catch(function () {}); d.close(); } },
    {
      label: "Get files", primary: true, submit: true, onClick: function (d) {
        if (running) return;
        var codes = boxes.filter(function (b) { return b.input.checked; }).map(function (b) { return b.code; });
        if (!codes.length) { d.fail("Choose at least one project group."); return; }
        running = true; stop = false;
        var done = { items: 0, files: 0, kept: 0, skipped: 0, clashes: 0, errors: [] };
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
                done.items++; done.files += r.downloaded.length; done.kept += r.kept.length; done.clashes += r.clashes.length;
                if (r.cancelled) stop = true;
                r.failed.forEach(function (t) { done.errors.push(job.item.number + ": " + t); });
                r.clashes.forEach(function (c) { done.errors.push(c.path + ": " + c.file + " – taken by " + c.with); });
              }, function (err) { done.errors.push(job.item.number + ": " + errorText(err)); });
            });
          }, Promise.resolve());
        }).then(function () {
          running = false;
          var text = (stop ? "Stopped. " : "Done. ") + done.items + " items, " + done.files + " files copied" +
            (done.kept ? ", " + done.kept + " locally modified files kept" : "") +
            (done.skipped ? ", " + done.skipped + " items without a released revision skipped" : "") +
            (done.clashes ? ", " + done.clashes + " name clashes (nothing overwritten)" : "") +
            (done.errors.length ? ", " + done.errors.length + " problems: " + done.errors.slice(0, 5).join("; ") : "") + ".";
          report.textContent = text;
          toast(text, done.errors.length ? "error" : null);
        }, function (err) { running = false; d.fail(err); });
      }
    }
  ], { wide: true });
}
