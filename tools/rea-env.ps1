# rea-env.ps1
# REA の Android (JADX) プロバイダを「この PowerShell プロセス内だけ」で有効化する。
# グローバル環境変数・ユーザープロファイル・.dsh 設定は一切変更しない。
#
# 使い方:
#   . .\tools\rea-env.ps1
#   rea doctor

$__tbxRoot = Split-Path -Parent $PSScriptRoot
$__jdkHome = Join-Path $PSScriptRoot "jdk21"
$__jadxJar = Join-Path $PSScriptRoot "jadx-headless-mcp-0.7.1-all.jar"

if (-not (Test-Path (Join-Path $__jdkHome "bin\java.exe"))) {
    throw "JDK が見つかりません: $__jdkHome\bin\java.exe"
}
if (-not (Test-Path $__jadxJar)) {
    throw "jadx-headless-mcp JAR が見つかりません: $__jadxJar"
}

$env:JAVA_HOME = $__jdkHome
$env:REA_JADX_MCP_JAR = $__jadxJar

$env:PATH = (Join-Path $__jdkHome "bin") + ";" + $env:PATH

Write-Host "[rea-env] JAVA_HOME      = $env:JAVA_HOME"
Write-Host "[rea-env] REA_JADX_MCP_JAR = $env:REA_JADX_MCP_JAR"
Write-Host "[rea-env] project root   = $__tbxRoot"

Remove-Variable __tbxRoot, __jdkHome, __jadxJar -ErrorAction SilentlyContinue
