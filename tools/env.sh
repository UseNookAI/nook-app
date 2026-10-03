# Portable Rust toolchain for Git Bash: . tools/env.sh
# As env.ps1: NOOK_RUST_ROOT, else the first tools/rust found beside the repository or above it.
if ! command -v cargo >/dev/null 2>&1; then
  root="${NOOK_RUST_ROOT:-}"
  if [ -z "$root" ]; then
    dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
    while [ -n "$dir" ]; do
      if [ -x "$dir/tools/rust/cargo/bin/cargo.exe" ] || [ -x "$dir/tools/rust/cargo/bin/cargo" ]; then
        root="$dir/tools/rust"
        break
      fi
      up="$(dirname "$dir")"
      [ "$up" = "$dir" ] && break
      dir="$up"
    done
  fi
  if [ -n "$root" ]; then
    export RUSTUP_HOME="$root/rustup"
    export CARGO_HOME="$root/cargo"
    export PATH="$root/cargo/bin:$PATH"
  else
    echo "No cargo on PATH and no portable toolchain in a tools/rust folder above the repository (set NOOK_RUST_ROOT)" >&2
  fi
fi

# As env.ps1: the Windows SDK's resource compiler as RC, unless set (Git Bash on Windows only).
if [ -z "${RC:-}" ] && command -v cygpath >/dev/null 2>&1; then
  pf86="$(printenv 'ProgramFiles(x86)' || true)"
  if [ -n "$pf86" ]; then
    kits="$(cygpath -u "$pf86")/Windows Kits/10/bin"
    arch=x64
    [ "${PROCESSOR_ARCHITECTURE:-}" = ARM64 ] && arch=arm64
    for ver in $(ls "$kits" 2>/dev/null | grep -E '^[0-9]+(\.[0-9]+){3}$' | sort -t. -k1,1nr -k2,2nr -k3,3nr -k4,4nr); do
      if [ -f "$kits/$ver/$arch/rc.exe" ]; then
        RC="$(cygpath -w "$kits/$ver/$arch/rc.exe")"
        export RC
        break
      fi
    done
  fi
fi
