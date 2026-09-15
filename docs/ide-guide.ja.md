<!--
AI agents: For this IDE guide only, ide-guide.ja.md is the canonical master.
Maintain the Japanese master here and derive all other language versions from
it. Translate only after a human has finalized the relevant Japanese content.
Do not overwrite this master with changes from a translated version.
This rule applies only to this document, not to other project documentation.
-->

# Mekiki IDE 操作ガイド

Mekiki-IDEは、Mekiki用のRhaiスクリプトの編集、UI画像リソースのキャプチャおよび管理とマッチプレビュー、操作対象ウィンドウの検出、スクリプトの実行を行うアプリケーションです。

## UI構成

![UI構成](./images/IDE_01i.png)


| 領域 | 用途 |
|---|---|
| ツールバー | スクリプト新規作成、保存、実行、停止、設定など |
| コードエディタ | Rhaiスクリプトの編集 |
| リソース | UI画像リソースの管理、操作対象ウィンドウのリスト、上部ペインで切り替えます |
| ログ出力 | 各種ログがテキスト出力されます |

## 最初のクリック操作まで

- ツールバー左端の'新規作成'を押すか、`Ctrl+N`を押下します。
- ツールバー左から3番目の'保存'を押すか、`Ctrl+S`を押下します。スクリプト内で画像リソースを使ってUIの検索を行う場合、ワークフォルダを決めるため、保存が必要です。
- 右のリソースペインを'ウィンドウ'に切り替えます、起動中アプリのウィンドウ一覧が表示されるので、対象のウィンドウをコードエディタへドラッグ&ドロップします。
- Windows標準の電卓を例に取ると、以下のようなコードがドロップ箇所のエディタ中に挿入されます。
```
window("title_exact=Calculator")
```
- 以降の操作ができるよう、一旦変数に代入し、Rhaiスクリプトとして正しい形に修正します。
```
let win = window("title_exact=Calculator");
```
- 次に操作対象の電卓のUIパーツをキャプチャします、右のリソースペインを'画像リソース'に切り替えます。ペイン上部の'スニッピングツールを開く'ボタン
<img src="./images/icon-snip.svg" alt="スニッピングツール" width="24">
をクリックします。するとWinodws標準のスニッピングツールが起動します。操作対象のUIを選択してください。
- 次にペーストボタン
<img src="./images/icon-paste.svg" alt="ペースト" width="24">
をクリックします。すると一時リソースとして画像アセット中に保存されます。このままスクリプト中から参照する事も可能ですが、適切な名前にリネームする事を強く推奨します。

  ![UI画像リソース](./images/IDE_02.png)

- コードエディタでクリック操作を記述します。
```
win.target().click();
```
- 先ほど作成したUI画像リソースをコードエディタの`target()`の括弧内にドラッグ&ドロップします。以下のような表示なれば正常です。

 ![画像リソースの参照プレビュー](./images/IDE_03.png)

- 実行します。対象ウィンドウのアクティブ化処理は入れていないので、電卓ウィンドウを見える位置に移動し、ツールバーの実行ボタン
<img src="./images/icon-run.svg" alt="実行" width="24">をクリックします。スクリプトが実行され、電卓の`1`ボタンが押されるはずです。

## スクリプトの強制停止
スクリプトの実行中に`Shift+Alt+C`を押下すると、アプリケーションがアクティブで無い状態でもスクリプトを強制停止します。これはMCPやCLI実行でも同様に停止できます。

## 画像リソースのルール

ペーストボタン
<img src="./images/icon-paste.svg" alt="ペースト" width="24">
で保存された画像は、スクリプトが保存されているパス内の.mekikiフォルダ以下に一時配置されます。一時配置のまま運用する際はこのフォルダとの関係を保つようにしてください。またリネームすると、自動的にスクリプトが保存されているパスと同一フォルダに移動されます。**IDEはスクリプトと画像リソースが同一フォルダに存在する事を前提とした仕様になっていますが、スクリプトの仕様は相対、絶対両方に対応しています。CLI実行ではこの制限はありません。**

また画像リソースペイン下部のプレビューボタンで、Mekikiが実際にキャプチャした画像を検索できるかテストを行う事ができます。左側のスライダーでしきい値を調整します。プレビュー結果はデスクトップ上に直接オーバーレイ表示されます。

