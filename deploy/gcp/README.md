# GCP にホスティングする(Terraform)

グラボードを Google Cloud に置くための Terraform 一式です。`terraform apply` と `deploy.sh` の 2 ステップで、
HTTPS(証明書は自動)・WebSocket・永続化・毎日のバックアップ・死活監視までそろいます。

```
                 ┌──────────── Compute Engine VM(Container-Optimized OS)────────────┐
利用者 ─HTTPS──▶ │ Caddy :443 ──(WebSocket もそのまま)──▶ graboard :8080            │
  固定 IP        │  証明書を自動取得・更新              boards.redb ─┐               │
                 └───────────────────────────────────────────────────┼───────────────┘
                                                                     ▼
                                     永続ディスク(毎日スナップショット、14 日保存)

Artifact Registry ← Cloud Build(deploy.sh build)   Secret Manager(ANTHROPIC_API_KEY など)
Cloud Monitoring の外形監視(/healthz)と、任意でメール通知
```

## 作られるもの

| リソース | 役割 |
| --- | --- |
| Compute Engine VM 1 台(既定 `e2-medium`、COS) | グラボードと Caddy の 2 コンテナを systemd で動かす。OS は自動更新 |
| 永続ディスク(既定 20 GB `pd-balanced`) | `boards.redb` と Caddy の証明書。VM を作り直しても残る(`prevent_destroy`) |
| スナップショットのスケジュール | 毎日 03:00 JST、既定 14 日保存 |
| 固定の外部 IP・VPC・ファイアウォール | 80 / 443 だけ公開。SSH は IAP 経由だけ。8080 番は外に開けない |
| Artifact Registry | イメージの置き場(古いものは自動で掃除) |
| Cloud Build 用のサービスアカウントとソース置き場 | `deploy.sh build` がイメージを作る |
| Secret Manager | API キーなどの入れ物。値は Terraform の外で入れる(状態ファイルに残さない) |
| Uptime check(+ 任意でメール通知) | `https://…/healthz` を 1 分ごとに確かめる |

