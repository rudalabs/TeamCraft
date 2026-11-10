// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Assembly graph: a navigable reference/BOM graph of a
// revision. The focused item sits in the middle, its BOM children and
// references below, the assemblies that use it above. Node size encodes
// structural complexity (its own BOM child count), node colour the lifecycle
// state. Every node is clickable: an assembly re-centres the graph on itself,
// anything else opens its overview – with a back control that stays in the
// same place so the graph is always one click away.
//
// Pure vanilla: the edges are SVG, the node cards an HTML layer stacked above
// it that gets the same view transform (zoom and pan move both together).
// The cards are deliberately not SVG foreignObjects: WebKit (WKWebView on
// macOS, WebKitGTK on Linux) does not apply ancestor SVG transforms to the
// HTML inside a foreignObject, so every card ended up at the origin. No
// external engine (the project ships no downloaded engines and the desktop UI
// has no build step).

import { api, enc, h, invoke, label } from "./core.js";
import { typeIcon } from "./navigator.js";
import { activate, findTab, openTab, renderActive } from "./tabs.js";

var SVG_NS = "http://www.w3.org/2000/svg";

// Per item:code, the cheap facts a node needs (type, complexity, a thumbnail
// file). Shared across re-centres so the graph is not refetched each hop.
var nodeInfo = {};
// Blob URLs of node thumbnails, by file SHA-256 (same bytes → one request).
// Bounded: beyond THUMB_CACHE_MAX the least recently used URL is revoked, so
// a long session of re-centring does not keep every preview alive.
var thumbUrls = {};
var thumbOrder = [];
var THUMB_CACHE_MAX = 200;
// Above this many neighbours only the first ENRICH_MAX are fetched one by one
// (each costs a full revision request); the rest keep their basic card.
var ENRICH_MAX = 48;
// Up to this many nodes per side sit on an arc; more are laid out in rows.
var ARC_MAX = 8;

function key(number, code) { return number + ":" + (code || ""); }

function svg(name, attrs) {
  var el = document.createElementNS(SVG_NS, name);
  if (attrs) Object.keys(attrs).forEach(function (k) { el.setAttribute(k, String(attrs[k])); });
  return el;
}

function clamp(v, lo, hi) { return v < lo ? lo : v > hi ? hi : v; }

// -------------------------------------------------------------------------
// Data: the centre and its neighbours, normalised from a revision's payload.
// -------------------------------------------------------------------------

/** Loads the full revision payload of one item/code (reusing the open tab's
 *  own data for its centre so the first graph costs no extra request). */
function loadCenter(tab, number, code) {
  if (tab.number === number && tab.code === code && tab.data) return Promise.resolve(tab.data);
  var base = "/items/" + enc(number);
  return Promise.all([
    api("GET", base + "/revisions/" + enc(code)),
    api("GET", base),
    api("GET", base + "/revisions/" + enc(code) + "/references").catch(function () { return null; })
  ]).then(function (r) {
    return {
      rev: r[0].revision, files: r[0].files, bom: r[0].bom, changes: r[0].changes,
      item: r[1].item, revisions: r[1].revisions,
      refs: r[2] || { outgoing: [], incoming: [], files: [], file_referrers: [], impacts: [] }
    };
  });
}

/** The neighbour nodes of a centre, deduplicated by item number. "down" =
 *  BOM children and references; "up" = items that use or reference this one. */
function neighbours(d) {
  var by = {}, order = [];
  function add(n) {
    if (!n.number || !n.code) return; // unresolved: nothing to open or draw
    var k = n.number;
    var ex = by[k];
    if (!ex) { by[k] = n; order.push(k); return; }
    // the same item on several BOM lines: its instances add up
    if (ex.rel === "bom" && n.rel === "bom") { ex.qty = (ex.qty || 0) + (n.qty || 0); return; }
    // keep a "down" (structural) relation over an "up" one, and fill blanks
    if (ex.dir === "up" && n.dir === "down") { n.alsoUsedBy = true; by[k] = n; }
    else { ex.type = ex.type || n.type; ex.name = ex.name || n.name; }
  }
  (d.bom || []).forEach(function (b) {
    add({ number: b.child_number, code: b.child_latest_code, state: b.child_latest_state,
      type: b.child_type, name: b.child_name, dir: "down", rel: "bom", qty: b.quantity });
  });
  (d.refs.outgoing || []).forEach(function (r) {
    add({ number: r.to_number, code: r.to_latest_code, state: r.to_latest_state,
      type: null, name: r.to_name, dir: "down", rel: "reference" });
  });
  (d.refs.incoming || []).forEach(function (r) {
    add({ number: r.from_number, code: r.from_code, state: r.from_state,
      type: null, name: r.from_name, dir: "up", rel: "used-by" });
  });
  (d.refs.file_referrers || []).forEach(function (f) {
    add({ number: f.item_number, code: f.revision_code, state: f.revision_state,
      type: null, name: f.item_name, dir: "up", rel: "used-by", via: f.filename });
  });
  return order.map(function (k) { return by[k]; });
}

