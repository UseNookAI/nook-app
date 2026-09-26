<#
.SYNOPSIS
  Builds the per-user installer from the working tree: target\release\bundle\nsis\Nook_<version>_x64-setup.exe.

.DESCRIPTION
  The same build tools\publish.ps1 makes from a clean worktree, but from this checkout as it is,
  uncommitted work included, and published nowhere. Its updater follows NOOK_RS_UPDATE_BASE when
  that is set, and nothing otherwise.
#>
[CmdletBinding()]
param([string]$Version)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'env.ps1')

Push-Location $repo
try {
  if (-not (Test-Path 'node_modules')) { npm install --no-audit --no-fund }
  if (-not (Test-Path 'ui\node_modules')) { npm --prefix ui install --no-audit --no-fund }
  if ($Version) {
    $env:NOOK_VERSION = $Version
    $conf = Join-Path $env:TEMP 'nook-rs-package.conf.json'
    [IO.File]::WriteAllText($conf, "{ `"version`": `"$Version`" }")
    npx tauri build --config $conf
  } else {
    npx tauri build
  }
  if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
  $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
  Get-ChildItem (Join-Path $target 'release\bundle\nsis\*.exe') | Sort-Object LastWriteTime -Descending | Select-Object -First 1 |
    ForEach-Object { Write-Host "[SUCCESS] Installer written to $($_.FullName)" -ForegroundColor Green }
} finally { Pop-Location }
