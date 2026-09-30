#!/usr/bin/env bash
# The Mac app from the working tree (on a Mac with Xcode's command line tools, Rust and Node 22):
# Nook.app and its disk image, then into dist-macos/
#   Nook-<version>-macos-arm64.dmg           what a person downloads and drags to Applications
#   Nook-<version>-macos-arm64.app.tar.gz    what an installed Nook downloads to update itself
#   build.json                               the version and commit, for nook-release manifest
#
# NOOK_VERSION sets the version (else the workspace's), NOOK_COMMIT the commit stamped in.
# Signing: with SIGNING_IDENTITY ("Developer ID Application: ...", its certificate in the keychain
# or given as APPLE_CERTIFICATE + APPLE_CERTIFICATE_PASSWORD) the app is signed with it, and with
# NOTARIZE_ID, NOTARIZE_PASSWORD and NOTARIZE_TEAM notarized too; without, it is signed ad hoc.
set -euo pipefail
cd "$(dirname "$0")/.."

version="${NOOK_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
commit="${NOOK_COMMIT:-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)}"
export NOOK_COMMIT="$commit"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.3}"

# Tauri reads these; an empty one would count as given, so only what is set is passed on.
if [ -n "${SIGNING_IDENTITY:-}" ]; then export APPLE_SIGNING_IDENTITY="$SIGNING_IDENTITY"; fi
if [ -z "${APPLE_CERTIFICATE:-}" ]; then unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD; fi
if [ -n "${NOTARIZE_ID:-}" ] && [ -n "${NOTARIZE_PASSWORD:-}" ] && [ -n "${NOTARIZE_TEAM:-}" ]; then
  export APPLE_ID="$NOTARIZE_ID" APPLE_PASSWORD="$NOTARIZE_PASSWORD" APPLE_TEAM_ID="$NOTARIZE_TEAM"
fi

config=()
if [ -n "${NOOK_VERSION:-}" ]; then
  config=(--config "{\"version\":\"$NOOK_VERSION\"}")
fi
# (macOS's bash 3.2 takes an empty array for an unset one under set -u: expanded only when set)
npx tauri build --ci --bundles app,dmg ${config[@]+"${config[@]}"}

bundle=target/release/bundle
app="$bundle/macos/Nook.app"
dmg="$(ls "$bundle"/dmg/*.dmg | head -1)"
codesign --verify --deep --strict --verbose=2 "$app"

out=dist-macos
rm -rf "$out"
mkdir -p "$out"
cp "$dmg" "$out/Nook-$version-macos-arm64.dmg"
# No AppleDouble files in the archive: the signature is inside the bundle, not in its xattrs.
COPYFILE_DISABLE=1 tar -C "$bundle/macos" -czf "$out/Nook-$version-macos-arm64.app.tar.gz" Nook.app
printf '{"version":"%s","commit":"%s"}\n' "$version" "$commit" > "$out/build.json"
shasum -a 256 "$out"/*.dmg "$out"/*.tar.gz
