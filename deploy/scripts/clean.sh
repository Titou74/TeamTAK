#!/usr/bin/env bash
# Stops TeamTAK and removes what install.sh created, to start again from nothing.
#
#   clean.sh            containers, volumes, deploy/.env and deploy/data (keeps the images)
#   clean.sh --images   also removes the teamtak/* images
#
# Everything is lost: accounts, teams, certificates, the certificate authority. Only touches
# Docker objects of the "teamtak" project.
#
# The backup archives in deploy/backups/ are kept: they are the way back
# (deploy/scripts/restore.sh). Delete that folder by hand to remove them too.
. "$(dirname "$0")/lib.sh"

IMAGES=0 YES=0
for arg in "$@"; do
  case "$arg" in
    --images) IMAGES=1 ;;
    --yes) YES=1 ;;
    *) die "unknown option: $arg" ;;
  esac
done
if [ "$YES" = 0 ]; then
  printf 'This deletes every TeamTAK account, team and certificate on this machine. Type "delete" to continue: '
  read -r answer
  [ "$answer" = delete ] || die "aborted"
fi

TAK_IMAGE_NAME="$(env_get TAK_IMAGE)"
TAK_DB_IMAGE_NAME="$(env_get TAK_DB_IMAGE)"
# Every version of the platform and agent images (teamtak/app:<version>).
PLATFORM_IMAGES="$(for repo in $PLATFORM_IMAGE_REPOS; do repo_tags "$repo"; done)"
# Every version of TAK Server's images: an upgrade keeps the previous one.
TAK_IMAGES="$(for repo in teamtak/takserver teamtak/takserver-db "${TAK_IMAGE_NAME%:*}" "${TAK_DB_IMAGE_NAME%:*}"; do
  [ -z "$repo" ] || repo_tags "$repo"
done | sort -u)"
if [ -f "$ENV_FILE" ]; then
  compose down -v --remove-orphans
else
  (cd "$DEPLOY_DIR" && docker compose -p teamtak -f docker-compose.yml down -v --remove-orphans 2>/dev/null) || true
fi
# deploy/data holds files owned by container users: remove them from a container.
if [ -d "$DATA_DIR" ]; then
  docker run --rm -u 0 --entrypoint sh -v "$DEPLOY_DIR:/deploy" "${TAK_IMAGE_NAME:-$(printf '%s\n' "$PLATFORM_IMAGES" | grep '^teamtak/app:' | head -n 1)}" -c 'rm -rf /deploy/data' 2>/dev/null || rm -rf "$DATA_DIR"
fi
rm -f "$ENV_FILE"
if [ "$IMAGES" = 1 ]; then
  for image in $TAK_IMAGES $PLATFORM_IMAGES; do
    [ -n "$image" ] && docker image rm "$image" 2>/dev/null || true
  done
fi
echo "Cleaned."
if ls "$DEPLOY_DIR/backups"/teamtak-backup-* > /dev/null 2>&1; then
  echo "Backup archives are still in $DEPLOY_DIR/backups/ (restore one with deploy/scripts/restore.sh)."
fi
