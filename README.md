# TeamTAK

**One TAK Server, shared by a whole community of teams.**

TeamTAK runs a [TAK Server](https://tak.gov) for a community (an association, a network of
volunteer teams, a group of clubs or units) and gives each team its own space on it. Teams organise
themselves: a leader invites the members and hands out their accesses, without waiting for anyone.
Each team keeps to itself by default, and teams meet on a common channel for the time of an event
they agree to share.

The administrator welcomes new teams and looks after the server. TeamTAK takes care of the rest:
TAK accounts, groups and certificates, monitoring, alerts, backups and an audit log.

One script installs everything on one server with Docker Compose, TAK Server 5.8 included.

## How a community uses it

1. **Someone asks for a team.** Anyone can sign in with their e-mail address; an administrator
   approves the new team. It is the only step that needs one.
2. **The leader brings the team in.** They invite the members by e-mail and choose how the team
   connects: one shared identity, one account per member, or named accesses for people outside
   the platform.
3. **Everyone gets connected.** Each access is handed over as a QR code, a username and password,
   or a ready-made zip. In ATAK and iTAK the team is one channel.
4. **Teams work together when they choose to.** A leader creates an event and invites other teams;
   each team accepts or declines. From the start of the event to its end, a shared channel appears
   on every device, then goes away, without anyone reconnecting.
5. **The server stays healthy.** The administrator sees the machine, the services and the connected
   devices, gets an e-mail when something goes wrong, and has a backup every day.

## Features

**For teams**

- **Passwordless sign-in**: a six-digit code sent by e-mail. No password to remember.
- **Self-service**: team leaders invite and remove members, and create, regenerate or withdraw
  accesses themselves.
- **Three connection modes**: a shared identity for the team, one account per member, or named
  accesses for guests.
- **Three ways to hand an access over**: QR code, username and password, or a zip (data package),
  with or without a client certificate.
- **Private by default**: a team never sees another team outside an event they share.

**For working together**

- **Cross-team events**: teams see each other from the start to the end of an event, then stop,
  with no administrator involved.
- **Leaders see who is on**: the devices connected with their team's accesses.
- **English and French**, in the UI and in the e-mails; other languages are two files away.

**For the administrator**

- **One approval step**: new teams, with a member cap per team.
- **Always in line**: the platform's database is the reference; the directory and TAK Server are
  corrected to match it, and any drift is reported.
- **Monitoring and alerts**: machine, services, TAK Server, certificates and connected devices,
  with e-mail alerts and an optional webhook.
- **Daily backups**, optionally encrypted, and a tested restore.
- **Audit log** of every action that changes something.

## Requirements

- A Linux server, amd64, with **8 GB of RAM** and about **12 GB of disk** for the images.
  **Ubuntu Server LTS (24.04)** is the recommended system.
- **Docker with Compose v2**, plus `git`, `unzip`, `bash` and `openssl`.
- **`takserver-docker-hardened-5.8-RELEASE-103.zip`** from [tak.gov](https://tak.gov) (a free
  account is needed to download it). It is under the tak.gov licence, so it is not in this
  repository and must never be committed (`.gitignore` excludes it). No Iron Bank account and no
  `docker login` are needed.
- For production: a **domain name** pointing to the server, and an **SMTP account** to send the
  login codes (Brevo, for example).

## Installation

### Production

