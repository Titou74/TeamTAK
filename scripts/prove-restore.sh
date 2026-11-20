#!/usr/bin/env bash
# Proves, for real, that a backup can be restored: on the local stack it prepares teams,
# accesses and zips, takes a backup, DESTROYS the stack (this project's Docker volumes,
# deploy/.env and deploy/data), restores it from the archive, and checks that the zips
# handed out before the backup still connect to TAK Server with the right group and that
# what was revoked is still refused.
#
#   scripts/prove-restore.sh --yes
#
# Local mode only: it refuses to run on a production installation. Everything on the local
# stack that is not in the backup is lost, which is the point. Nothing outside the Compose
# project "teamtak" is touched.
#
# Before destroying anything it copies deploy/.env and deploy/data to a folder outside the
# repository (TM_RESTORE_SAFE, a new temporary folder by default) and puts them back if the
# restore fails.
#
# Steps, each printed as it runs:
#   1. prepare the stack and take the backup      (tests/restore.rs, before_the_backup)
#   2. keep a copy of deploy/.env and deploy/data outside the repository
#   3. destroy                                     (deploy/scripts/clean.sh)
#   4. restore                                     (deploy/scripts/restore.sh)
#   5. verify                                      (tests/restore.rs, after_the_restore)
# Run scripts/test-integration.sh afterwards for the whole suite on the restored stack.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo > /dev/null || export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

step() { printf '\n\033[1m#### %s\033[0m\n' "$*"; }
die() { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

[ "${1:-}" = --yes ] || die "this destroys the local stack and restores it from a backup; run it with --yes"
[ -f deploy/.env ] || die "deploy/.env is missing: run ./install.sh --local first"
grep -q '^MODE=local$' deploy/.env || die "this proof only runs on a local-mode installation"

SAFE="${TM_RESTORE_SAFE:-$(mktemp -d "${TMPDIR:-/tmp}/teamtak-restore-proof.XXXXXX")}"
case "$SAFE" in "$PWD"/*) die "TM_RESTORE_SAFE must be outside the repository" ;; esac
mkdir -p "$SAFE"
export TM_RESTORE_STATE="$SAFE/state"
PROJECT_VOLUMES() { docker volume ls -q --filter label=com.docker.compose.project=teamtak; }

step "1/5 Preparing teams, accesses and zips, then asking the platform for a backup"
cargo test -p teamtak-server --test restore -- --ignored --exact before_the_backup --nocapture 2>&1 | tee "$SAFE/before.log" | grep -E '^PROOF|^test |test result|panicked' || true
grep -q '^test result: ok. 1 passed' "$SAFE/before.log" || die "the preparation failed; see $SAFE/before.log"
ARCHIVE="$(sed -n 's/^PROOF archive=//p' "$SAFE/before.log")"
[ -f "deploy/backups/$ARCHIVE" ] || die "the archive deploy/backups/$ARCHIVE does not exist"
echo "   archive: deploy/backups/$ARCHIVE ($(wc -c < "deploy/backups/$ARCHIVE" | tr -d ' ') bytes)"
echo "   zips kept outside the stack: $(ls "$TM_RESTORE_STATE" | grep -c '\.zip$')"

step "2/5 Keeping a copy of deploy/.env and deploy/data outside the repository"
rm -rf "$SAFE/original"
mkdir -p "$SAFE/original"
cp -p deploy/.env "$SAFE/original/.env"
cp -Rp deploy/data "$SAFE/original/data"
echo "   $SAFE/original"

step "3/5 Destroying this project's volumes, deploy/.env and deploy/data"
echo "   volumes before: $(PROJECT_VOLUMES | tr '\n' ' ')"
deploy/scripts/clean.sh --yes > "$SAFE/clean.log" 2>&1 || { cat "$SAFE/clean.log" >&2; die "the cleaning failed"; }
[ -z "$(PROJECT_VOLUMES)" ] || die "volumes of the project are left: $(PROJECT_VOLUMES)"
[ ! -e deploy/.env ] && [ ! -e deploy/data ] || die "deploy/.env or deploy/data is still there"
[ -z "$(docker ps -a -q --filter label=com.docker.compose.project=teamtak)" ] || die "containers of the project are left"
echo "   volumes after: none; containers: none; deploy/.env and deploy/data: gone"

put_back() {
  echo
  echo "The restore failed. Putting deploy/.env and deploy/data back as they were before the destruction." >&2
  rm -rf deploy/data deploy/.env
  cp -p "$SAFE/original/.env" deploy/.env
  cp -Rp "$SAFE/original/data" deploy/data
  echo "They are back (the Docker volumes are not: they are what the restore was to bring back)." >&2
  echo "Logs: $SAFE" >&2
}

step "4/5 Restoring from the archive"
start=$(date +%s)
if ! deploy/scripts/restore.sh --yes "deploy/backups/$ARCHIVE" 2>&1 | tee "$SAFE/restore.log" | grep -v -E '^\s*$'; then
  put_back
  exit 1
fi
grep -q 'Restored from' "$SAFE/restore.log" || { put_back; exit 1; }
echo "   restored in $(( $(date +%s) - start )) s"

step "5/5 Verifying with the zips handed out before the backup"
cargo test -p teamtak-server --test restore -- --ignored --exact after_the_restore --nocapture 2>&1 | tee "$SAFE/after.log" | grep -E '^PROOF|^test |test result|panicked|assertion' || true
grep -q '^test result: ok. 1 passed' "$SAFE/after.log" || die "the verification failed; see $SAFE/after.log"

step "The restore is proven"
echo "   Logs and the copy of the original files: $SAFE (holds secrets: delete it when done)"
echo "   Next: scripts/test-integration.sh, for the whole suite on the restored stack"
