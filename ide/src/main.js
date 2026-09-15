// Mekiki IDE のフロントエンド。
//
// バックエンド（Tauri コマンド）は ide/src-tauri/src/lib.rs にある。
// コアエンジンは同一プロセスに直接リンクされているので、
// ここからの呼び出しはプロセス境界を跨がない。

import { EditorState } from "@codemirror/state";
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  drawSelection,
  dropCursor,
  rectangularSelection,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentLess, insertTab } from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import { themeCompartment, editorThemeExt, syncEditorTheme } from "./cm-theme.js";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog, message } from "@tauri-apps/plugin-dialog";

import {
  imageLiteralField,
  setImageLiterals,
  cacheThumbnail,
  clearThumbnails,
  hasThumbnail,
  literalAtCursor,
} from "./inline-images.js";
import {
  applyStaticText,
  locale,
  locales,
  missingKeys,
  phrases,
  setLocale,
  t,
} from "./i18n/index.js";
import { attachSplitter } from "./splitter.js";
import { execLineField, showExecLine } from "./exec-line.js";
import { rhaiCompletion, completionKeymap } from "./rhai-complete.js";
import { attachTitlebar, setWindowTitle, syncMaximized } from "./titlebar.js";
import { openSettings } from "./settings.js";
import { windowSelectionFor } from "./window-spec.js";
import { createAssetLoader } from "./asset-loader.js";

const el = (id) => document.getElementById(id);

/// `data-i18n` の付いた要素に動的な文言を入れる。
///
/// 属性を外すのが要点。外さないと、ロケールを切り替えたときに
/// `applyStaticText` が初期文言へ戻してしまう
/// （実行結果が出ているステータス行が「準備中…」に戻る）。
function setDynamicText(element, text) {
  delete element.dataset.i18n;
  element.textContent = text;
}

const state = {
  baseDir: ".",
  defaultBaseDir: ".",
  path: null,
  literals: [],
  dirty: false,
  /// 右ペインで選んでいる画像。マッチ確認の対象にもなる。
  selectedAsset: null,
  /// 実行状態。`idle` / `running` / `paused` / `stopping`。
  run: "idle",
  /// ステップ実行を使うか。実行の前後をまたいで保つ。
  stepping: false,
  /// 実行状態を読みに行くタイマー。
  poll: null,
};

const TILE_SIZE_KEY = "mekiki.layout.tileSize";
const LAST_SCRIPT_KEY = "mekiki.file.lastScript";

function rememberedScriptPath() {
  try {
    return localStorage.getItem(LAST_SCRIPT_KEY) || null;
  } catch {
    return null;
  }
}

function rememberScriptPath(path) {
  try {
    if (path) {
      localStorage.setItem(LAST_SCRIPT_KEY, path);
    } else {
      localStorage.removeItem(LAST_SCRIPT_KEY);
    }
  } catch {
    // Remembering the file is convenient, but must not block editing.
  }
}

/// タイルからエディタへのドラッグ中だけ持つ。
/// WebView2 では dragover 時に自前 MIME が見えないことがあるので、
/// dataTransfer ではなくここを許可判定に使う。
let draggingAsset = null;
/// ウィンドウ行からエディタへのドラッグ中だけ持つ。理由は draggingAsset と同じ。
let draggingWindow = null;

// HTML の文言を最初に流し込む。エディタの構築より前に置くこと。
// 逆にすると、空のツールバーが一瞬見えてから文字が入る。
applyStaticText();

// ツールバーのアイコン画像は既定でドラッグできる。
// エディタへ落とすと URL がゴミとして貼られるので、開始時点で止める。
document.querySelector(".toolbar")?.addEventListener("dragstart", (event) => {
  event.preventDefault();
});

// ---------------------------------------------------------------------------
// エディタ
// ---------------------------------------------------------------------------

const view = new EditorView({
  parent: el("editor"),
  state: EditorState.create({
    doc: "",
    extensions: [
      // CodeMirror 自身が出す文言のカタログ。
      // 今の構成で出るのは制御文字の代替表示など数えるほどだが、
      // 補完や検索を足したときに配線を忘れないよう最初から通しておく。
      phrases(),
      lineNumbers(),
      history(),
      drawSelection(),
      rectangularSelection(),
      highlightActiveLine(),
      // Rhai 専用モードは無いが、呼び出し・文字列・コメントの見た目は
      // JavaScript のもので十分近い。画像リテラルの判定は Rust 側でやるので、
      // ここでの構文解析の正確さには依存しない。
      javascript(),
      themeCompartment.of(editorThemeExt()),
      rhaiCompletion(),
      imageLiteralField,
      execLineField,
      dropCursor(),
      EditorView.domEventHandlers({
        dragover(event) {
          if (!draggingAsset && !draggingWindow) return false;
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
          return true;
        },
        drop(event, view) {
          if (!draggingAsset && !draggingWindow) return false;
          event.preventDefault();
          const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
          if (draggingAsset) {
            const resource = draggingAsset;
            draggingAsset = null;
            insertAsset(resource, pos ?? undefined);
            return true;
          }
          const payload = draggingWindow;
          draggingWindow = null;
          insertWindow(payload.window, payload.windows, pos ?? undefined);
          return true;
        },
      }),
      // アプリのショートカット（Ctrl+S / Ctrl+Enter / Ctrl+P / F10）は
      // **ここには登録しない。** window 側の 1 か所で受ける（下の shortcuts）。
      //
      // CodeMirror の keymap はエディタにフォーカスがある時しか効かない。
      // 作業ディレクトリ欄や画像タイルを触っている間は保存も実行もできず、
      // ステップの F10 に至っては「進めながら他所を見る」使い方ができない。
      // 両方に置くと二重に走るので、置き場所は window に寄せる。
      // Tab は既定だとフォーカス移動。編集中はタブ文字を入れる。
      // 補完の確定は Enter。Tab を補完に使うと入力と衝突する。
      keymap.of([
        ...completionKeymap,
        { key: "Tab", run: insertTab, shift: indentLess },
        ...defaultKeymap,
        ...historyKeymap,
      ]),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          state.dirty = true;
          scheduleDetect();
          updateTitle();
        }
        if (update.selectionSet || update.docChanged) {
          updatePreviewButtons();
          updateCursorPos();
        }
      }),
      EditorView.lineWrapping,
    ],
  }),
});

