#!/usr/bin/env bash
# Installs TAK Manager from nothing: builds TAK Server from the tak.gov ZIP, generates the
# secrets and certificates, and starts the whole stack with Docker Compose.
#
# Safe to run again: whatever already exists (settings, certificates, images) is kept, the
# configuration is rendered again and the stack is brought up to date.
#
#   ./install.sh --local --admin-email you@example.org
#   ./install.sh --domain tak.example.org --admin-email you@example.org \
#                --smtp-host smtp-relay.brevo.com --smtp-username ... --smtp-password ... \
#                --mail-from "TAK Manager <no-reply@example.org>"
#
# See README.md for every option.
. "$(dirname "$0")/deploy/scripts/lib.sh"

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]

Mode (first run only; later runs reuse deploy/.env):
  --local                    Local mode: plain HTTP on localhost, mail catcher, private CA.
                             Default when --domain is not given.
  --domain <name>            Production mode: HTTPS with Let's Encrypt for this domain, which
                             must already point to this machine. Also TAK Server's public name.
  --host <name-or-ip>        Local mode only: address TAK clients use to reach this machine
                             (default: localhost). With a LAN address, ports listen on all
                             interfaces so phones can connect.

Settings (first run only):
  --admin-email <address>    First platform administrator.
  --acme-email <address>     Contact address given to Let's Encrypt (default: the admin's).
  --cert-validity-days <n>   Validity of TAK client certificates (default: 365).
  --tak-enroll-letsencrypt   Production: also give the Let's Encrypt certificate to TAK
                             Server's enrollment port (8446), which QR-code enrollment needs.
  --smtp-host <host> --smtp-port <port> --smtp-username <u> --smtp-password <p>
  --smtp-tls <starttls|tls|none> --mail-from <address>
                             Outgoing e-mail (production). Local mode uses the mail catcher.

Other:
  --zip <path>               TAK Server ZIP (default: takserver-docker-hardened-*.zip at the
                             repository root).
  --no-start                 Prepare everything but do not start the stack.
  -h, --help
EOF
}

MODE_ARG="" DOMAIN_ARG="" HOST_ARG="" ADMIN_EMAIL_ARG="" ACME_EMAIL_ARG="" VALIDITY_ARG=""
ENROLL_LE_ARG="" ZIP_ARG="" NO_START=0
SMTP_HOST_ARG="" SMTP_PORT_ARG="" SMTP_USERNAME_ARG="" SMTP_PASSWORD_ARG="" SMTP_TLS_ARG="" MAIL_FROM_ARG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --local) MODE_ARG=local ;;
    --domain) MODE_ARG=production; DOMAIN_ARG="$2"; shift ;;
    --host) HOST_ARG="$2"; shift ;;
    --admin-email) ADMIN_EMAIL_ARG="$2"; shift ;;
    --acme-email) ACME_EMAIL_ARG="$2"; shift ;;
    --cert-validity-days) VALIDITY_ARG="$2"; shift ;;
    --tak-enroll-letsencrypt) ENROLL_LE_ARG=true ;;
    --smtp-host) SMTP_HOST_ARG="$2"; shift ;;
    --smtp-port) SMTP_PORT_ARG="$2"; shift ;;
    --smtp-username) SMTP_USERNAME_ARG="$2"; shift ;;
    --smtp-password) SMTP_PASSWORD_ARG="$2"; shift ;;
    --smtp-tls) SMTP_TLS_ARG="$2"; shift ;;
    --mail-from) MAIL_FROM_ARG="$2"; shift ;;
    --zip) ZIP_ARG="$2"; shift ;;
    --no-start) NO_START=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
  shift
done

# ----------------------------------------------------------------------------- preflight

log "Checking the machine"
command -v docker > /dev/null || die "Docker is required"
docker compose version > /dev/null 2>&1 || die "Docker Compose v2 is required (docker compose)"
docker info > /dev/null 2>&1 || die "Docker is not running, or this user cannot use it"
if [ "$(uname -s)" = Linux ] && [ "$(id -u)" != 0 ]; then
  die "on Linux, run this script as root (sudo ./install.sh ...): it sets the ownership of files mounted into the containers"
