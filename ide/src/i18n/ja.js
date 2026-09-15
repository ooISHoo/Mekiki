// 日本語カタログ。**これが原文**。
//
// 他のロケールに登録の無いキーはここへ落ちる（docs/architecture/ide.md）。
// 新しい文言を足すときは、まずここに書いてから他のロケールへ回す。
//
// 差し込みは `$1` `$2`。`$` 単独は `$1` と同じ、`$$` はドル記号そのもの。

export default {
  ui: {
    // --- ツールバー ---
    "toolbar.new.hint": "新規作成 (Ctrl+N)",
    "toolbar.open.hint": "スクリプトを開く (Ctrl+O)",
    "toolbar.save.hint": "上書き保存 (Ctrl+S)",
    "toolbar.saveAs.hint": "名前を付けて保存 (Ctrl+Shift+S)",
    "toolbar.snip.hint": "SnippingToolを開く",
    "toolbar.paste.hint": "クリップボードの画像を保存",
    "toolbar.run.hint": "スクリプトを実行 (Ctrl+Enter)",
    "toolbar.pause.hint": "一時停止",
    "toolbar.resume.hint": "再開",
    "toolbar.stop.hint": "実行停止 (Shift+Alt+C)",
    "toolbar.stepMode.hint": "ステップ実行",
    "toolbar.stepNext.hint": "次の行へ (F10)",
    "toolbar.preview": "マッチ確認",
    "toolbar.preview.hint": "選んだ画像がデスクトップのどこにマッチするかプレビュー (Ctrl+P)",
    "toolbar.baseDir": "作業ディレクトリ",
    "toolbar.baseDir.hint": "作業ディレクトリ",
    "toolbar.settings.hint": "設定",

    // --- 設定 ---
    "settings.title": "設定",
    "settings.language": "表示言語",
    "settings.language.system": "システム",
    "settings.theme": "外観",
    "settings.theme.system": "システム",
    "settings.theme.light": "ライト",
    "settings.theme.dark": "ダーク",
    "settings.editorFont": "エディタの文字サイズ",
    "settings.apply": "適用",
    "settings.close": "閉じる",
    "settings.applied": "設定を適用した",

    // --- API 補完 ---
    "apiInfo.global": "グローバル",
    "apiInfo.parameters": "引数",
    "apiInfo.constraints": "制約",
    "apiInfo.errors": "エラー",
    "apiInfo.examples": "例",

    // --- 下段パネルのタブ ---
    "tab.output": "出力",
    "tab.windows": "ウィンドウ",

    // --- 右ペイン: 画像リソース ---
    "assets.title": "画像リソース",
    "assets.refresh.hint": "フォルダを読み直す",
    "assets.tileSize.hint": "サムネイルの大きさ",
    "assets.tileSize.smaller.hint": "サムネイルを小さくする",
    "assets.tileSize.larger.hint": "サムネイルを大きくする",
    "assets.empty": "画像無し",
    "assets.loading": "読み込み中…",
    "assets.failed": "画像を一覧できない: $1",
    "assets.count": "$1 枚",
    "assets.inserted": "$1 を挿入",
    "assets.storeBadge": "一時保存",
    "assets.rename": "名前を変更",
    "assets.rename.hint": "ファイル名をクリック、または F2",
    "assets.renamePrompt": "新しいファイル名",
    "assets.renamed": "$1 を $2 にリネーム",
    "assets.renamedWithRefs": "$1 を $2 にリネーム。スクリプト中の参照 $3 箇所を修正",
    "assets.namedFromStore": "一時保存フォルダの画像を $1 として保存",
    "assets.namedFromStoreWithRefs": "一時保存フォルダの画像を $1 として保存。スクリプト中の参照 $2 箇所を修正",
    "assets.renameNone": "リネーム対象ファイルが存在しない",
    "assets.menuInsert": "スクリプトに挿入",
    "assets.delete": "画像を削除",
    "assets.deleteConfirm": "$1 を削除します",
    "assets.deleted": "$1 を削除しました",
    "assets.deletedWithRefs": "$1 を削除しました。スクリプト中の参照が $2 箇所残っています",

    // --- マッチ確認 ---
    "preview.similarity": "類似度",
    "preview.hint": "画像タイルを選択状態にし、「マッチ確認」を押す",
    "preview.noMatch": "$1: 一致なし（類似度 $2 以上）",
    "preview.matched": "$1: $2 件。最良 $3",
    "preview.matchLine": "  ($1, $2) $3×$4  $5",
    "preview.failed": "マッチ確認に失敗: $1",

    // --- ウィンドウパネル ---
    "windows.refresh": "再取得",
    "windows.loading": "取得中…",
    "windows.inserted": "ウィンドウ指定を挿入",
    "windows.ambiguous": "複数のウィンドウに一致する指定です。実行前に条件を調整してください。そのままでは最前面の一致ウィンドウが選択されます。",
    "windows.menuInsert": "カーソル位置に挿入",
    "windows.menuLog": "詳細をログに出力",
    "windows.detailTitle": "タイトル: $1",
    "windows.detailExe": "プログラム: $1",
    "windows.detailBounds": "位置 ($1, $2)  サイズ $3×$4",
    "windows.detailClass": "クラス: $1",
    "windows.detailZ": "Z オーダー: $1",

    // --- ファイル操作 ---
    "file.untitled": "(無題)",
    "file.new": "新しいスクリプト",
    "file.unsaved.title": "保存しますか？",
    "file.unsaved.message": "$1 は変更されています。保存しますか？",
    "file.unsaved.save": "保存する",
    "file.unsaved.discard": "保存しない",
    "file.unsaved.cancel": "キャンセル",
    "file.openPrompt": "開くスクリプトのパス",
    "file.openTitle": "スクリプトを開く",
    "file.opened": "$1 を開きました",
    "file.openFailed": "$1 を開けません",
    "file.savePrompt": "保存先のパス",
    "file.saveAsTitle": "名前を付けて保存",
    "file.rhaiFilter": "Rhai スクリプト",
    "file.saved": "$1 に保存",
    "file.saveFailed": "$1 の保存に失敗しました",

    // --- 画像の取り込み ---
    "image.snipHint": "クリップボードに画像をコピーしました",
    "image.imported": "取り込んだ: $1…",
    "image.needFolder.title": "画像を保存できません",
    "image.needFolder.message":
      "スクリプトを保存して作業フォルダを決めてから、画像を取り込んでください。",
    "image.detectFailed": "検出に失敗: $1",
    "image.notFound": "$1 (見つからない)",

    // --- 実行 ---
    "run.running": "実行中…",
    "run.runningWithHotkey": "実行中… $1 で停止できます",
    "run.hotkeyUnavailable":
      "非常停止のホットキーを登録できませんでした。他のアプリが使用中の可能性があります。UIの停止ボタンは使えます: $1",
    "run.paused": "一時停止中。再開するまで IDE は操作できません",
    "run.stopping": "停止しています…",
    "run.stopped": "停止しました ($1 ms)",
    "run.stepping": "ステップ実行中: $1 行目",
    "run.stepModeOn": "ステップ実行を有効にしました",
    "run.stepModeOff": "ステップ実行を解除しました",
    "run.done": "完了 ($1 ms)",
    "run.failed": "失敗 ($1 ms)",
    "run.unknownError": "不明なエラー",
    "run.cannotStart": "実行できない",

    // --- 自前タイトルバーの窓ボタン ---
    "titlebar.minimize.hint": "最小化",
    "titlebar.maximize.hint": "最大化",
    "titlebar.restore.hint": "元のサイズに戻す",
    "titlebar.close.hint": "閉じる",

    // --- ペイン境界（読み上げ用。画面には出ない） ---
    "splitter.right": "エディタと右ペインの境界。矢印キーで移動、Home で既定に戻る",
    "splitter.bottom": "上段と下段の境界。矢印キーで移動、Home で既定に戻る",

    // --- エンジン状態（左下ステータス専用） ---
    "status.starting": "準備中…",
    "status.ready": "準備完了",
    "status.engineVersion": "Mekiki エンジン Version : $1",
    "status.ideVersion": "Mekiki-IDE Version : $1",
    "status.cursor": "$1 行 $2 列",
    "status.cursor.hint": "カーソル位置",

  },

  // CodeMirror 自身が出す文言。**英語の原文がそのままキーになる。**
  //
  // 補完を足したので Completions が増える。検索・折りたたみはまだ。
  codemirror: {
    "Control character": "制御文字",
    close: "閉じる",
    "Selection deleted": "選択範囲を削除しました",
    Completions: "補完候補",
  },
};