// OS の外観が変わったら、設定が「システム」のときだけエディタも追従する。
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  syncEditorTheme(view);
});

// ---------------------------------------------------------------------------
// 画像リテラルの検出とサムネイル解決
// ---------------------------------------------------------------------------

let detectTimer = null;
let detectGeneration = 0;

function scheduleDetect() {
  // 1 文字打つたびに往復すると重い。入力が落ち着いてから走らせる。
  clearTimeout(detectTimer);
  detectTimer = setTimeout(detect, 150);
}

async function detect() {
  const request = ++detectGeneration;
  const source = view.state.doc.toString();
  const baseDir = state.baseDir;
  let literals;
  try {
    literals = await invoke("detect_image_literals", { source });
  } catch (e) {
    log(t("image.detectFailed", e), true);
    return;
  }
  if (request !== detectGeneration) return;
  state.literals = literals;

  // まだ解決していない参照だけサムネイルを取りに行く。
  const pending = [...new Set(literals.map((l) => l.reference))].filter(
    (r) => !hasThumbnail(r),
  );
  if (pending.length > 0) {
    await Promise.all(
      pending.map(async (reference) => {
        try {
          const img = await invoke("resolve_image", {
            baseDir,
            reference,
          });
          if (request === detectGeneration) {
            cacheThumbnail(reference, img.data_url);
          }
        } catch {
          // 見つからない画像は破線枠で表示する。ここでは何もしない。
        }
      }),
    );
  }

  if (request !== detectGeneration) return;
  view.dispatch({ effects: setImageLiterals.of(literals) });
  updatePreviewButtons();
}

// ---------------------------------------------------------------------------
// 操作
// ---------------------------------------------------------------------------

/// エンジンの状態だけを左下へ出す。操作結果は `log` へ。
function setStatus(message, isError = false) {
  const s = el("status");
  setDynamicText(s, message);
  s.classList.toggle("error", isError);
}

/// 操作結果や案内を出力パネルへ残す。
function log(message, isError = false) {
  appendOutput([message], isError ? "err" : undefined);
}

function scriptFileName() {
  return state.path ? state.path.split(/[\\/]/).pop() : t("file.untitled");
}

function updateTitle() {
  const name = scriptFileName();
  const labeled = state.dirty ? `${name}*` : name;
  const titleEl = el("script-name");
  titleEl.textContent = labeled;
  titleEl.title = state.path ?? labeled;
  document.title = `${labeled} — Mekiki`;
  setWindowTitle(labeled);
  // パスが無い（無題）あいだは上書き先が無い。
  el("btn-save").disabled = !state.path;
}

/// 右下にメインカーソルの行・列を出す。列は行先頭からの文字数（1 始まり）。
function updateCursorPos() {
  const head = view.state.selection.main.head;
  const line = view.state.doc.lineAt(head);
  el("cursor-pos").textContent = t("status.cursor", line.number, head - line.from + 1);
}

function appendOutput(lines, className) {
  const out = el("output");
  for (const line of lines) {
    const div = document.createElement("div");
    if (className) div.className = className;
    div.textContent = line;
    out.appendChild(div);
  }
  out.scrollTop = out.scrollHeight;
}

/// 基点をスクリプトのある場所に合わせる。
///
/// **画像はスクリプトと同じフォルダにある**というのが Mekiki の前提なので、
/// 開いたときも保存先を変えたときも、基点はスクリプトを追いかける必要がある。
/// CLI も同じ規則（スクリプトのあるディレクトリが基点）なので、
/// ここを揃えておかないと同じスクリプトが IDE と CLI で違う挙動になる。
function showBaseDir() {
  const node = el("base-dir");
  node.textContent = state.baseDir;
  node.title = state.baseDir;
}

async function followScriptDir(path) {
  state.baseDir = scriptDir(path);
  showBaseDir();
  clearAssetsPane(t("assets.loading"));
  await refreshAssets();
}

function scriptDir(path) {
  return path.replace(/[\\/][^\\/]*$/, "") || ".";
}

/// 未保存の変更があるとき、保存するか聞いてよいなら true。
async function confirmDiscardIfDirty() {
  if (!state.dirty) return true;
  const save = t("file.unsaved.save");
  const discard = t("file.unsaved.discard");
  const cancel = t("file.unsaved.cancel");
  const result = await message(t("file.unsaved.message", scriptFileName()), {
    title: t("file.unsaved.title"),
    kind: "warning",
    buttons: { yes: save, no: discard, cancel },
  });
  // カスタムボタンだと "Yes"/"No" ではなくラベル文字列が返る。
  if (result === "Cancel" || result === cancel) return false;
  if (result === "No" || result === discard) return true;
  if (result !== "Yes" && result !== "Ok" && result !== save) return false;
  if (state.path) {
    await writeCurrentScript(state.path);
    return true;
  }
  await saveAs();
  return Boolean(state.path) && !state.dirty;
}