fi
info "Docker $(docker version --format '{{.Server.Version}}'), $(docker compose version --short), $(uname -s) $(uname -m)"

# ----------------------------------------------------------------------------- settings

mkdir -p "$DATA_DIR"
if [ ! -f "$ENV_FILE" ]; then
  log "Creating $ENV_FILE"
  MODE="${MODE_ARG:-local}"
  ZIP="${ZIP_ARG:-$(ls "$REPO_DIR"/takserver-docker-hardened-*.zip 2>/dev/null | head -n 1)}"
  [ -n "$ZIP" ] && [ -f "$ZIP" ] || die "TAK Server ZIP not found: put takserver-docker-hardened-<version>.zip at the repository root, or pass --zip"
  TAK_VERSION="$(basename "$ZIP" .zip)"; TAK_VERSION="${TAK_VERSION#takserver-docker-hardened-}"
  VALIDITY="${VALIDITY_ARG:-365}"
  case "$VALIDITY" in ''|*[!0-9]*) die "--cert-validity-days must be a number of days" ;; esac

  if [ "$MODE" = production ]; then
    [ -n "$DOMAIN_ARG" ] || die "--domain needs a domain name"
    PUBLIC_HOST="$DOMAIN_ARG"
    PUBLIC_URL="https://$DOMAIN_ARG"
    BIND_ADDRESS=0.0.0.0
    COOKIE_SECURE=true
    COMPOSE_FILES="docker-compose.yml:docker-compose.production.yml"
    # Inside the Docker network the public name resolves to the takserver container, so the
    # platform can verify whichever certificate 8446 presents (private CA or Let's Encrypt).
    TAK_ENROLL_URL="https://$DOMAIN_ARG:8446"
    TAK_NETWORK_ALIAS="$DOMAIN_ARG"
    SMTP_HOST="$SMTP_HOST_ARG" SMTP_PORT="${SMTP_PORT_ARG:-587}" SMTP_TLS="${SMTP_TLS_ARG:-starttls}"
    SMTP_USERNAME="$SMTP_USERNAME_ARG" SMTP_PASSWORD="$SMTP_PASSWORD_ARG"
    MAIL_FROM="${MAIL_FROM_ARG:-TAK Manager <no-reply@$DOMAIN_ARG>}"
    LOGIN_MAX_CODES_PER_IP=30
    LOGIN_MAX_FAILURES_PER_IP=30
  else
    PUBLIC_HOST="${HOST_ARG:-localhost}"
    PUBLIC_URL="http://$PUBLIC_HOST:8480"
    if [ "$PUBLIC_HOST" = localhost ] || [ "$PUBLIC_HOST" = 127.0.0.1 ]; then BIND_ADDRESS=127.0.0.1; else BIND_ADDRESS=0.0.0.0; fi
    COOKIE_SECURE=false
    COMPOSE_FILES="docker-compose.yml:docker-compose.local.yml"
    TAK_ENROLL_URL="https://takserver:8446"
    TAK_NETWORK_ALIAS="takserver.internal"
    SMTP_HOST="${SMTP_HOST_ARG:-mailpit}" SMTP_PORT="${SMTP_PORT_ARG:-1025}" SMTP_TLS="${SMTP_TLS_ARG:-none}"
    SMTP_USERNAME="$SMTP_USERNAME_ARG" SMTP_PASSWORD="$SMTP_PASSWORD_ARG"
    MAIL_FROM="${MAIL_FROM_ARG:-TAK Manager <tak-manager@localhost>}"
    # The integration tests sign in many accounts, and submit wrong codes, from one address;
    # replaying them must not lock that address out. The per-address limit stays as in
    # production, and is tested.
    LOGIN_MAX_CODES_PER_IP=1000
    LOGIN_MAX_FAILURES_PER_IP=1000
  fi
  [ "${ENROLL_LE_ARG:-false}" = true ] && [ "$MODE" != production ] && die "--tak-enroll-letsencrypt needs --domain"

  cat > "$ENV_FILE" <<ENVEOF
