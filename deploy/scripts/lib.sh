#!/usr/bin/env bash
# Shared by install.sh and the scripts of deploy/scripts/. Source it.
# Written for bash 3.2 (macOS) and later.
set -euo pipefail

# TM_DEPLOY_DIR is set by the host agent, which runs some of these scripts from inside its
# container, where the repository is mounted under another path.
DEPLOY_DIR="${TM_DEPLOY_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
REPO_DIR="$(cd "$DEPLOY_DIR/.." && pwd)"
DATA_DIR="$DEPLOY_DIR/data"
ENV_FILE="$DEPLOY_DIR/.env"
# Name of the Compose project (the `name:` of docker-compose.yml).
COMPOSE_PROJECT="${TM_COMPOSE_PROJECT:-teamtak}"

# uid of the "tak" user of the TAK Server image, and of the user of the app image.
TAK_UID=1001
APP_UID=10001

log() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '   %s\n' "$*"; }
die() { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

rand() { LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c "${1:-32}" || true; }

# ----------------------------------------------------------------------------- TAK Server ZIPs
# tak.gov publishes TAK Server as takserver-docker-hardened-<version>.zip, a new file for
# each version. No version is written in the scripts: the version of a ZIP is read from the
# ZIP itself (tak/version.txt), so a renamed download works too.

# Version of TAK Server in a ZIP ("5.8-RELEASE-103"): its tak/version.txt, or else its file
# name. Fails when neither gives something usable as an image tag.
tak_zip_version() {
  local zip="$1" entry="" v=""
  if command -v unzip > /dev/null; then
    entry="$(unzip -Z1 "$zip" 2>/dev/null | grep -E '^[^/]+/tak/version\.txt$' | head -n 1 || true)"
    [ -z "$entry" ] || v="$(unzip -p "$zip" "$entry" 2>/dev/null | head -c 64 | tr -d '[:space:]' || true)"
  fi
  if [ -z "$v" ]; then
    v="$(basename "$zip" .zip)"; v="${v#takserver-docker-}"; v="${v#hardened-}"
  fi
  case "$v" in [0-9]*) ;; *) return 1 ;; esac
  case "$v" in *[!A-Za-z0-9.-]*) return 1 ;; esac
  printf '%s\n' "$v"
}

# The TAK Server ZIPs at the repository root, oldest version first: "<version> <path>" lines.
tak_zips() {
  local zip v
  for zip in "$REPO_DIR"/takserver-docker-hardened-*.zip; do
    [ -f "$zip" ] || continue
    v="$(tak_zip_version "$zip")" || continue
    printf '%s %s\n' "$v" "$zip"
  done | sort -k1,1V
}

# Path of the ZIP of one version at the repository root; empty when there is none.
tak_zip_for_version() {
  local v zip
  tak_zips | while read -r v zip; do
    if [ "$v" = "$1" ]; then printf '%s\n' "$zip"; break; fi
  done
}

# True when version $1 is newer than version $2.
version_newer() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$1" ]
}

# ----------------------------------------------------------------------------- TeamTAK version

# Version of this checkout, which tags the platform and agent images (teamtak/app:<version>):
# from git, the release tag the checkout is on without its "v" ("1.0.1"), or between two
# releases the last one, the number of commits since and the commit ("1.0.1-3-g28f8b1b").
# Without git or its tags (a ZIP of the sources, a shallow clone): the version of Cargo.toml.
teamtak_version() {
  local v=""
  if command -v git > /dev/null; then
    # safe.directory: install.sh runs as root in a checkout another user owns.
    v="$(git -c safe.directory="$REPO_DIR" -C "$REPO_DIR" describe --tags --match 'v[0-9]*' 2>/dev/null || true)"
    v="${v#v}"
  fi
  [ -n "$v" ] || v="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO_DIR/Cargo.toml" | head -n 1)"
  # What Docker accepts as a tag.
  case "$v" in ''|[!A-Za-z0-9_]*|*[!A-Za-z0-9_.-]*) v=local ;; esac
  printf '%s\n' "${v:0:128}"
}

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
# COMPOSE_FILE names a generated file: written again if it went missing with deploy/data, so
# that Compose can still read the stack (to stop it, for example).
compose() {
  if [ ! -f "$DEPLOY_DIR/$PORTS_COMPOSE_FILE" ]; then mkdir -p "$DATA_DIR"; render_ports_compose; fi
  (cd "$DEPLOY_DIR" && docker compose "$@")
}

# Identifier of the container that runs a service of this stack, empty when there is none.
# Found by the labels Compose sets, so it works where `docker compose` cannot be used (inside
# the agent's container), and never matches a container of another project.
svc_container() {
  docker ps -a -q --filter "label=com.docker.compose.project=$COMPOSE_PROJECT" --filter "label=com.docker.compose.service=$1" | head -n 1
}

# Runs a command in the container of a service, with stdin passed through.
svc_exec() {
  local service="$1" id
  shift
  id="$(svc_container "$service")"
  [ -n "$id" ] || die "no container for the service $service: is the stack up?"
  docker exec -i "$id" "$@"
}

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

# ----------------------------------------------------------------------------- TAK client ports
# TAK Server listens for its clients on one TLS input, 8089 inside its container.
# TAK_CLIENT_PORTS lists the ports of the machine that lead to it ("8089,8443"): the first
# is written in the zips and QR codes, the others are alternatives for networks that block
# it. Docker publishes them through a Compose file generated from that list.

