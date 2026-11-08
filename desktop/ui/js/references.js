// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – References and change propagation; changes and history of a revision.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { api, badge, confirmDialog, dialog, enc, field, fmtDate, h, label, run, state, toast } from "./core.js";
import { canEditProject, refreshWorklistCount } from "./shell.js";
import { selectView } from "./navigator.js";
import { reloadActive, table } from "./tabs.js";
import { openRevision, revisionAbilities } from "./revision.js";
import { reviseRevision } from "./workspace.js";
import { openChange } from "./change.js";

// ---------------------------------------------------------------- references and change propagation

export var REFERENCE_KINDS = [
  { id: "drawing", label: "Drawing of" },
  { id: "specification", label: "Specification for" },
  { id: "reference", label: "References" }
];

export function kindLabel(kind) {
  var k = REFERENCE_KINDS.filter(function (x) { return x.id === kind; })[0];
  return k ? k.label : kind;
}

/** How a dependent depends on the item that changed ("uses it in its BOM"). */
export function relationText(r, target) {
  target = target || "it";
  switch (r) {
    case "bom": return "uses " + target + " in its BOM";
    case "drawing": return "is a drawing of " + target;
    case "specification": return "is a specification for " + target;
    case "reference": return "references " + target;
    case "simulation": return "is a simulation based on " + target;
    case "file": return "has a CAD file that references " + (target === "it" ? "its files" : "files of " + target);
    default: return "depends on " + target;
  }
}

/** What links the dependent to the item that changed, short ("BOM line"). */
export function relationShort(r) {
  switch (r) {
    case "bom": return "BOM line";
    case "drawing": return "Drawing";
    case "specification": return "Specification";
    case "reference": return "Reference";
    case "simulation": return "Simulation basis";
    case "file": return "CAD file reference";
    default: return "Dependency";
  }
}

/** Open impacts grouped by the dependent revision, in first-seen order,
 *  sources by number (the server's impacts::group_by_dependent). */
export function groupImpacts(list) {
  var groups = [], byRevision = {};
  list.forEach(function (i) {
    var g = byRevision[i.revision_id];
    if (!g) {
      g = byRevision[i.revision_id] = {
        item_id: i.item_id, item_number: i.item_number, item_name: i.item_name, project_id: i.project_id,
        project_code: i.project_code, revision_id: i.revision_id, revision_code: i.revision_code,
        revision_state: i.revision_state, latest_at: i.created_at, impacts: []
      };
      groups.push(g);
    }
    g.impacts.push(i);
  });
  var key = function (i) { return i.source_number + "/" + i.source_code; };
  groups.forEach(function (g) {
    g.impacts.sort(function (a, b) { return key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0; });
  });
  return groups;
}

/** The worklist's dependents (older servers send only the flat list). */
export function impactGroups(w) {
  return w.impact_groups || groupImpacts(w.impacts || []);
}

/** The open impacts on one dependent revision: a compact header with the
 *  actions for the whole group, one dense row per source, the hint once. */
