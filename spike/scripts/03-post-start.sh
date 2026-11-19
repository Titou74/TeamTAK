#!/usr/bin/env bash
# Run after `docker compose up -d`: waits until TAK Server is ready, then declares the
# admin.pem certificate as administrator (UserManager.jar from the ZIP).
# Run it again each time the takserver container is recreated: the authorisation is
# stored in /opt/tak/UserAuthenticationFile.xml, inside the container.
. "$(dirname "$0")/lib.sh"
load_env

log "Waiting for TAK Server (healthcheck)"
for _ in $(seq 1 60); do
  state=$(docker inspect -f '{{.State.Health.Status}}' tak-spike-takserver-1 2>/dev/null || echo absent)
  [ "$state" = healthy ] && break
  sleep 5
done
[ "$state" = healthy ] || { echo "TAK Server is not ready (state: $state)" >&2; exit 1; }
echo "TAK Server is ready"

log "Declaring admin.pem as administrator"
compose exec -T takserver bash -c 'cd /opt/tak && java -jar utils/UserManager.jar certmod -A certs/files/admin.pem' 2>&1 \
  | grep -v -E 'JDK_JAVA_OPTIONS|^--add-opens|^$' | tail -5
