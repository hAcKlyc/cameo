# build_release.ps1 — production Windows build: an NSIS installer (x64).
#
# Cameo bundles no sidecar (it drives the user's own `codex` CLI), so this just
# builds the app and wraps it in a per-user NSIS installer. The installer pulls
# the WebView2 runtime automatically when it is missing.
#
# Usage (PowerShell):
#   .\build_release.ps1            # build the NSIS installer
#   .\build_release.ps1 -Clean     # cargo clean first
#   .\build_release.ps1 -Open      # reveal the installer in Explorer when done
#
# Code signing (optional): Windows signing is configured in
# src-tauri\tauri.conf.json (bundle.windows.certificateThumbprint / signCommand).
# Without it the installer is unsigned and SmartScreen will warn on first run.
#
# macOS: use build_release.sh instead.

param(
  [switch]$Clean,
  [switch]$Open
)

$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot

function Info($m) { Write-Host "-> $m" }
function Ok($m)   { Write-Host "  [ok] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  [!] $m"  -ForegroundColor Yellow }
function Die($m)  { Write-Host "  [x] $m"  -ForegroundColor Red; exit 1 }
function Format-Size($path) {
  $bytes = (Get-Item -LiteralPath $path).Length
  if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
  if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
  if ($bytes -ge 1KB) { return "{0:N1} KB" -f ($bytes / 1KB) }
  return "$bytes B"
}
function Repair-NsisTauriUtilsCache {
  if (-not $env:LOCALAPPDATA) {
    Warn "LOCALAPPDATA is not set - cannot prefetch Tauri's NSIS helper cache"
    return
  }

  $url = 'https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll'
  $expectedSha1 = '75197FEE3C6A814FE035788D1C34EAD39349B860'
  $pluginDir = Join-Path $env:LOCALAPPDATA 'tauri\NSIS\Plugins\x86-unicode\additional'
  $dll = Join-Path $pluginDir 'nsis_tauri_utils.dll'
  $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("nsis_tauri_utils-" + [System.Guid]::NewGuid().ToString('N') + ".dll")

  Info "refreshing Tauri NSIS helper cache"
  New-Item -ItemType Directory -Path $pluginDir -Force | Out-Null
  if (Test-Path $dll) { Remove-Item -LiteralPath $dll -Force -ErrorAction SilentlyContinue }

  for ($attempt = 1; $attempt -le 3; $attempt++) {
    try {
      Info "downloading nsis_tauri_utils.dll (attempt $attempt/3)"
      Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
      if ((Get-Item -LiteralPath $tmp).Length -le 0) { throw "downloaded file is empty" }
      $actualSha1 = (Get-FileHash -LiteralPath $tmp -Algorithm SHA1).Hash.ToUpperInvariant()
      if ($actualSha1 -ne $expectedSha1) { throw "hash mismatch: expected $expectedSha1, got $actualSha1" }
      Move-Item -LiteralPath $tmp -Destination $dll -Force
      Ok "cached nsis_tauri_utils.dll at $dll"
      return
    } catch {
      Warn "download attempt $attempt failed: $($_.Exception.Message)"
      Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
      if ($attempt -lt 3) { Start-Sleep -Seconds (5 * $attempt) }
    }
  }

  Warn "could not prefetch nsis_tauri_utils.dll; retrying Tauri anyway"
}
function Invoke-TauriReleaseBuild {
  Info "pnpm tauri build --target $Target --bundles nsis"
  $env:NODE_OPTIONS = '--max-old-space-size=4096'
  pnpm tauri build --target $Target --bundles nsis
  $script:TauriBuildExitCode = $LASTEXITCODE
}

$Target = 'x86_64-pc-windows-msvc'

if (-not (Get-Command pnpm  -ErrorAction SilentlyContinue)) { Die "pnpm not on PATH (run .\setup.ps1)" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Die "cargo not on PATH (run .\setup.ps1)" }

