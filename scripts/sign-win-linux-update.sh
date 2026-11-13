#!/usr/bin/env bash
#
# Sign CI's Windows and Linux installers for self-update, on this Mac.
#
#   GH_TOKEN=$(gh auth token --user <release account>) \
#     scripts/sign-win-linux-update.sh rmux-v0.2.28-win-linux
#
# CI builds the `-win-linux` release **without** the updater key, on purpose:
# the key would be one malicious workflow step away from leaking, and whoever
# holds it can push code to every installed rmux (see scripts/lib/updater.sh).
# So the signing happens here instead:
#
#   1. download the NSIS `-setup.exe` and the `.AppImage` from that release
#   2. sign each with ~/.tauri/rmux-updater.key, bound to the version
#   3. upload the `.sig`s to that release
#   4. merge their entries into `latest.json` on the macOS release of the same
#      version (`rmux-vX.Y.Z`), which is the one marked latest and the one every
#      installed rmux reads
#
# Run after scripts/release-mac.sh, so the macOS entry is already in the
# manifest. DRY_RUN=1 downloads and signs but uploads nothing.

set -euo pipefail
cd "$(dirname "$0")/.."

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
die() { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

# shellcheck source=lib/updater.sh
. scripts/lib/updater.sh

TAG="${1:-}"
[[ "$TAG" =~ ^rmux-v([0-9]+\.[0-9]+\.[0-9]+)-win-linux$ ]] \
  || die "usage: $0 rmux-vX.Y.Z-win-linux"
VERSION="${BASH_REMATCH[1]}"
MAC_TAG="rmux-v$VERSION"

updater_key_check
command -v gh >/dev/null || die "the GitHub CLI (gh) is required"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

say "Downloading the installers from $TAG"
gh release download "$TAG" --repo "$REPO" -D "$WORK/assets" -p '*-setup.exe' -p '*.AppImage' \
  || die "could not download from $TAG — has CI finished, and is GH_TOKEN an account that can read it?"

SETUP=$(find "$WORK/assets" -name '*-setup.exe' | head -1)
APPIMAGE=$(find "$WORK/assets" -name '*.AppImage' | head -1)
[ -n "$SETUP" ] || [ -n "$APPIMAGE" ] || die "$TAG has neither a -setup.exe nor an .AppImage"

ENTRIES=()
SIGS=()
if [ -n "$SETUP" ]; then
  say "Signing $(basename "$SETUP")"
  updater_sign "$SETUP" "$VERSION"
  URL=$(download_url "$TAG" "$(basename "$SETUP")")
  ENTRIES+=(--entry "windows-x86_64=$URL=$SETUP.sig" --entry "windows-x86_64-nsis=$URL=$SETUP.sig")
  SIGS+=("$SETUP.sig")
else
  say "No -setup.exe on $TAG — Windows will not self-update this release"
fi
if [ -n "$APPIMAGE" ]; then
  say "Signing $(basename "$APPIMAGE")"
  updater_sign "$APPIMAGE" "$VERSION"
  URL=$(download_url "$TAG" "$(basename "$APPIMAGE")")
  ENTRIES+=(--entry "linux-x86_64=$URL=$APPIMAGE.sig" --entry "linux-x86_64-appimage=$URL=$APPIMAGE.sig")
  SIGS+=("$APPIMAGE.sig")
else
  say "No .AppImage on $TAG — Linux will not self-update this release"
fi

say "Merging into latest.json on $MAC_TAG"
EXISTING="-"
if gh release download "$MAC_TAG" --repo "$REPO" -p latest.json -D "$WORK/old" >/dev/null 2>&1; then
  EXISTING="$WORK/old/latest.json"
else
  say "warning: $MAC_TAG has no latest.json yet — writing one without a macOS entry"
fi
node scripts/update-manifest.mjs --in "$EXISTING" --out "$WORK/latest.json" --version "$VERSION" \
  --notes-url "https://github.com/$REPO/releases/tag/$MAC_TAG" "${ENTRIES[@]}"

if [ -n "${DRY_RUN:-}" ]; then
  say "DRY_RUN set — nothing uploaded. The manifest would be:"
  cat "$WORK/latest.json"
  exit 0
fi

gh release view "$MAC_TAG" --repo "$REPO" >/dev/null 2>&1 \
  || die "$MAC_TAG does not exist; run scripts/release-mac.sh first"
gh release upload "$TAG" --repo "$REPO" --clobber "${SIGS[@]}"
gh release upload "$MAC_TAG" --repo "$REPO" --clobber "$WORK/latest.json"
say "Done — Windows/Linux installs of older versions will now update to $VERSION"
