# コマンドライン(`graboard`)

ブラウザを開かずに、ボードを作る・変換する・書き出す。Web クライアントやサーバーと同じ Rust のコア
(`board-core`)でファイルを読み書きし、GPU なしの描画(`board-export`)で PNG / PDF にする。

```sh
cargo install --path crates/board-cli      # graboard が入る
graboard --help
```

日本語フォント(Noto Sans JP)をバイナリに同梱している(`--no-default-features` で外すと、
`--font`・環境変数 `GRABOARD_FONT`・システムのフォントの順に探す)。

## コマンド

| コマンド | 内容 |
| --- | --- |
| `graboard new [テンプレート] -o 出力` | テンプレートから新しいボード(名前は `graboard templates`) |
| `graboard convert 入力... -o 出力` | 読み込んで別の形式に。入力が複数なら横に並べて 1 枚にする |
| `graboard info 入力` | 要素の数・範囲・フレーム(スライドの順) |
| `graboard templates` | テンプレートの一覧 |
| `graboard formats` | 読み書きできる形式 |
| `graboard pull ボードID -o 出力` | サーバーのボードを書き出す(サーバーで描く) |
| `graboard push 入力... --board ID` | ファイルをサーバーのボードに読み込む(`--title 題名` なら新しいボードを作る) |

## 形式

読み込み(拡張子と中身から推測。`--from` で指定もできる):

| 形式 | 拡張子 |
| --- | --- |
| グラボード | `.graboard`(ボードの yrs 更新そのもの。画像も含めて欠けずに保存) |
| JSON | `.json`。要素の一覧(`--to json` や REST API の出力)、または REST API と同じ一括操作 `{"ops": [...]}` |
| マークダウン | `.md` `.txt`(見出しがフレーム、箇条書きが付箋、表が表) |
| Mermaid / PlantUML | `.mmd` `.mermaid` / `.puml` `.plantuml` `.pu` |
| draw.io / Miro | `.drawio` `.dio` `.xml` / Miro の書き出し(`.json`) |
| 表 | `.csv` `.tsv`(Shift_JIS も可)、`.xlsx` `.xls` `.ods` |

書き出し(`-o` の拡張子か `--to`): `.graboard` / `.png` / `.pdf` / `.json` / `.md`。`-o -` で標準出力。

## 書き出しの設定

| オプション | 内容 |
| --- | --- |
| `--scale 数` | 1 単位を何画素にするか(既定 2)。4,000 万画素を超えるときは縮める |
| `--padding 数` | 余白(既定 48) |
| `--background 色` | 背景色(`#rrggbb`)か `transparent` |
| `--frame 名前` | フレーム 1 つだけ(名前・番号・要素 ID) |
| `--frames` | フレームを 1 つ 1 ページの PDF に(スライドと同じ順) |
| `--title 題名` | PDF の文書名 |
| `--hide-private` | 個人作業エリアの中身を伏せる |

## 例

```sh
# テンプレートから画像を作る
graboard new kpt -o 振り返り.png

# Mermaid のフロー図を PDF に
graboard convert flow.mmd -o flow.pdf

# 議事録と表を 1 枚のボードにまとめて保存(あとで Web から読み込める)
graboard convert 議事録.md 売上.xlsx -o まとめ.graboard

# フレームごとのスライド PDF
graboard convert 企画.graboard --frames -o 企画.pdf

# スクリプトでボードを組み立てる(REST API の一括操作と同じ JSON)
cat > plan.json <<'JSON'
{"ops": [
  {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "企画"}},
  {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 300, "y": 0, "text": "開発"}},
  {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}, "end_cap": "arrow"}}
]}
JSON
graboard convert plan.json -o plan.png

# サーバーとやり取りする(トークンはボード一覧の「開発者向け」で作る)
export GRABOARD_URL=https://board.example.com GRABOARD_TOKEN=gbt_…
graboard pull abc123 --frames -o 企画.pdf
graboard push 図.drawio --board abc123
graboard push 振り返り.graboard --title "11 月の振り返り"
```

終了コードは成功で 0、失敗で 1、引数の誤りで 2。

## 描画について

`board-export` は CPU で塗るラスタライザで、図形(内外判定を 1 画素 4 点で調べるアンチエイリアス)・
線と矢印の飾り・破線・手書き・画像・動画の表紙・文字(折り返し・揃え・縦書き)を描く。
Web の GPU 描画に近い見た目にしてあるが、構成図のアイコン画像と付箋の作者名は描かない。
同じ描画をサーバーの書き出し API(`GET /api/v1/boards/{id}/export`、[docs/api.md](api.md))と
公開ページの OGP 画像でも使う。
