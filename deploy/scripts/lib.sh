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

# git in this checkout, run as the user who owns it. install.sh runs as root: what git would
# write as root (the files of .git, the checked-out files) would then refuse that user's own
# git commands.
repo_git() {
  local owner home
  owner="$(stat -c %U "$REPO_DIR" 2>/dev/null || stat -f %Su "$REPO_DIR" 2>/dev/null || true)"
  if [ "$(id -u)" != 0 ] || [ -z "$owner" ] || [ "$owner" = root ] || [ "$owner" = UNKNOWN ]; then
    git -c safe.directory="$REPO_DIR" -C "$REPO_DIR" "$@"
  elif command -v sudo > /dev/null; then
    sudo -u "$owner" -H git -C "$REPO_DIR" "$@"
  else
    home="$(getent passwd "$owner" | cut -d: -f6 || true)"
    runuser -u "$owner" -- env HOME="${home:-/}" git -C "$REPO_DIR" "$@"
  fi
}

# The release tags this checkout knows, newest first: v<major>.<minor>.<patch>, pre-releases
# (v1.1.0-rc.1) left out, as GitHub leaves them out of its latest release.
release_tags() {
  repo_git tag --list 'v*' --sort=-v:refname | grep -Ex 'v[0-9]+\.[0-9]+\.[0-9]+' || true
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
# TAK_CLIENT_PORTS lists the ports of the machine that lead to it ("8089,4443"): the first
# is written in the zips and QR codes, the others are alternatives for networks that block
# it. Docker publishes them through a Compose file generated from that list.
#
# TAK clients also need TAK Server's API, with their certificate: ATAK and iTAK read their
# channels there (the list of their groups, GET /Marti/api/groups/all), and send files
# through it. They look for it on 8443 of the server, whatever port they stream on, so 8443
# of the machine is TAK Server's own 8443, published as it is and never moved.
TAK_API_PORT=8443

# The Compose file that publishes them, relative to deploy/ (listed in COMPOSE_FILE).
PORTS_COMPOSE_FILE="data/docker-compose.ports.yml"

# The ports of a list, one per line: commas and spaces both separate them.
port_list() { printf '%s\n' "$1" | tr ', \t' '\n\n\n' | grep -v '^$' || true; }

# A list written the way deploy/.env keeps it: "8089,4443".
normalize_ports() { port_list "$1" | paste -s -d , - ; }

# What TeamTAK itself publishes on a port of the machine (TCP), in a mode; empty when
# nothing. The video server's ports count while it is turned on (VIDEO).
own_port_use() {
  case "$2:$1" in
    *:8446) echo "TAK Server's enrollment port" ;;
    *:"$TAK_API_PORT") echo "TAK Server's API port, where TAK clients read their channels" ;;
    production:80|production:443) echo "the web interface's" ;;
    local:"${WEB_PORT:-8480}") echo "the web interface's" ;;
    local:"${MAILPIT_PORT:-8025}") echo "the mail catcher's" ;;
    *)
      if video_on "${VIDEO:-off}"; then
        case "$1" in
          "${VIDEO_RTSP_PORT:-$VIDEO_RTSP_PORT_DEFAULT}") echo "the video server's RTSP port (VIDEO_RTSP_PORT)" ;;
          "${VIDEO_RTMP_PORT:-$VIDEO_RTMP_PORT_DEFAULT}") echo "the video server's RTMP port (VIDEO_RTMP_PORT)" ;;
        esac
      fi ;;
  esac
}

# Prints why a list of client ports cannot be used, and fails; silent when it can.
# Usage: client_ports_problem <list> <mode>
# The port of TAK Server's administration (TAK_ADMIN_PORT) is refused too.
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
    [ "$port" != "$(admin_port "${TAK_ADMIN_PORT:-}")" ] || {
      echo "port $port is TAK Server's administration port (TAK_ADMIN_PORT; --tak-admin-port on a first install)"; return 1; }
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

# Writes deploy/data/docker-compose.ports.yml from TAK_CLIENT_PORTS and TAK_ADMIN_PORT.
render_ports_compose() {
  local port admin
  admin="$(admin_port "${TAK_ADMIN_PORT:-}")"
  {
    echo "# Generated by install.sh from TAK_CLIENT_PORTS and TAK_ADMIN_PORT in deploy/.env: change"
    echo "# those lines and run ./install.sh again, not this file. Each port of the machine listed"
    echo "# under takserver leads to TAK Server's TLS input for clients, 8089 inside its container,"
    echo "# except $TAK_API_PORT: TAK Server's API, where TAK clients read their channels."
    echo "services:"
    echo "  takserver:"
    echo "    ports:"
    for port in $(port_list "${TAK_CLIENT_PORTS:-8089}"); do
      echo "      - \"\${BIND_ADDRESS}:$port:8089\""
    done
    echo "      - \"\${BIND_ADDRESS}:$TAK_API_PORT:8443\""
    if [ -n "$admin" ]; then
      echo "  # TAK Server's administration pages, behind the platform's sign-in ($TAK_ADMIN_CADDY_PORT inside Caddy)."
      echo "  caddy:"
      echo "    ports:"
      echo "      - \"\${BIND_ADDRESS}:$admin:$TAK_ADMIN_CADDY_PORT\""
    fi
  } > "$DEPLOY_DIR/$PORTS_COMPOSE_FILE"
}

