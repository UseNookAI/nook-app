<#
.SYNOPSIS
  Tests and publishes each new commit on main to the dev channel of Nook's update site.
  Ported from the Kotlin Nook's tools/pipeline.ps1.

.DESCRIPTION
  This repository's CI/CD on this PC (tools/install-pipeline.ps1 sets it up once; GitHub Actions in
  .github/workflows runs the same checks wherever the repository is pushed):

    a commit on main -> a git hook, or the ten-minute schedule, starts the "Nook pipeline" task
      -> this script -> tools/publish.ps1 -Test: checks, installer, signed dev manifest on the site
      -> every Nook on the dev channel reads it within a minute and installs it by itself once
         nothing is running. Stable gets a build only when tools/promote.ps1 puts one there.

  Each round publishes the newest commit on the branch, and commits that land meanwhile go in the
  next round, so a burst of commits is one build. A change that leaves the app as it is (docs/, *.md,
  tools/, .github/) is not built.
  A commit that fails is not tried again until there is a newer one (or -Retry), and the failure
  shows as a Windows notification. Each round's output is kept in <state>\logs, the outcomes in
  <state>\state.json and what the last run found in <state>\status.txt.

.PARAMETER Branch
  The branch to publish.
.PARAMETER Feed
  Publish to this folder (a test feed) instead of the download host.
.PARAMETER State
  Where the outcomes and logs go: %LOCALAPPDATA%\Nook-pipeline for the host, <feed>\pipeline for a folder.
.PARAMETER Retry
  Try the newest commit again although it failed.
