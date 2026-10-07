#!/usr/bin/env bash
# Linux 版 libmpv 的建置環境：在 pins.json 的 ubuntu:24.04 映像（digest 固定）裡，用固定日期的 apt 快照安裝固定版本的
# 編譯器與 -dev 套件。只在拋棄式容器裡以 root 執行。需要先裝好 jq、xz-utils、ca-certificates（快照伺服器只有 https）。
#   bash build/setup-base.sh <pins.json>
set -euo pipefail
pins=$1
die() { echo "::error::$*" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive
. /etc/os-release; [[ $VERSION_CODENAME == noble ]] || die "要在 Ubuntu 24.04 的容器裡執行"
apt-get update -qq --snapshot "$(jq -r .base.apt_snapshot "$pins")"
mapfile -t pinned < <(jq -r '.base.packages | to_entries[] | "\(.key)=\(.value)"' "$pins")
mapfile -t tools < <(jq -r '.base.tools[]' "$pins")
# --allow-downgrades：先裝的 ca-certificates 可能帶進比快照新的 libssl3t64
apt-get install -y -qq --no-install-recommends --allow-downgrades "${pinned[@]}" "${tools[@]}" >/dev/null
for p in "${pinned[@]}"; do
  got=$(dpkg-query -W -f='${Version}' "${p%%=*}")
  [[ $got == "${p#*=}" ]] || die "${p%%=*} 的版本是 $got，pins.json 寫 ${p#*=}"
done
dpkg-query -W -f='${Package} ${Version}\n' | sort > /opt/vsbuild/dpkg-versions.txt