async function open() {
  try {
    if (!(await confirmDiscardIfDirty())) return;
  } catch (e) {
    log(t("file.saveFailed", e), true);
    return;
  }
  try {
    const selected = await openDialog({
      title: t("file.openTitle"),
      defaultPath: defaultSavePath(),
      multiple: false,
      filters: [{ name: t("file.rhaiFilter"), extensions: ["rhai"] }],
    });
    if (!selected || Array.isArray(selected)) return;
    const path = selected;
    const source = await invoke("read_script", { path });
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: source },
    });
    state.path = path;
    state.dirty = false;
    rememberScriptPath(path);
    await followScriptDir(path);
    updateTitle();
    log(t("file.opened", path));
    await detect();
  } catch (e) {
    log(t("file.openFailed", e), true);
  }
}

function defaultSavePath() {
  if (state.path) return state.path;
  const dir = (state.baseDir || ".").replace(/[\\/]+$/, "");
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return `${dir}${sep}script.rhai`;
}

function ensureRhaiExt(path) {
  const name = path.split(/[\\/]/).pop() ?? "";
  return name.includes(".") ? path : `${path}.rhai`;
}

async function writeCurrentScript(path) {
  await invoke("write_script", { path, source: view.state.doc.toString() });
  const relocated = state.path !== path;
  state.path = path;
  state.dirty = false;
  rememberScriptPath(path);
  if (relocated) {
    await followScriptDir(path);
    await detect();
  }
  updateTitle();
  log(t("file.saved", path));
}

/// 開いているスクリプトへ上書きする。無題のときは何もしない。
async function save() {
  if (!state.path) return;
  try {
    await writeCurrentScript(state.path);
  } catch (e) {
    log(t("file.saveFailed", e), true);
  }
}

/// 保存先を選んで書く。無題でも使える。
async function saveAs() {
  try {
    const selected = await saveDialog({
      title: t("file.saveAsTitle"),
      defaultPath: defaultSavePath(),
      filters: [{ name: t("file.rhaiFilter"), extensions: ["rhai"] }],
    });
    if (!selected) return;
    await writeCurrentScript(ensureRhaiExt(selected));
  } catch (e) {
    log(t("file.saveFailed", e), true);
  }
}

/// 空の無題ドキュメントに戻す。未保存なら先に聞く。
async function newScript() {
  try {
    if (!(await confirmDiscardIfDirty())) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: "" },
    });
    state.path = null;
    state.dirty = false;
    rememberScriptPath(null);
    state.baseDir = state.defaultBaseDir;
    showBaseDir();
    clearAssetsPane(t("assets.empty"));
    updateTitle();
    await detect();
    log(t("file.new"));
  } catch (e) {
    log(t("file.saveFailed", e), true);
  }
}

/// 無題のあいだは作業フォルダが定まらないので、画像をディスクへ書かない。
async function requireSavedScript() {
  if (state.path) return true;
  await message(t("image.needFolder.message"), {
    title: t("image.needFolder.title"),
    kind: "warning",
  });
  return false;
}

async function snip() {
  try {
    if (!(await requireSavedScript())) return;
    await invoke("open_snipping_tool");
    log(t("image.snipHint"));
  } catch (e) {
    log(`${e}`, true);
  }
}

async function pasteImage() {
  try {
    if (!(await requireSavedScript())) return;
    const reference = await invoke("import_clipboard", {
      baseDir: state.baseDir,
    });
    log(t("image.imported", reference.slice(0, 22)));
    await refreshAssets();
  } catch (e) {
    log(`${e}`, true);
  }
}

/// 実行の状態を UI に反映する。
///
/// `paused` のあいだは**エンジンのワーカースレッドが止まっている**ので、
/// マッチ確認も画像一覧も応答しない。押しても無反応になるより、
/// 画面ごと伏せて「今は触れない」と分かる形にする。
function setRunState(next) {
  state.run = next;
  const running = next !== "idle";

  document.body.classList.toggle("running", running);
  document.body.classList.toggle("paused", next === "paused");

  // タスクバーのボタンにも同じ状態を出す。実行中は IDE の窓が
  // 操作対象の後ろに隠れるので、窓の中のボタンでは判別できない。
  // 描けなくても実行には関係ないので、記録だけして進む。
  invoke("set_run_indicator", { state: next }).catch((e) => {
    console.warn("set_run_indicator failed:", e);
  });

  el("btn-run").disabled = running;
  el("btn-pause").disabled = !running || next === "stopping";
  el("btn-stop").disabled = !running || next === "stopping";
  // 「次へ」はステップ実行を入れて走っているときだけ。
  el("btn-step-next").disabled = !running || !state.stepping || next === "stopping";

  // 一時停止中は再開ボタンとして振る舞う。
  const paused = next === "paused";
  el("btn-pause").classList.toggle("primary", paused);
  const hint = paused ? "toolbar.resume.hint" : "toolbar.pause.hint";
  el("btn-pause").title = t(hint);
  el("btn-pause").setAttribute("aria-label", t(hint));
}

