#!/usr/bin/env bash
# 各平台 libmpv 共同的元件（同名者）在每個 pins.json 都必須完全相同（版本、網址、雜湊、子模組、授權）。
# 改了某個元件就每個平台一起改，各自的 build_id 加一。
set -euo pipefail
cd "$(dirname "$0")/../.."
files=(packaging/*/libmpv/pins.json)
bad=$(jq -rs '[.[] | .components[]] | group_by(.name)
              | map(select((map(tojson) | unique | length) > 1) | .[0].name) | .[]' "${files[@]}")
if [[ -n $bad ]]; then
  echo "::error::這些元件在各平台的 pins.json 不一致：$(echo $bad)"
  exit 1
fi
for f in "${files[@]}"; do jq -r --arg f "$f" '"\($f)：\(.platform // "win64") \(.build_id)，\(.components | length) 個元件"' "$f"; done
