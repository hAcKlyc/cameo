#!/usr/bin/env pwsh
# publish_release.ps1 — sign + upload the Windows auto-update release to R2.
#
# Windows half of the release. macOS publishes independently from a Mac via
# publish_release.sh — the two write DIFFERENT manifest files on R2
# (windows-x86_64.json here, darwin-*.json there), so independent, async uploads
# never clobber each other.
#
# Prerequisite:  .\build_release.ps1           (NSIS installer + updater payload)
#
# Usage:
#   .\publish_release.ps1                       # sign + upload windows artifacts
#   .\publish_release.ps1 -DryRun               # print what would happen, upload nothing
#
# What it does:
#   1. Reads version from src-tauri/tauri.conf.json
#   2. Locates the auto-update payload (.nsis.zip), reads/creates its .sig
#      (signed at build time via TAURI_SIGNING_PRIVATE_KEY), writes a manifest
#      JSON ({version, notes, pub_date, signature, url}).
#   3. rclone uploads to R2:
#      - .nsis.zip + .sig + -setup.exe -> release/v<version>/
#      - manifest -> update/windows-x86_64.json
#
# R2 credentials come from .env (R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY /
# R2_ENDPOINT / R2_BUCKET). The same .env that hosts TAURI_SIGNING_PRIVATE_KEY
# also hosts these.

param([switch]$DryRun)

$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot

function Info($m) { Write-Host "-> $m" }
function Ok($m)   { Write-Host "  [ok] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  [!] $m"  -ForegroundColor Yellow }
function Die($m)  { Write-Host "  [x] $m"  -ForegroundColor Red; exit 1 }
function Test-NameHasVersionToken($name, $version) {
  $escaped = [regex]::Escape($version)
  return $name -match "(^|[^0-9])$escaped([^0-9]|$)"
}
function Assert-NameHasVersionToken($artifact, $label, $version) {
  if (-not (Test-NameHasVersionToken $artifact.Name $version)) {
    Die "$label version mismatch: expected file name to include $version, got $($artifact.Name)"
  }
}
function Format-Size($path) {
  $bytes = (Get-Item -LiteralPath $path).Length
  if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
  if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
  if ($bytes -ge 1KB) { return "{0:N1} KB" -f ($bytes / 1KB) }
  return "$bytes B"
}
function Resolve-LocalCommand($name) {
  $cmd = Get-Command $name -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }

  foreach ($candidate in @(
    (Join-Path $PSScriptRoot "$name.exe"),
    (Join-Path $PSScriptRoot $name)
  )) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
  }

  return $null
}

$Target = 'x86_64-pc-windows-msvc'

# -- load .env (KEY=VALUE per line) ------------------------------------------
function Import-DotEnv($path) {
  if (-not (Test-Path $path)) { Die ".env not found - needed for R2 creds and signing key" }
  Info "loading .env"
  Get-Content $path | ForEach-Object {
    $line = $_.Trim()
    if ($line -eq '' -or $line.StartsWith('#')) { return }
    $idx = $line.IndexOf('=')
    if ($idx -lt 1) { return }
    $k = $line.Substring(0, $idx).Trim()
    $v = $line.Substring($idx + 1).Trim().Trim('"').Trim("'")
    [Environment]::SetEnvironmentVariable($k, $v, 'Process')
  }
}
Import-DotEnv (Join-Path $PSScriptRoot '.env')

