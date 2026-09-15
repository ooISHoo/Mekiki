// 表示文字列の多言語対応。
//
// 調査と方針は docs/architecture/ide.md。要点は 3 つ。
//
//   1. 外部ライブラリは入れない。文字列が 3 桁で複数形処理も要らないため、
//      i18next のような機構は釣り合わない。
//   2. 差し込み記法は CodeMirror 6 の `$1` に揃える。CodeMirror 自身の文言も
//      同じカタログから引くので、覚える記法を 1 つにする。
//   3. **日本語が原文**。他のロケールに登録が無いキーは日本語へ落ちる。
//      CodeMirror の `phrase()` が「登録が無ければ原文を返す」のと同じ考え方。

import { Compartment, EditorState } from "@codemirror/state";

import ja from "./ja.js";
import en from "./en.js";

const CATALOGS = { ja, en };

/// 原文のロケール。フォールバック先でもある。
const SOURCE = "ja";

/// ロケールの手動指定を覚えておく場所。
const STORAGE_KEY = "mekiki.locale";

let current = SOURCE;

// CodeMirror の phrases はロケール切り替えで差し替える必要があるので、
// Compartment に入れておく。これが無いとエディタを作り直すことになる。
const phrasesCompartment = new Compartment();

/// OS の表示言語からロケールを決める。
///
/// WebView2 は `navigator.language` に OS の表示言語を返すので、
/// これだけで足りる。対応が日英の 2 つなので地域サブタグ
/// （`zh-CN` と `zh-TW` の区別など）は見ない。
function detectFromOs() {
  const tag = (navigator.language || SOURCE).toLowerCase();
  const primary = tag.split("-")[0];
  return primary in CATALOGS ? primary : SOURCE;
}

function detect() {
  return readStored() ?? detectFromOs();
}

/// 設定画面用。未保存なら `"system"`。
export function localePreference() {
  return readStored() ?? "system";
}

/// `"system"` なら OS の表示言語へ戻す。
export function setLocalePreference(pref, view = null) {
  if (pref === "system") {
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      // 保存できなくても切り替え自体は成立させる。
    }
    return setLocale(detectFromOs(), view);
  }
  return setLocale(pref, view);
}

function readStored() {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v && v in CATALOGS ? v : null;
  } catch {
    // localStorage が使えない構成でも動くようにしておく。
    return null;
  }
}

/// 現在のロケール。
export function locale() {
  return current;
}

/// 使えるロケールの一覧。言語切り替え UI を付けるときに使う。
export function locales() {
  return Object.keys(CATALOGS);
}

/// 文字列を引く。
///
/// 差し込みは CodeMirror と同じ規則。`$1` が 1 つめ、`$` は `$1` と同じ、
/// `$$` はドル記号そのもの。差し込む値が無ければ置換自体を行わない。
export function t(key, ...insert) {
  let text = lookup(key);

  if (insert.length) {
    text = text.replace(/\$(\$|\d*)/g, (m, i) => {
      if (i === "$") return "$";
      const n = +(i || 1);
      return !n || n > insert.length ? m : insert[n - 1];
    });
  }
  return text;
}

function lookup(key) {
  const active = CATALOGS[current]?.ui;
  if (active && Object.prototype.hasOwnProperty.call(active, key)) {
    return active[key];
  }
  const source = CATALOGS[SOURCE].ui;
  if (Object.prototype.hasOwnProperty.call(source, key)) {
    return source[key];
  }
  // キーそのものを返す。空文字を返すと画面から文字が消えて、
  // 「翻訳漏れ」なのか「そもそも出ない要素」なのか区別が付かなくなる。
  console.warn(`i18n: 未知のキー '${key}'`);
  return key;
}

/// CodeMirror 自身が出す文言のための拡張。
///
/// `@codemirror/*` は画面に出す文字列を必ず `view.state.phrase("原文")`
/// 経由で出す。ここに登録しておけば、後から補完や検索を有効にしても
/// 配線を足す必要は無い。
export function phrases() {
  return phrasesCompartment.of(phrasesFacet());
}

function phrasesFacet() {
  return EditorState.phrases.of(CATALOGS[current]?.codemirror ?? {});
}

/// `data-i18n` の付いた要素に文字列を流し込む。
///
/// HTML 側に日本語を残すとカタログと二重管理になるので、
/// 文言は属性で指す形にしてある。
///
///   data-i18n            … 要素の本文
///   data-i18n-title      … title 属性（ツールチップ）
///   data-i18n-aria-label … aria-label 属性
export function applyStaticText(root = document) {
  for (const el of root.querySelectorAll("[data-i18n]")) {
    el.textContent = t(el.dataset.i18n);
  }
  for (const el of root.querySelectorAll("[data-i18n-title]")) {
    el.title = t(el.dataset.i18nTitle);
  }
  for (const el of root.querySelectorAll("[data-i18n-aria-label]")) {
    el.setAttribute("aria-label", t(el.dataset.i18nAriaLabel));
  }

  // 読み上げソフトと折り返し規則がこれを見る。
  document.documentElement.lang = current;
}

/// ロケールを切り替える。
///
/// 静的な文言と CodeMirror の文言はその場で入れ替わる。
/// ステータス行のような一時的な表示は次の操作まで前の言語のまま残る。
/// そこまで追うと全メッセージの再生成が要るわりに、
/// 実際に切り替えるのは設定を触る一瞬だけなので割に合わない。
export function setLocale(code, view = null) {
  if (!(code in CATALOGS)) {
    console.warn(`i18n: 未対応のロケール '${code}'`);
    return false;
  }
  current = code;
  try {
    localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // 保存できなくても切り替え自体は成立させる。
  }

  applyStaticText();
  if (view) {
    view.dispatch({ effects: phrasesCompartment.reconfigure(phrasesFacet()) });
  }
  return true;
}

/// 現ロケールに訳の無い `ui` のキーを列挙する。
///
/// 翻訳作業のときに「あと何が残っているか」を出すためのもの。
/// 開発中はコンソールから `__mekikiI18n.missingKeys("en")` で呼べる。
///
/// `codemirror` の区画は見ない。**あちらは原文が英語**なので、
/// 英語カタログに登録が無いのが正しい状態だから。
export function missingKeys(code = current) {
  const have = CATALOGS[code]?.ui ?? {};
  return Object.keys(CATALOGS[SOURCE].ui).filter(
    (key) => !Object.prototype.hasOwnProperty.call(have, key),
  );
}

current = detect();
