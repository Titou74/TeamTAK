# TAK Manager

A self-service web platform for a TAK Server, centred on teams. A user asks for a team, an
administrator approves it, the team leader invites the members and chooses how they connect to
TAK Server; the platform creates the TAK accounts, groups and certificates and keeps them in line
with its own database. Teams never see each other, except for the duration of an event they agree
to share. For the administrator it also watches the server and the stack, raises alerts, takes
backups and keeps an audit log.

- Specification and decisions: [`docs/PLAN.md`](docs/PLAN.md)
- Monitoring, alerts, backups and audit log, what was verified and how: [`docs/phase-3-report.md`](docs/phase-3-report.md)
- Cross-team events, what was verified and how: [`docs/phase-2-report.md`](docs/phase-2-report.md)
- The core of the platform, what was verified: [`docs/phase-1-report.md`](docs/phase-1-report.md)
- Technical validation of TAK Server 5.8 with OpenLDAP: [`docs/phase-0-report.md`](docs/phase-0-report.md)

## What is in the box

| Service | Role |
|---|---|
| `caddy` | Web entry point: HTTPS with Let's Encrypt in production, plain HTTP in local mode |
| `app` | The platform: Rust API (axum, sqlx) serving the React UI as static files, and the reconciliation loop |
| `postgres` | The platform's database, the source of truth |
| `openldap` | Directory of TAK accounts and groups, written by the platform, read by TAK Server |
| `takserver`, `takserver-db` | TAK Server 5.8 and its database, built from the official tak.gov ZIP |
| `agent` | The host agent: reads the state of the machine and of this stack, takes the backups, restarts a service on request. The only service that holds the Docker socket |
| `mailpit` | Local mode only: catches outgoing e-mail so login codes can be read |

Published ports: the web UI (443 and 80 in production, 8480 in local mode), **8089** (TAK clients,
mutual TLS) and **8446** (TAK certificate enrollment). Port 8443 (TAK Server API) is not published:
the platform reaches it over the Docker network.

## Requirements

- Docker with Compose v2. Developed on an arm64 Mac (Docker 29, OrbStack); the target is an amd64
  Linux VPS with 8 GB of RAM.
