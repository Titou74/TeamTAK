# Monitoring, alerts and audit log

## The monitoring page

The **Monitoring** page, for administrators, shows what the platform last saw; nothing on it is
probed while a page loads.

| Shown | Where it comes from |
|---|---|
| The server: processor, load, memory, swap, each filesystem (Docker's data included), uptime, whether the clock is synchronised, pending system updates with the security ones apart, "restart required" | The host agent |
| The services of this stack: state, health, restarts, image, and whether a newer image exists | The host agent, and the registries for the last point |
| TAK Server: ports 8089, 8443 and 8446, version, its five processes, connected devices | The platform itself, over the Docker network; the processes from the agent |
| Certificates: root and intermediate authority, TAK Server's, the platform's, the one of TAK Server's administration pages (when they are open), the Let's Encrypt ones, client certificates | The platform's files, what each port presents, and its database |
| The platform: last clean synchronisation, drift, whether the starts and ends of events were applied, e-mails that failed | The platform's database |
| TeamTAK's version, and the latest release published on GitHub | The platform itself, and GitHub for the last point |
| A history of processor, memory, disk and connected devices | One sample every five minutes, kept 14 days |

The machine and the Docker socket are seen through the host agent only; see
[architecture](architecture.md#the-host-agent).

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
newer build is shown for information. Registries are asked twice a day at most. The images built on
the server carry their version: TAK Server's (`teamtak/takserver:5.8-RELEASE-103`), and for the
platform and the agent the version of the checkout given by git (`teamtak/app:1.0.1`, or
`teamtak/app:1.0.1-3-g28f8b1b` three commits after that release).

**TeamTAK's own version.** The platform asks GitHub, twice a day at most (every hour while GitHub
does not answer), which release of TeamTAK is the latest: one anonymous request to
`api.github.com/repos/Titou74/TeamTAK/releases/latest`, which leaves drafts and pre-releases out.
The answer is kept across restarts. While the version that runs is newer than that answer (the
platform was updated to a release published since, or not published yet), GitHub is asked again
at once when that version starts, then every hour until it has it. The **Check again** button
asks it at once, whenever it was last asked. When it is newer than the version that runs, the page shows it with a link to its release notes and
the update command, and a minor alert is raised; the alert is resolved once the new version is
installed. Nothing is downloaded or installed by the platform: the update is
[done over SSH](../README.md#updating-teamtak). A fork points the check at its own repository, and
`off` turns it off, with `RELEASE_REPOSITORY` ([configuration](configuration.md)). The command
`teamtak status` prints the same information.

**Connected devices.** Administrators see every device connected to TAK Server: its callsign, the
access it uses, its team, its channels, since when. A team leader sees, on the team's page, the
devices connected with the accesses of their own team; with a shared identity it is the only way
to know how many devices use it. Members do not see this list.

## Alerts

An alert is sent by e-mail to every administrator, in their own language: once when it is raised,
once when it is resolved, and at most once a day while it lasts. Everything raised or resolved by
the same look goes in one message. A problem must be seen twice in a row before it is an alert, so
a service that restarts does not write to anyone.

A resolved alert is marked as such, in green on the page and in place of its severity in the
e-mail, with how long the problem lasted: its sentence still describes the problem as it was. On
the page, the list of resolved alerts opens by itself for a day after a resolution.

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
| A newer release of TeamTAK published on GitHub | minor |
| E-mails failing | warning |
| The server's clock not synchronised | warning |
| The host agent not answering | warning |

Thresholds, the reminder period and the e-mail switch are in **Monitoring, Settings**. Its button
**Send me a test e-mail** sends one to the administrator who clicks, in their language, and says
at once whether the mail server took it, or what it answered (the same text, without addresses,
as on the dashboard). The command `teamtak send-test-mail` does the same from the server (see
[operations](operations.md)).

The leaders of a team are also told by e-mail, 30 and 7 days ahead, when a certificate shipped in
one of the team's zips nears its end: nothing renews it, the access must be regenerated and the
zip handed out again.

## Webhook

If an address is set in the settings, each alert raised, made worse, recalled or resolved is also
sent there as an HTTP `POST` with a JSON body:

```json
{
  "source": "teamtak",
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
`certificate_expiring`, …: the list is `AlertKind` in
[`crates/core/src/alerts.rs`](../crates/core/src/alerts.rs)), `severity` is `low`, `warning` or
`critical`, `subject` names what the alert is about when a kind can have several at once, and
`text` is one English sentence that a chat room can show as it is. A webhook that does not answer
is not retried; the next event is sent normally.

## Audit log

**Monitoring, Audit log** lists who did what, when, on which team or object, and from which
address: every state-changing action (requests and approvals, invitations, roles, modes, accesses
created, regenerated or removed, credentials shown and zips downloaded, events), administrator
sign-ins, visits to [TAK Server's administration](configuration.md#tak-servers-administration) (at
most one entry an hour per administrator), uses of the rescue command, and what the host agent
did. It can be filtered by period,
team, actor and action. No secret is ever recorded. Entries are kept one year by default.
