#!/usr/bin/env bash
# Installs TeamTAK from nothing: builds TAK Server from the tak.gov ZIP, generates the
# secrets and certificates, and starts the whole stack with Docker Compose.
#
# The first run, in a terminal, asks for every setting not given as an option, shows a
# summary and waits for a yes before writing anything. Later runs reuse deploy/.env and ask
# nothing.
#
# Safe to run again: whatever already exists (settings, certificates, images) is kept, the
# configuration is rendered again and the stack is brought up to date.
#
#   sudo ./install.sh                                         (asks the questions)
#   ./install.sh --local --admin-email you@example.org --non-interactive
#   ./install.sh --domain tak.example.org --admin-email you@example.org \
#                --smtp-host smtp-relay.brevo.com --smtp-username ... --smtp-password ... \
#                --mail-from "TeamTAK <no-reply@example.org>" --non-interactive
#
# See README.md for every option.
. "$(dirname "$0")/deploy/scripts/lib.sh"

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]

On the first run, in a terminal, the script asks for each setting below that is not given
as an option, then shows a summary before writing anything. Later runs reuse deploy/.env
and ask nothing.

Mode (first run only):
  --local                    Local mode: plain HTTP on localhost, mail catcher, private CA.
  --domain <name>            Production mode: HTTPS with Let's Encrypt for this domain, which
                             must already point to this machine. Also TAK Server's public name.
  --host <name-or-ip>        Local mode only: address TAK clients use to reach this machine
                             (default: localhost). With a LAN address, ports listen on all
                             interfaces so phones can connect.

Settings (first run only):
  --admin-email <address>    First platform administrator.
  --acme-email <address>     Contact address given to Let's Encrypt (default: the admin's).
  --cert-validity-days <n>   Validity of TAK client certificates (default: 365).
  --tak-client-ports <list>  Ports of this machine TAK clients connect to, separated by
                             commas (default: 8089). The first goes into the zips and QR
                             codes; the others are alternatives for networks that block it.
  --tak-admin-port <port|off>
                             Port of this machine where the platform's administrators open
                             TAK Server's own administration pages, after signing in to the
                             platform (default: 8443). off: not published.
  --tak-enroll-letsencrypt   Production: also give the Let's Encrypt certificate to TAK
                             Server's enrollment port (8446), which QR-code enrollment needs.
  --smtp-host <host> --smtp-port <port> --smtp-username <u> --smtp-password <p>
  --smtp-tls <starttls|tls|none> --mail-from <address>
                             Outgoing e-mail (production). Local mode uses the mail catcher.

TAK Server:
  --zip <path>               TAK Server ZIP from tak.gov, any version (it is read from the
                             ZIP). Default: the takserver-docker-hardened-<version>.zip at the
                             repository root; the newest when there are several (asked in a
                             terminal). Later runs only need it to rebuild missing images.
  --upgrade-tak              Move an installed platform to a newer TAK Server: the ZIP given
                             with --zip, or a newer one at the repository root (asked when
                             there are several). Backup, new images, restart. See README,
                             "Upgrading TAK Server".

TeamTAK:
  --release <version|latest|current>
                             TeamTAK release to install: this checkout moves to its tag
                             (v1.0.4) before anything else. latest: the newest release on
                             GitHub, pre-releases left out. current: the code as it is.
                             Default: latest in production, current in local mode (the code
                             being developed). An installation ahead of the latest release
                             is not moved back unless a version is given.

Other:
  --non-interactive          Ask nothing: options not given take their default. Implied when
                             the script does not run in a terminal.
  --no-start                 Prepare everything but do not start the stack.
  -h, --help
EOF
}

MODE_ARG="" DOMAIN_ARG="" HOST_ARG="" ADMIN_EMAIL_ARG="" ACME_EMAIL_ARG="" VALIDITY_ARG=""
ENROLL_LE_ARG="" ZIP_ARG="" NO_START=0 UPGRADE_TAK=0 NON_INTERACTIVE=0 TAK_PORTS_ARG="" TAK_ADMIN_PORT_ARG=""
SMTP_HOST_ARG="" SMTP_PORT_ARG="" SMTP_USERNAME_ARG="" SMTP_PASSWORD_ARG="" SMTP_TLS_ARG="" MAIL_FROM_ARG=""
RELEASE_ARG=""
# Options given on the command line, between spaces: they are not asked.
GIVEN=" "
# The options but --release, for this script run again from the release it moved to, which
# may be one that does not know --release.
RERUN_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --domain|--host|--admin-email|--acme-email|--cert-validity-days|--tak-client-ports|--tak-admin-port|--smtp-*|--mail-from|--zip|--release)
      [ $# -ge 2 ] || { usage >&2; die "$1 needs a value"; }
      [ "$1" = --release ] || RERUN_ARGS+=("$1" "$2") ;;
    *) RERUN_ARGS+=("$1") ;;
  esac
  GIVEN="$GIVEN${1#--} "
  case "$1" in
    --local) MODE_ARG=local ;;
    --domain) MODE_ARG=production; DOMAIN_ARG="$2"; shift ;;
    --host) HOST_ARG="$2"; shift ;;
    --admin-email) ADMIN_EMAIL_ARG="$2"; shift ;;
    --acme-email) ACME_EMAIL_ARG="$2"; shift ;;
    --cert-validity-days) VALIDITY_ARG="$2"; shift ;;
    --tak-enroll-letsencrypt) ENROLL_LE_ARG=true ;;
    --tak-client-ports) TAK_PORTS_ARG="$2"; shift ;;
    --tak-admin-port) TAK_ADMIN_PORT_ARG="$2"; shift ;;
    --smtp-host) SMTP_HOST_ARG="$2"; shift ;;
    --smtp-port) SMTP_PORT_ARG="$2"; shift ;;
    --smtp-username) SMTP_USERNAME_ARG="$2"; shift ;;
    --smtp-password) SMTP_PASSWORD_ARG="$2"; shift ;;
    --smtp-tls) SMTP_TLS_ARG="$2"; shift ;;
    --mail-from) MAIL_FROM_ARG="$2"; shift ;;
    --zip) ZIP_ARG="$2"; shift ;;
    --upgrade-tak) UPGRADE_TAK=1 ;;
    --release) RELEASE_ARG="$2"; shift ;;
    --non-interactive) NON_INTERACTIVE=1 ;;
    --no-start) NO_START=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
  shift
done
given() { case "$GIVEN" in *" $1 "*) return 0 ;; esac; return 1; }

