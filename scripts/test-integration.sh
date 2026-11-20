#!/usr/bin/env bash
# Integration tests against the real stack: the platform, OpenLDAP and TAK Server.
#
# Start the stack in local mode first (./install.sh --local). The tests drive the platform
# through its HTTP API, read the login codes in the mail catcher, look at what was written
# to OpenLDAP, and connect scripted TAK clients to TAK Server.
#
#   scripts/test-integration.sh                       the whole suite (about ten minutes)
#   scripts/test-integration.sh regenerating          the tests whose name contains a word
#
# Three files: tests/e2e.rs (accounts, teams, TAK accesses, reconciliation), tests/events.rs
# (cross-team events) and tests/monitoring.rs (the host agent, alerts, connected devices,
# the audit log, backups). One of the event tests stops and restarts the platform's
# container; the monitoring tests stop the reverse proxy for a minute, stop and restart the
# mail catcher, and write (then delete) two small backup archives in deploy/backups/.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo > /dev/null || export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

[ -f deploy/.env ] || { echo "deploy/.env is missing: run ./install.sh --local first" >&2; exit 1; }
grep -q '^MODE=local$' deploy/.env || { echo "the integration tests need the local mode (mail catcher)" >&2; exit 1; }
for service in app openldap takserver mailpit agent caddy; do
  id="$(cd deploy && docker compose ps -q "$service" 2>/dev/null | head -n 1)"
  state="$([ -n "$id" ] && docker inspect -f '{{.State.Status}}' "$id" 2>/dev/null || echo absent)"
  [ "$state" = running ] || { echo "service $service is $state: start the stack with ./install.sh --local" >&2; exit 1; }
done

# One test at a time: several of them watch what the reconciliation reports.
cargo test -p teamtak-server --test e2e --test events --test monitoring -- --ignored --test-threads=1 "$@"