# The Compose file that publishes them, relative to deploy/ (listed in COMPOSE_FILE).
PORTS_COMPOSE_FILE="data/docker-compose.ports.yml"

# The ports of a list, one per line: commas and spaces both separate them.
port_list() { printf '%s\n' "$1" | tr ', \t' '\n\n\n' | grep -v '^$' || true; }

# A list written the way deploy/.env keeps it: "8089,8443".
normalize_ports() { port_list "$1" | paste -s -d , - ; }

# What TeamTAK itself publishes on a port of the machine, in a mode; empty when nothing.
own_port_use() {
  case "$2:$1" in
    *:8446) echo "TAK Server's enrollment port" ;;
    production:80|production:443) echo "the web interface's" ;;
    local:"${WEB_PORT:-8480}") echo "the web interface's" ;;
    local:"${MAILPIT_PORT:-8025}") echo "the mail catcher's" ;;
  esac
}

# Prints why a list of client ports cannot be used, and fails; silent when it can.
# Usage: client_ports_problem <list> <mode>
client_ports_problem() {
  local port seen=" " use
  [ -n "$(port_list "$1")" ] || { echo "at least one port is needed, like 8089"; return 1; }
  for port in $(port_list "$1"); do
    case "$port" in
      *[!0-9]*|0*) echo "$port is not a port number"; return 1 ;;
    esac
    [ "${#port}" -le 5 ] && [ "$port" -le 65535 ] || { echo "$port is not a port number"; return 1; }
    use="$(own_port_use "$port" "$2")"
    [ -z "$use" ] || { echo "port $port is $use"; return 1; }
    case "$seen" in *" $port "*) echo "port $port is listed twice"; return 1 ;; esac
    seen="$seen$port "
  done
}

# Ports of the machine the containers of this stack publish now, one per line.
stack_ports() {
  local id
  for id in $(docker ps -q --filter "label=com.docker.compose.project=$COMPOSE_PROJECT"); do
    docker port "$id" 2>/dev/null || true
  done | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | sort -u
}

# True when a program accepts connections on this port of the machine.
port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# The ports of a list another program of this machine already listens on, separated by
# spaces. Ports this stack publishes itself are not counted.
client_ports_taken() {
  local ours port taken=""
  ours=" $(stack_ports | tr '\n' ' ') "
  for port in $(port_list "$1"); do
    case "$ours" in *" $port "*) continue ;; esac
    if port_in_use "$port"; then taken="$taken${taken:+ }$port"; fi
  done
  printf '%s' "$taken"
}

# Writes deploy/data/docker-compose.ports.yml from TAK_CLIENT_PORTS.
render_ports_compose() {
  local port
  {
    echo "# Generated by install.sh from TAK_CLIENT_PORTS in deploy/.env: change that line and run"
    echo "# ./install.sh again, not this file. Each port of the machine listed here leads to TAK"
    echo "# Server's TLS input for clients, 8089 inside its container."
    echo "services:"
    echo "  takserver:"
    echo "    ports:"
    for port in $(port_list "${TAK_CLIENT_PORTS:-8089}"); do
      echo "      - \"\${BIND_ADDRESS}:$port:8089\""
    done
  } > "$DEPLOY_DIR/$PORTS_COMPOSE_FILE"
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

# Copies the file $1 to deploy/data/tak/$2, in place: a bind-mounted file must keep its
# inode. As root in a container when it cannot be written from here (it may already belong
# to uid 1001).
write_tak_file() {
  local src="$1" out="$DATA_DIR/tak/$2"
  if [ -f "$out" ] && cmp -s "$src" "$out"; then
    :
  elif [ -w "$out" ] || { [ ! -e "$out" ] && [ -w "$DATA_DIR/tak" ]; }; then
    cat "$src" > "$out"
  else
    docker run --rm -u 0 --entrypoint sh -v "$DATA_DIR/tak:/t" -v "$src:/new:ro" "$TAK_IMAGE" -c "cat /new > '/t/$2'"
  fi
}

# Writes deploy/data/tak/CoreConfig.xml from the template and deploy/.env.
# The 8446 connector presents the Let's Encrypt certificate once it has been handed over
# (deploy/scripts/sync-letsencrypt.sh), the private CA's otherwise.
#
# Sets CORE_CONFIG_CHANGED to true when the rendered configuration differs from the one
# rendered last time. TAK Server reads the file only when it starts, and rewrites it then
# (comments dropped, a serverId added): the file itself cannot tell whether the settings
# changed, so a checksum of the last rendering is kept next to it.
CORE_CONFIG_CHANGED=false
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
  local stamp="$DATA_DIR/tak/CoreConfig.rendered.cksum" sum
  sum="$(cksum < "$tmp")"
  CORE_CONFIG_CHANGED=true
  if [ -f "$out" ] && [ "$(cat "$stamp" 2>/dev/null)" = "$sum" ]; then
    CORE_CONFIG_CHANGED=false
  fi
  write_tak_file "$tmp" CoreConfig.xml
  printf '%s\n' "$sum" > "$tmp"
  write_tak_file "$tmp" CoreConfig.rendered.cksum
  rm -f "$tmp"
}

# Waits until a compose service reports healthy. Usage: wait_healthy <service> <seconds>
wait_healthy() {
  local service="$1" timeout="$2" start state id
  start=$(date +%s)
  while :; do
    id="$(svc_container "$service")"
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
