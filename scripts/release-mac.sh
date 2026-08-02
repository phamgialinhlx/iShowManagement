#!/usr/bin/env bash
#
# Build a macOS app someone *else* can open.
#
# ## The distinction that matters
#
# An **Apple Development** certificate signs an app for machines registered to
# your developer account. On anyone else's Mac, Gatekeeper refuses it — and it
# refuses quietly enough that the app looks broken rather than blocked. Verified:
# a Development-signed rmux.app already fails `spctl -a -t exec` on the machine
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
#   xcrun notarytool store-credentials rmux-notary \
#     --apple-id you@example.com --team-id XXXXXXXXXX
#
# It prompts for the password on stdin, so the secret never reaches argv at all.
if xcrun notarytool history --keychain-profile "${NOTARY_PROFILE:-rmux-notary}" >/dev/null 2>&1; then
  NOTARY=(--keychain-profile "${NOTARY_PROFILE:-rmux-notary}")
  say "Using the stored notary profile: ${NOTARY_PROFILE:-rmux-notary}"
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
say "Building the UI"
pnpm exec vite build
touch src-tauri/build.rs

say "Building and signing the app"
# Tauri signs during bundling when this is set, which is better than signing
# afterwards: it signs the nested binaries — the agents in Resources — in the
# right order. A `--deep` sign after the fact is documented by Apple as
# unreliable for exactly that.
APPLE_SIGNING_IDENTITY="$IDENTITY" pnpm tauri build --bundles app,dmg

APP="target/release/bundle/macos/rmux.app"
[ -d "$APP" ] || die "no bundle at $APP"

# --- 3b. sign the nested executables ----------------------------------------
#
# **Tauri does not sign binaries inside `Resources`.** It signs the main
# executable and the bundle wrapper and stops there — so `Resources/agents/`
# ships unsigned, and Apple refuses the whole submission for it:
#
#   rmux.app/Contents/Resources/agents/rmux-agent
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
done < <(find "$APP/Contents/Resources" -type f -perm -u+x)

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
ZIP="target/release/bundle/macos/rmux.zip"
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

# The dmg is what gets sent, so it is stapled too — otherwise the app inside is
# fine and the disk image itself warns.
DMG=$(ls target/release/bundle/dmg/*.dmg 2>/dev/null | head -1 || true)
if [ -n "$DMG" ]; then
  xcrun stapler staple "$DMG" && xcrun stapler validate "$DMG"
fi

# --- 6. prove it ------------------------------------------------------------
#
# `spctl` is the same check Gatekeeper runs on the receiving machine. Anything
# other than "accepted" here means it will be refused there.
say "What Gatekeeper will say on someone else's Mac"
spctl -a -vvv -t exec "$APP"

say "Done"
echo "  app: $APP"
[ -n "$DMG" ] && echo "  dmg: $DMG  ← send this one"