# ----------------------------------------------------------------------------- video server
# The video server (MediaMTX, docker-compose.video.yml) runs when VIDEO=on. Devices reach it
# on three ports of the machine: RTSP and RTMP over TCP, SRT over UDP. Inside its container
# they stay 8554, 1935 and 8890.
VIDEO_COMPOSE_FILE="docker-compose.video.yml"
VIDEO_RTSP_PORT_DEFAULT=8554
VIDEO_RTMP_PORT_DEFAULT=1935
VIDEO_SRT_PORT_DEFAULT=8890

# True for a switch that is on (VIDEO=on).
video_on() {
  case "${1:-}" in on|ON|On|true|yes) return 0 ;; esac
  return 1
}

# The video server's TCP ports, separated by spaces; empty when it is off.
video_tcp_ports() {
  video_on "${VIDEO:-off}" || return 0
  printf '%s %s' "${VIDEO_RTSP_PORT:-$VIDEO_RTSP_PORT_DEFAULT}" "${VIDEO_RTMP_PORT:-$VIDEO_RTMP_PORT_DEFAULT}"
}

# Prints why the video server's ports cannot be used, and fails; silent when they can, or
# when it is off. Usage: video_ports_problem <mode>
video_ports_problem() {
  local rtsp="${VIDEO_RTSP_PORT:-$VIDEO_RTSP_PORT_DEFAULT}" rtmp="${VIDEO_RTMP_PORT:-$VIDEO_RTMP_PORT_DEFAULT}"
  local srt="${VIDEO_SRT_PORT:-$VIDEO_SRT_PORT_DEFAULT}" port use
  video_on "${VIDEO:-off}" || return 0
  for port in "$rtsp" "$rtmp" "$srt"; do
    case "$port" in ''|*[!0-9]*|0*) echo "$port is not a port number"; return 1 ;; esac
    [ "${#port}" -le 5 ] && [ "$port" -le 65535 ] || { echo "$port is not a port number"; return 1; }
  done
  [ "$rtsp" != "$rtmp" ] || { echo "VIDEO_RTSP_PORT and VIDEO_RTMP_PORT are the same port, $rtsp"; return 1; }
  # Against everything else TeamTAK publishes over TCP, the video server itself left out
  # (the subshell keeps VIDEO=off to itself).
  for port in "$rtsp" "$rtmp"; do
    use="$(VIDEO=off own_port_use "$port" "$1")"
    [ -z "$use" ] || { echo "port $port is $use"; return 1; }
    if port_list "${TAK_CLIENT_PORTS:-8089}" | grep -qx "$port"; then echo "port $port is a TAK client port (TAK_CLIENT_PORTS)"; return 1; fi
    [ "$port" != "$(admin_port "${TAK_ADMIN_PORT:-}")" ] || { echo "port $port is TAK Server's administration port (TAK_ADMIN_PORT)"; return 1; }
  done
  # Caddy answers HTTP/3 on 443/udp in production.
  if [ "$1" = production ] && [ "$srt" = 443 ]; then echo "port 443/udp is the web interface's"; return 1; fi
  return 0
}

# The text of install.sh's summary: where devices reach the video server.
video_text() {
  if video_on "${VIDEO:-off}"; then
    printf 'RTSP %s:%s, RTMP %s:%s, SRT %s:%s/udp' "$PUBLIC_HOST" "${VIDEO_RTSP_PORT:-$VIDEO_RTSP_PORT_DEFAULT}" \
      "$PUBLIC_HOST" "${VIDEO_RTMP_PORT:-$VIDEO_RTMP_PORT_DEFAULT}" "$PUBLIC_HOST" "${VIDEO_SRT_PORT:-$VIDEO_SRT_PORT_DEFAULT}"
  else
    printf 'off (VIDEO=off)'
  fi
}

# ----------------------------------------------------------------------------- TAK Server administration
# TAK Server's own administration pages are served by Caddy on TAK_ADMIN_PORT of the machine
# (8444 by default), to the platform's administrators only: Caddy asks the platform about the
# browser's session, then reaches TAK Server's 8443 over the Docker network with a certificate
# of its own, declared administrator. "off": the port is not published.
#
# Until 1.0.7 the default was 8443, which TAK clients need for TAK Server's API: install.sh
# moves an installation that still has it.
TAK_ADMIN_PORT_DEFAULT=8444
# The port the administration site listens on inside Caddy (Caddyfile.*).
TAK_ADMIN_CADDY_PORT=8444

# The port of TAK_ADMIN_PORT, empty when it is turned off.
admin_port() {
  case "${1:-}" in ''|off|OFF|Off|none) ;; *) printf '%s' "$1" ;; esac
}

