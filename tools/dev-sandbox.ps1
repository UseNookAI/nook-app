<#
.SYNOPSIS
  Runs Nook from source as a sandboxed copy beside an installed one. Ported from the Kotlin
  Nook's tools/dev-sandbox.ps1.

.DESCRIPTION
  A plain dev.ps1 run shares the installed app's home, single-instance lock and gateway port. This
  isolates all three:

    - its own home (NOOK_RS_HOME, default %TEMP%\nook-rs-sandbox, which a packaged shell does not
      redirect), with junctions to the engines and download cache of -EnginesFrom so nothing
      downloads twice; models come read-only from the installed Kotlin Nook as always
      (NOOK_RS_SHARED_MODELS overrides);
    - its own Tauri identifier (ai.nook.app.sandbox), so its single-instance lock and WebView data
      never collide;
    - a pinned gateway port (NOOK_RS_GATEWAY_PORT, default 41510), so it never takes 41434.

.PARAMETER NookHome
  The sandbox home. Created when missing.
.PARAMETER Port
  The gateway port for the sandbox.
.PARAMETER EnginesFrom
  A Nook home whose runtime\bin and runtime\downloads the sandbox links to; none by default.
.PARAMETER UpdateUrl
  A feed or address for the sandbox's updater (NOOK_RS_UPDATE_URL); none by default.
.PARAMETER UsageUrl
  Where the sandbox sends its usage report (NOOK_RS_USAGE_URL), e.g. a local admin server's
  http://127.0.0.1:8790/v1/ping; off by default, so a sandbox never counts as an install.
#>
[CmdletBinding()]
param(
  [string]$NookHome = (Join-Path $env:TEMP 'nook-rs-sandbox'),
  [int]$Port = 41510,
  [string]$EnginesFrom,
  [string]$UpdateUrl,
  [string]$UsageUrl = 'off'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'env.ps1')

foreach ($d in @($NookHome, (Join-Path $NookHome 'data'), (Join-Path $NookHome 'logs'), (Join-Path $NookHome 'runtime'))) {
  New-Item -ItemType Directory -Force -Path $d | Out-Null
}
if ($EnginesFrom) {
  foreach ($d in @('bin', 'downloads')) {
    $link = Join-Path $NookHome "runtime\$d"
    $target = Join-Path $EnginesFrom "runtime\$d"
    if (-not (Test-Path $link) -and (Test-Path $target)) { cmd /c mklink /J "$link" "$target" | Out-Null }
  }
}

$env:NOOK_RS_HOME = $NookHome
$env:NOOK_RS_GATEWAY_PORT = "$Port"
if ($UpdateUrl) { $env:NOOK_RS_UPDATE_URL = $UpdateUrl }
$env:NOOK_RS_USAGE_URL = $UsageUrl

Write-Host "Sandbox home: $NookHome"
Write-Host "Gateway:      http://127.0.0.1:$Port (token in $NookHome\gateway.json)"

Push-Location $repo
try {
  if (-not (Test-Path 'node_modules')) { npm install --no-audit --no-fund }
  if (-not (Test-Path 'ui\node_modules')) { npm --prefix ui install --no-audit --no-fund }
  $conf = Join-Path $NookHome 'sandbox.conf.json'
  [IO.File]::WriteAllText($conf, '{ "identifier": "ai.nook.app.sandbox", "productName": "Nook sandbox" }')
  npx tauri dev --config $conf
} finally { Pop-Location }
