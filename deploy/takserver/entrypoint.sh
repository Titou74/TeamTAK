#!/bin/bash
# Entrypoint of the takserver container, in place of the image's own.
#
# The image starts five Java processes in the background and then waits forever: if one of
# them dies, the container keeps "running" with part of TAK Server missing (phase 0 report,
# section 11). This script starts them the same way, then watches them. When one is gone it
# exits, so that Docker's restart policy brings the whole server back.
set -u

LOG=/opt/tak/logs/takserver.log
GRACE_SECONDS="${TAK_SUPERVISOR_GRACE_SECONDS:-90}"
PERIOD_SECONDS="${TAK_SUPERVISOR_PERIOD_SECONDS:-15}"

# One pattern per process started by /opt/tak/configureInDocker.sh.
PATTERNS=(
  "spring.profiles.active=config"
  "spring.profiles.active=messaging"
  "spring.profiles.active=api"
  "takserver-retention.jar"
  "takserver-pm.jar"
)

say() { echo "[teamtak supervisor] $*" | tee -a "$LOG"; }

# The image has no ps or pgrep: read the command lines from /proc.
running() {
  local pattern="$1" f
  for f in /proc/[0-9]*/cmdline; do
    if tr '\0' ' ' < "$f" 2>/dev/null | grep -q -- "^java .*$pattern"; then
      return 0
    fi
  done
  return 1
}

stop_all() {
  say "stopping"
  pkill_all
  exit 0
}

pkill_all() {
  local f pid
  for f in /proc/[0-9]*/cmdline; do
    pid="${f#/proc/}"; pid="${pid%/cmdline}"
    if tr '\0' ' ' < "$f" 2>/dev/null | grep -q -- "^java "; then
      kill "$pid" 2>/dev/null
    fi
  done
}

trap stop_all TERM INT

mkdir -p /opt/tak/logs
# "init" makes the image's script start the processes and then block; it runs in the
# background here so this script stays the container's main process.
/opt/tak/configureInDocker.sh init 2>&1 | tee -a "$LOG" &

say "started; first check in ${GRACE_SECONDS}s, then every ${PERIOD_SECONDS}s"
sleep "$GRACE_SECONDS" &
wait $!

while true; do
  for pattern in "${PATTERNS[@]}"; do
    if ! running "$pattern"; then
      say "process '$pattern' is not running any more: exiting so the container restarts"
      pkill_all
      exit 1
    fi
  done
  sleep "$PERIOD_SECONDS" &
  wait $!
done
