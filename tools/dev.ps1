<#
.SYNOPSIS
  Runs Nook from source with the UI hot-reloading (tauri dev), on its own home.

.DESCRIPTION
  Uses the portable Rust toolchain (tools\env.ps1) and the npm packages at the root and in ui\
  (installed on the first run). The app uses its normal home, %LOCALAPPDATA%\Nook-rs, and shares
  its single-instance lock with an installed Nook; tools\dev-sandbox.ps1 runs one beside it.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'env.ps1')

Push-Location $repo
try {
  if (-not (Test-Path 'node_modules')) { npm install --no-audit --no-fund }
  if (-not (Test-Path 'ui\node_modules')) { npm --prefix ui install --no-audit --no-fund }
  npx tauri dev
} finally { Pop-Location }