/** Lazily fetches each neighbour's type, complexity (its own BOM child count)
 *  and a preview file, then calls back so the node can grow and show its image.
 *  Bounded concurrency; results are cached across re-centres. */
function enrich(nodes, onUpdate, isLive) {
  var queue = nodes.filter(function (n) {
    var info = nodeInfo[key(n.number, n.code)];
    if (info) { Object.assign(n, info); return false; }
    return true;
  });
  var skipped = Math.max(0, queue.length - ENRICH_MAX);
  queue = queue.slice(0, ENRICH_MAX);
  onUpdate(); // paint what the cache already had (coalesced by the caller)
  var i = 0, running = 0;
  function pump() {
    while (running < 5 && i < queue.length && isLive()) {
      var n = queue[i++]; running++;
      fetchOne(n).then(function () { running--; if (isLive()) { onUpdate(n); pump(); } });
    }
  }
  function fetchOne(n) {
    return api("GET", "/items/" + enc(n.number) + "/revisions/" + enc(n.code)).then(function (r) {
      var thumbFile = (r.files || []).filter(function (f) { return f.format_info && f.format_info.thumbnail; })[0];
      var info = {
        type: n.type || r.revision.item_type,
        complexity: (r.bom || []).length,
        fileName: thumbFile ? thumbFile.filename : ((r.files || [])[0] ? r.files[0].filename : null),
        thumbId: thumbFile ? thumbFile.id : null,
        thumbSha: thumbFile ? thumbFile.sha256 : null
      };
      nodeInfo[key(n.number, n.code)] = info;
      Object.assign(n, info);
    }, function () { /* restricted or gone: leave the node as-is */ });
  }
  pump();
  return skipped;
}

// -------------------------------------------------------------------------
// Layout: the centre at the origin, "down" neighbours in the lower arc,
// "up" neighbours in the upper arc. Radius grows with the count so cards do
// not overlap; the rest is handled by zoom and pan.
// -------------------------------------------------------------------------

function nodeSize(complexity, isCenter) {
  var s = clamp(78 + 15 * Math.sqrt(complexity || 0), 78, 150);
  return isCenter ? Math.max(s, 124) : s;
}

function placeArc(list, a0, a1, minRadius) {
  var n = list.length;
  if (!n) return;
  var span = a1 - a0;
  var biggest = list.reduce(function (m, x) { return Math.max(m, x.size); }, 60);
  // enough radius that the gap between neighbours clears the widest card
  var perGap = n > 1 ? span / (n - 1) : span;
  var byGap = n > 1 ? (biggest * 1.3) / perGap : 0;
  var r = Math.max(minRadius, byGap);
  list.forEach(function (x, idx) {
    var a = n === 1 ? (a0 + a1) / 2 : a0 + perGap * idx;
    x.x = Math.cos(a) * r;
    x.y = Math.sin(a) * r;
  });
}

/** Many neighbours: centred rows stacked away from the centre (sign +1 =
 *  below, -1 = above), so the scene grows with √n instead of n. */
function placeRows(list, sign, minRadius) {
  var n = list.length;
  var biggest = list.reduce(function (m, x) { return Math.max(m, x.size); }, 60);
  var cell = biggest * 1.2;
  var cols = Math.max(ARC_MAX, Math.ceil(Math.sqrt(n * 2.5)));
  list.forEach(function (x, idx) {
    var row = Math.floor(idx / cols), col = idx % cols;
    var inRow = Math.min(cols, n - row * cols);
    x.x = (col - (inRow - 1) / 2) * cell;
    x.y = sign * (minRadius + biggest / 2 + row * cell);
  });
}

function layout(center, nodes) {
  // screen angles: 0 = right, +90° = down. Down arc ≈ straight down, up arc ≈ straight up.
  var rad = Math.PI / 180;
  var down = nodes.filter(function (n) { return n.dir === "down"; });
  var up = nodes.filter(function (n) { return n.dir === "up"; });
  var minR = center.size / 2 + 90;
  if (down.length > ARC_MAX) placeRows(down, 1, minR); else placeArc(down, 35 * rad, 145 * rad, minR);
  if (up.length > ARC_MAX) placeRows(up, -1, minR); else placeArc(up, 215 * rad, 325 * rad, minR);
  center.x = 0; center.y = 0;
}

