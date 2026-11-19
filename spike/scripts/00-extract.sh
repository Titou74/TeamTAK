#!/usr/bin/env bash
# Extracts the official ZIP into spike/work/ (git-ignored). Touches nothing else.
. "$(dirname "$0")/lib.sh"

[ -f "$TAK_ZIP" ] || { echo "ZIP not found: $TAK_ZIP" >&2; exit 1; }
mkdir -p "$WORK_DIR"
if [ -d "$TAK_SRC" ]; then
  echo "Already extracted: $TAK_SRC"
else
  log "Extracting $(basename "$TAK_ZIP")"
  unzip -q "$TAK_ZIP" -d "$WORK_DIR"
fi
cat "$TAK_SRC/tak/version.txt"; echo
