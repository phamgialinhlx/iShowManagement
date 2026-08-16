#!/usr/bin/env bash
#
# Build a macOS app someone *else* can open.
#
# ## The distinction that matters
#
# An **Apple Development** certificate signs an app for machines registered to
# your developer account. On anyone else's Mac, Gatekeeper refuses it — and it
# refuses quietly enough that the app looks broken rather than blocked. Verified:
# a Development-signed zmux.app already fails `spctl -a -t exec` on the machine
# that built it; it only launches there because a locally-built file never
# carries the quarantine flag that a download or an AirDrop attaches.
#
# Distribution needs three things, and all three or none:
#
#   1. a **Developer ID Application** certificate (not Apple Development)
#   2. **notarisation** — Apple scans the upload and issues a ticket
#   3. **stapling** — the ticket attached to the bundle, so it validates with no
#      network. Without it the first launch on a machine that is offline, or
#      behind a filter, fails exactly like an unsigned app.
#
# ## What you need before running this
#
# A Developer ID certificate requires the **Account Holder** role in the team.
# If Xcode offers no "Developer ID Application" option, that is the usual cause,
# and no amount of local configuration works around it.
#
# Credentials, either one:
#
#   export APPLE_API_KEY=…            # App Store Connect key id
#   export APPLE_API_ISSUER=…         # its issuer id
#   export APPLE_API_KEY_PATH=…       # the .p8 file
#
#   # or, simpler to obtain:
#   export APPLE_ID=you@example.com
#   export APPLE_PASSWORD=…           # an app-specific password, NOT your Apple ID password
#   export APPLE_TEAM_ID=…
#
# Deployment identifiers are read from the environment on purpose — none of them
# belong in a tracked file. See CLAUDE.local.md.

