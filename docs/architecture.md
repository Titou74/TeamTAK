# Architecture

How TeamTAK is put together and what it does behind the web UI. To install it, see the
[README](../README.md).

## Services

Everything runs with Docker Compose, in the project `teamtak`.

| Service | Role |
|---|---|
| `caddy` | Web entry point: HTTPS with Let's Encrypt in production, plain HTTP in local mode |
| `app` | The platform: Rust API (axum, sqlx) serving the React UI as static files, and the reconciliation loop |
| `postgres` | The platform's database, the source of truth |
| `openldap` | Directory of TAK accounts and groups, written by the platform, read by TAK Server |
| `takserver`, `takserver-db` | TAK Server and its database, built from the official tak.gov ZIP of the installed version (5.8 so far) |
| `agent` | The host agent: reads the state of the machine and of this stack, takes the backups, restarts a service on request. The only service that holds the Docker socket |
| `mailpit` | Local mode only: catches outgoing e-mail so login codes can be read |

## Ports

| Port | Use |
|---|---|
| 443, 80 | Web UI in production (80 serves the Let's Encrypt challenge and redirects to HTTPS) |
| 8480 | Web UI in local mode |
| 8089, and any other port of `TAK_CLIENT_PORTS` | TAK clients, mutual TLS ([Client ports](configuration.md#client-ports)) |
| 8446 | TAK certificate enrollment |
| 8443, or `TAK_ADMIN_PORT` | TAK Server's own administration pages, served by Caddy to the platform's administrators ([TAK Server's administration](configuration.md#tak-servers-administration)) |

TAK Server's own port 8443 (its API and administration pages) is not published: the platform and
Caddy reach it over the Docker network. Every client port of the machine leads to the same TLS input of TAK Server, 8089 inside its
container: TAK Server itself has a single input, and the monitoring watches that one.

## How it works

- **Accounts.** Sign-in is passwordless: a six-digit code sent by e-mail, valid ten minutes, usable
  once, rate-limited per address and per IP. Anyone can sign in; an account without a team can do
  nothing and has no TAK access.
- **Teams.** One team is one TAK group (its name is the channel shown in ATAK). An administrator
  approves team requests and sets the member cap; that is the only approval step. A team leader
  invites members by e-mail, up to the cap.
- **TAK accesses.** The leader picks the mode: one shared identity for the team (the default), or
  one account per member. In either mode the leader can add named external accesses for people
  outside the platform. Each access can be handed over as a QR code, as a username and password, or as a
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

## QR codes and port 8446

A zip carries its own truststore and works with the private certificate authority. A QR code
carries none: the phone must already trust the certificate presented on port 8446, which per the
official TAK Server guide means a publicly trusted one. With `--tak-enroll-letsencrypt`,
`deploy/scripts/sync-letsencrypt.sh` converts the certificate Caddy obtained into a Java keystore,
points the 8446 connector at it and restarts TAK Server. The install runs it once; afterwards the
platform asks the host agent to run it every day, which does nothing unless the certificate was
renewed, and an administrator can ask for it at once from the monitoring page ("Hand over now").
There is no cron job. **This path could not be tested** (no domain was available); see the
[phase 1](phase-1-report.md) and [phase 3](phase-3-report.md) reports.

## The host agent

The host agent is a small Rust program in its own container. It is the only component that sees
the machine (four host directories, read-only: `/etc`, `/var/lib`, `/run`, and `/sys/fs/cgroup` for
what each container uses of the processor and the memory) and the Docker socket; the web
application has neither. It listens on a Docker network that has no route to the outside
and that only the platform joins, and every call needs a secret generated by the install.

It can do four things besides reporting: restart one service of this stack, write a backup, hand
the Let's Encrypt certificate to TAK Server, and tidy TAK Server's logs. It has no way to run a
command, and refuses anything that is not a service of the Compose project `teamtak`, whatever
else runs on the same Docker.

Without the agent the platform keeps working: the monitoring page says the agent is missing, shows
what the platform checks by itself, and tells which command brings it back.