#>
[CmdletBinding()]
param(
  [string]$Branch = 'main',
  [string]$Feed,
  [string]$HostName,
  [string]$HostRoot,
  [string]$BaseUrl,
  [string]$SshKey,
  [string]$State,
  [switch]$Retry
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-lib.ps1')
$keepLogs = 30

$siteArgs = @{}
foreach ($p in 'Feed', 'HostName', 'HostRoot', 'BaseUrl', 'SshKey') { if ($PSBoundParameters.ContainsKey($p)) { $siteArgs[$p] = $PSBoundParameters[$p] } }
$site = New-Site @siteArgs
if (-not $State) {
  if ($site.Kind -eq 'folder') { $State = Join-Path $site.Root 'pipeline' }
  elseif ($env:LOCALAPPDATA) { $State = Join-Path $env:LOCALAPPDATA 'Nook-pipeline' }
  else { throw "LOCALAPPDATA is not set; pass -State" }
}
$State = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($State).TrimEnd('\')
$logs = Join-Path $State 'logs'
$statePath = Join-Path $State 'state.json'

function Show-Notification([string]$title, [string]$text) {
  # A Windows notification in PowerShell's name: the pipeline runs with no window to write to.
  try {
    $null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    $null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml("<toast><visual><binding template=`"ToastGeneric`"><text>$([Security.SecurityElement]::Escape($title))</text>" +
      "<text>$([Security.SecurityElement]::Escape($text))</text></binding></visual></toast>")
    $app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
  } catch {
    Write-Warning "Could not show a notification: $($_.Exception.Message)"
  }
}

function Write-Status([string]$text) {
  # what this run found, for whoever wonders why nothing happened; the task has no window
  Write-Host $text
  [IO.File]::WriteAllText((Join-Path $State 'status.txt'), "$(Get-Date -Format 's')  $text`r`n")
}

function Read-History {
  if (-not (Test-Path $statePath)) { return @() }
  @((Get-Content $statePath -Raw | ConvertFrom-Json).history)
}

function Save-Round($round) {
  # newest first, the last 30; written under another name and then renamed, so it is never half there
  $history = @(@($round) + @(Read-History) | Select-Object -First 30)
  $json = [pscustomobject]@{ branch = $Branch; site = $site.Name; last = $round; history = $history } | ConvertTo-Json -Depth 4
  [IO.File]::WriteAllText("$statePath.part", $json)
  Move-Item "$statePath.part" $statePath -Force
}

# As publish.ps1 does: a shell a packaged app started (the Claude desktop app is one) keeps what it
# writes under %LOCALAPPDATA% to that app, where nothing else would see it.
Assert-NotRedirected $State 'the state and logs would go where nothing else sees them; run the pipeline from your own terminal or its scheduled task'
if ($site.Kind -eq 'folder') { Assert-NotRedirected $site.Root 'no Nook would ever see this feed; run the pipeline from your own terminal or its scheduled task' }

New-Item -ItemType Directory -Force -Path $logs | Out-Null

# One pipeline per site; one started meanwhile waits, then finds whatever is left to do.
$lock = Enter-SiteLock $site 'pipeline'
try {
  # at most five builds a run; if commits keep coming, the next start carries on
  for ($i = 0; $i -lt 5; $i++) {
    $head = ([string](Invoke-Git rev-parse --verify "refs/heads/$Branch^{commit}")).Trim()
    $short = $head.Substring(0, 7)
    try {
      $dev = Read-SiteManifest $site 'dev'
    } catch {
      # the site is out of reach; the next start tries again
      Write-Status "$($_.Exception.Message); the next run tries again"
      break
    }
    if ($dev -and -not (Test-OurCommit $dev.commit)) {
      Write-Status ("Waiting: the dev channel of $($site.Name) serves $($dev.version) from $($dev.commit), not a commit of this repository. " +
        "To take it over, run tools\publish.ps1 -Test -Claim once.")
      break
    }
    $last = @(Read-History | Where-Object { $_.commit -eq $head }) | Select-Object -First 1
    if ($dev -and $head.StartsWith($dev.commit)) { Write-Status "$Branch ($short) is on the dev channel as $($dev.version)"; break }
    if ($last -and -not ($Retry -and $last.outcome -eq 'failed')) { Write-Status "$Branch ($short) was $($last.outcome) at $($last.at)"; break }
    $Retry = $false
    $subject = ([string](Invoke-Git log -1 --format=%s $head)).Trim()
    $round = [ordered]@{ commit = $head; subject = $subject; at = (Get-Date).ToString('s'); outcome = ''; version = ''; reason = ''; log = '' }

    # Docs, the release tools and the workflows are not in the app: nothing to build for them alone.
    if ($dev) {
      $changed = @(Invoke-Git diff --name-only $dev.commit $head)
      $code = @($changed | Where-Object { $_ -notmatch '^(docs|tools|\.github)/' -and $_ -notmatch '\.md$' })
      if ($code.Count -eq 0) {
        $round.outcome = 'skipped'
        $round.reason = "only docs, tools or workflows changed since $($dev.version) ($($dev.commit))"
        Write-Status "$short skipped: $($round.reason)"
        Save-Round ([pscustomobject]$round)
        continue
      }
    }

    $log = Join-Path $logs ('{0}-{1}.log' -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $short)
    $round.log = $log
    Write-Status "Publishing $Branch ($short, $subject); the output goes to $log"
    try {
      "Nook pipeline: $Branch at $head to $($site.Name), $(Get-Date -Format 's')" | Out-File -FilePath $log -Encoding utf8
      & (Join-Path $PSScriptRoot 'publish.ps1') -Ref $head -Test @siteArgs *>&1 | Out-File -FilePath $log -Append -Encoding utf8 -Width 500
      $now = Read-SiteManifest $site 'dev'
      if (-not $now -or -not $head.StartsWith($now.commit)) { throw "publish.ps1 ended but the dev channel does not serve $short; see the log" }
      $round.outcome = 'published'
      $round.version = $now.version
      Write-Status "$Branch ($short) is on the dev channel as $($round.version)"
      $before = @(Read-History | Where-Object { $_.outcome -ne 'skipped' }) | Select-Object -First 1
      if ($before -and $before.outcome -eq 'failed') { Show-Notification "Nook $($round.version) is on the dev channel" "$Branch passes again with $short ($subject)." }
    } catch {
      $round.outcome = 'failed'
      $round.reason = $_.Exception.Message
      "FAILED: $($round.reason)" | Out-File -FilePath $log -Append -Encoding utf8 -Width 500
      Write-Status "$short failed: $($round.reason)"
      Show-Notification "Nook pipeline: $short failed" "$($round.reason) The log is in $logs."
    }
    Save-Round ([pscustomobject]$round)
  }
} finally {
  Get-ChildItem $logs -Filter '*.log' | Sort-Object Name -Descending | Select-Object -Skip $keepLogs | Remove-Item -Force
  $lock.ReleaseMutex()
}
