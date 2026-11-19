#!/usr/bin/env bash
# Builds the TAK Server images from the official "hardened" Docker ZIP of tak.gov, with its
# Dockerfiles untouched and without any registry login (no Iron Bank account needed).
#
# Usage: build-tak-images.sh <zip> <takserver image> <database image>
#
# The ZIP is extracted to deploy/data/build/ (git-ignored) and the extraction is removed at
# the end: nothing from the ZIP stays outside the images.
#
# ROCKY_REPOS_HTTPS=0 disables the following workaround. The .repo files of the ZIP point to
# http://dl.rockylinux.org in plain HTTP; on some networks large HTTP downloads are cut and
# the database image fails with "Curl error (18): Transferred a partial file" (phase 0
# report, section 3). The same host serves HTTPS, so the extracted copy is switched to it.
. "$(dirname "$0")/lib.sh"

ZIP="$1"
IMG_TAK="$2"
IMG_DB="$3"
BUILD_DIR="$DATA_DIR/build"

[ -f "$ZIP" ] || die "TAK Server ZIP not found: $ZIP"

have() { docker image inspect "$1" > /dev/null 2>&1; }
if have "$IMG_TAK" && have "$IMG_DB"; then
  info "images already built: $IMG_TAK, $IMG_DB"
  exit 0
fi

command -v unzip > /dev/null || die "unzip is required to extract the TAK Server ZIP"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
trap 'rm -rf "$BUILD_DIR"' EXIT

log "Extracting $(basename "$ZIP")"
unzip -q "$ZIP" -d "$BUILD_DIR"
SRC="$(find "$BUILD_DIR" -maxdepth 1 -mindepth 1 -type d | head -n 1)"
[ -f "$SRC/docker/Dockerfile.hardened-takserver" ] || die "unexpected ZIP layout: docker/Dockerfile.hardened-takserver not found"
info "TAK Server $(cat "$SRC/tak/version.txt" 2>/dev/null || echo '(version file missing)')"

if [ "${ROCKY_REPOS_HTTPS:-1}" = 1 ]; then
  for f in "$SRC"/tak/security/rpms/repos/*.repo; do
    [ -f "$f" ] || continue
    sed 's#baseurl=http://dl.rockylinux.org#baseurl=https://dl.rockylinux.org#' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
  done
fi

build() { # image dockerfile
  local image="$1" dockerfile="$2" start logfile
  if have "$image"; then info "already built: $image"; return; fi
  log "Building $image ($dockerfile), a few minutes"
  start=$(date +%s)
  logfile="$DATA_DIR/build-$(echo "$dockerfile" | tr -c 'A-Za-z0-9.\n' '-').log"
  # Up to three attempts: package mirrors fail now and then.
  local attempt
  for attempt in 1 2 3; do
    if docker build --progress=plain -t "$image" -f "$SRC/docker/$dockerfile" "$SRC" > "$logfile" 2>&1; then
      info "built in $(( $(date +%s) - start )) s"
      rm -f "$logfile"
      return
    fi
    info "attempt $attempt failed, see $logfile"
  done
  tail -n 30 "$logfile" >&2
  die "could not build $image"
}

build "$IMG_TAK" Dockerfile.hardened-takserver
build "$IMG_DB" Dockerfile.hardened-takserver-db
