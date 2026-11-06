$ErrorActionPreference = "Continue"
Set-Location "C:\Users\User\workspace\rea-TeraBox"
. .\tools\rea-env.ps1 | Out-Null
$env:REA_JADX_HEAP_MIB = "6144"

$out = ".\analysis"
New-Item -ItemType Directory -Force -Path $out | Out-Null

$queries = @(
    "KmpFileRepository",
    "PreCreateDataBean",
    "SuperFile2",
    "QrCodeLoginService",
    "NetworkCommonParameterValues"
)

foreach ($q in $queries) {
    $safe = ($q -replace '[^A-Za-z0-9_]', '_')
    $dest = Join-Path $out ("classes-" + $safe + ".json")
    rea search-android-classes ".\_analyze\base.apk" $q --format json > $dest 2> ($dest + ".err")
    Write-Output ("$q => exit=$LASTEXITCODE size=" + (Get-Item $dest -ErrorAction SilentlyContinue).Length)
}