# Generated by install.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ). Holds secrets: never version or share this file.
# After editing it, run ./install.sh again to apply the change.

# --- Mode and addresses
MODE=$MODE
COMPOSE_FILE=$COMPOSE_FILES
# Name or address users and TAK clients reach this machine by
PUBLIC_HOST=$PUBLIC_HOST
PUBLIC_URL=$PUBLIC_URL
# Interface the published ports listen on
BIND_ADDRESS=$BIND_ADDRESS
COOKIE_SECURE=$COOKIE_SECURE
ADMIN_EMAIL=$ADMIN_EMAIL_ARG
ACME_EMAIL=${ACME_EMAIL_ARG:-$ADMIN_EMAIL_ARG}

# --- TAK Server
TAK_VERSION=$TAK_VERSION
TAK_IMAGE=tak-manager/takserver:$TAK_VERSION
TAK_DB_IMAGE=tak-manager/takserver-db:$TAK_VERSION
# Validity of the client certificates issued by enrollment, in days
CERT_VALIDITY_DAYS=$VALIDITY
# Safety net only: the platform pushes group changes itself. Seconds.
LDAP_UPDATE_INTERVAL=60
ROOT_CA_NAME=tak-manager-root-ca
INT_CA_NAME=tak-manager-ca
SERVER_CERT_NAME=takserver
TAK_API_CERT_NAME=tak-manager-api
# true: port 8446 presents the Let's Encrypt certificate (needed by QR-code enrollment)
TAK_ENROLL_LETSENCRYPT=${ENROLL_LE_ARG:-false}
TAK_ENROLL_URL=$TAK_ENROLL_URL
TAK_NETWORK_ALIAS=$TAK_NETWORK_ALIAS
# Name of the connection in the phones' server list
TAK_CONNECTION_NAME=TAK Manager
# PKCS#12 format of the certificates in zips: modern (AES-256) or legacy (Android 12 and older)
P12_FORMAT=modern
# Java heap sizes in MB; empty lets TAK Server size them from the machine's RAM
TAK_API_MAX_HEAP=
TAK_MESSAGING_MAX_HEAP=
TAK_CONFIG_MAX_HEAP=
TAK_PLUGIN_MANAGER_MAX_HEAP=
TAK_RETENTION_MAX_HEAP=

# --- E-mail
SMTP_HOST=$SMTP_HOST
SMTP_PORT=$SMTP_PORT
SMTP_USERNAME=$SMTP_USERNAME
SMTP_PASSWORD=$SMTP_PASSWORD
# starttls, tls or none
SMTP_TLS=$SMTP_TLS
MAIL_FROM=$MAIL_FROM

# --- Limits
DEFAULT_MEMBER_CAP=10
LOGIN_MAX_CODES_PER_EMAIL=5
LOGIN_MAX_CODES_PER_IP=$LOGIN_MAX_CODES_PER_IP
LOGIN_MAX_FAILURES_PER_IP=$LOGIN_MAX_FAILURES_PER_IP
RECONCILE_INTERVAL_SECONDS=60

# --- Directory
LDAP_BASE_DN=dc=takmanager,dc=internal

# --- Secrets (generated)
APP_SECRET=$(rand 48)
POSTGRES_PASSWORD=$(rand)
LDAP_ROOT_PW=$(rand)
LDAP_TAKSERVER_PW=$(rand)
LDAP_MANAGER_PW=$(rand)
TAK_DB_PASSWORD=$(rand)
# Password of TAK Server's keystores and CA keys
TAK_CERT_PASSWORD=$(rand)
ENVEOF
  chmod 600 "$ENV_FILE"
else
  info "keeping existing settings: $ENV_FILE"
  [ -z "$MODE_ARG$DOMAIN_ARG$HOST_ARG$VALIDITY_ARG" ] || info "note: mode, domain, host and validity options are only read on the first run; edit $ENV_FILE to change them"
