// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Files view of Home (the folders and files of the active
// project group): the pure part (no DOM, no server calls), so it can be
// checked on its own. browser.js draws it.
//
// The view shows the group the way its working folder looks: folders first,
// then the files – by name unless another order is chosen. The file name is
// what an engineer knows a part by; the item number follows it, where a
// narrow navigator can cut it off.

var collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Names as a file manager orders them: without regard to case, digits by
 *  their value (Part2 before Part10); a plain comparison breaks ties. */
export function compareNames(a, b) {
  a = a || ""; b = b || "";
  return collator.compare(a, b) || (a < b ? -1 : a > b ? 1 : 0);
}

/** The sub-folders of a level by name. */
export function sortFolders(folders) {
  return (folders || []).slice().sort(function (a, b) { return compareNames(a.name, b.name); });
}

/** What a row is known by: its file name, or the item's name without a file. */
export function entryName(e) {
  return e.filename || e.name || e.item_number || "";
}

/** `PRT-000042/B` – the revision a row opens. */
export function entryRevision(e) {
  return (e.item_number || "") + (e.code ? "/" + e.code : "");
}

/** The item type whose icon a row gets: what the file contains when the
 *  format tells (the drawing of a part is a drawing), else the item's type. */
export function entryIconType(e) {
  var byKind = { part: "part", assembly: "assembly", drawing: "document" };
  return byKind[e.format_kind] || e.item_type || "";
}

/** The orders Home offers (navigator.js SORTS): what comes first for each. */
var SORT_KEYS = {
  name: function () { return ""; },
  number: function (e) { return e.item_number || ""; },
  type: function (e) { return entryIconType(e); },
  state: function (e) { return e.state || ""; }
};

/**
 * The entries of a level in the chosen order (`name`, `number`, `type` or
 * `state`; anything else is `name`): the files first, then the items without
 * files; rows that are equal in the order follow by name, then by number.
 */
export function sortEntries(entries, by) {
  var key = SORT_KEYS[by] || SORT_KEYS.name;
  return (entries || []).slice().sort(function (a, b) {
    var ka = key(a), kb = key(b);
    return (a.filename ? 0 : 1) - (b.filename ? 0 : 1) ||
      (ka < kb ? -1 : ka > kb ? 1 : 0) ||
      compareNames(entryName(a), entryName(b)) ||
      compareNames(a.item_number, b.item_number);
  });
}

/** The tooltip of a row: everything the dense row leaves out. */
export function entryTitle(e, labelOf) {
  var parts = [entryRevision(e) + " – " + (e.name || "")];
  if (e.filename) parts.push(e.format_label || "file");
  else parts.push("no files yet");
  if (e.state) parts.push(labelOf ? labelOf(e.state) : e.state);
  if (e.checked_out_by_name) parts.push("checked out by " + e.checked_out_by_name);
  return parts.join(" · ");
}

/** Adds a further page of entries to the ones already shown; a row that both
 *  pages hold (the list changed between two requests) stays once. */
export function mergeEntries(have, more) {
  var seen = {};
  function id(e) { return e.file_id || "item:" + e.item_number; }
  var out = [];
  (have || []).concat(more || []).forEach(function (e) {
    if (seen[id(e)]) return;
    seen[id(e)] = true;
    out.push(e);
  });
  return out;
}

/** Entries not loaded yet. */
export function remaining(total, shown) {
  return Math.max((total || 0) - shown, 0);
}

/** Rows a group has in all: the entries at its top and the files in its folders. */
export function groupTotal(level) {
  return (level.folders || []).reduce(function (n, f) { return n + (f.files || 0); }, level.total || 0);
}

/** The key of a folder in the expanded map. */
export function folderKey(project, path) {
  return "dir:" + project + ":" + (path || "");
}

/** `12 files` / `1 file`. */
export function countText(n, one, many) {
  return n + " " + (n === 1 ? one : many);
}
