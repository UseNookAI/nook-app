<#
.SYNOPSIS
  Puts the build on the dev channel, or another one the site has, on the stable channel.
  Ported from the Kotlin Nook's tools/promote.ps1.

.DESCRIPTION
  Every build tools/publish.ps1 makes goes to dev first. When one has proved itself there, this makes
  it the stable release: every Nook on stable (the default channel) offers it within fifteen
  minutes. Nothing is built or copied again; only stable/latest.json and its signature change.

    1. takes the dev manifest (or with -Version that build's folder) and checks its signature,
    2. measures the installer on the site and checks it is the one the manifest describes,
    3. checks the build believes the key that signs for the site,
    4. writes and signs stable/latest.json for it with nook-release and sends it,
    5. reads it back and verifies it as the app will.

  Stable never goes back: an app takes only a higher version, so a bad release is fixed with a newer
  one. A stable channel that serves a build from outside this repository is only replaced with -Claim.

.PARAMETER Version
  The version to promote, one of the site's builds; the dev channel's by default.
.PARAMETER Notes
  One line for the update dialog; the dev manifest's, or the commit's subject.
.PARAMETER Claim
  Replace a stable channel that serves a build from outside this repository.
#>
[CmdletBinding()]
param(
  [string]$Version,
  [string]$Feed,
  [string]$HostName,
  [string]$HostRoot,
  [string]$BaseUrl,
  [string]$SshKey,
  [string]$Key,
  [string]$Notes,
  [switch]$Claim
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-lib.ps1')

$site = New-Site -Feed $Feed -HostName $HostName -HostRoot $HostRoot -BaseUrl $BaseUrl -SshKey $SshKey
if (-not $Key) { $Key = $site.Key }
if (-not (Test-Path -LiteralPath $Key)) { throw "No release key at $Key" }
if ($site.Kind -eq 'folder') {
  Assert-NotRedirected $site.Root 'no Nook would ever see this release'
} else {
  if (-not (Test-Path -LiteralPath $site.SshKey)) { throw "No SSH key for $($site.HostName) at $($site.SshKey)" }
  foreach ($tool in 'ssh', 'scp') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "No $tool on the PATH (Windows' OpenSSH client has both)" } }
}

$lock = Enter-SiteLock $site
try {
  Assert-SiteAgrees $site
  $stable = Read-SiteManifest $site 'stable'
  if ($Version) {
    $dirs = @(Get-SiteBuilds $site | Where-Object { $_ -match ('^' + [regex]::Escape($Version) + '-[0-9a-f]{7}$') })
    if ($dirs.Count -eq 0) { throw "$($site.Name) has no build of Nook $Version" }
    if ($dirs.Count -gt 1) { throw "$($site.Name) has more than one build of Nook ${Version}: $($dirs -join ', ')" }
    $commit = ($dirs[0] -split '-', 2)[1]
    $path = "builds/$($dirs[0])/Nook-$Version.exe"
    $info = Get-SiteFileInfo $site $path
    if (-not $info) { throw "$($site.Name) has no $path" }
    $dev = Read-SiteManifest $site 'dev'
    if (-not $PSBoundParameters.ContainsKey('Notes')) {
      $Notes = if ($dev -and $dev.version -eq $Version -and $dev.commit -eq $commit) { $dev.notes }
               elseif (Test-OurCommit $commit) { ([string](Invoke-Git log -1 --format=%s $commit)).Trim() }
               else { '' }
    }
  } else {
    $dev = Read-SiteManifest $site 'dev'
    if (-not $dev) { throw "$($site.Name) has nothing on the dev channel; tools\publish.ps1 puts a build there" }
    # only what this key signed is promoted: the dev manifest is what says which file is which build
    Assert-SignedBy $site 'dev' $Key
    $Version = $dev.version
    $commit = $dev.commit
    $path = "builds/$Version-$commit/$($dev.file)"
    if ($dev.url -ne (Get-SiteUrl $site $path)) { throw "The dev manifest names $($dev.url), which is not where $($site.Name) keeps Nook $Version ($path)" }
    $info = Get-SiteFileInfo $site $path
    if (-not $info) { throw "$($site.Name) has no $path, which the dev manifest names" }
    if ($info.sha256 -ne $dev.sha256 -or $info.size -ne [long]$dev.size) { throw "$path on $($site.Name) is not the installer the dev manifest describes" }
    if (-not $PSBoundParameters.ContainsKey('Notes')) { $Notes = $dev.notes }
  }

  if (-not (Test-OurCommit $commit)) { throw "Nook $Version was built from $commit, which is not a commit of this repository; only its own builds are promoted" }
  if ($stable) {
    if ($stable.version -eq $Version -and $stable.commit -eq $commit) {
      Step "Nook $Version ($commit) is on stable already; nothing to do"
      exit 0
    }
    if (-not (Test-OurCommit $stable.commit) -and -not $Claim) {
      throw ("The stable channel of $($site.Name) serves $($stable.version) from $($stable.commit), which is not a commit of this repository. " +
        "Promoting replaces it for everyone on stable; -Claim does that.")
    }
    if ((Compare-Version $Version $stable.version) -le 0) {
      throw "Nook $Version is not higher than $($stable.version), which is on stable; stable never goes back, so publish a fix as a newer build"
    }
    Assert-SignedBy $site 'stable' $Key
  }

  # The build must believe this key, or whoever takes it would stay on it.
  $keys = Join-Path $env:TEMP "nook-rs-promote-$commit-keys.txt"
  [IO.File]::WriteAllText($keys, ((Invoke-Git show "${commit}:resources/release-keys.txt") -join "`n") + "`n")
  $staging = Join-Path $env:TEMP "nook-rs-promote-$commit-manifests"
  try {
    Assert-BuildTrusts $keys $Key "Nook $Version ($commit)" $site

    Step "Promoting Nook $Version ($commit) to stable on $($site.Name)"
    Remove-Tree $staging
    $argv = @('manifest', $Key, '--channel', 'stable', '--sha256', $info.sha256, '--size', $info.size,
              '--url', (Get-SiteUrl $site $path), '--out', $staging, '--version', $Version, '--commit', $commit)
    if ($Notes) { $argv += @('--notes', [string]$Notes) }
    Invoke-NookRelease @argv | Out-Null
    Send-SiteManifest $site 'stable' $staging
    if (-not (Test-SiteManifest $site 'stable' $keys)) { throw "The stable manifest on $($site.Name) does not verify against $commit's release-keys.txt; the app would ignore it" }
  } finally {
    Remove-Tree $staging
    Remove-Item -LiteralPath $keys -Force -ErrorAction SilentlyContinue
  }

  Step "Nook $Version is the stable release"
  Write-Host "    $(Get-SiteUrl $site 'stable/latest.json')"
  Write-Host "    Nook on stable offers it within fifteen minutes, or at once from Settings > General > Updates > Check."
} finally {
  $lock.ReleaseMutex()
}
