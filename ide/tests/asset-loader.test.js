import assert from "node:assert/strict";
import test from "node:test";

import { createAssetLoader } from "../src/asset-loader.js";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a result from the previous directory is stale after a newer load", async () => {
  const oldRequest = deferred();
  const newRequest = deferred();
  const loader = createAssetLoader((baseDir) =>
    baseDir === "old" ? oldRequest.promise : newRequest.promise,
  );

  const oldLoad = loader.load("old");
  const newLoad = loader.load("new");
  newRequest.resolve([{ reference: "new.png" }]);
  oldRequest.resolve([{ reference: "old.png" }]);

  assert.deepEqual(await newLoad, {
    kind: "ready",
    list: [{ reference: "new.png" }],
  });
  assert.deepEqual(await oldLoad, { kind: "stale" });
});

test("clearing an untitled document invalidates an in-flight load", async () => {
  const request = deferred();
  const loader = createAssetLoader(() => request.promise);

  const load = loader.load("previous-script");
  loader.invalidate();
  request.resolve([{ reference: "previous.png" }]);

  assert.deepEqual(await load, { kind: "stale" });
});

test("only the current load reports a backend failure", async () => {
  const oldRequest = deferred();
  const newRequest = deferred();
  const loader = createAssetLoader((baseDir) =>
    baseDir === "old" ? oldRequest.promise : newRequest.promise,
  );

  const oldLoad = loader.load("old");
  const newLoad = loader.load("new");
  oldRequest.reject(new Error("old failure"));
  newRequest.reject(new Error("new failure"));

  assert.deepEqual(await oldLoad, { kind: "stale" });
  const result = await newLoad;
  assert.equal(result.kind, "failed");
  assert.match(String(result.error), /new failure/);
});

