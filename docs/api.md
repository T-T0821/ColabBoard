# REST API(外部からボードを操作する)

グラボードのボードは、ブラウザの外からも読み書きできる。スクリプトや CI、チャットボット、
ほかのツールとの連携に使う。API で加えた変更は、そのボードを開いている人の画面にすぐ反映される
(WebSocket の同期と同じ CRDT の更新として配られる)。

- ベース URL: `https://<サーバー>/api/v1`
- 形式: JSON(UTF-8)
- 認証: `Authorization: Bearer <API トークン>`
- Rust からは SDK クレート `graboard-sdk` も使える(準備中)。

## API トークン

1. ブラウザでログインした状態で `POST /api/tokens` から作る。
2. 表示されたトークン(`gbt_…`)を控える。**作ったときにしか表示されない**。
3. リクエストに `Authorization: Bearer gbt_…` を付ける。

| 項目 | 内容 |
| --- | --- |
| `scope` | `read`(読むだけ。閲覧者として扱う)か `write`(読み書き。編集者として扱う) |
| `boards` | 使えるボードを ID で限る(省略すると、自分が開けるボードすべて) |
| `expires_in_days` | 有効期間(日)。省略すると無期限 |

トークンで使える権限は「トークンのスコープ」と「持ち主のそのボードでの権限」の弱い方になる。
たとえば閲覧者として招待されたボードには、`write` のトークンでも書けない。
トークンでは共有設定の変更・ボードの削除・トークンの作成はできない(ブラウザのログインでだけ行う)。

サーバーにはトークンのハッシュ(BLAKE2b)だけを保存する。漏れたらすぐ一覧から無効にする。

| メソッド | パス | 内容 |
| --- | --- | --- |
| `GET` | `/api/tokens` | 自分のトークン一覧(秘密の部分は含まない) |
| `POST` | `/api/tokens` | 作成。`{"name": "CI", "scope": "write", "boards": ["abc123"], "expires_in_days": 90}` → `{"token": "gbt_…", "info": {…}}` |
| `DELETE` | `/api/tokens/{id}` | 無効にする |

## エンドポイント

ボードを開くのに共有リンクの鍵が要る場合は `?key=…` を付ける(トークンなしでも使える)。
ログインせずに `?room=` で作った「誰でも編集できるボード」は、トークンなしで読み書きできる。

