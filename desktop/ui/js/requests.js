// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Change requests (ECR).
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { REQUEST_REASONS, TAURI, api, append, badge, call, confirmDialog, dialog, enc, errorText, field, fmtDate, fmtSize, h, invoke, label, moreMenu, priorityBadge, run, setStatus, toast, tool, toolGroup } from "./core.js";
import { refreshWorklistCount, watchTool } from "./shell.js";
import { openTab, reloadActive, renderTabs, table } from "./tabs.js";
import { dd, dt, openRevision } from "./revision.js";
import { openChange } from "./change.js";
import { baseName } from "./dialogs.js";

// ======================================================================
// Change requests (ECR)
// ======================================================================

export function reasonLabel(value) {
  var r = REQUEST_REASONS.filter(function (x) { return x.value === value; })[0];
  return r ? r.label : label(value);
}

/** Lets the user choose a screenshot, mark-up or PDF; resolves to a path or null. */
export function chooseAttachment() {
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
export function requestChangeDialog(rev, filename) {
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

export function openRequest(number) {
  openTab({ key: "ecr:" + number, kind: "request", number: number, title: number });
}

export function loadRequest(tab) {
  if (tab.data) return Promise.resolve(tab.data);
  return api("GET", "/change-requests/" + enc(tab.number)).then(function (r) {
    tab.data = r;
    tab.title = r.change_request.number + " " + r.change_request.title;
    renderTabs();
    return r;
  });
}

export var attachmentCache = {};

export function attachmentImage(a) {
  var img = h("img.attachment-thumb.hidden", { alt: a.filename, title: a.filename });
  var url = attachmentCache[a.id] || (attachmentCache[a.id] = invoke("file_bytes", { kind: "request-attachment", id: a.id })
    .then(function (bytes) { return URL.createObjectURL(new Blob([bytes], { type: a.content_type })); }));
  url.then(function (u) { img.src = u; img.classList.remove("hidden"); }, function () { /* not available */ });
  return img;
}

export function attachToRequest(r) {
  chooseAttachment().then(function (path) {
    if (!path) return;
    run("Attaching " + baseName(path), function () { return call("upload_request_attachment", { number: r.number, path: path }); })
      .then(function (a) { toast("Attached " + a.filename + " to " + r.number); reloadActive(); }, function () {});
  }, function (e) { toast(errorText(e), "error"); });
}

export function createChangeFromRequest(tab) {
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

export function renderRequest(tab, body, toolbar) {
  var d = tab.data, r = d.change_request, can = d.can || {};
  function action(path, text, payload) {
    return run(text, function () { return api("POST", "/change-requests/" + enc(r.number) + "/" + path, payload === undefined ? null : payload); })
      .then(function () { toast(text + " – done"); reloadActive(); refreshWorklistCount().catch(function () {}); }, function () {});
  }
  var status = label(r.status);
  // one primary: start the triage, else turn an accepted request into its change order
  var next = can.start_triage ? "start_triage" : can.implement ? "implement" : null;
  append(toolbar, [
    toolGroup("Triage", [
      tool("Start triage", "checkout", function () { action("triage", "Starting the triage of " + r.number); }, {
        primary: next === "start_triage", disabled: !can.start_triage,
        title: can.start_triage ? "Take it up (in triage)" + (r.assignee ? "" : ", assigned to you") : "Only a new request is taken up, by the CAD editors of its project groups – this one is " + status
      }),
      tool("Take up again", "revise", function () { action("reopen", "Taking up " + r.number); }, {
        disabled: !can.reopen, title: can.reopen ? "Back into triage" : "Only a deferred request is taken up again, by the CAD editors of its project groups – this one is " + status
      }),
      tool("Create change order", "release", function () { createChangeFromRequest(tab); }, {
        primary: next === "implement", disabled: !can.implement,
        title: can.implement ? "Draft change order with the in-work revisions of the affected items" : "Possible once the request is accepted, for CAD editors of its project groups – this one is " + status
      })
    ]),
    toolGroup("Requester", [
      tool("Attach…", "plus", function () { attachToRequest(r); }, {
        disabled: !can.attach || !TAURI.dialog,
        title: can.attach ? "Attach a screenshot, mark-up or PDF" : "Attachments are added only while the request is open – this one is " + status
      }),
      tool("Withdraw", "cancel", function () { action("withdraw", "Withdrawing " + r.number); }, {
        disabled: !can.withdraw, title: can.withdraw ? "Take the request back" : "Only " + r.requested_by_name + " (or an administrator) can withdraw it, before it is decided"
      })
    ]),
    toolGroup("Collaborate", [watchTool(tab, "change-requests", r.number, !!d.watching)]),
    moreMenu([
      { label: "Open in browser", icon: "web", run: function () { call("open_web", { path: "/requests/" + r.id }).catch(function (e) { toast(errorText(e), "error"); }); },
        title: "Open this change request in the web UI (assign, link a change order, edit …)" },
      { label: "Refresh", icon: "refresh", run: reloadActive, shortcut: "F5", title: "Load this change request again" }
    ])
  ]);
  body.appendChild(h("div.object-head", null, [h("div", null, [
    h("h1", null, [h("span.mono", { text: r.number }), " · " + r.title]),
    h("div.meta", null, [badge(r.status), priorityBadge(r.priority), h("span", { text: "requested " + fmtDate(r.created_at) + " by " + r.requested_by_name })])
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
    var comment = h("textarea.review-comment", { id: "decision-comment", maxlength: "4000", "aria-describedby": "decision-comment-hint" });
    var decide = function (decision) {
      if (decision !== "accept" && !comment.value.trim()) {
        toast("Write in the comment why you " + decision + " " + r.number, "error");
        comment.focus();
        return;
      }
      action("decide", { accept: "Accepting ", defer: "Deferring ", reject: "Rejecting " }[decision] + r.number, { decision: decision, comment: comment.value });
    };
    body.appendChild(h("section.panel", null, [h("h2", { text: "Decision" }),
      h("label", { for: "decision-comment" }, ["Comment", comment]),
      h("p.hint.review-hint", { id: "decision-comment-hint", text: "Deferring or rejecting needs a comment: why, and what would change the decision." }),
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
