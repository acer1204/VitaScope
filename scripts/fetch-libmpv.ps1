# 下載 Windows 版 libmpv 開發包到 vendor/libmpv/windows-x64/
#
# 來源：本專案自己建置的 libmpv-2.dll（.github/workflows/libmpv-windows.yml），發佈在本儲存庫的
#       prerelease（tag libmpv-win64-rN；prerelease 不會被當成影戲的新版本）
# 用法：pwsh scripts/fetch-libmpv.ps1 [-WithSource <目錄>]
#
# 內容：libmpv-2.dll（引擎本體，執行時要跟 exe 放一起）、libmpv.dll.a（windows-gnu 連結用）、include/mpv/*.h、
#       THIRD-PARTY-WINDOWS.md 與 licenses/（DLL 內各元件的版本與授權條文，打包時一起附上）
# -WithSource：另外下載對應原始碼包（發佈流程用：每個影戲 Release 都要附上）
#
# 版本固定在 Tag 與 SHA-256（取自該 release 的 SHA256SUMS）：這個 dll 會跟著發佈版一起散布，
# 必須確定每次拿到的都是同一份。更新版本時一起改。

param(
    [string]$Tag = "libmpv-win64-r3",
    [string]$Sha256 = "480cd1987d476f387e20eaa892d7ebc7caa7e19949b33b6d26b664e771a6301b",
    [string]$SourceSha256 = "22bdfc355ff818f505f675853363776eb0414e148122b03ce1f1b555bba75bc6",
    [string]$WithSource = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "vendor/libmpv/windows-x64"
$base = "https://github.com/acer1204/VitaScope/releases/download/$Tag"
$id = $Tag -replace '^libmpv-', 'vitascope-libmpv-'
$marker = Join-Path $dest ".fetched"

function Get-Verified([string]$name, [string]$sha, [string]$to) {
    Write-Host "下載 $base/$name"
    # 用 curl.exe：PowerShell 的 Invoke-WebRequest 對 GitHub 大檔常會卡住
    curl.exe -L --fail --retry 3 -sS -o $to "$base/$name"
    if ($LASTEXITCODE -ne 0) { throw "下載失敗（curl exit $LASTEXITCODE）：$name" }
    $actual = (Get-FileHash $to -Algorithm SHA256).Hash
    if ($actual -ne $sha.ToUpper()) { Remove-Item $to; throw "SHA-256 不符：$name 預期 $sha，實際 $actual" }
}

if ((Test-Path -LiteralPath $marker) -and ((Get-Content -LiteralPath $marker -Raw).Trim() -eq "$Tag $Sha256")) {
    Write-Host "libmpv 已是 $Tag：$dest"
} else {
    # 先下載、核對、解壓到暫存資料夾，成功了才把舊版（包括之前用的別人建置的版本）整個換掉：
    # 下載失敗或雜湊不符時，原本能用的那一份還在
    $zip = Join-Path ([System.IO.Path]::GetTempPath()) "$id.zip"
    Get-Verified "$id.zip" $Sha256 $zip
    $new = "$dest.new"
    if (Test-Path -LiteralPath $new) { Remove-Item -LiteralPath $new -Recurse -Force }
    New-Item -ItemType Directory -Force $new | Out-Null
    # 用 Windows 內建的 tar（libarchive，解得開 zip）：從 Git Bash 執行時 PATH 上先找到的是 GNU tar，它解不開 zip、也看不懂 C: 路徑
    & (Join-Path $env:SystemRoot "System32\tar.exe") -xf $zip -C $new
    if ($LASTEXITCODE -ne 0) { throw "解壓失敗（tar exit $LASTEXITCODE）" }
    Remove-Item -LiteralPath $zip
    if (Test-Path -LiteralPath $dest) { Remove-Item -LiteralPath $dest -Recurse -Force }
    Move-Item -LiteralPath $new -Destination $dest
    # zip 裡的時間是固定的建置時間；改成現在，build.rs 比對大小與時間時才會換上新的 DLL
    Get-ChildItem $dest -Recurse -File | ForEach-Object { $_.LastWriteTime = Get-Date }
    # 參數要具名：-NoNewline（檔案系統的動態參數）放在前面時，位置參數會對調，變成在目前資料夾建立以內容為名的檔案
    Set-Content -LiteralPath $marker -Value "$Tag $Sha256" -NoNewline
    Write-Host "完成：$dest"
}
if ($WithSource) {
    New-Item -ItemType Directory -Force $WithSource | Out-Null
    Get-Verified "$id-src.tar.xz" $SourceSha256 (Join-Path $WithSource "$id-src.tar.xz")
}
