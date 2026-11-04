terraform {
  required_version = ">= 1.5"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = ">= 6.0, < 9.0"
    }
  }

  # 状態ファイルをチームで共有するときは GCS に置く(README の「状態ファイルの置き場所」)。
  # backend "gcs" {
  #   bucket = "my-project-tfstate"
  #   prefix = "graboard"
  # }
}

provider "google" {
  project = var.project_id
  region  = var.region
  zone    = var.zone
}