- `takserver-docker-hardened-5.8-RELEASE-103.zip` from [tak.gov](https://tak.gov), placed at the
  repository root. It is under the tak.gov licence and is never versioned (see `.gitignore`).
  No Iron Bank account and no `docker login` are needed.
- About 12 GB of disk for the images, `unzip`, and `bash`. `openssl` to restore an encrypted backup.
- On Linux, run the install as root: it sets the ownership of the files mounted into the containers.

## Install

### Local mode (development, tests, first look)

```bash
./install.sh --local --admin-email you@example.org
```

The script builds the two TAK Server images from the ZIP, generates the secrets
(`deploy/.env`), a certificate authority and TAK Server's certificates (`deploy/data/`), builds the
platform image, starts everything, declares the platform's certificate as TAK Server administrator,
and creates the first administrator. Allow ten minutes the first time, one minute afterwards.

It ends by printing the address of the UI (`http://localhost:8480`) and a one-time login code for
the administrator. Other login codes arrive in the mail catcher at `http://localhost:8025`.

To let phones on the same network reach TAK Server, give the machine's LAN address; the server
certificate then carries it and the ports listen on all interfaces:

```bash
./install.sh --local --host 192.168.1.20 --admin-email you@example.org
```

### Production

The domain must already point to the machine, and ports 80, 443, 8089 and 8446 must be open.

```bash
sudo ./install.sh --domain tak.example.org --admin-email you@example.org --smtp-host smtp-relay.brevo.com --smtp-port 587 --smtp-username 'SMTP_LOGIN' --smtp-password 'SMTP_KEY' --mail-from 'TAK Manager <no-reply@example.org>'
```

Caddy obtains the Let's Encrypt certificate for the web UI by itself. TAK clients connect to
`tak.example.org:8089`.

Add `--tak-enroll-letsencrypt` to also present the Let's Encrypt certificate on the enrollment
port, which QR-code enrollment needs (see [QR codes and port 8446](#qr-codes-and-port-8446)).

### Options

| Option | Meaning | Default |
|---|---|---|
| `--local` / `--domain <name>` | Mode: local, or production for this domain | local |
| `--host <name-or-ip>` | Local mode: address TAK clients use | `localhost` |
| `--admin-email <address>` | First administrator | none |
| `--cert-validity-days <n>` | Validity of TAK client certificates | 365 |
| `--tak-enroll-letsencrypt` | Production: Let's Encrypt certificate on port 8446 | off |
| `--smtp-host`, `--smtp-port`, `--smtp-username`, `--smtp-password`, `--smtp-tls`, `--mail-from` | Outgoing e-mail | mail catcher in local mode |
| `--acme-email <address>` | Contact given to Let's Encrypt | the administrator's |
| `--zip <path>` | TAK Server ZIP | the one at the repository root |
| `--upgrade-tak` | Move the installation to the TAK Server version of another ZIP ([Upgrading TAK Server](#upgrading-tak-server)) | |
| `--no-start` | Prepare everything without starting | |

Mode, addresses and certificate validity are read on the first run only. Afterwards the settings
live in `deploy/.env`: edit that file and run `./install.sh` again to apply a change. The script is
safe to run again; it keeps existing settings, certificates and images.

### Settings worth knowing (`deploy/.env`)

| Variable | Meaning |
|---|---|
| `CERT_VALIDITY_DAYS` | Validity of client certificates issued by enrollment. A certificate inside a zip stops working when it expires and nothing renews it; the platform records each expiry |
| `DEFAULT_MEMBER_CAP` | Member cap given to a new team (the administrator can change it per team) |
| `LOGIN_MAX_CODES_PER_EMAIL`, `LOGIN_MAX_CODES_PER_IP`, `LOGIN_MAX_FAILURES_PER_IP` | Hourly limits of the login codes |
| `RECONCILE_INTERVAL_SECONDS` | Period of the reconciliation loop |
| `MONITOR_INTERVAL_SECONDS` | How often the platform looks at the machine, the services and TAK Server, and evaluates the alerts (60; 10 in local mode) |
| `AGENT_TOKEN` | Secret shared by the platform and the host agent, generated by the install |
| `P12_FORMAT` | `modern` (AES-256, what TAK Server's own scripts produce) or `legacy` (for Android 12 and older) |
| `TAK_API_MAX_HEAP`, `TAK_MESSAGING_MAX_HEAP`, … | Java heap sizes of TAK Server in MB; empty lets it size them from the machine's RAM |
| `SMTP_*`, `MAIL_FROM` | Outgoing e-mail |

## Run

All commands are run from `deploy/`.

```bash
cd deploy
```

```bash
docker compose ps
```

```bash
docker compose logs -f app
```

```bash
docker compose down
```

```bash
docker compose up -d
```

`docker compose down` keeps the data (Docker volumes and `deploy/data/`). After `up -d`, TAK Server
takes about a minute to become healthy.

### Commands of the platform

```bash
docker compose exec app tak-manager status
```

Checks that the database, the directory and TAK Server are reachable, and says whether the host
agent answers.

```bash
docker compose exec app tak-manager grant-admin you@example.org
```

**Rescue command.** Makes the account a platform administrator (creating it if needed) and prints
a one-time login code. It works when e-mail is down: on the sign-in page, type that address, choose
"I already have a code" and enter it.

```bash
docker compose exec app tak-manager reconcile
```

Runs one reconciliation pass and prints what it changed.

```bash
docker compose exec agent tak-manager-agent report
```

Prints what the host agent sees, as JSON: the machine, this stack's containers, TAK Server's
processes, the backup archives.

### Start again from nothing

```bash
deploy/scripts/clean.sh
```

Removes the containers, the volumes, `deploy/.env` and `deploy/data/`: every account, team and
certificate is lost, including the certificate authority. `--images` also removes the
`tak-manager/*` images. The backup archives in `deploy/backups/` are kept.

## How it works

- **Accounts.** Sign-in is passwordless: a six-digit code sent by e-mail, valid ten minutes, usable
  once, rate-limited per address and per IP. Anyone can sign in; an account without a team can do
  nothing and has no TAK access.
- **Teams.** One team is one TAK group (its name is the channel shown in ATAK). An administrator
  approves team requests and sets the member cap; that is the only approval step. A team leader
  invites members by e-mail, up to the cap.
- **TAK accesses.** The leader picks the mode: one shared identity for the team, one account per
  member, or named accounts for people outside the platform (external accesses can be added in
  every mode). Each access can be handed over as a QR code, as a username and password, or as a
  zip (with or without a client certificate). No zip ever holds a callsign, a team colour, a role
  or a group.
- **Certificates** are issued by TAK Server's own enrollment, including those shipped in zips: the
  platform submits the signing request with the access's account. *Regenerate* revokes every
  certificate of the access, changes its password and enrolls again. *Remove* revokes, then deletes
  the account.
- **Cross-team events.** A team leader creates an event with a start and an end and invites other
  teams; each invited team's leader accepts or declines. No administrator is involved. While the
  event runs, every TAK account of the host and of the teams that accepted is also in a group
  named `evt-<event name>`, which ATAK and iTAK show as one more channel: the teams see each
  other. The group appears at the start and disappears at the end by itself, on devices that stay
  connected; nothing is added to any zip. A team that leaves, is removed or is suspended loses it
  at once. Dates are entered and shown in the browser's time zone and stored in UTC.
- **Reconciliation.** PostgreSQL is the source of truth. After every change, every minute, and at
  the instant an event starts or ends, a pass compares it with OpenLDAP and TAK Server and corrects
  the difference: accounts, group memberships, passwords, certificates that should not work,
  sessions left with no group. What a pass corrects without the platform having asked for it is
  reported as drift in the administration area.

### QR codes and port 8446

A zip carries its own truststore and works with the private certificate authority. A QR code
carries none: the phone must already trust the certificate presented on port 8446, which per the
official TAK Server guide means a publicly trusted one. With `--tak-enroll-letsencrypt`,
`deploy/scripts/sync-letsencrypt.sh` converts the certificate Caddy obtained into a Java keystore,
points the 8446 connector at it and restarts TAK Server. The install runs it once; afterwards the
platform asks the host agent to run it every day, which does nothing unless the certificate was
renewed, and an administrator can ask for it at once from the monitoring page ("Hand over now").
There is no cron job. **This path could not be tested** (no domain was available); see the
phase 1 and phase 3 reports.

## Monitoring

The **Monitoring** page, for administrators, shows what the platform last saw; nothing on it is
probed while a page loads.

| Shown | Where it comes from |
|---|---|
| The server: processor, load, memory, swap, each filesystem (Docker's data included), uptime, whether the clock is synchronised, pending system updates with the security ones apart, "restart required" | The host agent |
| The services of this stack: state, health, restarts, image, and whether a newer image exists | The host agent, and the registries for the last point |
| TAK Server: ports 8089, 8443 and 8446, version, its five processes, connected devices | The platform itself, over the Docker network; the processes from the agent |
| Certificates: root and intermediate authority, TAK Server's, the platform's, the Let's Encrypt ones, client certificates | The platform's files, what each port presents, and its database |
| The platform: last clean synchronisation, drift, whether the starts and ends of events were applied, e-mails that failed | The platform's database |
| A history of processor, memory, disk and connected devices | One sample every five minutes, kept 14 days |

**The host agent** is a small Rust program in its own container. It is the only component that
sees the machine (three host directories, read-only: `/etc`, `/var/lib`, `/run`) and the Docker
socket; the web application has neither. It listens on a Docker network that has no route to the
outside and that only the platform joins, and every call needs a secret generated by the install.
It can do four things besides reporting: restart one service of this stack, write a backup, hand
the Let's Encrypt certificate to TAK Server, and tidy TAK Server's logs. It has no way to run a
command, and refuses anything that is not a service of the Compose project `tak-manager`, whatever
else runs on the same Docker.

Without the agent the platform keeps working: the monitoring page says the agent is missing, shows
what the platform checks by itself, and tells which command brings it back.

**What the web UI does not do**, on purpose: it never installs system updates, never restarts the
server and never pulls an image. It reports, and shows the command to run over SSH:

```bash
sudo apt update && sudo apt full-upgrade
```

```bash
cd deploy && docker compose pull && docker compose up -d
```

Pending updates are known on Debian and Ubuntu only (elsewhere the page says "unknown"). The agent
simulates an upgrade against the package lists the server last downloaded; it does not download
them. If the page says the lists are old, the server's own daily `apt update` is not running
(`unattended-upgrades` or `apt-daily.timer`).

"A newer image exists" means, for an image pulled by tag (Caddy, PostgreSQL), that the registry now
serves another image under the same tag. The directory's image is pinned to one exact build; a
newer build is shown for information. Registries are asked twice a day at most.

**Connected devices.** Administrators see every device connected to TAK Server: its callsign, the
access it uses, its team, its channels, since when. A team leader sees, on the team's page, the
devices connected with the accesses of their own team; with a shared identity it is the only way
to know how many devices use it. Members do not see this list.

### Alerts

An alert is sent by e-mail to every administrator, in their own language: once when it is raised,
once when it is resolved, and at most once a day while it lasts. Everything raised or resolved by
the same look goes in one message. A problem must be seen twice in a row before it is an alert, so
a service that restarts does not write to anyone.

| Alert | Severity |
|---|---|
| A filesystem nearly full (90 % by default) | warning, then critical |
| Memory under pressure (92 %) | warning |
| A service stopped or missing; a service restarting in a loop; one of TAK Server's processes gone | critical |
| A service failing its health check | warning |
| TAK Server not answering; the directory not answering | critical |
| No clean synchronisation for 10 minutes | warning, then critical |
| An event not started on time; an event not ended on time | warning; critical |
| A certificate of the platform expiring (30 days, then 7); expired | warning, then critical |
| Client certificates expiring | minor, then warning |
| A backup failed; no backup for 36 hours | warning |
| Security updates waiting; restart required | minor |
| E-mails failing | warning |
| The server's clock not synchronised | warning |
| The host agent not answering | warning |

Thresholds, the reminder period and the e-mail switch are in **Monitoring, Settings**.

The leaders of a team are also told by e-mail, 30 and 7 days ahead, when a certificate shipped in
one of the team's zips nears its end: nothing renews it, the access must be regenerated and the
zip handed out again.

**Webhook.** If an address is set in the settings, each alert raised, made worse, recalled or
resolved is also sent there as an HTTP `POST` with a JSON body:

```json
{
  "source": "tak-manager",
  "version": 1,
  "event": "raised",
  "text": "The service caddy is not running (state: exited).",
  "url": "https://tak.example.org/monitoring",
  "alert": {
    "id": 12,
    "kind": "service_down",
    "subject": "caddy",
    "severity": "critical",
    "details": { "state": "exited" },
    "raised_at": "2026-11-19T17:40:12Z",
    "resolved_at": null
  }
}
```

`event` is `raised`, `escalated`, `reminder`, `resolved`, or `test` (the button of the settings
page, with `alert` null). `kind` is a stable code (`service_down`, `disk_full`,
`certificate_expiring`, …: the list is `AlertKind` in `crates/core/src/alerts.rs`), `severity` is
`low`, `warning` or `critical`, `subject` names what the alert is about when a kind can have
several at once, and `text` is one English sentence that a chat room can show as it is. A webhook
that does not answer is not retried; the next event is sent normally.

### Audit log

**Monitoring, Audit log** lists who did what, when, on which team or object, and from which
address: every state-changing action (requests and approvals, invitations, roles, modes, accesses
created, regenerated or removed, credentials shown and zips downloaded, events), administrator
sign-ins, uses of the rescue command, and what the host agent did. It can be filtered by period,
team, actor and action. No secret is ever recorded. Entries are kept one year by default.

## Backups and restore

Every day (03:00 UTC by default) the host agent writes one archive to `deploy/backups/`:

| In the archive | What it is |
|---|---|
| `platform.dump` | The platform's database (`pg_dump`) |
| `ldap-data.ldif` | The directory (`slapcat`) |
| `takserver.dump` | TAK Server's database (`pg_dump`) |
| `deploy/data/` | The certificate authority, TAK Server's configuration, Caddy's certificates (no logs) |
| `deploy/.env` | Settings and secrets, `APP_SECRET` among them |

Nothing is stopped to take it. The 14 latest archives are kept. The schedule, the number kept and
a **passphrase** are in **Monitoring, Settings**; **Monitoring, Backups** lists the archives and
the runs and has a "Back up now" button.

**An archive holds every secret of the platform**, the key of the certificate authority included.
Set a passphrase: the archives are then encrypted (AES-256, key derived with PBKDF2, by
`openssl`). Keep that passphrase somewhere else: the copy in the settings is inside the encrypted
archive, and a restore asks for it.

A backup can also be taken by hand on the server:

```bash
sudo deploy/scripts/backup.sh
```

### Copying the archives elsewhere

The platform writes to `deploy/backups/` and stops there. Sending the archives off the machine is
yours to set up, because where and with which credentials is yours to decide.
[`deploy/scripts/offsite-copy.example.sh`](deploy/scripts/offsite-copy.example.sh) is a starting
point: a script that sends each new archive once (rsync, rclone or restic), to run from the host's
cron after the hour of the backup.

### Restoring

```bash
sudo deploy/scripts/restore.sh deploy/backups/tak-manager-backup-20261119T031500Z.tar.gz
```

For an encrypted archive the script asks for the passphrase (or reads `BACKUP_PASSPHRASE`).

It replaces **everything**: the platform's database, the directory, TAK Server's database,
`deploy/.env` and `deploy/data/`. The four parts are always restored together. The previous
`deploy/.env` and `deploy/data/` are moved to `deploy/backups/replaced-<date>/`, not deleted. Then
it starts the stack and checks that the platform reaches the directory and TAK Server. Allow five
minutes.

On a **new machine**: install Docker, clone this repository, put the TAK Server ZIP of the same
version at its root, copy the archive over, and run the same command. The certificate authority
comes back with the archive, so the zips already handed out keep working, provided the machine
answers at the same name or address.

What to know:

- A restore is the moment of the backup. What was done since is lost, **and an access regenerated
  or removed since then works again with its old certificate**: regenerate it again.
- The first reconciliation after a restore should correct nothing. If the administration, "TAK
  server sync", shows corrections, read them before acting.

The whole path can be replayed on a local installation, which it destroys and restores:

```bash
scripts/prove-restore.sh --yes
```

## Updating

| What | How | Said by the monitoring page |
|---|---|---|
| The server's packages | `sudo apt update && sudo apt full-upgrade`, then `sudo reboot` if asked | pending updates, security updates, restart required |
| Caddy, PostgreSQL, the mail catcher | `cd deploy && docker compose pull && docker compose up -d` | "Newer image available" |
| The directory (OpenLDAP) | Pinned by digest in `deploy/docker-compose.yml`: change the digest, test, then `./install.sh` | "Pinned; a newer build exists" |
| This platform | `git pull`, then `sudo ./install.sh` | |
| TAK Server | Below | "TAK Server … is available", or a ZIP of another version at the repository root |

### Upgrading TAK Server

tak.gov has no way to be asked automatically. Record in **Monitoring, Settings** the latest
version you saw there; the monitoring page then says when it is newer than the installed one. It
also notices by itself a ZIP of another version at the repository root.

1. Download `takserver-docker-hardened-<new version>.zip` from tak.gov and put it at the root of
   the repository, next to the installed one.
2. Read the release notes of that version for changes to `CoreConfig.xml`.
3. Run the upgrade:

```bash
sudo ./install.sh --upgrade-tak
```

It takes a backup first, builds the two images of the new version, checks that the new database
image runs the same major version of PostgreSQL as the installed one (it stops before changing
anything otherwise), writes the new version in `deploy/.env`, and restarts the stack; TAK Server's
database image upgrades its own schema when it starts. It then checks that the platform reaches
TAK Server, as a normal install does.

To go back: put `TAK_VERSION`, `TAK_IMAGE` and `TAK_DB_IMAGE` back to the old version in
`deploy/.env` and run `sudo ./install.sh`. If the new version already changed the database, restore
the backup taken at step 3 instead.

If the PostgreSQL versions differ, the data volume cannot be reused as it is: take a backup, remove
the volume `tak-manager_tak-db-data`, start the new version on an empty database, and load
`takserver.dump` of the backup into it with `pg_restore`.

**This procedure could not be tested**: only one TAK Server ZIP exists so far. The steps it is
built from (backup, image build, start, checks) are the ones the install and the restore run.

## Develop

Layout:

```
crates/core/      domain rules, reconciliation, alert rules, LDAP and TAK Server adapters, zips (no HTTP, no SQL)
crates/server/    the API, the database, e-mail, the monitoring loop, the `tak-manager` binary; migrations/, tests/
crates/agent/     the host agent (`tak-manager-agent`) and the report it produces
web/              React + TypeScript + Vite, Tailwind, shadcn/ui, react-i18next
deploy/           Docker Compose, Caddy, OpenLDAP bootstrap files, TAK Server configuration, scripts
scripts/          test runners
spike/            the phase 0 validation stack, kept as a record
docs/             plan and phase reports
```

The directory and TAK Server are reached through two traits (`Directory`, `TakServer` in
`crates/core`); each has an in-memory implementation used by the unit tests.

To work on the UI with hot reload against the stack running in Docker:

```bash
cd web && pnpm install && pnpm dev
```

To rebuild and restart the platform after a change:

```bash
./install.sh
```

### Languages

The UI is in English by default, French is available, and every text goes through translation
keys. To add a language, add `web/src/locales/<code>/translation.json` and `email.json` with the
same keys as English, and one entry in `web/src/i18n/languages.ts`. The server picks the e-mail
texts up from the same folder at build time. `pnpm check:i18n` verifies that every language has the
same keys and placeholders and that every key used by the code exists.

## Test

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

```bash
scripts/test-integration.sh
```

Runs the integration tests against the real stack, which must be up in local mode
(`./install.sh --local`). They drive the platform through its HTTP API, read login codes in the
mail catcher, read what was written to OpenLDAP, and connect scripted TAK clients to TAK Server on
8089 and 8446. They prove, among other things, that two members of a team see each other, that
another team does not see them, that regenerating a shared identity cuts every old certificate,
and that two teams see each other from the scheduled start of an event to its scheduled end and
not a moment outside it, without reconnecting. For the monitoring they prove that the host agent
reports this stack and refuses every other container, that a stopped service raises one alert
e-mail in each administrator's language and one more when it is back, who sees which connected
devices, what the audit log records, and that a backup asked from the API is written and readable.
The tests use random names and can be replayed on the same stack. A word after the command runs
only the tests whose name contains it. The whole suite takes about ten minutes; it stops and
restarts the `app` container, stops the reverse proxy for a minute, and restarts the mail catcher.

```bash
scripts/prove-restore.sh --yes
```

Proves the restore on a local installation: prepares teams, accesses and zips, takes a backup,
destroys the project's volumes and generated files, restores, and checks with the zips of before.
