#!/usr/bin/env bash
# Stops the stack and removes what the spike created.
#   99-clean.sh            containers, volumes and generated files (keeps images and the extracted ZIP)
#   99-clean.sh --images   also removes the three tak-spike/* images and the extracted ZIP
# Never touches any image, volume or Docker cache that does not belong to the spike.
. "$(dirname "$0")/lib.sh"
if [ -f "$WORK_DIR/.env" ]; then
  compose down -v --remove-orphans
else
  docker compose --project-directory "$SPIKE_DIR" -p tak-spike down -v --remove-orphans 2>/dev/null || true
fi
rm -rf "$WORK_DIR/tak" "$WORK_DIR/clients" "$WORK_DIR/packages" "$WORK_DIR/results" "$WORK_DIR/.env" "$SPIKE_DIR/.env"
if [ "${1:-}" = "--images" ]; then
  docker image rm "$IMG_CA" "$IMG_TAK" "$IMG_DB" 2>/dev/null || true
  rm -rf "$TAK_SRC"
fi
echo "Cleaned."
