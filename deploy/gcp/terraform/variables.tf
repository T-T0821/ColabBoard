variable "project_id" {
  description = "デプロイ先の GCP プロジェクト ID"
  type        = string
}

variable "region" {
  description = "リージョン(Artifact Registry・固定 IP・サブネット)"
  type        = string
  default     = "asia-northeast1"
}

variable "zone" {
  description = "VM とデータディスクを置くゾーン"
  type        = string
  default     = "asia-northeast1-b"
}

variable "name" {
  description = "作るリソースの名前の頭に付ける文字列"
  type        = string
  default     = "graboard"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,20}$", var.name))
    error_message = "name は英小文字で始まる 2〜21 文字の英小文字・数字・ハイフンにしてください。"
  }
}

variable "domain" {
  description = "公開するドメイン(例: board.example.com)。空なら固定 IP から <ip>.sslip.io を使う(DNS 設定なしで HTTPS になる)"
  type        = string
  default     = ""
}

variable "acme_email" {
  description = "Let's Encrypt の証明書期限切れ通知を受け取るメールアドレス(空でも動く)"
  type        = string
  default     = ""
}

variable "machine_type" {
  description = "VM のマシンタイプ。ボードはメモリ上に載るので、大きなボードを多く使うなら上げる"
  type        = string
  default     = "e2-medium"
}

variable "data_disk_size_gb" {
  description = "データディスク(boards.redb と証明書)の大きさ"
  type        = number
  default     = 20
}

variable "data_disk_type" {
  description = "データディスクの種類"
  type        = string
  default     = "pd-balanced"
}

variable "snapshot_retention_days" {
  description = "データディスクの毎日のスナップショットを残す日数(0 でスナップショットを取らない)"
  type        = number
  default     = 14
}

variable "image_tag" {
  description = "起動するイメージのタグ(Artifact Registry の graboard イメージ)"
  type        = string
  default     = "latest"
}

variable "env" {
  description = "サーバーに渡す環境変数(秘密でないもの)。COOKIE_SECURE / TRUST_PROXY / PUBLIC_URL / DATA_DIR は自動で入る。LOG_FORMAT は既定で gcp"
  type        = map(string)
  default = {
    RUST_LOG = "info"
  }
}

variable "secret_env_names" {
  description = "Secret Manager に置いて環境変数として渡すもの。Terraform は入れ物だけ作り、値は gcloud で入れる(状態ファイルに残さない)"
  type        = list(string)
  default     = ["ANTHROPIC_API_KEY"]

  validation {
    condition     = alltrue([for n in var.secret_env_names : can(regex("^[A-Z][A-Z0-9_]*$", n))])
    error_message = "secret_env_names は環境変数名(英大文字・数字・_)にしてください。"
  }
}

variable "allowed_source_ranges" {
  description = "グラボード(https)に入れるアクセス元の CIDR。既定は制限なし。例: [\"203.0.113.0/24\", \"198.51.100.10/32\"]"
  type        = list(string)
  default     = ["0.0.0.0/0"]

  validation {
    condition     = length(var.allowed_source_ranges) > 0 && alltrue([for r in var.allowed_source_ranges : can(cidrhost(r, 0)) && !strcontains(r, ":")])
    error_message = "allowed_source_ranges は 1 つ以上の IPv4 の CIDR(例: 203.0.113.10/32)にしてください。"
  }
}

variable "allow_uptime_check_ips" {
  description = "allowed_source_ranges で制限するとき、外形監視(Uptime check)の送信元 IP も通す"
  type        = bool
  default     = true
}

variable "ssh_source_ranges" {
  description = "SSH(22 番)を許す送信元。既定は IAP 経由だけ(gcloud compute ssh --tunnel-through-iap)"
  type        = list(string)
  default     = ["35.235.240.0/20"]
}

variable "alert_email" {
  description = "外形監視(https://…/healthz)が落ちたときに通知するメールアドレス。空なら監視だけ作り通知しない"
  type        = string
  default     = ""
}

variable "deletion_protection" {
  description = "VM を terraform destroy や誤操作で消せないようにする(データディスクは常に prevent_destroy で守る)"
  type        = bool
  default     = true
}

variable "caddy_image" {
  description = "前段の HTTPS リバースプロキシ(Caddy)のイメージ"
  type        = string
  default     = "caddy:2.10"
}

variable "gcs_backup" {
  description = "サーバーの定期バックアップ(BACKUP_LOCATION)を GCS のバケットに置く。有効にしたら HMAC キーを Secret Manager に入れる(README の「GCS へのバックアップ」)"
  type        = bool
  default     = false
}
