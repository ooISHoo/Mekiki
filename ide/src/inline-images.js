// 画像リテラルをインラインのサムネイルに差し替える CodeMirror 拡張。
//
// 設計上の注意点は docs/architecture/ide.md に記録している。
// 押さえどころは 3 つ。
//
//   1. 検出は Rust 側（mekiki_scripting::scan）に任せる。
//      正規表現だとコメント中や代入右辺の画像パスまで拾ってしまう。
//   2. atomicRanges を提供する。無いとカーソルがリテラル内部を 1 文字ずつ動く。
//   3. WidgetType.eq を実装する。無いと再描画のたびに img を作り直す。

import { StateEffect, StateField, RangeSetBuilder } from "@codemirror/state";
import { Decoration, EditorView, WidgetType } from "@codemirror/view";

import { t } from "./i18n/index.js";

/// 検出結果を差し込むためのエフェクト。
export const setImageLiterals = StateEffect.define();

/// 参照 -> data URL。解決済みのものを覚えておく。
const thumbnails = new Map();

export function cacheThumbnail(reference, dataUrl) {
  thumbnails.set(reference, dataUrl);
}

export function hasThumbnail(reference) {
  return thumbnails.has(reference);
}

export function clearThumbnails() {
  thumbnails.clear();
}

class ImageWidget extends WidgetType {
  constructor(reference, raw) {
    super();
    this.reference = reference;
    this.raw = raw;
    this.dataUrl = thumbnails.get(reference) ?? null;
  }

  // これが無いと、無関係な再描画のたびに DOM を作り直して重くなる。
  eq(other) {
    return other.reference === this.reference && other.dataUrl === this.dataUrl;
  }

  toDOM() {
    const wrap = document.createElement("span");
    wrap.className = "mekiki-thumb" + (this.dataUrl ? "" : " missing");
    wrap.title = this.raw;

    if (this.dataUrl) {
      const img = document.createElement("img");
      img.src = this.dataUrl;
      img.alt = this.reference;
      wrap.appendChild(img);
    }

    const label = document.createElement("span");
    label.className = "name";
    label.textContent = this.dataUrl
      ? shortName(this.reference)
      : t("image.notFound", shortName(this.reference));
    wrap.appendChild(label);
    return wrap;
  }

  // ウィジェット内のクリックをエディタに渡す。
  // 渡さないと、サムネイルを押してもカーソルが移動しない。
  ignoreEvent() {
    return false;
  }
}

function shortName(reference) {
  if (reference.startsWith("sha256:")) {
    return reference.slice(7, 15) + "…";
  }
  const parts = reference.split(/[\\/]/);
  return parts[parts.length - 1].replace(/\.(png|jpe?g|bmp|gif|webp)$/i, "");
}

function build(literals) {
  const builder = new RangeSetBuilder();
  // Rust 側は出現順で返すが、RangeSetBuilder は昇順を要求するので念のため並べる。
  const sorted = [...literals].sort((a, b) => a.from - b.from);
  for (const lit of sorted) {
    builder.add(
      lit.from,
      lit.to,
      Decoration.replace({ widget: new ImageWidget(lit.reference, lit.raw) }),
    );
  }
  return builder.finish();
}

export const imageLiteralField = StateField.define({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const effect of tr.effects) {
      if (effect.is(setImageLiterals)) {
        return build(effect.value);
      }
    }
    // 検出は非同期なので、文書が変わった直後は既存の装飾を位置だけ動かす。
    // ここで捨てるとサムネイルが 1 文字打つたびに消えて点滅する。
    return tr.docChanged ? deco.map(tr.changes) : deco;
  },
  provide: (f) => [
    EditorView.decorations.from(f),
    // 画像 1 個をカーソル移動・削除の 1 単位にする。
    EditorView.atomicRanges.of((view) => view.state.field(f)),
  ],
});

/// カーソル位置にある画像リテラルを返す。無ければ null。
export function literalAtCursor(view, literals) {
  const pos = view.state.selection.main.head;
  for (const lit of literals) {
    // 端も含める。サムネイルの直後にカーソルがある状態で拾えないと使いにくい。
    if (pos >= lit.from && pos <= lit.to) {
      return lit;
    }
  }
  return null;
}
