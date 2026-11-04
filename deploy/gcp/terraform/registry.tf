resource "google_artifact_registry_repository" "graboard" {
  location      = var.region
  repository_id = var.name
  format        = "DOCKER"
  description   = "グラボードのコンテナイメージ"

  # 古いイメージを溜めない: 新しい 10 個とタグ latest は残し、30 日より前のタグなしは消す。
  cleanup_policies {
    id     = "keep-recent"
    action = "KEEP"
    most_recent_versions {
      keep_count = 10
    }
  }
  cleanup_policies {
    id     = "keep-latest"
    action = "KEEP"
    condition {
      tag_state    = "TAGGED"
      tag_prefixes = ["latest"]
    }
  }
  cleanup_policies {
    id     = "delete-old-untagged"
    action = "DELETE"
    condition {
      tag_state  = "UNTAGGED"
      older_than = "2592000s"
    }
  }

  depends_on = [google_project_service.apis]
}

# イメージをビルドして push する専用のサービスアカウント(Cloud Build が使う)。
resource "google_service_account" "build" {
  account_id   = "${var.name}-build"
  display_name = "グラボードのイメージビルド(Cloud Build)"

  depends_on = [google_project_service.apis]
}

resource "google_artifact_registry_repository_iam_member" "build_writer" {
  location   = google_artifact_registry_repository.graboard.location
  repository = google_artifact_registry_repository.graboard.name
  role       = "roles/artifactregistry.writer"
  member     = "serviceAccount:${google_service_account.build.email}"
}

resource "google_project_iam_member" "build_logs" {
  project = var.project_id
  role    = "roles/logging.logWriter"
  member  = "serviceAccount:${google_service_account.build.email}"
}

# gcloud builds submit がソースを上げる置き場。7 日で消す。
resource "google_storage_bucket" "build_source" {
  name                        = "${var.project_id}-${var.name}-build"
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  force_destroy               = true

  lifecycle_rule {
    condition {
      age = 7
    }
    action {
      type = "Delete"
    }
  }

  depends_on = [google_project_service.apis]
}

resource "google_storage_bucket_iam_member" "build_source_reader" {
  bucket = google_storage_bucket.build_source.name
  role   = "roles/storage.objectViewer"
  member = "serviceAccount:${google_service_account.build.email}"
}
