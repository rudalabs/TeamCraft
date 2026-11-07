// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Change tab.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { api, append, badge, call, clear, enc, errorText, fmtDate, h, label, lockBadge, moreMenu, priorityBadge, run, state, toast, tool, toolGroup } from "./core.js";
import { canEdit, refreshWorklistCount, watchTool } from "./shell.js";
import { openTab, recordLink, reloadActive, renderTabs, table } from "./tabs.js";
import { agentButton } from "./agent.js";
import { dd, dt, openRevision } from "./revision.js";
import { relationText } from "./references.js";

// ======================================================================
// Change tab
// ======================================================================

export function openChange(number) {
  openTab({ key: "chg:" + number, kind: "change", number: number, title: number });
}

export function loadChange(tab) {
  if (tab.data) return Promise.resolve(tab.data);
  return api("GET", "/changes/" + enc(tab.number)).then(function (r) {
    tab.data = r;
    tab.title = r.change.number + " " + r.change.title;
    renderTabs();
    return r;
  });
}

/**
 * Suggested reviewers of a draft change (GET …/suggested-reviewers): one
 * Add button per person with the reasons as text. Loads after the tab.
 */
export function suggestedReviewers(number) {
  var box = h("div.suggestions");
  api("GET", "/changes/" + enc(number) + "/suggested-reviewers").then(function (r) {
    if (!r.suggestions.length) return;
    box.appendChild(h("h3", { text: "Suggested reviewers" }));
    r.suggestions.forEach(function (s, i) {
      var whyId = "suggested-why-" + number + "-" + i;
      var why = h("p.hint", { id: whyId, text: s.reasons.join("; ") });
      var add = h("button.small", { type: "button", "aria-describedby": whyId, on: { click: function () {
        add.disabled = true;
        run("Adding " + s.display_name, function () {
          return api("POST", "/changes/" + enc(number) + "/reviewers", { username: s.username });
        }).then(function () { toast("Added reviewer " + s.display_name); reloadActive(); }, function () { add.disabled = false; });
      } } }, "Add " + s.display_name);
      box.appendChild(h("div.suggestion", null, [add, why]));
    });
  }, function () { /* suggestions are optional */ });
  return box;
}

