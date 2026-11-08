// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Structure view of the navigator: the pure part (no DOM, no
// server calls), so it can be checked on its own. navigator.js draws it.

/** Lines of one level shown before "show all". */
export var LEVEL_LIMIT = 50;

/** Navigator modes: the flat list of all items, or the structure view. */
export var MODES = ["all", "structure"];

export function normalizeMode(value) {
  return MODES.indexOf(value) !== -1 ? value : "all";
}

/** The first LEVEL_LIMIT nodes of a level, the rest hidden until "show all":
 *  { shown, hidden } (hidden counts the lines of the whole level, also those
 *  the server cut off at its own maximum). */
export function cutLevel(nodes, total, showAll, limit) {
  limit = limit || LEVEL_LIMIT;
  var all = showAll ? nodes : nodes.slice(0, limit);
  return { shown: all, hidden: Math.max((total == null ? nodes.length : total) - all.length, 0) };
}

/** The key of a node in the expanded map: the path of item numbers from the group. */
export function pathKey(parentKey, number) {
  return parentKey + "/" + number;
}

/** What a line of the structure shows: label, whether it can be opened and
 *  expanded, the placeholder text for lines without a visible revision. */
export function describe(node, rule) {
  if (node.restricted) {
    return { kind: "restricted", label: "restricted item", placeholder: "Restricted item – in a project group you are not a member of",
      openable: false, expandable: false };
  }
  if (!node.code) {
    return { kind: "unresolved", label: node.item_number, placeholder: rule === "released" ? "not released yet" : "no revision",
      openable: false, expandable: false };
  }
  return { kind: "revision", label: node.item_number + "/" + node.code, placeholder: "",
    openable: true, expandable: (node.bom_lines || 0) > 0 };
}

/** "⧉ N" badge text: a part used in several assemblies, none for 0 or 1. */
export function usageBadge(node) {
  return node.used_in > 1 ? "⧉ " + node.used_in : "";
}

/** Quantity text of a line: ×8, ×0.5; none for a top-level item. */
export function quantityText(node) {
  if (node.quantity == null) return "";
  return "×" + String(Math.round(node.quantity * 1e6) / 1e6);
}

/** The other appearances of the same item in a list of rendered rows. */
export function twins(rows, number) {
  return rows.filter(function (r) { return r.number === number; });
}

/** Top-level view of a project group from the server answer, nodes by number. */
export function rootNodes(answer) {
  return { nodes: (answer && answer.nodes) || [], total: (answer && answer.total) || 0 };
}
