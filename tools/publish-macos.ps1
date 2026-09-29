<#
.SYNOPSIS
  Publishes a Mac build to the download host (or a test feed): the disk image people download, the
  tarball installed Macs update from, and their signed manifests.

.DESCRIPTION
  The build is a folder as tools/package-macos.sh leaves it (dist-macos/), or the artifact of the
  macOS workflow's run for a tag mac-v<version> (gh run download <run> -D <folder>): the disk image
  Nook-<version>-macos-arm64.dmg, the update Nook-<version>-macos-arm64.app.tar.gz and build.json
  with its version and commit. On the site:

    builds/<version>-<commit>/Nook-<version>-macos-arm64.dmg and .app.tar.gz
    <channel>/macos-arm64/latest.json and latest.json.sig  the Mac app's manifest (nook-release
                                                           manifest --platform macos-arm64)
    <channel>/macos-arm64/download.json                    the disk image's address, version, size
                                                           and SHA-256, for usenook.ai's Download for
                                                           Mac button (Website/tools/set-download.py)

  Windows' <channel>/latest.json is never touched. The manifests are signed with the key the site is
  signed with (the original release key on the host), which every Nook build believes. As for
  Windows: the build must come from this repository, trust that key, and a channel never goes to a
  lower version; one publish at a time.

.PARAMETER Dist
  The folder with the build.
.PARAMETER Channel
  The channels to publish to (dev, stable or both; default both).
.PARAMETER Notes
  What changed, for the update dialog.
.PARAMETER Feed
  Publish to this folder (a test feed) instead of the download host.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Dist,
  [ValidateSet('dev', 'stable')][string[]]$Channel = @('dev', 'stable'),
  [string]$Notes,
  [string]$Feed
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-lib.ps1')
$Platform = 'macos-arm64'

$siteArgs = @{}
if ($Feed) { $siteArgs['Feed'] = $Feed }
$site = New-Site @siteArgs
if (-not $site.Key -or -not (Test-Path -LiteralPath $site.Key)) { throw "No release key for $($site.Name) (tools\release.local.json)" }

# The build: a dist folder, or an artifact folder that holds one.
$Dist = (Resolve-Path -LiteralPath $Dist).Path
$buildJson = Get-ChildItem -LiteralPath $Dist -Recurse -Filter 'build.json' | Select-Object -First 1
if (-not $buildJson) { throw "No build.json in $Dist" }
$dir = $buildJson.DirectoryName
$build = Get-Content -Raw -LiteralPath $buildJson.FullName | ConvertFrom-Json
$version = [string]$build.version
$commit = ([string]$build.commit).Substring(0, [Math]::Min(7, ([string]$build.commit).Length))
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "Not a version: $version" }
$dmgName = "Nook-$version-$Platform.dmg"
$tarName = "Nook-$version-$Platform.app.tar.gz"
$dmg = Join-Path $dir $dmgName
$tar = Join-Path $dir $tarName
foreach ($f in $dmg, $tar) { if (-not (Test-Path -LiteralPath $f)) { throw "Missing: $f" } }
if (-not (Test-OurCommit $commit)) { throw "Commit $commit is not in this repository: not a build Nook made" }
Assert-BuildTrusts (Join-Path $NookRepo 'resources\release-keys.txt') $site.Key "Nook $version for Mac" $site
Assert-SiteAgrees $site

$lock = Enter-SiteLock $site
try {
  foreach ($c in $Channel) {
    $there = Read-SiteManifest $site "$c/$Platform"
    if ($there -and (Compare-Version $there.version $version) -gt 0) {
      throw "$c already has Nook $($there.version) for Mac: it never goes back to $version"
    }
    if ($there) { Assert-SignedBy $site "$c/$Platform" $site.Key }
  }

  $folder = "builds/$version-$commit"
  Step "Sending Nook $version ($commit) for Mac to $($site.Name)/$folder"
  foreach ($f in $dmg, $tar) {
    $name = Split-Path -Leaf $f
    Send-SiteFile $site $f "$folder/$name"
    $info = Get-SiteFileInfo $site "$folder/$name"
    $want = (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant()
    if (-not $info -or $info.sha256 -ne $want) { throw "$name did not arrive whole" }
  }

  $staging = Join-Path $env:TEMP ('nook-macos-publish-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $staging | Out-Null
  try {
    $dmgInfo = [ordered]@{
      version = $version
      commit  = $commit
      url     = Get-SiteUrl $site "$folder/$dmgName"
      size    = (Get-Item -LiteralPath $dmg).Length
      sha256  = (Get-FileHash -LiteralPath $dmg -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $note = if ($Notes) { $Notes } else { "Nook $version for Mac" }
    foreach ($c in $Channel) {
      Step "Signing the $c manifest for Mac"
      Invoke-NookRelease manifest $site.Key --channel $c --installer $tar --url (Get-SiteUrl $site "$folder/$tarName") `
        --out $staging --version $version --commit $commit --notes $note --platform $Platform | Out-Null
      $download = Join-Path $staging "$c\$Platform\download.json"
      [IO.File]::WriteAllText($download, (($dmgInfo | ConvertTo-Json) + "`n"), (New-Object Text.UTF8Encoding($false)))
      Send-SiteFile $site $download "$c/$Platform/download.json"
      Send-SiteManifest $site "$c/$Platform" $staging
      if (-not (Test-SiteManifest $site "$c/$Platform" (Join-Path $NookRepo 'resources\release-keys.txt'))) {
        throw "The $c manifest for Mac on $($site.Name) does not verify as the app checks it"
      }
      Step "$c/$Platform/latest.json: Nook $version ($commit)"
    }
  } finally {
    Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
  }
  Step "Download: $($dmgInfo.url)"
} finally {
  $lock.ReleaseMutex()
}