// -------------------------------------------------------------------------
// Rendering
// -------------------------------------------------------------------------

function thumbInto(img, sha, id) {
  if (!sha || !id) return;
  var p = thumbUrls[sha];
  if (p) thumbOrder.splice(thumbOrder.indexOf(sha), 1);
  else {
    p = thumbUrls[sha] = invoke("file_bytes", { kind: "thumbnail", id: id })
      .then(function (bytes) { return URL.createObjectURL(new Blob([bytes])); });
    while (thumbOrder.length >= THUMB_CACHE_MAX) {
      var old = thumbOrder.shift();
      thumbUrls[old].then(function (u) { URL.revokeObjectURL(u); }, function () {});
      delete thumbUrls[old];
    }
  }
  thumbOrder.push(sha);
  p.then(function (u) { img.src = u; img.classList.remove("hidden"); }, function () { /* no image */ });
}

/** The HTML card of one node (in the cards layer). */
function nodeCard(n, isCenter, onOpen) {
  var cls = "g-node state-" + (n.state || "unknown") + " type-" + (n.type || "unknown") + (isCenter ? " is-center" : "");
  var thumb = h("div.g-thumb");
  if (n.thumbId) {
    var img = h("img.g-img.hidden", { alt: "", draggable: "false" });
    thumbInto(img, n.thumbSha, n.thumbId);
    thumb.appendChild(img);
  }
  thumb.appendChild(h("span.g-glyph", { text: typeIcon(n.type) })); // fallback / sits behind the image
  var caption = n.fileName || (n.number + (n.code ? "/" + n.code : ""));
  var badges = [];
  if (n.complexity) badges.push(h("span.g-badge", { title: n.complexity + " BOM children" }, "⚙ " + n.complexity));
  if (n.rel === "reference") badges.push(h("span.g-badge.g-ref", { title: "Reference, not a BOM child" }, "ref"));
  if (n.dir === "up") badges.push(h("span.g-badge.g-up", { title: n.via ? "Its file references this (" + n.via + ")" : "Uses this item" }, "uses"));
  var card = h("div." + cls.split(" ").join("."), {
    role: "button", tabindex: "0",
    title: n.number + (n.code ? "/" + n.code : "") + " · " + (n.name || "") +
      "\n" + (n.type === "assembly" ? "Open its graph" : "Open the overview"),
    on: {
      click: function (e) { e.stopPropagation(); onOpen(n); },
      keydown: function (e) { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onOpen(n); } }
    }
  }, [
    thumb,
    h("div.g-cap", null, [
      h("span.g-name", { text: caption, title: caption }),
      h("span.g-sub", null, [h("span.g-dot"), h("span.g-num", { text: (n.number || "") + (n.code ? "/" + n.code : "") })])
    ]),
    badges.length ? h("div.g-badges", null, badges) : null,
    quantityChip(n)
  ]);
  return card;
}

/** "×4" in the top-left corner of a BOM child: how many instances of it the
 *  centre assembly contains (summed over its BOM lines), also for ×1, so every
 *  child reads the same. Null for references, users and the centre itself. */
function quantityChip(n) {
  if (n.rel !== "bom" || n.qty == null) return null;
  var q = Math.round(n.qty * 1e4) / 1e4; // units like m or kg keep their decimals
  return h("span.g-qty", { title: q + (q === 1 ? " instance" : " instances") + " in this assembly", "aria-label": "Quantity " + q }, "×" + q);
}

