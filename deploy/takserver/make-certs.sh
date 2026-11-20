#!/bin/bash
# Runs INSIDE a throwaway container of the TAK Server image (install.sh pipes it to bash),
# in /opt/tak/certs, where the official certificate scripts live.
#
# It creates, with those scripts:
#   - a root CA,
#   - an intermediate CA that signs everything else, including enrollment certificates,
#   - TAK Server's own certificate,
#   - the client certificate the platform uses on the TAK Server API.
#
# Two things differ from running makeCert.sh as shipped, both done on a patched COPY of the
# script inside this container (nothing from the ZIP is modified or copied elsewhere):
#   - validity: makeCert.sh hard-codes 730 days for every certificate, CA included. An
#     intermediate CA that expires after two years would take every client certificate
#     with it, so its validity is set here;
#   - names: makeCert.sh puts a single name in the server certificate. The server is
#     reached as "takserver" inside the Docker network and by its public name from phones,
#     so both must be in the certificate.
#
# Environment: ROOT_CA_NAME, INT_CA_NAME, SERVER_CERT_NAME, API_CERT_NAME, EXTRA_ALT_NAMES
# (lines "DNS.2 = name" / "IP.1 = address"), INT_CA_DAYS, SERVER_CERT_DAYS, API_CERT_DAYS,
# CAPASS, PASS, STATE, CITY, ORGANIZATIONAL_UNIT.
set -euo pipefail
cd /opt/tak/certs

grep -q -- '-days 730' makeCert.sh || { echo "makeCert.sh: validity not found, the script changed" >&2; exit 1; }
grep -q '^\$ALTNAMEFIELD = \$SNAME$' makeCert.sh || { echo "makeCert.sh: alt name line not found, the script changed" >&2; exit 1; }
sed -e 's/-days 730/-days ${CERT_DAYS}/' \
    -e 's/^\$ALTNAMEFIELD = \$SNAME$/&\n${EXTRA_ALT_NAMES:-}/' \
    makeCert.sh > makeCert-teamtak.sh
chmod u+x makeCert-teamtak.sh

./makeRootCa.sh --ca-name "$ROOT_CA_NAME"
# MOVE_CA=true: the intermediate becomes the CA that signs what follows.
CERT_DAYS="$INT_CA_DAYS" MOVE_CA=true ./makeCert-teamtak.sh ca "$INT_CA_NAME"
CERT_DAYS="$SERVER_CERT_DAYS" ./makeCert-teamtak.sh server "$SERVER_CERT_NAME"
CERT_DAYS="$API_CERT_DAYS" EXTRA_ALT_NAMES="" ./makeCert-teamtak.sh client "$API_CERT_NAME"

cd files
# The platform reads its key from a file only it can read; no password to store next to it.
openssl pkcs8 -topk8 -nocrypt -in "$API_CERT_NAME.key" -passin "pass:$PASS" -out "$API_CERT_NAME-plain.key"

# Check that the patches took effect.
sans="$(openssl x509 -in "$SERVER_CERT_NAME.pem" -noout -ext subjectAltName)"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  name="${line#*= }"
  echo "$sans" | grep -q -- "$name" || { echo "server certificate lacks the name $name" >&2; exit 1; }
done <<< "DNS.1 = $SERVER_CERT_NAME
${EXTRA_ALT_NAMES:-}"
min_seconds=$(( (INT_CA_DAYS - 1) * 86400 ))
openssl x509 -in "$INT_CA_NAME.pem" -noout -checkend "$min_seconds" > /dev/null \
  || { echo "intermediate CA validity is shorter than $INT_CA_DAYS days" >&2; exit 1; }

echo "--- certificates"
for c in root-ca "$INT_CA_NAME" "$SERVER_CERT_NAME" "$API_CERT_NAME"; do
  echo "$c: $(openssl x509 -in "$c.pem" -noout -subject -enddate | tr '\n' ' ')"
done
echo "$sans"
