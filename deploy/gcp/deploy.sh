#!/usr/bin/env bash
# グラボードを GCP に出す(terraform apply のあとに使う)。
#
#   deploy/gcp/deploy.sh            # build + restart(いつもの更新)
#   deploy/gcp/deploy.sh build      # Cloud Build でイメージを作って push する
#   deploy/gcp/deploy.sh restart    # VM 上のコンテナを最新のイメージで起動し直す(数秒止まる)
#   deploy/gcp/deploy.sh reboot     # VM を止めて起動する(terraform で env などを変えたあと)
#   deploy/gcp/deploy.sh logs       # サーバーと Caddy の直近のログ
#   deploy/gcp/deploy.sh status     # https://…/healthz を確かめる
#
# 必要なもの: gcloud(gcloud auth login 済み)と terraform。
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tf="$here/terraform"

out() { terraform -chdir="$tf" output -raw "$1"; }

project=$(out project_id)
region=$(out region)
zone=$(out zone)
instance=$(out instance_name)

ssh_vm() {
  gcloud compute ssh "$instance" --project "$project" --zone "$zone" --tunnel-through-iap --command "$1"
}

build() {
  local tag
  tag=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M%S)
  if ! git -C "$root" diff --quiet HEAD 2>/dev/null; then tag="$tag-dirty"; fi
  echo "==> build $(out image_repository):$tag (10〜20 分かかります)"
  gcloud builds submit "$root" \
    --project "$project" \
    --region "$region" \
    --config "$here/cloudbuild.yaml" \
    --ignore-file "$here/.gcloudignore" \
    --gcs-source-staging-dir "gs://$(out build_source_bucket)/source" \
    --service-account "projects/$project/serviceAccounts/$(out build_service_account)" \
    --substitutions "_IMAGE=$(out image_repository),_TAG=$tag"
}

restart() {
  echo "==> restart $instance"
  ssh_vm 'sudo systemctl restart graboard.service && sleep 3 && sudo systemctl --no-pager --lines=0 status graboard.service'
}

reboot() {
  echo "==> reboot $instance(停止時に未保存の変更を書き出してから止まります)"
  gcloud compute instances stop "$instance" --project "$project" --zone "$zone"
  gcloud compute instances start "$instance" --project "$project" --zone "$zone"
}

logs() {
  ssh_vm 'sudo journalctl -u graboard.service -u caddy.service -n 200 --no-pager'
}

status() {
  local url
  url=$(out url)
  echo "==> ${url}healthz"
  curl -fsS "${url}healthz" && echo
}

case "${1:-all}" in
  all) build && restart && status ;;
  build) build ;;
  restart) restart ;;
  reboot) reboot ;;
  logs) logs ;;
  status) status ;;
  *) sed -n '2,10p' "$0"; exit 1 ;;
esac