/** Draws the whole scene into the pane and wires zoom/pan. */
function draw(pane, center, nodes, onOpen) {
  var root = svg("svg", { class: "g-svg" });
  var defs = svg("defs");
  ["bom", "reference", "used-by"].forEach(function (rel) {
    var m = svg("marker", { id: "g-arrow-" + rel, viewBox: "0 0 8 8", refX: 7, refY: 4,
      markerWidth: 7, markerHeight: 7, orient: "auto-start-reverse" });
    m.appendChild(svg("path", { d: "M0 0 L8 4 L0 8 z", class: "g-arrow g-arrow-" + rel }));
    defs.appendChild(m);
  });
  root.appendChild(defs);
  var view = svg("g", { class: "g-view" });
  root.appendChild(view);
  var edges = svg("g", { class: "g-edges" });
  view.appendChild(edges);
  // the cards: an HTML layer in scene coordinates, transformed like `view`
  var layer = h("div.g-cards-layer");

  function cardNode(n, isCenter) {
    var holder = h("div.g-card");
    placeCard(holder, n);
    holder.appendChild(nodeCard(n, isCenter, onOpen));
    layer.appendChild(holder);
    n._card = holder;
  }

  function edge(n) {
    var up = n.dir === "up";
    var from = up ? n : center, to = up ? center : n;
    var line = svg("line", { x1: from.x, y1: from.y, x2: to.x, y2: to.y,
      class: "g-edge g-edge-" + n.rel, "marker-end": "url(#g-arrow-" + n.rel + ")" });
    edges.appendChild(line);
    n._edge = line;
  }

  nodes.forEach(edge);
  nodes.forEach(function (n) { cardNode(n, false); });
  cardNode(center, true);

  pane.appendChild(root);
  pane.appendChild(layer);

  // ---- zoom (mouse-centred) and pan ----
  var tf = { k: 1, x: 0, y: 0 };
  // once the user zooms or pans, late enrichment must not yank the view back;
  // a double-click on the background fits again on request.
  var userMoved = false;
  // the SVG group and the cards layer always get the same transform
  function apply() {
    view.setAttribute("transform", "translate(" + tf.x + "," + tf.y + ") scale(" + tf.k + ")");
    layer.style.transform = "translate(" + tf.x + "px, " + tf.y + "px) scale(" + tf.k + ")";
  }

  function fit() {
    var all = nodes.concat([center]);
    var minX = 0, minY = 0, maxX = 0, maxY = 0;
    all.forEach(function (n) {
      minX = Math.min(minX, n.x - n.size / 2); maxX = Math.max(maxX, n.x + n.size / 2);
      minY = Math.min(minY, n.y - n.size / 2); maxY = Math.max(maxY, n.y + n.size / 2);
    });
    var pad = 40;
    var w = pane.clientWidth || 800, hgt = pane.clientHeight || 600;
    var bw = (maxX - minX) + pad * 2, bh = (maxY - minY) + pad * 2;
    tf.k = clamp(Math.min(w / bw, hgt / bh), 0.25, 1.4);
    tf.x = w / 2 - ((minX + maxX) / 2) * tf.k;
    tf.y = hgt / 2 - ((minY + maxY) / 2) * tf.k;
    apply();
  }

  // The listeners sit on the pane, so they see events on the SVG, on the
  // empty layer (it lets them through) and on the cards (they bubble).
  pane.addEventListener("wheel", function (e) {
    e.preventDefault();
    // the SVG box: the scene origin of both the SVG and the cards layer
    // (inside the pane's border)
    var rect = root.getBoundingClientRect();
    var cx = e.clientX - rect.left, cy = e.clientY - rect.top;
    userMoved = true;
    var factor = Math.exp(-e.deltaY * 0.0015);
    var k2 = clamp(tf.k * factor, 0.25, 3.5);
    factor = k2 / tf.k;
    // keep the point under the cursor fixed
    tf.x = cx - (cx - tf.x) * factor;
    tf.y = cy - (cy - tf.y) * factor;
    tf.k = k2;
    apply();
  }, { passive: false });

  // Pan from anywhere, also from a card: a press that moves more than a few
  // pixels pans the graph and swallows the click that follows; a press that
  // stays put is a click and opens the card. The browser's own image drag
  // and text selection never start (mousedown/dragstart are cancelled, the
  // cards are user-select: none).
  var drag = null, swallowClick = false;
  var CLICK_SLOP = 4;
  pane.addEventListener("pointerdown", function (e) {
    if (e.button !== undefined && e.button !== 0) return;
    drag = { id: e.pointerId, x: e.clientX, y: e.clientY, ox: tf.x, oy: tf.y, moved: false };
    swallowClick = false;
  });
  pane.addEventListener("pointermove", function (e) {
    if (!drag || e.pointerId !== drag.id) return;
    var dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    if (!drag.moved) {
      if (Math.hypot(dx, dy) < CLICK_SLOP) return;
      drag.moved = true;
      // capture only once it is a drag, so a plain click still reaches the card
      try { pane.setPointerCapture(e.pointerId); } catch (err) { /* pointer already gone */ }
      root.classList.add("grabbing"); pane.classList.add("grabbing");
    }
    userMoved = true;
    tf.x = drag.ox + dx; tf.y = drag.oy + dy; apply();
  });
  function endDrag() {
    if (drag && drag.moved) swallowClick = true;
    drag = null; root.classList.remove("grabbing"); pane.classList.remove("grabbing");
  }
  pane.addEventListener("pointerup", endDrag);
  pane.addEventListener("pointercancel", function () { endDrag(); swallowClick = false; });
  // the click after a drag must not open or re-centre (capture phase: before the card sees it)
  pane.addEventListener("click", function (e) {
    if (!swallowClick) return;
    swallowClick = false;
    e.stopPropagation(); e.preventDefault();
  }, true);
  // no native image drag, no text selection when pressing on the graph
  pane.addEventListener("mousedown", function (e) { if (e.button === 0) e.preventDefault(); });
  pane.addEventListener("dragstart", function (e) { e.preventDefault(); });
  pane.addEventListener("dblclick", function (e) {
    if (e.target.closest(".g-node")) return;
    userMoved = false; fit();
  });

  // fit once the pane has a size (it was just inserted)
  requestAnimationFrame(fit);
  return { refit: function () { if (!userMoved) fit(); } };
}

