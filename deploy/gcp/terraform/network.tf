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

# アクセス元の制限(allowed_source_ranges)。既定は制限なし。
# 制限するときは、外形監視(monitoring.tf)が届くよう Google の Uptime check の IP も足す。
data "google_monitoring_uptime_check_ips" "this" {
  count = local.ip_restricted && var.allow_uptime_check_ips ? 1 : 0
}

locals {
  ip_restricted = !contains(var.allowed_source_ranges, "0.0.0.0/0")

  uptime_check_ranges = local.ip_restricted && var.allow_uptime_check_ips ? [
    for ip in data.google_monitoring_uptime_check_ips.this[0].uptime_check_ips :
    "${ip.ip_address}/32" if !strcontains(ip.ip_address, ":")
  ] : []

  web_source_ranges = distinct(concat(var.allowed_source_ranges, local.uptime_check_ranges))
}

# HTTPS(グラボード本体)。allowed_source_ranges からだけ開ける。
# 8080 番(グラボード本体)は開けない: Caddy からだけつながる(TRUST_PROXY=1 の前提)。
resource "google_compute_firewall" "https" {
  name          = "${var.name}-https"
  network       = google_compute_network.graboard.id
  direction     = "INGRESS"
  source_ranges = local.web_source_ranges
  target_tags   = [var.name]

  allow {
    protocol = "tcp"
    ports    = ["443"]
  }
  allow {
    protocol = "udp"
    ports    = ["443"] # HTTP/3
  }
}

# HTTP は IP を制限していても全体に開ける。Let's Encrypt が証明書の発行・更新で
# どこからでも 80 番に来るため(HTTP-01 チャレンジ)。80 番では https への転送と
# チャレンジの応答しかしないので、グラボードの中身には届かない。
resource "google_compute_firewall" "http" {
  name          = "${var.name}-http"
  network       = google_compute_network.graboard.id
  direction     = "INGRESS"
  source_ranges = ["0.0.0.0/0"]
  target_tags   = [var.name]

  allow {
    protocol = "tcp"
    ports    = ["80"]
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