# Questions are written to stderr and answered on stdin, so only when both are a terminal:
# never from a script, a pipe or a log redirection (restore.sh runs this script that way).
INTERACTIVE=0
if [ "$NON_INTERACTIVE" = 0 ] && [ -t 0 ] && [ -t 2 ]; then INTERACTIVE=1; fi

# ----------------------------------------------------------------------------- questions

say() { printf '   %s\n' "$*" >&2; }
trim() { local s="$1"; s="${s#"${s%%[![:space:]]*}"}"; s="${s%"${s##*[![:space:]]}"}"; printf '%s' "$s"; }

# ask <variable> <question> <default> [check]: sets the variable to the answer, or to the
# default when the answer is empty. Asks again while the check (a command given the answer)
# fails; it prints why.
ask() {
  local __var="$1" __question="$2" __default="$3" __check="${4:-}" __answer __why
  while :; do
    if [ -n "$__default" ]; then printf '   %s [%s]: ' "$__question" "$__default" >&2
    else printf '   %s: ' "$__question" >&2; fi
    IFS= read -r __answer || { printf '\n' >&2; die "no answer: stopped, nothing was written"; }
    __answer="$(trim "$__answer")"
    [ -n "$__answer" ] || __answer="$__default"
    if [ -n "$__check" ] && ! __why="$($__check "$__answer")"; then
      printf '   \033[33m%s\033[0m\n' "$__why" >&2
      continue
    fi
    printf -v "$__var" '%s' "$__answer"
    return 0
  done
}

# ask_secret <variable> <question>: like ask, without echoing the answer.
ask_secret() {
  local __var="$1" __answer
  printf '   %s: ' "$2" >&2
  IFS= read -r -s __answer || { printf '\n' >&2; die "no answer: stopped, nothing was written"; }
  printf ' (hidden)\n' >&2
  printf -v "$__var" '%s' "$__answer"
}

# ask_yes_no <variable> <question> <y|n>: sets the variable to true or false.
ask_yes_no() {
  local __yn
  ask __yn "$2 (y/n)" "$3" check_yes_no
  case "$__yn" in [yYoO]*) printf -v "$1" true ;; *) printf -v "$1" false ;; esac
}

check_yes_no() {
  case "$1" in [yY]|[yY][eE][sS]|[oO]|[oO][uU][iI]|[nN]|[nN][oO]|[nN][oO][nN]) return 0 ;; esac
  echo "answer y or n"; return 1
}
check_choice() { # <max> <answer>
  case "$2" in ''|*[!0-9]*) ;; *) [ "$2" -ge 1 ] && [ "$2" -le "$1" ] && return 0 ;; esac
  echo "answer a number from 1 to $1"; return 1
}
check_mode() {
  case "$1" in 1|2|production|local) return 0 ;; esac
  echo "answer 1 (production) or 2 (local)"; return 1
}
check_email() {
  case "$1" in
    *[[:space:]]*|*@*@*|*'<'*|*'>'*) ;;
    ?*@?*.?*) return 0 ;;
  esac
  echo "an e-mail address is needed, like you@example.org"; return 1
}
check_domain() {
  printf '%s' "$1" | grep -Eq '^([A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z][A-Za-z0-9-]*[A-Za-z0-9]$' && return 0
  echo "a domain name is needed, like tak.example.org"; return 1
}
check_host() {
  printf '%s' "$1" | grep -Eq '^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$' && return 0
  echo "a host name or an IPv4 address is needed, like localhost or 192.168.1.20"; return 1
}
check_days() {
  case "$1" in ''|*[!0-9]*|0*) echo "a number of days is needed, like 365"; return 1 ;; esac
}
check_port() {
  case "$1" in ''|*[!0-9]*) ;; *) [ "$1" -ge 1 ] && [ "$1" -le 65535 ] && return 0 ;; esac
  echo "a port number is needed, like 587"; return 1
}
# "tak.example.org:8089, also 8443": where TAK clients connect.
client_ports_text() {
  local others
  others="$(port_list "$1" | sed 1d | paste -s -d , - | sed 's/,/, /g')"
  printf '%s:%s%s' "$PUBLIC_HOST" "$(port_list "$1" | sed -n 1p)" "${others:+, also $others}"
}

# Address of TAK Server's administration pages: the web UI's host, on TAK_ADMIN_PORT.
admin_url() {
  local base="${PUBLIC_URL%/}"
  case "$base" in *://*:*) base="${base%:*}" ;; esac
  printf '%s:%s' "$base" "$1"
}
admin_text() {
  if [ -n "$(admin_port "$1")" ]; then printf '%s (administrators, after signing in)' "$(admin_url "$1")"
  else printf 'not published (TAK_ADMIN_PORT=off)'; fi
}

# Valid for the mode, and free: no other program of this machine listens on them.
check_client_ports() {
  local taken
  client_ports_problem "$1" "$MODE" || return 1
  taken="$(client_ports_taken "$1")"
  [ -n "$taken" ] || return 0
  echo "already used on this machine by another program: $taken; choose other ports"; return 1
}
check_tls() {
  case "$1" in starttls|tls|none) return 0 ;; esac
  echo "answer starttls, tls or none"; return 1
}
check_mail_from() {
  case "$1" in *@*) return 0 ;; esac
  echo "an address is needed, like TeamTAK <no-reply@example.org>"; return 1
}
check_zip_path() {
  [ -f "$(expand_path "$1")" ] && return 0
  echo "no such file: $1"; return 1
}

# A path as typed or dropped into a terminal: quotes, "\ " and a leading ~ are undone.
expand_path() {
  local p="$1"
  case "$p" in \'*\'|\"*\") p="${p#?}"; p="${p%?}" ;; esac
  p="$(printf '%s' "$p" | sed 's/\\\(.\)/\1/g')"
  case "$p" in "~"|"~/"*) p="$HOME${p#\~}" ;; esac
  printf '%s' "$p"
}

# This machine's address on its local network, empty when unknown. A hint only.
lan_address() {
  { hostname -I 2>/dev/null | awk '{print $1}' || true
    ipconfig getifaddr en0 2>/dev/null || true
    ipconfig getifaddr en1 2>/dev/null || true
  } | grep -E '^[0-9]+(\.[0-9]+){3}$' | head -n 1 || true
}

# Lists "<version> <path>" lines as a numbered menu, newest last, and sets ZIP to the one
# chosen (the newest by default).
choose_zip_from() {
  local list="$1" n=0 newest v zip mark
  newest="$(printf '%s\n' "$list" | grep -c .)"
  say "$2"
  while read -r v zip; do
    n=$((n + 1)) mark=""
    [ "$n" != "$newest" ] || mark="   (newest)"
    say "  $n) TAK Server $v   $(basename "$zip")$mark"
  done <<EOF
$list
EOF
  ask ZIP_CHOICE "Which one" "$newest" "check_choice $newest"
  ZIP="$(printf '%s\n' "$list" | sed -n "${ZIP_CHOICE}p" | cut -d' ' -f2-)"
}