// -------------------------------------------------------------------------
// The sub-tab entry point and the shared back control.
// -------------------------------------------------------------------------

/** Pushes the tab's graph state onto `tab` (the navigation stack of centres).*/
function graphState(tab) {
  if (!tab.graph) {
    var r = tab.data.rev;
    tab.graph = { stack: [{ number: r.item_number, code: r.code, name: r.name, type: r.item_type }] };
  }
  return tab.graph;
}

/** Re-centres the graph on an item within the same tab. */
function recenter(tab, n) {
  tab.graph.stack.push({ number: n.number, code: n.code, name: n.name, type: n.type });
  renderActive();
}

/** Opens a non-assembly node's overview, remembering the way back to the graph. */
function openOverview(tab, n) {
  var origin = { key: tab.key, stack: tab.graph.stack.slice() };
  var partKey = revKey(n.number, n.code);
  var t = findTab(partKey);
  if (t) t.returnToGraph = origin; // set before openTab renders it
  openTab({ key: partKey, kind: "revision", number: n.number, code: n.code,
    title: n.number + "/" + n.code, sub: "overview", returnToGraph: origin });
}

// the key openRevision() (revision.js) gives a revision tab
function revKey(number, code) { return "rev:" + number + ":" + code; }

function onOpen(tab) {
  return function (n) {
    var info = nodeInfo[key(n.number, n.code)];
    var type = (info && info.type) || n.type;
    // assemblies (and, until typed, things that clearly have a structure) stay in the graph
    if (type === "assembly" || (!type && n.complexity)) recenter(tab, n);
    else openOverview(tab, n);
  };
}

/** The Graph sub-tab. */
export function revGraph(tab, el) {
  var g = graphState(tab);
  var current = g.stack[g.stack.length - 1];
  // every render gets a token; answers for an older render (the user went
  // back, re-centred or left the tab meanwhile) are dropped.
  var gen = g.gen = (g.gen || 0) + 1;
  function live() { return tab.sub === "graph" && tab.graph === g && g.gen === gen && pane.isConnected; }

  var pane = h("div.graph-pane");
  pane.appendChild(h("div.g-loading", { text: "Loading the graph of " + current.number + "/" + current.code + " …" }));
  el.appendChild(legend());
  el.appendChild(pane);

  loadCenter(tab, current.number, current.code).then(function (d) {
    if (!live()) return; // moved away while loading
    pane.textContent = "";
    var center = {
      number: d.rev.item_number, code: d.rev.code, name: d.rev.name, type: d.rev.item_type,
      state: d.rev.state, complexity: (d.bom || []).length, dir: "center", rel: "center"
    };
    var centerThumb = (d.files || []).filter(function (f) { return f.format_info && f.format_info.thumbnail; })[0];
    if (centerThumb) { center.thumbId = centerThumb.id; center.thumbSha = centerThumb.sha256; center.fileName = centerThumb.filename; }
    nodeInfo[key(center.number, center.code)] = { type: center.type, complexity: center.complexity,
      fileName: center.fileName, thumbId: center.thumbId || null, thumbSha: center.thumbSha || null };
    var nodes = neighbours(d);
    center.size = nodeSize(center.complexity, true);
    nodes.forEach(function (n) { n.size = nodeSize(n.complexity, false); });

    if (!nodes.length) {
      layout(center, nodes);
      draw(pane, center, nodes, onOpen(tab));
      pane.appendChild(h("div.g-empty", { text: current.number + " has no BOM children or references to show." }));
      return;
    }

    // first paint with what we know; grow + fill previews as details arrive.
    // Enrichment resolves many neighbours at once, so coalesce the re-layout
    // into one frame instead of redrawing per result.
    layout(center, nodes);
    var view = draw(pane, center, nodes, onOpen(tab));
    var pending = false;
    var skipped = enrich(nodes, function () {
      if (pending) return;
      pending = true;
      requestAnimationFrame(function () {
        pending = false;
        if (!live()) return;
        nodes.forEach(function (n) { n.size = nodeSize(n.complexity, false); });
        layout(center, nodes);
        redraw(center, nodes, onOpen(tab));
        view.refit();
      });
    }, live);
    if (skipped) {
      pane.appendChild(h("div.g-empty", { text: skipped + " of " + nodes.length +
        " neighbours shown without preview and complexity – open one to see its details." }));
    }
  }, function (err) {
    if (!live()) return;
    pane.textContent = "";
    pane.appendChild(h("p.notice.error", { text: "Could not load the graph: " + (err && err.message ? err.message : err) }));
  });
}