公開時に必要な `COOKIE_SECURE=1`・`TRUST_PROXY=1`・`PUBLIC_URL` は自動で設定します([docs/self-hosting.md](../../docs/self-hosting.md#https-とリバースプロキシ))。

### なぜ Cloud Run ではなく VM か

グラボードの保存先は redb(1 プロセスが 1 ファイルを持つ組み込み DB)で、同時編集のルーム・回数制限もプロセスのメモリ上にあります。
そのため **必ず 1 台で動かし、ローカルのブロックデバイスにファイルを置く** 必要があります。

- Cloud Run はインスタンスが増えたり入れ替わったりする(デプロイ中は新旧 2 つが同時に動く)。同じ redb を 2 つのプロセスが開けない
- Cloud Run で使えるボリュームは Cloud Storage FUSE か NFS(Filestore)で、どちらも redb のようなファイルロック + ランダム書き込みに向かない
- Cloud SQL に移すにはサーバー側の保存処理を作り直す必要がある

VM + 永続ディスクなら、セルフホスト手順と同じ構成のまま、安く・確実に動きます。
また、GCP のロードバランサは `X-Forwarded-For` の最後にロードバランサ自身の IP を付けるため、利用者の IP で回数制限をかける
グラボードとは相性が悪く、VM 上の Caddy で TLS を終端しています(ロードバランサ分の費用もかかりません)。

### 費用の目安(東京リージョン、概算)

| 項目 | 月額 |
| --- | --- |
| VM `e2-medium` | 約 $33(`e2-small` なら約 $16) |
| ディスク(起動 10 GB + データ 20 GB) | 約 $4 |
| 外部 IP(VM に付いている間) | 約 $4 |
| スナップショット | 差分のみ。小さなボードなら数十セント |
| Cloud Build(`E2_HIGHCPU_8`、1 回 15 分ほど) | 1 回あたり約 $0.25 |

合計で **月 40 ドル前後** です。正確な額は [料金計算ツール](https://cloud.google.com/products/calculator) で確かめてください。

## 前提

- 課金が有効な GCP プロジェクト(オーナー権限、または同等のロール)
- [gcloud CLI](https://cloud.google.com/sdk/docs/install) と [Terraform](https://developer.hashicorp.com/terraform/install) 1.5 以上
- 独自ドメインを使うなら、その DNS を編集できること(なくても `<IP>.sslip.io` で HTTPS になります)

## 手順

### 1. ログイン

```sh
gcloud auth login
gcloud auth application-default login   # Terraform が使う
gcloud config set project my-gcp-project
```

### 2. 設定を書く

```sh
cd deploy/gcp/terraform
cp terraform.tfvars.example terraform.tfvars
$EDITOR terraform.tfvars      # project_id は必須。domain・env・alert_email はお好みで
```

### 3. 基盤を作る

```sh
terraform init
terraform apply
```

初回は API の有効化を含めて 3〜5 分ほどです。VM はこの時点で起動しますが、イメージがまだないのでグラボードは動いていません(次の手順で入ります)。

独自ドメインを使う場合は、ここで出力された `ip_address` にドメインの **A レコード** を向けてください。
DNS が届くまで、Caddy は証明書の取得をやり直し続けます(数分〜)。

### 4. 秘密の値を入れる(AI 機能などを使うとき)

```sh
printf '%s' 'sk-ant-...' | gcloud secrets versions add graboard-ANTHROPIC_API_KEY --data-file=-
```

名前は `terraform output secret_ids` で確かめられます。値のないものは渡さないので、使わない機能は空のままで構いません。
`secret_env_names` に `OIDC_CLIENT_SECRET` などを足せば、ほかの秘密も同じように渡せます。

### 5. イメージを作って起動する

```sh
cd ../../..          # リポジトリのルートへ
deploy/gcp/deploy.sh
```

Cloud Build でイメージを作り(Rust のビルドで 10〜20 分)、VM 上のグラボードを起動し直し、`/healthz` を確かめます。
URL は `terraform -chdir=deploy/gcp/terraform output url` です。

## 運用

| やりたいこと | 方法 |
| --- | --- |
| 新しい版に更新する | `git pull && deploy/gcp/deploy.sh`(停止は数秒。クライアントはつなぎ直して自動で同期) |
| 環境変数・マシンタイプを変える | `terraform.tfvars` を直して `terraform apply` → `deploy/gcp/deploy.sh reboot` |
| 秘密の値を変える | `gcloud secrets versions add …` → `deploy/gcp/deploy.sh restart` |
| ログを見る | `deploy/gcp/deploy.sh logs`、または Cloud Logging(コンテナのログが送られます) |
| VM に入る | `gcloud compute ssh graboard --zone asia-northeast1-b --tunnel-through-iap` |
| 特定の版に戻す | `terraform.tfvars` に `image_tag = "<git の短い SHA>"` → `terraform apply` → `deploy.sh reboot` |

`deploy.sh restart` は VM 上のグラボードだけを再起動し、`reboot` は VM を止めて起動します(起動時の設定を読み直す)。
どちらも SIGTERM で止めるので、未保存の変更は書き出されます。

### バックアップから戻す

1. コンソールの「Compute Engine → スナップショット」か `gcloud compute snapshots list` で戻したい時点を選ぶ
2. スナップショットから新しいディスクを作る: `gcloud compute disks create graboard-data-restored --source-snapshot=<名前> --zone asia-northeast1-b`
3. VM を止め、`graboard-data` を外して新しいディスクを `--device-name=data` で付け、起動する
   (Terraform で管理し続けるなら、`terraform state rm` と `terraform import` でディスクを差し替える)

ファイル単位で取り出したいときは、VM に入って `/mnt/disks/data/graboard/boards.redb` をコピーします
(`sudo systemctl stop graboard` してからコピーし、`start` で戻す)。

### 状態ファイルの置き場所

既定では `terraform.tfstate` が手元に作られます。複数人や CI で扱うなら GCS に置いてください。

```sh
gcloud storage buckets create gs://my-gcp-project-tfstate --location=asia-northeast1 --uniform-bucket-level-access
gcloud storage buckets update gs://my-gcp-project-tfstate --versioning
```

`versions.tf` の `backend "gcs"` のコメントを外して `terraform init -migrate-state` します。
状態ファイルに秘密の値は入りません(Secret Manager の値は Terraform の外で入れるため)。

### 片付け

データディスクは `prevent_destroy` で守っています。すべて消すときは:

1. 必要ならバックアップを取る
2. `terraform.tfvars` に `deletion_protection = false` を書いて `terraform apply`
3. `compute.tf` の `prevent_destroy = true` を外す
4. `terraform destroy`

スナップショットは元のディスクを消しても残ります(`KEEP_AUTO_SNAPSHOTS`)。要らなければ別に消してください。

## 変数

主なものだけ。すべては [`terraform/variables.tf`](terraform/variables.tf) にあります。

| 変数 | 既定値 | 内容 |
| --- | --- | --- |
| `project_id` | (必須) | GCP プロジェクト ID |
| `region` / `zone` | `asia-northeast1` / `asia-northeast1-b` | 置く場所 |
| `domain` | `""` | 公開するドメイン。空なら `<IP>.sslip.io` |
| `acme_email` | `""` | Let's Encrypt の通知先 |
| `machine_type` | `e2-medium` | VM の大きさ |
| `data_disk_size_gb` | `20` | データディスクの大きさ(後から増やせる) |
| `snapshot_retention_days` | `14` | スナップショットを残す日数。`0` で取らない |
| `env` | `{ RUST_LOG = "info" }` | サーバーの環境変数([一覧](../../docs/self-hosting.md#環境変数)) |
| `secret_env_names` | `["ANTHROPIC_API_KEY"]` | Secret Manager から渡す環境変数 |
| `image_tag` | `latest` | 起動するイメージのタグ |
| `alert_email` | `""` | `/healthz` が 5 分以上失敗したら通知するメールアドレス |
| `deletion_protection` | `true` | VM の削除保護 |

## うまくいかないとき

- **https で開けない**: `deploy.sh logs` で Caddy のログを見る。独自ドメインなら A レコードが `ip_address` を向いているか、80 番に届くか
  (Let's Encrypt の HTTP-01 チャレンジに使う)を確かめる
- **502 になる**: グラボードがまだ起動していない。初回は `deploy.sh build` が終わるまでイメージがない
- **AI 機能が使えない**: `deploy.sh logs` に `secret ANTHROPIC_API_KEY: loaded` が出ているか。値を入れたあとは `deploy.sh restart`
- **Cloud Build が権限エラーになる**: `terraform apply` を最後まで通したか。組織のポリシーで外部 IP やサービスアカウントの作成が禁止されていないか