export function renderChange(tab, body, toolbar) {
  var c = tab.data.change, me = state.me;
  var owner = canEdit() && (c.created_by === me.id || me.role === "admin");
  var myReview = tab.data.reviews.filter(function (r) { return r.reviewer_id === me.id; })[0];
  var canReview = c.status === "in_review" && myReview && myReview.decision === "pending";
  function action(path, text) {
    run(text, function () { return api("POST", "/changes/" + enc(c.number) + "/" + path); })
      .then(function () { toast(text + " – done"); reloadActive(); refreshWorklistCount(); }, function () {});
  }
  var notOwner = owner ? null : "Only " + c.created_by_name + " (who created it) or an administrator can do this";
  var canSubmit = owner && c.status === "draft", canWithdraw = owner && c.status === "in_review";
  // The review decision lives in the toolbar – the comment field below goes with it.
  // A visible label and hint as in the web review form (a placeholder would vanish once you type).
  var comment = canReview ? h("textarea.review-comment", { id: "review-comment", maxlength: "4000", "aria-describedby": "review-comment-hint" }) : null;
  function decide(decision) {
    if (decision === "reject" && !comment.value.trim()) {
      toast("Write in the comment why you reject " + c.number, "error");
      comment.focus();
      return;
    }
    run(decision === "approve" ? "Approving" : "Rejecting", function () {
      return api("POST", "/changes/" + enc(c.number) + "/review", { decision: decision, comment: comment.value });
    }).then(function (r) {
      toast(c.number + " – " + (decision === "approve" ? "approved" : "rejected") + "; status " + label(r.change.status));
      reloadActive(); refreshWorklistCount();
    }, function () {});
  }
  append(toolbar, [
    canReview ? toolGroup("Review", [
      tool("Approve change", "approve", function () { decide("approve"); }, { primary: true, title: "Approve " + c.number + " (with the comment below, if any)" }),
      tool("Reject", "cancel", function () { decide("reject"); }, { danger: true, title: "Reject " + c.number + " – the comment below says why; the revisions go back to WIP" })
    ]) : null,
    toolGroup("Lifecycle", [
      tool("Submit for review", "checkin", function () { action("submit", "Submitting " + c.number); }, {
        primary: canSubmit, disabled: !canSubmit,
        title: canSubmit ? "Send it to the reviewers" : notOwner || "Only a draft is submitted – this one is " + label(c.status)
      }),
      tool("Withdraw", "cancel", function () { action("withdraw", "Withdrawing " + c.number); }, {
        disabled: !canWithdraw,
        title: canWithdraw ? "Take it back from review – the revisions return to WIP" : notOwner || "Only a change in review can be withdrawn – this one is " + label(c.status)
      })
    ]),
    toolGroup("Collaborate", [
      watchTool(tab, "changes", c.number, !!tab.data.watching),
      agentButton(tab)
    ]),
    moreMenu([
      { label: "Open in browser", icon: "web", run: function () { call("open_web", { path: "/changes/" + c.id }).catch(function (e) { toast(errorText(e), "error"); }); }, title: "Open this change in the web UI (reviewers, items, cancel …)" },
      { label: "Refresh", icon: "refresh", run: reloadActive, shortcut: "F5", title: "Load this change again" }
    ])
  ]);
  body.appendChild(h("div.object-head", null, [h("div", null, [
    h("h1", null, [h("span.mono", { text: c.number }), " · " + c.title]),
    h("div.meta", null, [badge(c.status), priorityBadge(c.priority), h("span", { text: "created " + fmtDate(c.created_at) + " by " + c.created_by_name })])
  ])]));
  var hints = {
    draft: "Draft: add the revisions to release and at least one reviewer, then submit.",
    in_review: "In Review: all reviewers must approve; a single rejection sends the revisions back to WIP.",
    rejected: "Rejected: fix the revisions, then reopen and resubmit in the web UI – or cancel the change.",
    released: "Released: all revisions of this change are released."
  };
  if (hints[c.status]) body.appendChild(h("p.notice.info", { text: hints[c.status] }));

  if (canReview) {
    body.appendChild(h("section.panel", null, [h("h2", { text: "Your review" }),
      h("label", { for: "review-comment" }, ["Comment", comment]),
      h("p.hint.review-hint", { id: "review-comment-hint", text: "A rejection needs a comment: what has to change. Approve change and Reject are in the toolbar above." })]));
  }

  append(body, h("div.grid-2", null, [
    h("section.panel", null, [h("h2", { text: "Details" }), h("dl.props", null, [
      dt("Description"), dd(h("span.pre", { text: c.description || "—" })),
      dt("Submitted"), dd(fmtDate(c.submitted_at)),
      dt("Closed"), dd(fmtDate(c.closed_at)),
      dt("Last update"), dd(fmtDate(c.updated_at))
    ])]),
    h("section.panel", null, [h("h2", { text: "Reviewers" }),
      tab.data.reviews.length ? table(["Reviewer", "Decision", "Comment"], tab.data.reviews.map(function (r) {
        return { cells: [r.reviewer_name, badge(r.decision), h("span.pre", { text: r.comment || "" })] };
      }), { nowrap: [0, 1] }) : h("p.empty", { text: "No reviewers assigned yet." }),
      owner && c.status === "draft" ? suggestedReviewers(c.number) : null])
  ]));
  body.appendChild(h("section.panel", null, [h("h2", { text: "Affected revisions" }),
    tab.data.items.length ? table(["Revision", "Name", "State"], tab.data.items.map(function (i) {
      var open = function () { openRevision(i.item_number, i.code); };
      return { cells: [recordLink(i.item_number + "/" + i.code, open), i.name, h("span", null, [badge(i.state), i.checked_out_by_name ? lockBadge(i.checked_out_by_name, i.checked_out_by === state.me.id) : null])], open: open };
    })) : h("p.empty", { text: "No revisions yet." })]));
  // Release readiness: unreleased dependencies block the submit.
  if ((c.status === "draft" || c.status === "rejected") && tab.data.items.length) {
    var readyPanel = h("section.panel", null, [h("h2", { text: "Release readiness" }), h("p.muted.small", { text: "Checking …" })]);
    body.appendChild(readyPanel);
    api("GET", "/changes/" + enc(c.number) + "/readiness").then(function (r) {
      clear(readyPanel);
      var rows = r.blockers.concat(r.warnings);
      var addable = r.blockers.some(function (d) { return d.addable; });
      append(readyPanel, [
        h("div.row", null, [h("h2", { text: "Release readiness" }),
          owner && c.status === "draft" && addable ? h("button.primary.small", { type: "button", on: { click: function () {
            run("Adding the dependencies", function () { return api("POST", "/changes/" + enc(c.number) + "/dependencies"); })
              .then(function (res) { toast(res.added.length ? "Added " + res.added.join(", ") : "Nothing to add"); reloadActive(); }, function () {});
          } } }, "Add all to this change") : null]),
        r.ready
          ? h("p.notice.ok", { text: "Ready: every BOM line and reference is released or part of this change." })
          : h("p.notice.error", { text: r.blockers.length + (r.blockers.length === 1 ? " dependency is" : " dependencies are") + " not released – add them to this change or release them first, then submit." }),
        rows.length ? table(["Dependency", "Name", "Needed by", "State"], rows.map(function (d) {
          var note = d.other_change ? " in " + d.other_change : d.problem === "obsolete" ? " – obsolete, replace it" : !d.blocking ? " – found by the CAD file scan" : "";
          return {
            cells: [h("span.mono", { text: d.item_number + "/" + d.latest_code }), d.item_name,
              h("span", null, [h("span.mono", { text: d.revision_label }), " " + relationText(d.relation)]),
              h("span", null, [badge(d.latest_state), h("span.dim", { text: note })])],
            open: function () { openRevision(d.item_number, d.latest_code); }
          };
        })) : null
      ]);
    }, function (err) { clear(readyPanel); append(readyPanel, [h("h2", { text: "Release readiness" }), h("p.notice.error", { text: errorText(err) })]); });
  }
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