fi
load_env
ZIP="${ZIP_ARG:-$REPO_DIR/takserver-docker-hardened-$TAK_VERSION.zip}"
info "mode: $MODE — public host: $PUBLIC_HOST — web: $PUBLIC_URL — client certificates: $CERT_VALIDITY_DAYS days"
if [ "$MODE" = production ] && [ -z "$SMTP_HOST" ]; then
  info "WARNING: SMTP_HOST is empty in $ENV_FILE: login codes cannot be e-mailed until it is set."
fi

# ----------------------------------------------------------------------------- TAK Server images

log "TAK Server images"
"$DEPLOY_DIR/scripts/build-tak-images.sh" "$ZIP" "$TAK_IMAGE" "$TAK_DB_IMAGE"

# ----------------------------------------------------------------------------- certificates

mkdir -p "$DATA_DIR/tak/certs" "$DATA_DIR/tak/logs" "$DATA_DIR/app/tak" "$DATA_DIR/caddy/data" "$DATA_DIR/caddy/config"

if [ ! -f "$DATA_DIR/tak/certs/$SERVER_CERT_NAME.jks" ]; then
  log "Generating the certificate authority and TAK Server's certificates"
  # Names the server certificate must carry besides "takserver": it is reached by its
  # public name from phones and by a Docker network name from the platform.
  alt="DNS.2 = localhost"
  dns_n=3 ip_n=1
  for name in "$PUBLIC_HOST" "$TAK_NETWORK_ALIAS" 127.0.0.1; do
    case "$alt" in *"= $name"*) continue ;; esac
    if echo "$name" | grep -Eq '^[0-9]{1,3}(\.[0-9]{1,3}){3}$'; then
      alt="$alt
IP.$ip_n = $name"; ip_n=$((ip_n + 1))
    else
      alt="$alt
DNS.$dns_n = $name"; dns_n=$((dns_n + 1))
    fi
  done
  # Generated inside a container and copied out with `docker cp`, as the ZIP's README does:
  # no bind mount, so no question of who may write where (phase 0 report, section 11).
  cid="$(docker create -i --entrypoint bash -w /opt/tak/certs \
    -e ROOT_CA_NAME="$ROOT_CA_NAME" -e INT_CA_NAME="$INT_CA_NAME" \
    -e SERVER_CERT_NAME="$SERVER_CERT_NAME" -e API_CERT_NAME="$TAK_API_CERT_NAME" \
    -e EXTRA_ALT_NAMES="$alt" \
    -e INT_CA_DAYS=3650 -e SERVER_CERT_DAYS=730 -e API_CERT_DAYS=730 \
    -e CAPASS="$TAK_CERT_PASSWORD" -e PASS="$TAK_CERT_PASSWORD" \
    -e STATE=XX -e CITY=TAK -e ORGANIZATIONAL_UNIT=TAKManager \
    "$TAK_IMAGE" -s)"
  if ! docker start -a -i "$cid" < "$DEPLOY_DIR/takserver/make-certs.sh" > "$DATA_DIR/certs.log" 2>&1; then
    tail -n 30 "$DATA_DIR/certs.log" >&2
    docker rm "$cid" > /dev/null
    die "certificate generation failed"
  fi
  sed -n '/^--- certificates/,$p' "$DATA_DIR/certs.log" | sed 's/^/   /'
  docker cp "$cid:/opt/tak/certs/files/." "$DATA_DIR/tak/certs/"
  docker rm "$cid" > /dev/null
  rm -f "$DATA_DIR/certs.log"
else
  info "certificates already present in $DATA_DIR/tak/certs"
fi

# What the platform needs to call TAK Server: the CA chain (intermediate and root), and its
# own certificate and key.
cp "$DATA_DIR/tak/certs/$INT_CA_NAME.pem" "$DATA_DIR/app/tak/ca-chain.pem"
cp "$DATA_DIR/tak/certs/$TAK_API_CERT_NAME.pem" "$DATA_DIR/app/tak/api-cert.pem"
cp "$DATA_DIR/tak/certs/$TAK_API_CERT_NAME-plain.key" "$DATA_DIR/app/tak/api-key.pem"

