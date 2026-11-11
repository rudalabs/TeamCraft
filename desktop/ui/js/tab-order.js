// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Pure rules for the order of the object tabs.
// No DOM, no state: unit-tested by scripts/desktop-tab-order.test.mjs (plain
// Node.js, run by the CI job `ui`). js/tabs.js applies the results.

/**
 * `list` with the entry at `from` moved so that it sits before the entry
 * that was at `to` (`to` = list.length: the end). `to` is a drop slot as the
 * pointer sees it: 0..length, counted in the list before the move. Returns
 * the same array when nothing changes (dropped on its own slot or the slot
 * after it), else a new one.
 */
export function moveItem(list, from, to) {
  var n = list.length;
  if (from < 0 || from >= n || to < 0 || to > n) return list;
  if (to === from || to === from + 1) return list;
  var out = list.slice();
  var item = out.splice(from, 1)[0];
  out.splice(to > from ? to - 1 : to, 0, item);
  return out;
}

/** The entry at `index` moved one place (`delta` -1 left, +1 right); the same array at an end. */
export function moveBy(list, index, delta) {
  var target = index + delta;
  if (index < 0 || index >= list.length || target < 0 || target >= list.length) return list;
  return moveItem(list, index, delta < 0 ? target : target + 1);
}

/** Index of the neighbour `delta` steps away, wrapping around; -1 for an empty list. */
export function stepIndex(index, count, delta) {
  if (count <= 0) return -1;
  if (index < 0 || index >= count) return delta < 0 ? count - 1 : 0;
  return (((index + delta) % count) + count) % count;
}

/**
 * The drop slot (0..mids.length) for a pointer at `x`, given the horizontal
 * midpoints of the tabs in order: the slot before the first tab whose
 * midpoint lies right of the pointer.
 */
export function dropSlot(mids, x) {
  for (var i = 0; i < mids.length; i++) if (x < mids[i]) return i;
  return mids.length;
}

/**
 * Keys a context-menu close removes: mode "this" (just `key`), "others"
 * (all but `key`) or "right" (the tabs after `key`). Unknown key: none.
 */
export function keysToClose(keys, key, mode) {
  var i = keys.indexOf(key);
  if (i === -1) return [];
  if (mode === "others") return keys.filter(function (k) { return k !== key; });
  if (mode === "right") return keys.slice(i + 1);
  return [key];
}
