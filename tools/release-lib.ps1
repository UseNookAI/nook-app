<#
  What tools/publish.ps1, tools/promote.ps1 and tools/pipeline.ps1 share; they dot-source it.
  Ported from the Kotlin Nook's tools/release-lib.ps1.

  Builds are published to a site, laid out as the app reads it:

    <base>/<channel>/latest.json and latest.json.sig            each channel's signed manifest
    <base>/builds/<version>-<commit>/Nook-<version>.exe  the installers

  A site is the download host every Nook updates from (the default), written over SSH and read over
  HTTPS as the app reads it, or a folder on this machine (-Feed): a test feed. Another host can be
  named with -HostName, -HostRoot, -BaseUrl and -SshKey. Every file goes in under a .part name and
  is then renamed, so nothing half written is ever served.

  Every Nook ever installed from the host, the Kotlin ones included, believes only what the
  original release key signs, so that key signs everything there; a folder is signed with this
  repository's own key. Both keys and the host's SSH key stay outside every repository. Manifests
  are signed by crates/nook-release, the same code the app verifies with.
#>

# The download host behind https://dl.usenook.ai/nook, and where its SSH key and the release keys
# are, come from tools\release.local.json (see release.local.example.json). That file stays on
# the machine that publishes (.gitignore), so the host's address and the keys' places are in no
# repository.
$NookLocalFile = Join-Path $PSScriptRoot 'release.local.json'
$NookLocal = if (Test-Path -LiteralPath $NookLocalFile) { Get-Content -Raw -LiteralPath $NookLocalFile | ConvertFrom-Json } else { $null }
$NookHost = @{
  HostName = $(if ($NookLocal) { $NookLocal.hostName })
  HostRoot = $(if ($NookLocal -and $NookLocal.hostRoot) { $NookLocal.hostRoot } else { '/srv/dl/nook' })
  BaseUrl  = 'https://dl.usenook.ai/nook'
  SshKey   = $(if ($NookLocal) { $NookLocal.sshKey })
  Key      = $(if ($NookLocal) { $NookLocal.releaseKey })
}
# A folder feed (tests) is signed with this repository's own key.
$NookFolderKey = if ($env:NOOK_RS_RELEASE_KEY_FILE) { $env:NOOK_RS_RELEASE_KEY_FILE } elseif ($NookLocal) { $NookLocal.folderKey } else { '' }
$NookTools = $PSScriptRoot
$NookRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'env.ps1')

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Green }

function Compare-Version([string]$a, [string]$b) {
  $x = @($a.Split('.') | ForEach-Object { [int]($_ -replace '[^0-9].*$', '') })
  $y = @($b.Split('.') | ForEach-Object { [int]($_ -replace '[^0-9].*$', '') })
  for ($i = 0; $i -lt [Math]::Max($x.Count, $y.Count); $i++) {
    $p = if ($i -lt $x.Count) { $x[$i] } else { 0 }
    $q = if ($i -lt $y.Count) { $y[$i] } else { 0 }
    if ($p -ne $q) { return [Math]::Sign($p - $q) }
  }
  return 0
}

function Invoke-Git {
  # Git's warnings (line endings and the like) go to stderr, which PowerShell 5.1 turns into a
  # terminating error when the output is redirected; the exit code is what counts.
  $ErrorActionPreference = 'Continue'
  $out = & git -C $NookRepo @args 2>$null
  if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed with exit code $LASTEXITCODE" }
  $out
}

function Test-OurCommit([string]$commit) {
  # True when the commit is in this repository: a build Nook published, not someone else's.
  if ($commit -notmatch '^[0-9a-f]{7,40}$') { return $false }
  $ErrorActionPreference = 'Continue'
  & git -C $NookRepo rev-parse --verify --quiet "$commit^{commit}" 2>$null | Out-Null
  $LASTEXITCODE -eq 0
}

