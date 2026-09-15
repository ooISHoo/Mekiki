// ペイン境界のドラッグ。
//
// # 作りの要点
//
// 1. **grid のトラック長を CSS 変数で持ち、ドラッグ中はその変数だけを書き換える。**
//    要素の width/height を直接いじると、隣のトラックとの辻褄を自分で
//    合わせることになる。変数にしておけばブラウザが面倒を見る。
// 2. **利用者が動かすまで変数を書かない。** 既定値は CSS 側にだけ置いてある
//    （下段なら「窓の高さの 34%、ただし 110〜340px」）。起動時に実測して
//    px で焼き付けると、**そのときの窓の大きさに固定されてしまう**。
//    実際、小さい窓で開いたセッションで下段が下限に張り付いた。
// 3. **主ペインに必ず残す量を決めておく。** これが無いと、行きすぎたドラッグで
//    エディタが 0 まで潰れて戻せなくなる。
// 4. **窓を縮めたときに詰め直す。** ドラッグで px 固定になった後に窓を縮めると、
//    また主ペインが潰れうる。

/// 主ペインに必ず残す量。これ以上は詰められない。
const MIN_PRIMARY = 160;

/// キーボード操作 1 回あたりの移動量。
const KEY_STEP = 16;

/// 境界を 1 つ有効にする。
///
/// - `handle` … つまむ要素。動かす対象は**その次の要素**
/// - `container` … トラックを持つ grid 要素。CSS 変数はここに載せる
/// - `axis` … `"horizontal"` なら下段の高さ、`"vertical"` なら右ペインの幅
/// - `cssVar` … 書き換える CSS 変数名。既定値は CSS 側の `var()` 第 2 引数
/// - `min` … 従ペイン（下段・右ペイン）の下限
/// - `storageKey` … 位置の保存先。省略すると保存しない
/// - `onChange` … 変わったあとに呼ばれる（CodeMirror の再計測など）
export function attachSplitter({
  handle,
  container,
  axis,
  cssVar,
  min = 0,
  storageKey,
  onChange,
}) {
  const isHorizontal = axis === "horizontal";
  const pane = handle.nextElementSibling;

  const rect = () => container.getBoundingClientRect();

  /// 従ペインに割ける最大。主ペインの取り分を先に確保する。
  ///
  /// ハンドル自身もトラックを 1 本使っているので、その分を差し引く。
  /// 引かないと `MIN_PRIMARY` がハンドルの太さだけ目減りする。
  const maxFor = (r) => {
    const h = handle.getBoundingClientRect();
    const total = isHorizontal ? r.height : r.width;
    const handleSize = isHorizontal ? h.height : h.width;
    return Math.max(min, total - handleSize - MIN_PRIMARY);
  };

  const clamp = (size, r) =>
    Math.round(Math.min(maxFor(r), Math.max(min, size)));

  /// 利用者が明示的に決めた大きさを持っているか。
  const isPinned = () => container.style.getPropertyValue(cssVar) !== "";

  /// 今の実寸。変数が無いときは CSS の既定で描かれた結果を測る。
  const current = () => {
    const b = pane.getBoundingClientRect();
    return Math.round(isHorizontal ? b.height : b.width);
  };

  const announce = () => handle.setAttribute("aria-valuenow", String(current()));

  const apply = (size) => {
    container.style.setProperty(cssVar, `${size}px`);
    announce();
    onChange?.();
  };

  /// CSS の既定に戻す。変数を消すので、また窓の大きさに追従するようになる。
  const reset = () => {
    container.style.removeProperty(cssVar);
    if (storageKey) clearStored(storageKey);
    announce();
    onChange?.();
  };

  // --- 初期値 ---
  //
  // 保存値があるときだけ変数を書く。無ければ CSS の既定に任せる。

  const stored = storageKey ? readStored(storageKey) : null;
  if (stored !== null) {
    apply(clamp(stored, rect()));
  } else {
    announce();
  }

  // --- ドラッグ ---

  const onPointerMove = (event) => {
    const r = rect();
    // 境界は常に「奥側の端からの距離」で決まる。
    // 下段なら下端から、右ペインなら右端から。
    const size = isHorizontal ? r.bottom - event.clientY : r.right - event.clientX;
    apply(clamp(size, r));
  };

  const endDrag = () => {
    // 追従は window で受ける。`setPointerCapture` でも同じことができるが、
    // 掴んだ要素が消えた場合やイベントを合成した場合に捕捉が成立せず、
    // ドラッグが途中で切れる。window なら経路が 1 本で済む。
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", endDrag);
    window.removeEventListener("pointercancel", endDrag);
    handle.classList.remove("dragging");
    document.body.classList.remove("resizing");
    document.body.style.cursor = "";
    if (storageKey) writeStored(storageKey, current());
  };

  handle.addEventListener("pointerdown", (event) => {
    // 主ボタンのみ。右クリックや中クリックで掴まない。
    if (event.button !== 0) return;
    event.preventDefault();
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", endDrag);
    window.addEventListener("pointercancel", endDrag);
    handle.classList.add("dragging");
    // ドラッグ中はテキスト選択を止め、カーソル形状を全体で固定する。
    // これが無いと、境界から外れた瞬間にエディタの文字を選択し始め、
    // カーソルも I 字に変わって「掴んでいる」感じが切れる。
    document.body.classList.add("resizing");
    document.body.style.cursor = isHorizontal ? "row-resize" : "col-resize";
  });

  /// ダブルクリックで既定に戻す。
  handle.addEventListener("dblclick", reset);

  // --- キーボード ---
  //
  // ポインタが使えない場面でも動かせるようにしておく。
  // role="separator" と tabindex は index.html 側に付けてある。
  handle.addEventListener("keydown", (event) => {
    const grow = isHorizontal ? "ArrowUp" : "ArrowLeft";
    const shrink = isHorizontal ? "ArrowDown" : "ArrowRight";

    if (event.key === "Home") {
      event.preventDefault();
      reset();
      return;
    }

    let next = null;
    if (event.key === grow) next = current() + KEY_STEP;
    else if (event.key === shrink) next = current() - KEY_STEP;
    if (next === null) return;

    event.preventDefault();
    apply(clamp(next, rect()));
    if (storageKey) writeStored(storageKey, current());
  });

  // --- 入れ物の大きさが変わったとき ---
  //
  // ドラッグで px を決めたあとに窓を縮めると、主ペインがまた潰れる。
  // 保存値はそのままに、表示だけ現在の入れ物に収まるよう詰め直す。
  const refit = () => {
    // 決め打ちしていないなら CSS の既定に任せる。ここで書き込むと
    // 「一度も触っていないのに大きさが固定される」ことになる。
    if (!isPinned()) {
      announce();
      return;
    }
    const r = rect();
    const fitted = clamp(current(), r);
    // 変わるときだけ書く。無条件に書くと ResizeObserver が
    // 自分の変更を拾い続けてループ警告になる。
    if (fitted !== current()) apply(fitted);
  };

  // **両方に繋ぐ。** 片方だけでは取りこぼす環境を実際に踏んだ。
  //
  // - `ResizeObserver` … 窓以外の理由で入れ物が変わる場合を拾える。
  //   ただしフレームの生成に紐づくので、描画が止まっている状況では来ない
  // - `window` の resize … 入れ物ではなく窓の変化しか見ないが、
  //   ResizeObserver が来ない状況でも届くことがある
  //
  // どちらが先に来ても `refit` は冪等なので二重に走って困らない。
  new ResizeObserver(refit).observe(container);
  window.addEventListener("resize", refit);
}

function readStored(key) {
  try {
    const v = parseFloat(localStorage.getItem(key));
    return Number.isFinite(v) ? v : null;
  } catch {
    return null;
  }
}

function writeStored(key, value) {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // 保存できなくてもドラッグ自体は成立させる。
  }
}

function clearStored(key) {
  try {
    localStorage.removeItem(key);
  } catch {
    // 同上。
  }
}
