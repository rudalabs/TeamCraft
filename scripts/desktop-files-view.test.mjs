// SPDX-License-Identifier: MPL-2.0
// Unit tests of desktop/ui/js/files-view.js (the pure part of the Files view
// of Home). Plain Node.js, no packages; run by the CI job `ui`:
//
//   node scripts/desktop-files-view.test.mjs
//
// The module sits in a folder without package.json, so it is imported as a
// data: URL module instead of by path.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const source = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../desktop/ui/js/files-view.js");
const {
  compareNames, countText, entryIconType, entryName, entryRevision, entryTitle, folderKey,
  groupTotal, mergeEntries, remaining, sortEntries, sortFolders
} = await import("data:text/javascript;base64," + Buffer.from(fs.readFileSync(source, "utf8")).toString("base64"));

let passed = 0;
function test(name, fn) {
  fn();
  passed++;
  console.log("ok - " + name);
}

const file = (filename, more) => Object.assign({ file_id: "f-" + filename, filename, item_number: "PRT-000001",
  item_type: "part", name: filename.replace(/\.[^.]+$/, ""), code: "A", state: "in_work",
  format_kind: null, format_label: null, checked_out_by_name: null }, more || {});
const loose = (number, name, more) => Object.assign({ file_id: null, filename: null, item_number: number,
  item_type: "part", name, code: "A", state: "in_work", format_kind: null, format_label: null,
  checked_out_by_name: null }, more || {});

test("names sort like in a file manager: case-blind, digits by value", () => {
  const names = ["wheel.ipt", "Part10.ipt", "part2.ipt", "Axle.ipt", "Part1.ipt"];
  assert.deepEqual(names.slice().sort(compareNames), ["Axle.ipt", "Part1.ipt", "part2.ipt", "Part10.ipt", "wheel.ipt"]);
  assert.equal(compareNames("a", "a"), 0);
  assert.equal(compareNames(null, "a") < 0, true);
  // the same name in another case is a stable, different entry
  assert.notEqual(compareNames("Bracket.ipt", "bracket.ipt"), 0);
  assert.equal(compareNames("Bracket.ipt", "bracket.ipt"), -compareNames("bracket.ipt", "Bracket.ipt"));
});

test("folders by name", () => {
  const sorted = sortFolders([{ name: "wheels" }, { name: "Chassis" }, { name: "10 misc" }, { name: "2 frame" }]);
  assert.deepEqual(sorted.map((f) => f.name), ["2 frame", "10 misc", "Chassis", "wheels"]);
  assert.deepEqual(sortFolders(null), []);
});

test("by name: files first by file name, then the items without files – never by number", () => {
  const rows = [
    loose("PRT-000001", "Zinc plate"),
    file("rover.iam", { item_number: "ASM-000009" }),
    loose("PRT-000002", "bought motor"),
    file("Bracket.ipt", { item_number: "PRT-000900" }),
    file("axle.ipt", { item_number: "PRT-000500" })
  ];
  const want = ["axle.ipt", "Bracket.ipt", "rover.iam", "bought motor", "Zinc plate"];
  assert.deepEqual(sortEntries(rows, "name").map(entryName), want);
  // no order or an unknown one is the order by name
  assert.deepEqual(sortEntries(rows).map(entryName), want);
  assert.deepEqual(sortEntries(rows, "size").map(entryName), want);
  // the input is left alone
  assert.equal(entryName(rows[0]), "Zinc plate");
  // two files of the same name (two items): by number, so the order is stable
  const twins = sortEntries([file("a.ipt", { file_id: "2", item_number: "PRT-000002" }), file("a.ipt", { file_id: "1", item_number: "PRT-000001" })]);
  assert.deepEqual(twins.map((e) => e.item_number), ["PRT-000001", "PRT-000002"]);
  assert.deepEqual(sortEntries(null), []);
});

