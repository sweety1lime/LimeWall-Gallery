# Fetches the canonical GNU license texts that the LGPL runtime components
# (libmpv, ffmpeg) require us to distribute. Kept out of git (like third_party/)
# and pulled from the authoritative source rather than transcribed. Bundled into
# the portable build by scripts/build-portable.ps1. See licenses/THIRD-PARTY-NOTICES.md.
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $repoRoot "licenses\vendor"
New-Item -ItemType Directory -Force $dir | Out-Null

# libmpv is LGPL-2.1-or-later; the bundled build links FFmpeg (LGPL-3.0, i.e.
# additional permissions on top of GPL-3.0), so all three texts are needed.
$texts = @{
    "lgpl-2.1.txt" = "https://www.gnu.org/licenses/lgpl-2.1.txt"
    "lgpl-3.0.txt" = "https://www.gnu.org/licenses/lgpl-3.0.txt"
    "gpl-3.0.txt"  = "https://www.gnu.org/licenses/gpl-3.0.txt"
}

foreach ($name in $texts.Keys) {
    $target = Join-Path $dir $name
    $url = $texts[$name]
    Write-Host "downloading $url"
    curl.exe -sfL -o $target $url
    if ($LASTEXITCODE -ne 0) { throw "download failed: $url" }
    # A hijacked mirror would not return the GNU license preamble.
    if (-not (Select-String -Path $target -Pattern "GNU (LESSER )?GENERAL PUBLIC LICENSE" -Quiet)) {
        Remove-Item $target
        throw "unexpected content for $name - deleted"
    }
}

Write-Host "done; license texts in $dir"
