# Phase 3 report — monitoring, alerts, backups and audit log

Date: 2026-11-19. Scope: section 3 "Host agent", section 4 "Monitoring (administrator)" and
section 5 "Phase 3" of [`PLAN.md`](PLAN.md), built on [`phase-1-report.md`](phase-1-report.md)
(section 10) and [`phase-2-report.md`](phase-2-report.md) (section 8). How to install, run, back
up, restore and test is in the [`README`](../README.md).

Environment: the same as phases 1 and 2 (Mac arm64, Docker 29.8 through OrbStack, TAK Server
`5.8-RELEASE-103` from the tak.gov ZIP, OpenLDAP 2.6, PostgreSQL 17). The TAK Server images were
not rebuilt. **The "host" the agent saw is OrbStack's Linux VM**, which is neither Debian nor
Ubuntu: everything that depends on a real Debian or Ubuntu server was exercised on captured
output and in containers, not on a host (section 5).

## 1. Summary

The administrator now has a **Monitoring** page that watches the server and the stack the way
infra-TAK and FastTAK do, alerts by e-mail, daily backups with a proven restore, and an audit
log. A team leader sees the devices connected with their team's accesses. It works end to end on
this machine.

| Area | State |
|---|---|
| Host agent (Rust, a Compose service) | Works; unit-tested, and tested on the real stack, including what it refuses |
| Dashboard: server, services, TAK Server, certificates, platform, history | Works; walked through in English and French |
| Alerts by e-mail (and webhook), raised, resolved, recalled | Works; a stopped service raised one e-mail per administrator in their language in 20 s, and one more when it was back |
| Connected devices, for administrators and team leaders | Works; tested with real TAK sessions |
| Audit log | Works; every state-changing action of phases 1 to 3, tested for content and for the absence of secrets |
| Backups, daily and on request, optionally encrypted | Works; tested |
| **Restore** | **Proven for real**, on the second attempt: the first one found a defect (section 4) |
| Actions from the UI: restart a service, back up, hand over Let's Encrypt | Restart and backup work and are tested; the hand-over is untestable here |
| Pending updates and "restart required" on a real Debian or Ubuntu host | **Not verified on a host** (parsers verified on real `apt` output) |
| Let's Encrypt, real SMTP, amd64, a TAK Server upgrade | **Not verified** |

Five things are worth knowing before reading further:

1. **The first real restore failed**, and that is why the proof was worth running. The directory
   export carried an entry that cannot be loaded back. Fixed on both sides; the restore then
   passed from the very archive that had failed, and again in one uninterrupted run (section 4).
2. **A restore un-revokes.** An access regenerated after the backup works again with its old
   certificate once the backup is restored. Observed, not assumed (section 4, step 5).
3. **Probing TAK Server's client port with TLS filled its log with errors**, one per look.
   Found by reading that log; the port is now only asked whether it accepts connections
   (section 6.1).
4. **The agent does not mount the whole host**, only `/etc`, `/var/lib` and `/run`, and has no
   route to the Internet (section 7).
5. **The development machine has two alerts open for good reasons of its own**: its disk is
   98 % full, and the kernel of the Docker VM reports its clock as not synchronised. They are
   real readings, and they exercised the alerting throughout.

## 2. What was built

```
crates/agent/                      the host agent: report types, parsers, Docker wrapper, actions, HTTP API
crates/core/src/alerts.rs          alert kinds, thresholds, the rules, raise / resolve / remind
crates/core/src/certs.rs           reading a certificate's subject and expiry
crates/core/src/versions.rs        TAK Server versions, image references
crates/core/src/policy.rs          four new actions in the single authorisation function
crates/server/migrations/0003_monitoring.sql
crates/server/src/audit.rs         the audit log: recording, and its API
crates/server/src/settings.rs      settings an administrator changes from the UI
crates/server/src/monitor/         the monitoring loop, probes, alerting, daily jobs, devices, API
crates/server/src/mail.rs          every e-mail attempt is counted
crates/server/tests/monitoring.rs  eight integration tests on the real stack
crates/server/tests/restore.rs     the two halves of the restore proof
deploy/agent/Dockerfile            the agent's image
deploy/scripts/backup.sh, restore.sh, offsite-copy.example.sh
deploy/scripts/sync-letsencrypt.sh rewritten to run from the agent as well as from the host
scripts/prove-restore.sh           replays the restore proof
web/src/pages/monitoring.tsx, web/src/components/monitoring/   the dashboard
web/src/locales/*/                 462 new UI texts and the alert e-mails, in English and French
```

### 2.1 The host agent

A small Rust binary (`tak-manager-agent`, about 2,400 lines with its tests) in its own container.
It is the only component with the Docker socket; the web application has none.

