// 実行中の行にしるしを付ける CodeMirror 拡張。
//
// 行番号は Rhai のデバッガから来る（`mekiki-scripting` の step モジュール）。
// ここは受け取った番号を装飾に変えるだけ。
//
// 行そのものに `Decoration.line` を付ける。文字範囲ではなく行に付けるので、
// 途中で行が編集されて長さが変わっても崩れない。

import { StateEffect, StateField } from "@codemirror/state";
import { Decoration, EditorView } from "@codemirror/view";

/// 実行中の行を差し替える。1 始まり。0 か null でしるしを消す。
export const setExecLine = StateEffect.define();

const execLineMark = Decoration.line({ class: "cm-exec-line" });

export const execLineField = StateField.define({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const effect of tr.effects) {
      if (!effect.is(setExecLine)) continue;

      const line = effect.value;
      if (!line || line < 1 || line > tr.state.doc.lines) {
        return Decoration.none;
      }
      const from = tr.state.doc.line(line).from;
      return Decoration.set([execLineMark.range(from)]);
    }
    // 編集が入ったら位置を追従させる。捨てるとしるしが点滅する。
    return tr.docChanged ? deco.map(tr.changes) : deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/// しるしを付け、必要なら見えるところまでスクロールする。
///
/// `scroll` は自動で追いかけるかどうか。ステップ実行では追いかけたいが、
/// 自由実行では**画面が飛び回って読めなくなる**ので既定は追わない。
export function showExecLine(view, line, { scroll = false } = {}) {
    const effects = [setExecLine.of(line)];
  if (scroll && line >= 1 && line <= view.state.doc.lines) {
    effects.push(EditorView.scrollIntoView(view.state.doc.line(line).from, {
      y: "center",
    }));
  }
  view.dispatch({ effects });
}
