import test from "node:test";
import assert from "node:assert/strict";

import { rhaiString, windowSpecFor, windowSpecValue, windowSelectionFor } from "../src/window-spec.js";

test("a unique executable produces the stable short form", () => {
  const window = { exe: "Editor.exe", title: "Report", class_name: "Editor" };
  assert.equal(windowSpecFor(window, [window]), 'window("exe=Editor.exe")');
});

test("a duplicate executable escapes an exact title", () => {
  const window = {
    exe: "Editor.exe",
    title: "Report, Q1 \\ Draft *",
    class_name: "Editor",
  };
  assert.equal(
    windowSpecFor(window, [window, { ...window, title: "Other" }]),
    'window("exe=Editor.exe,title_exact=Report\\\\, Q1 \\\\\\\\ Draft *")',
  );
});

test("a title fallback cannot be mistaken for structured syntax", () => {
  const window = { exe: "", title: "A=B, C", class_name: "" };
  assert.equal(
    windowSpecFor(window, [window]),
    'window("title_exact=A=B\\\\, C")',
  );
});

test("window and Rhai escaping are separate layers", () => {
  assert.equal(windowSpecValue("A,B\\C"), "A\\,B\\\\C");
  assert.equal(rhaiString('A\\,B"'), '"A\\\\,B\\""');
});

test("a shared host uses its exact title regardless of other hosted windows", () => {
  const calculator = { exe: "APPLICATIONFRAMEHOST.EXE", title: "電卓", class_name: "ApplicationFrameWindow" };
  const other = { ...calculator, title: "Photos" };
  for (const windows of [[calculator], [calculator, other]]) {
    assert.deepEqual(windowSelectionFor(calculator, windows), {
      snippet: 'window("title_exact=電卓")', ambiguous: false,
    });
  }
});

test("executable counting follows case-insensitive API matching", () => {
  const editor = { exe: "Editor.exe", title: "Report", class_name: "Editor" };
  assert.equal(windowSpecFor(editor, [editor, { ...editor, exe: "EDITOR.EXE", title: "Other" }]),
    'window("exe=Editor.exe,title_exact=Report")');
});

test("unavailable executable prefers title over a shared class", () => {
  const window = { exe: "", title: "Report", class_name: "Shared" };
  assert.equal(windowSpecFor(window, [window]), 'window("title_exact=Report")');
});

test("class narrows duplicate titles when it distinguishes the target", () => {
  const window = { exe: "", title: "Report", class_name: "Editor" };
  assert.deepEqual(windowSelectionFor(window, [window, { ...window, class_name: "Viewer" }]), {
    snippet: 'window("title_exact=Report,class=Editor")', ambiguous: false,
  });
});

test("class prefix collisions remain ambiguous without adding a transient selector", () => {
  const window = { exe: "Editor.exe", title: "Report", class_name: "Editor" };
  assert.deepEqual(windowSelectionFor(window, [window, { ...window, class_name: "EditorChild" }]), {
    snippet: 'window("exe=Editor.exe,title_exact=Report")', ambiguous: true,
  });
});

test("shared-host title collisions include ordinary applications", () => {
  const window = { exe: "ApplicationFrameHost.exe", title: "Calculator", class_name: "Frame" };
  assert.equal(windowSelectionFor(window, [window, { ...window, exe: "Calculator.exe" }]).ambiguous, true);
});
