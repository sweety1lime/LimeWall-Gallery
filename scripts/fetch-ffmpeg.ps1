# Downloads the pinned LGPL ffmpeg build used by the library import pipeline
# (GIF -> mp4, previews). Source and licensing: docs/third-party.md.
param(
    [string]$Tag = "2026-07-10-e5486b96d7",
    [string]$Asset = "ffmpeg-lgpl-x86_64-git-35f8f4bdc.7z",
    [string]$Sha256 = "4EBCF42AF804FC5B6119C1C2D248B2509707A773A3A1F76B81F97E77BE353E48"
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $repoRoot "third_party\ffmpeg"
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
    New-Item -ItemType Directory -Force $unpacked | Out-Null
    tar -xf $archive -C $unpacked
    if ($LASTEXITCODE -ne 0) { throw "extraction failed: install 7-Zip or a tar with 7z support" }
}

$ffmpeg = Join-Path $unpacked "ffmpeg.exe"
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg.exe not found in archive" }
Write-Host "done; ffmpeg at $ffmpeg"
