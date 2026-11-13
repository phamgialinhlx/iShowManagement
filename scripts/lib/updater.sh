# Shared by release-mac.sh and sign-win-linux-update.sh. Sourced, not run.
#
# ## The updater key never leaves this Mac
#
# Every installed rmux trusts exactly one public key (`plugins.updater.pubkey`
# in src-tauri/tauri.conf.json) and installs whatever that key signed. Its
# private half is therefore the most valuable secret this project has: anyone
# holding it can push code to every machine running rmux. It lives at
# ~/.tauri/rmux-updater.key (0600) and is **never** put in CI secrets or the
# repo — a CI secret is one malicious workflow step away from exfiltration,
# which has already happened to this repository once. CI builds with
# `createUpdaterArtifacts` off (the default), so it needs no key at all.
#
# The key reaches `tauri signer sign` through its environment, never argv
# (`ps` shows argv to every account on the machine).

REPO="${RMUX_RELEASE_REPO:-phamgialinhlx/iShowManagement}"
UPDATER_KEY="${RMUX_UPDATER_KEY:-$HOME/.tauri/rmux-updater.key}"

updater_key_check() {
  [ -f "$UPDATER_KEY" ] || die "no updater key at $UPDATER_KEY — it is generated once with
  pnpm tauri signer generate --ci -p '' -w ~/.tauri/rmux-updater.key
and its .pub must match plugins.updater.pubkey in src-tauri/tauri.conf.json."
  local mode
  mode=$(stat -f '%Lp' "$UPDATER_KEY" 2>/dev/null || stat -c '%a' "$UPDATER_KEY")
  [ "$mode" = "600" ] || die "$UPDATER_KEY is mode $mode; refusing a signing key others can read (chmod 600 it)"
}

# updater_sign <file> <version> — writes <file>.sig, bound to <version> so an
# updater with requireSignedVersion refuses it under any other version number.
updater_sign() {
  local file="$1" version="$2"
  rm -f "$file.sig"
  TAURI_SIGNING_PRIVATE_KEY="$(cat "$UPDATER_KEY")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
    pnpm --silent tauri signer sign --app-version "$version" "$file" >/dev/null
  [ -s "$file.sig" ] || die "signing $file produced no signature"
}

# download_url <tag> <asset name>
download_url() {
  printf 'https://github.com/%s/releases/download/%s/%s' "$REPO" "$1" "$2"
}