/** Places a card holder in scene coordinates: its centre at (x, y). */
function placeCard(holder, n) {
  holder.style.left = (n.x - n.size / 2) + "px";
  holder.style.top = (n.y - n.size / 2) + "px";
  holder.style.width = n.size + "px";
  holder.style.height = n.size + "px";
}

/** Re-positions and re-skins the existing cards/edges after enrichment. */
function redraw(center, nodes, open) {
  nodes.concat([center]).forEach(function (n) {
    if (!n._card) return;
    placeCard(n._card, n);
    var hadFocus = n._card.contains(document.activeElement);
    var fresh = nodeCard(n, n.dir === "center", open);
    n._card.textContent = ""; n._card.appendChild(fresh);
    if (hadFocus) fresh.focus({ preventScroll: true });
  });
  nodes.forEach(function (n) {
    if (!n._edge) return;
    var up = n.dir === "up";
    var from = up ? n : center, to = up ? center : n;
    n._edge.setAttribute("x1", from.x); n._edge.setAttribute("y1", from.y);
    n._edge.setAttribute("x2", to.x); n._edge.setAttribute("y2", to.y);
  });
}

function backChip(text, title, onClick) {
  return h("button.graph-back", { type: "button", title: title, on: { click: onClick } }, text);
}

/** The back control for the sub-tab strip, in one fixed place for every tab:
 *  "← Graph" on an item opened from a graph (returns to that graph, even in a
 *  different tab), or "← Back" when the graph itself has been navigated into a
 *  sub-assembly. Null when there is nowhere to go back to. revision.js renders
 *  it as the first item of the sub-tab row so it never moves. */
export function graphBackControl(tab) {
  if (tab.sub === "graph" && tab.graph && tab.graph.stack.length > 1) {
    return backChip("← Back", "Back to the previous assembly in the graph", function () {
      tab.graph.stack.pop(); renderActive();
    });
  }
  if (tab.returnToGraph) {
    return backChip("← Graph", "Back to the assembly graph", function () { returnToGraph(tab); });
  }
  return null;
}

function returnToGraph(tab) {
  var o = tab.returnToGraph;
  var g = findTab(o.key);
  if (g) { g.sub = "graph"; g.graph = { stack: o.stack.slice() }; activate(o.key); return; }
  // the graph's tab was closed: reopen it on the assembly the graph showed
  var top = o.stack[o.stack.length - 1];
  var k = revKey(top.number, top.code);
  var t = findTab(k);
  if (t) t.graph = { stack: o.stack.slice() };
  openTab({ key: k, kind: "revision", number: top.number, code: top.code,
    title: top.number + "/" + top.code, sub: "graph", graph: { stack: o.stack.slice() } });
}

function legend() {
  function sw(cls, text) { return h("span.g-legend-item", null, [h("span.g-legend-dot." + cls), text]); }
  return h("div.graph-legend", null, [
    h("span.g-legend-label", { text: "Colour = state:" }),
    sw("state-in_work", label("in_work")),
    sw("state-in_review", label("in_review")),
    sw("state-released", label("released")),
    sw("state-obsolete", label("obsolete")),
    h("span.g-legend-sep"),
    h("span.g-legend-label", { text: "Size = BOM complexity · ⚙ children · ×n instances in this assembly · click a node to open it · double-click the background to fit" })
  ]);
}
