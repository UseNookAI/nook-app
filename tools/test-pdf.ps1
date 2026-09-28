<#
.SYNOPSIS
  Runs the PDF engine's tests with the real PDFium: the build of resources\runtime\engines.json,
  downloaded once and checked against its SHA-256.

.DESCRIPTION
  The PDF editor's own tests that need PDFium are ignored in a plain `cargo test`, since PDFium is
  a download. This fetches the pinned build into -Cache (once), then runs each of them on documents
  they make themselves, one per process: PDFium binds once per process, so two in one would
  fail. CI's "PDF engine" job and tools\publish.ps1 -Test both run it.

.PARAMETER Cache
  Where the PDFium download is kept between runs.
.PARAMETER Out
  Where the tests write the PDFs and pictures they make.
#>
[CmdletBinding()]
param(
  [string]$Cache = (Join-Path ([IO.Path]::GetTempPath()) 'nook-test-engines'),
  [string]$Out = (Join-Path ([IO.Path]::GetTempPath()) 'nook-test-pdf')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$spec = (Get-Content -Raw -LiteralPath (Join-Path $root 'resources\runtime\engines.json') | ConvertFrom-Json).components.pdfium
$build = $spec.backends.any[0]
$version = ($spec.version -replace '[^A-Za-z0-9]+', '-')
$dir = Join-Path $Cache "pdfium-$version"
$dll = Join-Path $dir 'bin\pdfium.dll'

if (-not (Test-Path -LiteralPath $dll)) {
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $archive = Join-Path $Cache $build.name
  Write-Host "==> Downloading PDFium $($spec.version) ($([math]::Round($build.bytes / 1MB, 1)) MB)"
  $ProgressPreference = 'SilentlyContinue'
  Invoke-WebRequest -Uri $build.url -OutFile $archive -UseBasicParsing
  $sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
  if ($sha -ne $build.sha256) {
    Remove-Item -LiteralPath $archive -Force
    throw "PDFium's download does not match engines.json (sha256 $sha, expected $($build.sha256))"
  }
  tar -xzf $archive -C $dir
  if ($LASTEXITCODE -ne 0) { throw "Could not unpack $archive (tar exit code $LASTEXITCODE)" }
  Remove-Item -LiteralPath $archive -Force
  if (-not (Test-Path -LiteralPath $dll)) { throw "No bin\pdfium.dll in PDFium's archive" }
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
Set-Location -LiteralPath $root
$env:NOOK_TEST_PDFIUM = $dll
$env:NOOK_TEST_OUT = $Out
$tests = @(
  'pdf::editor::live::keeps_the_rest_of_the_page',
  'pdf::editor::live::edits_text_in_a_picture',
  'pdf::editor::live::stops_between_pages'
)
foreach ($test in $tests) {
  Write-Host "==> $test"
  # stderr merged inside cmd: PowerShell 5.1 would turn cargo's progress into errors.
  cmd /c "cargo test -p nook-core --lib -- --ignored --exact $test --nocapture 2>&1"
  if ($LASTEXITCODE -ne 0) { throw "$test failed (exit code $LASTEXITCODE)" }
}
Write-Host "==> The PDF engine's tests passed"
