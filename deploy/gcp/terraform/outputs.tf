output "url" {
  description = "グラボードの URL"
  value       = local.public_url
}

output "ip_address" {
  description = "固定 IP。独自ドメインを使うなら A レコードをここに向ける"
  value       = google_compute_address.graboard.address
}

output "project_id" {
  value = var.project_id
}

output "region" {
  value = var.region
}

output "zone" {
  value = var.zone
}

output "instance_name" {
  description = "VM の名前"
  value       = google_compute_instance.graboard.name
}

output "image" {
  description = "VM が起動するイメージ"
  value       = local.image
}

output "image_repository" {
  description = "イメージの置き場(タグなし)"
  value       = "${local.registry_host}/${var.project_id}/${google_artifact_registry_repository.graboard.repository_id}/graboard"
}

output "build_service_account" {
  description = "イメージのビルドに使うサービスアカウント(deploy.sh が使う)"
  value       = google_service_account.build.email
}

output "build_source_bucket" {
  description = "ビルドのソースを上げるバケット(deploy.sh が使う)"
  value       = google_storage_bucket.build_source.name
}

output "secret_ids" {
  description = "値を入れる Secret Manager の名前"
  value       = { for k, s in google_secret_manager_secret.env : k => s.secret_id }
}
