// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Dialogs: new item, settings, about.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { $, ITEM_TYPES, TAURI, api, call, dialog, errorText, field, h, label, setStatus, state, toast } from "./core.js";
import { canEdit, canEditProject, freshProjects, refreshWorklistCount } from "./shell.js";
import { selectView } from "./navigator.js";
import { openRevision } from "./revision.js";

// ======================================================================
// Dialogs: new item, settings, about
// ======================================================================

/** `C:\cad\Bracket.ipt` → `Bracket.ipt`. */
export function baseName(path) {
  return String(path).split(/[\\/]/).pop();
}

export var ASSEMBLY_EXT = ["iam", "asm", "sldasm", "catproduct", "f3z"];
export var DRAWING_EXT = ["idw", "dwg", "dxf", "dft", "slddrw", "catdrawing", "drw", "ipn", "pdf"];

/** Name and item type suggested by a CAD file (Creo iterations ignored). */
export function guessFromFile(path) {
  var base = baseName(path).replace(/\.\d+$/, "");
  var dot = base.lastIndexOf(".");
  var ext = dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
  return {
    name: dot > 0 ? base.slice(0, dot) : base,
    type: ASSEMBLY_EXT.indexOf(ext) >= 0 ? "assembly" : DRAWING_EXT.indexOf(ext) >= 0 ? "document" : "part"
  };
}

export function newItemDialog() { freshProjects().then(openNewItemDialog); }

export function openNewItemDialog() {
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