function Remove-Tree([string]$path) {
  # rmdir through the \\?\ prefix: the build output under a worktree is deeper than MAX_PATH.
  if (-not (Test-Path -LiteralPath $path)) { return }
  cmd /c "rmdir /s /q `"\\?\$path`" 2>&1" | Out-Null
  if (Test-Path -LiteralPath $path) { Write-Warning "Could not remove $path; delete it by hand" }
}

function Assert-NotRedirected([string]$path, [string]$consequence) {
  # A shell that a packaged app started (the Claude desktop app is one) has what it writes under
  # %LOCALAPPDATA% kept in that app's private copy, Packages\<app>\LocalCache\Local, which nothing
  # else reads. A throwaway folder shows which it is.
  if (-not $env:LOCALAPPDATA -or -not $path.StartsWith($env:LOCALAPPDATA, [StringComparison]::OrdinalIgnoreCase)) { return }
  $probe = 'nook-publish-probe-' + [guid]::NewGuid().ToString('N')
  New-Item -ItemType Directory -Path (Join-Path $env:LOCALAPPDATA $probe) | Out-Null
  $owner = Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Packages') -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "LocalCache\Local\$probe") } | Select-Object -First 1
  Remove-Item -LiteralPath (Join-Path $env:LOCALAPPDATA $probe)
  if ($owner) {
    throw "This shell runs inside the packaged app $($owner.Name), which keeps what it writes under %LOCALAPPDATA% to itself: $consequence. Run it from your own terminal."
  }
}

function Get-NookRelease {
  # The release tool, built from this repository once per run (release profile, shared target).
  if ($script:NookReleaseExe -and (Test-Path -LiteralPath $script:NookReleaseExe)) { return $script:NookReleaseExe }
  $target = if ($env:NOOK_RS_TOOLS_TARGET) { $env:NOOK_RS_TOOLS_TARGET } else { Join-Path (Split-Path -Parent $NookRepo) 'nook-rs-target-tools' }
  $ErrorActionPreference = 'Continue'
  & cargo build -q --release -p nook-release --manifest-path (Join-Path $NookRepo 'Cargo.toml') --target-dir $target 2>&1 | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "Could not build nook-release (cargo exit code $LASTEXITCODE)" }
  $script:NookReleaseExe = Join-Path $target 'release\nook-release.exe'
  $script:NookReleaseExe
}

function Invoke-NookRelease {
  $exe = Get-NookRelease
  $ErrorActionPreference = 'Continue'
  $out = & $exe @args 2>&1
  if ($LASTEXITCODE -ne 0) { throw "nook-release $($args[0]) failed: $($out -join ' ')" }
  $out
}

function New-Site {
  param([string]$Feed, [string]$HostName, [string]$HostRoot, [string]$BaseUrl, [string]$SshKey)
  if ($Feed) {
    $root = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Feed).TrimEnd('\')
    return [pscustomobject]@{ Kind = 'folder'; Name = $root; Root = $root; BaseUrl = ([Uri]$root).AbsoluteUri.TrimEnd('/'); Key = $NookFolderKey }
  }
  if (-not $HostName -and -not $NookHost.HostName) {
    throw "No download host: copy tools\release.local.example.json to tools\release.local.json and fill it in, or pass -HostName and -SshKey"
  }
  $site = [pscustomobject]@{
    Kind     = 'host'
    Name     = ''
    HostName = $(if ($HostName) { $HostName } else { $NookHost.HostName })
    Root     = $(if ($HostRoot) { $HostRoot.TrimEnd('/') } else { $NookHost.HostRoot })
    BaseUrl  = $(if ($BaseUrl) { $BaseUrl.TrimEnd('/') } else { $NookHost.BaseUrl })
    SshKey   = $(if ($SshKey) { $SshKey } else { $NookHost.SshKey })
    Key      = $NookHost.Key
  }
  $site.Name = $site.BaseUrl
  # the host's paths go into shell commands between single quotes
  if ($site.Root -match "'") { throw "The host folder may not contain a single quote: $($site.Root)" }
  $site
}

function Get-SitePath($site, [string]$path) {
  # where a file of the site is: a path on this machine for a folder, on the host otherwise
  if ($site.Kind -eq 'folder') { return (Join-Path $site.Root ($path -replace '/', '\')) }
  "$($site.Root)/$path"
}

function Get-SiteUrl($site, [string]$path) {
  # the address the app downloads a file of the site from
  if ($site.Kind -eq 'folder') { return ([Uri](Get-SitePath $site $path)).AbsoluteUri }
  "$($site.BaseUrl)/$path"
}

function Format-NativeArgument([string]$a) {
  # One argument on a Windows command line: quoted when it has a space or a quote in it
  if ($a -ne '' -and $a -notmatch '[\s"]') { return $a }
  '"' + ($a -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-Bounded([string]$program, [string[]]$arguments, [int]$seconds) {
  # Runs a program with a limit on its time: its exit code (-1 when it had to be ended), stdout and
  # stderr. ssh's own timeouts cover the connecting and a peer that stops answering its keepalives,
  # not a session that stalls in between (the Kotlin Nook's publish once waited 20 minutes on a
  # "mkdir -p" after an upload, holding the publish lock).
  $psi = New-Object Diagnostics.ProcessStartInfo
  $psi.FileName = (Get-Command $program -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
  $psi.Arguments = ($arguments | ForEach-Object { Format-NativeArgument $_ }) -join ' '
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $p = [Diagnostics.Process]::Start($psi)
  $p.StandardInput.Close()
  $out = $p.StandardOutput.ReadToEndAsync()
  $err = $p.StandardError.ReadToEndAsync()
  if (-not $p.WaitForExit($seconds * 1000)) {
    try { $p.Kill() } catch { }
    [void]$p.WaitForExit(10000)
    return [pscustomobject]@{ Code = -1; Out = ''; Err = "no end within $seconds s, so it was stopped" }
  }
  $p.WaitForExit()
  [pscustomobject]@{ Code = $p.ExitCode; Out = $out.Result; Err = $err.Result.Trim() }
}

$SshOptions = @('-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new', '-o', 'ConnectTimeout=30',
  '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4')

function Invoke-SiteSsh($site, [string]$command, [int]$seconds = 120, [int]$tries = 3) {
  # The output lines. Each command here is safe to repeat, so a try that stalls or loses its
  # connection (ssh's exit code 255) is made again.
  $argv = @('-i', $site.SshKey) + $SshOptions + @($site.HostName, $command)
  for ($i = 1; $i -le $tries; $i++) {
    $r = Invoke-Bounded 'ssh' $argv $seconds
    if ($r.Code -eq 0) {
      $lines = @($r.Out -split "`r?`n")
      if ($lines.Count -gt 0 -and $lines[-1] -eq '') { $lines = @($lines | Select-Object -First ($lines.Count - 1)) }
      return $lines
    }
    if ($r.Code -ne -1 -and $r.Code -ne 255) { break }
    if ($i -lt $tries) {
      Write-Warning "ssh $($site.HostName) did not answer (try $i of $tries): $($r.Err)"
      Start-Sleep -Seconds (10 * $i)
    }
  }
  throw "ssh $($site.HostName) failed with exit code $($r.Code) on: $command $($r.Err)"
}

function Copy-SiteUp($site, [string]$from, [string]$to, [int]$tries = 3) {
  # scp, tried again after a dropped connection or a stall: five minutes plus a second per 20 kB
  $seconds = 300 + [int]((Get-Item -LiteralPath $from).Length / 20KB)
  $argv = @('-i', $site.SshKey) + $SshOptions + @('-q', $from, "$($site.HostName):$to")
  for ($i = 1; $i -le $tries; $i++) {
    $r = Invoke-Bounded 'scp' $argv $seconds
    if ($r.Code -eq 0) { return }
    Write-Warning "Copying to $($site.HostName) failed (try $i of $tries): $($r.Err)"
    if ($i -lt $tries) { Start-Sleep -Seconds (15 * $i) }
  }
  throw "Could not copy $from to $($site.HostName):$to"
}

function Save-SiteFile($site, [string]$path, [string]$to) {
  # Copies a file of the site to $to, read as the app reads it; $false when the site has none.
  if (Test-Path -LiteralPath $to) { Remove-Item -LiteralPath $to -Force }
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $to) | Out-Null
  if ($site.Kind -eq 'folder') {
    $from = Get-SitePath $site $path
    if (-not (Test-Path -LiteralPath $from)) { return $false }
    Copy-Item -LiteralPath $from $to -Force
    return $true
  }
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  $web = New-Object Net.WebClient
  $web.Headers['User-Agent'] = 'Nook-publish'
  $web.Headers['Cache-Control'] = 'no-cache'
  try {
    # the query only keeps any cache in between from answering with an older copy
    $web.DownloadFile("$($site.BaseUrl)/${path}?fresh=$([DateTime]::UtcNow.Ticks)", $to)
    return $true
  } catch {
    $e = $_.Exception
    while ($e -and $e -isnot [Net.WebException]) { $e = $e.InnerException }
    if ($e -and $e.Response -and [int]$e.Response.StatusCode -eq 404) {
      if (Test-Path -LiteralPath $to) { Remove-Item -LiteralPath $to -Force }
      return $false
    }
    throw "Could not read $($site.BaseUrl)/${path}: $($_.Exception.Message)"
  } finally {
    $web.Dispose()
  }
}

function Read-SiteManifest($site, [string]$channel) {
  # A channel's latest.json as the app reads it, or $null when the channel has none.
  $tmp = Join-Path $env:TEMP ('nook-site-' + [guid]::NewGuid().ToString('N') + '.json')
  try {
    if (-not (Save-SiteFile $site "$channel/latest.json" $tmp)) { return $null }
    [IO.File]::ReadAllText($tmp, [Text.Encoding]::UTF8) | ConvertFrom-Json
  } finally {
    if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
  }
}

function Test-SiteManifest($site, [string]$channel, [string]$keys) {
  # What the app does before it believes a manifest: read it and its signature, and check the one
  # against the keys in a release-keys.txt (or against one public key).
  $dir = Join-Path $env:TEMP ('nook-site-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $dir | Out-Null
  try {
    $m = Join-Path $dir "$channel\latest.json"
    if (-not (Save-SiteFile $site "$channel/latest.json" $m) -or -not (Save-SiteFile $site "$channel/latest.json.sig" "$m.sig")) { return $false }
    $exe = Get-NookRelease
    $ErrorActionPreference = 'Continue'
    & $exe verify $keys $m 2>$null | Out-Null
    $LASTEXITCODE -eq 0
  } finally {
    Remove-Item -LiteralPath $dir -Recurse -Force
  }
}

function Send-SiteFile($site, [string]$from, [string]$path) {
  $to = Get-SitePath $site $path
  if ($site.Kind -eq 'folder') {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $to) | Out-Null
    Copy-Item -LiteralPath $from "$to.part" -Force
    Move-Item -LiteralPath "$to.part" $to -Force
    return
  }
  $dir = $to.Substring(0, $to.LastIndexOf('/'))
  Invoke-SiteSsh $site "mkdir -p '$dir'" | Out-Null
  Copy-SiteUp $site $from "$to.part"
  Invoke-SiteSsh $site "chmod 644 '$to.part' && mv -f '$to.part' '$to'" | Out-Null
}

function Send-SiteManifest($site, [string]$channel, [string]$staging) {
  # A channel's latest.json and its signature from $staging\<channel>: the signature goes in first,
  # then the manifest, each by a rename. An app that reads between the two finds a manifest that
  # does not verify, ignores it and asks again later.
  $json = Join-Path $staging "$channel\latest.json"
  if ($site.Kind -eq 'folder') {
    $dir = Join-Path $site.Root $channel
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item -LiteralPath "$json.sig" (Join-Path $dir 'latest.json.sig.part') -Force
    Copy-Item -LiteralPath $json (Join-Path $dir 'latest.json.part') -Force
    Move-Item -LiteralPath (Join-Path $dir 'latest.json.sig.part') (Join-Path $dir 'latest.json.sig') -Force
    Move-Item -LiteralPath (Join-Path $dir 'latest.json.part') (Join-Path $dir 'latest.json') -Force
    return
  }
  $dir = "$($site.Root)/$channel"
  Invoke-SiteSsh $site "mkdir -p '$dir'" | Out-Null
  Copy-SiteUp $site "$json.sig" "$dir/latest.json.sig.part"
  Copy-SiteUp $site $json "$dir/latest.json.part"
  Invoke-SiteSsh $site ("cd '$dir' && chmod 644 latest.json.sig.part latest.json.part && " +
    "mv -f latest.json.sig.part latest.json.sig && mv -f latest.json.part latest.json") | Out-Null
}

function Get-SiteFileInfo($site, [string]$path) {
  # The sha256 and size of a file on the site, or $null when it is not there.
  $p = Get-SitePath $site $path
  if ($site.Kind -eq 'folder') {
    if (-not (Test-Path -LiteralPath $p)) { return $null }
    return [pscustomobject]@{ sha256 = (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant(); size = (Get-Item -LiteralPath $p).Length }
  }
  $out = @(Invoke-SiteSsh $site "if test -f '$p'; then sha256sum '$p' | cut -d' ' -f1; wc -c < '$p'; fi")
  if ($out.Count -lt 2) { return $null }
  [pscustomobject]@{ sha256 = $out[0].Trim().ToLowerInvariant(); size = [long]$out[1].Trim() }
}

function Get-SiteFree($site) {
  # Bytes free where the builds go.
  if ($site.Kind -eq 'folder') {
    New-Item -ItemType Directory -Force -Path (Join-Path $site.Root 'builds') | Out-Null
    return [long](Get-PSDrive -Name (Split-Path -Qualifier $site.Root).TrimEnd(':')).Free
  }
  $out = @(Invoke-SiteSsh $site "mkdir -p '$($site.Root)/builds' && df -Pk '$($site.Root)/builds' | tail -n 1")
  [long](($out[-1].Trim() -split '\s+')[3]) * 1024
}

function Get-SiteBuilds($site) {
  # The names of the build folders, <version>-<commit>.
  if ($site.Kind -eq 'folder') {
    $dir = Join-Path $site.Root 'builds'
    if (-not (Test-Path -LiteralPath $dir)) { return @() }
    return @(Get-ChildItem -LiteralPath $dir -Directory | ForEach-Object { $_.Name })
  }
  @(Invoke-SiteSsh $site "ls -1 '$($site.Root)/builds' 2>/dev/null || true" | Where-Object { $_ })
}

function Remove-OldBuilds($site, [int]$keep) {
  # This repository's builds beyond the newest $keep go, except any a manifest names. Builds from
  # outside this repository are left to whoever published them.
  $named = @(foreach ($c in 'stable', 'dev') { $m = Read-SiteManifest $site $c; if ($m) { "$($m.version)-$($m.commit)" } })
  $ours = @(Get-SiteBuilds $site | Where-Object { $_ -match '^\d+(\.\d+)*-[0-9a-f]{7}$' -and (Test-OurCommit ($_ -split '-', 2)[1]) })
  $byVersion = { (($_ -split '-')[0].Split('.') | ForEach-Object { '{0:D6}' -f [int]$_ }) -join '.' }
  $ours | Sort-Object -Property @{ Expression = $byVersion; Descending = $true } | Select-Object -Skip $keep |
    Where-Object { $named -notcontains $_ } | ForEach-Object {
    Write-Host "    removing the old build $_"
    if ($site.Kind -eq 'folder') { Remove-Tree (Get-SitePath $site "builds/$_") }
    else { Invoke-SiteSsh $site "rm -rf '$($site.Root)/builds/$_'" | Out-Null }
  }
}

function Get-PublicKey([string]$key) {
  $public = ([string](Invoke-NookRelease public $key | Select-Object -Last 1)).Trim()
  # the line is "<key>   # comment" when the tool prints a release-keys.txt line
  $public = ($public -split '\s+', 2)[0]
  if (-not $public) { throw "Could not read the release key $key" }
  $public
}

function Assert-SignedBy($site, [string]$channel, [string]$key) {
  # The Nooks that follow a channel believe whoever signed what it serves now; a manifest signed
  # with another key would be ignored by every one of them. A channel with nothing in it passes.
  if (-not (Read-SiteManifest $site $channel)) { return }
  if (-not (Test-SiteManifest $site $channel (Get-PublicKey $key))) {
    throw "The $channel manifest of $($site.Name) is not signed with $key, so the Nooks that follow it may not believe what that key signs"
  }
}

function Assert-SiteAgrees($site) {
  # The claim and key checks go by what the host serves over HTTPS; the files are written over SSH.
  # Each manifest must be the same both ways, or a publish could replace what those checks never saw.
  if ($site.Kind -ne 'host') { return }
  foreach ($c in 'stable', 'dev') {
    $there = Get-SiteFileInfo $site "$c/latest.json"
    $tmp = Join-Path $env:TEMP ('nook-site-' + [guid]::NewGuid().ToString('N') + '.json')
    try {
      $served = Save-SiteFile $site "$c/latest.json" $tmp
      $hash = if ($served) { (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash.ToLowerInvariant() }
    } finally {
      if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
    }
    if ([bool]$there -ne [bool]$served -or ($there -and $there.sha256 -ne $hash)) {
      throw ("$($site.BaseUrl)/$c/latest.json is not what $($site.HostName) has at $(Get-SitePath $site "$c/latest.json") " +
        "($(if ($served) { 'served' } else { 'not served' }), $(if ($there) { 'there' } else { 'not there' })): the address and the folder must be the same place")
    }
  }
}

function Assert-BuildTrusts([string]$keysFile, [string]$key, [string]$label, $site) {
  # A build that does not believe the key its manifests are signed with never updates again:
  # whoever installs it stays on it. So this is checked before anything is sent.
  $public = Get-PublicKey $key
  $trusted = @(Get-Content -LiteralPath $keysFile | ForEach-Object { (($_ -split '#', 2)[0].Trim() -split '\s+', 2)[0] } | Where-Object { $_ })
  if ($trusted -notcontains $public) {
    throw "$label does not believe the key that signs $($site.Name) ($public is not in its release-keys.txt): whoever installed it would never update again"
  }
}

function Enter-SiteLock($site, [string]$kind = 'publish') {
  # One publish or promote per site at a time: two would take the same version or undo each other.
  $id = -join ([Security.Cryptography.SHA1]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes($site.Name.ToLowerInvariant()))[0..7] |
    ForEach-Object { $_.ToString('x2') })
  $lock = New-Object Threading.Mutex($false, "Global\NookRS-$kind-$id")
  try {
    if (-not $lock.WaitOne(0)) { Write-Host "Another $kind for $($site.Name) is running; waiting for it"; [void]$lock.WaitOne() }
  } catch [Threading.AbandonedMutexException] {
    # its owner ended without letting go, and the lock is this one's now
  }
  $lock
}
