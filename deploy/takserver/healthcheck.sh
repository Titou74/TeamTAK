#!/bin/bash
# Healthcheck of the takserver container. The image's own check needs an administrator
# certificate inside the container; this one needs nothing:
#   - 8446 answers 401 to an enrollment request without credentials: the API process is up;
#   - 8443 and 8089 accept connections: the API and the messaging process listen.
code="$(curl -sk -o /dev/null -m 8 -w '%{http_code}' https://localhost:8446/Marti/api/tls/config)"
[ "$code" = 401 ] || { echo "8446 answered $code instead of 401"; exit 1; }
nc -z localhost 8443 || { echo "8443 is closed"; exit 1; }
nc -z localhost 8089 || { echo "8089 is closed"; exit 1; }