# The TAK Server ZIP of a first install, in ZIP, and its version, in TAK_VERSION: --zip;
# else the one at the repository root, the newest when there are several (asked in a
# terminal); else, in a terminal, a path asked for.
choose_first_zip() {
  local list count
  if [ -n "$ZIP_ARG" ]; then
    ZIP="$ZIP_ARG"
  else
    list="$(tak_zips)"
    count="$(printf '%s\n' "$list" | grep -c . || true)"
    if [ "$count" = 0 ]; then
      [ "$INTERACTIVE" = 1 ] || die "TAK Server ZIP not found: put takserver-docker-hardened-<version>.zip (from tak.gov) at the repository root, or pass --zip <path>"
      say "No TAK Server ZIP at the repository root. Download takserver-docker-hardened-<version>.zip"
      say "from tak.gov (a free account is needed), then give its path, or drop the file here."
      ask ZIP "Path of the ZIP" "" check_zip_path
      ZIP="$(expand_path "$ZIP")"
    elif [ "$count" = 1 ] || [ "$INTERACTIVE" = 0 ]; then
      ZIP="$(printf '%s\n' "$list" | tail -n 1 | cut -d' ' -f2-)"
    else
      choose_zip_from "$list" "Several TAK Server ZIPs are at the repository root:"
    fi
  fi
  [ -f "$ZIP" ] || die "TAK Server ZIP not found: $ZIP"
  TAK_VERSION="$(tak_zip_version "$ZIP")" || die "cannot tell the TAK Server version of $ZIP: tak/version.txt is missing and the name is not takserver-docker-hardened-<version>.zip"
}

# ----------------------------------------------------------------------------- TeamTAK release

# Moves this checkout to the release asked for (--release; by default the latest in
# production, the code as it is in local mode), then runs the script again from there: the
# script is part of the release. Database migrations are never undone, so an installation
# ahead of the latest release stays where it is unless a version is given.
choose_release() {
  local wanted="$RELEASE_ARG" mode tag target head changed installed=0
  # Run again by itself, from the release it moved to.
  [ -z "${TM_RELEASE_CHOSEN:-}" ] || return 0
  if [ -z "$wanted" ]; then
    mode="$MODE_ARG"
    [ -n "$mode" ] || mode="$(env_get MODE)"
    if [ "$mode" = local ]; then wanted=current; else wanted=latest; fi
  fi
  [ "$wanted" != current ] || return 0
  [ ! -f "$ENV_FILE" ] || installed=1

  log "Choosing the TeamTAK release"
  if ! command -v git > /dev/null || ! repo_git rev-parse --git-dir > /dev/null 2>&1; then
    [ "$wanted" = latest ] || die "--release needs git and a git checkout of TeamTAK (git clone)"
    info "not a git checkout: TeamTAK is installed from these files as they are"
    return 0
  fi
  repo_git fetch --quiet --tags \
    || info "the releases could not be fetched (see above): only those this checkout already knows are used"
  if [ "$wanted" = latest ]; then
    tag="$(release_tags | sed -n 1p)"
    if [ -z "$tag" ]; then
      info "no release known: TeamTAK is installed from this checkout as it is"
      return 0
    fi
  else
    tag="v${wanted#[vV]}"
    case "$tag" in v[0-9]*) ;; *) die "--release takes a version (1.0.4), latest or current, not: $wanted" ;; esac
    case "$tag" in *[!A-Za-z0-9.-]*) die "--release takes a version (1.0.4), latest or current, not: $wanted" ;; esac
    repo_git rev-parse --quiet --verify "refs/tags/$tag^{commit}" > /dev/null \
      || die "no release $tag; the latest ones: $(release_tags | sed -n 1,5p | paste -s -d ' ' -)"
  fi
  target="$(repo_git rev-parse "refs/tags/$tag^{commit}")"
  head="$(repo_git rev-parse HEAD)"
  if [ "$target" = "$head" ]; then
    info "TeamTAK ${tag#v}: this checkout is on it"
    return 0
  fi
  if repo_git merge-base --is-ancestor "$target" HEAD && [ "$wanted" = latest ] && [ "$installed" = 1 ]; then
    info "this checkout ($(teamtak_version)) is ahead of the latest release, ${tag#v}: kept as it is"
    return 0
  fi
  changed="$(repo_git status --porcelain --untracked-files=no)"
  [ -z "$changed" ] || die "files of this checkout were changed, so it cannot move to ${tag#v}:
$changed
Undo the changes (git checkout -- <file>), or keep this code as it is with --release current"
  if [ "$installed" = 1 ] && repo_git merge-base --is-ancestor "$target" HEAD; then
    info "going back to ${tag#v}: the database migrations of the newer code are not undone; restore a backup taken before them (README, \"Going back\")"
  fi
  info "TeamTAK $(teamtak_version) -> ${tag#v}"
  repo_git -c advice.detachedHead=false checkout --quiet --detach "refs/tags/$tag" || die "could not check out $tag"
  export TM_RELEASE_CHOSEN=1
  exec bash "$REPO_DIR/install.sh" ${RERUN_ARGS[@]+"${RERUN_ARGS[@]}"}
}
choose_release

# ----------------------------------------------------------------------------- preflight

# The version of this checkout, from git (teamtak_version in deploy/scripts/lib.sh).
TEAMTAK_VERSION="$(teamtak_version)"

log "Checking the machine"
command -v docker > /dev/null || die "Docker is required"
docker compose version > /dev/null 2>&1 || die "Docker Compose v2 is required (docker compose)"
docker info > /dev/null 2>&1 || die "Docker is not running, or this user cannot use it"
if [ "$(uname -s)" = Linux ] && [ "$(id -u)" != 0 ]; then
  die "on Linux, run this script as root (sudo ./install.sh ...): it sets the ownership of files mounted into the containers"
fi
info "TeamTAK ${TEAMTAK_VERSION:-of an unknown version}, Docker $(docker version --format '{{.Server.Version}}'), $(docker compose version --short), $(uname -s) $(uname -m)"

# ----------------------------------------------------------------------------- settings

