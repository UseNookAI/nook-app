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
