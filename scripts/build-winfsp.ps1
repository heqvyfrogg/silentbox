#!/usr/bin/env pwsh
# WinFsp バックエンド込みのビルド。
#
# 実測した制約:
#   - windows-gnu ターゲットでは winfsp-sys が失敗する。MSVC 必須。
#   - winfsp-x64.dll が無いと実行時に 1285 で落ちる。
#     exe と同じディレクトリへコピーする。
#
# 使い方: pwsh -File .\scripts\build-winfsp.ps1 [-Release]
param(
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$toolchain = "stable-x86_64-pc-windows-msvc"
$target = "x86_64-pc-windows-msvc"

Write-Host "== ツールチェーンの確認: $toolchain" -ForegroundColor Cyan
$installed = rustup toolchain list | Select-String -Pattern ([regex]::Escape($toolchain))
if (-not $installed) {
    Write-Host "   $toolchain がありません。次を実行してください:" -ForegroundColor Yellow
    Write-Host "   rustup toolchain install $toolchain"
    exit 1
}

$profile = if ($Release) { "--release" } else { "" }
$argsList = @(
    "run", $toolchain, "build",
    "-p", "terabox-cli",
    "--features", "winfsp-backend",
    "--target", $target
)
if ($Release) { $argsList += "--release" }

Write-Host "== ビルド" -ForegroundColor Cyan
& cargo @argsList
if ($LASTEXITCODE -ne 0) {
    Write-Host "ビルドに失敗しました。" -ForegroundColor Red
    exit $LASTEXITCODE
}

$outDir = if ($Release) {
    Join-Path $root "target\$target\release"
} else {
    Join-Path $root "target\$target\debug"
}

# winfsp-x64.dll を exe の隣へ配置する（遅延リンクの解決に必要）。
$dllCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} "WinFsp\bin\winfsp-x64.dll"),
    (Join-Path $env:ProgramFiles "WinFsp\bin\winfsp-x64.dll")
)
$dll = $dllCandidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1

if ($dll) {
    Copy-Item $dll (Join-Path $outDir "winfsp-x64.dll") -Force
    Write-Host "== winfsp-x64.dll を $outDir へ配置しました" -ForegroundColor Green
} else {
    Write-Host "== 警告: WinFsp が見つかりません。--mode webdav を使ってください。" -ForegroundColor Yellow
}

$exe = Join-Path $outDir "tbx.exe"
if (Test-Path $exe) {
    Write-Host "== 動作確認" -ForegroundColor Cyan
    & $exe --version
    Write-Host "== 完了: $exe" -ForegroundColor Green
} else {
    Write-Host "== tbx.exe が見つかりません: $exe" -ForegroundColor Red
    exit 1
}
