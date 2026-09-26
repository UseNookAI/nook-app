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
