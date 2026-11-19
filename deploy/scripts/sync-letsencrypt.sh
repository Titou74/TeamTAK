#!/usr/bin/env bash
# Hands Caddy's Let's Encrypt certificate to TAK Server's enrollment port (8446).
#
# Why: a QR code carries no truststore, so a phone enrolling from a QR code must already
# trust the certificate presented on 8446. Per the official guide (appendix E, "Quick
# Connect") that means a publicly trusted certificate. Port 8446 is served by TAK Server,
# not by Caddy, so the certificate Caddy obtained for the web UI is converted to a Java
# keystore and TAK Server is restarted to load it.
#
# Run once after the first start (install.sh does it with --tak-enroll-letsencrypt), then
# daily: it does nothing unless the certificate changed (renewal, every ~60 days).
# Restarting TAK Server disconnects the clients for about a minute; they reconnect alone.
#
#   sync-letsencrypt.sh [--wait <seconds>] [--install-cron] [--force]
#
# NOT TESTED: no domain was available when this was written (phase 1 report).
. "$(dirname "$0")/lib.sh"
load_env

WAIT=0 INSTALL_CRON=0 FORCE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --wait) WAIT="$2"; shift ;;
    --install-cron) INSTALL_CRON=1 ;;
    --force) FORCE=1 ;;
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
else
  log "Converting the Let's Encrypt certificate for TAK Server"
  # PEM -> PKCS#12 -> JKS with the tools of the TAK Server image; written under a temporary
  # name and moved into place so TAK Server never reads a half-written keystore.
  docker run --rm -u 0 --entrypoint bash \
    -v "$crt:/in/cert.crt:ro" -v "$key:/in/cert.key:ro" -v "$DATA_DIR/tak/certs:/out" \
    -e PASS="$TAK_CERT_PASSWORD" -e FINGERPRINT="$fingerprint" "$TAK_IMAGE" -c '
      set -e
      openssl pkcs12 -export -in /in/cert.crt -inkey /in/cert.key -name letsencrypt \
        -out /tmp/le.p12 -passout "pass:$PASS"
      rm -f /out/letsencrypt.jks.new
      keytool -importkeystore -noprompt -alias letsencrypt \
        -srckeystore /tmp/le.p12 -srcstoretype PKCS12 -srcstorepass "$PASS" \
        -destkeystore /out/letsencrypt.jks.new -deststoretype JKS -deststorepass "$PASS" -destkeypass "$PASS"
      mv /out/letsencrypt.jks.new /out/letsencrypt.jks
      printf "%s" "$FINGERPRINT" > /out/letsencrypt.cksum
      chown 1001:0 /out/letsencrypt.jks /out/letsencrypt.cksum
      chmod 640 /out/letsencrypt.jks
    '
  render_core_config
  log "Restarting TAK Server to load it"
  compose restart takserver
  wait_healthy takserver 420 || die "TAK Server did not come back healthy"
  info "port 8446 now presents the Let's Encrypt certificate of $PUBLIC_HOST"
fi

if [ "$INSTALL_CRON" = 1 ]; then
  if [ "$(uname -s)" = Linux ] && [ -d /etc/cron.d ] && [ "$(id -u)" = 0 ]; then
    printf '%s\n' "# TAK Manager: reload TAK Server when the Let's Encrypt certificate is renewed" \
      "23 4 * * * root $DEPLOY_DIR/scripts/sync-letsencrypt.sh >> /var/log/tak-manager-letsencrypt.log 2>&1" \
      > /etc/cron.d/tak-manager-letsencrypt
    info "daily check installed: /etc/cron.d/tak-manager-letsencrypt"
  else
    info "no /etc/cron.d here: schedule $DEPLOY_DIR/scripts/sync-letsencrypt.sh to run daily"
  fi
fi