mkdir -p "$DATA_DIR"
ZIP=""
if [ ! -f "$ENV_FILE" ]; then
  if [ "$INTERACTIVE" = 1 ]; then
    log "Settings"
    say "First installation: a few questions. Press Enter to keep the value in brackets."
    say "Nothing is written before the summary at the end. (--non-interactive asks nothing.)"
    echo >&2
  fi

  choose_first_zip
  info "TAK Server $TAK_VERSION, from $(basename "$ZIP")"
  [ "$INTERACTIVE" = 0 ] || echo >&2

  MODE="$MODE_ARG"
  [ -n "$MODE" ] || [ -z "$HOST_ARG" ] || MODE=local
  if [ -z "$MODE" ] && [ "$INTERACTIVE" = 1 ]; then
    say "How will this installation be used?"
    say "  1) production: on a server, with a domain name pointing to it, HTTPS from Let's Encrypt"
    say "     and login codes sent through your SMTP account"
    say "  2) local: on this computer or its local network, plain HTTP, e-mails kept by a mail"
    say "     catcher (first look, development, tests)"
    ask MODE "Mode (1 or 2)" "" check_mode
    case "$MODE" in 1) MODE=production ;; 2) MODE=local ;; esac
    echo >&2
  fi
  MODE="${MODE:-local}"

  DOMAIN="$DOMAIN_ARG" CLIENT_HOST="$HOST_ARG"
  ADMIN_EMAIL="$ADMIN_EMAIL_ARG" ACME_EMAIL="$ACME_EMAIL_ARG" VALIDITY="$VALIDITY_ARG"
  ENROLL_LE="${ENROLL_LE_ARG:-false}" TAK_PORTS="$TAK_PORTS_ARG"
  # Not asked: refused as a client port while the questions run.
  TAK_ADMIN_PORT="${TAK_ADMIN_PORT_ARG:-8443}"
  SMTP_HOST="$SMTP_HOST_ARG" SMTP_PORT="$SMTP_PORT_ARG" SMTP_TLS="$SMTP_TLS_ARG"
  SMTP_USERNAME="$SMTP_USERNAME_ARG" SMTP_PASSWORD="$SMTP_PASSWORD_ARG" MAIL_FROM="$MAIL_FROM_ARG"

  if [ "$INTERACTIVE" = 1 ]; then
    if [ "$MODE" = production ]; then
      if [ -z "$DOMAIN" ]; then
        say "Domain name of this server. Its DNS record must already point here: Let's Encrypt"
        say "checks it as soon as the stack starts. TAK clients connect to it too."
        ask DOMAIN "Domain name" "" check_domain
      fi
    elif ! given host; then
      lan="$(lan_address)"
      say "Address TAK clients use to reach this machine. localhost: this computer only. To let"
      say "phones on the local network connect, give this machine's LAN address${lan:+ (here: $lan)}."
      ask CLIENT_HOST "Address" localhost check_host
    fi
    if ! given tak-client-ports; then
      echo >&2
      say "Ports TAK clients connect to. 8089 is the usual one. Add others, separated by commas,"
      say "for networks that block 8089, or give another one if a program of this machine already"
      say "uses it. The first goes into the zips and QR codes; the others can be chosen per device."
      [ "$MODE" != production ] || say "Port 443 is not available: the web interface uses it."
      ask TAK_PORTS "Ports for TAK clients" 8089 check_client_ports
      echo >&2
    fi
    given admin-email || ask ADMIN_EMAIL "E-mail address of the first administrator" "" check_email
    if [ "$MODE" = production ] && ! given acme-email; then
      ask ACME_EMAIL "Contact address given to Let's Encrypt" "$ADMIN_EMAIL" check_email
    fi
    given cert-validity-days || ask VALIDITY "Validity of TAK client certificates, in days" 365 check_days
    if [ "$MODE" = production ]; then
      if ! given tak-enroll-letsencrypt; then
        echo >&2
        say "Enrollment port 8446: QR-code enrollment needs the Let's Encrypt certificate there;"
        say "data packages (zips) work either way. Can be changed later in deploy/.env."
        ask_yes_no ENROLL_LE "Present the Let's Encrypt certificate on port 8446" n
      fi
      echo >&2
      say "Outgoing e-mail, for the login codes and the alerts: an SMTP account (Brevo, for"
      say "example: smtp-relay.brevo.com). Leave the server empty to set it later in deploy/.env."
      given smtp-host || ask SMTP_HOST "SMTP server" "" ""
      if [ -n "$SMTP_HOST" ]; then
        given smtp-port || ask SMTP_PORT "SMTP port" 587 check_port
        if ! given smtp-tls; then
          tls_default=starttls
          [ "${SMTP_PORT:-587}" != 465 ] || tls_default=tls
          ask SMTP_TLS "Encryption: starttls, tls or none" "$tls_default" check_tls
        fi
        given smtp-username || ask SMTP_USERNAME "SMTP username (empty if none)" "" ""
        if [ -n "$SMTP_USERNAME" ] && ! given smtp-password; then
          ask_secret SMTP_PASSWORD "SMTP password or key"
        fi
        given mail-from || ask MAIL_FROM "Sender of the e-mails (an address your SMTP account may send from)" "TeamTAK <no-reply@$DOMAIN>" check_mail_from
      fi
    fi
  fi

  # Options given on the command line are checked here; answers were checked when typed.
  VALIDITY="${VALIDITY:-365}"
  check_days "$VALIDITY" > /dev/null || die "--cert-validity-days must be a number of days"
  [ -z "$ADMIN_EMAIL" ] || check_email "$ADMIN_EMAIL" > /dev/null || die "--admin-email: not an e-mail address: $ADMIN_EMAIL"
  ACME_EMAIL="${ACME_EMAIL:-$ADMIN_EMAIL}"
  [ "$ENROLL_LE" != true ] || [ "$MODE" = production ] || die "--tak-enroll-letsencrypt needs --domain"

  if [ "$MODE" = production ]; then
    [ -n "$DOMAIN" ] || die "--domain needs a domain name"
    check_domain "$DOMAIN" > /dev/null || die "--domain: not a domain name: $DOMAIN"
    PUBLIC_HOST="$DOMAIN"
    PUBLIC_URL="https://$DOMAIN"
    BIND_ADDRESS=0.0.0.0
    COOKIE_SECURE=true
    COMPOSE_FILES="docker-compose.yml:docker-compose.production.yml:$PORTS_COMPOSE_FILE"
    # Inside the Docker network the public name resolves to the takserver container, so the
    # platform can verify whichever certificate 8446 presents (private CA or Let's Encrypt).
    TAK_ENROLL_URL="https://$DOMAIN:8446"
    TAK_NETWORK_ALIAS="$DOMAIN"
    SMTP_PORT="${SMTP_PORT:-587}" SMTP_TLS="${SMTP_TLS:-starttls}"
    MAIL_FROM="${MAIL_FROM:-TeamTAK <no-reply@$DOMAIN>}"
    LOGIN_MAX_CODES_PER_IP=30
    LOGIN_MAX_FAILURES_PER_IP=30
    MONITOR_INTERVAL_SECONDS=60
  else
    PUBLIC_HOST="${CLIENT_HOST:-localhost}"
    check_host "$PUBLIC_HOST" > /dev/null || die "--host: not a host name or an IPv4 address: $PUBLIC_HOST"
    PUBLIC_URL="http://$PUBLIC_HOST:8480"
    if [ "$PUBLIC_HOST" = localhost ] || [ "$PUBLIC_HOST" = 127.0.0.1 ]; then BIND_ADDRESS=127.0.0.1; else BIND_ADDRESS=0.0.0.0; fi
    COOKIE_SECURE=false
    COMPOSE_FILES="docker-compose.yml:docker-compose.local.yml:$PORTS_COMPOSE_FILE"
    TAK_ENROLL_URL="https://takserver:8446"
    TAK_NETWORK_ALIAS="takserver.internal"
    SMTP_HOST="${SMTP_HOST:-mailpit}" SMTP_PORT="${SMTP_PORT:-1025}" SMTP_TLS="${SMTP_TLS:-none}"
    MAIL_FROM="${MAIL_FROM:-TeamTAK <teamtak@localhost>}"
    # The integration tests sign in many accounts, and submit wrong codes, from one address;
    # replaying them must not lock that address out. The per-address limit stays as in
    # production, and is tested.
    LOGIN_MAX_CODES_PER_IP=1000
    LOGIN_MAX_FAILURES_PER_IP=1000
    # The integration tests wait for alerts to be raised and resolved.
    MONITOR_INTERVAL_SECONDS=10
  fi
  check_port "$SMTP_PORT" > /dev/null || die "--smtp-port: not a port number: $SMTP_PORT"
  check_tls "$SMTP_TLS" > /dev/null || die "--smtp-tls must be starttls, tls or none"
  why="$(admin_port_problem "$TAK_ADMIN_PORT" "$MODE")" || die "--tak-admin-port: $why"
  TAK_PORTS="$(normalize_ports "${TAK_PORTS:-8089}")"
  why="$(client_ports_problem "$TAK_PORTS" "$MODE")" || die "--tak-client-ports: $why"

  if [ "$INTERACTIVE" = 1 ]; then
    log "Summary"
    info "Mode:                  $MODE"
    info "Web:                   $PUBLIC_URL"
    info "TAK clients:           $(client_ports_text "$TAK_PORTS"), enrollment $PUBLIC_HOST:8446"
    info "TAK Server admin:      $(admin_text "$TAK_ADMIN_PORT")"
    info "Administrator:         ${ADMIN_EMAIL:-none (later: docker compose exec app teamtak grant-admin <email>)}"
    if [ "$MODE" = production ]; then
      info "Let's Encrypt contact: ${ACME_EMAIL:-none}"
      if [ "$ENROLL_LE" = true ]; then info "Port 8446:             Let's Encrypt certificate"
      else info "Port 8446:             private CA's certificate"; fi
      if [ -z "$SMTP_HOST" ]; then info "E-mail:                not set: login codes cannot be e-mailed yet"
      else
        info "E-mail:                $SMTP_HOST:$SMTP_PORT ($SMTP_TLS), ${SMTP_USERNAME:+user $SMTP_USERNAME, }${SMTP_PASSWORD:+password given, }from $MAIL_FROM"
      fi
    else
      info "E-mail:                mail catcher, http://localhost:${MAILPIT_PORT:-8025}"
    fi
    info "TAK Server:            $TAK_VERSION ($(basename "$ZIP"))"
    info "Client certificates:   valid $VALIDITY days"
    echo >&2
    ask_yes_no GO "Write deploy/.env and install with these settings" y
    [ "$GO" = true ] || { info "Stopped: nothing was written. Run ./install.sh again to answer again."; exit 1; }
  fi

  log "Creating $ENV_FILE"
  cat > "$ENV_FILE" <<ENVEOF
