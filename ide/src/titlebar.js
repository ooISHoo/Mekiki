// 枠無しウィンドウのタイトルバー操作。
//
// Windows の標準タイトルバーには HTML を置けないので、
// decorations を外して同じ行を自前で描いている。

import { getCurrentWindow } from "@tauri-apps/api/window";

import { t } from "./i18n/index.js";

const el = (id) => document.getElementById(id);

/// 最小化 / 最大化 / 閉じるを繋ぎ、最大化状態に合わせて見た目を合わせる。
export async function attachTitlebar() {
  const win = getCurrentWindow();

  el("titlebar-minimize").onclick = () => {
    win.minimize();
  };
  el("titlebar-maximize").onclick = () => {
    win.toggleMaximize();
  };
  el("titlebar-close").onclick = () => {
    win.close();
  };

  await syncMaximized(win);
  await win.onResized(() => {
    syncMaximized(win);
  });
  await win.onFocusChanged(({ payload: focused }) => {
    document.body.classList.toggle("window-blurred", !focused);
  });
}

export async function syncMaximized(win = getCurrentWindow()) {
  let maximized = false;
  try {
    maximized = await win.isMaximized();
  } catch {
    return;
  }
  document.body.classList.toggle("maximized", maximized);
  const key = maximized ? "titlebar.restore.hint" : "titlebar.maximize.hint";
  const btn = el("titlebar-maximize");
  const label = t(key);
  btn.title = label;
  btn.setAttribute("aria-label", label);
  delete btn.dataset.i18nTitle;
  delete btn.dataset.i18nAriaLabel;
}

export function setWindowTitle(title) {
  getCurrentWindow()
    .setTitle(title)
    .catch(() => {
      // ブラウザ単体では窓タイトルは変えられない。
    });
}
