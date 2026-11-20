#!/usr/bin/env bash
# Writes one backup archive of the whole platform into deploy/backups/.
#
# The archive holds everything needed to rebuild the platform on this or another machine:
#
#   ldap-data.ldif      the directory (accounts, groups), exported with slapcat
#   ldap-config.ldif    the directory's configuration, for reference only
#   takserver.dump      TAK Server's database (pg_dump, custom format)
#   platform.dump       the platform's database (pg_dump, custom format)
#   deploy/.env         settings and secrets, APP_SECRET among them
#   deploy/data/        the certificate authority, TAK Server's configuration, Caddy's
#                       certificates (logs left out)
#   manifest.txt        what the archive is and where it comes from
#
# IT HOLDS EVERY SECRET OF THE PLATFORM. Give BACKUP_PASSPHRASE to encrypt it (AES-256,
# key derived with PBKDF2), and keep that passphrase somewhere else than in the platform:
# the copy stored in its settings is inside the archive.
#
#   deploy/scripts/backup.sh
#   BACKUP_PASSPHRASE='…' BACKUP_KEEP=30 deploy/scripts/backup.sh
#
# The databases are dumped by their own tools while the stack runs; nothing is stopped.
# The directory is exported first and the platform's database last: restored together, the
# platform never knows less than the directory, so the reconciliation that follows a restore
# has nothing to delete.
#
# Run every day by the host agent on the platform's request (administration, "Backups").
# Run by hand, it does the same. Restore with deploy/scripts/restore.sh.
. "$(dirname "$0")/lib.sh"
load_env

BACKUP_DIR="${BACKUP_DIR:-$DEPLOY_DIR/backups}"
KEEP="${BACKUP_KEEP:-14}"
PASSPHRASE="${BACKUP_PASSPHRASE:-}"
case "$KEEP" in ''|*[!0-9]*|0) die "BACKUP_KEEP must be a number of archives, at least 1" ;; esac

command -v docker > /dev/null || die "docker is required"
[ -z "$PASSPHRASE" ] || command -v openssl > /dev/null || die "openssl is required to encrypt the archive"
mkdir -p "$BACKUP_DIR"
[ -w "$BACKUP_DIR" ] || die "cannot write to $BACKUP_DIR"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
name="teamtak-backup-$stamp.tar.gz"
[ -z "$PASSPHRASE" ] || name="$name.enc"
# In the destination folder: the archive is moved into place at the end, never half-written
# under its final name.
work="$(mktemp -d "$BACKUP_DIR/.work-XXXXXX")"
trap 'rm -rf "$work"' EXIT
chmod 700 "$work"

log "Exporting the directory"
# Only the platform's tree (-s): the image's database also holds a leftover "dc=nodomain"
# entry of its packaging, which cannot be loaded back and is not ours.
svc_exec openldap slapcat -n 1 -s "$LDAP_BASE_DN" -o ldif-wrap=no > "$work/ldap-data.ldif" || die "could not export the directory (slapcat)"
grep -q "^dn: $LDAP_BASE_DN\$" "$work/ldap-data.ldif" || die "the directory export does not hold $LDAP_BASE_DN"
svc_exec openldap slapcat -n 0 -o ldif-wrap=no > "$work/ldap-config.ldif" 2> /dev/null || info "note: the directory's configuration could not be exported (not needed for a restore)"

log "Dumping TAK Server's database"
svc_exec takserver-db pg_dump -U postgres -d cot -Fc > "$work/takserver.dump" || die "could not dump TAK Server's database"

log "Dumping the platform's database"
svc_exec postgres pg_dump -U teamtak -d teamtak -Fc > "$work/platform.dump" || die "could not dump the platform's database"

for dump in takserver.dump platform.dump; do
  [ "$(head -c 5 "$work/$dump")" = PGDMP ] || die "$dump is not a PostgreSQL dump"
done

{
  echo "format: 1"
  echo "created: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "tak_version: ${TAK_VERSION:-}"
  echo "mode: ${MODE:-}"
  echo "public_host: ${PUBLIC_HOST:-}"
  echo "ldap_base_dn: $LDAP_BASE_DN"
  echo "ldap_entries: $(grep -c '^dn: ' "$work/ldap-data.ldif")"
} > "$work/manifest.txt"

log "Writing the archive"
members="manifest.txt ldap-data.ldif takserver.dump platform.dump"
[ -s "$work/ldap-config.ldif" ] && members="$members ldap-config.ldif"
# Logs are not part of a backup, and neither are leftovers of an interrupted image build.
pack() {
  local status=0
  # shellcheck disable=SC2086
  tar -cz -f - \
    --exclude 'deploy/data/tak/logs/*' --exclude 'deploy/data/build' --exclude 'deploy/data/*.log' \
    -C "$work" $members \
    -C "$REPO_DIR" deploy/.env deploy/data || status=$?
  # GNU tar ends with 1 when a file changed while it was read (Caddy's storage, on a live
  # stack): the archive is complete, and it is read back below.
  if [ "$status" = 1 ] && tar --version 2> /dev/null | grep -q 'GNU tar'; then status=0; fi
  return "$status"
}
if [ -n "$PASSPHRASE" ]; then
  export BACKUP_PASSPHRASE="$PASSPHRASE"
  pack | openssl enc -aes-256-cbc -pbkdf2 -iter 200000 -md sha256 -salt -pass env:BACKUP_PASSPHRASE -out "$work/$name" \
    || die "could not write the archive"
  check() { openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -md sha256 -pass env:BACKUP_PASSPHRASE -in "$work/$name" | tar -tz -f -; }
else
  pack > "$work/$name" || die "could not write the archive"
  check() { tar -tz -f "$work/$name"; }
fi

# An archive that cannot be read back is not a backup.
listing="$(check)" || die "the archive cannot be read back"
for member in manifest.txt ldap-data.ldif takserver.dump platform.dump deploy/.env "deploy/data/tak/certs/$INT_CA_NAME.pem" deploy/data/tak/CoreConfig.xml; do
  printf '%s\n' "$listing" | grep -q -x "$member" || die "the archive lacks $member"
done

chmod 600 "$work/$name"
mv "$work/$name" "$BACKUP_DIR/$name"
size="$(wc -c < "$BACKUP_DIR/$name" | tr -d ' ')"
info "$BACKUP_DIR/$name ($size bytes$([ -n "$PASSPHRASE" ] && echo ', encrypted'))"

# Retention: the newest $KEEP archives stay. Only files named by this script are counted
# or deleted; anything else in the folder is left alone.
pruned=0 seen=0
for archive in $(ls -1 "$BACKUP_DIR" | grep -E '^teamtak-backup-[0-9]{8}T[0-9]{6}Z\.tar\.gz(\.enc)?$' | sort -r); do
  seen=$((seen + 1))
  if [ "$seen" -gt "$KEEP" ]; then
    rm -f "$BACKUP_DIR/$archive"
    pruned=$((pruned + 1))
  fi
done
[ "$pruned" = 0 ] || info "$pruned older archive(s) deleted (keeping $KEEP)"

# Read by the agent.
echo "BACKUP_RESULT file=$name size=$size encrypted=$([ -n "$PASSPHRASE" ] && echo 1 || echo 0) pruned=$pruned"
