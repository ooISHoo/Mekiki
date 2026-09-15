// エディタのライト／ダーク切替。
//
// Rhai 専用の CodeMirror 6 テーマは見当たらない。公式の参考実装は
// `@codemirror/theme-one-dark`（One Dark）。カーソル #528bff、
// 行番号 #7d8799、背景 #282c34。

import { Compartment } from "@codemirror/state";
import { syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";

export const themeCompartment = new Compartment();

const THEME_KEY = "mekiki.theme";

export function prefersDark() {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === "dark") return true;
    if (stored === "light") return false;
  } catch {
    // 保存が読めなくても OS に従う。
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function editorThemeExt(dark = prefersDark()) {
  return dark
    ? oneDark
    : syntaxHighlighting(defaultHighlightStyle, { fallback: true });
}

export function syncEditorTheme(view) {
  if (!view) return;
  view.dispatch({
    effects: themeCompartment.reconfigure(editorThemeExt()),
  });
}
