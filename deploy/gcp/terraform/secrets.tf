# 秘密の値(API キーなど)の入れ物。値は Terraform の外で入れる:
#
#   printf '%s' "sk-ant-..." | gcloud secrets versions add graboard-ANTHROPIC_API_KEY --data-file=-
#
# VM は起動のたびに最新の版を読み、環境変数としてコンテナに渡す(値の入っていないものは渡さない)。
resource "google_secret_manager_secret" "env" {
  for_each = toset(local.secret_env_names)

  secret_id = "${var.name}-${each.value}"
  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_iam_member" "vm_access" {
  for_each = google_secret_manager_secret.env

  secret_id = each.value.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.vm.email}"
}
