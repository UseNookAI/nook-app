<#
.SYNOPSIS
  Builds Nook from a commit and publishes it to the dev channel of an update site; with -Stable,
  to stable as well. Ported from the Kotlin Nook's tools/publish.ps1.

.DESCRIPTION
  A Nook build reads <base>/<channel>/latest.json at start and then every minute on the dev
  channel (or from a folder feed), every fifteen on stable. This puts a new build there:

    1. checks out the commit into a clean worktree under %TEMP%, so uncommitted work never ships,
    2. checks that the build will believe the key that signs for the site,
    3. with -Test, runs the checks there (rustfmt, clippy, the Rust and UI tests, the typecheck) and
       publishes nothing if one fails,
    4. builds the NSIS installer there with the next version number, stamped with the site's base
       so the build follows it (NOOK_RS_UPDATE_BASE),
    5. copies it to <site>/builds/<version>-<commit>/Nook-<version>.exe,
    6. writes and signs dev/latest.json (and with -Stable stable/latest.json) with nook-release,
    7. reads them back and verifies them as the app will,
    8. keeps the newest -Keep of this repository's builds on the site and removes the worktree.

  The site is the download host every Nook updates from, https://dl.usenook.ai/nook, signed with
  the original release key that every Nook installed from it believes; -Feed publishes to a folder
  instead (a test feed, signed with this repository's own key). Until this repository first
  publishes there, its channels serve the Kotlin Nook's builds, which -Claim replaces.

  The next version is the workspace's (Cargo.toml) when that is higher than every version the site
  serves, else the highest one with its patch number plus one, so each build is newer than all
  before it. A channel that serves a build from outside this repository is only replaced with -Claim.

  Needs: git, node/npm, the Rust toolchain (tools\env.ps1 finds the portable one), and the release
  key, which stays outside every repository. Tauri downloads the NSIS tools on its first bundle.

.PARAMETER Ref
  The commit to build; HEAD by default.
.PARAMETER Stable
  Publish to the stable channel as well as dev.
.PARAMETER Feed
  Publish to this folder (a test feed) instead of the download host.
.PARAMETER Key
  The release signing key: the original key for the host, this repository's own for a folder.
.PARAMETER Version
  A version to publish instead of the next one; it must be higher than every one the site serves.
.PARAMETER Notes
  One line for the update dialog; the commit's subject by default.
.PARAMETER Keep
  How many of this repository's builds to keep on the site; the ones a manifest names are always kept.
.PARAMETER Force
  Publish even when the channels already serve this commit.
.PARAMETER Test
  Run the checks on the commit first, and publish nothing if any of them fail.
.PARAMETER Claim
  Replace a channel that serves a build from outside this repository.
#>
[CmdletBinding()]
param(
  [string]$Ref = 'HEAD',
  [switch]$Stable,
  [string]$Feed,
  [string]$HostName,
  [string]$HostRoot,
  [string]$BaseUrl,
  [string]$SshKey,
  [string]$Key,
  [string]$Version,
  [string]$Notes = '',
  [int]$Keep = 5,
  [switch]$Force,
  [switch]$Test,
  [switch]$Claim
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-lib.ps1')
$parent = Split-Path -Parent $NookRepo
$channels = @('dev') + @(if ($Stable) { 'stable' })

$site = New-Site -Feed $Feed -HostName $HostName -HostRoot $HostRoot -BaseUrl $BaseUrl -SshKey $SshKey
if (-not $Key) { $Key = $site.Key }
if (-not (Test-Path -LiteralPath $Key)) { throw "No release key at $Key (tools\release keygen makes one; keep it outside the repository)" }
if ($site.Kind -eq 'folder') {
  Assert-NotRedirected $site.Root 'no Nook would ever see this publish'
} else {
  if (-not (Test-Path -LiteralPath $site.SshKey)) { throw "No SSH key for $($site.HostName) at $($site.SshKey)" }
  foreach ($tool in 'ssh', 'scp') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "No $tool on the PATH (Windows' OpenSSH client has both)" } }
}
foreach ($tool in 'git', 'npm', 'cargo') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "No $tool on the PATH" } }

