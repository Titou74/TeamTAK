#!/usr/bin/env bash
# Hands Caddy's Let's Encrypt certificate to TAK Server's enrollment port (8446).
#
# Why: a QR code carries no truststore, so a phone enrolling from a QR code must already
# trust the certificate presented on 8446. Per the official guide (appendix E, "Quick
# Connect") that means a publicly trusted certificate. Port 8446 is served by TAK Server,
# not by Caddy, so the certificate Caddy obtained for the web UI is converted to a Java
# keystore and TAK Server is restarted to load it.
#
# It does nothing unless the certificate changed (renewal, every 60 days or so). When it
# did, TAK Server is restarted: the clients are disconnected for about a minute and
# reconnect by themselves.
#
# Who runs it:
#   - install.sh, once, after the first start (--tak-enroll-letsencrypt);
#   - the host agent, every day on the platform's request, and when an administrator
#     clicks "Hand over now" (administration, "Monitoring"). There is no cron job any more.
# It can also be run by hand on the host:
#
#   sync-letsencrypt.sh [--wait <seconds>] [--force]
#
# It works the same on the host and inside the agent's container: the conversion runs in
# the takserver container, which has openssl and keytool and the certificate folder.
#
# NOT TESTED: no domain was available when this was written (phase 1 and phase 3 reports).
. "$(dirname "$0")/lib.sh"
load_env

WAIT=0 FORCE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --wait) WAIT="$2"; shift ;;
    --force) FORCE=1 ;;
    # Accepted and ignored: the daily check is the agent's job now.
    --install-cron) ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done

[ "$MODE" = production ] || die "Let's Encrypt needs production mode (a domain)"
[ "${TAK_ENROLL_LETSENCRYPT:-false}" = true ] || die "TAK_ENROLL_LETSENCRYPT is not true in $ENV_FILE"

CERT_ROOT="$DATA_DIR/caddy/data/caddy/certificates"
find_cert() { find "$CERT_ROOT" -type f -name "$PUBLIC_HOST.crt" 2>/dev/null | head -n 1; }

start=$(date +%s)
crt="$(find_cert)"
while [ -z "$crt" ] && [ $(( $(date +%s) - start )) -lt "$WAIT" ]; do
  sleep 5
  crt="$(find_cert)"
done
[ -n "$crt" ] || die "no certificate for $PUBLIC_HOST under $CERT_ROOT yet: is Caddy running and the domain pointing here?"
key="${crt%.crt}.key"
[ -f "$key" ] || die "private key not found next to $crt"

fingerprint="$(cksum < "$crt")"
stamp="$DATA_DIR/tak/certs/letsencrypt.cksum"
if [ "$FORCE" = 0 ] && [ -f "$stamp" ] && [ "$(cat "$stamp" 2>/dev/null)" = "$fingerprint" ] && [ -f "$DATA_DIR/tak/certs/letsencrypt.jks" ]; then
  info "certificate unchanged, nothing to do"
  echo "LETSENCRYPT_RESULT outcome=unchanged"
  exit 0
fi

log "Converting the Let's Encrypt certificate for TAK Server"
[ -n "$(svc_container takserver)" ] || die "the takserver container does not exist: start the stack first"
# PEM -> PKCS#12 -> JKS with the tools of the takserver container, where the certificate
# folder is mounted. The keystore password, the key and the certificate travel on standard
# input, never on a command line. The keystore is written under a temporary name and moved
# into place, so TAK Server never reads a half-written one.
{
  printf '%s\n' "$TAK_CERT_PASSWORD"
  printf '%s\n' "$fingerprint"
  cat "$key"
  echo "-----TAK-MANAGER-SEPARATOR-----"
  cat "$crt"
} | svc_exec takserver bash -c '
    set -e
    umask 077
    work="$(mktemp -d)"
    trap "rm -rf \"$work\"" EXIT
    IFS= read -r PASS
    IFS= read -r FINGERPRINT
    awk -v dir="$work" "BEGIN { out = dir \"/cert.key\" } /^-----TAK-MANAGER-SEPARATOR-----\$/ { out = dir \"/cert.crt\"; next } { print > out }"
    export PASS
    openssl pkcs12 -export -in "$work/cert.crt" -inkey "$work/cert.key" -name letsencrypt -out "$work/le.p12" -passout env:PASS
    out=/opt/tak/certs/files
    rm -f "$out/letsencrypt.jks.new"
    keytool -importkeystore -noprompt -alias letsencrypt \
      -srckeystore "$work/le.p12" -srcstoretype PKCS12 -srcstorepass:env PASS \
      -destkeystore "$out/letsencrypt.jks.new" -deststoretype JKS -deststorepass:env PASS -destkeypass:env PASS
    chmod 640 "$out/letsencrypt.jks.new"
    mv "$out/letsencrypt.jks.new" "$out/letsencrypt.jks"
    printf "%s" "$FINGERPRINT" > "$out/letsencrypt.cksum"
  ' || die "the conversion failed"

# The 8446 connector points at the keystore once it exists.
render_core_config
log "Restarting TAK Server to load it"
docker restart --time 30 "$(svc_container takserver)" > /dev/null
wait_healthy takserver 420 || die "TAK Server did not come back healthy"
info "port 8446 now presents the Let's Encrypt certificate of $PUBLIC_HOST"
echo "LETSENCRYPT_RESULT outcome=updated"