# Generated by install.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ). Holds secrets: never version or share this file.
# After editing it, run ./install.sh again to apply the change.

# --- TeamTAK
# Version of this checkout, from git, set by install.sh at every run: the tag of the
# platform and agent images (teamtak/app, teamtak/agent)
TEAMTAK_VERSION=$TEAMTAK_VERSION

# --- Mode and addresses
MODE=$MODE
COMPOSE_FILE=$COMPOSE_FILES
# Name or address users and TAK clients reach this machine by
PUBLIC_HOST=$PUBLIC_HOST
PUBLIC_URL=$PUBLIC_URL
# Interface the published ports listen on
BIND_ADDRESS=$BIND_ADDRESS
# Ports of this machine TAK clients connect to, separated by commas. The first goes into the
# zips and QR codes; the others are alternatives for networks that block it.
TAK_CLIENT_PORTS=$TAK_PORTS
# Port of this machine where the platform's administrators open TAK Server's own
# administration pages, after signing in to the platform; off: not published
TAK_ADMIN_PORT=$TAK_ADMIN_PORT
COOKIE_SECURE=$COOKIE_SECURE
ADMIN_EMAIL=$ADMIN_EMAIL
ACME_EMAIL=$ACME_EMAIL

# --- TAK Server
TAK_VERSION=$TAK_VERSION
TAK_IMAGE=teamtak/takserver:$TAK_VERSION
TAK_DB_IMAGE=teamtak/takserver-db:$TAK_VERSION
# Validity of the client certificates issued by enrollment, in days
CERT_VALIDITY_DAYS=$VALIDITY
# Safety net only: the platform pushes group changes itself. Seconds.
LDAP_UPDATE_INTERVAL=60
ROOT_CA_NAME=teamtak-root-ca
INT_CA_NAME=teamtak-ca
SERVER_CERT_NAME=takserver
TAK_API_CERT_NAME=teamtak-api
# Certificate Caddy presents to TAK Server for its administration pages
TAK_ADMIN_UI_CERT_NAME=teamtak-admin-ui
# true: port 8446 presents the Let's Encrypt certificate (needed by QR-code enrollment)
TAK_ENROLL_LETSENCRYPT=$ENROLL_LE
TAK_ENROLL_URL=$TAK_ENROLL_URL
TAK_NETWORK_ALIAS=$TAK_NETWORK_ALIAS
# Name of the connection in the phones' server list
TAK_CONNECTION_NAME=TeamTAK
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
# How often the platform looks at the machine, the services and TAK Server, and evaluates
# the alerts. Seconds.
MONITOR_INTERVAL_SECONDS=$MONITOR_INTERVAL_SECONDS