test("the other orders of Home: number, type, state – files stay before the items without files", () => {
  const rows = [
    file("c.ipt", { item_number: "PRT-000003", state: "released" }),
    file("a.iam", { item_number: "ASM-000002", item_type: "assembly", format_kind: "assembly", state: "in_work" }),
    file("b.idw", { item_number: "PRT-000001", format_kind: "drawing", state: "in_review" }),
    file("d.ipt", { item_number: "PRT-000001", state: "in_work" }),
    loose("AAA-000001", "Loose", { state: "in_work" })
  ];
  assert.deepEqual(sortEntries(rows, "number").map(entryName), ["a.iam", "b.idw", "d.ipt", "c.ipt", "Loose"]);
  assert.deepEqual(sortEntries(rows, "type").map(entryName), ["a.iam", "b.idw", "c.ipt", "d.ipt", "Loose"]);
  assert.deepEqual(sortEntries(rows, "state").map(entryName), ["b.idw", "a.iam", "d.ipt", "c.ipt", "Loose"]);
});

test("a row is known by its file name; the revision follows", () => {
  assert.equal(entryName(file("Rover.iam")), "Rover.iam");
  assert.equal(entryName(loose("PRT-000007", "Bought motor")), "Bought motor");
  assert.equal(entryName({ item_number: "PRT-000007" }), "PRT-000007");
  assert.equal(entryRevision(file("x.ipt", { item_number: "PRT-000042", code: "B" })), "PRT-000042/B");
  assert.equal(entryRevision({ item_number: "PRT-000042" }), "PRT-000042");
});

test("the icon follows what the file contains, else the item type", () => {
  assert.equal(entryIconType(file("a.idw", { format_kind: "drawing", item_type: "part" })), "document");
  assert.equal(entryIconType(file("a.iam", { format_kind: "assembly", item_type: "part" })), "assembly");
  assert.equal(entryIconType(file("a.ipt", { format_kind: "part", item_type: "assembly" })), "part");
  assert.equal(entryIconType(file("a.step", { format_kind: "exchange", item_type: "assembly" })), "assembly");
  assert.equal(entryIconType(file("a.bin", { item_type: "software" })), "software");
  assert.equal(entryIconType(loose("DOC-000001", "Plan", { item_type: "document" })), "document");
});

test("the tooltip carries what the row leaves out", () => {
  const label = (s) => ({ in_work: "WIP", released: "Released" })[s];
  assert.equal(entryTitle(file("Bracket.ipt", { name: "Bracket", format_label: "Inventor part", state: "released" }), label),
    "PRT-000001/A – Bracket · Inventor part · Released");
  assert.equal(entryTitle(file("notes.txt", { name: "Notes", checked_out_by_name: "Lena" }), label),
    "PRT-000001/A – Notes · file · WIP · checked out by Lena");
  assert.equal(entryTitle(loose("PRT-000007", "Bought motor")), "PRT-000007/A – Bought motor · no files yet · in_work");
});

test("a further page is added once", () => {
  const a = file("a.ipt"), b = file("b.ipt"), c = file("c.ipt"), l = loose("PRT-000009", "Loose");
  assert.deepEqual(mergeEntries([a, b], [b, c, l]).map(entryName), ["a.ipt", "b.ipt", "c.ipt", "Loose"]);
  assert.deepEqual(mergeEntries([l], [l]).length, 1);
  assert.deepEqual(mergeEntries(null, [a]).length, 1);
  assert.equal(remaining(120, 50), 70);
  assert.equal(remaining(3, 5), 0);
  assert.equal(remaining(undefined, 0), 0);
});

test("the rows of a group: its top entries and the files in its folders", () => {
  assert.equal(groupTotal({ folders: [{ files: 521 }, { files: 3 }], total: 7 }), 531);
  assert.equal(groupTotal({ folders: [], total: 0 }), 0);
  assert.equal(groupTotal({}), 0);
});

test("folder keys keep the groups apart", () => {
  assert.equal(folderKey("ROVER", "chassis/brackets"), "dir:ROVER:chassis/brackets");
  assert.equal(folderKey("ROVER", ""), "dir:ROVER:");
  assert.notEqual(folderKey("ROVER", "a"), folderKey("ROCKET", "a"));
});

test("counts read naturally", () => {
  assert.equal(countText(1, "file", "files"), "1 file");
  assert.equal(countText(0, "file", "files"), "0 files");
  assert.equal(countText(12, "file", "files"), "12 files");
});

console.log(passed + " tests passed");
