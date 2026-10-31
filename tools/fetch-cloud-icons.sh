#!/usr/bin/env bash
# AWS / Azure / Google Cloud の公式アイコン集をダウンロードし、構成図用に取り込む。
#
#   tools/fetch-cloud-icons.sh                 # 3 社ともダウンロードして取り込む
#   tools/fetch-cloud-icons.sh --aws a.zip     # 手元の ZIP を使う(--azure / --gcp も同じ)
#   tools/fetch-cloud-icons.sh --only aws      # 1 社だけ(aws / azure / gcp)
#
# アイコンの利用条件は各社のページを確認してください。
#   AWS:   https://aws.amazon.com/architecture/icons/
#   Azure: https://learn.microsoft.com/azure/architecture/icons/
#   GCP:   https://cloud.google.com/icons
# 公式の配布 URL は変わることがあります。ダウンロードに失敗したら上のページから ZIP を取得し、
# --aws などで渡してください。URL は環境変数 AWS_ICONS_URL / AZURE_ICONS_URL / GCP_ICONS_URL でも変えられます。
set -euo pipefail

cd "$(dirname "$0")/.."

AWS_ICONS_URL=${AWS_ICONS_URL:-https://d1.awsstatic.com/onedam/marketing-channels/website/public/shared/architecture-icon-release/Icon-package_07312026.5846e92413caa21490223536cc97f1269e44fa92.zip}
AZURE_ICONS_URL=${AZURE_ICONS_URL:-https://arch-center.azureedge.net/icons/Azure_Public_Service_Icons_V24.zip}
GCP_ICONS_URL=${GCP_ICONS_URL:-https://services.google.com/fh/files/misc/core-products-icons.zip}

declare -A local_zip=()
only=""
out="crates/board-web/assets/cloud-icons"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --aws|--azure|--gcp) local_zip[${1#--}]="$2"; shift 2 ;;
    --only) only="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "不明な引数: $1" >&2; exit 2 ;;
  esac
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

args=()
for p in aws azure gcp; do
  [[ -n "$only" && "$only" != "$p" ]] && continue
  if [[ -n "${local_zip[$p]:-}" ]]; then
    args+=("--$p" "${local_zip[$p]}")
    continue
  fi
  case "$p" in
    aws) url=$AWS_ICONS_URL ;;
    azure) url=$AZURE_ICONS_URL ;;
    gcp) url=$GCP_ICONS_URL ;;
  esac
  echo "$p: $url をダウンロードしています"
  if curl -fL --retry 2 -o "$work/$p.zip" "$url"; then
    args+=("--$p" "$work/$p.zip")
  else
    echo "$p: ダウンロードに失敗しました。公式ページから ZIP を取得して --$p で渡してください" >&2
  fi
done

if [[ ${#args[@]} -eq 0 ]]; then
  echo "取り込む ZIP がありません" >&2
  exit 1
fi
cargo run --release -q -p cloud-icons -- "${args[@]}" --out "$out"
