# サーバー自身の定期バックアップ(24 時間ごと、運用画面からボード単位で戻せる)の置き場所。
# ディスクのスナップショット(compute.tf)とは補い合う: スナップショットはディスクごと、
# こちらはボード単位で戻せ、VM やディスクとは別の場所に残る。
#
# サーバーは S3 互換 API で書くので、GCS の HMAC キーを使う。キーは Terraform では作らない
# (秘密が状態ファイルに残るため)。README の手順で gcloud で作り、Secret Manager に入れる。
resource "google_storage_bucket" "backup" {
  count = var.gcs_backup ? 1 : 0

  name                        = "${var.project_id}-${var.name}-backup"
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  # 世代の整理はサーバー(BACKUP_KEEP)が行う。消し忘れの保険として 180 日で消す。
  lifecycle_rule {
    condition {
      age = 180
    }
    action {
      type = "Delete"
    }
  }

  depends_on = [google_project_service.apis]
}

# HMAC キーを持たせるサービスアカウント。このバケットだけを読み書きできる。
resource "google_service_account" "backup" {
  count = var.gcs_backup ? 1 : 0

  account_id   = "${var.name}-backup"
  display_name = "グラボードのバックアップ(GCS の HMAC キー用)"

  depends_on = [google_project_service.apis]
}

resource "google_storage_bucket_iam_member" "backup_writer" {
  count = var.gcs_backup ? 1 : 0

  bucket = google_storage_bucket.backup[0].name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup[0].email}"
}
