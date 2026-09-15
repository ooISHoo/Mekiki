// IDE の設定。localStorage に持ち、適用で反映する。
//
// ダイアログは `show()`（非モーダル）。メイン窓の編集や実行を止めない。

import {
  applyStaticText,
  localePreference,
  locales,
  setLocalePreference,
  t,
} from "./i18n/index.js";
import { syncEditorTheme } from "./cm-theme.js";

const THEME_KEY = "mekiki.theme";
const FONT_KEY = "mekiki.editorFontSize";
const FONT_MIN = 11;
const FONT_MAX = 22;
const FONT_DEFAULT = 13;

function readStored(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStored(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // 保存できなくても今のセッションには反映する。
  }
}

function readTheme() {
  const v = readStored(THEME_KEY);
  return v === "light" || v === "dark" ? v : "system";
}

function readFontSize() {
  const n = Number.parseInt(readStored(FONT_KEY) ?? "", 10);
  if (!Number.isFinite(n)) return FONT_DEFAULT;
  return Math.min(FONT_MAX, Math.max(FONT_MIN, n));
}

export function applyTheme(theme, view = null) {
  if (theme === "light" || theme === "dark") {
    document.documentElement.dataset.theme = theme;
  } else {
    delete document.documentElement.dataset.theme;
  }
  syncEditorTheme(view);
}

export function applyEditorFont(size) {
  document.documentElement.style.setProperty("--editor-font-size", `${size}px`);
}

applyTheme(readTheme());
applyEditorFont(readFontSize());

function fillForm() {
  const language = document.getElementById("settings-language");
  const theme = document.getElementById("settings-theme");
  const font = document.getElementById("settings-editor-font");
  if (language) language.value = localePreference();
  if (theme) theme.value = readTheme();
  if (font) font.value = String(readFontSize());
}

function applyFromForm(view) {
  const language = document.getElementById("settings-language")?.value ?? "system";
  const theme = document.getElementById("settings-theme")?.value ?? "system";
  const fontRaw = Number.parseInt(
    document.getElementById("settings-editor-font")?.value ?? "",
    10,
  );
  const font = Number.isFinite(fontRaw)
    ? Math.min(FONT_MAX, Math.max(FONT_MIN, fontRaw))
    : FONT_DEFAULT;

  if (language === "system" || locales().includes(language)) {
    setLocalePreference(language, view);
  }

  if (theme === "system") {
    try {
      localStorage.removeItem(THEME_KEY);
    } catch {
      // 保存できなくても見た目は変える。
    }
  } else {
    writeStored(THEME_KEY, theme);
  }
  applyTheme(theme, view);

  writeStored(FONT_KEY, String(font));
  applyEditorFont(font);

  applyStaticText();
}

function positionDialog(dialog) {
  if (dialog.dataset.moved === "1") return;
  const toolbar = document.querySelector(".toolbar");
  const top = (toolbar?.getBoundingClientRect().bottom ?? 72) + 12;
  dialog.style.top = `${top}px`;
  dialog.style.right = "16px";
  dialog.style.left = "auto";
}

function enableDrag(dialog) {
  const head = dialog.querySelector(".settings-head");
  if (!head || head.dataset.dragReady === "1") return;
  head.dataset.dragReady = "1";

  let drag = null;
  head.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    if (event.target.closest("button")) return;
    const box = dialog.getBoundingClientRect();
    drag = { dx: event.clientX - box.left, dy: event.clientY - box.top };
    head.setPointerCapture(event.pointerId);
  });
  head.addEventListener("pointermove", (event) => {
    if (!drag) return;
    const x = Math.min(
      window.innerWidth - dialog.offsetWidth - 8,
      Math.max(8, event.clientX - drag.dx),
    );
    const y = Math.min(
      window.innerHeight - dialog.offsetHeight - 8,
      Math.max(8, event.clientY - drag.dy),
    );
    dialog.style.left = `${x}px`;
    dialog.style.top = `${y}px`;
    dialog.style.right = "auto";
    dialog.dataset.moved = "1";
  });
  head.addEventListener("pointerup", () => {
    drag = null;
  });
}

/// 非モーダルの設定窓を出す。既に開いていれば前面へ。
export function openSettings(view, onApplied) {
  const dialog = document.getElementById("settings-dialog");
  if (!dialog) return;

  applyStaticText(dialog);
  fillForm();
  enableDrag(dialog);

  if (!dialog.open) {
    positionDialog(dialog);
    dialog.show();
  }

  dialog.querySelector("#settings-apply").onclick = () => {
    applyFromForm(view);
    fillForm();
    onApplied?.();
  };
  dialog.querySelector("#settings-close").onclick = () => {
    dialog.close();
  };
}