# --- Directory
LDAP_BASE_DN=dc=teamtak,dc=internal

# --- Secrets (generated)
APP_SECRET=$(rand 48)
# Shared by the platform and the host agent
AGENT_TOKEN=$(rand 48)
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
  for option in local domain host admin-email acme-email cert-validity-days tak-client-ports tak-admin-port tak-enroll-letsencrypt smtp-host smtp-port smtp-username smtp-password smtp-tls mail-from; do
    if given "$option"; then
      info "note: --$option and the other settings options are only read on the first run; edit $ENV_FILE to change them"
      break
    fi
  done
fi
# The version of this checkout follows git: rewritten in place when it changed (checkout of another release).
if [ -z "$(env_get TEAMTAK_VERSION)" ]; then
  printf '\n# Version of this checkout, from git, set by install.sh at every run: the tag of the\n# platform and agent images (teamtak/app, teamtak/agent)\nTEAMTAK_VERSION=%s\n' "$TEAMTAK_VERSION" >> "$ENV_FILE"
elif [ "$(env_get TEAMTAK_VERSION)" != "$TEAMTAK_VERSION" ]; then
  tmp="$(mktemp)"
  sed "s/^TEAMTAK_VERSION=.*/TEAMTAK_VERSION=$TEAMTAK_VERSION/" "$ENV_FILE" > "$tmp"
  cat "$tmp" > "$ENV_FILE"
  rm -f "$tmp"
fi
# Settings added since this file was generated (an installation made before phase 3).
if [ -z "$(env_get AGENT_TOKEN)" ]; then
  env_set AGENT_TOKEN "$(rand 48)"
  info "added to $ENV_FILE: AGENT_TOKEN (secret shared with the host agent)"
fi
if [ -z "$(env_get MONITOR_INTERVAL_SECONDS)" ]; then
  if [ "$(env_get MODE)" = local ]; then env_set MONITOR_INTERVAL_SECONDS 10; else env_set MONITOR_INTERVAL_SECONDS 60; fi
fi
# Ports of TAK clients: 8089 until now, published through a generated Compose file.
if [ -z "$(env_get TAK_CLIENT_PORTS)" ]; then
  env_set TAK_CLIENT_PORTS 8089
  info "added to $ENV_FILE: TAK_CLIENT_PORTS=8089 (ports TAK clients connect to; add others there)"
fi
# TAK Server's administration pages, through Caddy: 8443, unless a TAK client port or another
# program of this machine already has it.
if [ -z "$(env_get TAK_ADMIN_UI_CERT_NAME)" ]; then
  env_set TAK_ADMIN_UI_CERT_NAME teamtak-admin-ui
fi
if [ -z "$(env_get TAK_ADMIN_PORT)" ]; then
  if port_list "$(env_get TAK_CLIENT_PORTS)" | grep -qx 8443; then
    env_set TAK_ADMIN_PORT off
    info "added to $ENV_FILE: TAK_ADMIN_PORT=off (8443 is a TAK client port here; give another free port to open TAK Server's administration)"
  elif [ -n "$(client_ports_taken 8443)" ]; then
    env_set TAK_ADMIN_PORT off
    info "added to $ENV_FILE: TAK_ADMIN_PORT=off (another program of this machine uses 8443; give another free port to open TAK Server's administration)"
  else
    env_set TAK_ADMIN_PORT 8443
    info "added to $ENV_FILE: TAK_ADMIN_PORT=8443 (TAK Server's administration pages, for the platform's administrators)"
  fi
fi
compose_files="$(env_get COMPOSE_FILE)"
case ":$compose_files:" in
  *":$PORTS_COMPOSE_FILE:"*) ;;
  *) env_set COMPOSE_FILE "${compose_files:+$compose_files:}$PORTS_COMPOSE_FILE" ;;
esac
load_env
why="$(admin_port_problem "$TAK_ADMIN_PORT" "$MODE")" || die "TAK_ADMIN_PORT in $ENV_FILE: $why"
why="$(client_ports_problem "$TAK_CLIENT_PORTS" "$MODE")" || die "TAK_CLIENT_PORTS in $ENV_FILE: $why"
render_ports_compose
# Backup archives are written here by the host agent. They hold every secret.
mkdir -p "$DEPLOY_DIR/backups"
chmod 700 "$DEPLOY_DIR/backups"

# ----------------------------------------------------------------------------- TAK Server upgrade

