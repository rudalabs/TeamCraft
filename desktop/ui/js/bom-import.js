// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Import BOM.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { api, badge, call, clear, dialog, enc, field, fmtNum, h, label, setStatus, state, toast } from "./core.js";
import { canEdit, canEditProject, freshProjects, refreshWorklistCount } from "./shell.js";
import { selectView } from "./navigator.js";
import { activeTab, reloadActive, stat, table } from "./tabs.js";
import { openRevision } from "./revision.js";
import { baseName } from "./dialogs.js";

// ------------------------------------------------------------ Import BOM

/** Lines of the plan shown in the preview (the server imports all). */
export var BOM_PREVIEW_LINES = 15;

/**
 * Import BOM…: a BOM table (CSV/Excel) or STEP assembly exported by the CAD
 * program, below a checked-out revision (`rev`) or – without `rev` – as a
 * new assembly. The server reads the columns: the first call is a dry run
 * whose plan (counts, notes, first lines) the dialog shows, Import sends the
 * same file again and the server creates everything in one transaction.
 */
export function importBomDialog(rev) { freshProjects().then(function () { openImportBomDialog(rev || null); }); }

/** Lines of the plan to tick by default: new lines, and existing lines whose
 * quantity differs from the instances counted in the CAD file (the ×1 lines
 * of an earlier folder import). An assumed quantity never replaces one. */
export function cadPlanSelected(c) {
  if (c.status === "new") return true;
  return c.status === "differs" && c.quantity_source === "cad_instances";
}

/**
 * Quantities from CAD…: the BOM the CAD references of this revision lead to
 * (`GET …/bom/from-references`: instances counted in SOLIDWORKS, NX and STEP
 * assemblies), compared with the BOM – adds missing lines and corrects the
 * quantity of existing ones (e.g. the quantity 1 of an earlier folder import).
 */