async function run() {
  if (state.run !== "idle") return;

  const source = view.state.doc.toString();
  el("output").innerHTML = "";
  setRunState("running");
  startPolling();

  // 実行中だけ非常停止のホットキーを握る。
  // 失敗しても実行はできるので、止められない旨だけ伝えて進む。
  let hotkey = null;
  try {
    await invoke("arm_stop_hotkey");
    hotkey = await invoke("stop_hotkey_label");
    setStatus(t("run.runningWithHotkey", hotkey));
  } catch (e) {
    // 他のアプリが同じ組み合わせを握っていると失敗する。
    appendOutput([t("run.hotkeyUnavailable", e)], "err");
    setStatus(t("run.running"));
  }

  try {
    const result = await invoke("run_script", {
      baseDir: state.baseDir,
      source,
    });
    appendOutput(result.output);
    if (result.interrupted) {
      // 停止は利用者が押した結果。失敗色にはしない。
      setStatus(t("run.stopped", result.elapsed_ms));
    } else if (result.ok) {
      setStatus(t("run.done", result.elapsed_ms));
    } else {
      appendOutput([result.error ?? t("run.unknownError")], "err");
      setStatus(t("run.failed", result.elapsed_ms), true);
    }
  } catch (e) {
    appendOutput([`${e}`], "err");
    setStatus(t("run.cannotStart"), true);
  } finally {
    stopPolling();
    // ホットキーは OS の共有資源なので、終わったら必ず離す。
    await invoke("disarm_stop_hotkey").catch(() => {});
    // 終わったら印を消す。残すと「まだそこで止まっている」ように見える。
    showExecLine(view, 0);
    setRunState("idle");
  }
}

/// 実行中だけ、現在行を読みに行って印を移す。
///
/// **押し込み（Tauri のイベント）ではなく読みに行く形にしている。**
/// ワーカースレッドはスクリプト実行中に戻ってこないので、
/// そちらから通知を出すには別の配線が要る。`run_status` は
/// ジョブキューを通らない共有フラグの読み出しなので、実行中でも応答する。
function startPolling() {
  stopPolling();
  state.poll = setInterval(async () => {
    let status;
    try {
      status = await invoke("run_status");
    } catch {
      return;
    }
    // ステップ中は行を追いかける。自由実行で追いかけると
    // 画面が飛び回って読めなくなるので、印だけ動かす。
    showExecLine(view, status.line, { scroll: status.stepping });

    if (status.stepping && status.line > 0 && state.run === "running") {
      setStatus(t("run.stepping", status.line));
    }
  }, 80);
}

function stopPolling() {
  if (state.poll !== null) {
    clearInterval(state.poll);
    state.poll = null;
  }
}

async function toggleStepMode() {
  state.stepping = !state.stepping;
  el("btn-step-mode").setAttribute("aria-pressed", String(state.stepping));
  el("btn-step-mode").classList.toggle("primary", state.stepping);
  await invoke("set_stepping", { enabled: state.stepping });
  setRunState(state.run);
  log(t(state.stepping ? "run.stepModeOn" : "run.stepModeOff"));
}

async function stepNext() {
  if (state.run !== "running" || !state.stepping) return;
  await invoke("step_next");
}

async function togglePause() {
  if (state.run === "running") {
    // 先に UI を伏せる。ワーカーが止まってからでは
    // このコマンドの応答も返ってこない可能性がある。
    setRunState("paused");
    setStatus(t("run.paused"));
    await invoke("pause_script");
  } else if (state.run === "paused") {
    await invoke("resume_script");
    setRunState("running");
    setStatus(t("run.running"));
  }
}

async function stopRun() {
  if (state.run === "idle") return;
  // 一時停止中でも止められる。停止要求は一時停止に上書きされない。
  setRunState("stopping");
  setStatus(t("run.stopping"));
  await invoke("stop_script");
}

/// マッチ確認の対象。カーソル上の画像リテラルを優先し、無ければ選んだタイル。
/// `preferTile` は右ペインのボタン用。タイルを選んだのにカーソル側を
/// 探してしまうのを避ける。
function previewReference(preferTile) {
  if (preferTile && state.selectedAsset) {
    return state.selectedAsset.reference;
  }
  const lit = literalAtCursor(view, state.literals);
  if (lit) return lit.reference;
  return state.selectedAsset?.reference ?? null;
}

function updatePreviewButtons() {
  const has = Boolean(previewReference(false));
  el("btn-preview-run").disabled = !has;
}

async function previewCurrent(preferTile = false) {
  const reference = previewReference(preferTile);
  if (!reference) {
    log(t("preview.hint"));
    return;
  }

  const similarity = Number(el("similarity").value);

  try {
    const result = await invoke("preview_match", {
      baseDir: state.baseDir,
      reference,
      similarity,
    });
    logPreview(reference, result, similarity);
  } catch (e) {
    appendOutput([t("preview.failed", e)], "err");
  }
}

function logPreview(reference, result, similarity) {
  const count = result.matches.length;
  const summary =
    count === 0
      ? t("preview.noMatch", reference, Number(similarity).toFixed(2))
      : t(
          "preview.matched",
          reference,
          count,
          Math.max(...result.matches.map((m) => m.score)).toFixed(4),
        );
  const lines = [summary];
  for (const m of result.matches) {
    lines.push(
      t("preview.matchLine", m.x, m.y, m.width, m.height, m.score.toFixed(4)),
    );
  }
  appendOutput(lines);
}

// ---------------------------------------------------------------------------
// 右ペイン: 画像タイル
// ---------------------------------------------------------------------------

function setAssetsMessage(text) {
  const box = el("assets");
  box.innerHTML = "";
  const msg = document.createElement("div");
  msg.className = "tiles-message";
  msg.textContent = text;
  box.appendChild(msg);
}

function setAssetsCount(n) {
  const count = el("assets-count");
  setDynamicText(count, n > 0 ? t("assets.count", n) : "");
}

const assetLoader = createAssetLoader((baseDir) =>
  invoke("list_images", { baseDir }),
);

function clearAssetsPane(message) {
  assetLoader.invalidate();
  detectGeneration += 1;
  clearTimeout(detectTimer);
  clearThumbnails();
  state.literals = [];
  view.dispatch({ effects: setImageLiterals.of([]) });
  state.selectedAsset = null;
  draggingAsset = null;
  setAssetsCount(0);
  setAssetsMessage(message);
  updatePreviewButtons();
}

