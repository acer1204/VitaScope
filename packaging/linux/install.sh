#!/bin/sh
# 影戲 VitaScope：安裝到目前使用者（~/.local），不需要 root。
#   ./install.sh               安裝 / 更新
#   ./install.sh --default     安裝並設為影音檔的預設播放器
#   ./install.sh --uninstall   移除
set -eu

APP_ID=io.github.acer1204.vitascope
here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin=$HOME/.local/bin
desktop=$data/applications/$APP_ID.desktop

refresh() {
    # 讓「開啟方式」清單與圖示馬上更新（工具不存在就略過，桌面環境之後也會自己重新掃描）
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$data/applications" || true
    fi
    if command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -q -t -f "$data/icons/hicolor" 2>/dev/null || true
    fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin/vitascope" "$desktop"
    find "$data/icons/hicolor" \( -name "$APP_ID.png" -o -name "$APP_ID.svg" \) -exec rm -f {} + 2>/dev/null || true
    # --default 設的預設程式（只刪影戲的，其他程式的設定不動）。有影戲的項目才改；
    # 是捷徑的話（例如放在 dotfiles 裡）改它指到的檔案，不要把捷徑換成一般檔案
    mimeapps=${XDG_CONFIG_HOME:-$HOME/.config}/mimeapps.list
    id_re=$(printf '%s' "$APP_ID" | sed 's/\./\\./g')
    if [ -f "$mimeapps" ] && grep -q "$id_re\.desktop" "$mimeapps"; then
        target=$(readlink -f "$mimeapps")
        sed -i -e "/=$id_re\.desktop;*\$/d" -e "s/$id_re\.desktop;//g" "$target"
    fi
    refresh
    echo "已移除。設定檔在 ~/.config/vitascope，不需要可以自行刪除。"
    exit 0
fi

install -Dm755 "$here/vitascope" "$bin/vitascope"

# 圖示：share/icons/hicolor/<尺寸>/apps/$APP_ID.png（規格要求至少 48x48）
(cd "$here/share/icons" && find hicolor -type f) | while IFS= read -r f; do
    install -Dm644 "$here/share/icons/$f" "$data/icons/$f"
done

# Exec 寫絕對路徑：從桌面環境啟動時 ~/.local/bin 不一定在 PATH 裡。
# 路徑放在雙引號裡（Desktop Entry 規格的 Exec 一節）：引號內的 " ` $ \ 要加反斜線，而檔案裡的值
# 會先做一般字串的跳脫處理（\\ → \），所以 " ` $ 寫成 \\"、\ 寫成 \\\\。
# 路徑裡有 % 時沒辦法寫（規格是寫成 %%，但 GNOME 的 GLib 用沒展開的路徑找程式，會說找不到）：
# 改用 PATH 裡的 vitascope，也不寫 TryExec。
# 用 ENVIRON 把值交給 awk：awk -v 會解讀反斜線，sed 的取代字串又怕 & 和 |
case "$bin" in
    *%*)
        exec_line="Exec=vitascope %F"
        try_line=""
        echo "提醒：安裝路徑裡有 %，選單項目改用 PATH 裡的 vitascope（$bin 要在 PATH 裡）"
        ;;
    *)
        quoted=$(printf '%s' "$bin/vitascope" | sed -e 's/\\/\\\\\\\\/g' -e 's/["`$]/\\\\&/g')
        exec_line="Exec=\"$quoted\" %F"
        try_line="TryExec=$(printf '%s' "$bin/vitascope" | sed -e 's/\\/\\\\/g')"
        ;;
esac
mkdir -p "$data/applications"
EXEC_LINE="$exec_line" TRY_LINE="$try_line" awk '
    /^Exec=/    { print ENVIRON["EXEC_LINE"]; next }
    /^TryExec=/ { if (ENVIRON["TRY_LINE"] != "") print ENVIRON["TRY_LINE"]; next }
    { print }' "$here/share/applications/$APP_ID.desktop" > "$desktop"
refresh

if [ "${1:-}" = "--default" ]; then
    if command -v xdg-mime >/dev/null; then
        types=$(sed -n 's/^MimeType=//p' "$desktop" | tr ';' ' ')
        # shellcheck disable=SC2086 # 要拆成多個參數
        xdg-mime default "$APP_ID.desktop" $types
        echo "已設為預設播放器（寫在 ~/.config/mimeapps.list）"
    else
        echo "找不到 xdg-mime（xdg-utils 套件），請到系統設定選擇預設應用程式"
    fi
fi
echo "已安裝：$bin/vitascope"

# 這個版本用系統的 libmpv；少了就提醒（AppImage 版不需要）
if command -v ldd >/dev/null && ldd "$bin/vitascope" 2>/dev/null | grep -q 'not found'; then
    echo "缺少以下函式庫，請先安裝 libmpv（Ubuntu / Debian：sudo apt install libmpv2；Fedora：sudo dnf install mpv-libs）："
    ldd "$bin/vitascope" | grep 'not found'
fi
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "提醒：$bin 不在 PATH 裡，從終端機執行要打完整路徑（從應用程式選單開啟不受影響）" ;;
esac
