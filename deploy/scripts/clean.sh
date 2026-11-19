#!/usr/bin/env bash
# Stops TAK Manager and removes what install.sh created, to start again from nothing.
#
#   clean.sh            containers, volumes, deploy/.env and deploy/data (keeps the images)
#   clean.sh --images   also removes the tak-manager/* images
#
# Everything is lost: accounts, teams, certificates, the certificate authority. Only touches
# Docker objects of the "tak-manager" project.
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
  printf 'This deletes every TAK Manager account, team and certificate on this machine. Type "delete" to continue: '
  read -r answer
  [ "$answer" = delete ] || die "aborted"
fi

TAK_IMAGE_NAME="$(env_get TAK_IMAGE)"
TAK_DB_IMAGE_NAME="$(env_get TAK_DB_IMAGE)"
if [ -f "$ENV_FILE" ]; then
  compose down -v --remove-orphans
else
  (cd "$DEPLOY_DIR" && docker compose -p tak-manager -f docker-compose.yml down -v --remove-orphans 2>/dev/null) || true
fi
# deploy/data holds files owned by container users: remove them from a container.
if [ -d "$DATA_DIR" ]; then
  docker run --rm -u 0 --entrypoint sh -v "$DEPLOY_DIR:/deploy" "${TAK_IMAGE_NAME:-tak-manager/app:local}" -c 'rm -rf /deploy/data' 2>/dev/null || rm -rf "$DATA_DIR"
fi
rm -f "$ENV_FILE"
if [ "$IMAGES" = 1 ]; then
  for image in "$TAK_IMAGE_NAME" "$TAK_DB_IMAGE_NAME" tak-manager/app:local; do
    [ -n "$image" ] && docker image rm "$image" 2>/dev/null || true
  done
fi
echo "Cleaned."
