#!/usr/bin/env bash
# Bring the external copy of this project's Claude history up to date.
#
# The transcript is appended to while a session is running, so a copy taken
# mid-session is always a few lines behind — harmless, but it means the last
# exchange is missing. Run this once after quitting Claude Code and the two are
# identical.
set -euo pipefail
SRC="$HOME/.claude/projects/-Users-macprom1-Code-rmux"
DST="$HOME/.claude/projects/-Volumes-Na-s-Mac-Data-Codes-rmux"
[ -d "$SRC" ] || { echo "nothing at $SRC"; exit 0; }
mkdir -p "$DST"
rsync -a --exclude '*.aug7-backup' "$SRC/" "$DST/"
echo "synced:"
echo "  from $SRC"
echo "  to   $DST"
ls -lh "$DST" | grep -v '^total'
