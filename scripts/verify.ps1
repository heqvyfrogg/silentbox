#!/usr/bin/env pwsh
# 受け入れ基準のうち、認証なしで実行できるものをまとめて確認する。
# 使い方: pwsh -File .\scripts\verify.ps1
# 終了コードは失敗数。
#
# 方針: 各チェックは失敗時に throw する。`$LASTEXITCODE` の残留に依存しない。

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$script:failures = 0

function Check([string]$name, [scriptblock]$block) {
    Write-Host "== $name" -ForegroundColor Cyan
    try {
        & $block
        Write-Host "   [ok]" -ForegroundColor Green
    } catch {
        Write-Host "   [NG] $_" -ForegroundColor Red
        $script:failures++
    }
}

Check "cargo test --workspace" {
    cargo test --workspace --quiet
    if ($LASTEXITCODE -ne 0) { throw "cargo test が $LASTEXITCODE で失敗しました" }
}

Check "実ネットワーク（認証不要）" {
    cargo test -p terabox-core --test live_anonymous --quiet -- --ignored
    if ($LASTEXITCODE -ne 0) { throw "実ネットワーク試験が $LASTEXITCODE で失敗しました" }
}

$exe = Join-Path $root "target\release\tbx.exe"
if (-not (Test-Path $exe)) { $exe = Join-Path $root "target\debug\tbx.exe" }
if (-not (Test-Path $exe)) {
    Write-Host "tbx.exe が見つかりません。先に cargo build を実行してください。" -ForegroundColor Red
    exit 1
}
Write-Host "== 使用するバイナリ: $exe" -ForegroundColor Cyan

# 検証専用のホーム（本番の資格情報を触らない）。
$env:TBX_HOME = Join-Path $root "_run\home"
New-Item -ItemType Directory -Force -Path $env:TBX_HOME | Out-Null

Check "tbx --version" {
    & $exe --version | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "tbx --version が $LASTEXITCODE で失敗しました" }
}

Check "tbx doctor（未ログインでも診断は成功）" {
    & $exe doctor | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "tbx doctor が $LASTEXITCODE で失敗しました" }
}

Check "tbx doctor --json は機械可読" {
    $json = & $exe doctor --json | Out-String
    if ($LASTEXITCODE -ne 0) { throw "tbx doctor --json が $LASTEXITCODE で失敗しました" }
    $parsed = $json | ConvertFrom-Json
    if ($parsed.Count -lt 5) { throw "診断項目が少なすぎます: $($parsed.Count)" }
    Write-Host "   診断項目数: $($parsed.Count)"
}

# 失敗を期待するコマンドは、標準出力と標準エラーをファイルへ落として判定する。
# Windows PowerShell 5.1 の `2>&1` は ErrorActionPreference=Stop と衝突して
# 例外になるため、リダイレクトで受ける。
function Invoke-ExpectFailure([string]$exe, [string[]]$argv, [string]$mustContain) {
    $outFile = Join-Path $env:TEMP ("tbx-check-" + [guid]::NewGuid().ToString("N") + ".out")
    $errFile = Join-Path $env:TEMP ("tbx-check-" + [guid]::NewGuid().ToString("N") + ".err")
    $proc = Start-Process -FilePath $exe -ArgumentList $argv -NoNewWindow -Wait `
        -RedirectStandardOutput $outFile -RedirectStandardError $errFile -PassThru

    # tbx は UTF-8 で書く。Get-Content の既定（ANSI）で読むと文字化けするため、
    # バイト列を明示的に UTF-8 として復号する。
    $text = ""
    foreach ($f in @($outFile, $errFile)) {
        if (Test-Path $f) {
            $bytes = [System.IO.File]::ReadAllBytes($f)
            if ($bytes.Length -gt 0) {
                $text += [System.Text.Encoding]::UTF8.GetString($bytes)
            }
        }
    }
    Remove-Item $outFile, $errFile -Force -ErrorAction SilentlyContinue

    if ($proc.ExitCode -eq 0) {
        throw "失敗するはずのコマンドが 0 で終了しました: $text"
    }
    if ($mustContain -and ($text -notmatch [regex]::Escape($mustContain))) {
        throw "想定した文言「$mustContain」がありません: $text"
    }
    return @{ ExitCode = $proc.ExitCode; Text = $text }
}

Check "tbx ls は未ログインを明示エラーで返す" {
    $r = Invoke-ExpectFailure $exe @("ls", "/") "セッション"
    Write-Host "   予期どおり失敗（exit $($r.ExitCode)）"
}

Check "tbx quota は未ログインを明示エラーで返す" {
    $r = Invoke-ExpectFailure $exe @("quota") "セッション"
    Write-Host "   予期どおり失敗（exit $($r.ExitCode)）"
}

Check "tbx share は未ログインを明示エラーで返す" {
    $r = Invoke-ExpectFailure $exe @("share", "https://www.terabox.com/s/1test") "セッション"
    Write-Host "   予期どおり失敗（exit $($r.ExitCode)）"
}

Check "tbx transfers は空でも成功" {
    & $exe transfers | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "tbx transfers が $LASTEXITCODE で失敗しました" }
}

Check "tbx transfers --json" {
    $json = & $exe transfers --json | Out-String
    if ($LASTEXITCODE -ne 0) { throw "tbx transfers --json が $LASTEXITCODE で失敗しました" }
    $parsed = $json | ConvertFrom-Json
    Write-Host "   未完了の転送: $($parsed.Count) 件"
}

Write-Host ""
if ($script:failures -eq 0) {
    Write-Host "全て成功しました。" -ForegroundColor Green
} else {
    Write-Host "$script:failures 件失敗しました。" -ForegroundColor Red
}
exit $script:failures
