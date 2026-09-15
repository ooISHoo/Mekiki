import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import en from "../src/i18n/en.js";
import ja from "../src/i18n/ja.js";

const JAPANESE_TEXT = /[\u3040-\u30ff\u3400-\u9fff]/u;

function placeholders(text) {
  return [...text.matchAll(/\$(\$|\d*)/g)]
    .filter((match) => match[1] !== "$")
    .map((match) => Number(match[1] || 1))
    .sort((left, right) => left - right);
}

test("English UI catalog has every source key and no stale keys", () => {
  assert.deepEqual(Object.keys(en.ui).sort(), Object.keys(ja.ui).sort());
});

test("translations preserve interpolation placeholders", () => {
  for (const [key, source] of Object.entries(ja.ui)) {
    assert.deepEqual(
      placeholders(en.ui[key]),
      placeholders(source),
      `placeholder mismatch for ${key}`,
    );
  }
});

test("English UI resources contain no Japanese text", () => {
  for (const [key, value] of Object.entries(en.ui)) {
    assert.doesNotMatch(value, JAPANESE_TEXT, key);
  }
});

function sourceFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      return entry.name === "gen" ? [] : sourceFiles(path);
    }
    return [".json", ".rs", ".toml"].includes(extname(path)) ? [path] : [];
  });
}

test("Tauri backend sources contain no Japanese text", () => {
  const testDir = dirname(fileURLToPath(import.meta.url));
  const tauriDir = resolve(testDir, "../src-tauri");
  for (const path of sourceFiles(tauriDir)) {
    assert.doesNotMatch(readFileSync(path, "utf8"), JAPANESE_TEXT, path);
  }
});