foreach ($v in 'R2_ACCESS_KEY_ID', 'R2_SECRET_ACCESS_KEY', 'R2_ENDPOINT', 'R2_BUCKET') {
  if (-not [Environment]::GetEnvironmentVariable($v)) { Die "$v missing from .env" }
}
if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
  Warn "TAURI_SIGNING_PRIVATE_KEY not set - publish will fail unless a non-empty .sig already exists"
} elseif ($null -eq [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'Process')) {
  [Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', '', 'Process')
}
$Rclone = Resolve-LocalCommand 'rclone'
if (-not $Rclone) { Die "rclone not found - install it or place rclone.exe in the project root" }
Ok "rclone: $Rclone"

# -- version -----------------------------------------------------------------
$pkgVer = (node -p "require('./package.json').version" 2>$null)
$confVer = (node -p "require('./src-tauri/tauri.conf.json').version" 2>$null)
$cargoVer = ((Select-String -Path 'src-tauri\Cargo.toml' -Pattern '^version\s*=\s*"(.*)"').Matches[0].Groups[1].Value)
if ($pkgVer -eq $confVer -and $confVer -eq $cargoVer) {
  $Version = $confVer
  Ok "version: $Version (package.json = tauri.conf.json = Cargo.toml)"
} else {
  Die "version mismatch: package.json=$pkgVer tauri.conf.json=$confVer Cargo.toml=$cargoVer"
}
$PubDate = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
$Notes = "Cameo v$Version"
$DownloadBase = "https://r.cameo.ink/release/v$Version"

# -- locate artifacts --------------------------------------------------------
$nsisDir = Join-Path $PSScriptRoot "src-tauri\target\$Target\release\bundle\nsis"
if (-not (Test-Path $nsisDir)) { Die "no Windows bundle at $nsisDir - run .\build_release.ps1 first" }

# Manifests are written into a temp dir then uploaded.
$manifestDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cameo-pub-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null

# Track files we'll upload as { Src; Dst } records.
$uploads = New-Object System.Collections.ArrayList
function Queue($src, $dst) { [void]$uploads.Add([pscustomobject]@{ Src = $src; Dst = $dst }) }

# Sign a payload when the signing key is available; otherwise require a
# pre-existing non-empty .sig. Re-signing avoids pairing a fresh payload with a
# stale signature that happens to have the same filename.
function Ensure-Sig($payload) {
  $sig = "$payload.sig"
  if ($env:TAURI_SIGNING_PRIVATE_KEY) {
    Info "  signing $(Split-Path $payload -Leaf)"
    Remove-Item -LiteralPath $sig -Force -ErrorAction SilentlyContinue
    $keyfile = New-TemporaryFile
    Set-Content -Path $keyfile.FullName -Value $env:TAURI_SIGNING_PRIVATE_KEY -NoNewline
    try {
      if ($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
        pnpm tauri signer sign -f $keyfile.FullName -p $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD $payload | Out-Null
      } else {
        pnpm tauri signer sign -f $keyfile.FullName $payload | Out-Null
      }
    } finally { Remove-Item $keyfile.FullName -ErrorAction SilentlyContinue }
  }
  if ((Test-Path $sig) -and ((Get-Item -LiteralPath $sig).Length -gt 0)) { return $sig }
  return $null
}

function Write-Manifest($name, $payloadFilename, $sigFile) {
  $signature = ''
  if ($sigFile -and (Test-Path $sigFile)) { $signature = (Get-Content $sigFile -Raw).Trim() }
  if (-not $signature) { Die "signature empty for $payloadFilename" }
  $out = Join-Path $manifestDir "$name.json"
  $json = @"
{
  "version": "$Version",
  "notes": "$Notes",
  "pub_date": "$PubDate",
  "signature": "$signature",
  "url": "$DownloadBase/$payloadFilename"
}
"@
  # UTF-8 WITHOUT BOM - a BOM breaks serde_json parsing on the client.
  [System.IO.File]::WriteAllText($out, $json, (New-Object System.Text.UTF8Encoding $false))
  Ok "manifest: $name.json"
  Queue $out "update/$name.json"
}

# GitHub release assets. R2 (above) carries the Tauri auto-UPDATER; the
# open-source DOWNLOAD channel is GitHub Releases (-setup.exe + latest_win.json),
# which cameo_web serves via cameo.ink/update/ (releases/latest/download).
$ghRepo  = "hAcKlyc/cameo"
$ghFiles = @()

# -- Windows scan (x64) ------------------------------------------------------
$zip = Get-ChildItem -Path $nsisDir -Filter '*.nsis.zip' -ErrorAction SilentlyContinue |
  Where-Object { Test-NameHasVersionToken $_.Name $Version } |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
$exe = Get-ChildItem -Path $nsisDir -Filter '*-setup.exe' -ErrorAction SilentlyContinue |
  Where-Object { Test-NameHasVersionToken $_.Name $Version } |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1

if (-not $zip) { Die "no current-version .nsis.zip updater payload in $nsisDir" }
Assert-NameHasVersionToken $zip 'updater payload' $Version
$sig = Ensure-Sig $zip.FullName
if (-not $sig) { Die "updater signature missing: $($zip.FullName).sig" }
$zipName = $zip.Name
Queue $zip.FullName "release/v$Version/$zipName"
Queue $sig "release/v$Version/$zipName.sig"
Write-Manifest "windows-x86_64" $zipName $sig

if (-not $exe) { Die "no current-version NSIS installer in $nsisDir" }
Assert-NameHasVersionToken $exe 'installer' $Version
Queue $exe.FullName "release/v$Version/$($exe.Name)"
$ghFiles += $exe.FullName  # also publish the installer to the GitHub release
Ok "  installer: $($exe.Name)"

# Website download manifest (latest_win.json) -> GitHub release. cameo_web's
# download button fetches cameo.ink/update/latest_win.json (proxying
# releases/latest/download/latest_win.json); url -> the GitHub asset.
$latestWin = Join-Path $manifestDir 'latest_win.json'
$ghDlBase  = "https://github.com/$ghRepo/releases/download/v$Version"
$latestJson = @"
{
  "version": "$Version",
  "pub_date": "$PubDate",
  "release_notes": "$Notes",
  "downloads": {
    "win_x64": { "name": "Windows x64", "url": "$ghDlBase/$($exe.Name)" }
  }
}
"@
[System.IO.File]::WriteAllText($latestWin, $latestJson, (New-Object System.Text.UTF8Encoding $false))
$ghFiles += $latestWin
Ok "manifest: latest_win.json (website download -> GitHub release)"

# -- upload ------------------------------------------------------------------
if ($uploads.Count -eq 0) { Die "no artifacts to upload - did you run .\build_release.ps1 first?" }

Write-Host ""
Info "preparing to upload $($uploads.Count) file(s) to R2 bucket '$($env:R2_BUCKET)':"
foreach ($u in $uploads) {
  Write-Host "    $(Split-Path $u.Src -Leaf) ($(Format-Size $u.Src))"
  Write-Host "         -> r2:$($env:R2_BUCKET)/$($u.Dst)"
}
Write-Host ""

if ($ghFiles.Count -gt 0) {
  Info "GitHub release v$Version will receive $($ghFiles.Count) asset(s):"
  foreach ($f in $ghFiles) { Write-Host "    $(Split-Path $f -Leaf) ($(Format-Size $f))" }
  Write-Host ""
}

if ($DryRun) { Ok "DRY RUN - no upload performed."; exit 0 }

$yn = Read-Host "Proceed with upload? [y/N]"
if ($yn -notmatch '^[Yy]$') { Warn "aborted by user"; exit 0 }

# Configure rclone's ad-hoc `:s3:` backend via env vars. Do NOT use an inline
# connection string (`:s3,endpoint=…:`) — R2_ENDPOINT contains "://" and rclone
# truncates an unquoted value at the first colon, yielding endpoint="https".
$env:RCLONE_S3_PROVIDER          = "Cloudflare"
$env:RCLONE_S3_ACCESS_KEY_ID     = $env:R2_ACCESS_KEY_ID
$env:RCLONE_S3_SECRET_ACCESS_KEY = $env:R2_SECRET_ACCESS_KEY
$env:RCLONE_S3_ENDPOINT          = $env:R2_ENDPOINT
$env:RCLONE_S3_REGION            = "auto"
# R2 tokens can't create buckets; skip rclone's pre-upload bucket check/create
# (the bucket already exists) - otherwise the CreateBucket probe 403s.
$env:RCLONE_S3_NO_CHECK_BUCKET   = "true"
$emptyConf = New-TemporaryFile
try {
  foreach ($u in $uploads) {
    $remote = ":s3:$($env:R2_BUCKET)/$($u.Dst)"
    Info "rclone copyto $(Split-Path $u.Src -Leaf) -> r2:$($env:R2_BUCKET)/$($u.Dst)"
    & $Rclone --config $emptyConf.FullName --progress --stats=1s copyto $u.Src $remote
    if ($LASTEXITCODE -ne 0) { Die "rclone upload failed for $($u.Src)" }
  }
} finally { Remove-Item $emptyConf.FullName -ErrorAction SilentlyContinue }

Write-Host ""
Ok "Windows release v$Version published"
Write-Host ""
Write-Host "  +- live manifest URL ------------------------------------------"
foreach ($u in $uploads) {
  if ($u.Dst -like 'update/*') { Write-Host "     https://r.cameo.ink/$($u.Dst)" }
}
Write-Host "  +--------------------------------------------------------------"
Write-Host ""

# -- GitHub Release (open-source distribution + website download source) ------
# Tag + create (idempotent) and upload the installer + latest_win.json. macOS
# publishes to the SAME release from publish_release.sh (whichever runs first
# creates it; the other uploads with --clobber).
if ((Get-Command gh -ErrorAction SilentlyContinue) -and $ghFiles.Count -gt 0) {
  $tag = "v$Version"
  Info "GitHub release: $ghRepo@$tag"
  & git rev-parse -q --verify "refs/tags/$tag" *> $null
  if ($LASTEXITCODE -ne 0) { & git tag -a $tag -m "Cameo $tag"; & git push origin $tag; Ok "tagged $tag" }
  else { Ok "tag $tag already exists" }
  & gh release view $tag *> $null
  if ($LASTEXITCODE -ne 0) { & gh release create $tag --title "Cameo $tag" --notes "$Notes" --verify-tag; Ok "created GitHub release $tag" }
  Info "uploading $($ghFiles.Count) GitHub release asset(s) - this may take a while"
  & gh release upload $tag @ghFiles --clobber
  if ($LASTEXITCODE -eq 0) { Ok "uploaded $($ghFiles.Count) asset(s) -> download links live at cameo.ink" }
  Write-Host ""
} else {
  Warn "gh CLI missing or no installer - skipped GitHub release; cameo_web download"
  Warn "button stays on the releases page until a release with latest_win.json exists."
  Write-Host ""
}

Warn "REMINDER: bump src-tauri/tauri.conf.json's version BEFORE running this"
Warn "script for the next release, or you'll re-publish v$Version."
Warn "macOS publishes separately - run ./publish_release.sh on the Mac."
