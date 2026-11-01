resource "google_compute_network" "graboard" {
  name                    = var.name
  auto_create_subnetworks = false

  depends_on = [google_project_service.apis]
}

resource "google_compute_subnetwork" "graboard" {
  name          = var.name
  network       = google_compute_network.graboard.id
  region        = var.region
  ip_cidr_range = "10.10.0.0/24"
}

# 公開する固定 IP。ドメインの A レコードはここに向ける。
resource "google_compute_address" "graboard" {
  name   = var.name
  region = var.region

  depends_on = [google_project_service.apis]
}

# HTTP(証明書の取得と https への転送)と HTTPS だけを開ける。
# 8080 番(グラボード本体)は開けない: Caddy からだけつながる(TRUST_PROXY=1 の前提)。
resource "google_compute_firewall" "web" {
  name          = "${var.name}-web"
  network       = google_compute_network.graboard.id
  direction     = "INGRESS"
  source_ranges = ["0.0.0.0/0"]
  target_tags   = [var.name]

  allow {
    protocol = "tcp"
    ports    = ["80", "443"]
  }
  allow {
    protocol = "udp"
    ports    = ["443"] # HTTP/3
  }
}

resource "google_compute_firewall" "ssh" {
  count = length(var.ssh_source_ranges) > 0 ? 1 : 0

  name          = "${var.name}-ssh"
  network       = google_compute_network.graboard.id
  direction     = "INGRESS"
  source_ranges = var.ssh_source_ranges
  target_tags   = [var.name]

  allow {
    protocol = "tcp"
    ports    = ["22"]
  }
}