export function impactGroup(g, inRevision) {
  var can = canEditProject(g.project_id);
  var n = g.impacts.length;
  var openIt = function () { openRevision(g.item_number, g.revision_code, "references"); };
  return h("article.impact-group", null, [
    h("header.impact-group-head", null, [
      h("h3", null, inRevision
        ? ["Check needed: ", h("span.mono", { text: g.item_number + "/" + g.revision_code })]
        : [h("button.link.mono", { type: "button", on: { click: openIt } }, g.item_number + "/" + g.revision_code), " ",
           h("span.impact-group-name", { text: g.item_name })]),
      h("span.impact-count", { text: n + " change" + (n === 1 ? "" : "s") + " to check" }),
      h("div.actions", null, [
        inRevision ? null : h("button.small.ghost", { type: "button", on: { click: openIt } }, "Open"),
        !inRevision && can && g.revision_state === "released"
          ? h("button.small.ghost", { type: "button", on: { click: function () { reviseRevision(g.item_number, g.revision_code); } } }, "Revise") : null,
        can && n > 1 ? h("button.small", { type: "button", on: { click: function () { acknowledgeAllDialog(g); } } }, "Acknowledge all…") : null
      ])
    ]),
    h("ul.impact-rows", null, g.impacts.map(function (i) {
      return h("li", { title: g.item_number + " " + relationText(i.relation) + (i.detail ? " (" + i.detail + ")" : "") }, [
        h("span.impact-source", null, [
          h("button.link.mono", { type: "button", on: { click: function () { openRevision(i.source_number, i.source_code); } } }, i.source_number + "/" + i.source_code),
          " ", h("span.impact-source-name", { text: i.source_name })]),
        h("span.impact-event", null, [i.event === "released" ? "released" : badge("obsolete"),
          i.change_number ? " via " : null,
          i.change_number ? h("button.link.mono.plain-weight", { type: "button", on: { click: function () { openChange(i.change_number); } } }, i.change_number) : null]),
        // a BOM line keeps its short detail (find no.); a referencing file name sits in the tooltip
        h("span.impact-relation", { title: i.detail ? relationText(i.relation) + " · " + i.detail : null },
          [relationShort(i.relation), i.detail && i.relation === "bom" ? h("span.impact-detail", { text: " · " + i.detail }) : null]),
        can ? h("button.small.ghost", { type: "button", on: { click: function () { acknowledgeDialog(i); } } }, "Acknowledge…") : null
      ]);
    })),
    h("p.hint", { text: "Revising and releasing " + g.item_number + " resolves " + (n === 1 ? "it" : "all " + n) + " as well." })
  ]);
}

export function acknowledgeAllDialog(g) {
  var note = h("textarea", { maxlength: "2000", required: true, placeholder: "e.g. form, fit and function unchanged" });
  var n = g.impacts.length;
  dialog("Acknowledge all for " + g.item_number + "/" + g.revision_code, h("div.stack", null, [
    h("p", { text: "One note for all " + n + " changes – " + g.item_number + " needs no change for any of them:" }),
    h("ul.plain", null, g.impacts.map(function (i) {
      return h("li", null, [h("span.mono", { text: i.source_number + "/" + i.source_code }), " " + i.source_name + " – " +
        (i.event === "released" ? "released" : "obsolete") + " · " + relationShort(i.relation)]);
    })),
    field("Note", note)
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    {
      label: "Acknowledge " + n, primary: true, submit: true, onClick: function (d) {
        api("POST", "/items/" + enc(g.item_number) + "/revisions/" + enc(g.revision_code) + "/impacts/acknowledge",
          { note: note.value, impacts: g.impacts.map(function (i) { return i.id; }) }).then(function (r) {
          d.close();
          toast(g.item_number + "/" + g.revision_code + " checked: " + r.acknowledged.length + " acknowledged" +
            (r.skipped ? " (" + r.skipped + " resolved in the meantime)" : ""));
          refreshWorklistCount().catch(function () {});
          if (state.view === "worklist") selectView("worklist");
          reloadActive();
        }, function (err) { d.fail(err); });
      }
    }
  ]);
}

export function acknowledgeDialog(i) {
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

export function addReferenceDialog(tab) {
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

export function revReferences(tab, el) {
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

export function revChanges(tab, el) {
  var list = tab.data.changes;
  el.appendChild(h("section.panel", null, [h("h2", { text: "Change orders" }),
    list.length ? table(["Change", "Title", "Status", "Created"], list.map(function (c) {
      return { cells: [h("span.mono", { text: c.number }), c.title, badge(c.status), fmtDate(c.created_at)], open: function () { openChange(c.number); } };
    })) : h("p.empty", { text: "Not part of any change order yet – “Release…” in the toolbar creates one." })]));
}

export function revHistory(tab, el) {
  var rev = tab.data.rev;
  return api("GET", "/items/" + enc(rev.item_number) + "/history?limit=200").then(function (list) {
    el.appendChild(h("section.panel", null, [h("h2", { text: "History" }),
      list.length ? table(["When", "Who", "What"], list.map(function (e) {
        return { cells: [fmtDate(e.at), e.username, e.summary] };
      })) : h("p.empty", { text: "No activity yet." })]));
  });
}
