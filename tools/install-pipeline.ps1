<#
.SYNOPSIS
  Sets tools/pipeline.ps1 to run by itself, so each commit on main reaches the dev channel.
  Ported from the Kotlin Nook's tools/install-pipeline.ps1.

.DESCRIPTION
  Run it once, from your own terminal. It needs no administrator rights and changes two things:

    - a scheduled task, "Nook pipeline", that runs pipeline.ps1 as you, without a window, every
      ten minutes while you are signed in and whenever git starts it;
    - post-commit, post-merge and post-rewrite hooks in this repository that start the task, so a
      commit is on its way within seconds. A hook of your own is left alone; the schedule still
      picks the commit up.

  Then it starts the task once. -Remove takes both away again. Each build goes to the dev channel
  of the download host, https://dl.usenook.ai/nook (a folder with -Feed): a Nook
  RS set to dev (Settings > General > Updates) installs it by itself. Stable gets a build only when
  tools/promote.ps1 puts one there.
#>
[CmdletBinding()]
param([switch]$Remove, [string]$Feed)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$task = 'Nook pipeline'
$marker = '# Nook pipeline (tools/install-pipeline.ps1)'

$hooks = & git -C $repo rev-parse --git-path hooks
if ($LASTEXITCODE -ne 0) { throw "$repo is not a git repository" }
if (-not [IO.Path]::IsPathRooted($hooks)) { $hooks = Join-Path $repo $hooks }

if ($Remove) {
  if (Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
    Write-Host "Removed the scheduled task '$task'"
  }
  foreach ($name in 'post-commit', 'post-merge', 'post-rewrite') {
    $hook = Join-Path $hooks $name
    if ((Test-Path $hook) -and (Get-Content $hook -Raw).Contains($marker)) {
      Remove-Item $hook
      Write-Host "Removed the $name hook"
    }
  }
  exit 0
}

# The task: PowerShell under a console with no window (conhost --headless), so nothing flashes up.
$ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$pipeline = Join-Path $PSScriptRoot 'pipeline.ps1'
$feedArg = if ($Feed) { " -Feed `"$Feed`"" } else { '' }
$action = New-ScheduledTaskAction -Execute (Join-Path $env:SystemRoot 'System32\conhost.exe') -WorkingDirectory $repo `
  -Argument "--headless `"$ps`" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$pipeline`"$feedArg"
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(10) -RepetitionInterval (New-TimeSpan -Minutes 10)
$principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
# a start while one runs waits its turn instead of being dropped: that run may have read main already
$settings = New-ScheduledTaskSettingsSet -MultipleInstances Queue -StartWhenAvailable -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Hours 2)
Register-ScheduledTask -TaskName $task -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force `
  -Description "Tests and publishes each new commit on main of $repo to the dev channel Nook updates from (tools/pipeline.ps1)." | Out-Null
Write-Host "Registered the scheduled task '$task'"

# The hooks: git runs them with its own sh, so LF line endings and a POSIX shell. That sh (MSYS)
# rewrites arguments that start with a slash into file paths ("/run" becomes C:/Program Files/Git/run),
# so schtasks would never see its switches; MSYS_NO_PATHCONV turns that off. The Kotlin pipeline's
# hooks had this bug, and only its ten-minute schedule ever picked commits up.
New-Item -ItemType Directory -Force -Path $hooks | Out-Null
$body = "#!/bin/sh`n$marker`n# Starts the task that tests and publishes main; it returns at once and the task runs on its own.`n" +
  "MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' schtasks.exe /run /tn `"$task`" >/dev/null 2>&1`nexit 0`n"
foreach ($name in 'post-commit', 'post-merge', 'post-rewrite') {
  $hook = Join-Path $hooks $name
  if ((Test-Path $hook) -and -not (Get-Content $hook -Raw).Contains($marker)) {
    Write-Warning "$hook is someone else's hook; left as it is, so the schedule picks up commits there instead"
    continue
  }
  [IO.File]::WriteAllText($hook, $body)
  Write-Host "Added the $name hook"
}

Start-ScheduledTask -TaskName $task
. (Join-Path $PSScriptRoot 'release-lib.ps1')
$site = New-Site -Feed $Feed
$state = if ($site.Kind -eq 'folder') { Join-Path $site.Root 'pipeline' } else { Join-Path $env:LOCALAPPDATA 'Nook-pipeline' }
Write-Host ""
Write-Host "The pipeline is running now and after every commit on main."
Write-Host "  What it found: $state\status.txt; what it did: $state\state.json; its output: $state\logs"
Write-Host "  Set Nook to the dev channel (Settings > General > Updates) and it installs each build by itself,"
Write-Host "  once nothing is running in it. A failed build shows as a Windows notification."
Write-Host "  tools\promote.ps1 makes the dev build the stable release."