function selectAsset(resource) {
  state.selectedAsset = resource;
  for (const tile of el("assets").querySelectorAll(".tile")) {
    tile.classList.toggle("selected", tile.dataset.ref === resource.reference);
  }
  updatePreviewButtons();
}

function isStoreAsset(res) {
  return typeof res?.reference === "string" && res.reference.startsWith("sha256:");
}

function fileNameOf(reference) {
  const parts = reference.split("/");
  return parts[parts.length - 1];
}

/// 開いているスクリプト中の、この参照を使っている画像リテラルを書き換える。
/// `image:` の有無は元の書き方を残す。
function rewriteLiterals(oldRef, newRef) {
  const hits = state.literals.filter((l) => l.reference === oldRef);
  if (hits.length === 0) return 0;

  const changes = [...hits]
    .sort((a, b) => b.from - a.from)
    .map((lit) => {
      const inner = lit.raw.startsWith("image:") ? `image:${newRef}` : newRef;
      return { from: lit.from + 1, to: lit.to - 1, insert: inner };
    });
  view.dispatch({ changes });
  return hits.length;
}

async function applyRename(res, next) {
  const fromStore = isStoreAsset(res);
  const current = fromStore ? res.name : fileNameOf(res.reference);
  if (!fromStore && next.trim() === current) return;

  try {
    const newRef = await invoke("rename_image", {
      baseDir: state.baseDir,
      reference: res.reference,
      newName: next,
    });
    if (res.thumbnail) {
      cacheThumbnail(newRef, res.thumbnail);
    }
    const updated = rewriteLiterals(res.reference, newRef);
    state.selectedAsset = {
      ...res,
      reference: newRef,
      name: newRef,
      in_store: false,
    };
    await detect();
    await refreshAssets();
    const named = fileNameOf(newRef);
    if (fromStore) {
      log(
        updated > 0
          ? t("assets.namedFromStoreWithRefs", named, updated)
          : t("assets.namedFromStore", named),
      );
    } else {
      log(
        updated > 0
          ? t("assets.renamedWithRefs", current, named, updated)
          : t("assets.renamed", current, named),
      );
    }
  } catch (e) {
    log(`${e}`, true);
  }
}

function tileNameEl(resource) {
  return [...el("assets").querySelectorAll(".tile")].find(
    (tile) => tile.dataset.ref === resource.reference,
  )?.querySelector(".tile-name");
}

function beginRename(resource, nameEl) {
  if (nameEl.querySelector("input")) return;

  const input = document.createElement("input");
  input.type = "text";
  input.className = "tile-name-edit";
  input.spellcheck = false;
  input.value = isStoreAsset(resource)
    ? "image.png"
    : fileNameOf(resource.reference);

  const previous = nameEl.textContent;
  nameEl.replaceChildren(input);
  input.focus();
  input.select();

  let finished = false;
  const finish = (commit) => {
    if (finished) return;
    finished = true;
    const next = input.value;
    nameEl.textContent = previous;
    if (commit && next.trim()) {
      applyRename(resource, next);
    }
  };

  input.onkeydown = (event) => {
    event.stopPropagation();
    if (event.key === "Enter") {
      event.preventDefault();
      finish(true);
    } else if (event.key === "Escape") {
      event.preventDefault();
      finish(false);
    }
  };
  input.onmousedown = (event) => event.stopPropagation();
  input.onclick = (event) => event.stopPropagation();
  input.ondblclick = (event) => event.stopPropagation();
  input.onblur = () => finish(true);
}

function startRename(resource) {
  if (!resource) {
    log(t("assets.renameNone"), true);
    return;
  }
  selectAsset(resource);
  const nameEl = tileNameEl(resource);
  if (nameEl) {
    beginRename(resource, nameEl);
    return;
  }
  const current = isStoreAsset(resource)
    ? "image.png"
    : fileNameOf(resource.reference);
  const next = prompt(t("assets.renamePrompt"), current);
  if (next === null || !next.trim()) return;
  applyRename(resource, next);
}

// ---------------------------------------------------------------------------
// タイルのコンテキストメニュー
// ---------------------------------------------------------------------------

const assetMenu = document.createElement("div");
assetMenu.className = "menu";
assetMenu.hidden = true;
document.body.appendChild(assetMenu);

function hideAssetMenu() {
  assetMenu.hidden = true;
}

function showAssetMenu(event, resource) {
  event.preventDefault();
  selectAsset(resource);
  assetMenu.innerHTML = "";

  const add = (label, action, kind) => {
    const item = document.createElement("button");
    item.type = "button";
    item.textContent = label;
    if (kind) item.classList.add(kind);
    item.onclick = () => {
      hideAssetMenu();
      action();
    };
    assetMenu.appendChild(item);
  };
  add(t("assets.rename"), () => startRename(resource));
  add(t("assets.menuInsert"), () => insertAsset(resource));
  add(t("assets.delete"), () => startDelete(resource), "danger");

  assetMenu.style.left = `${event.clientX}px`;
  assetMenu.style.top = `${event.clientY}px`;
  assetMenu.hidden = false;
  const pad = 8;
  const { width, height } = assetMenu.getBoundingClientRect();
  const left = Math.min(event.clientX, window.innerWidth - width - pad);
  const top = Math.min(event.clientY, window.innerHeight - height - pad);
  assetMenu.style.left = `${Math.max(pad, left)}px`;
  assetMenu.style.top = `${Math.max(pad, top)}px`;
}

document.addEventListener("click", hideAssetMenu);
window.addEventListener("blur", hideAssetMenu);