# ----------------------------------------------------------------------------- TAK Server configuration

log "TAK Server configuration"
# Provided before the first start: three of TAK Server's processes each create it when it
# is missing, and the one that loses the race dies (phase 0 report, section 11).
if [ ! -f "$DATA_DIR/tak/TAKIgniteConfig.xml" ]; then
  docker run --rm --entrypoint cat "$TAK_IMAGE" /opt/tak/TAKIgniteConfig.example.xml > "$DATA_DIR/tak/TAKIgniteConfig.xml"
fi
# Starts empty; grant_api_certificate fills it. Kept here so the administrator role of the
# platform's certificate survives a re-creation of the container.
if [ ! -f "$DATA_DIR/tak/UserAuthenticationFile.xml" ]; then
  printf '%s\n' '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' \
    '<UserAuthenticationFile xmlns="http://bbn.com/marti/xml/bindings"/>' > "$DATA_DIR/tak/UserAuthenticationFile.xml"
fi
render_core_config
fix_ownership
info "CoreConfig.xml rendered (client certificates valid $CERT_VALIDITY_DAYS days, 8443 not published)"

# ----------------------------------------------------------------------------- platform image

log "Building the platform image (first build: several minutes)"
compose build app > "$DATA_DIR/build-app.log" 2>&1 || { tail -n 40 "$DATA_DIR/build-app.log" >&2; die "could not build the platform image"; }
rm -f "$DATA_DIR/build-app.log"
info "tak-manager/app:local built"

if [ "$NO_START" = 1 ]; then
  log "Prepared. Start with: cd deploy && docker compose up -d"
  exit 0
fi

# ----------------------------------------------------------------------------- start

log "Starting the stack"
compose up -d --remove-orphans 2>&1 | grep -v -E '^\s*$' | sed 's/^/   /' || true

info "waiting for TAK Server (about a minute)"
wait_healthy takserver 420 || { compose logs --tail 40 takserver >&2; die "TAK Server did not become healthy"; }
log "Declaring the platform's certificate as TAK Server administrator"
grant_api_certificate

info "waiting for the platform"
wait_healthy app 120 || { compose logs --tail 40 app >&2; die "the platform did not become healthy"; }

if [ "${TAK_ENROLL_LETSENCRYPT:-false}" = true ]; then
  log "Let's Encrypt certificate for the enrollment port (8446)"
  "$DEPLOY_DIR/scripts/sync-letsencrypt.sh" --wait 180 --install-cron || info "WARNING: not done yet; run deploy/scripts/sync-letsencrypt.sh once Caddy has its certificate"
fi

log "Checking that the platform reaches the directory and TAK Server"
# TAK Server's API can take a little longer than its healthcheck to accept the platform's
# certificate: try for two minutes before giving up.
ready=0
for _ in $(seq 1 24); do
  if compose exec -T app tak-manager status > "$DATA_DIR/status.log" 2>&1; then ready=1; break; fi
  sleep 5
done
sed 's/^/   /' "$DATA_DIR/status.log"; rm -f "$DATA_DIR/status.log"
[ "$ready" = 1 ] || die "the platform cannot reach the directory or TAK Server; see: cd deploy && docker compose logs app takserver"

if [ -n "${ADMIN_EMAIL:-}" ]; then
  log "Administrator"
  compose exec -T app tak-manager grant-admin "$ADMIN_EMAIL"
fi

log "TAK Manager is running"
info "Web:               $PUBLIC_URL"
info "TAK clients:       $PUBLIC_HOST:8089 (TLS)   enrollment: $PUBLIC_HOST:8446"
if [ "$MODE" = local ]; then
  info "Mail catcher:      http://localhost:${MAILPIT_PORT:-8025} (login codes arrive here)"
fi
info "Stop:              cd deploy && docker compose down      (start again: docker compose up -d)"
info "Rescue admin code: cd deploy && docker compose exec app tak-manager grant-admin <email>"
