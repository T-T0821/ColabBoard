# グラボードを GCP に置く。
#
# 構成: Compute Engine の VM 1 台(Container-Optimized OS)で、グラボードのコンテナと
# Caddy(HTTPS の自動証明書 + WebSocket を通すリバースプロキシ)を動かす。
# データ(boards.redb)は別の永続ディスクに置き、毎日スナップショットを取る。
#
# Cloud Run を使わない理由は README の「なぜ Cloud Run ではなく VM か」を参照
# (redb は 1 プロセスが 1 ファイルを持つ組み込み DB で、ローカルのブロックデバイスが要る)。

locals {
  backup_env = var.gcs_backup ? {
    BACKUP_LOCATION = "s3://${var.project_id}-${var.name}-backup/${var.name}?endpoint=https://storage.googleapis.com&region=auto"
  } : {}

  # Secret Manager から渡すもの。GCS へのバックアップを使うなら HMAC キーも足す。
  secret_env_names = distinct(concat(
    var.secret_env_names,
    var.gcs_backup ? ["BACKUP_ACCESS_KEY_ID", "BACKUP_SECRET_ACCESS_KEY"] : [],
  ))

  apis = [
    "compute.googleapis.com",
    "artifactregistry.googleapis.com",
    "secretmanager.googleapis.com",
    "cloudbuild.googleapis.com",
    "iap.googleapis.com",
    "logging.googleapis.com",
    "monitoring.googleapis.com",
    "storage.googleapis.com",
  ]

  registry_host = "${var.region}-docker.pkg.dev"
  image         = "${local.registry_host}/${var.project_id}/${google_artifact_registry_repository.graboard.repository_id}/graboard:${var.image_tag}"

  # ドメインがなければ固定 IP から sslip.io の名前を作る(1.2.3.4 → 1-2-3-4.sslip.io)。
  domain     = var.domain != "" ? var.domain : "${replace(google_compute_address.graboard.address, ".", "-")}.sslip.io"
  public_url = "https://${local.domain}/"

  # LOG_FORMAT=gcp: ログを Cloud Logging の構造化ログにし、エラーを Error Reporting に載せる(var.env で上書きできる)。
  # 公開時に必須の設定は固定で入れる(docs/self-hosting.md の「HTTPS とリバースプロキシ」)。
  server_env = merge({ LOG_FORMAT = "gcp" }, local.backup_env, var.env, {
    COOKIE_SECURE = "1"
    TRUST_PROXY   = "1"
    PUBLIC_URL    = local.public_url
    DATA_DIR      = "/data"
  })
}

resource "google_project_service" "apis" {
  for_each = toset(local.apis)

  service            = each.value
  disable_on_destroy = false
}