/// 選択中タイルの Delete 削除。
///
/// SHORTCUTS はフォーカス位置を見ずに横取りする方針なので、単独の
/// Delete をあそこへ足すとエディタや入力欄の文字削除まで奪ってしまう。
/// ペインのコンテナで受ければタイルにフォーカスがある時だけ効く。
/// リネーム入力は keydown を stopPropagation しているため、名前編集中の
/// Delete が画像削除に化けることはない。
el("assets").addEventListener("keydown", (event) => {
  if (event.key !== "Delete") return;
  if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
  if (!state.selectedAsset) return;
  event.preventDefault();
  startDelete(state.selectedAsset);
});

// ---------------------------------------------------------------------------
// ショートカット
// ---------------------------------------------------------------------------

/// アプリ全体のショートカット。
///
/// **エディタの keymap ではなく窓で受ける。** CodeMirror に登録すると
/// エディタにフォーカスがある時しか効かず、作業ディレクトリ欄や
/// 画像タイルを触っている間は保存も実行もできない。
///
/// ここに挙げた組み合わせはいずれも入力欄で意味を持たないので、
/// フォーカス位置で分岐せず一律に横取りしてよい。
/// 逆に、単独の文字キーをここへ足すときは入力欄の判定が要る。
const SHORTCUTS = [
  { key: "n", ctrl: true, run: () => newScript() },
  { key: "o", ctrl: true, run: () => open() },
  { key: "s", ctrl: true, shift: true, run: () => saveAs() },
  { key: "s", ctrl: true, run: () => save() },
  { key: "Enter", ctrl: true, run: () => run() },
  { key: "p", ctrl: true, run: () => previewCurrent(false) },
  { key: "F10", run: () => stepNext() },
  // 停止。SikuliX に合わせた組み合わせで、こちらは窓の中だけ。
  // 窓の外からも止めたい場合はグローバルホットキー（下記）が効く。
  { key: "c", shift: true, alt: true, run: () => stopRun() },
];

function matches(binding, event) {
  // key は英字で大文字小文字が揺れる（Shift の有無で変わる）ので畳む。
  if (event.key.toLowerCase() !== binding.key.toLowerCase()) return false;
  // 指定していない修飾キーは「押されていないこと」を要求する。
  // 緩くすると Ctrl+Shift+S のような別の割り当てまで拾ってしまう。
  return (
    Boolean(binding.ctrl) === (event.ctrlKey || event.metaKey) &&
    Boolean(binding.shift) === event.shiftKey &&
    Boolean(binding.alt) === event.altKey
  );
}

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    hideAssetMenu();
    return;
  }
  // IME 変換中は横取りしない。確定の Enter を実行と取り違える。
  if (event.isComposing) return;

  const binding = SHORTCUTS.find((b) => matches(b, event));
  if (!binding) return;

  event.preventDefault();
  binding.run();
});

function assetLabel(resource) {
  return isStoreAsset(resource) ? resource.name : fileNameOf(resource.reference);
}

function startDelete(resource) {
  if (!resource) return;
  if (!confirm(t("assets.deleteConfirm", assetLabel(resource)))) return;
  applyDelete(resource);
}

async function applyDelete(res) {
  const label = assetLabel(res);
  const leftover = state.literals.filter((l) => l.reference === res.reference).length;
  try {
    await invoke("delete_image", {
      baseDir: state.baseDir,
      reference: res.reference,
    });
    if (state.selectedAsset?.reference === res.reference) {
      state.selectedAsset = null;
    }
    await detect();
    await refreshAssets();
    log(
      leftover > 0
        ? t("assets.deletedWithRefs", label, leftover)
        : t("assets.deleted", label),
    );
  } catch (e) {
    log(`${e}`, true);
  }
}

function insertAsset(resource, pos) {
  if (resource.thumbnail) {
    cacheThumbnail(resource.reference, resource.thumbnail);
  }
  const literal = `"image:${resource.reference}"`;
  const from = pos ?? view.state.selection.main.head;
  const to = pos == null ? view.state.selection.main.anchor : pos;
  view.dispatch({
    changes: { from, to, insert: literal },
    selection: { anchor: from + literal.length },
  });
  log(t("assets.inserted", resource.name));
  detect();
}

function renderAssets(list) {
  const box = el("assets");
  box.innerHTML = "";
  setAssetsCount(list.length);

  if (list.length === 0) {
    setAssetsMessage(t("assets.empty"));
    return;
  }

  for (const res of list) {
    if (res.thumbnail) {
      cacheThumbnail(res.reference, res.thumbnail);
    }

    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "tile";
    btn.dataset.ref = res.reference;
    btn.title = res.reference;
    if (state.selectedAsset?.reference === res.reference) {
      btn.classList.add("selected");
    }

    const thumb = document.createElement("span");
    thumb.className = "tile-thumb";
    const img = document.createElement("img");
    img.src = res.thumbnail;
    img.alt = res.name;
    img.draggable = false;
    thumb.appendChild(img);

    const name = document.createElement("span");
    name.className = "tile-name";
    name.textContent = res.name;
    name.title = t("assets.rename.hint");
    name.onclick = (event) => {
      event.preventDefault();
      event.stopPropagation();
      startRename(res);
    };

    const meta = document.createElement("span");
    meta.className = "tile-meta";
    const dim = document.createElement("span");
    dim.textContent = `${res.width}\u00d7${res.height}`;
    meta.appendChild(dim);
    if (isStoreAsset(res) || res.in_store || res.inStore) {
      const badge = document.createElement("span");
      badge.className = "tile-badge";
      badge.textContent = t("assets.storeBadge");
      meta.appendChild(badge);
    }

    btn.append(thumb, name, meta);
    btn.draggable = true;
    btn.ondragstart = (event) => {
      draggingAsset = { reference: res.reference, name: res.name };
      event.dataTransfer.effectAllowed = "copy";
      try {
        event.dataTransfer.clearData();
      } catch {
        // 空のままなら、既定の text/plain を CodeMirror が拾わない。
      }
    };
    btn.ondragend = () => {
      draggingAsset = null;
    };
    btn.onclick = () => selectAsset(res);
    btn.ondblclick = () => insertAsset(res);
    btn.oncontextmenu = (event) => showAssetMenu(event, res);
    btn.onkeydown = (event) => {
      if (event.key === "F2") {
        event.preventDefault();
        startRename(res);
      }
    };
    box.appendChild(btn);
  }
}

