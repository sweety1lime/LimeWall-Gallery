# Builds a portable LimeWall folder: the UI, the renderer daemon and their
# runtime dependencies laid out side by side, so LimeWall.exe runs without a
# dev server. Output: dist/LimeWall/.
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$out = Join-Path $repoRoot "dist\LimeWall"

# Runtime binaries must be fetched first (kept out of git).
$libmpv = Join-Path $repoRoot "third_party\mpv\unpacked\libmpv-2.dll"
$ffmpeg = Join-Path $repoRoot "third_party\ffmpeg\unpacked\ffmpeg.exe"
if (-not (Test-Path $libmpv)) { throw "run scripts/fetch-libmpv.ps1 first" }
if (-not (Test-Path $ffmpeg)) { throw "run scripts/fetch-ffmpeg.ps1 first" }

# LGPL license texts must ship with the LGPL binaries above. Fetch them if the
# maintainer has not run scripts/fetch-licenses.ps1 yet; only warn when offline
# so a dev build still completes (a public release must include them).
$vendorLicenses = Join-Path $repoRoot "licenses\vendor"
if (-not (Test-Path (Join-Path $vendorLicenses "lgpl-3.0.txt"))) {
    Write-Host "fetching GNU license texts..."
    try { & (Join-Path $PSScriptRoot "fetch-licenses.ps1") }
    catch { Write-Warning "could not fetch license texts (offline?): $_ — bundling notice only" }
}

Write-Host "building renderer (release)..."
& cargo build -p renderer --release
if ($LASTEXITCODE -ne 0) { throw "renderer build failed" }

Write-Host "building UI (release, no installer)..."
Push-Location (Join-Path $repoRoot "apps\ui")
& npm.cmd run tauri build -- --no-bundle
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "UI build failed" }
Pop-Location

$renderer = Join-Path $repoRoot "target\release\renderer.exe"
$uiRelease = Join-Path $repoRoot "apps\ui\src-tauri\target\release"
$ui = Get-ChildItem $uiRelease -Filter "*.exe" |
    Where-Object { $_.Name -in @("LimeWall.exe", "ui.exe") } |
    Select-Object -First 1
if (-not $ui) { throw "UI executable not found in $uiRelease" }

if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force $out | Out-Null
New-Item -ItemType Directory -Force (Join-Path $out "shaders\anime4k") | Out-Null

Copy-Item $ui.FullName (Join-Path $out "LimeWall.exe") -Force
Copy-Item $renderer (Join-Path $out "renderer.exe") -Force
Copy-Item $libmpv $out -Force
Copy-Item $ffmpeg $out -Force
Copy-Item (Join-Path $repoRoot "assets\shaders\FSR.glsl") (Join-Path $out "shaders") -Force
Copy-Item (Join-Path $repoRoot "assets\shaders\anime4k\*.glsl") (Join-Path $out "shaders\anime4k") -Force
# Sample web wallpaper for testing.
Copy-Item (Join-Path $repoRoot "assets\web") (Join-Path $out "web") -Recurse -Force

# Third-party license notices (LGPL binaries above + MIT shaders/three.js).
$licensesOut = Join-Path $out "licenses"
New-Item -ItemType Directory -Force $licensesOut | Out-Null
Copy-Item (Join-Path $repoRoot "licenses\THIRD-PARTY-NOTICES.md") $licensesOut -Force
if (Test-Path $vendorLicenses) {
    Copy-Item (Join-Path $vendorLicenses "*.txt") $licensesOut -Force
} else {
    Write-Warning "GNU license texts missing — run scripts/fetch-licenses.ps1 before a public release"
}

Write-Host ""
Write-Host "portable build ready: $out"
Get-ChildItem $out -Recurse -File | ForEach-Object {
    "  {0,-22} {1,8:N0} bytes" -f $_.FullName.Substring($out.Length + 1), $_.Length
}
Write-Host ""
Write-Host "run it: `"$out\LimeWall.exe`""
