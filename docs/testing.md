# Testing

## Unit tests

```bash
scripts/test-unit.sh
```

Runs the Rust unit tests (domain rules, authorisation, login-code limits, the rules of events and
which groups an account gets at a given time, reconciliation against in-memory backends, zip and
PKCS#12 generation, e-mail texts, the alert rules, the reading of what a host reports with output
captured from real Debian and Ubuntu systems, what the host agent refuses, and the check that
every API error code, reconciliation event, alert kind and audit action is translated in every
language) and the web checks (translation files, lint rule against hard-coded text, type check).
It needs no running service.

## Integration tests

```bash
scripts/test-integration.sh
```

Runs the integration tests against the real stack, which must be up in local mode
(`./install.sh --local`). They drive the platform through its HTTP API, read login codes in the
mail catcher, read what was written to OpenLDAP, and connect scripted TAK clients to TAK Server on
8089 and 8446.

They prove, among other things, that two members of a team see each other, that another team does
not see them, that regenerating a shared identity cuts every old certificate, and that two teams
see each other from the scheduled start of an event to its scheduled end and not a moment outside
it, without reconnecting. For the monitoring they prove that the host agent reports this stack and
refuses every other container, that a stopped service raises one alert e-mail in each
administrator's language and one more when it is back, who sees which connected devices, what the
audit log records, and that a backup asked from the API is written and readable.

The tests use random names and can be replayed on the same stack. A word after the command runs
only the tests whose name contains it. The whole suite takes about ten minutes; it stops and
restarts the `app` container, stops the reverse proxy for a minute, and restarts the mail catcher.

## Restore proof

```bash
scripts/prove-restore.sh --yes
```

Proves the restore on a local installation: prepares teams, accesses and zips, takes a backup,
destroys the project's volumes and generated files, restores, and checks with the zips of before.
