#!/usr/bin/env bash
# Restores the whole platform from an archive written by deploy/scripts/backup.sh.
#
#   deploy/scripts/restore.sh deploy/backups/teamtak-backup-20261119T031500Z.tar.gz
#   BACKUP_PASSPHRASE='…' deploy/scripts/restore.sh <archive>.tar.gz.enc
#   deploy/scripts/restore.sh --yes <archive>          no question asked
#
# EVERYTHING on this machine is replaced by what the archive holds: the platform's database,
# the directory, TAK Server's database, deploy/.env and deploy/data (the certificate
# authority). What was done since the archive was written is lost; in particular an access
# regenerated or removed since then is valid again, with its old certificate.
#
# The four parts are always restored together, never one alone: a platform database older
# than the directory would make the reconciliation delete what it does not know.
#
# Works on the machine the archive comes from and on a new one. On a new machine: install
# Docker, clone the repository, put the TAK Server ZIP at its root, copy the archive over,
# and run this script. It needs the same things as install.sh, plus openssl for an
# encrypted archive.
#
# What it does, in order:
#   1. reads the archive (asks for the passphrase when it is encrypted) and checks it;
#   2. stops the stack and removes its Docker volumes; the current deploy/.env and
#      deploy/data are moved to deploy/backups/replaced-<date>/, not deleted;
#   3. puts deploy/.env and deploy/data back, and prepares the images (install.sh --no-start);
#   4. starts the databases and the directory alone and loads the three exports;
#   5. starts everything (install.sh) and checks that the platform reaches the directory
#      and TAK Server.
. "$(dirname "$0")/lib.sh"

YES=0 ARCHIVE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --yes) YES=1 ;;
    -h|--help) sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) [ -z "$ARCHIVE" ] || die "one archive at a time"; ARCHIVE="$1" ;;
  esac
  shift
done
[ -n "$ARCHIVE" ] || die "usage: restore.sh [--yes] <archive>"
[ -f "$ARCHIVE" ] || die "archive not found: $ARCHIVE"
ARCHIVE="$(cd "$(dirname "$ARCHIVE")" && pwd)/$(basename "$ARCHIVE")"

command -v docker > /dev/null || die "Docker is required"
docker info > /dev/null 2>&1 || die "Docker is not running, or this user cannot use it"
if [ "$(uname -s)" = Linux ] && [ "$(id -u)" != 0 ]; then
  die "on Linux, run this script as root (sudo deploy/scripts/restore.sh ...)"
fi

# ----------------------------------------------------------------------------- 1. the archive

ENCRYPTED=0
case "$ARCHIVE" in
  *.tar.gz.enc) ENCRYPTED=1 ;;
  *.tar.gz) ;;
  *) die "not an archive of backup.sh (expected .tar.gz or .tar.gz.enc): $ARCHIVE" ;;
esac
if [ "$ENCRYPTED" = 1 ]; then
  command -v openssl > /dev/null || die "openssl is required to read an encrypted archive"
  if [ -z "${BACKUP_PASSPHRASE:-}" ]; then
    [ -t 0 ] || die "the archive is encrypted: give its passphrase in BACKUP_PASSPHRASE"
    printf 'Passphrase of the archive: '
    read -r -s BACKUP_PASSPHRASE
    echo
  fi
  export BACKUP_PASSPHRASE
fi

mkdir -p "$DEPLOY_DIR/backups"
WORK="$(mktemp -d "$DEPLOY_DIR/backups/.restore-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
chmod 700 "$WORK"

log "Reading $(basename "$ARCHIVE")"
unpack() {
  if [ "$ENCRYPTED" = 1 ]; then
    openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -md sha256 -pass env:BACKUP_PASSPHRASE -in "$ARCHIVE" | tar -xz -p -f - -C "$WORK"
  else
    tar -xz -p -f "$ARCHIVE" -C "$WORK"
  fi
}
unpack 2> "$WORK/unpack.err" || { head -n 3 "$WORK/unpack.err" >&2; die "the archive cannot be read$([ "$ENCRYPTED" = 1 ] && echo ' (wrong passphrase?)')"; }
rm -f "$WORK/unpack.err"
for member in manifest.txt ldap-data.ldif takserver.dump platform.dump deploy/.env deploy/data/tak/CoreConfig.xml; do
  [ -e "$WORK/$member" ] || die "the archive lacks $member: not a complete backup"
done
grep -q '^format: 1$' "$WORK/manifest.txt" || die "unknown archive format: $(head -n 1 "$WORK/manifest.txt")"
for dump in takserver.dump platform.dump; do
  [ "$(head -c 5 "$WORK/$dump")" = PGDMP ] || die "$dump is not a PostgreSQL dump"
done
sed 's/^/   /' "$WORK/manifest.txt"

if [ "$YES" = 0 ]; then
  echo
  echo "This replaces the platform's database, the directory, TAK Server's database, deploy/.env"
  echo "and deploy/data on this machine with the content of the archive."
  printf 'Type "restore" to continue: '
  read -r answer
  [ "$answer" = restore ] || die "aborted, nothing was changed"
fi

# ----------------------------------------------------------------------------- 2. what is there now

log "Stopping the stack and removing its volumes"
if [ -f "$ENV_FILE" ]; then
  compose down -v --remove-orphans 2>&1 | grep -v -E '^\s*$' | sed 's/^/   /' || true
else
  # No settings here (a new machine, or a broken one): the project name is enough to find
  # what belongs to it.
  (cd "$DEPLOY_DIR" && docker compose -p "$COMPOSE_PROJECT" -f docker-compose.yml down -v --remove-orphans > /dev/null 2>&1) || true
fi