**1. Prepare the server.** On Ubuntu Server 24.04 LTS, install Docker Engine and its Compose
plugin by following [Docker's instructions for Ubuntu](https://docs.docker.com/engine/install/ubuntu/),
then the tools the scripts use:

```bash
sudo apt update && sudo apt install -y git unzip openssl
```

**2. Point the domain and open the ports.** Create a DNS record for the domain (for example
`tak.example.org`) that points to the server, and open these ports in the firewall: 80/tcp,
443/tcp and 443/udp (web), 8089/tcp (TAK clients) and 8446/tcp (enrollment). The domain must
resolve before the install runs, because Caddy asks Let's Encrypt for the certificate as soon as it
starts.

**3. Get the code.**

```bash
git clone https://github.com/Titou74/teamtak.git
```

```bash
cd teamtak
```

**4. Add the TAK Server ZIP.** Download `takserver-docker-hardened-5.8-RELEASE-103.zip` from
tak.gov and put it at the root of the repository. From your own computer, for example:

```bash
scp takserver-docker-hardened-5.8-RELEASE-103.zip user@tak.example.org:teamtak/
```

**5. Run the install**, as root, with your domain, the address of the first administrator and
your SMTP settings:

```bash
sudo ./install.sh --domain tak.example.org --admin-email you@example.org --smtp-host smtp-relay.brevo.com --smtp-port 587 --smtp-username 'SMTP_LOGIN' --smtp-password 'SMTP_KEY' --mail-from 'TeamTAK <no-reply@example.org>'
```

The script builds the two TAK Server images from the ZIP, generates the secrets (`deploy/.env`),
a certificate authority and TAK Server's certificates (`deploy/data/`), builds the platform and
agent images, starts everything, declares the platform's certificate as TAK Server administrator,
and creates the first administrator. Allow ten minutes the first time, one minute afterwards.

Add `--tak-enroll-letsencrypt` to also present the Let's Encrypt certificate on the enrollment
port, which QR-code enrollment needs (see
[QR codes and port 8446](docs/architecture.md#qr-codes-and-port-8446)).

**6. Sign in.** The script ends by printing the address of the UI and a one-time login code for
the administrator. Open `https://tak.example.org`, type the administrator's address, choose
"I already have a code" and enter it. TAK clients connect to `tak.example.org:8089`.

**7. Protect what was generated.** Three paths now hold everything that cannot be rebuilt. None is
versioned, and none should ever be shared:

| Path | Holds |
|---|---|
| `deploy/.env` | Settings and every secret, `APP_SECRET` among them |
| `deploy/data/` | The certificate authority, TAK Server's certificates and configuration, Caddy's certificates |
| `deploy/backups/` | The daily archives, which contain the two above and the three databases |

In **Monitoring, Settings**, set a backup passphrase, then arrange for the archives to leave the
server ([Copying the archives elsewhere](docs/backups.md#copying-the-archives-elsewhere)).

**8. Welcome the first teams.** Share the address of the UI with the community: each future team
leader signs in and asks for their team, and you approve it from the administration area.

### Local mode (development, tests, first look)

With Docker running and the ZIP at the repository root:

```bash
./install.sh --local --admin-email you@example.org
```

It ends by printing the address of the UI (`http://localhost:8480`) and a one-time login code for
the administrator. Other login codes arrive in the mail catcher at `http://localhost:8025`. On
Linux, run it with `sudo`.

To let phones on the same network reach TAK Server, give the machine's LAN address; the server
certificate then carries it and the ports listen on all interfaces:

```bash
./install.sh --local --host 192.168.1.20 --admin-email you@example.org
```

### Install options

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

Mode, addresses and certificate validity are read on the **first run only**. Afterwards the
settings live in `deploy/.env` ([configuration](docs/configuration.md)): edit that file and run
`./install.sh` again to apply a change. The script is safe to run again; it keeps existing
settings, certificates and images.

### If the install stops

The script says which step failed and prints the end of the relevant log. Fix the cause and run the
same command again: it resumes from what already exists. If login codes do not arrive, the rescue
command prints one without e-mail:

```bash
cd deploy && docker compose exec app teamtak grant-admin you@example.org
```

## Updating

There are five things to keep up to date. The monitoring page tells when most of them have
something waiting; nothing is ever updated from the web UI, on purpose.

| What | How | Said by the monitoring page |
|---|---|---|
| TeamTAK itself | `git pull`, then `sudo ./install.sh` ([below](#updating-teamtak)) | |
| Caddy, PostgreSQL, the mail catcher | `cd deploy && docker compose pull && docker compose up -d` | "Newer image available" |
| The directory (OpenLDAP) | Pinned by digest in `deploy/docker-compose.yml`: change the digest, test, then `./install.sh` | "Pinned; a newer build exists" |
| The server's packages | `sudo apt update && sudo apt full-upgrade`, then `sudo reboot` if asked | pending updates, security updates, restart required |
| TAK Server | [Upgrading TAK Server](#upgrading-tak-server) | "TAK Server … is available", or a ZIP of another version at the repository root |

### Updating TeamTAK

Run from the root of the repository, on the server.

**1. Take a backup.** It is the way back if the new version does not suit you.

```bash
sudo deploy/scripts/backup.sh
```

**2. Get the new version.**

```bash
git pull
```

**3. Apply it.**

```bash
sudo ./install.sh
```

No option is needed: the script reads `deploy/.env`. It keeps the settings, the secrets, the
certificate authority and the TAK Server images, adds to `deploy/.env` the settings a new version
introduces, rebuilds the platform and agent images, and restarts the services that changed. The
platform applies its database migrations by itself when it starts. The script then runs the same
checks as a first install and stops with an error if the platform cannot reach the directory or
TAK Server.

**4. Check.** The **Monitoring** page should show every service running, and:

```bash
cd deploy && docker compose exec app teamtak status
```

**Going back.** Database migrations are not undone by returning to older code. Check out the
previous version (`git checkout <commit>`), run `sudo ./install.sh`, then restore the archive of
step 1 ([Restoring](docs/backups.md#restoring)).

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
the volume `teamtak_tak-db-data`, start the new version on an empty database, and load
`takserver.dump` of the backup into it with `pg_restore`.

**This procedure could not be tested**: only one TAK Server ZIP exists so far. The steps it is
built from (backup, image build, start, checks) are the ones the install and the restore run.

## Documentation

Everything else is in [`docs/`](docs/README.md):

- [Architecture](docs/architecture.md): services, ports, how teams, accesses, events and
  reconciliation work
- [Configuration](docs/configuration.md) of `deploy/.env`
- [Day-to-day commands](docs/operations.md)
- [Monitoring, alerts and audit log](docs/monitoring.md), webhook format included
- [Backups and restore](docs/backups.md)
- [Development](docs/development.md) and [testing](docs/testing.md)
- The [specification](docs/PLAN.md) and the reports of each development phase
