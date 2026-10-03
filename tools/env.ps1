# Puts the portable Rust toolchain on this shell's PATH, unless cargo is already there. Dot-source
# it: . .\tools\env.ps1
# The toolchain is NOOK_RUST_ROOT, else the first tools\rust found beside the repository or in a
# folder above it (so worktrees beside the repository find it too).
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $rustRoot = $env:NOOK_RUST_ROOT
    if (-not $rustRoot) {
        $dir = Split-Path $PSScriptRoot -Parent
        while ($dir) {
            $candidate = Join-Path $dir 'tools\rust'
            if (Test-Path (Join-Path $candidate 'cargo\bin\cargo.exe')) { $rustRoot = $candidate; break }
            $up = Split-Path $dir -Parent
            if ($up -eq $dir) { break }
            $dir = $up
        }
    }
    if ($rustRoot -and (Test-Path (Join-Path $rustRoot 'cargo\bin\cargo.exe'))) {
        $env:RUSTUP_HOME = Join-Path $rustRoot 'rustup'
        $env:CARGO_HOME = Join-Path $rustRoot 'cargo'
        $env:Path = (Join-Path $rustRoot 'cargo\bin') + ';' + $env:Path
    } else {
        Write-Warning "No cargo on PATH and no portable toolchain in a tools\rust folder above $PSScriptRoot (set NOOK_RUST_ROOT)"
    }
}

# The Windows SDK's resource compiler, for the icon and version info built into Nook.exe
# (tauri-winres, through embed-resource), unless RC names one already. embed-resource looks for it
# under the SDK folder the registry names and then through Visual Studio; where the 64-bit registry
# names a Windows Kits folder without the tools, only Visual Studio's answer finds it, and not every
# time (one publish's clippy found it and its tests, seconds later, did not).
if (-not $env:RC) {
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $rc = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+){3}$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "$arch\rc.exe" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    if ($rc) { $env:RC = $rc }
}