if [ -e "$ENV_FILE" ] || [ -d "$DATA_DIR" ]; then
  aside="$DEPLOY_DIR/backups/replaced-$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$aside"
  chmod 700 "$aside"
  [ -e "$ENV_FILE" ] && mv "$ENV_FILE" "$aside/.env"
  [ -d "$DATA_DIR" ] && mv "$DATA_DIR" "$aside/data"
  info "the previous deploy/.env and deploy/data are in $aside (delete it once the restore is checked)"
fi

# ----------------------------------------------------------------------------- 3. files and images

log "Restoring deploy/.env and deploy/data"
mv "$WORK/deploy/.env" "$ENV_FILE"
chmod 600 "$ENV_FILE"
mv "$WORK/deploy/data" "$DATA_DIR"
mkdir -p "$DATA_DIR/tak/logs"

log "Preparing the images and the configuration"
# Builds what is missing (TAK Server from the ZIP, the platform, the agent), renders the
# configuration from the restored settings and sets the ownership of the restored files.
# It keeps the restored certificate authority: it only generates one when there is none.
"$REPO_DIR/install.sh" --no-start > "$WORK/install.log" 2>&1 || { tail -n 30 "$WORK/install.log" >&2; die "install.sh --no-start failed"; }
load_env

# ----------------------------------------------------------------------------- 4. the data

log "Starting the databases and the directory"
compose up -d postgres takserver-db openldap 2>&1 | grep -v -E '^\s*$' | sed 's/^/   /' || true
wait_healthy postgres 120 || die "the platform's database did not start"
wait_healthy openldap 120 || die "the directory did not start"
# This image creates TAK Server's tables at its first start; it is healthy once they exist.
wait_healthy takserver-db 420 || die "TAK Server's database did not start"

log "Loading the platform's database"
svc_exec postgres pg_restore -U teamtak -d teamtak --clean --if-exists --no-owner --single-transaction < "$WORK/platform.dump" \
  || die "could not load the platform's database"
info "$(svc_exec postgres psql -U teamtak -d teamtak -tAc "select count(*) || ' accounts, ' || (select count(*) from teams) || ' teams, ' || (select count(*) from tak_accesses) || ' TAK accesses' from accounts")"

log "Loading TAK Server's database"
# The database is dropped and created again from the dump. The container's healthcheck
# connects to it every few seconds, which can make the drop fail: try a few times.
loaded=0
for _ in 1 2 3 4 5 6; do
  svc_exec takserver-db psql -U postgres -d postgres -q -c "select pg_terminate_backend(pid) from pg_stat_activity where datname = 'cot' and pid <> pg_backend_pid()" > /dev/null 2>&1 || true
  if svc_exec takserver-db pg_restore -U postgres -d postgres --clean --if-exists --create < "$WORK/takserver.dump" > "$WORK/tak-restore.log" 2>&1; then
    loaded=1
    break
  fi
  sleep 2
done
[ "$loaded" = 1 ] || { tail -n 15 "$WORK/tak-restore.log" >&2; die "could not load TAK Server's database"; }
info "$(svc_exec takserver-db psql -U postgres -d cot -tAc "select count(*) || ' certificates known to TAK Server, ' || count(revocation_date) || ' of them revoked' from certificate")"

log "Loading the directory"
# The directory was just created empty by its image, with the service accounts and
# passwords of the restored deploy/.env. Its content is replaced by the export, offline.
compose stop openldap > /dev/null 2>&1
# Only the entries of the platform's tree are loaded. An export may hold an entry of another
# naming context (archives written before the export was narrowed carry the image's leftover
# "dc=nodomain"), which the directory's database refuses. Entries are separated by an empty
# line and, the export being unwrapped, each "dn:" is one line; a base64 "dn::" is kept.
awk -v base="$LDAP_BASE_DN" '
  BEGIN { RS = ""; ORS = "\n\n" }
  {
    keep = 1
    n = split($0, lines, "\n")
    for (i = 1; i <= n; i++) {
      if (index(lines[i], "dn: ") == 1) {
        dn = substr(lines[i], 5)
        tail = "," base
        keep = (dn == base) || (length(dn) > length(tail) && substr(dn, length(dn) - length(tail) + 1) == tail)
        break
      }
    }
    if (keep) print
  }' "$WORK/ldap-data.ldif" > "$WORK/ldap-restore.ldif"
grep -q "^dn: $LDAP_BASE_DN\$" "$WORK/ldap-restore.ldif" || die "the directory export does not hold $LDAP_BASE_DN"
chmod 644 "$WORK/ldap-restore.ldif"
compose run --rm --no-deps -T --entrypoint bash -v "$WORK/ldap-restore.ldif:/restore/ldap-data.ldif:ro" openldap -c '
    set -e
    rm -f /var/lib/ldap/data.mdb /var/lib/ldap/lock.mdb
    setpriv --reuid openldap --regid openldap --clear-groups /usr/sbin/slapadd -n 1 -q -F /etc/ldap/slapd.d -l /restore/ldap-data.ldif
  ' > "$WORK/ldap-restore.log" 2>&1 || { tail -n 15 "$WORK/ldap-restore.log" >&2; die "could not load the directory"; }
compose start openldap > /dev/null 2>&1
wait_healthy openldap 120 || die "the directory did not start again"
info "$(grep -c '^dn: ' "$WORK/ldap-restore.ldif") entries loaded"

# ----------------------------------------------------------------------------- 5. everything

log "Starting the whole stack"
"$REPO_DIR/install.sh" || die "the stack did not start; see the messages above"

log "Restored from $(basename "$ARCHIVE")"
info "The platform now runs its first reconciliation: administration, \"TAK server sync\", should show"
info "both backends reachable and nothing corrected. If it reports changes, read them before acting."
info "An access regenerated or removed after $(sed -n 's/^created: //p' "$WORK/manifest.txt") is back as it was then."
