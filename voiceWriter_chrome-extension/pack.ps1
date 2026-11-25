# Packages the extension into dist/voicewriter-chrome-<version>.zip for the
# Chrome Web Store. Only the files the extension needs are included (no
# store/ images, docs or this script).
#
# Usage (from any folder):  powershell -ExecutionPolicy Bypass -File chrome-extension\pack.ps1
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$ext = $PSScriptRoot
$manifest = Get-Content (Join-Path $ext "manifest.json") -Raw | ConvertFrom-Json
$version = $manifest.version

$files = @(
  "manifest.json",
  "background.js",
  "common.js",
  "content.js",
  "sayit-inject.js",
  "sayit-relay.js",
  "sayit-test.html",
  "sayit-test.js",
  "offscreen.html",
  "offscreen.js",
  "live.html",
  "live.js",
  "popup.html",
  "popup.css",
  "popup.js",
  "welcome.html",
  "welcome.css",
  "welcome.js",
  "icons/icon16.png",
  "icons/icon24.png",
  "icons/icon32.png",
  "icons/icon48.png",
  "icons/icon128.png"
)

foreach ($f in $files) {
  if (-not (Test-Path (Join-Path $ext $f))) { throw "Missing file: $f" }
}

$dist = Join-Path $ext "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$zipPath = Join-Path $dist "voicewriter-chrome-$version.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

# Entries are written by hand with forward slashes; Windows PowerShell's
# Compress-Archive can write backslashes, which the Web Store rejects.
$zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
try {
  foreach ($f in $files) {
    [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
      $zip, (Join-Path $ext $f), $f, [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
  }
} finally {
  $zip.Dispose()
}

$size = [Math]::Round((Get-Item $zipPath).Length / 1KB, 1)
Write-Output "Created $zipPath ($($files.Count) files, $size KB) for VoiceWriter $version"