# -- load .env (KEY=VALUE per line) ------------------------------------------
function Import-DotEnv($path) {
  if (-not (Test-Path $path)) { return }
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

# -- initialize MSVC environment (so cargo finds the linker/assembler) -------
function Initialize-Msvc {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  if (-not (Test-Path $vswhere)) { Warn "vswhere not found - relying on the existing environment"; return }
  $vcPath = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath 2>$null
  if (-not $vcPath) { Warn "VC++ tools not found - relying on the existing environment"; return }
  $vcvars = Join-Path $vcPath 'VC\Auxiliary\Build\vcvars64.bat'
  if (-not (Test-Path $vcvars)) { Warn "vcvars64.bat not found - relying on the existing environment"; return }
  Info "initializing MSVC environment (vcvars64)"
  cmd /c "`"$vcvars`" && set" | ForEach-Object {
    if ($_ -match '^(.*?)=(.*)$') {
      [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
    }
  }
  Ok "MSVC environment loaded"
}
Initialize-Msvc

# -- target ------------------------------------------------------------------
$installed = (rustup target list --installed 2>$null)
if (-not ($installed -contains $Target)) { Info "adding rust target $Target"; rustup target add $Target }

# -- version sanity (package.json / Cargo.toml / tauri.conf.json) ------------
$pkgVer   = (node -p "require('./package.json').version" 2>$null)
$confVer  = (node -p "require('./src-tauri/tauri.conf.json').version" 2>$null)
$cargoVer = ((Select-String -Path 'src-tauri\Cargo.toml' -Pattern '^version\s*=\s*"(.*)"').Matches[0].Groups[1].Value)
if ($pkgVer -eq $confVer -and $confVer -eq $cargoVer) {
  Ok "version $pkgVer (package.json = tauri.conf.json = Cargo.toml)"
} else {
  Warn "version mismatch: package.json=$pkgVer tauri.conf.json=$confVer Cargo.toml=$cargoVer"
}

# -- signing note ------------------------------------------------------------
$thumb = (node -p "require('./src-tauri/tauri.conf.json').bundle?.windows?.certificateThumbprint || ''" 2>$null)
if ($thumb) { Ok "code signing configured (certificateThumbprint)" }
else { Warn "no Windows code-signing configured - installer will be UNSIGNED (SmartScreen will warn)" }

# -- Tauri update signing key (for auto-update payload signatures) ----------
if ($env:TAURI_SIGNING_PRIVATE_KEY) {
  Ok "tauri update signing key present - .exe.sig will be generated for auto-update"
} else {
  Warn "no TAURI_SIGNING_PRIVATE_KEY - .exe.sig will NOT be generated. Auto-update payloads can't be published."
}

if (-not (Test-Path node_modules)) {
  Info "pnpm install (first run)"
  pnpm install
  if ($LASTEXITCODE -ne 0) { Die "pnpm install failed" }
}

if ($Clean) {
  Info "cargo clean + rm dist"
  Push-Location src-tauri; cargo clean; Pop-Location
  if (Test-Path dist) { Remove-Item -Recurse -Force dist }
}

Info "typecheck"
pnpm typecheck
if ($LASTEXITCODE -ne 0) { Die "typecheck failed" }

$start = Get-Date

# -- build NSIS installer + updater payload ----------------------------------
# `nsis` produces the user-visible installer .exe (Cameo_<ver>_x64-setup.exe)
# and, when the updater is configured, the .nsis.zip + .nsis.zip.sig pair that
# tauri-plugin-updater downloads at runtime. Windows Tauri only accepts
# `msi`/`nsis` as bundle names; unlike macOS, there is no separate `updater`
# bundle value to pass here.
$script:TauriBuildExitCode = 0
Invoke-TauriReleaseBuild
$buildCode = $script:TauriBuildExitCode
if ($buildCode -ne 0) {
  $releaseExe = Join-Path $PSScriptRoot "src-tauri\target\$Target\release\cameo.exe"
  if (Test-Path $releaseExe) {
    Warn "tauri build failed after the release exe was produced; NSIS bundling may have hit a corrupt cache or GitHub download timeout"
    Repair-NsisTauriUtilsCache
    Info "retrying tauri build once"
    Invoke-TauriReleaseBuild
    $buildCode = $script:TauriBuildExitCode
  }
  if ($buildCode -ne 0) { Die "tauri build failed" }
}

$nsisDir = Join-Path $PSScriptRoot "src-tauri\target\$Target\release\bundle\nsis"
$installer = Get-ChildItem -Path $nsisDir -Filter '*-setup.exe' -ErrorAction SilentlyContinue |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { Die "no NSIS installer produced in $nsisDir" }

# Tauri places the updater payload alongside the installer (`.nsis.zip`).
$updaterZip = Get-ChildItem -Path $nsisDir -Filter '*.nsis.zip' -ErrorAction SilentlyContinue |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
$updaterSig = if ($updaterZip) {
  Get-ChildItem -Path $nsisDir -Filter "$($updaterZip.Name).sig" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
} else { $null }

$secs = [int]((Get-Date) - $start).TotalSeconds
Write-Host ""
Ok "built in ${secs}s"
Write-Host ""
Write-Host "  +- release artifacts ---------------------------------------"
Write-Host "     installer : $($installer.FullName) ($(Format-Size $installer.FullName))"
if ($updaterZip) {
  Write-Host "     update    : $($updaterZip.FullName) ($(Format-Size $updaterZip.FullName))"
  if ($updaterSig) { Write-Host "               : $($updaterSig.FullName) ($(Format-Size $updaterSig.FullName))" }
  else { Warn "no .nsis.zip.sig - set TAURI_SIGNING_PRIVATE_KEY and re-run for auto-update support" }
} else {
  Warn "no .nsis.zip updater payload produced"
}
Write-Host "  +-----------------------------------------------------------"
Write-Host ""

if ($Open) { Start-Process explorer.exe "/select,`"$($installer.FullName)`"" }