set -euo pipefail
cd "$(dirname "$0")/.."

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
die() { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

# --- 1. the certificate ------------------------------------------------------
#
# Checked before anything is built, because discovering it after a three-minute
# release compile is a waste of everyone's afternoon.
IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  IDENTITY=$(security find-identity -v -p codesigning \
    | grep "Developer ID Application" | head -1 | sed 's/.*"\(.*\)"/\1/') || true
fi

if [ -z "$IDENTITY" ]; then
  say "No Developer ID Application certificate found."
  cat <<'TXT'
What you have is probably an "Apple Development" certificate. That one signs for
your own registered machines only — an app signed with it is rejected by
Gatekeeper on anyone else's Mac, and looks broken rather than blocked.

  security find-identity -v -p codesigning

To get the right one you need the Account Holder role on the team, then
Xcode > Settings > Accounts > Manage Certificates > + > Developer ID Application.

Until then, `pnpm tauri build` produces an app that works on this machine, and a
friend has to go through System Settings > Privacy & Security > Open Anyway.
TXT
  exit 1
fi
say "Signing as: $IDENTITY"

# --- 2. credentials for the notary service ----------------------------------
#
# **A stored keychain profile is the preferred form**, and not only for
# convenience: every other option puts an app-specific password into a command
# line, and `ps` shows one user's argv to every account on the machine. Same
# reason the Claude credential never travels in argv. Create one once with:
#
#   xcrun notarytool store-credentials zmux-notary \
#     --apple-id you@example.com --team-id XXXXXXXXXX
#
# It prompts for the password on stdin, so the secret never reaches argv at all.
if xcrun notarytool history --keychain-profile "${NOTARY_PROFILE:-zmux-notary}" >/dev/null 2>&1; then
  NOTARY=(--keychain-profile "${NOTARY_PROFILE:-zmux-notary}")
  say "Using the stored notary profile: ${NOTARY_PROFILE:-zmux-notary}"
elif [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ]; then
  NOTARY=(--key "${APPLE_API_KEY_PATH:?APPLE_API_KEY_PATH must point at the .p8}" \
          --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then
  NOTARY=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
          --team-id "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required with APPLE_ID}")
else
  die "No notary credentials. Store a profile (see above), or set APPLE_API_* / APPLE_ID+APPLE_PASSWORD+APPLE_TEAM_ID."
fi

# --- 3. build ----------------------------------------------------------------
#
# `dist/` is not a cargo build input, so a UI-only change relinks in a second
# and embeds the *previous* bundle — the app then runs code you did not build,
# silently. Touching build.rs is what forces the re-embed.
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
[ -n "$VERSION" ] || die "could not read the version from Cargo.toml"

say "Building the UI"
pnpm exec vite build
touch src-tauri/build.rs

say "Building and signing the app"
# Tauri signs during bundling when this is set, which is better than signing
# afterwards: it signs the nested binaries — the agents in Resources — in the
# right order. A `--deep` sign after the fact is documented by Apple as
# unreliable for exactly that.
#
# **`--bundles app` only.** Building the dmg here too produces it from the app as
# it stands *at that moment* — which is before step 3b signs the nested agents
# and re-seals the wrapper. The dmg therefore captured a stale app: correctly
# built, but with an ad-hoc-signed agent inside and no notarisation ticket. It
# then failed to staple with "Record not found", the script reported success
# anyway, and the file labelled "send this one" was rejected by Gatekeeper.
# Verified by mounting it: `spctl` said `rejected / Unnotarized Developer ID`.
# The dmg is now built in step 5b, from the finished article.
APPLE_SIGNING_IDENTITY="$IDENTITY" pnpm tauri build --bundles app

APP="target/release/bundle/macos/zmux.app"
[ -d "$APP" ] || die "no bundle at $APP"

# --- 3b. sign the nested executables ----------------------------------------
#
# **Tauri does not sign binaries inside `Resources`.** It signs the main
# executable and the bundle wrapper and stops there — so `Resources/agents/`
# ships unsigned, and Apple refuses the whole submission for it:
#
#   zmux.app/Contents/Resources/agents/zmuxd
#     · not signed with a valid Developer ID certificate
#     · signature does not include a secure timestamp
#     · executable does not have the hardened runtime enabled
#
# Three things this has to get right:
#
#   **Inside out.** Nested code is signed first, then the wrapper, because
#   signing the app seals a hash of everything inside it. Do it the other way
#   and the outer signature is invalidated by the inner one.
#
#   **`--timestamp`.** A local `codesign` omits the secure timestamp by default,
#   and notarisation requires it. It needs the network — an offline build fails
#   here rather than silently producing something Apple will reject.
#
#   **Mach-O only.** `agents/` also holds static musl binaries for Linux hosts.
#   They are not Mach-O, cannot be signed, and Apple ignores them — attempting
#   it would fail the build for no reason.
say "Signing nested executables"
while IFS= read -r binary; do
  case "$(file -b "$binary")" in
    *Mach-O*)
      echo "  $(basename "$binary")"
      codesign --force --timestamp --options runtime --sign "$IDENTITY" "$binary"
      ;;
  esac
#   **`Contents/MacOS` as well as `Contents/Resources`, and that is not
#   symmetry for its own sake.** `zmux-askpass` is a *sidecar*: Tauri puts it
#   beside the main executable, because that is where `askpass::helper_path`
#   looks for it. Sealing the wrapper does not give a nested Mach-O its own
#   signature, and notarisation checks every executable in the bundle
#   individually — so a sidecar missed here fails the whole submission with a
#   complaint about one binary, after the upload and the wait. The main `zmux`
#   binary is matched too; signing it here is harmless, since the bundle seal
#   below replaces that signature anyway.
done < <(find "$APP/Contents/MacOS" "$APP/Contents/Resources" -type f -perm -u+x)

# Re-seal the wrapper now that its contents have changed.
codesign --force --timestamp --options runtime --sign "$IDENTITY" "$APP"

# The hardened runtime is *required* for notarisation, and its absence is only
# reported after the upload — so it is checked here instead.
#
# Captured into a variable rather than piped into `grep -q`, and that is not
# style. Under `set -o pipefail`, `grep -q` exits the instant it matches, which
# closes the pipe and hands `codesign` a SIGPIPE — so the *pipeline* reports 141
# and the check fails on a bundle that is perfectly signed. It is a race against
# how much codesign still had to write, which is why it reproduces
# intermittently and never when you run the line by hand. This bit us exactly
# once, on a correctly hardened build.
SIGNATURE=$(codesign -dvvv "$APP" 2>&1 || true)
case "$SIGNATURE" in
  *"flags="*runtime*) ;;
  *) die "the bundle is not hardened-runtime signed; notarisation would be refused" ;;
