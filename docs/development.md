# Development

## Layout

```
crates/core/      domain rules, reconciliation, alert rules, LDAP and TAK Server adapters, zips (no HTTP, no SQL)
crates/server/    the API, the database, e-mail, the monitoring loop, the `teamtak` binary; migrations/, tests/
crates/agent/     the host agent (`teamtak-agent`) and the report it produces
web/              React + TypeScript + Vite, Tailwind, shadcn/ui, react-i18next
deploy/           Docker Compose, Caddy, OpenLDAP bootstrap files, TAK Server configuration, scripts
scripts/          test runners
spike/            the phase 0 validation stack, kept as a record
docs/             this documentation, the plan and the phase reports
```

The directory and TAK Server are reached through two traits (`Directory`, `TakServer` in
`crates/core`); each has an in-memory implementation used by the unit tests. How the services fit
together is in [architecture](architecture.md).

## Working locally

Install a local stack first (see "Local mode" in the [README](../README.md#local-mode-development-tests-first-look)):

```bash
./install.sh --local --admin-email you@example.org
```

To work on the UI with hot reload against the stack running in Docker:

```bash
cd web && pnpm install && pnpm dev
```

To rebuild and restart the platform after a change:

```bash
./install.sh
```

## Languages

The UI is in English by default, French is available, and every text goes through translation
keys. To add a language, add `web/src/locales/<code>/translation.json` and `email.json` with the
same keys as English, and one entry in `web/src/i18n/languages.ts`. The server picks the e-mail
texts up from the same folder at build time. `pnpm check:i18n` verifies that every language has the
same keys and placeholders and that every key used by the code exists.

## Tests

See [testing](testing.md).