- **What it sees.** The Docker socket, and three host directories mounted read-only: `/etc` (the
  distribution and its package sources), `/var/lib` (the package database, Docker's disk) and
  `/run` (the "restart required" flag). Processor, load, memory and uptime come from its own
  `/proc`, which describes the whole machine for these. The repository is mounted read-only, with
  two folders writable: `deploy/backups` and `deploy/data/tak`.
- **Who can talk to it.** It listens on a Docker network declared `internal`, which has no route
  to the outside in either direction and which only `app` also joins. Every call must carry
  `AGENT_TOKEN`, generated by `install.sh`. Checked on the running stack: `caddy` cannot even
  resolve its name, and the agent cannot open a connection to the Internet.
- **What it answers.** `GET /v1/report`: the machine (processor, load, memory, swap, each
  filesystem, uptime, clock, pending updates, restart required), the containers of the Compose
  project `tak-manager` (state, health, restart count, image, registry digest), TAK Server's five
  Java processes (`docker top`), the backup archives, the state of the Let's Encrypt hand-over,
  and the versions of the TAK Server ZIPs at the repository root.
- **What it does**, and nothing else: `POST /v1/services/{service}/restart`, `/v1/backup`,
  `/v1/letsencrypt/sync`, `/v1/logs/rotate`. There is no way to name a command, a path or a
  container. A service is compared with a fixed list before Docker is asked anything, then
  resolved to a container identifier through the Compose labels of this project; it never
  restarts itself.
- **It never** applies updates, reboots, or pulls an image.
- **Distribution-specific part behind a trait** (`Distribution`): `Apt` for Debian, Ubuntu and
  what declares itself like Debian; nothing else. Elsewhere the report says "unknown".
  - *Pending updates*: the agent runs its own `apt-get -s dist-upgrade` (simulation) pointed at
    the host's package database and lists, read-only, and counts the packages, those coming from
    a security archive apart. It never refreshes the lists: their age is reported.
  - *Restart required*: the flag file `/run/reboot-required`, and the packages that asked.
  - *Clock*: `adjtimex(2)`, the kernel's own answer, the one `timedatectl` prints.

The platform works without the agent. The dashboard then says so in a banner, shows what the
platform checks by itself, and gives the command that brings the agent back; alerts that depend
on the agent are neither raised nor resolved while it is away.

### 2.2 The monitoring loop

One background task in the platform looks at everything every `MONITOR_INTERVAL_SECONDS` (60; 10
in local mode): the agent's report, TAK Server's three ports and the certificate each presents,
its version and sessions, the directory's port, and the platform's own tables. Every probe has a
timeout of a few seconds and they run together. The result is kept in memory; **every page reads
that, and no request of an ordinary user triggers a probe**. An administrator can ask for a fresh
look ("Look now").

- **TAK Server**: 8089 (connection only), 8443 and 8446 (connection and certificate),
  `/Marti/api/version`, `/Marti/api/subscriptions/all`. A newer version is known two ways: the
  administrator records the latest one seen on tak.gov, and a ZIP of another version at the
  repository root is noticed by the agent.
- **Certificates**: root and intermediate authority and the platform's API certificate from the
  platform's files; TAK Server's as presented on 8443; the enrollment port's when it differs
  (Let's Encrypt); the website's as Caddy presents it, in production. Client certificates from
  `tak_certificates`, which the reconciliation keeps in line with `GET /Marti/api/certadmin/cert`
  (never `…/cert/active`).
- **Images**: for those pulled by tag, the registry is asked which image the tag serves now
  (`HEAD` on the manifest, anonymous token) at most every twelve hours, the answers kept in the
  database. On this machine it found newer images of `caddy:2.11-alpine` and `postgres:17-alpine`.
- **The platform**: last clean reconciliation, drift over 24 hours and 7 days, starts and ends of
  events not yet applied (`reconcile_status.desired_at`), and e-mail failures. Every e-mail
  attempt, in the foreground or the background, is now counted; a failure records which e-mail
  and why, never to whom.
- **History**: one sample every five minutes (processor, load, memory, swap, fullest disk,
  connected devices), kept 14 days: at most 4,032 rows. Charts get at most 300 points.

### 2.3 Alerts

Two pure functions in `crates/core/src/alerts.rs`. `evaluate` turns what was observed into
findings and says which kinds it could judge; `plan` compares them with the open alerts and
decides what to raise, escalate, recall or resolve.

| Alert (code) | Severity | Default threshold |
|---|---|---|
| `disk_full` (per filesystem) | warning, critical 5 points higher or from 97 % | 90 % |
| `memory_pressure` | warning | 92 % |
| `service_down`, `service_restarting`, `tak_process_down` | critical | 3 restarts in 15 minutes |
| `service_unhealthy` | warning | |
| `tak_unreachable`, `directory_unreachable` | critical | |
| `reconciliation_failing` | warning, critical after six times as long | 10 minutes |
| `event_not_started` / `event_not_ended` | warning / critical | 2 minutes late |
| `certificate_expiring`, `certificate_expired` | warning, then critical | 30 and 7 days |
| `client_certificates_expiring` (one alert, whatever their number) | minor, then warning | 30 and 7 days |
| `backup_failed`, `backup_stale` | warning | 36 hours |
| `security_updates`, `reboot_required` | minor | |
| `mail_failing` | warning | 3 failures in an hour, none delivered since |
| `clock_unsynchronized` | warning | |
| `agent_unreachable` | warning | |

- **No storm.** A problem must be seen on two looks in a row to become an alert, and be gone on
  two to be resolved. Everything one look raises, resolves or recalls goes in **one e-mail** per
  administrator; a single alert is named in the subject, several are counted.
- **One notification when raised, one when resolved, a reminder at most once a day** (a
  setting). An alert that gets worse (a certificate going from 30 to 7 days) is announced again.
- **What could not be read is not judged.** If the agent is away, a "disk full" alert stays open
  and silent rather than being resolved by ignorance.
- **In the recipient's language**, with numbers written the way that language writes them. The
  same sentences are shown by the dashboard; a test keeps the two in step.
- **Webhook**: the same transitions as a JSON `POST`, one per alert, with stable codes and one
  English sentence. The format is in the README. A "Send a test" button checks an address.
- **Team leaders** are told by e-mail, in their language, when a certificate shipped in one of
  their team's zips enters the 30-day period, and again the 7-day one: once each, never twice.

### 2.4 Connected devices

`GET /api/admin/devices`: every session TAK Server lists, matched with the access it uses, its
team, its callsign, its channels, the application it announced, the address it connects from.
`GET /api/teams/{id}/devices`: the same for the accesses of one team, without addresses, for its
leaders (and administrators). Members get 403; a leader of another team gets 404, as for
everything about a team that is not theirs.

TAK Server does not say since when a device is connected. The platform notes when it first saw
each session, so "since" is exact to one look; a device already connected when the platform
started is shown as "… or earlier". Sessions without any group, which TAK Server lists without
their account and keeps listing after they are gone (phase 1), are shown apart, to administrators.

### 2.5 The audit log

A table (`audit_log`): when, who (the account's address as it was), which action (a stable
code), which team (its name as it was), which object, a few named details, and the client's
address. 39 actions: every state-changing action of phases 1 and 2, administrator sign-ins, the
rescue command, the settings, and what the agent did, whether an administrator or the daily
schedule asked. Showing an access's credentials and downloading a zip are recorded too.

**No secret can go in by naming a new field**: details are a closed list of 17 named fields
(`mode`, `role`, `kind`, `form`, `member_cap`, `outcome`, `file`, …). A change of settings records
the names of the settings, never their values.

Shown to administrators with filters (period, team, actor, action) and paging; kept one year by
default, purged daily.

### 2.6 Backups and restore

`deploy/scripts/backup.sh` writes one archive: `slapcat` of the directory, `pg_dump` of TAK
Server's database and of the platform's, `deploy/data` (the certificate authority; logs left
out) and `deploy/.env`. The directory is exported first and the platform's database last, so the
restored platform never knows less than the restored directory. The archive is read back before
it is given its final name. With a passphrase it is encrypted by `openssl` (AES-256-CBC, key
derived with PBKDF2, 200,000 rounds). The newest N archives are kept (14); only files the script
named are ever counted or deleted.

The platform asks the agent for a backup once a day, from the hour set (03:00 UTC), and when an
administrator clicks "Back up now". Each run is recorded. On this stack an archive is 2.5 MB and
takes about one second.

`deploy/scripts/restore.sh` replaces everything from one archive: section 4.

Also run daily: the Let's Encrypt hand-over when it is enabled, the housekeeping of TAK Server's
logs, and the purge of the tables that grow.

**TAK Server's logs.** TAK Server already rolls its logs every day and keeps 90 compressed files
of each, with no limit on size. The agent now deletes rolled files older than 30 days (a
setting) and cuts any live log larger than 50 MB (the supervisor's own `takserver.log`, which
nothing rolled).

### 2.7 API

| Call | Does |
|---|---|
| `GET /api/admin/monitoring` (`?refresh=true`) | The latest look, open alerts, those resolved in 7 days |
| `GET /api/admin/monitoring/history?hours=` | Samples for the charts |
| `GET /api/admin/alerts/summary` | Number of open alerts, for the navigation |
| `GET /api/admin/devices`, `GET /api/teams/{id}/devices` | Connected devices |
| `POST /api/admin/services/{service}/restart` | Restarts one service of the stack |
| `POST /api/admin/backups/run` | Writes a backup now |
| `POST /api/admin/letsencrypt/sync` | Hands the Let's Encrypt certificate to TAK Server |
| `GET`, `PATCH /api/admin/settings`, `POST …/webhook-test` | Thresholds, alerts, backups, retention |
| `GET /api/admin/audit` | The audit log, filtered and paged |

Five new error codes, each translated: `agent_unavailable`, `agent_busy`, `service_not_allowed`,
`action_failed`, `webhook_not_set`.

Authorisation is in `policy::can` as before, with four new actions: `ViewTeamDevices`
(administrators and the team's leaders), `ViewAllDevices`, `ViewMonitoring` and `OperatePlatform`
(administrators).

### 2.8 Web UI

- A **Monitoring** entry in the navigation for administrators, with the number of open alerts.
- **Overview**: alerts first; the server with gauges; the services, each with its state, its
  image and a "Restart" button behind a confirmation that says what the restart does to users;
  TAK Server; certificates; the platform; charts over 24 hours or 7 days. Where the platform
  reports something it will not do itself (updates, images, a TAK Server upgrade), the command to
  run over SSH is shown with a copy button.
- **Connected devices**, **Backups** (archives, runs, "Back up now", how to restore), **Audit
  log** (filters, paging), **Settings**.
- On a **team's page**, for its leaders: "Connected devices".
- Dates, numbers, sizes, percentages and lists go through `Intl` in the UI's language.

## 3. What was verified, and how

### 3.1 Automated tests

| Suite | Command | Result |
|---|---|---|
| Rust unit tests | `scripts/test-unit.sh` | 213 pass (42 in `agent`, 107 in `core`, 64 in `server`); 94 before this phase |
| Web checks | `scripts/test-unit.sh` | translation check (843 texts in two languages, 116 server codes translated), lint, type check pass |
| Integration tests on the real stack | `scripts/test-integration.sh` | 26 pass: the 14 of phase 1, the 4 of phase 2 and 8 new ones, in about 7 minutes |

Results of the last run, **on the stack restored by the proof of section 4**, rebuilt with the
final code:

```
scripts/test-unit.sh
  test result: ok. 42 passed; 0 failed      (tak-manager-agent)
  test result: ok. 107 passed; 0 failed     (tak-manager-core)
  test result: ok. 64 passed; 0 failed      (tak-manager-server)
  i18n check passed: 2 languages (en, fr), 2 files each, 843 UI texts in en, 116 server codes translated.
  (then lint and type check)

scripts/test-integration.sh
  tests/e2e.rs         test result: ok. 14 passed; 0 failed; finished in 44.36s
  tests/events.rs      test result: ok.  4 passed; 0 failed; finished in 218.78s
  tests/monitoring.rs  test result: ok.  8 passed; 0 failed; finished in 139.69s
```

The monitoring tests were first run alone while they were being written: seven of the eight
passed at once; the eighth failed on the test itself (it compared text with the line ends e-mail
travels with) and showed one defect of the platform, the alert e-mail linking to the
administration page instead of the monitoring page. The `check:i18n` script now also reads the
server's codes (API errors, alert kinds and severities, audit actions and details) from the Rust
sources and fails when one has no text in a language; the server's own tests check the same.

New unit tests, by subject:

- **Alerts** (23 in `core`): each rule and its threshold; what is not judged when a source cannot
  be read; raising after two looks and not for a blip; one reminder a day over a simulated week
  (seven, not ten thousand); escalation; resolving after two looks; an alert whose source is
  unreadable staying as it is; everything failing at once giving one batch; thresholds brought
  within reason.
- **What a host reports** (`agent`), on output captured from real systems: `apt-get -s
  dist-upgrade` of `debian:12.0` (47 packages, 9 from the security archive) and of an
  `ubuntu:22.04` image of January 2023 (61, 48), both simulated by the apt of Debian 13 pointed at
  their package state, which is exactly what the agent does; an up-to-date system; a failed
  simulation read as "unknown", never as "up to date"; `/etc/os-release` of both; `/proc/meminfo`,
  `/proc/stat`, `/proc/loadavg`; `docker inspect`; `docker top` of the running TAK Server; the
  kernel's clock status; disk usage counted like `df`.
- **What the agent refuses**: a restart of containers of other projects, of itself, of a
  container name, of an identifier, of anything with shell characters, without even asking
  Docker; every call without the secret; any other route; a container of another project that
  Docker would list by mistake.
- **Certificates**: subject, issuer and expiry; whole days left; 30 and 7 days; the day of
  expiry.
- **Authorisation**: who sees connected devices and the monitoring, who may act.
- **Audit**: codes unique and translated; the detail fields are a closed list none of which can
  hold a secret; texts bounded and single-line.
- **Settings**: a change touches only what it names; unknown fields refused; bounds.
- **E-mails**: every alert kind has its sentence in every language, with no placeholder left
  and the same placeholders as the dashboard; one alert named in the subject, several counted;
  the decimal comma in French; failure texts scrubbed of addresses.
- **Others**: the webhook document and its delivery to a local listener; the agent client
  (secret sent, refusal kept, absent agent); restart loops counted over 15 minutes; daily jobs
  due once a day; log housekeeping on real files; image update status; registry challenges.

### 3.2 Integration tests (`crates/server/tests/monitoring.rs`)

Black-box like the others: the platform's HTTP API through Caddy, the mail catcher, real TAK
sessions, `docker compose` for stopping a service, and the agent called from inside its own
container.

| Test | Proves |
|---|---|
| `the_agent_reports_the_machine_and_this_stack_only` | Real figures for the machine; the eight services of the stack, running, and nothing else; which image is local, pulled by tag, pinned; TAK Server's three ports, version and five processes; the four certificates with plausible expiries; the report carries no secret of `deploy/.env`. **The agent refuses to restart every container of the other projects running on this Docker** (their real names), itself, a container name; has no route to run a command; refuses a wrong secret. The platform refuses the same before asking. Non-administrators get 403 on every monitoring call |
| `a_stopped_service_raises_an_alert_by_email_and_its_resolution` | The reverse proxy is stopped. **20 seconds later** each administrator has one e-mail, in French for one ("[TAK Manager] Alerte : Le service caddy ne tourne pas (état : exited).") and in English for the other, linking to the monitoring page. Nothing more is sent while it lasts. Started again: one "Résolu" e-mail each; the alert is in the history as resolved |
| `thresholds_are_settings_and_a_change_raises_and_resolves_an_alert` | Invalid settings refused; lowering the disk threshold to 50 % raises `disk_full` for Docker's disk with its real figure, and an e-mail; back to 90 % resolves it; the change is in the audit log by name; a history sample exists |
| `leaders_are_told_in_their_language_when_a_zip_certificate_nears_its_end` | Nobody is told a year ahead; when the warning period covers the certificate, the team's leader receives one e-mail in French naming the access and what to do; once only; members are not written to |
| `connected_devices_are_seen_by_administrators_and_by_the_leaders_of_the_team` | Two devices on one shared identity and one of another team, really connected. The administrator sees all three with team, access, channels and address. **The leader sees exactly their team's two**, without address. A member gets 403, a leader of another team 404. A device that leaves leaves the list |
| `the_audit_log_records_who_did_what_and_never_a_secret` | A path of 18 actions on one team is recorded in order with actor, object, details and client address; the administrator's sign-in and the rescue command are there; **neither the access's passwords nor the certificate store's password nor any key of `deploy/.env` appears anywhere in the log**; filters by team, actor, action and period; paging; the deleted team still offered by the filter |
| `a_backup_is_written_on_request_listed_recorded_and_encrypted_when_asked` | "Back up now" writes an archive holding the five parts (listed with `tar`), shown by the dashboard and recorded in the audit log. With a passphrase the next one is encrypted, **opens with that passphrase and no other** (`openssl` on the Mac reading what OpenSSL in the agent wrote), and the passphrase is in no answer and no audit entry. The test deletes its two archives |
| `an_administrator_restarts_a_service_of_this_stack_from_the_api` | The mail catcher is restarted through the API (its container's start time changes), the restart is in the audit log, and e-mail works again. The Let's Encrypt hand-over, not set up here, answers "failed" instead of pretending |

### 3.3 Other checks on the running stack

- **The agent's isolation.** From `caddy`: the name `agent` does not resolve. From the agent: no
  route to the Internet. `docker network inspect`: the network is internal and holds two
  containers.
- **The agent as a Compose service with every capability dropped** but two (reading and writing
  files of other users): report, backup and restart all work.
- **Registries**: asked once at the first look; answers stored; the dashboard showed "Newer image
  available" for Caddy and PostgreSQL and "Pinned version" for OpenLDAP.
- **The certificate conversion of the Let's Encrypt hand-over**, in the real `takserver`
  container with a self-signed ECDSA P-256 certificate (Caddy's default key type): key and
  certificate passed on standard input, PKCS#12, then a JKS holding one private-key entry. This
  answers half of an open question of phase 1 (whether such a key converts); whether TAK Server
  then serves it is still untested.
- **`install.sh` on an installation made before this phase.** Run on the phase 2 stack, it built
  the agent, applied the migration, and the first look ran the daily jobs (a backup, the purges)
  by itself. Its branch that adds the new settings was then run for real on a `deploy/.env`
  stripped of them: it added `AGENT_TOKEN` and the monitoring interval, recreated `app` and
  `agent` with the new secret, and ended with "host agent: ok".
- **`tak-manager status`** now says whether the agent answers.
- **`deploy/scripts/backup.sh` run by hand on the host** (here the Mac, with its own `bash`, `tar`
  and no agent involved) writes the same archive as when the agent runs it.

### 3.4 Walk-through of the dashboard

In the app's browser pane, signed in as an administrator: once on the build that first passed
the tests, where the defects below were found, and again on the final build, in both languages,
where each fix was seen.

- **English.** The navigation shows "Monitoring" with a red "2". Overview: the two open alerts
  with their severity and age; the server (14 cores, memory and three filesystems with gauges,
  the 98.6 % one in red, "not synchronised", "Unknown: this server does not run Debian or
  Ubuntu…"); the eight services with state, health, image and update status, "Newer image
  available" on two of them; TAK Server's ports, processes and version; the four certificates and
  "101 client certificates in use"; the platform; the charts. Connected devices: none, and three
  sessions without a channel explained. Backups: schedule shown in the browser's time zone,
  archives, runs, restore instructions, a warning that archives are not encrypted. Audit log:
  filters and entries. Settings: every field with its default.
- **Actions.** Recording "5.9-release-12" as the latest version seen: saved as `5.9-RELEASE-12`,
  and the overview then says "TAK Server 5.9-RELEASE-12 is available on tak.gov" (the setting was
  cleared afterwards). "Restart" on the mail catcher: confirmation, then "mailpit was restarted".
  "Back up now": a confirmation that says the archive will not be encrypted, then "Backup
  written: tak-manager-backup-20261119T183448Z.tar.gz" and the archive at the top of the list.
- **French.** The language switch turns the whole page: "Supervision", "Vue d'ensemble",
  "Appareils connectés", "Sauvegardes", "Journal d'audit", "Réglages"; "2 alertes en cours";
  numbers and dates in the French way ("9,9 Go libres sur 15,7 Go", "16 nov. 2036", "il y a 9
  minutes"); the services' states, the certificates, the alert sentences. A team's page shows
  "0 appareil connecté" with its explanation.

It found ten defects or rough edges, all fixed and checked again:

1. "since 8 minutes ago" / "depuis il y a 9 minutes": now "raised 8 minutes ago", "started …".
2. No full stop between two sentences of the alerts card.
3. Alert figures with a decimal point in French ("98.6 %"), on the page and in e-mails.
4. After saving settings, a restart or a backup, the page showed the old state for up to 15
   seconds: it now asks for a fresh look.
5. The 24-hour chart had the same time of day at both ends of its axis: dates are shown.
6. "3 connected devices" counted sessions without a channel, which are not devices.
7. The mail catcher's restart confirmation did not say its messages are lost.
8. Long e-mail addresses broke in the middle of words in the audit log.
9. French texts said "responsables" where the rest of the UI says "chefs d'équipe".
10. The team's device list said only leaders see it, to an administrator who was reading it.

Not clicked: "Send a test" for the webhook (no address to send to; its delivery is unit-tested
against a local listener), "Hand over now" (not shown without Let's Encrypt), and "Restart" on
the services other than the mail catcher.

## 4. The restore, proven for real

`scripts/prove-restore.sh --yes` replays all of it. What it does, and what happened.

**Step 1 — a stack with something to lose** (`before_the_backup`). Three teams. *Alpha*, a
shared identity whose zip is kept as a phone would keep it, and a guest access with its zip.
*Bravo*, individual accounts: the leader's zip, then regenerated, so the first certificate is
revoked and the second is in use; both zips kept. *Charlie*, an access created, its zip kept,
then removed. Devices connect with these zips to check the starting point: Alpha sees its guest,
Bravo does not see Alpha, the two dead certificates are refused. Then the backup, asked through
the API as an administrator does. **After the backup**, Alpha's guest access is regenerated (its
kept zip is now refused) and a fourth team is created: the stack is ahead of its backup.

**Step 2 — a copy outside the repository** of `deploy/.env` and `deploy/data`, to put back if
the restore fails.

**Step 3 — destruction** with `deploy/scripts/clean.sh`: the four Docker volumes of the project
(`postgres-data`, `ldap-config`, `ldap-data`, `tak-db-data`), its containers, `deploy/.env` and
`deploy/data`. The script checks that none is left. Only objects of the Compose project
`tak-manager` are touched; the images are kept.

**Step 4 — restore** with `deploy/scripts/restore.sh` on the archive: reads and checks it; puts
`deploy/.env` and `deploy/data` back; runs `install.sh --no-start` (images, configuration,
ownership; the certificate authority found in place is kept); starts the two databases and the
directory alone; loads the platform's database (`pg_restore --clean`), TAK Server's (dropped and
created again from the dump), and the directory (`slapadd`, offline); then runs `install.sh`,
which starts everything and checks that the platform reaches the directory and TAK Server.

**Step 5 — verification** (`after_the_restore`), with the zips kept in step 1:

| Check | Result |
|---|---|
| The zip handed out before the backup still connects, and the device exchanges positions with another device of its team; the directory gives the account its team's group and no other | yes |
| Another team connects with its own old zip and neither sees the first nor is seen | yes |
| The certificate revoked before the backup (regeneration) is refused; so is the one of the access removed before the backup | yes |
| The platform hands out the very same zip certificate as before (same fingerprint): the stored certificate and `APP_SECRET` both came back | yes |
| The leader signs in by e-mail code, finds the team, its members and its two accesses | yes |
| The team created after the backup does not exist | yes |
| The guest access regenerated after the backup is back as it was: **its old zip connects again** | yes — this is the limit of any restore, now observed |
| Reconciliation after the restore: both backends reachable, no error, nothing corrected | yes |
| The audit log of before the backup is there | yes |

### What happened

**First attempt: the restore failed at the directory.**

```
== Loading the directory
slapadd: line 1: database #1 (dc=takmanager,dc=internal) not configured to hold "dc=nodomain";
         no database configured for that naming context
ERROR: could not load the directory
```

The directory's image keeps, in the same database as the platform's tree, a leftover entry
`dc=nodomain` from its packaging. `slapcat -n 1` exported it with everything else, and `slapadd`
refuses to load an entry outside the database's own suffix. The backup script had also checked
that its archive could be read back, which it could: only a real restore shows this. The proof
script put `deploy/.env` and `deploy/data` back from its copy, as designed.

Fixed on both sides: the export is limited to the platform's tree (`slapcat -s`), and the
restore loads only the entries of that tree, so an archive written before the fix restores too.

**Second attempt, from the very archive that had failed**, with the fixed script: restored in
53 seconds (images already built), and the nine checks above passed.

```
== Loading the platform's database
   332 accounts, 141 teams, 119 TAK accesses
== Loading TAK Server's database
   148 certificates known to TAK Server, 44 of them revoked
== Loading the directory
   225 entries loaded
…
   database:   ok (332 accounts)
   directory:  ok (119 accounts, 98 groups)
   TAK Server: ok (version 5.8-RELEASE-103-HEAD, 104 active client certificates)
   host agent: ok (8 services of this stack, 4 backup archives)
```

**Third run: the whole proof again, uninterrupted**, with the final code, a new backup and a
new destruction.

```
#### 1/5 Preparing teams, accesses and zips, then asking the platform for a backup
test before_the_backup ... ok
   archive: deploy/backups/tak-manager-backup-20261119T182411Z.tar.gz (2577477 bytes)
   zips kept outside the stack: 5
#### 2/5 Keeping a copy of deploy/.env and deploy/data outside the repository
#### 3/5 Destroying this project's volumes, deploy/.env and deploy/data
   volumes before: tak-manager_ldap-config tak-manager_ldap-data tak-manager_postgres-data tak-manager_tak-db-data
   volumes after: none; containers: none; deploy/.env and deploy/data: gone
#### 4/5 Restoring from the archive
   338 accounts, 144 teams, 122 TAK accesses
   153 certificates known to TAK Server, 46 of them revoked
   230 entries loaded
   database:   ok (338 accounts)
   directory:  ok (122 accounts, 100 groups)
   TAK Server: ok (version 5.8-RELEASE-103-HEAD, 107 active client certificates)
   host agent: ok (8 services of this stack, 5 backup archives)
   restored in 53 s
#### 5/5 Verifying with the zips handed out before the backup
PROOF old zip of e2e-przwmh-shared connects and exchanges positions inside the team
PROOF another team connects and stays isolated
PROOF certificates revoked before the backup are still refused
PROOF the platform hands out the same zip certificate as before the backup
PROOF what was done after the backup is not there: the restore is the moment of the backup
PROOF reconciliation after the restore: no error, nothing corrected; 122 accounts in the directory
PROOF the audit log of before the backup is there (9 entries for the team)
test after_the_restore ... ok
#### The restore is proven
```

The 53 seconds are with the four images already built; on a new machine the TAK Server images
are built first, which takes several minutes. TAK Server's database (34 MB here) and the
platform's (11 MB) are small; a larger TAK Server database makes the dump and its loading longer.

**Then the integration suite on the restored stack**: section 3.1.

About the phase 1 warning (a platform database older than the directory makes a manual pass
delete what it does not know): a restore never restores one part alone, the export order makes
the platform's database the newest of the three, and after the restore the reconciliation
corrected nothing. The automatic guard of phase 1 is still there for the case the script cannot
prevent, someone loading one dump by hand.

## 5. What was not verified

| Not verified | Why | What it would take |
|---|---|---|
| **A real Debian or Ubuntu amd64 host.** That the agent reads `/etc`, `/var/lib/apt`, `/var/lib/dpkg` and `/run` of a real server; file ownership under `deploy/` as root | The only Linux available is OrbStack's VM, which is not Debian | The first production install; the checklist of section 9 |
| **Pending updates and "restart required" on a host** | Same. The parsing is tested on real `apt` output, and the method (the agent's apt reading another system's package state) was run for real against the state of Debian 12 and Ubuntu 22.04 containers; the flag file is tested on a temporary folder | On the VPS: compare the dashboard with `apt list --upgradable` and `ls /run/reboot-required` |
| **A host whose package lists are never refreshed** | Depends on the server's own configuration | The dashboard says how old the lists are, and that the count may be out of date after a week |
| **Clock synchronisation on a real host** | The VM's kernel says "not synchronised" (it is set by its hypervisor, not by NTP), which is a true reading and the only one seen | On the VPS: compare with `timedatectl` |
| **Let's Encrypt**: the website's certificate in the dashboard, the hand-over to port 8446 by the agent, TAK Server serving the result | No domain | A production install with `--tak-enroll-letsencrypt`. The conversion to a keystore was run for real (section 3.3); nothing after it |
| **Real SMTP**, and therefore the `mail_failing` alert against a real refusal | No credentials | Brevo on the VPS. The rule is unit-tested; the counting was seen working for successes only |
| **A TAK Server upgrade** (`install.sh --upgrade-tak`) | Only one ZIP exists | A second ZIP. The option is written from the steps the install and the restore already run, and stops before changing anything if the database image's PostgreSQL major version differs |
| **Restoring on another machine** | One machine | The procedure of the README; the restore proof ran on the same machine, after destroying the volumes and the generated files but not the images |
| **The webhook against a real service** (Slack, ntfy…) | No address | The settings' "Send a test". Delivery is tested against a local listener |
| **Alerts over days**: the daily reminder, a certificate crossing 30 then 7 days, a backup becoming too old | Would take days | The rules are unit-tested with simulated time, including a week of reminders |
| **A restart loop, an unhealthy service, a TAK Server process dying, memory pressure, a full disk** as real incidents | Not provoked: they disturb the machine or the other projects on it | Unit-tested rules. `service_down` and `disk_full` were seen for real |
| **Restarting `takserver`, `postgres`, `openldap`, `app` or `caddy` from the UI** | Only the mail catcher was restarted through the API; the same call restarts the others. For `app` and `caddy` the answer is sent before the restart, a path that never ran | Click them on the VPS at a quiet time |
| **Log housekeeping on months of TAK Server logs** | The logs here are a day old | Tested on temporary files; on the VPS, look at `deploy/data/tak/logs` after a month |
| **Scale**: a dashboard for 40 devices, an audit log of a year | Not in scope | The history is bounded by design; the audit log is indexed and paged |
| Phones, production mode as a whole, `ldaps://` | Unchanged from phases 1 and 2 | See those reports |

## 6. Findings

### 6.1 A TLS probe on TAK Server's client port is logged as an error

To read TAK Server's certificate the first version opened a TLS connection on 8089 without a
client certificate, at every look. `takserver-messaging.log` then gained one line per look:

```
ERROR c.b.m.n.n.h.NioNettyHandlerBase - NioNettyServerHandler error. Cause: …OpenSslHandshakeException:
error:100000c0:SSL routines:OPENSSL_internal:PEER_DID_NOT_RETURN_A_CERTIFICATE.
```

That is 1,440 lines a day in production, written by the monitoring into the log it is meant to
keep tidy. Port 8089 is now only asked whether it accepts connections, which TAK Server does not
log (its own healthcheck does the same). The certificate is read on 8443, whose connector logs
nothing for the same probe, and on 8446, which asks for no client certificate.

### 6.2 The directory's database holds an entry that is not the platform's

Section 4. `slapcat -n 1` of the `vegardit/openldap` image exports `dc=nodomain` along with the
configured tree, and that entry cannot be loaded back. Exports are limited with `-s`.

### 6.3 A running container can name an image Docker no longer has

After `install.sh` rebuilt an identical agent image, the running agent container still referred
to its image by an identifier that no longer existed (the image store here is containerd's),
and `docker image inspect` of all the stack's images at once failed for that one. The agent lost
the registry digests of **every** image, and the dashboard said "Not compared yet" for all of
them. The agent now keeps what Docker did find and looks the missing one up by name.

### 6.4 Docker gives locally built images a digest

`RepoDigests` is not empty for an image built on this machine with the containerd image store
(`tak-manager/app@sha256:…`), so "has a registry digest" cannot tell a pulled image from a built
one. Images are classified by name: `tak-manager/…` is local.

### 6.5 TAK Server's sessions

`/Marti/api/subscriptions/all` gives, for each session, the callsign, the client UID, the
application and its version when the client announced them, the address and port it connects
from, the time of its last message, and its groups; **not** when it connected. A session without
any group is listed without `username`, and stays listed long after its connection is gone (three
such entries, an hour old, were on the test stack throughout).

### 6.6 TAK Server already rolls its logs

Phase 1 reported that TAK Server's logs are "not rotated by this project". TAK Server's own
logging configuration rolls each log daily into a compressed file and keeps 90 of them, without
a size limit. What was missing is a shorter retention, and the supervisor's `takserver.log`,
which is appended to at every start and never rolled.

### 6.7 Two alerts are open on the development machine

`disk_full` for the filesystem that holds the repository (the Mac's own disk, 98.6 % used) and
`clock_unsynchronized` (the VM's kernel has the "unsynchronised" flag set, although its clock is
right: the hypervisor sets it without telling the kernel). Both are true readings. On a VPS with
an NTP client the second one will not appear; if it does, the clock really is not disciplined.

## 7. Deviations from the plan and from the brief

| Subject | Plan or brief | What was done, and why |
|---|---|---|
| The agent's view of the host | "The host mounted read-only" | Three directories read-only (`/etc`, `/var/lib`, `/run`) instead of `/`. They are all the report needs; and on this Mac a recursive mount of the VM's `/` would have put the Mac's own files inside the container, which the brief forbids |
| Who asks the registries | "Whether a newer image exists" was listed under the agent's report | The platform asks, with the image names and digests the agent reports. The agent has no route out, which is worth more than the symmetry |
| The agent's network | "Reachable by the `app` service only" | A Docker network declared `internal`, joined by `app` and `agent` only, plus the shared secret |
| The Let's Encrypt hand-over | "Move `sync-letsencrypt.sh` from cron to the agent" | Done. The script was rewritten to work both on the host (the install still calls it once) and inside the agent: the conversion now runs in the `takserver` container and takes its secrets on standard input. The platform asks for it daily; the cron entry is removed by `install.sh` if a previous install left one |
| Off-site copies | "The folder and a documented hook" | The hook is an example script for the host's cron (`offsite-copy.example.sh`), not something the agent runs: the agent has no route out, on purpose. Listed as a limitation |
| Alert reminders | "At most once a day" | The period is a setting (24 hours by default) |
| Alert kinds | The list of the brief | All of them, plus `agent_unreachable`, `tak_process_down` and `certificate_expired` (so that "expires in -2 days" is never written) |
| E-mails per alert | "One notification when an alert is raised" | One e-mail per look that has something to say, listing everything raised, resolved and recalled by that look. With one alert it is the same thing; with ten it is one e-mail instead of ten |
| Where the dashboard lives | "The administrator's dashboard" in `admin.tsx` | Its own page, `/monitoring`, with five tabs; the administration page already had four |
| Since when a device is connected | "Since when" | Since the platform first saw the session: TAK Server does not give the time of connection (section 6.5) |
| Backup encryption | "Optionally encrypted with a passphrase" | `openssl enc` (AES-256-CBC, PBKDF2). It authenticates nothing: a damaged archive is caught by gzip's checksum, not by the cipher. Chosen because `openssl` is on every Debian and Ubuntu server and on the Mac, so a restore needs nothing else |
| Thresholds | "Configurable with sensible defaults" | In the database, changed from the UI, each clamped to a range that still makes sense |
| A fifth daily job | Not in the brief | The purges and the log housekeeping run with the backup, once a day, and say what they did |

## 8. Known limitations

- **Backups stay on the machine** until the administrator sets up a copy elsewhere. The
  dashboard and the README say so.
- **A restore un-revokes** what was regenerated or removed after the backup (section 4).
- **Archives are encrypted only if a passphrase is set**, and the dashboard warns when it is
  not. The passphrase is stored (encrypted with `APP_SECRET`) so the daily backup can use it; a
  copy must be kept elsewhere, since a restore asks for it.
- **If the platform is down, nothing is backed up and nothing alerts.** The daily jobs and the
  alerts live in the platform. An outside check of `https://…/api/health` is the answer to "who
  watches the watcher", and is not provided.
- **If e-mail is down, the alert about e-mail cannot arrive by e-mail.** The webhook and the
  dashboard still say it.
- **Pending updates are as fresh as the server's own package lists.** The agent never runs
  `apt update`.
- **"Restart required" relies on the flag file**, which Ubuntu always writes and Debian writes
  for most packages that need it. A kernel upgraded without the flag is not detected.
- **"A newer image exists" compares one tag.** A new minor version of Caddy or PostgreSQL under
  another tag is not looked for.
- **A newer TAK Server is known only if the administrator records it**, or drops its ZIP.
- **"Connected since" starts at the platform's start** for devices already connected then.
- **The agent can restart any service of this stack on the platform's request.** Whoever becomes
  an administrator of the platform can therefore restart TAK Server. They cannot do anything else
  on the host through it.
- **The agent holds the Docker socket**, which is root on the host for whoever controls the
  agent. That is why it has no route out, a closed API and a secret. Dropping its capabilities
  is tidiness, not a boundary.
- **The date fields of the audit log follow the browser**, like those of events (phase 2).
- **Alert e-mails go to every administrator**; there is no per-administrator choice.
- Everything listed in phases 1 and 2 still holds, except "there is no audit log" and "which
  devices are connected".

## 9. Left for the user

1. **The first production install**, on the amd64 Debian or Ubuntu VPS. A checklist:
   1. `sudo ./install.sh --domain … --admin-email … --smtp-…` (add `--tak-enroll-letsencrypt`
      for QR codes). It must end with "host agent: ok".
   2. Sign in, open **Monitoring**. The banner about the agent must be absent, and the server
      card must name the distribution.
   3. Compare "System updates" with `apt list --upgradable 2>/dev/null | wc -l` on the server,
      and "Restart required" with `ls /run/reboot-required`. If the page says the package lists
      are old, enable the server's daily `apt update` (`sudo apt install unattended-upgrades`).
   4. "Clock" must say "synchronised"; otherwise `timedatectl` and an NTP client.
   5. **Settings**: set a backup passphrase and write it down elsewhere; check the hour of the
      backup; set a webhook if you use one and press "Send a test".
   6. **Backups**: "Back up now", then copy the archive off the server and set up the daily copy
      (`deploy/scripts/offsite-copy.example.sh`).
   7. Stop a harmless service for a minute (`docker compose stop caddy`, then `start`): one alert
      e-mail must arrive through Brevo, then its resolution. That is also the first real test of
      SMTP for alerts.
   8. With `--tak-enroll-letsencrypt`: the platform card must say the enrollment port presents
      the Let's Encrypt certificate; the certificates card must list "Website (Let's Encrypt)"
      and "Enrollment port (8446)". Then a QR enrollment from a phone.
   9. Once, on a quiet day: restore the latest archive on a second machine, or on the same one
      (`deploy/scripts/restore.sh`), and connect a phone with a zip handed out before.
2. **Decide where the archives go** off the machine.
3. **When a new TAK Server is out**: the first run of `install.sh --upgrade-tak`, which is the
   first test of that option. Take a snapshot of the VPS before.
4. An **outside check** of the platform's address, if you want to be told when the whole server
   is down.
5. What phases 0 to 2 left: the phone checks, and whether LDAP should be encrypted inside the
   Docker network.

## 10. Repository hygiene and state of the machine

Nothing generated is versioned. `deploy/backups/` is git-ignored, like `deploy/.env` and
`deploy/data/`. The list of files to be committed was checked for the TAK Server ZIP and anything
extracted from it, `.env` files, keys, certificates, keystores and backup archives (`*.zip`,
`*.p12`, `*.jks`, `*.pem`, `*.key`, `*.tar.gz`, `*.enc`, `deploy/data/`, `deploy/backups/`): none.
The fixtures under `crates/agent/tests/fixtures/` are output of `apt-get` and two
`/etc/os-release` files of public Debian and Ubuntu images.

What the tests and the proof left behind was removed: the archives written by the tests (they
delete their own), the five written by hand and by the proof runs, the encrypted test archive,
the folder where a restore had put the previous `deploy/.env` and `deploy/data` aside, and the
copies of those two kept outside the repository during the proof. One archive is left in
`deploy/backups/`, the last one, written from the UI: a backup of the stack as it is at the end
of this phase, not encrypted.

Other projects share this Docker. No container, image or volume outside the Compose project
`tak-manager` was stopped, restarted, removed or pruned; the six that were running
(`mapping-db-1`, `silicate-db`, `opsan-db`, `opsan-s3`, `mission-organizer-db`,
`mission-organizer-s3`) still are. Two throwaway images were pulled for the
`apt` captures (`debian:12.0-slim`, `ubuntu:jammy-20230126`) and removed afterwards;
`debian:trixie-slim`, the agent's base image, was pulled and stays.

State of the development machine at the end of the phase: the containers are stopped
(`docker compose down`); the Docker volumes, `deploy/.env`, `deploy/data/` and `deploy/backups/`
are kept, so `cd deploy && docker compose up -d` brings back the stack as tested. That stack is
the one **restored from a backup** during the proof, then used by the full test suite. Images:
`tak-manager/app` and the new `tak-manager/agent` were built; `tak-manager/takserver` and
`tak-manager/takserver-db` were not rebuilt. Disk: 13 GB free at the end, 16 GB at the start.
