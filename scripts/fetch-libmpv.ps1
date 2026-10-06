# 下載 Windows 版 libmpv 開發包到 vendor/libmpv/windows-x64/
#
# 來源：shinchiro/mpv-winbuild-cmake（Windows 版 mpv 最常用的建置來源）
# 用法：pwsh scripts/fetch-libmpv.ps1
#
# 內容：libmpv-2.dll（引擎本體，執行時要跟 exe 放一起）、
#       libmpv.dll.a（windows-gnu 連結用）、include/mpv/*.h
#
# 版本固定在下面的 Tag / Asset，並檢查 SHA-256：這個 dll 會跟著發佈版一起散布，
# 必須確定每次拿到的都是同一份。更新版本時三個值要一起改。

param(
    [string]$Tag = "20261006",
    [string]$Asset = "mpv-dev-x86_64-20261006-git-6c092d978b.7z",
    [string]$Sha256 = "BC0E016BDF6573BADA824873D4746AB58D9BFAC0DDD59C046F160312F81F9548"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "vendor/libmpv/windows-x64"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) $Asset

if (Test-Path (Join-Path $dest "libmpv-2.dll")) {
    Write-Host "libmpv 已存在：$dest（要重新下載請先刪除該資料夾）"
    exit 0
}

$url = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$Tag/$Asset"
Write-Host "下載 $url"
# 用 curl.exe：PowerShell 的 Invoke-WebRequest 對 GitHub 大檔常會卡住
curl.exe -L --fail -sS -o $tmp $url
if ($LASTEXITCODE -ne 0) {
    throw "下載失敗（curl exit $LASTEXITCODE）。上游只保留最近約 30 個版本，舊版本可能已被移除"
}

$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash
if ($actual -ne $Sha256) {
    Remove-Item $tmp
    throw "SHA-256 不符：預期 $Sha256，實際 $actual"
}

New-Item -ItemType Directory -Force $dest | Out-Null
# Windows 10 1803+ 內建的 tar（libarchive）可以解 7z
tar -xf $tmp -C $dest
if ($LASTEXITCODE -ne 0) { throw "解壓失敗（tar exit $LASTEXITCODE）" }
Remove-Item $tmp

Write-Host "完成：$dest"
Get-ChildItem $dest -Recurse -File | ForEach-Object { "  " + $_.FullName.Substring($dest.Length + 1) }