| メソッド | パス | 必要な権限 | 内容 |
| --- | --- | --- | --- |
| `GET` | `/me` | ログイン | 自分(トークンで呼んだときはトークンの情報も) |
| `GET` | `/boards` | ログイン | 自分のボード一覧(トークンで限ったボードだけ) |
| `POST` | `/boards` | `write`(ボードを限らないトークン) | ボードを作る `{"title": "…"}` |
| `GET` | `/boards/{id}` | 閲覧 | ボードの情報と自分の権限 |
| `GET` | `/boards/{id}/elements` | 閲覧 | 要素の一覧(下の絞り込み) |
| `POST` | `/boards/{id}/elements` | 編集 | 要素を 1 つ作る([新しい要素](#新しい要素)) → 201 |
| `GET` | `/boards/{id}/elements/{eid}` | 閲覧 | 要素を 1 つ取る |
| `PATCH` | `/boards/{id}/elements/{eid}` | 編集 | 要素を変える([変更](#変更)) |
| `DELETE` | `/boards/{id}/elements/{eid}` | 編集 | 要素を消す → 204 |
| `POST` | `/boards/{id}/ops` | 編集 | まとめて作る・変える・消す([一括操作](#一括操作)) |
| `GET` | `/boards/{id}/comments` | 閲覧 | コメント一覧 |
| `POST` | `/boards/{id}/comments` | コメント | コメントを書く `{"body": "…", "x": 0, "y": 0}` |

失敗したときは HTTP ステータスと `{"error": "理由"}` を返す。

| ステータス | 意味 |
| --- | --- |
| 400 | 入力の誤り(どの操作の何がおかしいか、メッセージに書いてある) |
| 401 | ログインしていない、またはトークンが正しくない・期限切れ |
| 403 | 権限が足りない |
| 404 | ボードや要素がない、またはそのボードを開く権限がない |

### 要素の絞り込み

`GET /boards/{id}/elements` のクエリ文字列。全部省略すると全要素を奥から順に返す。

| パラメーター | 例 | 内容 |
| --- | --- | --- |
| `type` | `shape` | `shape` / `connector` / `stroke` |
| `kind` | `sticky` | 図形の種類 |
| `bbox` | `0,0,1000,800` | この範囲(x0,y0,x1,y1)に重なる要素 |
| `text` | `課題` | 文字にこの語を含む(大文字小文字を区別しない) |
| `ids` | `1a2b,3c4d` | ID を指定 |
| `limit` | `100` | 最大件数 |

## 要素の形

ID は 16 進の文字列、座標はボードのワールド座標(左上が原点、y は下向き)、色は `#rrggbb` か `#rrggbbaa`。

```json
{
  "id": "5f3a00000001",
  "type": "shape",
  "kind": "sticky",
  "x": 10, "y": 20, "w": 200, "h": 200,
  "text": "API から",
  "fill": "#fff3a3", "stroke": "#00000000", "stroke_width": 0,
  "text_color": "#1f1f1f", "font_size": 0, "align": "center",
  "z": 12, "group": 0, "locked": false, "link": ""
}
```

```json
{
  "id": "5f3a00000003",
  "type": "connector",
  "from": {"element": "5f3a00000001"},
  "to": {"x": 600, "y": 120},
  "text": "", "color": "#1f1f1f", "width": 2,
  "style": "elbow", "start_cap": "none", "end_cap": "arrow",
  "z": 13, "group": 0, "locked": false, "link": ""
}
```

```json
{"id": "…", "type": "stroke", "points": [[0, 0], [10, 4]], "color": "#1f1f1f", "width": 4, "highlighter": false, "z": 14, "group": 0, "locked": false, "link": ""}
```

図形の `kind` は `sticky`(付箋)、`text`、`frame`、`rect`、`round-rect`、`ellipse`、`triangle`、`diamond`、
`hexagon`、`star`、`cloud`、`speech`、`arrow-right`、`cylinder`、`document` など(一覧は
`crates/board-core/src/shape.rs` の `KINDS`)。画像は API では作れない(`has_image: true` で有無だけわかる)。

### 新しい要素

`type` 以外は省略でき、種類ごとの既定値になる(付箋は 200×200 の黄色)。

```json
{"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "メモ", "fill": "#b5e3ff"}
{"type": "shape", "kind": "frame", "x": -50, "y": -80, "w": 1200, "h": 800, "text": "振り返り"}
{"type": "connector", "from": {"element": "5f3a00000001"}, "to": {"element": "5f3a00000002"}, "text": "依存"}
{"type": "stroke", "points": [[0, 0], [40, 10], [80, 0]], "color": "#e5484d", "width": 6}
```

コネクタの端は `{"x": …, "y": …}`(点)か `{"element": "<ID>"}`(要素につなぐ。相手が動くと追従する)。
`{"element": "…", "anchor": [0.5, 0]}` のように相手の外接矩形内の位置(0〜1)も指定できる。

### 変更

指定したフィールドだけ変わる。要素の種類に合わないフィールド(付箋の `from` など)は 400 になる。

```json
{"x": 300, "text": "更新しました", "fill": "#c4f0c2", "locked": true, "z": 100}
```

| 種類 | 使えるフィールド |
| --- | --- |
| 図形 | `x` `y` `w` `h` `kind` `text` `fill` `stroke` `stroke_width` `text_color` `font_size` `align` `icon` |
| コネクタ | `from` `to` `text` `color` `width` `style` `start_cap` `end_cap` |
| 手書きの線 | `x` `y`(外接矩形の左上を動かす) `points` `color` `width` `highlighter` |
| どれでも | `z` `group` `locked` `link` |

### 一括操作

`POST /boards/{id}/ops` は複数の操作を順に適用する。**1 つでも失敗したら何も変わらない**。
同じ一括操作で作った要素は `"$0"`, `"$1"`, …(作った順)で参照できる。

```json
{"ops": [
  {"op": "create", "element": {"type": "shape", "kind": "rect", "x": 0, "y": 0, "text": "設計"}},
  {"op": "create", "element": {"type": "shape", "kind": "rect", "x": 400, "y": 0, "text": "実装"}},
  {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}}},
  {"op": "update", "id": "5f3a00000001", "set": {"text": "済"}},
  {"op": "delete", "id": "5f3a00000002"}
]}
```

結果:

```json
{"created": ["…", "…", "…"], "elements": [/* 作った・変えた要素 */], "deleted": ["5f3a00000002"]}
```

1 回に送れる操作は 2000 個まで。文字は 1 要素 20,000 文字まで。

## curl の例

```sh
TOKEN=gbt_xxxxxxxxxxxx_yyyyyyyy…
BASE=http://localhost:8080/api/v1

# ボードを作って付箋を置く
BOARD=$(curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"title":"API で作ったボード"}' $BASE/boards | jq -r .id)
curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"type":"shape","kind":"sticky","x":0,"y":0,"text":"こんにちは"}' \
  $BASE/boards/$BOARD/elements

# 「課題」を含む付箋を探す
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/boards/$BOARD/elements?kind=sticky&text=課題"
```

## 仕組み

サーバーは今のボードの状態を写した使い捨てのボードに操作を適用し、できた CRDT の更新を
ルームに取り込んで全員に配る(`Hub::edit`)。同時にブラウザで編集している人の変更とは yrs がそのまま合わせる。
API での変更もバージョン履歴の自動保存の対象になる。
