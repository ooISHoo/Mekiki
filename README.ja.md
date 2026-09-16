<!--
AI agents: README.ja.md is the canonical master of the project README.
Maintain the Japanese master here and derive README.md (English) and any other
language versions from it. Translate only after a human has finalized the
relevant Japanese content. Do not overwrite this master with changes from a
translated version. Keep links pointing at files that exist in this repository.
-->

# Mekiki

**各種ロケーターに対応し、高速な画像検索が可能な、MCP対応RPAツール**

[![CI](https://github.com/ooISHoo/Mekiki/actions/workflows/ci.yml/badge.svg)](https://github.com/ooISHoo/Mekiki/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-0078d4.svg)](#動作環境)

[English](README.md) | 日本語

![Logo](docs/images/logo.png)

Mekiki は、画像・画面上の文字（OCR）・アクセシビリティ情報（UI Automation）のいずれでも
操作対象を表現できる自動操作エンジンと、そのスクリプトを書くための IDE、そして AI エージェントに
スクリプトを記述させるための MCP サーバから構成されています。スクリプト言語は [Rhai](https://rhai.rs/) です。
主にサンドボックス環境下の MCP 経由で AI エージェントにスクリプトの記述を依頼し、
セキュアな環境にスクリプトだけ持ち込む、という使い方を想定しています。

> **開発初期段階です。** スクリプト API や MCP の仕様は予告なく変更されます。

## 特徴

- **GPU アクセラレーション** テンプレートマッチングは wgpu の compute shader で実行し、ピラミッド探索との組み合わせで
  4K 全画面探索も非常に高速です。GPU が無い環境では CPU 実装に自動でフォールバックします。
- **モダンなマッチングアルゴリズム** ZMD（ゼロ平均ダイス類似度）による画像マッチングにより、従来のアルゴリズムより正確な検出が可能です。
- **IDE** 画像リテラルがエディタ内にサムネイル表示され、キャプチャ・挿入・マッチ確認・実行・ステップ実行が 1 つのアプリで完結します。
- **MCP サーバ** AI エージェントによる `.rhai` スクリプトの自動記述が可能です。
- **Rust 記述による安全な実装** エンジン基本部分は全て Rust で記述され、Rhai スクリプトと結合されています。

## スクリプト例

```rhai
let win = expect_window("exe=notepad.exe").to_appear(10_000);
win.activate();

win.target("ui:name=ファイル,type=menuitem").click();   // アクセシビリティ API
win.target("ocr:名前を付けて保存").click();              // 画面上の文字（OCR）
win.target("image:field.png")
    .right_of("ocr:ファイル名", 200)                     // アンカー相対
    .type_text("report.txt");
target("ui:name=保存,type=button").click();

expect(win.target("ocr:report.txt")).to_appear(5000);  // 結果を検証
```

書き方の全体は [スクリプトガイド](docs/script-guide.ja.md)、関数と型の完全な一覧は
[Rhai API リファレンス](api/generated/rhai-api.md) を参照してください。

## 動作環境

- Windows 10 / 11（64bit）
- Vulkan または DirectX 12 が使える環境を推奨

## インストール

### インストーラーを使う

[Releases](https://github.com/ooISHoo/Mekiki/releases) から `Mekiki-<version>-windows-x64-setup.exe`
（NSIS）または `.msi` を取得して実行します。IDE と MCP サーバ `mekiki-mcp.exe` に加え、
利用者向けドキュメント一式（スクリプトガイド、IDE 操作ガイド、Rhai API リファレンス、
MCP の起動ガイドとリファレンス、`AGENTS.md`、ライセンス情報）とサンプルスクリプトが
インストール先の `docs/`、`api/`、`examples/` に入ります。
コマンドライン版 `mekiki.exe` も同じフォルダに入ります。インストーラーは PATH を変更しないので、
コマンドラインから使う場合はインストール先フォルダをご自身で PATH に追加してください。

インストーラーは自動起動・サービス登録・AI クライアントへの登録を**行いません**。

### ソースからビルドする

必要なもの:

- Rust stable（[rustup](https://rustup.rs)）と Visual Studio Build Tools（MSVC）
- Node.js 22 以降（IDE のフロントエンド）

```bash
git clone https://github.com/ooISHoo/Mekiki.git
cd mekiki
cd ide && npm install && cd ..
cargo build --release --features mekiki-ide/custom-protocol
```

成果物は `target/release/` に `mekiki-ide.exe`、`mekiki.exe`、`mekiki-mcp.exe` として出力されます。

> `--features mekiki-ide/custom-protocol` は IDE を単体起動可能にするために必須です。
> 無いと Tauri が開発モード扱いになり、Vite の開発サーバを探しに行って起動できません。
> `cd ide && npm run tauri build` でビルドする場合は自動で付与されます。

インストーラーを生成するには `scripts\build-installer.bat` を実行します（NSIS 形式。
`msi` または `all` を引数に指定すると MSI も生成）。

## 使い方

### IDE

![Mekiki IDE](docs/images/IDE_01.png)

右ペインの「ウィンドウ」から操作対象をエディタへドラッグ＆ドロップすると `window(...)` が挿入されます。
画像は SnippingTool を直接呼び出しキャプチャでき、画像リソースペインでプレビューと管理ができます。
`Ctrl+P` でマッチ位置をプレビュー可能で、ウィンドウ同様エディタにドラッグ＆ドロップして、コードの任意箇所に挿入できます。
操作の流れは [IDE 操作ガイド](docs/ide-guide.ja.md) を参照してください。

実行中はタスクバーのボタンに進捗表示が出ます。実行中、IDEが非アクティブでも `Shift+Alt+C` で
停止できます。

### コマンドライン

```bash
target/release/mekiki windows                          # ウィンドウ一覧
target/release/mekiki run examples/01-observe.rhai     # スクリプト実行
target/release/mekiki capture 300 300 120 120 ok.png   # 画面の一部を画像に保存
```

段階別のサンプルは [examples/](examples/README.md) にあります。

### MCP サーバ

`mekiki-mcp` は Mekiki のエンジンを [Model Context Protocol](https://modelcontextprotocol.io/)
で公開します。エージェントは `read_text` / `ui_tree` / `find` で画面を探索し、
`check_script` / `run_script` で検証した `.rhai` を `save_script` で残します。

```bash
target/release/mekiki-mcp.exe --base D:\MekikiWork            # デスクトップ操作あり
target/release/mekiki-mcp.exe --base D:\MekikiWork --observe  # 観察のみ
```

- 起動前の確認手順は [MCP 起動ガイド](docs/mcp-start.md)、ツールとオプションの一覧は
  [MCP リファレンス](docs/mcp-reference.md)、エージェントに読ませる briefing は
  [AGENTS.md](AGENTS.md)（英語）です。
- 実行中はタスクトレイにアイコンが出ます。メニューの **Quit Mekiki MCP** で、
  エージェントを介さずに終了できます。非常停止は `Shift+Alt+C` です。
- **IDE と MCP サーバを同時に動かさないでください。** 入力注入が競合します。
  デスクトップを動かす主体は同時に 1 つです。
- **現在の実装は意図的にセキュリティが緩めです。** 通常操作に承認プロンプトはありません。
  何を制限すべきかはエージェントの利用報告をもとに決める方針で、各エージェントには
  終了前のセキュリティ報告を義務づけています。誤操作されても困らない、重要なデータが無いマシンで動かしてください。

## ドキュメント

| 読む人 | ドキュメント |
|---|---|
| スクリプトを書く | [スクリプトガイド](docs/script-guide.ja.md) / [Rhai API リファレンス](api/generated/rhai-api.md) |
| IDE を使う | [IDE 操作ガイド](docs/ide-guide.ja.md) |
| AI エージェントと使う | [MCP 起動ガイド](docs/mcp-start.md) / [MCP リファレンス](docs/mcp-reference.md) / [AGENTS.md](AGENTS.md) |
| 設計を知りたい | [docs/architecture/](docs/architecture/)（Rhai API、マッチング、ウィンドウロケータ、IDE、MCP） |
| 保守する | [docs/maintenance/](docs/maintenance/)（Windows のキャプチャと UIA、入力の信頼性、MCP 運用の教訓、性能判断） / [テスト戦略](docs/testing.md) / [リリースチェックリスト](docs/release-checklist.md) |

全体の索引は [docs/README.md](docs/README.md) にあります。

## リポジトリ構成

```
crates/matching/     テンプレートマッチング（template-matching のフォーク + ZMD スコア、GPU/CPU）
crates/capture/      画面キャプチャとウィンドウ列挙
crates/input/        マウス・キーボード入力注入
crates/overlay/      デバッグ用の枠表示
crates/ocr/          画面文字認識
crates/uia/          アクセシビリティ API（UI Automation）
crates/core/         Target / Pattern / Match モデル、ピラミッド探索、自動待機、アサーション
crates/scripting/    Rhai への API 登録、アセット管理、CLI（mekiki）
crates/mcp/          MCP サーバ（mekiki-mcp）
ide/                 Tauri + CodeMirror 6 の IDE
api/                 Rhai API の正本（rhai-api.toml）と生成物
tools/               API ドキュメント生成、ゴールデンテスト生成
examples/            スクリプト例
testdata/            テスト用フィクスチャ
docs/                利用者向け・保守者向けドキュメント
```

`capture` / `input` などの OS 依存クレートはトレイトで抽象化してあり、非 Windows でも
ビルドが通ります。これは他 OS 対応のためではなく、OS 依存のコードが上位レイヤへ漏れていないことを
CI で保つための仕組みです。

## 開発

```bash
cargo test --workspace --release
```

`--release` を付けるのは、CPU 参照実装が debug ビルドでは桁違いに遅いためです。
GPU が使える環境では、skip を許さない設定で GPU 経路まで検証できます。

```bash
MEKIKI_REQUIRE_GPU=1 cargo test --workspace --release
```

- Rhai API を変更するときは `api/rhai-api.toml` を編集し、`cargo run -p mekiki-api-gen` で
  リファレンスと IDE の補完データを再生成します。生成物は直接編集しません。
- CI は `cargo fmt --check`、`cargo clippy -D warnings`、ワークスペース全体のテストを
  Windows 上で実行します。
- ベンチマークとゴールデン期待値の再生成には Python 3.10 以降と OpenCV のインストールが必要です
  （`tools/golden/`）。実行時の依存ではありません。
- ゴールデンテストとベンチマークの実行手順は
  [docs/maintenance/performance-decisions.md](docs/maintenance/performance-decisions.md) にあります。

## 現在の設計方針

- **SikuliX は API セットの参考であって互換対象ではありません。** 既存の `.sikuli`
  スクリプトを動かすことは目指していません。
- **対象は Windows のみです。** macOS / Linux は要件に含めていません。ただしマルチプラットフォームを念頭にした実装配慮はされています。
- **エンジンの診断メッセージとソースコメントは英語、IDE の UI は日本語と英語です。**

## ライセンス

Mekiki は [Apache License 2.0](LICENSE) で提供します。[NOTICE](NOTICE) も参照してください。

`crates/matching` は [template-matching](https://github.com/urholaukkarinen/template-matching)（MIT）
のフォークを起点としており、同クレートは MIT ライセンスを維持しています。詳細は
[ライセンスポリシー](docs/licensing.md) と [第三者ソフトウェア一覧](docs/third-party-software.md)
にあります。
