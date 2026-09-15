<!--
AI agents: For this script guide only, script-guide.ja.md is the canonical
master. Maintain the Japanese master here and derive all other language
versions from it. Translate only after a human has finalized the relevant
Japanese content. Do not overwrite this master with changes from a translated
version. This rule applies only to this document, not to other project
documentation.
-->

# Mekiki スクリプトガイド

このガイドは Mekiki および [Rhai](https://rhai.rs/) スクリプトを初めて記述する読者向けに、ごく基本から解説を行います。

関数や引数の完全な一覧は、正本の API カタログ（`api/rhai-api.toml`）から自動生成された [Rhai API リファレンス](../api/generated/rhai-api.md)
にあります。応用に進む場合はリファレンスを参照してください。

## 1. スクリプトの実行方法

- **IDE から**: スクリプトを開き、ツールバーの実行ボタンを押します。
  `print()` の出力とエラーはログ欄に出ます。使い方は [IDE 操作ガイド](ide-guide.ja.md) を参照してください。
- **コマンドラインから**: `mekiki run script.rhai` と実行します。

実行中にキーボードの `Shift+Alt+C` を押すと、IDE のウィンドウが非アクティブでも実行を停止できます。

## 2. 最初のスクリプト

メモ帳を開いた状態で、次の 4 行を実行してみてください。

```rhai
let win = expect_window("exe=notepad.exe").to_appear(10000); // メモ帳が現れるまで最大 10 秒待つ
win.type_text("Mekiki からこんにちは");                       // メモ帳を前面に出して文字を打つ
win.press("ctrl+s");                                          // 保存ダイアログを開く
expect_window("名前を付けて保存").to_appear(5000);             // ダイアログが出たことを確かめる
```

この 4 行に、Mekiki のスクリプトの骨格が全部入っています。

1. **ウィンドウを検索して変数に代入する。** `expect_window(...)` は条件に合うウィンドウが
   現れるまで待ち、見つかったウィンドウを返します。`let win = ...` で変数を宣言しつつ代入します。
2. **そのウィンドウに対して操作する。** `win.type_text(...)` や `win.press(...)` のように、
   変数の後ろに `.操作()` を続けます。
3. **結果を確認する。** `expect_window(...).to_appear(...)` は、期待した状態にならなければ
   エラーで止まります。「たぶん成功した」ではなく「成功を確認した」という事実がスクリプトに残ります。

## 3. Rhai の文法で最低限知っておくこと

```rhai
// を書くと改行までコメントになる

let name = "山田";                 // 変数。文字列は " " で囲む
let count = 3;                     // 数値
let total = count * 2 + 1;         // 演算式

print(`こんにちは ${name} さん`);   // ` ` で囲むと ${} の中に変数を埋め込める
print(total);                      // IDE のログ欄に出る

if total > 5 {                     // 条件分岐
    print("多い");
} else {
    print("少ない");
}

for i in 0..3 {                    // 0, 1, 2 と繰り返す
    print(i);
}
for item in ["りんご", "みかん"] {  // 配列を順に
    print(item);
}

fn greet(who) {                    // 関数。同じ手順をまとめる
    print(`やあ ${who}`);
}
greet("田中");
```

- 文の終わりには `;` を付けます。
- 操作に失敗するとエラーで止まります。止めたくない箇所は例外キャッチ構文 `try { ... } catch (e) { ... }` で囲みます。
  `e` にはエラーの内容が入ります。

```rhai
try {
    win.target("ocr:更新があります").click();
} catch (e) {
    print("更新の案内は出なかったので先へ進む");
}
```

## 4. 操作したい場所の指し方（ロケータ）

`target("...")` の中に書く文字列を**ロケータ**と呼びます。座標を書く代わりに
「何を探すか」を書くのが Mekiki の基本です。探し方は 3 種類あります。

| 探し方 | 書き方 | 向いている場面 |
|---|---|---|
| 画像 | `"ok.png"` または `"image:ok.png"` | アイコンなど、文字も名前も無いもの |
| 画面上の文字（OCR） | `"ocr:保存"` | ボタンやラベルの文字が見えているもの |
| UI 要素（アクセシビリティ情報） | `"ui:保存"` / `"ui:name=保存,type=button"` / `"ui:id=saveButton"` | 一般的な Windows アプリのボタン・入力欄。見た目が変わっても効く |

このほかに、座標をそのまま指す `"point:100,200"` と、矩形を指す `"region:0,0,800,600"` があります。
座標は画面全体の絶対座標です。最後の手段と考えてください。

**迷ったら `ui:` → `ocr:` → 画像の順に試してください。** `ui:` はテーマや DPI が変わっても効き、
画面キャプチャが使えない環境でも動きます。

### 画像ファイルの置き場所

- スクリプトと同じフォルダに置き、ファイル名だけで指します（`"ok.png"`）。サブフォルダは
  `"buttons/ok.png"` のように書きます。
- IDE の貼り付けボタンで取り込んだ画像は `.mekiki/images` フォルダに入り、
  `"image:sha256:..."` という内容ハッシュで参照されます。名前を付けてリネームすると
  スクリプトと同じフォルダに移動します。
- 絶対パス（`"C:\\shots\\ok.png"`）も使えます。

### `ui:` の名前と種類を調べる

`ui:` で指すには、アプリが公開している要素の名前や種類を知る必要があります。
一度だけ次のスクリプトを実行し、ログに出た一覧からコピーしてください。

```rhai
let win = window("exe=notepad.exe");
for item in win.list_ui() {
    print(item);          // ui:name=...,id=...,type=... の形で出る
}
```

`type=` に書ける種類は次の 16 個です。

```text
button  checkbox  combobox  edit  hyperlink  image  list  listitem
menuitem  radiobutton  tab  tabitem  text  tree  treeitem  window
```

日本語の Windows では、ボタン名に `保存(S)` のようなショートカット文字が付いています。
`ui:name=保存` と書けばどちらにも一致するので、`(S)` は書かなくてかまいません。

## 5. ウィンドウで範囲を絞る

ほとんどの操作は、まずウィンドウを取り、その中で探します。画面全体から探すより速く、
後ろにある別のウィンドウの同じボタンを誤って押すこともなくなります。

```rhai
let win = window("exe=notepad.exe");            // 今あるウィンドウを即座に取る（無ければエラー）
let win = expect_window("exe=notepad.exe").to_appear(10000); // 現れるまで待つ（起動直後はこちら）

win.target("ocr:保存").click();                  // win の中だけを探す
```

| 指定方法 | 書き方 | 探し方 |
|---|---|---|
| タイトルの一部 | `window("メモ帳")` | タイトルに「メモ帳」を含む |
| 実行ファイル名 | `window("exe=notepad.exe")` | **通常はこれ。** 開いているファイル名でタイトルが変わっても効く |
| タイトル完全一致 | `window("title_exact=電卓")` | タイトル全体が一致 |
| タイトルのパターン | `window("title=議事録*")` | `*` は 0 文字以上、`?` は 1 文字 |
| 複数条件 | `window("exe=notepad.exe,title=議事録*")` | すべて満たすもの |
| ウィンドウクラス | `window("class=Notepad")` | クラス名が `Notepad` で始まる |
| プロセス ID | `window("pid=1234")` | 指定したプロセスのウィンドウ |

電卓や設定など一部の Windows アプリは `ApplicationFrameHost.exe` を共有しているため、
`exe=` では区別できません。それらは `title_exact=電卓` のようにタイトルで指してください。

`win.activate()` でウィンドウを前面に出せます。`win.type_text()` と `win.press()` は
自動で前面に出してから入力するので、通常は明示的に呼ぶ必要はありません。

## 6. 操作する

`target(...)` の後ろに操作を続けます。どの操作も、対象が**現れて、動きが止まる**まで
自動で待ってから実行します。既定の待ち時間は 3 秒です。

```rhai
win.target("ui:name=保存,type=button").click();      // クリック
win.target("ocr:ファイル").double_click();            // ダブルクリック
win.target("ocr:ファイル").right_click();             // 右クリック
win.target("ui:type=edit").type_text("report.txt");   // クリックしてから入力
win.target("ui:type=edit").press("ctrl+a");           // 対象の上でキーを押す
win.target("ocr:一覧").scroll(0, -3);                 // 対象の上でスクロール（負の値で下へ）
win.target("file.png").drag_to(win.target("folder.png")); // ドラッグ
win.target("ocr:ヘルプ").hover();                     // マウスを載せるだけ
```

ウィンドウに直接入力する方法もあります。ウィンドウを前面に出し、そのとき
フォーカスがある場所に入力します。クリックはしません。

```rhai
win.type_text("本文をここに");
win.press("ctrl+s");
```

キーの書き方は、修飾キーとキー名を `+` でつなぎます。`ctrl+s`、`shift+alt+c`、`enter`、`tab`、
`escape`、`f5`、`alt+f4` などです。文字を打つときはキーではなく `type_text` を使ってください。
キーボード配列に依存せず、日本語もそのまま打てます。

### 文字入力で気を付けること

- `type_text` の中の改行は Enter、タブは Tab として送られます。
- どこに入力されるかは、そのときのフォーカス次第です。通知や別ウィンドウの起動でフォーカスが
  移ることがあるので、`win.type_text()` か `target(...).type_text()` のように相手を明示してください。
  `type_text("...")` と単独で呼ぶ形は、相手を指定しないので避けてください。
- ウィンドウがダイアログで塞がれていると、入力はダイアログの名前を添えたエラーになります。
  その場合はダイアログの方を対象にしてください。
- 大事な入力は、打った後に `read_value` や `read_text` で読み返して確認してください。
  Windows 11 のメモ帳は既定の速さでも文字を取りこぼすことがあります。

## 7. 待つ・確かめる

「処理が終わるまで `sleep(3000)` で待つ」書き方は、遅い日に壊れます。代わりに
**目に見える結果**を待ってください。

```rhai
expect(win.target("ocr:保存しました")).to_appear(5000);        // 現れるのを待つ（出なければエラー）
expect(win.target("ocr:処理中")).to_vanish(30000);              // 消えるのを待つ
expect(win.target("ui:type=listitem")).to_have_count_at_least(1, 5000); // 1 件以上になるのを待つ

let win = expect_window("exe=app.exe").to_appear(10000);       // ウィンドウが現れるのを待つ
expect_window("title_exact=進捗").to_vanish(60000);             // ウィンドウが消えるのを待つ
```

- 引数のミリ秒に `0` を渡すと既定の待ち時間になります。
- `to_appear` は見つかった位置（Match）を返すので、そのまま `.click()` できます。
- 一回だけ見て真偽が欲しいときは `t.exists()` を使います。待ちません。
- 数分かかる処理を待つときだけ、`exists()` と `sleep()` を組み合わせて自分の間隔で見に行きます。
  `expect` の再試行は 333 ミリ秒ごとに画面全体を探すので、長時間には向きません。

```rhai
let done = win.target("ocr:完了");
let waited = 0;
while !done.exists() {
    if waited >= 600000 { throw "10 分待っても完了しなかった"; }
    sleep(2000);
    waited += 2000;
}
```

## 8. 画面から読み取る

```rhai
let lines = win.read_text();                       // OCR で読んだ文字列の配列（読み順）
let value = win.read_value("ui:id=amountBox");     // 入力欄などの値を UI 情報から読む（パスワード欄は不可）
let text = clipboard();                            // クリップボードの文字列
set_clipboard("貼り付ける文字");

let m = win.find("ocr:合計");                      // 位置を 1 つ取る（無ければエラー）
print(`${m.x}, ${m.y}, ${m.width}x${m.height}, score=${m.score}`);
let all = win.find_all("ui:type=listitem");        // 全部取る（無ければ空の配列）
print(all.len());
```

`find` や `to_appear` が返す **Match は、その時点の座標の記録**です。画面が変わると
古くなるので、画面遷移をまたいで持ち回らず、必要になったときにもう一度探してください。

## 9. 同じものが複数あるとき

```rhai
win.target("ui:type=button").first().click();                  // 読み順で最初
win.target("ui:type=button").last().click();                   // 読み順で最後
win.target("ui:type=button").nth(2).click();                   // 読み順で 3 番目（0 から数える）
win.target("icon.png").best().click();                         // 一番よく似ているもの

win.target("ui:name=開く,type=button").right_of("ocr:顧客A", 300).click(); // 「顧客A」の右 300px 以内
win.target("ui:type=edit").below("ocr:氏名", 100).type_text("山田");    // 「氏名」の下 100px 以内
```

`right_of` `left_of` `above` `below` `near` の基準（アンカー）には `ocr:` か画像を使います。
`nth` より、ラベルとの位置関係で指す方が、並び順が変わっても壊れません。

同じ目的の指定を並べておくと、前のものが見つからないときに次を試します。安定していて
軽いものを先に書きます。

```rhai
win.target("ui:name=保存,type=button").or("ocr:保存").or("save.png").click();
```

## 10. ソフトウェアカーソルを使っているアプリケーションの操作

ゲームなど、ソフトウェアカーソルを使っているアプリケーションの場合、既定で有効化されている操作直前の再探索で、カーソル自身が邪魔をしてしまい、操作に失敗してしまいます。
このため、ソフトウェアカーソルを使っているアプリケーションでは、次のAPIを使って再探索を無効化してください。特定 UI だけ再探索を有効にする事もできます。

```rhai
set_recheck(false);
let ok = win.target("ok.png");
ok.hover();               // ここで探す
ok.click();               // 探し直さず、hover で見つけた場所を押す

win.target("ocr:次へ").recheck(true).click();   // この対象だけ探し直す
```

次にマウスの移動速度です。既定速度はソフトウェアカーソルのアプリケーションに合わせ、安定側に寄せた控えめな値になっています（具体的な値は
[Rhai API リファレンス](../api/generated/rhai-api.md) の「Defaults and settings」を参照）。
それでもフレームレートが低いアプリケーションでは、カーソルの位置と実際の操作座標がずれることがあります。その場合は、アプリの反応を見ながら移動速度を調整してください。

```rhai
set_move_speed(600);      // 遅くする。追従が遅いアプリ向け
set_move_speed(2400);     // 速くする。毎秒 2400 ピクセル
set_move_speed(0);        // 移動を省いて瞬時に
```

通常の Windows アプリケーションでは `0`（瞬時移動）でも概ね問題ありません。ただし、
ポインタが乗ったことに反応して開くメニューやツールチップを操作する箇所では、
既定のままにしておくのが安全です。

## 11. うまく動かないとき

**エラーメッセージを確認** 画像や文字が見つからないと、次のようにログ出力されます。

```text
'ok_button.png' not found in 3840x2160+0+0 (required 0.70 / best 0.61 / waited 3.0s)
  diagnostic files: ...-screen.png, ...-heatmap.png, ...-annotated.png, ...-candidates.txt
```

- `required` は必要な一致度、`best` は画面で見つかった一番近いものの一致度です。
  近ければ画像を撮り直すか `.similar(0.6)` のように下げます。大きく離れていれば、
  別の画面やウィンドウを見ています。
- 診断ファイルには、そのときの画面、一致度の分布、候補の位置が保存されます。

**検索結果を確認**

```rhai
win.target("ocr:保存").highlight(1000);   // 見つけた場所を 1 秒枠で囲む
win.save("evidence.png");                 // ウィンドウを画像として保存
```

**パラメータの調整**

| 症状 | 対処 |
|---|---|
| 見つかるまでに 3 秒以上かかる | `.timeout(10000)` を付ける。全体に効かせるなら `set_timeout(10000)` |
| 画像が見つからない | `.similar(0.6)`。既定は 0.7。画像を撮り直すのが先 |
| 常に動いている表示（アニメーション）で止まらない | `.force()` で静止待ちを省く |
| 押した直後の画面で次の対象が見つからない | 前の操作の結果を `expect(...).to_appear(...)` で待ってから次へ |
| アプリが短いクリックを無視する | `set_click_hold(80)` |
| 文字が抜ける | `set_type_interval(50)` で 1 文字ごとの間隔を延ばす |
| マウスの移動が遅く、全体の実行時間が長い | `set_move_speed(2400)` で移動速度を上げる（前項） |
| `hover` の直後の `click` で、対象を探し直す時間が無駄 | `set_recheck(false)` で直前の探索結果を使い回す（前項） |

設定は `set_...()` で変え、そのスクリプトの実行中だけ有効です。読み戻す関数は無いので、
元に戻したければ元の値を自分で入れ直します。

**アプリケーションによる制限**

なおゲームアプリケーションなどでは、アンチチート機能で外部からのマウスやキーボードの入力注入を一切拒否するものがあります。まず短いスクリプトを書いて外部操作が許可されているか確認してください。

## 12. 壊れにくいスクリプトの心得

- 画面全体ではなく、ウィンドウの中で探す。
- 座標や並び順ではなく、名前・種類・ラベルとの位置関係で指す。
- `sleep` ではなく `expect` で「結果が出た」ことを待つ。
- 一様な領域（真っ白な四角など）を画像にしない。特徴の無い画像はどこにでも一致します。
- 探した位置（Match）を画面遷移の後まで使い回さない。
- 入力は相手（ウィンドウか対象）を必ず指定する。
- やみくもに `try` で握り潰さず、適切なエラーハンドリングを書きましょう。

## 付録: チートシート

```rhai
// ウィンドウ
let win = window("exe=app.exe");                               // 今すぐ
let win = expect_window("exe=app.exe").to_appear(10000);       // 現れるまで待つ
win.activate();

// 対象の指定
win.target("ok.png")  win.target("ocr:保存")  win.target("ui:name=保存,type=button")
win.target("point:100,200")  win.target("region:0,0,800,600")

// 操作
.click()  .double_click()  .right_click()  .middle_click()  .hover()
.type_text("文字")  .press("ctrl+s")  .scroll(0, -3)  .drag_to(win.target("b.png"))
win.type_text("文字")  win.press("enter")

// 絞り込み
.first()  .last()  .nth(i)  .best()  .or("ocr:保存")  .similar(0.8)  .timeout(5000)  .force()
.right_of("ocr:ラベル", 200)  .left_of(...)  .above(...)  .below(...)  .near(...)

// 待つ・確かめる
expect(t).to_appear(ms)  .to_vanish(ms)  .to_have_count(n, ms)  .to_have_count_at_least(n, ms)
expect_window("...").to_appear(ms)  .to_vanish(ms)
t.exists()  t.wait_vanish(ms)  sleep(ms)

// 読む
win.read_text()  win.read_value("ui:id=x")  win.list_ui()  clipboard()  set_clipboard("...")
win.find("...")  win.find_all("...")   // Match: x y width height center_x center_y score

// 見る・記録
t.highlight(1000)  win.save("shot.png")  print("...")

// 設定（実行中だけ有効）
set_timeout(ms)  set_similarity(0.7)  set_type_interval(ms)  set_click_hold(ms)
set_move_speed(px_per_sec)  set_recheck(bool)  t.recheck(bool)
```
