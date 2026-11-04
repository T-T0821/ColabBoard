# セルフホスト手順

グラボードは、サーバー 1 つ(Rust 製の単一バイナリ)と保存ファイル 1 つ(`boards.redb`)だけで動きます。
外部のデータベースやクラウドサービスはいりません。社内ネットワークや自分のサーバーに置けば、ボードの中身が外に出ません。

方法は 3 つあります。

| 方法 | 向いている場面 |
| --- | --- |
| [Docker / Docker Compose](#1-docker--docker-compose) | いちばん手軽。サーバーに Docker があるとき |
| [単一バイナリ](#2-単一バイナリ) | Docker を使わないとき。ファイル 1 つをコピーして動かす |
| [ソースから](#3-ソースからビルド済みクライアントを別に置く) | 開発中、またはクライアントを自分で差し替えるとき |

Google Cloud に置く場合は、Terraform で VM・HTTPS・バックアップ・監視までまとめて作れます([deploy/gcp](../deploy/gcp/README.md))。

## 1. Docker / Docker Compose

```sh
git clone https://github.com/T-T0821/ColabBoard.git && cd ColabBoard
docker compose up -d          # → http://localhost:8080
```

`compose.yaml` は、`colabboard-data` ボリュームの `/data` にボードとアカウントを保存します。`docker compose down` してもデータは残ります
(消すときは `docker compose down -v`)。

Compose を使わない場合:

```sh
docker build -t colabboard .
docker run -d --name colabboard -p 8080:8080 -v colabboard-data:/data --restart unless-stopped colabboard
```

- イメージは distroless(シェルなし)で、uid 65532 の一般ユーザーとして動きます
- `HEALTHCHECK` は `board-server healthcheck`(自分の `/healthz` を確かめる)。`docker ps` で状態が見えます
- 環境変数は `-e` か `compose.yaml` の `environment:` で渡します([一覧](#環境変数))
- 初回のビルドは Rust のコンパイルで 10 分ほどかかります。2 回目からはキャッシュが効きます

## 2. 単一バイナリ

`embed` 機能付きでビルドすると、Web クライアント(HTML・WASM・フォントなど)がサーバーのバイナリに埋め込まれます。
できたファイル 1 つをサーバーにコピーするだけで動きます。

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
(cd crates/board-web && trunk build --release)          # 先にクライアントを作る
cargo build -p board-server --release --features embed  # → target/release/board-server

# サーバーで
DATA_DIR=/var/lib/graboard ./board-server
```

`STATIC_DIR` を指定しない、かつ既定の `crates/board-web/dist` もない場合に、埋め込んだクライアントを配ります。
ハッシュ付きのファイル(`*.wasm` `*.js` `*.css`)には長期キャッシュ、それ以外(`index.html` `sw.js` など)には `no-cache` を付けます。
`trunk build --release` は brotli / gzip で圧縮したファイル(`.br` `.gz`)も作るので、ブラウザが受け取れる形式で返します(wasm は約 1/4)。
これは `STATIC_DIR` で配る場合も同じです。リバースプロキシ側で圧縮する必要はありません。

### systemd で常駐させる

```ini
# /etc/systemd/system/graboard.service
[Unit]
Description=Graboard
After=network.target

[Service]
ExecStart=/usr/local/bin/board-server
Environment=PORT=8080
Environment=DATA_DIR=/var/lib/graboard
Environment=COOKIE_SECURE=1
DynamicUser=yes
StateDirectory=graboard
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now graboard
```

停止(SIGTERM)時には未保存の変更を書き出してから終わります。

## 3. ソースから(ビルド済みクライアントを別に置く)

```sh
(cd crates/board-web && trunk build --release)
cargo build -p board-server --release
DATA_DIR=/var/lib/graboard STATIC_DIR=/srv/graboard/dist ./target/release/board-server
```

## HTTPS とリバースプロキシ

インターネットや社内に公開するときは、前段に HTTPS のリバースプロキシを置きます。
グラボード自身は HTTP だけを話し、証明書の取得・更新・TLS はプロキシに任せます(Caddy なら自動)。
サーバーは `127.0.0.1` か内部ネットワークだけで待ち受け、外から 8080 番に直接つながらないようにしてください。

プロキシの後ろでは、次の 3 つを設定します。

| 環境変数 | 値 | 理由 |
| --- | --- | --- |
| `COOKIE_SECURE` | `1` | ログイン Cookie を HTTPS だけで送る。`Strict-Transport-Security` も付く |
| `TRUST_PROXY` | `1` | プロキシが付ける `X-Forwarded-For` から利用者の IP を取る。これがないと、全員がプロキシの IP に見えて回数制限が全員まとめてかかる |
| `PUBLIC_URL` | `https://board.example.com/` | Slack 通知などのリンク |

`TRUST_PROXY=1` は、プロキシを通らずに直接つながれない構成のときだけにしてください(直接つながれると IP を偽れます)。
WebSocket(`/ws/…`)も通す必要があります。Service Worker(オフラインで開く機能)も HTTPS か `localhost` でしか動きません。

**Caddy**(証明書を自動で取る。WebSocket と `X-Forwarded-For` は何もしなくても通る):

```
board.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

**nginx**:

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name board.example.com;
    ssl_certificate     /etc/letsencrypt/live/board.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/board.example.com/privkey.pem;

    client_max_body_size 64m;   # 画像・PDF の貼り付け用

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;   # 利用者の IP(TRUST_PROXY=1 で使う)
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 1h;   # WebSocket を切らない
    }
}

server {   # http は https へ
    listen 80;
    server_name board.example.com;
    return 301 https://$host$request_uri;
}
```

証明書は `certbot --nginx -d board.example.com` などで取ります。

**Docker Compose で Caddy と並べる例**:

```yaml
services:
  graboard:
    build: .
    environment:
      COOKIE_SECURE: "1"
      TRUST_PROXY: "1"
      PUBLIC_URL: https://board.example.com/
    volumes: [colabboard-data:/data]
    restart: unless-stopped
    # ports は開けない(Caddy からだけつながる)
  caddy:
    image: caddy:2
    command: caddy reverse-proxy --from board.example.com --to graboard:8080
    ports: ["80:80", "443:443"]
    volumes: [caddy-data:/data]
    restart: unless-stopped
volumes:
  colabboard-data:
  caddy-data:
```

## 公開前の守り

サーバーには次の守りが組み込まれています。上限は環境変数で変えられます(`0` で制限なし)。数はメモリ上で数えるので、再起動すると数え直します。

| 守り | 既定 | 環境変数 |
| --- | --- | --- |
| ログイン失敗の回数制限(アカウントごと、15 分) | 10 回 | `LOGIN_FAILURES_PER_ACCOUNT` |
| ログイン失敗の回数制限(IP ごと、15 分) | 30 回 | `LOGIN_FAILURES_PER_IP` |
| アカウント作成の回数制限(IP ごと、1 時間) | 20 回 | `SIGNUPS_PER_IP` |
| アカウント数の上限(パスワードでの作成。SSO は IdP 側で絞る) | なし | `MAX_USERS` |
| 1 人が持てるボード数 | 500 | `MAX_BOARDS_PER_USER` |
| API・WebSocket 接続の要求(IP ごと、1 分) | 600 回 | `REQUESTS_PER_MINUTE` |
| 同時につなげる WebSocket(IP ごと) | 50 本 | `CONNECTIONS_PER_IP` |
| 1 つのボードに同時につなげる人数 | 200 人 | `PEERS_PER_BOARD` |
| WebSocket 1 本のメッセージ数(1 秒、短時間はこの 5 倍まで) | 300 通 | `MESSAGES_PER_SECOND` |
| パスワードのハッシュ計算を同時に行う数 | 4 件 | `PASSWORD_HASH_SLOTS` |

上限に達すると、ログインやアカウント作成は「〇分ほど待ってからやり直してください」と表示します(HTTP 429 と `Retry-After`)。
メッセージを送りすぎる WebSocket は切断します(クライアントはつなぎ直して全体を同期し直すので、編集は失われません)。

ほかに、次のことをしています。

- **パスワード**: Argon2id でハッシュ化して保存。存在しないログイン名でも同じだけ時間をかけ、名前の有無を推測させない
- **ログイン Cookie**: `HttpOnly; SameSite=Lax`。`COOKIE_SECURE=1` で `Secure`
- **ほかのサイトからの WebSocket**: `Origin` が自分のホストと違う接続では、ログイン Cookie を使わない(共有リンクの鍵や、誰でも開けるボードは使える)。
  クライアントを別のオリジンから配るときは `ALLOWED_ORIGINS=https://app.example.com` のように許可する
- **セキュリティヘッダー**: すべての応答に `X-Content-Type-Options: nosniff`、`X-Frame-Options: SAMEORIGIN`(埋め込み表示 `?embed=1` は `EMBED_ALLOWED_ORIGINS` のサイトから埋め込める)、`Referrer-Policy: same-origin`
  (共有リンクの鍵をほかのサイトへ送らない)、`Cross-Origin-Opener-Policy: same-origin`。`COOKIE_SECURE=1` なら HSTS(`HSTS=0` で止める)
- **Content-Security-Policy**: HTML には CSP を付け、スクリプトは自分のファイルと、`index.html` のインラインスクリプト(ハッシュで指定)だけ動かす。
  `index.html` を書き換えても、ハッシュは配信時に計算し直す。外の画像(マークダウンの取り込み)は https なら読める。困ったときは `CSP=0` で止められる
- **入力の大きさ**: API の JSON は 2 MB まで、WebSocket の 1 通は `MAX_MESSAGE_MB`(既定 128 MB)まで

### 公開前のチェックリスト

- [ ] HTTPS のリバースプロキシを置き、`COOKIE_SECURE=1` と `TRUST_PROXY=1` を設定した
- [ ] サーバーの 8080 番は外から直接つながらない
- [ ] 知らない人に使わせないなら `ANONYMOUS_BOARDS=0`(URL を知っていれば誰でも編集できるボードを作らせない)と `MAX_USERS` を設定した
- [ ] `DATA_DIR` を定期的にバックアップしている(下の「バックアップと更新」)
- [ ] AI 機能を使うなら、`ANTHROPIC_API_KEY` をサーバーの環境変数だけに置いた(ブラウザには渡らない)
- [ ] 大勢で使うなら、プロキシ側でも接続数・帯域の制限をかけた(サーバーを複数台にする場合は特に)

## 環境変数

| 環境変数 | 既定値 | 内容 |
| --- | --- | --- |
| `PORT` | `8080` | 待ち受けポート |
| `DATA_DIR` | `data`(Docker では `/data`) | 保存先(`boards.redb`)。空にすると保存しない |
| `STATIC_DIR` | `crates/board-web/dist` | 配信するクライアント。埋め込み版を使うなら指定しない |
| `COOKIE_SECURE` | `0` | HTTPS で公開するときは `1` |
| `TRUST_PROXY` | `0` | リバースプロキシの後ろに置くときは `1`(`X-Forwarded-For` から利用者の IP を取る) |
| `ALLOWED_ORIGINS` | なし | ログイン付きの WebSocket を受け付けるほかのオリジン(カンマ区切り) |
| `CSP` / `HSTS` | `1` / `COOKIE_SECURE` と同じ | `0` でそのヘッダーを付けない |
| 回数制限・上限 | [公開前の守り](#公開前の守り) | `LOGIN_FAILURES_PER_ACCOUNT` `MAX_USERS` `MAX_BOARDS_PER_USER` など |
| `ANONYMOUS_BOARDS` | `1` | `0` でログインなしのボード(URL を知っていれば誰でも編集できるもの)を禁止 |
| `MAX_MESSAGE_MB` | `128` | WebSocket で受け取る 1 通の上限。初回同期でボード全体を送るので、数十万要素のボードを使うなら増やす(10 万要素で約 28 MB) |
| `ANTHROPIC_API_KEY` | なし | 設定すると AI 機能が使える |
| `ANTHROPIC_MODEL` | `claude-opus-5-5` | AI 機能で使うモデル |
| `RUST_LOG` | `info` | ログの詳しさ |

## バックアップと更新

- データは `DATA_DIR/boards.redb` の 1 ファイルです。サーバーを止めてからコピーすれば、そのままバックアップになります
  (Docker なら `docker compose stop && docker run --rm -v colabboard-data:/data -v $PWD:/backup busybox cp /data/boards.redb /backup/`)
- 更新は、新しいバイナリ(またはイメージ)に差し替えて再起動するだけです。保存形式は互換を保ちます
- 各ブラウザも自分のボードを IndexedDB に保存しているので、サーバーが止まっている間の編集は再起動後に自動でマージされます

## 動作確認

```sh
curl -fsS http://localhost:8080/healthz      # → ok
./board-server healthcheck && echo ok        # 同じことを終了コードで(PORT を見る)
```
