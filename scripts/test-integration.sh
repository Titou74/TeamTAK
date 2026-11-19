#!/usr/bin/env bash
# Integration tests against the real stack: the platform, OpenLDAP and TAK Server.
#
# Start the stack in local mode first (./install.sh --local). The tests drive the platform
# through its HTTP API, read the login codes in the mail catcher, look at what was written
# to OpenLDAP, and connect scripted TAK clients to TAK Server.
#
#   scripts/test-integration.sh                       the whole suite (about two minutes)
#   scripts/test-integration.sh regenerating          the tests whose name contains a word
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo > /dev/null || export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

[ -f deploy/.env ] || { echo "deploy/.env is missing: run ./install.sh --local first" >&2; exit 1; }
grep -q '^MODE=local$' deploy/.env || { echo "the integration tests need the local mode (mail catcher)" >&2; exit 1; }
for service in app openldap takserver mailpit; do
  id="$(cd deploy && docker compose ps -q "$service" 2>/dev/null | head -n 1)"
  state="$([ -n "$id" ] && docker inspect -f '{{.State.Status}}' "$id" 2>/dev/null || echo absent)"
  [ "$state" = running ] || { echo "service $service is $state: start the stack with ./install.sh --local" >&2; exit 1; }
done

# One test at a time: several of them watch what the reconciliation reports.
cargo test -p tak-manager-server --test e2e -- --ignored --test-threads=1 "$@"
