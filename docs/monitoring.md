# Monitoring, alerts and audit log

## The monitoring page

The **Monitoring** page, for administrators, shows what the platform last saw; nothing on it is
probed while a page loads.

| Shown | Where it comes from |
|---|---|
| The server: processor, load, memory, swap, each filesystem (Docker's data included), uptime, whether the clock is synchronised, pending system updates with the security ones apart, "restart required" | The host agent |
| The services of this stack: state, health, restarts, image, and whether a newer image exists | The host agent, and the registries for the last point |
| TAK Server: each port of `TAK_CLIENT_PORTS` (8089 by default), 8443 and 8446, version, its five processes, connected devices | The platform itself, over the Docker network; which ports Docker publishes, and the processes, from the agent |
| Certificates: root and intermediate authority, TAK Server's, the platform's, the one of TAK Server's administration pages (when they are open), the Let's Encrypt ones, client certificates | The platform's files, what each port presents, and its database |
| The platform: last clean synchronisation, drift, whether the starts and ends of events were applied, e-mails that failed | The platform's database |
| TeamTAK's version, and the latest release published on GitHub | The platform itself, and GitHub for the last point |
| What each service uses now: processor, memory, disk | The host agent |
| A history of the server's processor, memory, swap, fullest disk and connected devices, and of each service's processor, memory and disk | One sample a minute, kept 14 days |

The machine and the Docker socket are seen through the host agent only; see
[architecture](architecture.md#the-host-agent).

**TAK clients' ports.** The page shows one line per port of `TAK_CLIENT_PORTS`
([client ports](configuration.md#client-ports)). They all lead to TAK Server's single TLS input
(8089 inside its container), so a port answers when that input accepts connections and Docker
publishes the port to it, as the agent reads in TAK Server's container. A port listed in
`deploy/.env` but not published, because `./install.sh` was not run again after the change, is
shown as **not published** and raises the "TAK Server not answering" alert, which names it. The
platform does not connect to the ports of the machine themselves: from a container, that would
depend on the address they are bound to and on the machine's firewall, which the phones'
connections do not go through, and would raise false alarms. What a firewall or the network in
front of the server lets through is therefore not seen. Without the agent, the input alone
decides; the ports it last saw published are kept until it answers again.

**TAK Server's API.** The 8443 line is TAK Server's API, where devices read their channels
([Channels on the devices](architecture.md#channels-on-the-devices)). The platform reaches it over
the Docker network; it is shown as **not published**, with the same alert, when Docker does not
lead 8443 of the machine to it: devices then stream and see their team, but show no channel.

**Swap.** The server's card shows the share of the swap space in use, as the kernel gives it
(`SwapTotal` and `SwapFree` of `/proc/meminfo`), or "no swap" when the server has none, which is
common on a VPS: a server without swap is never shown as 0 % used. Swap in use is not a problem by
itself: the kernel moves out pages nobody has touched for a long time, and the share can stay high
for days after a peak. Whether memory is short now is what the memory figure says: it counts as
available what the kernel can give to programs without swapping (`MemAvailable`), and the "memory
under pressure" alert (92 % by default) is raised on it. There is no alert on swap.

Swap matters for TAK Server. Its five Java processes size their heaps from the machine's memory
unless told otherwise, and a heap partly in swap makes each garbage collection slow, which TAK
clients feel as delays. When memory stays high and the swap in use keeps growing, give TAK Server
smaller heaps (`TAK_API_MAX_HEAP`, `TAK_MESSAGING_MAX_HEAP`…, see
[configuration](configuration.md)) or the server more memory.

The history keeps the share of swap in use with each sample, one a minute, and draws it next to the
server's memory, so a slow climb shows. A server without swap has no swap chart. Swap is not
measured service by service.

**What each service uses.** The agent reads it for every container of the stack, at each look:

- **processor**: its share of the whole server since the previous look, all cores together, so
  the services add up to about the server's own figure. It is read in the kernel's control groups
  (`/sys/fs/cgroup`, mounted read-only into the agent);
- **memory**: what it uses, without the file cache the system can take back: the figure
  `docker stats` shows;
- **disk**: the space its data takes, its volumes and its folders under `deploy/data` (TAK Server's
  database, its logs and certificates, the platform's database, the directory…). Files of the
  image itself are not counted. The agent has no data of its own and shows none.

A figure the agent cannot read is shown as unknown: Docker keeping its data elsewhere than in
`/var/lib/docker`, or its control groups in an unusual place (rootless Docker).

**The history** keeps one sample a minute for 14 days, and one row an hour for ten years: each
hour's average and its highest minute, for the server and for each service (about 88,000 rows per
series over ten years). The hours are filled as the minutes come in, and the existing 14 days were
turned into hours when this history was added. A chart over one hour shows each sample; over longer
periods each point is the average of a few minutes (5 minutes over 24 hours, 34 over 7 days), or of
whole hours once the period reaches further back than the minutes (3 hours over 30 days, 15 over
six months), so that a chart never has more than 300 points. A light band above each line rises to
the highest sample of each point: a short peak stays visible however long the period. Under the
server's charts, one chart per service shows the chosen measure (processor, memory or disk), all on
the same scale, the heaviest service first. A gap in a line is a time the platform was not looking
(stopped, or being updated).

The period is the last hour, 6 hours, 24 hours, 7 days, 30 days, 3 or 6 months, or any two dates
within the ten years. To look for what makes the load rise:

- every chart shows the moment under the pointer (or the arrow keys) at once, with its value;
- dragging across a chart, or + on the keyboard, zooms all of them on that moment; zooming out goes
  back one step, and a zoom within the last 14 days gets one point a minute;
- the layout is a grid, full-width rows one above the other on the same time axis, or one combined
  chart. The combined chart draws processor, memory, swap, disk, connected devices and up to three
  services together, each curve against its own highest over the period, so that measures of
  different units can be compared; its legend gives the real values and hides or shows each curve.

The API is `GET /api/admin/monitoring/history?hours=N`, or `?from=…&to=…` in seconds since 1970.

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
