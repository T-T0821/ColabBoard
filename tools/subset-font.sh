#!/usr/bin/env bash
# 付箋の文字描画に使う日本語フォント(Noto Sans JP, SIL OFL 1.1)を作り直すスクリプト。
# 可変フォントから Regular(wght=400)を切り出し、JIS 第1・第2水準 + かな + 記号に絞る。
#   必要: pip install fonttools
set -euo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d)
curl -sSL -o "$work/vf.ttf" "https://raw.githubusercontent.com/google/fonts/main/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf"
fonttools varLib.instancer "$work/vf.ttf" wght=400 -o "$work/regular.ttf" -q
pyftsubset "$work/regular.ttf" \
  --unicodes-file=font-unicodes.txt \
  --output-file=../crates/board-web/assets/fonts/NotoSansJP-Regular.ttf \
  --layout-features='*' --name-IDs='*' --no-hinting
rm -rf "$work"