esac

# --- 4. notarise -------------------------------------------------------------
#
# A zip, not the .app: the notary service takes an archive, and `ditto` with
# `--keepParent` is the form Apple documents. `zip -r` mangles symlinks inside
# frameworks.
say "Notarising — this uploads to Apple and usually takes a few minutes"
ZIP="target/release/bundle/macos/zmux.zip"
ditto -c -k --keepParent "$APP" "$ZIP"

xcrun notarytool submit "$ZIP" "${NOTARY[@]}" --wait \
  || die "notarisation failed — 'xcrun notarytool log <id>' gives the reason per binary"

# --- 5. staple ---------------------------------------------------------------
#
# The step people skip. Without it the ticket exists only on Apple's servers, so
# a first launch offline or behind a filter fails like an unsigned app.
say "Stapling the ticket"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"

# --- 5b. the disk image, built from the finished app -------------------------
#
# Made here rather than by Tauri, for the ordering reason above: this has to
# contain the app *after* nested signing, notarisation and stapling, and nothing
# in the bundler's pipeline runs that late. `hdiutil` on a staging folder is the
# ordinary way to do it — note that the app inside is still the genuine Tauri
# bundle, which is the part that must never be hand-assembled.
#
# The ticket is stapled to the app before it goes in, so the copy the recipient
# drags to /Applications validates offline on first launch.
say "Building the disk image from the stapled app"
DMG_DIR="target/release/bundle/dmg"
DMG="$DMG_DIR/zmux_${VERSION}_aarch64.dmg"
STAGE=$(mktemp -d)
mkdir -p "$DMG_DIR"
cp -R "$APP" "$STAGE/"
# The drag-to-install affordance. Without it the window is a lone icon and the
# recipient is left to guess where it goes.
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "zmux" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

# A disk image is itself code-signed and notarised — separately from the app it
# carries. Its ticket is keyed to the dmg's own hash, which is why stapling one
# that was never submitted fails with "Record not found" rather than saying what
# is wrong.
say "Signing and notarising the disk image"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"
xcrun notarytool submit "$DMG" "${NOTARY[@]}" --wait \
  || die "the disk image was refused — 'xcrun notarytool log <id>' says why"

# No `&&` here, deliberately. Under `set -e` a failure on the left of `&&` is
# exempt from exiting, so the previous version swallowed a failed staple and
# went on to print "send this one" for a file Gatekeeper rejects.
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

# --- 6. prove it ------------------------------------------------------------
#
# `spctl` is the same check Gatekeeper runs on the receiving machine. Anything
# other than "accepted" here means it will be refused there.
say "What Gatekeeper will say on someone else's Mac"
spctl -a -vvv -t exec "$APP"

# **The app inside the dmg, not just the app on disk.** They were different
# once, and the difference was invisible from here: the standalone bundle passed
# every check while the file actually being sent contained an unnotarised copy.
# Mounting it is the only way to check the artefact rather than its neighbour.
say "…and to the copy inside the disk image"
MNT=$(mktemp -d)
hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$MNT" >/dev/null \
  || die "could not mount the disk image to verify it"
VERDICT=$(spctl -a -vvv -t exec "$MNT/zmux.app" 2>&1 || true)
STAPLED=$(xcrun stapler validate "$MNT/zmux.app" 2>&1 || true)
hdiutil detach "$MNT" >/dev/null 2>&1 || true
rmdir "$MNT" 2>/dev/null || true
echo "$VERDICT"
case "$VERDICT" in
  *accepted*) ;;
  *) die "the app inside the dmg would be REFUSED on another Mac — do not send it" ;;
esac
case "$STAPLED" in
  *worked*) ;;
  *) die "the app inside the dmg has no stapled ticket; a first launch offline would fail" ;;
esac

say "Done"
echo "  app: $APP"
echo "  dmg: $DMG  ← send this one (verified from inside the image)"
