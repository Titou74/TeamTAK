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
./install.sh --local --admin-email you@example.org --non-interactive
```

(Without `--non-interactive`, it asks for the settings not given.) To work on the UI with hot reload against the stack running in Docker:

```bash
cd web && pnpm install && pnpm dev
```

To rebuild and restart the platform after a change:

```bash
./install.sh
```

## Languages

The UI is in English by default, French is available, and every text goes through translation
keys. To add a language, add `web/src/locales/<code>/translation.json`, `email.json` and
`terms.json` (the [terms of use](architecture.md#terms-of-use)) with the same keys as English, and
one entry in `web/src/i18n/languages.ts`. The server picks the e-mail texts up from the same folder
at build time. `pnpm check:i18n` verifies that every language has the same keys and placeholders
and that every key used by the code exists.

## Tests

See [testing](testing.md).

## Releasing a version

Versions follow `MAJOR.MINOR.PATCH`. The version is written in two places, which must agree:
`version` under `[workspace.package]` in `Cargo.toml` (the three crates take it from there; the
server and the agent report it, and `install.sh` prints it), and `version` in `web/package.json`.

1. Change both, then update `Cargo.lock` with `cargo update -w --offline`.
2. Run the [tests](testing.md), commit on `main` and push.
3. Tag the commit `v<version>` (`git tag -a v1.0.2 -m "TeamTAK 1.0.2"`) and push the tag.
4. Publish a release of that tag on GitHub, titled `TeamTAK <version>`, with what changed and
   anything an administrator must do when updating. Leave pre-releases marked as such.

Installed platforms ask GitHub for the latest release twice a day (see
[monitoring](monitoring.md)): once it is published, their monitoring page and an alert tell their
administrators. A draft or a pre-release is not offered to them.

Installations run a release's tag, not `main`: in production, every run of `install.sh` fetches
the tags and moves the checkout to the highest `v<major>.<minor>.<patch>` one, then runs again
from there (`--release` picks another). What is committed on `main` reaches them only once it is
tagged; a tag with a suffix (`v1.1.0-rc.1`) is skipped. Local mode keeps the checkout as it is, so
`./install.sh` above runs the code being worked on.
