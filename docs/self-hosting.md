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
- [ ] 直接接続(P2P)を使わせないなら `P2P_ENABLED=0`(チームごとならチームの設定で止める)
- [ ] 知らない人に使わせないなら `ANONYMOUS_BOARDS=0`(URL を知っていれば誰でも編集できるボードを作らせない)と `MAX_USERS` を設定した
- [ ] バックアップを別の場所に置いている(`BACKUP_LOCATION`。下の「バックアップと復元」)
- [ ] `ADMIN_LOGINS` を設定し、エラーの通知先(`SENTRY_DSN` か `ERROR_SLACK_WEBHOOK`、GCP なら `LOG_FORMAT=gcp`)を決めた
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
| `P2P_ENABLED` | `1` | `0` でサーバーを通さない直接接続(P2P)を止める。直接接続では中身がサーバーを通らないので、操作ログに残るのは使った記録と通報だけ |
| `MAX_MESSAGE_MB` | `128` | WebSocket で受け取る 1 通の上限。初回同期でボード全体を送るので、数十万要素のボードを使うなら増やす(10 万要素で約 28 MB) |
| `ANTHROPIC_API_KEY` | なし | 設定すると AI 機能が使える |
| `ANTHROPIC_MODEL` | `claude-opus-5-5` | AI 機能で使うモデル |
| `GITHUB_TOKEN` / `GITHUB_SYNC_REPOS` | なし | ボードを GitHub で管理するときのサーバーのトークンと、それを使ってよいリポジトリ([docs/github.md](github.md)) |
| `GITHUB_WEBHOOK_SECRET` | なし | GitHub の push を自動で取り込む Webhook の署名の鍵 |
| `PUBLIC_URL` | なし | 公開している URL。Slack 通知・通知メールのリンクとメールアドレスの確認リンクに使う |
| `SMTP_URL` / `MAIL_FROM` | なし | そろうとメール通知が使える。下の「メール・プッシュ通知」を参照 |
| `PUSH_NOTIFICATIONS` | `1` | `0` でプッシュ通知を止める |
| `VAPID_PRIVATE_KEY` / `VAPID_SUBJECT` | 自動 | プッシュ通知の鍵と問い合わせ先 |
| `PUSH_ALLOWED_HOSTS` | 主要なプッシュサービス | プッシュ通知を送ってよいホスト |
| `DUE_REMIND_HOURS` | `24` | コメントの期限の何時間前に知らせるか |
| `NOTIFY_UTC_OFFSET` | `+09:00` | 通知メールに書く時刻のタイムゾーン |
| `RUST_LOG` | `info` | ログの詳しさ |
| `LOG_FORMAT` | `text` | `gcp` で Cloud Logging の構造化ログ、`json` で 1 行 1 件の JSON([エラー監視](#エラー監視)) |
| `SENTRY_DSN` / `SENTRY_ENVIRONMENT` | なし | エラーを Sentry(または互換のサービス)へ送る |
| `ERROR_SLACK_WEBHOOK` | なし | 新しい種類のエラーを Slack の Incoming Webhook に知らせる |
| `BACKUP_LOCATION` | `DATA_DIR/backups` | 定期バックアップの置き場所(`file:///…` か `s3://…`。[バックアップと復元](#バックアップと復元)) |
| `BACKUP_INTERVAL_HOURS` / `BACKUP_KEEP` | `24` / `14` | バックアップの間隔(`0` で定期を止める)と、残す数(定期・手動それぞれ) |
| `BACKUP_ACCESS_KEY_ID` / `BACKUP_SECRET_ACCESS_KEY` | なし | `s3://` の置き場所の認証情報(なければ `AWS_…`) |

## エラー監視

サーバーのエラーと、利用者のブラウザで起きたエラーを 1 か所に集めます。設定しなくても動き、
`ADMIN_LOGINS` のサーバー管理者はボード一覧の「運用」→「エラー」で一覧を見られます。

- **集めるもの**: サーバーのログの ERROR(保存の失敗など)とパニック(スタックトレース付き)、
  ブラウザの JavaScript のエラー・処理されなかった Promise の失敗・wasm のパニック(`POST /api/errors` に自動で送る)
- **まとめ方**: メッセージと場所が同じものは 1 つにまとめ、回数・最初と最後の時刻・最新の 1 件の詳細
  (スタック、ページ、ブラウザ、ログイン中の人、ボード、版)を残す。数字や ID の違いは同じものとみなす
- **解決済み**: 直したら「解決済みにする」。また起きると未解決に戻り、通知も飛ぶ
- ブラウザからの受け付けは、同じサイトのページからだけ、1 つの IP から 1 分に 30 件まで、1 件 16 KB まで。共有リンクの鍵は URL から外して送る

外への知らせ(どれも任意):

```sh
SENTRY_DSN=https://<公開鍵>@o123.ingest.sentry.io/456   # 起きたエラーを Sentry へ(ブラウザのものもサーバー経由で)。1 分に 60 件まで
SENTRY_ENVIRONMENT=production
ERROR_SLACK_WEBHOOK=https://hooks.slack.com/services/…  # 新しい種類のエラー(と再発)だけ。1 時間に 20 件まで
PUBLIC_URL=https://board.example.com/                     # Slack のメッセージに運用の画面へのリンクを付ける
```

**Google Cloud で動かすとき** は `LOG_FORMAT=gcp` にすると、ログが Cloud Logging の構造化ログ(`severity`・`sourceLocation` 付きの JSON)になり、
ERROR は Error Reporting の形(`@type: …ReportedErrorEvent`)で出るので、コンテナのログを送るだけで Error Reporting に載ります(通知は Error Reporting の設定で)。
Datadog・Loki などに送るなら `LOG_FORMAT=json` です。

## バックアップと復元

サーバーは全データ(アカウント・ボード・コメント・バージョン履歴・動画メモ・チーム・操作ログ・集めたエラー、
チームの保存場所に置いたボードの中身も)を 1 つのファイル(`.grabak.gz`)にまとめて、定期的に残します。

- **既定**: 24 時間ごと、定期と手動それぞれ 14 個まで、置き場所は `DATA_DIR/backups`。
  同じディスクなので、誤って消したボードは戻せますが、ディスクの故障には別の場所が必要です
- **別の場所に置く**(おすすめ): `BACKUP_LOCATION` に、保存場所(`STORAGE_LOCATIONS`)と同じ書き方で指定します

```sh
# Google Cloud Storage(バケットの「相互運用性」で HMAC キーを作る)
BACKUP_LOCATION='s3://my-graboard-backups/prod?endpoint=https://storage.googleapis.com&region=auto'
BACKUP_ACCESS_KEY_ID=GOOG1…  BACKUP_SECRET_ACCESS_KEY=…
# AWS S3 / Cloudflare R2 / MinIO など
BACKUP_LOCATION='s3://graboard-backups/prod?region=ap-northeast-1'
# 別ディスク・NAS
BACKUP_LOCATION=file:///mnt/backup/graboard
```

バケット側でもバージョニングや保持ポリシーを付けておくと、置き場所ごと消える事故にも備えられます。

**取る**: 定期のほか、運用の画面「バックアップ」→「今すぐバックアップ」、またはサーバーを止めて `board-server backup`。
一覧からファイルをダウンロードできます(サーバー管理者だけ)。失敗するとログに ERROR が出るので、エラー監視の通知にも載ります。

**ボードを 1 つ戻す**: 運用の画面で戻したい時点の「ボードを戻す…」を押し、ボードを選んで「戻す」。

- 今もあるボードは、中身をその時点に戻します(普通の編集として全員の画面に反映)。戻す前の状態はバージョン履歴に「バックアップ(…)から戻す前」として残ります
- 削除したボードは、コメント・バージョン履歴・動画メモ・メンバーごと作り直し、オーナーのボード一覧に戻ります(チームがもうなければ個人ボードとして)
- どちらも操作ログに「バックアップから復元」と残ります

**全体を戻す**(ディスクの故障・サーバーの引っ越しなど): サーバーを止めてから実行します。今のデータは `boards.redb.before-restore-<時刻>` として脇に残ります。

```sh
board-server restore 1793491200000123 --yes        # BACKUP_LOCATION にある ID で
board-server restore ./graboard-2026-11-01.grabak.gz --yes   # ダウンロードしたファイルで
# Docker なら(イメージの ENTRYPOINT が board-server なので、サブコマンドだけを渡す)
docker compose stop
docker compose run --rm colabboard restore <ID> --yes
docker compose start
```

チームの保存場所に置いていたボードの中身は、同じ `STORAGE_LOCATIONS` が設定されていればそこへ、なければこのサーバーに戻します。

## 更新
## メール・プッシュ通知

メンション・返信・コメントの期限は、画面のベルに加えてメールとプッシュ通知でも届けられます。
各自がボード一覧の「通知の設定」で、メールアドレスの登録・この端末でのプッシュ通知の有効化・受け取る種類を選びます。

**メール** は SMTP で送ります。`SMTP_URL` と `MAIL_FROM` をそろえ、確認リンクのために `PUBLIC_URL` も設定してください。

```sh
SMTP_URL=smtps://apikey:SECRET@smtp.example.com      # 465 番(TLS)
# SMTP_URL=smtp://user:pass@smtp.example.com:587?tls=required   # 587 番(STARTTLS)
MAIL_FROM="グラボード <noreply@example.com>"
PUBLIC_URL=https://board.example.com/
```

- 登録したアドレスには確認のメールを送り、リンク(24 時間有効)を開くまで通知は送りません(他人のアドレスに送りつけられないように)。送り直しは 1 分に 1 回まで
- 既定で送るのはメンションと期限。返信はメールだと多くなりがちなので、各自がオンにしたときだけ
- 送り主のドメインには SPF・DKIM を設定しておくと、迷惑メールに入りにくくなります

**プッシュ通知** は Web Push(VAPID)で、設定なしで使えます(鍵はサーバーが作って `DATA_DIR` に保存)。

- ブラウザの決まりで、HTTPS(か `localhost`)で開いたときだけ有効にできます
- iPhone・iPad(iOS 16.4 以降)は、Safari の共有メニューから「ホーム画面に追加」したアプリで開いたときに有効にできます
- サーバーが送るのは中身のない合図だけです。受け取った端末の Service Worker が `/api/notifications` から内容を取って表示するので、コメントの本文は Google・Apple などのプッシュサービスを通りません
- 複数台のサーバーで動かすときや鍵を固定したいときは、`VAPID_PRIVATE_KEY`(P-256 の秘密鍵 32 バイトを base64url にしたもの)を指定します。鍵を変えると、各自がプッシュ通知を有効にし直す必要があります
- 送り先は既知のプッシュサービス(`fcm.googleapis.com`・`*.push.services.mozilla.com`・`*.push.apple.com`・`*.notify.windows.com`)だけです。社内の別のサービスを使うなら `PUSH_ALLOWED_HOSTS` を設定します

**期限** はコメントのスレッドに付けられ、1 分ごとに見回って、期限の `DUE_REMIND_HOURS` 時間前と過ぎたときに、メンションされた人と書いた人へ知らせます(スレッドを解決すれば止まり、期限を変えると知らせ直します)。

## バックアップと更新

- 更新は、新しいバイナリ(またはイメージ)に差し替えて再起動するだけです。保存形式は互換を保ちます
- 心配なら、先に運用の画面で「今すぐバックアップ」を押しておきます。手で写すなら、サーバーを止めて `DATA_DIR/boards.redb` をコピーします
  (Docker なら `docker compose stop && docker run --rm -v colabboard-data:/data -v $PWD:/backup busybox cp /data/boards.redb /backup/`)
- 各ブラウザも自分のボードを IndexedDB に保存しているので、サーバーが止まっている間の編集は再起動後に自動でマージされます

## 動作確認

```sh
curl -fsS http://localhost:8080/healthz      # → ok
./board-server healthcheck && echo ok        # 同じことを終了コードで(PORT を見る)
```
