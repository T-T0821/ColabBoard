# セルフホスト手順

グラボードは、サーバー 1 つ(Rust 製の単一バイナリ)と保存ファイル 1 つ(`boards.redb`)だけで動きます。
外部のデータベースやクラウドサービスはいりません。社内ネットワークや自分のサーバーに置けば、ボードの中身が外に出ません。

方法は 3 つあります。

| 方法 | 向いている場面 |
| --- | --- |
| [Docker / Docker Compose](#1-docker--docker-compose) | いちばん手軽。サーバーに Docker があるとき |
| [単一バイナリ](#2-単一バイナリ) | Docker を使わないとき。ファイル 1 つをコピーして動かす |
| [ソースから](#3-ソースからビルド済みクライアントを別に置く) | 開発中、またはクライアントを自分で差し替えるとき |

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
ハッシュ付きのファイル(`*.wasm` `*.js` `*.css`)には長期キャッシュ、`index.html` と `sw.js` には `no-cache` を付けます。

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

インターネットや社内に公開するときは、前段に HTTPS のリバースプロキシを置き、`COOKIE_SECURE=1` にします。
WebSocket(`/ws/…`)も通す必要があります。Service Worker(オフラインで開く機能)も HTTPS か `localhost` でしか動きません。

**Caddy**(証明書を自動で取る):

```
board.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

**nginx**:

```nginx
server {
    listen 443 ssl;
    server_name board.example.com;
    # ssl_certificate ...;

    client_max_body_size 64m;   # 画像・PDF の貼り付け用

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 1h;   # WebSocket を切らない
    }
}
```

## 環境変数

| 環境変数 | 既定値 | 内容 |
| --- | --- | --- |
| `PORT` | `8080` | 待ち受けポート |
| `DATA_DIR` | `data`(Docker では `/data`) | 保存先(`boards.redb`)。空にすると保存しない |
| `STATIC_DIR` | `crates/board-web/dist` | 配信するクライアント。埋め込み版を使うなら指定しない |
| `COOKIE_SECURE` | `0` | HTTPS で公開するときは `1` |
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
