# ボードを GitHub で管理する

ボードを GitHub のリポジトリにテキストとして置き、コミット・プルリクエスト・レビューで管理する。
GitHub 上でファイルを直しても、同期すればボードに取り込まれる。

## ファイルの形(`.graboard.json`)

```json
{
  "graboard": 1,
  "title": "11 月の振り返り",
  "elements": [
{"id":"7a00000001","type":"shape","align":"center","corner_radius":4,"fill":"#fff3a3","font_size":0,"h":200,"kind":"sticky","stroke":"#00000000","stroke_width":0,"text":"よかったこと","text_color":"#1f1f1f","valign":"middle","w":200,"x":0,"y":0,"z":1},
{"id":"7a00000002","type":"shape","align":"center","corner_radius":4,"fill":"#fff3a3","font_size":0,"h":200,"kind":"sticky","stroke":"#00000000","stroke_width":0,"text":"次にやること","text_color":"#1f1f1f","valign":"middle","w":200,"x":300,"y":0,"z":2},
{"id":"7a00000003","type":"connector","color":"#1f1f1f","end_cap":"arrow","from":{"element":"7a00000001"},"start_cap":"none","style":"elbow","text":"","to":{"element":"7a00000002"},"width":2,"z":3}
  ]
}
```

- **1 要素 1 行**で、重なり順(奥から)に並ぶ。付箋を 1 枚動かすと差分も 1 行になり、別々の要素を
  変えたブランチは Git の行単位のマージでぶつかりにくい。
- 要素の中のキーは `id`・`type` のあとに名前順。既定値の項目(リンクなし・ロックなしなど)は書かない。
- 数値は短い表記(`100.0` は `100`、`0.1` は `0.1`)、色は `#rrggbb` か `#rrggbbaa`。
- 画像は中身のハッシュを名前にした別ファイル(ボードのファイルと同じディレクトリの
  `assets/<hash>.png`)で、要素には `"image": "assets/…"` だけを書く。同じ画像は 1 つにまとまる。
- 要素の ID は読み込み直しても変わらない。手で書くときは 16 進の文字列を付ける。
- 項目の名前は REST API の要素の形([api.md](api.md))とほぼ同じ。省略した項目は種類ごとの既定値になる。

```json
{"graboard": 1, "title": "手書き", "elements": [
{"id": "1", "type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "企画"},
{"id": "2", "type": "shape", "kind": "sticky", "x": 300, "y": 0, "text": "開発"},
{"id": "3", "type": "connector", "from": {"element": "1"}, "to": {"element": "2"}}
]}
```

## つなぐ(ボードのオーナー)

「共有」パネルの「GitHub で管理」に入力する。

| 項目 | 内容 |
| --- | --- |
| リポジトリ | `owner/name` か `https://github.com/owner/name` |
| ブランチ | 省略するとリポジトリの既定のブランチ |
| ファイルの場所 | 省略すると `boards/{ボードID}.graboard.json`。`.json` で終わる相対パス |
| トークン | fine-grained の個人アクセストークン(そのリポジトリの **Contents** と **Pull requests** の読み書き)。保存後は表示しない |
| 自動で取り込む | GitHub への push を Webhook で受けてボードにマージする |

保存するとき、トークンでリポジトリとブランチを読めるか確かめる。サーバーの `GITHUB_TOKEN` を
使うなら、管理者が `GITHUB_SYNC_REPOS` に対象のリポジトリを入れておく(入っていないリポジトリには、
誰のボードからでもサーバーのトークンを使わせない)。

## 同期する(編集者)

「履歴」パネルの「GitHub」の欄から。

| 操作 | 内容 |
| --- | --- |
| 同期 | 「最後に同期した時点」「ボードの今」「リポジトリの今」を要素・項目単位で 3 方向マージし、ボードに取り込んでからコミットする |
| プルリクエストにする | リポジトリの変更をボードに取り込んだうえで、ボードの今を新しいブランチ(`graboard/{ボードID}-{時刻}`)にコミットしてプルリクエストを出す |
| 変更を確かめる | ボードとリポジトリのどちらが変わったかを見る |
| 上書きする | リポジトリの内容をボードに読み込む / ボードの内容をリポジトリに書く(マージしない) |

- 片方だけが変えた要素・項目はその変更を取る。同じ要素の別の項目(片方は位置、もう片方は文字)なら両方残る。
- 両方が同じ項目を違う値にした、片方が消してもう片方が変えたときは、ボード側(変えた側)を残して
  「衝突」として知らせる。
- マージで消えた要素につながっていたコネクタは、つなぎ先を点にする。
- ボードに取り込む前の状態は「GitHub(owner/name)から取り込む前」として版の履歴に残るので、いつでも戻せる。
- コミットは 1 回にまとまり(Git Data API)、リポジトリにない画像だけを送る。コミットメッセージを
  入れなければ「追加・変更・削除」の数から作る。
- 同期の途中で GitHub 側のブランチが進んでいたら、上書きせずに「もう一度同期してください」と返す。

## GitHub の push を自動で取り込む

1. サーバーに `GITHUB_WEBHOOK_SECRET` を設定する。
2. リポジトリの Settings → Webhooks で、Payload URL を `https://<サーバー>/api/github/webhook`、
   Content type を `application/json`、Secret を同じ値にして、`push` イベントを送る。
3. ボードの設定で「GitHub への push を自動で取り込む」をオンにする。

push で対象のファイルが変わると、同期と同じ 3 方向マージでボードに取り込む(コミットはしない。
ボード側だけの変更は次の同期でコミットされる)。

## API

| メソッド | パス | 内容 |
| --- | --- | --- |
| `GET` | `/api/boards/{id}/github?key=` | つながり(`GithubLink` か `null`。トークンは返さない、編集者以上) |
| `PUT` | `/api/boards/{id}/github` | つなぐ(`{"repo", "branch"?, "path"?, "token"?, "auto_pull"}`、オーナーのみ) |
| `DELETE` | `/api/boards/{id}/github` | 外す(リポジトリのファイルはそのまま) |
| `GET` | `/api/boards/{id}/github/status?key=` | `{"remote_exists", "board_changed", "repo_changed", "link"}` |
| `POST` | `/api/boards/{id}/github/sync?key=` | `{"mode": "sync"\|"push"\|"pull", "message"?, "pull_request"?}` → 取り込んだ・コミットした数、衝突、コミット・プルリクエストの URL |
| `POST` | `/api/github/webhook` | GitHub の Webhook(`X-Hub-Signature-256` の署名を確かめる) |

つなぐ・外す・同期は操作ログ(`board.github.link` / `board.github.unlink` / `board.github.sync`)に残る。