async function refreshAssets() {
  setAssetsMessage(t("assets.loading"));
  const result = await assetLoader.load(state.baseDir);
  if (result.kind === "stale") return;
  if (result.kind === "failed") {
    state.selectedAsset = null;
    draggingAsset = null;
    setAssetsCount(0);
    setAssetsMessage(t("assets.failed", result.error));
    updatePreviewButtons();
    return;
  }
  if (
    state.selectedAsset &&
    !result.list.some((asset) => asset.reference === state.selectedAsset.reference)
  ) {
    state.selectedAsset = null;
  }
  renderAssets(result.list);
  updatePreviewButtons();
}

function clampTileSize(n) {
  return Math.min(240, Math.max(48, Math.round(n / 8) * 8));
}

function applyTileSize(n, persist) {
  const size = clampTileSize(n);
  const input = el("tile-size");
  el("side-right").style.setProperty("--tile-size", `${size}px`);
  input.value = String(size);
  el("btn-tile-size-minus").disabled = size <= Number(input.min);
  el("btn-tile-size-plus").disabled = size >= Number(input.max);
  if (persist) {
    try {
      localStorage.setItem(TILE_SIZE_KEY, String(size));
    } catch {
      // 保存できなくても表示は変える。
    }
  }
}

function nudgeTileSize(delta) {
  applyTileSize(Number(el("tile-size").value) + delta, true);
}

function initTileSize() {
  let initial = 96;
  try {
    const v = parseFloat(localStorage.getItem(TILE_SIZE_KEY));
    if (Number.isFinite(v)) initial = v;
  } catch {
    // 読めなければ既定。
  }
  applyTileSize(initial, false);
}

async function refreshWindows() {
  const container = el("windows");
  container.textContent = t("windows.loading");
  try {
    const list = await invoke("list_windows");

    container.innerHTML = "";
    for (const w of list) {
      const row = document.createElement("div");
      row.className = "win-row";
      row.draggable = true;
      row.title = w.title;

      if (w.icon) {
        const img = document.createElement("img");
        img.className = "win-icon";
        img.src = w.icon;
        img.alt = "";
        img.draggable = false;
        row.appendChild(img);
      } else {
        const ph = document.createElement("span");
        ph.className = "win-icon-placeholder";
        row.appendChild(ph);
      }

      const title = document.createElement("span");
      title.className = "win-title";
      title.textContent = w.title;
      row.appendChild(title);

      const ident = document.createElement("span");
      ident.className = "win-exe";
      ident.textContent = w.exe || w.class_name;
      ident.title = `exe=${w.exe}\nclass=${w.class_name}`;
      row.appendChild(ident);

      row.ondragstart = (event) => {
        draggingWindow = { window: w, windows: list };
        event.dataTransfer.effectAllowed = "copy";
        try {
          event.dataTransfer.clearData();
        } catch {
          // 空のままなら、既定の text/plain を CodeMirror が拾わない。
        }
      };
      row.ondragend = () => {
        draggingWindow = null;
      };
      row.oncontextmenu = (event) => showWindowMenu(event, w, list);
      container.appendChild(row);
    }
  } catch (e) {
    container.textContent = `${e}`;
  }
}

function showWindowMenu(event, w, windows) {
  event.preventDefault();
  hideAssetMenu();
  assetMenu.innerHTML = "";

  const add = (label, action) => {
    const item = document.createElement("button");
    item.type = "button";
    item.textContent = label;
    item.onclick = () => {
      hideAssetMenu();
      action();
    };
    assetMenu.appendChild(item);
  };
  add(t("windows.menuInsert"), () => insertWindow(w, windows));
  add(t("windows.menuLog"), () => logWindowDetails(w));

  assetMenu.style.left = `${event.clientX}px`;
  assetMenu.style.top = `${event.clientY}px`;
  assetMenu.hidden = false;
  const pad = 8;
  const { width, height } = assetMenu.getBoundingClientRect();
  const left = Math.min(event.clientX, window.innerWidth - width - pad);
  const top = Math.min(event.clientY, window.innerHeight - height - pad);
  assetMenu.style.left = `${Math.max(pad, left)}px`;
  assetMenu.style.top = `${Math.max(pad, top)}px`;
}

function logWindowDetails(w) {
  log(t("windows.detailTitle", w.title));
  log(t("windows.detailExe", w.exe || "—"));
  log(t("windows.detailBounds", w.x, w.y, w.width, w.height));
  log(t("windows.detailClass", w.class_name || "—"));
  log(t("windows.detailZ", w.z_order));
}

function insertWindow(w, windows, pos) {
  const { snippet, ambiguous } = windowSelectionFor(w, windows);
  const from = pos ?? view.state.selection.main.head;
  const to = pos == null ? view.state.selection.main.anchor : pos;
  view.dispatch({
    changes: { from, to, insert: snippet },
    selection: { anchor: from + snippet.length },
  });
  log(t("windows.inserted"));
  if (ambiguous) log(t("windows.ambiguous"), true);
}