# Moves the installation to the TAK Server version of another ZIP. Everything it changes
# before the restart can be undone by putting the three TAK_ lines of deploy/.env back.
if [ "$UPGRADE_TAK" = 1 ]; then
  log "Upgrading TAK Server"
  if [ -n "$ZIP_ARG" ]; then
    [ -f "$ZIP_ARG" ] || die "TAK Server ZIP not found: $ZIP_ARG"
    ZIP="$ZIP_ARG"
  else
    # The ZIPs at the repository root of a newer version than the installed one.
    candidates="$(tak_zips | while read -r v zip; do
      if version_newer "$v" "$TAK_VERSION"; then printf '%s %s\n' "$v" "$zip"; fi
    done)"
    count="$(printf '%s\n' "$candidates" | grep -c . || true)"
    if [ "$count" = 0 ]; then
      die "no ZIP of a version newer than $TAK_VERSION at the repository root: put takserver-docker-hardened-<new version>.zip there, or pass --zip <path>"
    elif [ "$count" = 1 ]; then
      ZIP="$(printf '%s\n' "$candidates" | cut -d' ' -f2-)"
    elif [ "$INTERACTIVE" = 1 ]; then
      choose_zip_from "$candidates" "Several newer TAK Server ZIPs are at the repository root:"
    else
      die "several ZIPs newer than $TAK_VERSION at the repository root ($(printf '%s\n' "$candidates" | cut -d' ' -f1 | tr '\n' ' ')): give the one to install with --zip <path>"
    fi
  fi
  NEW_VERSION="$(tak_zip_version "$ZIP")" || die "cannot tell the TAK Server version of $ZIP"
  [ "$NEW_VERSION" != "$TAK_VERSION" ] || die "TAK Server $TAK_VERSION is already the installed version"
  version_newer "$NEW_VERSION" "$TAK_VERSION" || die "$(basename "$ZIP") holds TAK Server $NEW_VERSION, older than the installed $TAK_VERSION. Going back is done with the backup taken by the upgrade: see README, \"Upgrading TAK Server\"."
  OLD_VERSION="$TAK_VERSION" OLD_DB_IMAGE="$TAK_DB_IMAGE"
  NEW_IMAGE="teamtak/takserver:$NEW_VERSION" NEW_DB_IMAGE="teamtak/takserver-db:$NEW_VERSION"
  info "installed: $OLD_VERSION — new: $NEW_VERSION ($(basename "$ZIP"))"
  [ -n "$(svc_container takserver-db)" ] || die "the stack must be running for an upgrade (the backup is taken from it)"
  if [ "$INTERACTIVE" = 1 ]; then
    ask_yes_no GO "Take a backup, build TAK Server $NEW_VERSION and restart the stack on it" y
    [ "$GO" = true ] || { info "Stopped: nothing was changed."; exit 1; }
  fi

  info "backup first"
  "$DEPLOY_DIR/scripts/backup.sh" > "$DATA_DIR/upgrade-backup.log" 2>&1 || { tail -n 20 "$DATA_DIR/upgrade-backup.log" >&2; die "the backup failed: nothing was changed"; }
  info "$(sed -n 's/^BACKUP_RESULT file=\([^ ]*\).*/archive: deploy\/backups\/\1/p' "$DATA_DIR/upgrade-backup.log")"
  rm -f "$DATA_DIR/upgrade-backup.log"

  "$DEPLOY_DIR/scripts/build-tak-images.sh" "$ZIP" "$NEW_IMAGE" "$NEW_DB_IMAGE"

  # TAK Server's database lives in a Docker volume written by the old image's PostgreSQL.
  # A new major version of PostgreSQL cannot open it: stop before anything is switched.
  pg_major() { docker run --rm --entrypoint postgres "$1" --version 2>/dev/null | sed -n 's/.* \([0-9][0-9]*\)\..*/\1/p'; }
  old_pg="$(pg_major "$OLD_DB_IMAGE")" new_pg="$(pg_major "$NEW_DB_IMAGE")"
  if [ -z "$old_pg" ] || [ -z "$new_pg" ] || [ "$old_pg" != "$new_pg" ]; then
    die "the new database image runs PostgreSQL ${new_pg:-?}, the installed one ${old_pg:-?}: the data volume cannot be reused as it is. Nothing was changed. See README, \"Upgrading TAK Server\"."
  fi

  env_set TAK_VERSION "$NEW_VERSION"
  env_set TAK_IMAGE "$NEW_IMAGE"
  env_set TAK_DB_IMAGE "$NEW_DB_IMAGE"
  load_env
  info "deploy/.env now names TAK Server $NEW_VERSION (to go back: TAK_VERSION, TAK_IMAGE and TAK_DB_IMAGE to $OLD_VERSION, then ./install.sh; the database is in the backup above)"
fi

# The ZIP of the installed version, which build-tak-images.sh reads only when an image is
# missing. A first install and an upgrade have already chosen it.
if [ -z "$ZIP" ]; then
  if [ -n "$ZIP_ARG" ]; then
    [ -f "$ZIP_ARG" ] || die "TAK Server ZIP not found: $ZIP_ARG"
    zip_version="$(tak_zip_version "$ZIP_ARG" || true)"
    [ "$zip_version" = "$TAK_VERSION" ] || die "$ZIP_ARG holds TAK Server ${zip_version:-of an unknown version}, and $TAK_VERSION is installed: to move to it, add --upgrade-tak"
    ZIP="$ZIP_ARG"
  else
    ZIP="$(tak_zip_for_version "$TAK_VERSION")"
    newest="$(tak_zips | tail -n 1 | cut -d' ' -f1)"
    if [ -n "$newest" ] && version_newer "$newest" "$TAK_VERSION"; then
      info "TAK Server $newest is at the repository root (installed: $TAK_VERSION); to move to it: sudo ./install.sh --upgrade-tak"
    fi
  fi
fi
info "mode: $MODE — public host: $PUBLIC_HOST — web: $PUBLIC_URL — client certificates: $CERT_VALIDITY_DAYS days"
info "TAK clients: $(client_ports_text "$TAK_CLIENT_PORTS")"
info "TAK Server admin: $(admin_text "$TAK_ADMIN_PORT")"
if [ "$NO_START" = 0 ]; then
  # Docker cannot publish a port another program holds, and would only say so once
  # everything is built.
  taken="$(client_ports_taken "$TAK_CLIENT_PORTS")"
  [ -z "$taken" ] || die "TAK clients' port(s) $taken already used on this machine by another program. Choose other ports in TAK_CLIENT_PORTS ($ENV_FILE), then run ./install.sh again"
  taken="$(client_ports_taken "$(admin_port "$TAK_ADMIN_PORT")")"
  [ -z "$taken" ] || die "port $taken of TAK Server's administration already used on this machine by another program. Choose another port in TAK_ADMIN_PORT ($ENV_FILE), or off, then run ./install.sh again"
fi
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
    -e STATE=XX -e CITY=TAK -e ORGANIZATIONAL_UNIT=TeamTAK \
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

# The certificate Caddy presents to TAK Server for the administration pages. Made apart from
# the others, with TAK Server's own script and CA, so an installation made before it existed
# gets it too.
if [ ! -f "$DATA_DIR/tak/certs/$TAK_ADMIN_UI_CERT_NAME-plain.key" ]; then
  log "Generating the certificate of TAK Server's administration pages"
  if ! docker run --rm -i -u 0 --entrypoint bash -w /opt/tak/certs \
      -v "$DATA_DIR/tak/certs:/opt/tak/certs/files" \
      -e NAME="$TAK_ADMIN_UI_CERT_NAME" -e CAPASS="$TAK_CERT_PASSWORD" -e PASS="$TAK_CERT_PASSWORD" \
      -e STATE=XX -e CITY=TAK -e ORGANIZATIONAL_UNIT=TeamTAK \
      "$TAK_IMAGE" -s > "$DATA_DIR/certs.log" 2>&1 <<'CERTEOF'
set -euo pipefail
./makeCert.sh client "$NAME"
cd files
openssl pkcs8 -topk8 -nocrypt -in "$NAME.key" -passin "pass:$PASS" -out "$NAME-plain.key"
openssl x509 -in "$NAME.pem" -noout -subject -enddate | tr '\n' ' '
CERTEOF
  then
    tail -n 30 "$DATA_DIR/certs.log" >&2
    die "certificate generation failed"
  fi
  info "$(tail -n 1 "$DATA_DIR/certs.log")"
  rm -f "$DATA_DIR/certs.log"
