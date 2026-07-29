# Assembles the Windows x64 portable ZIP for LogScope.
#
# Portable-first contract (ADR-0002):
# - ordinary ZIP, no installer, no self-extractor, no bootstrapper;
# - no signing identity required or used;
# - one relocatable application directory; runtime treats it as read-only;
# - optional fixed WebView2 runtime bundled under .\webview2 so first launch
#   never installs or downloads WebView2 (fully offline artifact);
# - deterministic assembly from an explicit manifest + SHA-256 checksum.
#
# Usage:
#   pwsh scripts/package-portable.ps1 [-Version 0.0.0]
#       [-WebView2FixedDir <extracted fixed-version runtime folder>]
#       [-SkipBuild]
#
# The fixed WebView2 runtime is the "Fixed Version" distribution from
# Microsoft (a folder containing msedgewebview2.exe). Without it the archive
# is still produced but marked "webview2": "evergreen-required", which is
# NOT the fully offline artifact.

[CmdletBinding()]
param(
    [string]$Version = "0.0.0",
    [string]$WebView2FixedDir = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$target = "x86_64-pc-windows-msvc"
$archiveName = "LogScope-$Version-windows-x64-portable"
$outDir = Join-Path $repo "dist-portable"
$stageDir = Join-Path $outDir $archiveName

if (-not $SkipBuild) {
    Write-Host "Building frontend..."
    Push-Location (Join-Path $repo "apps/desktop")
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "frontend build failed" }
    Pop-Location

    Write-Host "Building release executable..."
    cargo build --release -p logscope-desktop
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$exe = Join-Path $repo "target/release/logscope.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

if (Test-Path $stageDir) { Remove-Item -Recurse -Force $stageDir }
New-Item -ItemType Directory -Force $stageDir | Out-Null
New-Item -ItemType Directory -Force $outDir | Out-Null

# --- Explicit package manifest (files copied are exactly these) -----------
$entries = @()
function Add-PackagedFile([string]$Source, [string]$RelDest) {
    $dest = Join-Path $script:stageDir $RelDest
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Copy-Item $Source $dest
    $hash = (Get-FileHash -Algorithm SHA256 $dest).Hash.ToLowerInvariant()
    $script:entries += [ordered]@{
        path      = $RelDest.Replace('\', '/')
        bytes     = (Get-Item $dest).Length
        sha256    = $hash
    }
}

Add-PackagedFile $exe "logscope.exe"

# Third-party notices: generated license summary for bundled native parts.
$notices = Join-Path $repo "docs/THIRD-PARTY-NOTICES.md"
if (Test-Path $notices) { Add-PackagedFile $notices "THIRD-PARTY-NOTICES.md" }
$readme = Join-Path $repo "docs/PORTABLE-README.md"
if (Test-Path $readme) { Add-PackagedFile $readme "README.md" }

# Optional fixed WebView2 runtime.
$webview2State = "evergreen-required"
if ($WebView2FixedDir -ne "") {
    if (-not (Test-Path (Join-Path $WebView2FixedDir "msedgewebview2.exe"))) {
        throw "WebView2FixedDir does not look like a fixed-version runtime (msedgewebview2.exe missing)"
    }
    Write-Host "Bundling fixed WebView2 runtime..."
    Get-ChildItem -Recurse -File $WebView2FixedDir | ForEach-Object {
        $rel = $_.FullName.Substring($WebView2FixedDir.Length).TrimStart('\', '/')
        Add-PackagedFile $_.FullName (Join-Path "webview2" $rel)
    }
    $webview2State = "fixed-runtime-bundled"
}

# --- Machine-readable manifest -------------------------------------------
$manifest = [ordered]@{
    name             = "LogScope"
    version          = $Version
    target           = $target
    portable         = $true
    installer        = $false
    signed           = $false
    webview2         = $webview2State
    built_at_utc     = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    files            = $entries
}
$manifestPath = Join-Path $stageDir "package-manifest.json"
$manifest | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 $manifestPath

# --- Deterministic ZIP ----------------------------------------------------
# Fixed entry order (manifest order + manifest itself) and fixed timestamps
# so identical inputs produce an identical archive.
$zipPath = Join-Path $outDir "$archiveName.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zipStream = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::Create)
$zip = New-Object System.IO.Compression.ZipArchive($zipStream, [System.IO.Compression.ZipArchiveMode]::Create)
$fixedTime = [System.DateTimeOffset]::new(2000, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
$allFiles = @($entries | ForEach-Object { $_.path }) + @("package-manifest.json")
foreach ($rel in $allFiles) {
    $srcPath = Join-Path $stageDir $rel
    $entry = $zip.CreateEntry("$archiveName/$rel", [System.IO.Compression.CompressionLevel]::Optimal)
    $entry.LastWriteTime = $fixedTime
    $entryStream = $entry.Open()
    $fileStream = [System.IO.File]::OpenRead($srcPath)
    $fileStream.CopyTo($entryStream)
    $fileStream.Dispose()
    $entryStream.Dispose()
}
$zip.Dispose()
$zipStream.Dispose()

$zipHash = (Get-FileHash -Algorithm SHA256 $zipPath).Hash.ToLowerInvariant()
"$zipHash *$archiveName.zip" | Set-Content -Encoding ascii "$zipPath.sha256"

Write-Host ""
Write-Host "Portable archive : $zipPath"
Write-Host "Archive size     : $([math]::Round((Get-Item $zipPath).Length / 1MB, 1)) MiB"
Write-Host "SHA-256          : $zipHash"
Write-Host "WebView2         : $webview2State"