$full = ([string](Invoke-Git rev-parse --verify "$Ref^{commit}")).Trim()
$sha = $full.Substring(0, 7)
if ($Ref -eq 'HEAD' -and (Invoke-Git status --porcelain)) {
  Write-Warning "The working tree has uncommitted changes; they are not in this build, which is made from $sha."
}
if (-not $PSBoundParameters.ContainsKey('Notes')) { $Notes = ([string](Invoke-Git log -1 --format=%s $full)).Trim() }

# Builds share one target folder beside the repository, so each publish compiles only what changed.
$target = if ($env:NOOK_RS_PUBLISH_TARGET) { $env:NOOK_RS_PUBLISH_TARGET } else { Join-Path $parent 'nook-rs-target-publish' }

$locks = @(Enter-SiteLock $site)
try {
  Step "Reading what $($site.Name) serves"
  Assert-SiteAgrees $site
  $now = @{}
  foreach ($c in 'stable', 'dev') { $now[$c] = Read-SiteManifest $site $c }

  foreach ($c in $channels) {
    $m = $now[$c]
    if ($m -and -not (Test-OurCommit $m.commit) -and -not $Claim) {
      throw ("The $c channel of $($site.Name) serves $($m.version) from $($m.commit), which is not a commit of this repository. " +
        "Publishing replaces it for everyone who follows that channel; -Claim does that.")
    }
  }

  $done = @($channels | Where-Object { -not $now[$_] -or $now[$_].commit -ne $sha }).Count -eq 0
  if ($done -and -not $Force -and -not $Version) {
    Step "$sha is already on $($channels -join ' and ') as $($now['dev'].version); nothing to do (-Force publishes it again as a new version)"
    exit 0
  }
  if ($Stable -and $now['dev'] -and $now['dev'].commit -eq $sha -and -not $Force -and -not $Version) {
    throw "$sha is on dev as $($now['dev'].version) already: tools\promote.ps1 puts that build on stable without building it again"
  }

  $last = $null
  foreach ($m in @($now.Values)) {
    if ($m -and (-not $last -or (Compare-Version $m.version $last.version) -gt 0)) { $last = $m }
  }
  $cargoToml = (Invoke-Git show "${full}:Cargo.toml") -join "`n"
  $base = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"').Groups[1].Value
  if (-not $base) { throw "No [workspace.package] version in Cargo.toml at $sha" }
  if ($Version) {
    if ($last -and (Compare-Version $Version $last.version) -le 0) {
      throw "$Version is not higher than $($last.version), which is published already; the app would not take it"
    }
  } elseif (-not $last -or (Compare-Version $base $last.version) -gt 0) {
    $Version = $base
  } else {
    $p = @($last.version.Split('.'))
    while ($p.Count -lt 3) { $p += '0' }
    $p[$p.Count - 1] = [string]([int]($p[$p.Count - 1] -replace '[^0-9].*$', '') + 1)
    $Version = $p -join '.'
  }

  # Whoever follows a channel now believes the key that signed it; this one must be that key.
  foreach ($c in $channels) { Assert-SignedBy $site $c $Key }

  Step "Publishing Nook $Version from $sha to $($channels -join ' and ') on $($site.Name)"
  $worktree = Join-Path $env:TEMP "nook-rs-publish-$sha"
  Remove-Tree $worktree
  Invoke-Git worktree prune
  Invoke-Git worktree add --quiet --detach $worktree $full

  try {
    $keys = Join-Path $worktree 'resources\release-keys.txt'
    Assert-BuildTrusts $keys $Key "Nook $Version ($sha)" $site

    $env:CARGO_TARGET_DIR = $target
    $env:NOOK_VERSION = $Version
    $env:NOOK_COMMIT = $sha
    $env:NOOK_RS_UPDATE_BASE = $site.BaseUrl
    # stderr is merged inside cmd: PowerShell 5.1 would turn cargo's and npm's progress into errors.
    function Invoke-InTree([string]$what, [string]$command) {
      cmd /c "cd /d `"$worktree`" && ($command) 2>&1"
      if ($LASTEXITCODE -ne 0) { throw "$what failed (exit code $LASTEXITCODE, reasons above), so nothing was published" }
    }

    Step "Installing the npm packages"
    Invoke-InTree 'npm ci' 'npm ci --no-audit --no-fund && npm --prefix ui ci --no-audit --no-fund'

    if ($Test) {
      Step "Testing: rustfmt, clippy, the Rust tests, the UI typecheck and tests (a few minutes)"
      Invoke-InTree 'rustfmt' 'cargo fmt --all -- --check'
      Invoke-InTree 'The UI typecheck' 'npm --prefix ui run typecheck'
      Invoke-InTree 'The UI tests' 'npm --prefix ui test'
      Invoke-InTree 'clippy' 'cargo clippy --workspace --all-targets -- -D warnings'
      Invoke-InTree 'The Rust tests' 'cargo test --workspace'
    }

    Step "Building the installer (a few minutes)"
    [IO.File]::WriteAllText((Join-Path $worktree 'publish.conf.json'), "{ `"version`": `"$Version`" }")
    Invoke-InTree 'The installer build' 'npx tauri build --ci --config publish.conf.json'
    $bundle = Join-Path $target 'release\bundle\nsis'
    $built = Get-ChildItem -LiteralPath $bundle -Filter "*_${Version}_x64-setup.exe" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $built) { throw "No installer for $Version in $bundle" }
    $name = "Nook-$Version.exe"
    $staged = Join-Path $worktree $name
    Copy-Item -LiteralPath $built.FullName $staged -Force
    $size = (Get-Item -LiteralPath $staged).Length
    $hash = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash.ToLowerInvariant()
    $path = "builds/$Version-$sha/$name"

    $there = Get-SiteFileInfo $site $path
    if ($there -and $there.sha256 -eq $hash) {
      Step "$path is on $($site.Name) already"
    } else {
      $free = Get-SiteFree $site
      if ($free -lt 2 * $size) {
        throw "$($site.Name) has $([Math]::Round($free / 1MB)) MB free, too little for a $([Math]::Round($size / 1MB)) MB installer; remove old builds there first"
      }
      Step "Copying $name ($([Math]::Round($size / 1MB, 1)) MB) to $($site.Name)"
      Send-SiteFile $site $staged $path
      $there = Get-SiteFileInfo $site $path
      if (-not $there -or $there.sha256 -ne $hash -or $there.size -ne $size) { throw "$path on $($site.Name) is not the installer that was built" }
    }
    $url = Get-SiteUrl $site $path

    Step "Signing and sending the manifests"
    $staging = Join-Path $env:TEMP "nook-rs-publish-$sha-manifests"
    Remove-Tree $staging
    $published = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    foreach ($c in $channels) {
      $argv = @('manifest', $Key, '--channel', $c, '--installer', $staged, '--url', $url, '--out', $staging,
                '--version', $Version, '--commit', $sha, '--published', $published)
      if ($Notes) { $argv += @('--notes', $Notes) }
      Invoke-NookRelease @argv | Out-Null
    }
    foreach ($c in $channels) { Send-SiteManifest $site $c $staging }
    Remove-Tree $staging

    # What the app will do: read each manifest as it does and check it against the keys the new
    # build carries.
    foreach ($c in $channels) {
      if (-not (Test-SiteManifest $site $c $keys)) { throw "The $c manifest on $($site.Name) does not verify against $sha's release-keys.txt; the app would ignore it" }
    }
  } finally {
    Remove-Tree $worktree
    Invoke-Git worktree prune
  }

  Remove-OldBuilds $site $Keep

  Step "Published Nook $Version ($sha, $([Math]::Round($size / 1MB, 1)) MB) to $($channels -join ' and ')"
  Write-Host "    $url"
  foreach ($c in $channels) { Write-Host "    $(Get-SiteUrl $site "$c/latest.json")" }
  if (-not $Stable) { Write-Host "    Nook builds on the dev channel install it by themselves; tools\promote.ps1 puts it on stable." }
} finally {
  foreach ($l in $locks) { $l.ReleaseMutex() }
}