# Prints why a value of TAK_ADMIN_PORT cannot be used, and fails; silent when it can.
# Usage: admin_port_problem <value> <mode>. A client port is refused by client_ports_problem.
admin_port_problem() {
  local port use
  port="$(admin_port "$1")"
  [ -n "$port" ] || return 0
  case "$port" in
    *[!0-9]*|0*) echo "$port is not a port number, nor off"; return 1 ;;
  esac
  [ "${#port}" -le 5 ] && [ "$port" -le 65535 ] || { echo "$port is not a port number"; return 1; }
  use="$(own_port_use "$port" "$2")"
  [ -z "$use" ] || { echo "port $port is $use"; return 1; }
}

# Where Caddy finds the certificate it presents to TAK Server, its key and the CA chain.
ADMIN_UI_CADDY_DIR="data/caddy/tak-admin"

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

# Declares administrator in TAK Server the platform's API certificate and the one Caddy
# presents for the administration pages. The result is stored in UserAuthenticationFile.xml,
# which lives in deploy/data/tak/.
grant_admin_certificates() {
  local name
  for name in "$TAK_API_CERT_NAME" "${TAK_ADMIN_UI_CERT_NAME:-teamtak-admin-ui}"; do
    compose exec -T takserver bash -c "cd /opt/tak && java -jar utils/UserManager.jar certmod -A certs/files/$name.pem" 2>&1 \
      | grep -v -E 'JDK_JAVA_OPTIONS|^--add-opens|^$' | tail -n 5
  done
}

# ----------------------------------------------------------------------------- old images
# Docker keeps an image until it is removed, even once no container runs it: each update would
# otherwise leave behind a few hundred MB (the platform, the agent, Caddy...) to several GB
# (TAK Server).

# The platform's and agent's images, tagged with TeamTAK's version (docker-compose.yml).
PLATFORM_IMAGE_REPOS="teamtak/app teamtak/agent"

# Identifiers of the images the containers of this stack run (sha256:...), one per line.
stack_image_ids() {
  local id
  for id in $(docker ps -a -q --filter "label=com.docker.compose.project=$COMPOSE_PROJECT"); do
    docker inspect --format '{{.Image}}' "$id" 2>/dev/null || true
  done | sort -u
}

# The tags of the images of a repository ("teamtak/app:1.3.1"), one per line.
repo_tags() {
  docker image ls --filter "reference=$1" --format '{{.Repository}}:{{.Tag}}' | grep -v ':<none>$' || true
}

# Removes what an update left behind, once the stack runs on its new images:
# - the platform's and agent's images of other versions than TEAMTAK_VERSION;
# - TAK Server's images of other versions than the installed one, except the newest of them:
#   after --upgrade-tak, the version to go back to stays ready, without its ZIP;
# - among the images given in $1 (identifiers, separated by spaces or newlines: what the stack
#   ran before the update), those it runs no longer: a version of Caddy, PostgreSQL or the
#   video server that TeamTAK no longer names, the image a rebuild under the same tag left
#   untagged, the image of a service turned off;
# - the images Compose built for this stack and later left untagged.
# Never forced: Docker refuses to remove an image a container still uses, of this stack or of
# another project, and one that carries several tags. Prints what it removes.
remove_old_images() {
  local before="${1:-}" tak_repo="${TAK_IMAGE%:*}" tak_db_repo="${TAK_DB_IMAGE%:*}"
  local prev keep repo ref id tags tag now short
  # TAK Server's newest version other than the installed one.
  prev="$( { repo_tags "$tak_repo"; repo_tags "$tak_db_repo"; } | sed 's/.*://' \
    | { grep -vxF "${TAK_IMAGE##*:}" || true; } | sort -V | tail -n 1)"
  keep=" $TAK_IMAGE $TAK_DB_IMAGE "
  [ -z "$prev" ] || keep="$keep$tak_repo:$prev $tak_db_repo:$prev "
  for repo in $PLATFORM_IMAGE_REPOS; do keep="$keep$repo:$TEAMTAK_VERSION "; done

  # By tag: the other versions of the images built here.
  for ref in $(for repo in $PLATFORM_IMAGE_REPOS "$tak_repo" "$tak_db_repo"; do repo_tags "$repo"; done); do
    case "$keep" in *" $ref "*) continue ;; esac
    docker image rm "$ref" > /dev/null 2>&1 && info "removed an image no longer used: $ref" || true
  done

  # By identifier: what the stack ran before and runs no longer, what Compose left untagged.
  now=" $(stack_image_ids | tr '\n' ' ') "
  for id in $before $(docker image ls -q --no-trunc --filter dangling=true --filter "label=com.docker.compose.project=$COMPOSE_PROJECT"); do
    case "$now" in *" $id "*) continue ;; esac
    tags="$(docker image inspect --format '{{join .RepoTags " "}}' "$id" 2>/dev/null)" || continue
    for tag in $tags; do
      case "$keep" in *" $tag "*) continue 2 ;; esac
    done
    short="${id#sha256:}"
    docker image rm "$id" > /dev/null 2>&1 && info "removed an image no longer used: ${tags:-${short:0:12}, untagged}" || true
  done
}
