#!/usr/bin/env bash
# Builds the three images of the ZIP, with its Dockerfiles untouched and without
# any `docker login` (no Iron Bank account).
# Usage: 01-build-images.sh [ca|takserver|db ...]   (default: all three)
#
# ROCKY_REPOS_HTTPS=1: network workaround, see README. The files
#   tak/security/rpms/repos/*.repo of the ZIP point to http://dl.rockylinux.org
#   (plain HTTP). On a network that cuts long HTTP downloads, dnf fails with
#   "Curl error (18): Transferred a partial file". With this variable, the three
#   files are switched to https:// for the duration of the build, in the extracted
#   copy (spike/work/), then restored to their original content.
. "$(dirname "$0")/lib.sh"

[ -d "$TAK_SRC" ] || "$SPIKE_DIR/scripts/00-extract.sh"
mkdir -p "$WORK_DIR/logs"
targets=("$@"); [ ${#targets[@]} -gt 0 ] || targets=(ca takserver db)

build() { # name image dockerfile [args...]
  local name="$1" image="$2" dockerfile="$3"; shift 3
  log "Build $image ($dockerfile)"
  local start; start=$(date +%s)
  docker build --progress=plain -t "$image" -f "$TAK_SRC/docker/$dockerfile" "$@" "$TAK_SRC" \
    > "$WORK_DIR/logs/build-$name.log" 2>&1 \
    || { tail -40 "$WORK_DIR/logs/build-$name.log"; echo "BUILD FAILED: $name" >&2; exit 1; }
  echo "OK in $(( $(date +%s) - start )) s — $(docker image inspect "$image" --format '{{.Architecture}} {{.Size}} bytes')"
}

REPOS_DIR="$TAK_SRC/tak/security/rpms/repos"
REPOS_BAK="$WORK_DIR/orig-repos"
restore_repos() {
  if [ -d "$REPOS_BAK" ]; then cp "$REPOS_BAK"/*.repo "$REPOS_DIR"/ && rm -rf "$REPOS_BAK"; fi
}
restore_repos   # in case a previous build was interrupted
if [ "${ROCKY_REPOS_HTTPS:-0}" = 1 ]; then
  log "ROCKY_REPOS_HTTPS=1: Rocky repositories over https for this build"
  mkdir -p "$REPOS_BAK" && cp "$REPOS_DIR"/*.repo "$REPOS_BAK"/
  trap restore_repos EXIT
  for f in "$REPOS_DIR"/*.repo; do
    sed 's#baseurl=http://dl.rockylinux.org#baseurl=https://dl.rockylinux.org#' "$REPOS_BAK/$(basename "$f")" > "$f"
  done
fi

for t in "${targets[@]}"; do
  case "$t" in
    ca) build ca "$IMG_CA" Dockerfile.ca \
          --build-arg ARG_CA_NAME=tak-spike-ca --build-arg ARG_STATE=XX \
          --build-arg ARG_CITY=Spike --build-arg ARG_ORGANIZATIONAL_UNIT=TAKManager ;;
    takserver) build takserver "$IMG_TAK" Dockerfile.hardened-takserver ;;
    db) build db "$IMG_DB" Dockerfile.hardened-takserver-db ;;
    *) echo "unknown target: $t" >&2; exit 1 ;;
  esac
done
docker images --filter 'reference=tak-spike/*'
