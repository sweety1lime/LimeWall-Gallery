# Downloads the pinned LGPL libmpv build and puts libmpv-2.dll next to the
# renderer binaries. Source and licensing: docs/third-party.md.
param(
    [string]$Tag = "2026-07-10-e5486b96d7",
    [string]$Asset = "mpv-dev-lgpl-x86_64-20260710-git-e5486b96d7.7z",
    [string]$Sha256 = "826F2F7FA72E8DF4912327703D9EF3CF7D6E5A0F42D8002A11A554142BED0616"
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $repoRoot "third_party\mpv"
New-Item -ItemType Directory -Force $dir | Out-Null
$archive = Join-Path $dir $Asset

if (-not (Test-Path $archive)) {
    $url = "https://github.com/zhongfly/mpv-winbuild/releases/download/$Tag/$Asset"
    Write-Host "downloading $url"
    curl.exe -sL -o $archive $url
    if ($LASTEXITCODE -ne 0) { throw "download failed" }
}

$hash = (Get-FileHash $archive -Algorithm SHA256).Hash
if ($hash -ne $Sha256) {
    Remove-Item $archive
    throw "SHA-256 mismatch: expected $Sha256, got $hash - archive deleted, re-run to retry"
}
Write-Host "sha256 OK"

$unpacked = Join-Path $dir "unpacked"
$sevenZip = @("$env:ProgramFiles\7-Zip\7z.exe", "${env:ProgramFiles(x86)}\7-Zip\7z.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if ($sevenZip) {
    & $sevenZip x -y -o"$unpacked" $archive | Out-Null
} else {
    # Windows bsdtar (libarchive) can read 7z archives.
    New-Item -ItemType Directory -Force $unpacked | Out-Null
    tar -xf $archive -C $unpacked
    if ($LASTEXITCODE -ne 0) { throw "extraction failed: install 7-Zip or a tar with 7z support" }
}

$dll = Join-Path $unpacked "libmpv-2.dll"
if (-not (Test-Path $dll)) { throw "libmpv-2.dll not found in archive" }
foreach ($profile in "debug", "release") {
    $target = Join-Path $repoRoot "target\$profile"
    if (Test-Path $target) {
        Copy-Item $dll $target -Force
        Write-Host "copied libmpv-2.dll -> target\$profile"
    }
}
Write-Host "done; dll at $dll"
