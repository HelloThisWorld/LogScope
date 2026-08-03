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
    # Must go through the Tauri CLI, NOT `cargo build`. A plain cargo build
    # produces an executable that loads the frontend from `devUrl`
    # (http://localhost:5173) instead of embedding `frontendDist`, so the
    # packaged app starts and then shows the WebView2 "can't reach this page"
    # error. The CLI also runs `beforeBuildCommand`, which builds the frontend.
    Write-Host "Building frontend + release executable via the Tauri CLI..."
    Push-Location (Join-Path $repo "apps/desktop")
    npm run tauri build -- --no-bundle
    if ($LASTEXITCODE -ne 0) { Pop-Location; throw "tauri build failed" }
    Pop-Location
}

$exe = Join-Path $repo "target/release/logscope.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

# --- Guard: the executable must actually carry the frontend ---------------
# Packaging a dev-mode binary is invisible until first launch, and every
# artifact from 0.0.0 through 0.2.1 shipped broken this way. Assert that each
# asset referenced by the built index.html is present inside the executable.
$distIndex = Join-Path $repo "apps/desktop/dist/index.html"
if (-not (Test-Path $distIndex)) { throw "missing $distIndex - frontend was never built" }
$assetRefs = [regex]::Matches((Get-Content -Raw $distIndex), 'assets/[A-Za-z0-9._-]+') |
    ForEach-Object { $_.Value } | Sort-Object -Unique
if ($assetRefs.Count -eq 0) { throw "no asset references found in $distIndex" }

$exeText = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($exe))
foreach ($ref in $assetRefs) {
    if ($exeText.IndexOf($ref, [StringComparison]::Ordinal) -lt 0) {
        throw ("$exe does not embed '$ref'. It was built without the Tauri CLI " +
               "and would start with a 'localhost refused to connect' error page. " +
               "Rebuild with: npm run tauri build -- --no-bundle")
    }
}
Remove-Variable exeText
Write-Host "Frontend embedding verified ($($assetRefs.Count) assets)."

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
    # Derived from the executable so identical inputs -> identical archive.
    built_at_utc     = (Get-Item $exe).LastWriteTimeUtc.ToString("yyyy-MM-ddTHH:mm:ssZ")
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
