// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Files view of Home: the folders and files of the active
// project group, one folder at a time (GET /projects/{code}/files), so a big
// group is never loaded as a whole. The rules are in files-view.js.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { ICONS, api, badge, clear, enc, errorText, h, label, lockIcon, state, svgIcon, toast } from "./core.js";
import { typeIcon } from "./navigator.js";
import { openRevision } from "./revision.js";
import { countText, entryIconType, entryName, entryRevision, entryTitle, folderKey, groupTotal, mergeEntries, remaining, sortEntries, sortFolders } from "./files-view.js";

/** Entries asked for per request (the server's maximum). */
var PAGE = 500;

/** A row that opens with a click, Enter or Space. */
function row(spec, attrs, children, onOpen) {
  attrs.tabindex = "0";
  attrs.on = {
    click: onOpen,
    keydown: function (e) {
      if (e.target !== el || (e.key !== "Enter" && e.key !== " ")) return;
      e.preventDefault();
      onOpen();
    }
  };
  var el = h(spec, attrs, children);
  return el;
}

function fetchLevel(p, path, offset) {
  return api("GET", "/projects/" + enc(p.code) + "/files?folder=" + enc(path) + "&limit=" + PAGE + "&offset=" + offset);
}

/**
 * Draws the files of project group `p` into the list `ul`.
 * `opts.sort` is the order of the files (files-view.js sortEntries),
 * `opts.count` the element that gets the number of rows of the group, and
 * `opts.olderServer()` draws the list for a server that does not know the
 * file browser yet.
 */
export function renderFiles(p, ul, opts) {
  clear(ul);
  ul.appendChild(h("li.loading", { text: "Loading" }));
  return fetchLevel(p, "", 0).then(function (level) {
    if (opts.count) opts.count.textContent = String(groupTotal(level));
    drawLevel(p, "", ul, level, opts.sort, "No files yet.");
  }, function (err) {
    if (err && err.status === 404 && opts.olderServer) return opts.olderServer();
    clear(ul);
    ul.appendChild(h("li.notice.error", { text: errorText(err) }));
  });
}

/** Loads one folder into `ul`. */
function loadFolder(p, path, ul, sort) {
  clear(ul);
  ul.appendChild(h("li.loading", { text: "Loading" }));
  return fetchLevel(p, path, 0).then(function (level) {
    drawLevel(p, path, ul, level, sort, "This folder is empty.");
  }, function (err) {
    clear(ul);
    ul.appendChild(h("li.notice.error", { text: errorText(err) }));
  });
}

function drawLevel(p, path, ul, level, sort, emptyText) {
  var loaded = level.entries || [];
  var total = level.total || loaded.length;
  function more() {
    fetchLevel(p, path, loaded.length).then(function (next) {
      var before = loaded.length;
      loaded = mergeEntries(loaded, next.entries);
      // nothing new came: the list shrank meanwhile, there is no more
      if (loaded.length === before) total = loaded.length;
      paint();
    }, function (err) { toast(errorText(err), "error"); });
  }
  function paint() {
    clear(ul);
    var folders = sortFolders(level.folders);
    var entries = sortEntries(loaded, sort);
    if (!folders.length && !entries.length) {
      ul.appendChild(h("li.empty", { text: emptyText }));
      return;
    }
    folders.forEach(function (f) { ul.appendChild(folderNode(p, f, sort)); });
    var files = entries.filter(function (e) { return e.filename; });
    var loose = entries.filter(function (e) { return !e.filename; });
    files.forEach(function (e) { ul.appendChild(entryNode(e)); });
    if (loose.length) {
      ul.appendChild(h("li.tree-sep", { text: "Items without files" }));
      loose.forEach(function (e) { ul.appendChild(entryNode(e)); });
    }
    var rest = remaining(total, loaded.length);
    if (rest) {
      ul.appendChild(h("li", null, h("button.link.small", { type: "button", on: { click: more } },
        "show " + Math.min(rest, PAGE) + " more of " + total)));
    }
  }
  paint();
}

/** A folder: expands into its own level when opened. */
function folderNode(p, f, sort) {
  var key = folderKey(p.code, f.path);
  var open = !!state.expanded[key];
  var children = h("ul.tree" + (open ? "" : ".hidden"));
  var twisty = h("span.twisty", { text: open ? "▾" : "▸" });
  var node = row("div.node.folder", {
    title: f.path + " – " + countText(f.files, "file", "files"), "aria-expanded": String(open)
  }, [
    twisty, h("span.ico", null, svgIcon(ICONS.folder)),
    h("span.label", null, h("strong", { text: f.name })),
    h("span.meta", null, h("span.count", { text: String(f.files) }))
  ], function () {
    open = state.expanded[key] = !state.expanded[key];
    twisty.textContent = open ? "▾" : "▸";
    node.setAttribute("aria-expanded", String(open));
    children.classList.toggle("hidden", !open);
    if (open) loadFolder(p, f.path, children, sort);
  });
  if (open) loadFolder(p, f.path, children, sort);
  return h("li", null, [node, children]);
}

/** A file (or an item without files): the name first, the revision dimmed
 *  after it – like the rows of All items (navigator.js itemNode). */
function entryNode(e) {
  var key = "rev:" + e.item_number + ":" + e.code;
  var meta = [];
  if (e.state) meta.push(badge(e.state));
  if (e.checked_out_by_name) {
    var mine = !!state.me && e.checked_out_by_name === state.me.display_name;
    meta.push(h("span.lock-owner", { title: "Checked out by " + (mine ? "you" : e.checked_out_by_name) }, lockIcon(mine)));
  }
  var node = row("div.node.file" + (state.active === key ? ".selected" : ""), {
    title: entryTitle(e, label), "data-item": e.item_number
  }, [
    h("span.twisty"), h("span.ico", { text: typeIcon(entryIconType(e)) }),
    h("span.label", null, [h("span.fname", { text: entryName(e) }), h("span.dim.mono", { text: " " + entryRevision(e) })]),
    h("span.meta", null, meta)
  ], function () { openRevision(e.item_number, e.code); });
  return h("li", null, node);
}
