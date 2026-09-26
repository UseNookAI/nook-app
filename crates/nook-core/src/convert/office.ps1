# Nook's document converter: opens a file in Word, Excel or PowerPoint, hidden and read-only, and
# saves it as another format. Nook writes this script beside its work and runs it with
# powershell -File, one file a run.
#   -App word|excel|powerpoint  -In <file>  -Out <file>  -Format <the program's format number;
#   -1 for Excel's PDF, which is an export>  -PidFile <file: the program's process started here,
#   for Nook to end should this run be stopped>
param(
  [Parameter(Mandatory = $true)][string]$App,
  [Parameter(Mandatory = $true)][string]$In,
  [Parameter(Mandatory = $true)][string]$Out,
  [Parameter(Mandatory = $true)][int]$Format,
  [string]$PidFile = ''
)
$ErrorActionPreference = 'Stop'
$missing = [Type]::Missing
# A password-protected file fails on this made-up password instead of asking for one.
$noPassword = 'nook-no-password'

function Release($o) {
  if ($o) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($o) }
}

# The program's processes before it is started, and after: the new one is this run's.
function Running($name) { @(Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object { $_.Id }) }
function Started($name, $before) {
  if (-not $PidFile) { return }
  $new = @(Running $name | Where-Object { $before -notcontains $_ })
  Set-Content -LiteralPath $PidFile -Value ($new -join "`r`n") -Encoding ascii
}

switch ($App) {
  'word' {
    # Word takes every argument by reference.
    $before = Running 'WINWORD'
    $word = New-Object -ComObject Word.Application
    Started 'WINWORD' $before
    $noChanges = 0
    # A PDF opens with a question ("Word will now convert your PDF...") no setting of a hidden
    # Word answers but this option of the person's, set for the conversion and put back after.
    $options = "HKCU:\Software\Microsoft\Office\$($word.Version)\Word\Options"
    $pdf = $In.ToLower().EndsWith('.pdf')
    $had = $null
    if ($pdf) {
      if (-not (Test-Path $options)) { New-Item -Path $options -Force | Out-Null }
      $had = (Get-ItemProperty -Path $options -Name DisableConvertPdfWarning -ErrorAction SilentlyContinue).DisableConvertPdfWarning
      Set-ItemProperty -Path $options -Name DisableConvertPdfWarning -Value 1 -Type DWord
    }
    try {
      $word.Visible = $false
      $word.DisplayAlerts = 0
      # Open(FileName, ConfirmConversions, ReadOnly, AddToRecentFiles, PasswordDocument,
      #      PasswordTemplate, Revert, WritePasswordDocument, WritePasswordTemplate, Format,
      #      Encoding, Visible)
      $file = $In; $confirm = $false; $readOnly = $true; $recent = $false; $password = $noPassword
      $blank = ''; $revert = $true; $blank2 = ''; $blank3 = ''; $anyFormat = 0; $encoding = $missing; $visible = $false
      $doc = $word.Documents.Open([ref]$file, [ref]$confirm, [ref]$readOnly, [ref]$recent, [ref]$password,
        [ref]$blank, [ref]$revert, [ref]$blank2, [ref]$blank3, [ref]$anyFormat, [ref]$encoding, [ref]$visible)
      try {
        $target = $Out; $as = $Format
        $doc.SaveAs2([ref]$target, [ref]$as)
      } finally { $doc.Close([ref]$noChanges); Release $doc }
    } finally {
      $word.Quit([ref]$noChanges); Release $word
      if ($pdf) {
        if ($null -eq $had) { Remove-ItemProperty -Path $options -Name DisableConvertPdfWarning -ErrorAction SilentlyContinue }
        else { Set-ItemProperty -Path $options -Name DisableConvertPdfWarning -Value $had -Type DWord }
      }
    }
  }
  'excel' {
    $before = Running 'EXCEL'
    $excel = New-Object -ComObject Excel.Application
    Started 'EXCEL' $before
    try {
      $excel.Visible = $false
      $excel.DisplayAlerts = $false
      $excel.AskToUpdateLinks = $false
      # Open(FileName, UpdateLinks, ReadOnly, Format, Password)
      $book = $excel.Workbooks.Open($In, 0, $true, $missing, $noPassword)
      try {
        if ($Format -lt 0) { $book.ExportAsFixedFormat(0, $Out) } else { $book.SaveAs($Out, $Format) }
      } finally { $book.Close($false); Release $book }
    } finally { $excel.Quit(); Release $excel }
  }
  'powerpoint' {
    # PowerPoint runs once: when it is open already, this is the person's own, and it stays open.
    $running = Running 'POWERPNT'
    $ppt = New-Object -ComObject PowerPoint.Application
    Started 'POWERPNT' $running
    $before = $ppt.Presentations.Count
    try {
      # Open(FileName, ReadOnly, Untitled, WithWindow)
      $deck = $ppt.Presentations.Open($In, -1, 0, 0)
      try { $deck.SaveAs($Out, $Format) } finally { $deck.Close(); Release $deck }
    } finally {
      if ($before -eq 0 -and $ppt.Presentations.Count -eq 0) { $ppt.Quit() }
      Release $ppt
    }
  }
  default { throw "Unknown program $App" }
}
