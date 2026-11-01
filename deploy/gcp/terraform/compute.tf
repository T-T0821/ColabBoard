# VM が使うサービスアカウント。イメージの取得・秘密の読み出し・ログとメトリクスの送信だけを許す。
resource "google_service_account" "vm" {
  account_id   = "${var.name}-vm"
  display_name = "グラボードの VM"

  depends_on = [google_project_service.apis]
}

resource "google_artifact_registry_repository_iam_member" "vm_reader" {
  location   = google_artifact_registry_repository.graboard.location
  repository = google_artifact_registry_repository.graboard.name
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.vm.email}"
}

resource "google_project_iam_member" "vm" {
  for_each = toset([
    "roles/logging.logWriter",
    "roles/monitoring.metricWriter",
  ])

  project = var.project_id
  role    = each.value
  member  = "serviceAccount:${google_service_account.vm.email}"
}

# データ(boards.redb と Caddy の証明書)を置くディスク。VM を作り直しても残る。
resource "google_compute_disk" "data" {
  name = "${var.name}-data"
  type = var.data_disk_type
  zone = var.zone
  size = var.data_disk_size_gb

  lifecycle {
    # 消すとボードがすべて失われる。本当に消すときはこの行を外してから destroy する。
    prevent_destroy = true
  }

  depends_on = [google_project_service.apis]
}

# 毎日 03:00(JST)にスナップショットを取る。redb はクラッシュ安全なので、動かしたままで取ってよい。
resource "google_compute_resource_policy" "daily_snapshot" {
  count = var.snapshot_retention_days > 0 ? 1 : 0

  name   = "${var.name}-daily-snapshot"
  region = var.region

  snapshot_schedule_policy {
    schedule {
      daily_schedule {
        days_in_cycle = 1
        start_time    = "18:00" # UTC
      }
    }
    retention_policy {
      max_retention_days    = var.snapshot_retention_days
      on_source_disk_delete = "KEEP_AUTO_SNAPSHOTS"
    }
    snapshot_properties {
      storage_locations = [var.region]
      labels = {
        app = var.name
      }
    }
  }
}

resource "google_compute_disk_resource_policy_attachment" "daily_snapshot" {
  count = var.snapshot_retention_days > 0 ? 1 : 0

  name = google_compute_resource_policy.daily_snapshot[0].name
  disk = google_compute_disk.data.name
  zone = var.zone
}

resource "google_compute_instance" "graboard" {
  name         = var.name
  machine_type = var.machine_type
  zone         = var.zone
  tags         = [var.name]

  boot_disk {
    initialize_params {
      image = "cos-cloud/cos-stable"
      size  = 10
      type  = "pd-balanced"
    }
  }

  attached_disk {
    source      = google_compute_disk.data.id
    device_name = "data" # → /dev/disk/by-id/google-data
  }

  network_interface {
    subnetwork = google_compute_subnetwork.graboard.id
    access_config {
      nat_ip = google_compute_address.graboard.address
    }
  }

  service_account {
    email  = google_service_account.vm.email
    scopes = ["cloud-platform"]
  }

  shielded_instance_config {
    enable_secure_boot          = true
    enable_vtpm                 = true
    enable_integrity_monitoring = true
  }

  metadata = {
    user-data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
      domain        = local.domain
      acme_email    = var.acme_email
      image         = local.image
      caddy_image   = var.caddy_image
      registry_host = local.registry_host
      env           = local.server_env
      project_id    = var.project_id
      secret_prefix = var.name
      secret_names  = var.secret_env_names
    })
    google-logging-enabled    = "true"
    google-monitoring-enabled = "true"
    enable-oslogin            = "TRUE"
  }

  allow_stopping_for_update = true
  deletion_protection       = var.deletion_protection

  lifecycle {
    # cos-stable は新しい版が出るたびに別のイメージになる。VM を作り直さないよう無視する
    # (OS 自体は自動更新される)。
    ignore_changes = [boot_disk[0].initialize_params[0].image]
  }

  depends_on = [
    google_artifact_registry_repository_iam_member.vm_reader,
    google_secret_manager_secret_iam_member.vm_access,
    google_project_iam_member.vm,
  ]
}