export function bomFromCadDialog(rev) {
  if (!canEdit()) { toast("Viewers cannot change BOMs.", "error"); return; }
  if (rev.checked_out_by !== state.me.id) { toast("Check out " + rev.item_number + "/" + rev.code + " first – the BOM is changed under your check-out.", "error"); return; }
  var base = "/items/" + enc(rev.item_number) + "/revisions/" + enc(rev.code) + "/bom/from-references";
  var result = h("div.stack.bom-plan", null, [h("p.loading", { text: "Reading the CAD files of " + rev.item_number + "/" + rev.code })]);
  var boxes = [];
  var applyButton = null;
  var handle = dialog("Quantities from CAD – " + rev.item_number + "/" + rev.code, h("div.stack", null, [
    h("p.hint", { text: "How often the assembly places each part, counted in its CAD files (SOLIDWORKS 2015+, NX, STEP assemblies with external part files). Other formats (Inventor, Solid Edge, …) do not tell – their quantity stays as it is." }),
    result
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    {
      label: "Apply", primary: true, submit: true, onClick: function (d) {
        var children = boxes.filter(function (b) { return b.box.checked; }).map(function (b) { return b.child.item_number; });
        if (!children.length) { d.fail("Tick at least one line."); return; }
        applyButton.disabled = true;
        api("POST", base, { children: children, update_quantities: true }).then(function (r) {
          d.close();
          var n = function (list, one, many) { return list.length ? list.length + " " + (list.length === 1 ? one : many) : null; };
          toast([n(r.added, "line added", "lines added"), n(r.updated, "quantity corrected", "quantities corrected"),
            n(r.refused, "refused (circular)", "refused (circular)")].filter(Boolean).join(" · ") || "Nothing changed");
          var tab = activeTab();
          if (tab && tab.kind === "revision" && tab.number === rev.item_number && tab.code === rev.code) { tab.sub = "structure"; reloadActive(); }
        }, function (err) { applyButton.disabled = false; d.fail(err); });
      }
    }
  ], { wide: true });
  applyButton = handle.form.querySelector(".dialog-actions button.primary");
  applyButton.disabled = true;
  api("GET", base).then(function (p) {
    clear(result);
    if (!p.children.length) {
      result.appendChild(h("p.notice", { text: "The CAD files of this revision lead to no BOM lines." }));
      return;
    }
    var rows = p.children.map(function (c) {
      var box = h("input", { type: "checkbox", checked: cadPlanSelected(c), disabled: c.status === "circular" || c.status === "already_in_bom" });
      boxes.push({ box: box, child: c });
      return {
        cells: [
          box,
          h("span.mono", { text: c.item_number }),
          c.item_name,
          h("span", { title: c.quantity_label, text: c.quantity_text + (c.quantity_source === "assumed" ? " (assumed)" : "") }),
          c.bom_quantity_text == null ? "–" : c.bom_quantity_text,
          c.status_label
        ]
      };
    });
    result.appendChild(table(["", "Number", "Name", "CAD", "BOM", "Status"], rows, { numeric: [3, 4] }));
    if (p.not_in_teamcraft.length) {
      result.appendChild(h("p.hint", { text: p.not_in_teamcraft.length + " referenced files are not in TeamCraft: " +
        p.not_in_teamcraft.slice(0, 5).map(function (u) { return u.name; }).join(", ") + (p.not_in_teamcraft.length > 5 ? ", …" : "") }));
    }
    applyButton.disabled = false;
  }, function (err) { clear(result); handle.fail(err); });
}

export function openImportBomDialog(rev) {
  if (!canEdit()) { toast("Viewers cannot import BOMs.", "error"); return; }
  if (rev && rev.checked_out_by !== state.me.id) { toast("Check out " + rev.item_number + "/" + rev.code + " first – the BOM is imported under your check-out.", "error"); return; }
  var projects = rev ? [] : state.projects.filter(function (p) { return p.active && canEditProject(p.id); });
  if (!rev && !projects.length) { toast("You are not a member of any project group – ask an admin to add you.", "error"); return; }
  var path = null;
  var plan = null;
  var fileName = h("span.mono", { text: "no file chosen" });
  var project = rev ? null : h("select", null, projects.map(function (p) { return h("option", { value: p.code, text: p.code + " – " + p.name }); }));
  var name = rev ? null : h("input", { type: "text", maxlength: "200", placeholder: "from the assembly row or the file name" });
  var numbers = h("select", null, [
    h("option", { value: "file", text: "Part numbers from the file" }),
    h("option", { value: "generate", text: "New numbers (numbering scheme of the group)" })
  ]);
  var match = h("input", { type: "checkbox", checked: true });
  var result = h("div.stack.bom-plan");
  var handle = null;
  var importButton = null;

  function request() {
    var r = { numbers: numbers.value, match_existing: match.checked };
    if (rev) { r.item = rev.item_number; r.code = rev.code; } else { r.project = project.value; r.name = name.value; }
    return r;
  }
  // Every change of the options needs a new preview before Import.
  function invalidate() {
    plan = null;
    if (importButton) importButton.disabled = true;
    clear(result);
    if (path) result.appendChild(h("p.hint", { text: "Options changed – Preview again." }));
  }
  [project, name, numbers, match].forEach(function (el) {
    if (el) el.addEventListener(el.tagName === "INPUT" && el.type === "text" ? "input" : "change", invalidate);
  });

  function showPlan(p) {
    clear(result);
    var target = p.target ? "Into " + p.root_label : "New assembly " + (p.root_number || "(number generated)") + " · " + p.root_name;
    result.appendChild(h("p", null, [h("strong", { text: target }), " · project group " + p.project_code]));
    result.appendChild(h("div.stats", null, [
      stat(p.lines.length, p.lines.length === 1 ? "BOM line" : "BOM lines"),
      stat(p.new_items, p.new_items === 1 ? "new item" : "new items"),
      stat(p.existing_items, p.existing_items === 1 ? "existing item" : "existing items")
    ]));
    if (p.warnings.length) {
      result.appendChild(h("div.notice", null, [h("strong", { text: p.warnings.length + (p.warnings.length === 1 ? " note" : " notes") }),
        h("ul", null, p.warnings.map(function (w) { return h("li", { text: w }); }))]));
    }
    if (!p.lines.length) {
      result.appendChild(h("p.notice.error", { text: "No BOM lines found in " + p.filename + " – check the columns (part number, quantity, level)." }));
      return;
    }
    var rows = p.lines.slice(0, BOM_PREVIEW_LINES).map(function (l) {
      var it = p.items[l.child];
      var indent = h("span.indent");
      indent.style.width = ((l.level - 1) * 1.2) + "rem";
      var number = it.number || "(generated)";
      return {
        cells: [
          h("span", null, [indent, h("span.mono", { text: String(l.find_number) })]),
          h("span.mono", { text: number + (it.number && it.file_number && it.file_number !== it.number ? " ← " + it.file_number : "") }),
          it.name,
          fmtNum(l.quantity, 6) + " " + (it.unit || ""),
          it.existing ? h("span.dim", { text: "existing " + (it.existing_label || "") }) : badge("new", "new " + label(it.item_type))
        ]
      };
    });
    result.appendChild(table(["Find no.", "Number", "Name", "Qty", "Item"], rows, { numeric: [3] }));
    if (p.lines.length > BOM_PREVIEW_LINES) {
      result.appendChild(h("p.hint", { text: "… and " + (p.lines.length - BOM_PREVIEW_LINES) + " more lines." }));
    }
  }

  function preview() {
    if (!path) { handle.fail("Choose the BOM file first."); return; }
    plan = null;
    importButton.disabled = true;
    clear(result);
    result.appendChild(h("p.loading", { text: "Reading " + baseName(path) }));
    call("import_bom", { path: path, request: request(), dryRun: true }).then(function (p) {
      plan = p;
      showPlan(p);
      importButton.disabled = !p.lines.length;
    }, function (err) {
      clear(result);
      handle.fail(err);
    });
  }

  var pick = h("button", { type: "button", on: { click: function () {
    call("pick_bom_file").then(function (chosen) {
      if (!chosen) return;
      path = chosen;
      fileName.textContent = baseName(chosen);
      preview();
    }, function (err) { handle.fail(err); });
  } } }, "Choose file…");

  var title = rev ? "Import BOM into " + rev.item_number + "/" + rev.code : "Import BOM as a new assembly";
  handle = dialog(title, h("div.stack", null, [
    field("BOM file", h("div.row", null, [pick, fileName]),
      "CSV, Excel or ODS export of the CAD program's BOM, or a STEP assembly"),
    rev ? h("p.hint", { text: "The lines are added to the structure of " + rev.item_number + "/" + rev.code + "; missing items are created in " + (rev.project_code || "its project group") + "." }) : null,
    rev ? null : h("div.grid-2", null, [field("Project group", project), field("Name", name, "optional")]),
    h("div.grid-2", null, [field("Numbers", numbers), h("label.check", null, [match, "Use existing items with the same number"])]),
    result,
    h("p.hint", { text: "The column recognition is a heuristic – check the preview. CATIA V5 Bill of Material reports are not understood; use a flat or indented export." })
  ]), [
    { label: "Cancel", onClick: function (d) { d.close(); } },
    { label: "Preview", onClick: function () { preview(); } },
    {
      label: "Import", primary: true, submit: true, onClick: function (d) {
        if (!plan) { preview(); return; }
        importButton.disabled = true;
        setStatus("Importing " + baseName(path) + " …");
        call("import_bom", { path: path, request: request(), dryRun: false }).then(function (r) {
          setStatus("");
          d.close();
          var created = r.created.length;
          toast("Imported " + r.lines + (r.lines === 1 ? " line" : " lines") + " into " + r.root +
            (created ? " · " + created + (created === 1 ? " new item" : " new items") : "") +
            (r.warnings.length ? " · " + r.warnings.length + (r.warnings.length === 1 ? " note" : " notes") : ""));
          var parts = String(r.root).split("/");
          var tab = rev ? activeTab() : null;
          if (rev && tab && tab.kind === "revision" && tab.number === rev.item_number && tab.code === rev.code) {
            tab.sub = "structure";
            reloadActive();
          } else if (parts.length === 2) {
            openRevision(parts[0], parts[1], "structure");
          }
          if (!rev && state.view === "home") selectView("home");
          refreshWorklistCount();
        }, function (err) {
          setStatus("");
          importButton.disabled = false;
          d.fail(err);
        });
      }
    }
  ], { wide: true });
  importButton = handle.form.querySelector(".dialog-actions button.primary");
  importButton.disabled = true;
}
