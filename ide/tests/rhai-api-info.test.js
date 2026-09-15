import assert from "node:assert/strict";
import test from "node:test";

import {
  buildApiInfoModel,
  formatApiSignature,
} from "../src/rhai-api-info.js";

const items = [
  {
    owner: "",
    name: "screen",
    params: [],
    returns: "Region",
    summary: "Return the primary display.",
  },
  {
    owner: "",
    name: "screen",
    params: [{ name: "index", type: "int", description: "" }],
    returns: "Region",
    summary: "Return a display by index.",
  },
  {
    owner: "Target",
    name: "click",
    params: [],
    returns: "Match",
    summary: "Resolve and click.",
  },
  {
    owner: "Match",
    name: "click",
    params: [],
    returns: "unit",
    summary: "Click the stored point.",
  },
  {
    owner: "Match",
    name: "score",
    kind: "property",
    params: [],
    returns: "float",
    summary: "Return the similarity score.",
  },
];

test("formats global and member signatures", () => {
  assert.equal(formatApiSignature(items[1]), "screen(index: int) -> Region");
  assert.equal(formatApiSignature(items[2]), "Target.click() -> Match");
  assert.equal(formatApiSignature(items[4]), "Match.score -> float");
});

test("groups global overloads without including members", () => {
  const groups = buildApiInfoModel(items, "screen", "global");
  assert.equal(groups.length, 1);
  assert.equal(groups[0].owner, "");
  assert.deepEqual(
    groups[0].overloads.map((item) => item.signature),
    ["screen() -> Region", "screen(index: int) -> Region"],
  );
});

test("groups ambiguous members by owner", () => {
  const groups = buildApiInfoModel(items, "click", "member");
  assert.deepEqual(
    groups.map((group) => [group.owner, group.overloads[0].signature]),
    [
      ["Match", "Match.click() -> unit"],
      ["Target", "Target.click() -> Match"],
    ],
  );
});