function showRightPanel(name) {
  for (const tab of document.querySelectorAll("#side-right .tab")) {
    tab.classList.toggle("active", tab.dataset.panel === name);
  }
  for (const panel of document.querySelectorAll("#side-right .side-panel")) {
    panel.classList.toggle("active", panel.id === `panel-${name}`);
  }
  // 一覧は開いたときに取る。EnumWindows は安い。
  if (name === "windows") refreshWindows();
}

// ---------------------------------------------------------------------------
// 配線
// ---------------------------------------------------------------------------

el("btn-new").onclick = newScript;
el("btn-open").onclick = open;
el("btn-save").onclick = save;
el("btn-save-as").onclick = saveAs;
el("btn-snip").onclick = snip;
el("btn-paste").onclick = pasteImage;
el("btn-run").onclick = run;
el("btn-pause").onclick = togglePause;
el("btn-stop").onclick = stopRun;
el("btn-step-mode").onclick = toggleStepMode;
el("btn-step-next").onclick = stepNext;
el("btn-settings").onclick = () =>
  openSettings(view, () => {
    updateTitle();
    updateCursorPos();
    syncMaximized();
    log(t("settings.applied"));
  });
el("btn-preview-run").onclick = () => previewCurrent(true);
el("btn-refresh-windows").onclick = refreshWindows;
el("btn-refresh-assets").onclick = refreshAssets;

el("similarity").oninput = (e) => {
  el("similarity-value").textContent = Number(e.target.value).toFixed(2);
};

el("tile-size").oninput = (e) => applyTileSize(Number(e.target.value), false);
el("tile-size").onchange = (e) => applyTileSize(Number(e.target.value), true);
el("btn-tile-size-minus").onclick = () => nudgeTileSize(-Number(el("tile-size").step || 8));
el("btn-tile-size-plus").onclick = () => nudgeTileSize(Number(el("tile-size").step || 8));

el("editor").addEventListener("dragover", (event) => {
  if (!draggingAsset && !draggingWindow) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "copy";
});

for (const tab of document.querySelectorAll("#side-right .tab")) {
  tab.onclick = () => showRightPanel(tab.dataset.panel);
}

// ---------------------------------------------------------------------------
// ペイン境界
// ---------------------------------------------------------------------------

// 既定値は CSS 側にだけ置いてある（style.css の var() 第 2 引数）。
// ここで px を渡さないので、利用者が動かすまでは窓の大きさに追従する。
attachSplitter({
  handle: el("split-bottom"),
  container: document.querySelector("main"),
  axis: "horizontal",
  cssVar: "--bottom-height",
  min: 110, // タブが隠れない高さ
  storageKey: "mekiki.layout.bottomHeight",
  // 高さが変わったら CodeMirror に測り直させる。
  // 任せきりにすると、行の折り返しやカーソル位置の計算が古い高さのまま残る。
  onChange: () => view.requestMeasure(),
});

attachSplitter({
  handle: el("split-right"),
  container: el("top"),
  axis: "vertical",
  cssVar: "--right-width",
  min: 200,
  storageKey: "mekiki.layout.rightWidth",
  onChange: () => view.requestMeasure(),
});

// 言語切り替えの口。設定画面からも同じ関数を使う。
//
//   __mekikiI18n.set("en")            … 切り替える（次回起動でも保持される）
//   __mekikiI18n.missingKeys("en")    … まだ訳が無いキーの一覧
//
// 翻訳作業のときに、画面を出したまま両方の言語を見比べられるようにしてある。
window.__mekikiI18n = {
  get current() {
    return locale();
  },
  available: locales(),
  set: (code) => {
    const ok = setLocale(code, view);
    if (ok) {
      updateTitle();
      updateCursorPos();
      syncMaximized();
    }
    return ok;
  },
  missingKeys,
};

(async function init() {
  try {
    const v = await invoke("app_versions");
    log(t("status.engineVersion", v.engine));
    log(t("status.ideVersion", v.ide));
  } catch {
    // 取れなくても IDE は使える。
  }

  try {
    state.defaultBaseDir = await invoke("default_base_dir");
  } catch {
    state.defaultBaseDir = ".";
  }

  // コマンドライン引数を最優先し、無ければ前回のスクリプトを復元する。
  let startup = null;
  try {
    startup = await invoke("startup_script");
  } catch {
    // 引数無しでの起動。何もしない。
  }

  let openedPath = null;
  if (startup) {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: startup.source },
    });
    state.path = startup.path;
    state.baseDir = startup.base_dir;
    state.dirty = false;
    openedPath = startup.path;
    rememberScriptPath(startup.path);
  } else {
    const previousPath = rememberedScriptPath();
    if (previousPath) {
      try {
        const source = await invoke("read_script", { path: previousPath });
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: source },
        });
        state.path = previousPath;
        state.baseDir = scriptDir(previousPath);
        state.dirty = false;
        openedPath = previousPath;
      } catch (e) {
        // A moved or deleted file should not fail on every subsequent launch.
        rememberScriptPath(null);
        log(t("file.openFailed", e), true);
      }
    }
  }

  if (!openedPath) {
    state.baseDir = state.defaultBaseDir;
  }

  showBaseDir();
  updateTitle();
  updateCursorPos();
  await attachTitlebar();
  initTileSize();
  await detect();
  if (openedPath) {
    await refreshAssets();
  } else {
    clearAssetsPane(t("assets.empty"));
  }
  updatePreviewButtons();
  if (openedPath) log(t("file.opened", openedPath));
  setStatus(t("status.ready"));
})();