fi
# The platform watches its expiry: the certificate alone, never the key.
cp "$DATA_DIR/tak/certs/$TAK_ADMIN_UI_CERT_NAME.pem" "$DATA_DIR/app/tak/admin-ui-cert.pem"
# Read by Caddy only: its own copy, like the platform's above.
mkdir -p "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR"
chmod 700 "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR"
cp "$DATA_DIR/tak/certs/$INT_CA_NAME.pem" "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR/ca-chain.pem"
cp "$DATA_DIR/tak/certs/$TAK_ADMIN_UI_CERT_NAME.pem" "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR/cert.pem"
cp "$DATA_DIR/tak/certs/$TAK_ADMIN_UI_CERT_NAME-plain.key" "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR/key.pem"
chmod 600 "$DEPLOY_DIR/$ADMIN_UI_CADDY_DIR/key.pem"

# ----------------------------------------------------------------------------- TAK Server configuration

log "TAK Server configuration"
# Provided before the first start: three of TAK Server's processes each create it when it
# is missing, and the one that loses the race dies (phase 0 report, section 11).
if [ ! -f "$DATA_DIR/tak/TAKIgniteConfig.xml" ]; then
  docker run --rm --entrypoint cat "$TAK_IMAGE" /opt/tak/TAKIgniteConfig.example.xml > "$DATA_DIR/tak/TAKIgniteConfig.xml"
fi
# Starts empty; grant_admin_certificates fills it. Kept here so the administrator role of the
# platform's certificate survives a re-creation of the container.
if [ ! -f "$DATA_DIR/tak/UserAuthenticationFile.xml" ]; then
  printf '%s\n' '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' \
    '<UserAuthenticationFile xmlns="http://bbn.com/marti/xml/bindings"/>' > "$DATA_DIR/tak/UserAuthenticationFile.xml"
fi
render_core_config
fix_ownership
info "CoreConfig.xml rendered (client certificates valid $CERT_VALIDITY_DAYS days, 8443 not published)"

# ----------------------------------------------------------------------------- platform image

log "Building the platform and agent images (first build: several minutes)"
compose build app agent > "$DATA_DIR/build-app.log" 2>&1 || { tail -n 40 "$DATA_DIR/build-app.log" >&2; die "could not build the platform images"; }
rm -f "$DATA_DIR/build-app.log"
info "teamtak/app:$TEAMTAK_VERSION and teamtak/agent:$TEAMTAK_VERSION built"

# TAK Server reads CoreConfig.xml only when it starts. Compose re-creates its container when
# the image or the environment changed, not when that file did: a TAK Server already running,
# and left as it is by Compose, must be restarted to load a new CoreConfig.xml.
tak_running=""
if [ "$CORE_CONFIG_CHANGED" = true ]; then
  tak_id="$(svc_container takserver)"
  if [ -n "$tak_id" ] && [ "$(docker inspect -f '{{.State.Running}}' "$tak_id" 2>/dev/null)" = true ]; then
    tak_running="$tak_id"
  fi
fi

if [ "$NO_START" = 1 ]; then
  log "Prepared. Start with: cd deploy && docker compose up -d"
  [ -z "$tak_running" ] || info "TAK Server still runs the previous CoreConfig.xml: also run docker compose restart takserver"
  exit 0
fi

# ----------------------------------------------------------------------------- start

log "Starting the stack"
compose up -d --remove-orphans 2>&1 | grep -v -E '^\s*$' | sed 's/^/   /' || true
if [ -n "$tak_running" ] && [ "$(svc_container takserver)" = "$tak_running" ]; then
  info "CoreConfig.xml changed: restarting TAK Server to load it (clients disconnected for about a minute)"
  docker restart --time 30 "$tak_running" > /dev/null
fi

info "waiting for TAK Server (about a minute)"
wait_healthy takserver 420 || { compose logs --tail 40 takserver >&2; die "TAK Server did not become healthy"; }
log "Declaring the platform's certificates as TAK Server administrators"
grant_admin_certificates

info "waiting for the platform"
wait_healthy app 120 || { compose logs --tail 40 app >&2; die "the platform did not become healthy"; }
# The platform works without the agent, with a dashboard that says so: a warning, not a failure.
wait_healthy agent 60 || info "WARNING: the host agent is not healthy; monitoring, backups and restarts from the web UI need it. See: cd deploy && docker compose logs agent"

if [ "${TAK_ENROLL_LETSENCRYPT:-false}" = true ]; then
  log "Let's Encrypt certificate for the enrollment port (8446)"
  "$DEPLOY_DIR/scripts/sync-letsencrypt.sh" --wait 180 || info "WARNING: not done yet; the platform asks the agent again every day, or use \"Hand over now\" in the administration once Caddy has its certificate"
  # Installations made before phase 3 had a cron job for this; the agent does it now.
  rm -f /etc/cron.d/teamtak-letsencrypt 2>/dev/null || true
fi

log "Checking that the platform reaches the directory and TAK Server"
# TAK Server's API can take a little longer than its healthcheck to accept the platform's
# certificate: try for two minutes before giving up.
ready=0
for _ in $(seq 1 24); do
  if compose exec -T app teamtak status > "$DATA_DIR/status.log" 2>&1; then ready=1; break; fi
  sleep 5
done
sed 's/^/   /' "$DATA_DIR/status.log"; rm -f "$DATA_DIR/status.log"
[ "$ready" = 1 ] || die "the platform cannot reach the directory or TAK Server; see: cd deploy && docker compose logs app takserver"

if [ -n "${ADMIN_EMAIL:-}" ]; then
  log "Administrator"
  compose exec -T app teamtak grant-admin "$ADMIN_EMAIL"
fi

# The platform and agent images of other versions (and the "local" tag of installations made
# before the images carried the version): their containers were just replaced. Docker
# refuses to remove one a container still uses.
for image in $(docker image ls --filter reference=teamtak/app --filter reference=teamtak/agent --format '{{.Repository}}:{{.Tag}}'); do
  case "$image" in *":$TEAMTAK_VERSION"|*":<none>") continue ;; esac
  docker image rm "$image" > /dev/null 2>&1 && info "removed the image of another version: $image" || true
done

log "TeamTAK $TEAMTAK_VERSION is running"
info "Web:               $PUBLIC_URL"
info "TAK clients:       $(client_ports_text "$TAK_CLIENT_PORTS") (TLS)   enrollment: $PUBLIC_HOST:8446"
info "TAK Server admin:  $(admin_text "$TAK_ADMIN_PORT")"
if [ "$MODE" = local ]; then
  info "Mail catcher:      http://localhost:${MAILPIT_PORT:-8025} (login codes arrive here)"
fi
info "Backups:           deploy/backups/ (daily, set in the administration); restore: deploy/scripts/restore.sh"
info "Stop:              cd deploy && docker compose down      (start again: docker compose up -d)"
info "Rescue admin code: cd deploy && docker compose exec app teamtak grant-admin <email>"
