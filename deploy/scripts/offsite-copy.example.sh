#!/usr/bin/env bash
# Example of a hook that copies the backup archives off this machine.
#
# TeamTAK writes its archives to deploy/backups/ and stops there: a backup that only
# lives on the disk it protects does not survive that disk. Sending the archives elsewhere
# is yours to set up, because where "elsewhere" is, and with which credentials, is yours to
# decide. This file is a starting point, not something the platform runs.
#
# How to use it:
#
#   1. Copy it outside the repository (so an update does not overwrite it), for example to
#      /usr/local/sbin/teamtak-offsite, and adapt the "send" function below.
#   2. Run it from the host's scheduler, after the hour of the daily backup (03:00 UTC by
#      default, set in the administration under Monitoring, Settings). With cron, as root:
#
#        30 4 * * * /usr/local/sbin/teamtak-offsite /opt/teamtak/deploy/backups
#
#      or with a systemd path unit that starts it whenever the folder changes
#      (PathChanged=/opt/teamtak/deploy/backups).
#   3. Encrypt the archives (a passphrase in the administration, Monitoring, Settings):
#      what leaves the machine holds every secret of the platform.
#
# It sends each archive once: the names of those already sent are kept in a small file next
# to them. Deleting old copies at the destination is left to the destination (a lifecycle
# rule of the bucket, for example).
set -euo pipefail

BACKUP_DIR="${1:?usage: $0 <path to deploy/backups>}"
SENT_LIST="$BACKUP_DIR/.sent-offsite"

# Sends one archive. Replace with what fits: these are three common ways.
send() {
  local archive="$1"
  # rsync over SSH to another machine:
  #   rsync -a "$archive" backup@other-host.example.org:/srv/backups/teamtak/
  # rclone to an object store configured with `rclone config`:
  #   rclone copy "$archive" remote:teamtak-backups
  # restic, which also deduplicates and prunes:
  #   restic -r sftp:backup@other-host.example.org:/srv/restic backup "$archive"
  echo "offsite-copy: edit the send function of $0 before using it (would send $archive)" >&2
  return 1
}

touch "$SENT_LIST"
status=0
# Only the files the backup script names, finished ones (it writes under another name
# first), oldest first.
for archive in $(ls -1 "$BACKUP_DIR" | grep -E '^teamtak-backup-[0-9]{8}T[0-9]{6}Z\.tar\.gz(\.enc)?$' | sort); do
  grep -q -x -F "$archive" "$SENT_LIST" && continue
  if send "$BACKUP_DIR/$archive"; then
    echo "$archive" >> "$SENT_LIST"
  else
    echo "offsite-copy: could not send $archive" >&2
    status=1
  fi
done
exit "$status"
