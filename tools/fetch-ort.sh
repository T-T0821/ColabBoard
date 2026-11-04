#!/usr/bin/env bash
# ブラウザ内 AI で NPU / GPU を使うための ONNX Runtime Web を取り込む。
#
#   tools/fetch-ort.sh                  # npm から決まった版をダウンロードして取り込む
#   tools/fetch-ort.sh --tgz ort.tgz    # 手元の onnxruntime-web-*.tgz を使う
#
# crates/board-web/assets/ort/ に置き、trunk が /ort/ で配る(Content-Security-Policy が
# 同じオリジンのスクリプトしか許さないので、CDN からは読めない)。取り込んでいなければ、
# 音声認識は今までどおり CPU(wasm)だけで動く。ONNX Runtime は MIT ライセンス。
set -euo pipefail

cd "$(dirname "$0")/.."

ORT_VERSION=${ORT_VERSION:-1.31.1}
out="crates/board-web/assets/ort"
tgz=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tgz) tgz="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "不明な引数: $1" >&2; exit 2 ;;
  esac
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [[ -z "$tgz" ]]; then
  url="https://registry.npmjs.org/onnxruntime-web/-/onnxruntime-web-${ORT_VERSION}.tgz"
  echo "onnxruntime-web ${ORT_VERSION} をダウンロードしています: $url"
  curl -fL --retry 2 -o "$work/ort.tgz" "$url"
  tgz="$work/ort.tgz"
fi
tar -xzf "$tgz" -C "$work"

mkdir -p "$out"
# WebNN と WebGPU の両方が入った版(jsep)だけを使う。
for f in ort.min.mjs ort-wasm-simd-threaded.jsep.mjs ort-wasm-simd-threaded.jsep.wasm; do
  cp "$work/package/dist/$f" "$out/$f"
done
cp "$work/package/LICENSE" "$out/LICENSE.txt" 2>/dev/null || true
echo "$out に取り込みました:"
ls -la "$out"
