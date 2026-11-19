#!/usr/bin/env bash
# Shared by install.sh and the scripts of deploy/scripts/. Source it.
# Written for bash 3.2 (macOS) and later.
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(cd "$DEPLOY_DIR/.." && pwd)"
DATA_DIR="$DEPLOY_DIR/data"
ENV_FILE="$DEPLOY_DIR/.env"

# uid of the "tak" user of the TAK Server image, and of the user of the app image.
TAK_UID=1001
APP_UID=10001

log() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '   %s\n' "$*"; }
die() { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

rand() { LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c "${1:-32}" || true; }

# Loads deploy/.env into the environment. The file is read as KEY=VALUE lines, the way
# Docker Compose reads it, not executed: values may hold spaces and shell characters.
load_env() {
  [ -f "$ENV_FILE" ] || die "$ENV_FILE is missing: run ./install.sh first"
  local line
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    export "${line%%=*}=${line#*=}"
  done < "$ENV_FILE"
}

# Docker Compose for the stack. Run from deploy/: the COMPOSE_FILE list of .env is relative
# to the current directory.
compose() { (cd "$DEPLOY_DIR" && docker compose "$@"); }

# Value of KEY in deploy/.env, empty when absent.
env_get() {
  [ -f "$ENV_FILE" ] || return 0
  sed -n "s/^$1=//p" "$ENV_FILE" | tail -n 1
}

# Sets KEY=VALUE in deploy/.env, replacing an existing line. Values must not contain newlines.
env_set() {
  local key="$1" value="$2" tmp
  tmp="$(mktemp)"
  if [ -f "$ENV_FILE" ]; then grep -v "^$key=" "$ENV_FILE" > "$tmp" || true; fi
  printf '%s=%s\n' "$key" "$value" >> "$tmp"
  cat "$tmp" > "$ENV_FILE"
  rm -f "$tmp"
}

# Runs a command as root inside a throwaway container with deploy/data mounted on /d.
# Used for ownership: bind-mounted files must belong to the uid of the container that uses
# them, whoever runs the install.
as_root_in_data() {
  docker run --rm -u 0 --entrypoint sh -v "$DATA_DIR:/d" "$TAK_IMAGE" -c "$1"
}

# Gives deploy/data/tak to TAK Server (uid 1001) and deploy/data/app to the platform.
fix_ownership() {
  as_root_in_data "
    chown -R $TAK_UID:0 /d/tak && chmod -R u+rwX,g+rwX /d/tak
    chown -R $APP_UID:0 /d/app && chmod -R u+rX,g+rX,o-rwx /d/app
  "
}

# Writes deploy/data/tak/CoreConfig.xml from the template and deploy/.env.
# The 8446 connector presents the Let's Encrypt certificate once it has been handed over
# (deploy/scripts/sync-letsencrypt.sh), the private CA's otherwise.
render_core_config() {
  local attrs="" note="Certificate: TAK Server's own, signed by the private CA."
  if [ "${TAK_ENROLL_LETSENCRYPT:-false}" = true ] && [ -f "$DATA_DIR/tak/certs/letsencrypt.jks" ]; then
    attrs=" keystore=\"JKS\" keystoreFile=\"/opt/tak/certs/files/letsencrypt.jks\" keystorePass=\"$TAK_CERT_PASSWORD\""
    note="Certificate: Let's Encrypt, so a QR code can enroll a phone that has no truststore yet."
  fi
  local out="$DATA_DIR/tak/CoreConfig.xml" tmp
  tmp="$(mktemp)"
  sed -e "s|__LDAP_BASE_DN__|$LDAP_BASE_DN|g" \
      -e "s|__LDAP_TAKSERVER_PW__|$LDAP_TAKSERVER_PW|g" \
      -e "s|__LDAP_UPDATE_INTERVAL__|${LDAP_UPDATE_INTERVAL:-60}|g" \
      -e "s|__DB_PASSWORD__|$TAK_DB_PASSWORD|g" \
      -e "s|__CERT_PASSWORD__|$TAK_CERT_PASSWORD|g" \
      -e "s|__INT_CA_NAME__|$INT_CA_NAME|g" \
      -e "s|__SERVER_CERT_NAME__|$SERVER_CERT_NAME|g" \
      -e "s|__CERT_VALIDITY_DAYS__|$CERT_VALIDITY_DAYS|g" \
      -e "s|__ENROLL_CONNECTOR_NOTE__|$note|g" \
      -e "s|__ENROLL_KEYSTORE_ATTRS__|$attrs|g" \
      "$DEPLOY_DIR/takserver/CoreConfig.tpl.xml" > "$tmp"
  # Rewritten in place: the file is bind-mounted into the container (its inode must not
  # change), and it may already belong to uid 1001.
  if [ -f "$out" ] && ! [ -w "$out" ]; then
    docker run --rm -u 0 --entrypoint sh -v "$DATA_DIR/tak:/t" -v "$tmp:/new.xml:ro" "$TAK_IMAGE" -c 'cat /new.xml > /t/CoreConfig.xml'
  else
    cat "$tmp" > "$out"
  fi
  rm -f "$tmp"
}

# Waits until a compose service reports healthy. Usage: wait_healthy <service> <seconds>
wait_healthy() {
  local service="$1" timeout="$2" start state id
  start=$(date +%s)
  while :; do
    id="$(compose ps -q "$service" 2>/dev/null | head -n 1)"
    state="absent"
    [ -n "$id" ] && state="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$id" 2>/dev/null || echo absent)"
    [ "$state" = healthy ] && return 0
    if [ $(( $(date +%s) - start )) -ge "$timeout" ]; then
      echo "   $service is not healthy after ${timeout}s (state: $state)" >&2
      return 1
    fi
    sleep 5
  done
}

# Declares the platform's API certificate as administrator in TAK Server. The result is
# stored in UserAuthenticationFile.xml, which lives in deploy/data/tak/.
grant_api_certificate() {
  compose exec -T takserver bash -c "cd /opt/tak && java -jar utils/UserManager.jar certmod -A certs/files/$TAK_API_CERT_NAME.pem" 2>&1 \
    | grep -v -E 'JDK_JAVA_OPTIONS|^--add-opens|^$' | tail -n 5
}
