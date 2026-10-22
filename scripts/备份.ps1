# 备份当前 release 版 jianyan.exe 到 backups\jianyan_<版本号>.exe
# 用法：powershell -File scripts\备份.ps1 0_3_0
param([Parameter(Mandatory = $true)][string]$版本号)

$根 = Split-Path -Parent $PSScriptRoot
$源 = Join-Path $根 "target\release\jianyan.exe"
$备份目录 = Join-Path $根 "backups"
if (-not (Test-Path $源)) {
    Write-Host "错误：找不到 $源（请先 cargo build --release）" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $备份目录 -Force | Out-Null
$目标 = Join-Path $备份目录 "jianyan_$版本号.exe"
Copy-Item $源 $目标 -Force
Write-Host "已备份：$目标"
