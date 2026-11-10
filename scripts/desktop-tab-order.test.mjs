// SPDX-License-Identifier: MPL-2.0
// Unit tests of desktop/ui/js/tab-order.js (the pure rules of the tab order).
// Plain Node.js, no packages; run by the CI job `ui`:
//
//   node scripts/desktop-tab-order.test.mjs
//
// The module sits in a folder without package.json, so it is imported as a
// data: URL module instead of by path.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const file = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../desktop/ui/js/tab-order.js");
const src = fs.readFileSync(file, "utf8");
const { moveItem, moveBy, stepIndex, dropSlot, keysToClose } = await import(
  "data:text/javascript;base64," + Buffer.from(src).toString("base64"));

let n = 0;
function test(name, fn) { fn(); n++; console.log("ok - " + name); }

test("dragging C before A gives C A B", () => {
  assert.deepEqual(moveItem(["A", "B", "C"], 2, 0), ["C", "A", "B"]);
});
test("dragging A behind C (end slot) gives B C A", () => {
  assert.deepEqual(moveItem(["A", "B", "C"], 0, 3), ["B", "C", "A"]);
});
test("dragging A between B and C gives B A C", () => {
  assert.deepEqual(moveItem(["A", "B", "C"], 0, 2), ["B", "A", "C"]);
});
test("dropping on the own slot or the one after it changes nothing", () => {
  const l = ["A", "B", "C"];
  assert.equal(moveItem(l, 1, 1), l);
  assert.equal(moveItem(l, 1, 2), l);
  assert.equal(moveItem(l, 2, 3), l);
});
test("out-of-range arguments change nothing and never mutate", () => {
  const l = ["A", "B"];
  assert.equal(moveItem(l, -1, 0), l);
  assert.equal(moveItem(l, 2, 0), l);
  assert.equal(moveItem(l, 0, 3), l);
  assert.deepEqual(l, ["A", "B"]);
});
test("Ctrl+Shift+PageUp on B in A B C gives B A C; at the start nothing", () => {
  const l = ["A", "B", "C"];
  assert.deepEqual(moveBy(l, 1, -1), ["B", "A", "C"]);
  assert.equal(moveBy(l, 0, -1), l);
});
test("Ctrl+Shift+PageDown moves right; at the end nothing", () => {
  const l = ["A", "B", "C"];
  assert.deepEqual(moveBy(l, 1, 1), ["A", "C", "B"]);
  assert.equal(moveBy(l, 2, 1), l);
});
test("single and empty lists", () => {
  assert.deepEqual(moveBy(["A"], 0, 1), ["A"]);
  assert.deepEqual(moveBy([], 0, 1), []);
});
test("stepIndex wraps and copes with no active tab", () => {
  assert.equal(stepIndex(0, 3, -1), 2);
  assert.equal(stepIndex(2, 3, 1), 0);
  assert.equal(stepIndex(1, 3, 1), 2);
  assert.equal(stepIndex(-1, 3, 1), 0);
  assert.equal(stepIndex(-1, 3, -1), 2);
  assert.equal(stepIndex(0, 0, 1), -1);
});
test("dropSlot from tab midpoints", () => {
  const mids = [50, 150, 250];
  assert.equal(dropSlot(mids, 10), 0);
  assert.equal(dropSlot(mids, 100), 1);
  assert.equal(dropSlot(mids, 200), 2);
  assert.equal(dropSlot(mids, 900), 3);
  assert.equal(dropSlot([], 5), 0);
});
test("keysToClose", () => {
  const k = ["a", "b", "c", "d"];
  assert.deepEqual(keysToClose(k, "b", "this"), ["b"]);
  assert.deepEqual(keysToClose(k, "b", "others"), ["a", "c", "d"]);
  assert.deepEqual(keysToClose(k, "b", "right"), ["c", "d"]);
  assert.deepEqual(keysToClose(k, "d", "right"), []);
  assert.deepEqual(keysToClose(k, "x", "others"), []);
});
console.log(n + " tests passed");
