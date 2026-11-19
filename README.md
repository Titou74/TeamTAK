# TAK Manager

A self-service web platform for a TAK Server, centred on teams. A user asks for a team, an
administrator approves it, the team leader invites the members and chooses how they connect to
TAK Server; the platform creates the TAK accounts, groups and certificates and keeps them in line
with its own database. Teams never see each other, except for the duration of an event they agree
to share.

- Specification and decisions: [`docs/PLAN.md`](docs/PLAN.md)
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
- About 12 GB of disk for the images, `unzip`, and `bash`.
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

Checks that the database, the directory and TAK Server are reachable.

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

### Start again from nothing

```bash
deploy/scripts/clean.sh
```

Removes the containers, the volumes, `deploy/.env` and `deploy/data/`: every account, team and
certificate is lost, including the certificate authority. `--images` also removes the
`tak-manager/*` images.

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
points the 8446 connector at it and restarts TAK Server; a daily cron entry repeats this when the
certificate is renewed. **This path could not be tested** (no domain was available); see the
phase 1 report.

## Develop

Layout:

```
crates/core/      domain rules, reconciliation, LDAP and TAK Server adapters, zips (no HTTP, no SQL)
crates/server/    the API, the database, e-mail, the `tak-manager` binary; migrations/, tests/
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
PKCS#12 generation, e-mail texts, and the check that every API error code and reconciliation
event is translated in every language) and the web checks (translation
files, lint rule against hard-coded text, type check). It needs no running service.

```bash
scripts/test-integration.sh
```

Runs the integration tests against the real stack, which must be up in local mode
(`./install.sh --local`). They drive the platform through its HTTP API, read login codes in the
mail catcher, read what was written to OpenLDAP, and connect scripted TAK clients to TAK Server on
8089 and 8446. They prove, among other things, that two members of a team see each other, that
another team does not see them, that regenerating a shared identity cuts every old certificate,
and that two teams see each other from the scheduled start of an event to its scheduled end and
not a moment outside it, without reconnecting. The tests use random names and can be replayed on
the same stack. A word after the command runs only the tests whose name contains it. The whole
suite takes about five minutes; one of the event tests stops and restarts the `app` container.
