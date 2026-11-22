# TeamTAK — specification and roadmap

Reference document for the project. Each phase is developed in a separate session that starts from this
file, then from the previous phase's report (`docs/phase-N-report.md`). If a decision changes, this file
is updated in the same session.

## 1. Goal

A self-service web platform, centred on teams, to manage a brand-new TAK Server (independent of the
.deb TAK Server already in production elsewhere). Existing tools (infra-TAK, FastTAK) are centred on
the administrator; this one is centred on teams: a user creates a team, invites members, chooses how
they connect to TAK Server, and can pool teams together for the duration of an event.

Sizing: about a dozen teams, ~10 users per team at peak, 30 to 40 simultaneous TAK clients. No
federation for now.

## 2. Software choices (settled)

| Component | Choice |
|---|---|
| TAK Server | 5.8, official "hardened" Docker ZIP from tak.gov (`takserver-docker-hardened-5.8-RELEASE-103.zip` at the repo root, not versioned) |
| API and host agent | Rust (axum, tokio, sqlx) |
| Web UI | React + TypeScript + Vite, Tailwind, shadcn/ui; static files served by the Rust binary |
| Internationalisation | react-i18next; English by default, French available |
| Platform database | PostgreSQL |
| TAK accounts and groups | OpenLDAP (directory read by TAK Server) |
| HTTPS / reverse proxy | Caddy + Let's Encrypt |
| E-mail | Brevo (SMTP), SPF and DKIM on the domain; SMTP settings come from the environment, and local mode uses a mail-catcher container |
| Deployment | Docker Compose on a new VPS, 8 GB RAM minimum |
| Monitoring | Home-made Rust agent, no Prometheus or Grafana |

Rejected, do not re-propose: Go, Next.js, LLDAP, Authentik, Google login, uncontrolled sign-up,
mandatory per-device certificates.

## 3. Architecture

Docker Compose services: `caddy`, `app` (Rust API + static UI), `postgres` (platform), `openldap`,
`takserver`, `takserver-db`, `agent` (host agent), and `mediamtx` (video server) when `VIDEO=on`.
Local mode adds `mailpit` (mail catcher).

- **Source of truth**: the platform's PostgreSQL database. An idempotent reconciliation loop compares
  the desired state with the actual state of the directory (and of TAK Server for certificates) and
  fixes any drift. Detected drift is visible in the admin area.
- **Groups**: written to OpenLDAP by the platform; TAK Server re-reads them by itself (`updateinterval`
  on the `<ldap>` element) and pushes them to already-connected clients. No group is ever baked into a
  zip. After each write the platform calls `GET /Marti/api/groups/update/{username}`: the change then
  applies in under a second and `updateinterval` is only a safety net. The group of a cross-team
  event is handled the same way: it exists only between the event's start and its end.
- **Certificates**: issued and revoked by TAK Server (enrollment on 8446, TAK Server's own CA), on the
  platform's request. The platform does not hold the CA key. For a zip that carries a client
  certificate, the platform submits the CSR itself with the LDAP account concerned, then packs key and
  certificate into a `.p12`. Revocation: `DELETE /Marti/api/certadmin/cert/revoke/{ids}`.
- **Platform access to the TAK Server API** (8443): a client certificate declared administrator with
  `UserManager.jar certmod -A`; the resulting `UserAuthenticationFile.xml` must live in a volume. The
  `adminGroup` attribute of `<ldap>` does not grant that role.
- **Directory and TAK Server access are isolated** behind a module (trait) so the backend can change
  without touching the rest.
- **Host agent**: reports the VPS state and runs privileged actions from an allowlist; the web
  application has no access to the Docker socket.
- **Exposed ports**: 443 (web; 80 as well, for the ACME challenge and the redirect), 8089 (TAK
  clients, TLS), 8443 (TAK Server's API, for TAK clients, mutual TLS) and 8446 (TAK enrollment).
  Phase 1 had kept TAK Server's 8443 unpublished; real devices then showed no channel, because
  ATAK and iTAK read their list of groups there, on 8443 whatever port they stream on: it is
  published as it is since 2026-11-20. Caddy publishes 8444 of the machine (`TAK_ADMIN_PORT`) for
  TAK Server's own administration pages: only for the platform's administrators, through their
  TeamTAK session, with a certificate Caddy alone holds (decided on 2026-11-20; on 8443 until the
  devices needed it).

TAK Server settings found in the official 5.8 guide (`tak/docs/TAK_Server_Configuration_Guide.pdf`
inside the ZIP), to use in `CoreConfig.xml`, `<auth>` element:

- `x509groups="true"`, `x509addAnonymous="false"`: groups looked up from the certificate CN, no
  `__ANON__` group (teams stay isolated).
- `x509useGroupCache="true"`: enables ATAK channels.
- `x509useGroupCacheDefaultActive` and `x509useGroupCacheDefaultUpdatesActive` set to `true`: channels,
  including those added after the first login (events), are active by default.
- `x509useGroupCacheRequiresExtKeyUsage` (default `true`, keep it): channels can only be toggled with
  certificates issued through TAK enrollment (extended key usage `challengePassword`). Every platform
  certificate goes through enrollment, including those shipped inside zips.
- `x509checkRevocation="true"`: immediate revocation of enrollment certificates.
- `<ldap>` element: `style="DS"`, `updateinterval`, `serviceAccountDN`, `serviceAccountCredential`,
  `groupObjectClass`, `userObjectClass`, `groupBaseRDN`, `userBaseRDN`, `groupprefix`.

Established in phase 0 (details and evidence in `docs/phase-0-report.md`, reference configuration in
`spike/`):

- `updateinterval` is in **seconds** (the official guide wrongly says milliseconds).
- With `style="DS"`, TAK Server reads the `memberOf` attribute of the entry
  `uid={username},ou=People,…`: OpenLDAP needs the memberOf overlay, set to `groupOfNames` / `member`.
  The LDAP URL carries no base DN; `userstring`, `userBaseRDN` and `groupBaseRDN` are full DNs.
- Directory layout: `inetOrgPerson` users under `ou=People`, `groupOfNames` groups under `ou=Group`; the
  TAK group name is the group's `cn`. A `groupOfNames` cannot be empty: a group exists in the directory
  only while it has at least one member.
- OpenLDAP image: `vegardit/openldap` (2.6, rebuilt weekly, amd64 and arm64), pinned by digest. Two
  service accounts: read-only for TAK Server, read-write for the platform.
- A connected user removed from their **last** group keeps the old groups for as long as the session
  stays open. The platform must then cut the session
  (`DELETE /Marti/api/subscriptions/delete/{subscriptionUid}`) or revoke the certificate. Deleting the
  LDAP account without revoking the certificate does not cut an open session either.
- CA: an intermediate CA (`makeCert.sh ca`) signs enrollment certificates
  (`<certificateSigning CA="TAKServer">`); the truststore handed to clients holds root and intermediate.
- `TAKIgniteConfig.xml` must exist before the TAK Server container first starts (otherwise one of its
  processes dies on first launch), and the container does not supervise its five Java processes:
  monitoring relies on the healthcheck and on `/actuator/health`.
- The database image build downloads packages over plain HTTP from `dl.rockylinux.org`; the install
  script must carry the HTTPS workaround used in the spike.
- Stated by the official guide (appendix E, "Quick Connect"), not tested: enrolling from a QR code, with
  no truststore preloaded on the phone, requires the 8446 connector to present a Let's Encrypt (or
  DigiCert) certificate. Port 8446 is served by TAK Server, not by Caddy, so that certificate must also
  be provisioned inside TAK Server and reloaded at each renewal. A zip carries its own truststore and
  does not depend on this.

Decided or established in phase 1 (details and evidence in `docs/phase-1-report.md`):

- **Certificate validity**: client certificates last 365 days, configurable at install
  (`CERT_VALIDITY_DAYS`). The platform records the expiry of every certificate it knows in its own
  database (`tak_certificates.expires_at`) for the monitoring phase.
- **Intermediate CA**: valid 10 years. The ZIP's `makeCert.sh` hard-codes 730 days for every
  certificate, CA included, which would end every client certificate after two years; the install
  runs a patched copy of the script inside a throwaway container. TAK Server's own certificate and
  the platform's API certificate keep 730 days and must be renewed (phase 3 alerts on them).
- **No LDAP lockout** on TAK accounts: passwords are 24 random characters generated by the platform,
  and a lockout on the exposed port 8446 would let anyone lock a known username. The rest of the
  password policy is kept.
- **LDAP stays unencrypted** inside the Docker network for now (known limitation).
- **Port 8446 and QR codes**: optional at install (`--tak-enroll-letsencrypt`): the Let's Encrypt
  certificate obtained by Caddy is converted to a JKS, the 8446 connector points at it, and TAK
  Server is restarted at each renewal (daily cron). Without it, 8446 uses the private CA and only
  zips work. Implemented, **not tested** (no domain).
- **Zip layout**: the one of the official `make-connection-package.sh`, behind a single function
  (`build_connection_zip`) so an iTAK variant can be added after the phone tests.
- **`certadmin/cert/active` is not "not revoked"**: on TAK Server 5.8 it is the latest certificate
  of each user and device. It keeps listing a revoked certificate, and hides an older one that a
  device replaced by enrolling again, although that older one still connects. The platform reads the
  full list (`GET /Marti/api/certadmin/cert`) and filters it itself.
- **Open sessions and groups**: TAK Server updates the groups of an open session only when the
  session has at least one group before and after. It never removes the last group (phase 0) and
  never gives a first group to a session opened without any (phase 1). In both cases the platform
  cuts the session and the client reconnects. A session without any group is listed by TAK Server
  without its username.
- **TAK Server's database** creates its tables at startup with a tool that connects to the host name
  `tak-database`; the `takserver-db` service carries that network alias.
- **Supervision**: the `takserver` container runs our entrypoint, which starts TAK Server as the
  image does and exits when one of its five Java processes is gone, so the restart policy brings it
  back. The administrator role of the platform's certificate survives (bind-mounted
  `UserAuthenticationFile.xml`).
- **Usernames** of TAK accesses are derived from the team (`<team>-shared`, `<team>-<member>`,
  `<team>-x-<label>`) and are never reused once an access is removed.

Decided or established in phase 2 (details and evidence in `docs/phase-2-report.md`):

- **An event has no stored state.** It has a start, an end and a cancellation date; whether it is
  upcoming, running, ended or cancelled follows from the clock. Every reconciliation pass computes
  the groups of each account for the instant it runs at: its team's group, plus the group of each
  running event its team takes part in. Starting and ending an event is therefore only a matter of
  running a pass at the right moment, and a pass run late gives the right result.
- **Scheduler.** Besides the periodic pass, the platform sleeps until the next start or end of an
  event and runs a pass then (origin `schedule` in the journal). Measured on the development
  machine: about 0.2 s between the scheduled time and the change reaching connected clients.
- **Drift and events.** A membership that a pass adds or removes for a group whose event started
  or ended since the last complete pass is the schedule at work, not drift, whatever triggered the
  pass (including the first pass after a restart). `reconcile_status.desired_at` holds the time
  the last complete pass computed its desired state for.
- **TAK Server applies an added or removed event group to open sessions**, without reconnection,
  as phase 0 predicted: the account keeps its team group throughout. This now holds for the
  platform as a whole and is asserted by the integration tests.
- **Group-less sessions are cut less often.** A pass that only moved accounts in or out of
  additional groups (an event starting or ending) no longer cuts the sessions TAK Server lists
  without a username; they are still cut when an account receives its first group.
- **If the platform is down when an event ends, its group stays** until the platform is back:
  nothing else writes to the directory. The first pass after the restart removes it.
- **Dates in e-mails are in UTC**, written in the recipient's language: the server does not know
  where the reader is. The web UI shows and reads dates in the browser's time zone.

Decided or established in phase 3 (details and evidence in `docs/phase-3-report.md`):

- **The host agent is a Compose service**, not a systemd unit: the install stays "Docker Compose
  only". It holds the Docker socket and three host directories read-only (`/etc`, `/var/lib`,
  `/run`), not the whole host: that is all its report needs, and on the development Mac a mount of
  `/` would have exposed the Mac's own files through the Docker VM. It sits alone on a Docker
  network without route to the outside (`internal`), which only `app` also joins, and every call
  carries a secret generated by the install (`AGENT_TOKEN`).
- **What the agent can do is a closed list**: one report, and four actions (restart one service of
  this stack, write a backup, hand the Let's Encrypt certificate to TAK Server, tidy TAK Server's
  logs). No command, path or container can be named in a request; a service is checked against a
  fixed list and resolved to a container through the Compose labels of this project only.
- **The agent has no outgoing connection.** What needs the Internet (asking registries about
  newer images, the webhook) is done by `app`.
- **Pending system updates** are read without running anything on the host: the agent runs its
  own `apt-get -s dist-upgrade` pointed at the host's package database and lists, both read-only.
  Debian and Ubuntu only, behind a trait; elsewhere the report says "unknown". "Restart required"
  is the flag file `/run/reboot-required`. The clock is the kernel's own answer (`adjtimex`).
- **"A newer image exists"** means: for an image pulled by tag, the registry serves another image
  under that tag; for the image pinned by digest (OpenLDAP), information only; for the images
  built here, nothing (TAK Server has its own version check). Registries are asked at most every
  twelve hours, with a `HEAD` request.
- **A newer TAK Server** is known in two ways: the administrator records the latest version seen
  on tak.gov, and a ZIP of another version at the repository root is noticed. The upgrade is
  `./install.sh --upgrade-tak`, written and **not tested** (only one ZIP exists).
- **Monitoring never sits on a user's path.** One loop looks at everything every
  `MONITOR_INTERVAL_SECONDS` (60; 10 in local mode), with a timeout on every probe, and keeps the
  result in memory; pages read that. The list of connected devices a team leader sees comes from
  it too.
- **Probing TAK Server's client port with TLS is logged by TAK Server as an error** when no client
  certificate is presented. Port 8089 is therefore only asked whether it accepts connections; the
  server's certificate is read on 8443, which stays quiet.
- **Alerts.** Rules are pure functions of what was observed. A problem is an alert once seen on
  two looks in a row, and resolved once gone on two; what could not be read is neither raised nor
  resolved. One e-mail per look that has something to say, to every administrator in their
  language; a reminder at most once per `reminder_hours` (24); the same transitions go to an
  optional webhook as JSON. Thresholds are settings.
- **Backups** are logical: `slapcat` of the directory, `pg_dump` of both databases, plus
  `deploy/data` and `deploy/.env`, in one archive, optionally encrypted (`openssl`, AES-256,
  PBKDF2). The directory is exported first and the platform's database last, so a restore never
  gives the platform less than the directory holds. A restore replaces the four parts together,
  never one alone. It was proven for real (phase 3 report, section 4).
- **A restore un-revokes** what was regenerated or removed after the backup: TAK Server's
  revocations live in its database, which goes back with the rest. Observed, not assumed.
- **The audit log is a table.** Its details are a closed list of named fields, none of which can
  hold a secret; administrator sign-ins, the rescue command and every action of the agent are in
  it. Retention is a setting (one year).
- **The monitoring has its own page** (`/monitoring`), next to the administration: overview,
  connected devices, backups, audit log, settings.

## 4. Functional scope

### Accounts and approval
- Passwordless login to the platform: one-time code sent by e-mail, short-lived, rate-limited per
  address and per IP. No Google login.
- Anyone can sign in, but an account without a team can do nothing and has no TAK access.
- The administrator approves team creation requests only.
- An approved team leader invites members without administrator approval, up to a per-team member cap.
- A server-side rescue command grants administrator access if e-mail is down: it flags the account
  as administrator and prints a one-time login code (`teamtak grant-admin <email>`).
- Roles: platform administrator, team leader, member. A team can have several leaders and always
  keeps at least one. An administrator runs the platform (approval, member cap, suspension,
  deletion) but does not see a team's TAK credentials unless they are a member of it.
- An administrator can suspend a team: its accounts are kept but lose their TAK group, and their
  sessions are cut, until the team is reactivated.

### Teams and connecting to TAK Server
One team = one TAK group, named after the team (lowercase, accents folded; the `evt-` prefix is
kept for events). The team leader picks the mode (shared identity by default), and can change it
later (accesses that do not fit the new mode are revoked):
- **Shared identity**: a single TAK account and a single zip for the whole team. "Regenerate" revokes
  the certificate and produces a new zip. The risk (lost phone, departing member) is accepted by the
  team. Limit found in phase 0: channel state belongs to the account, so one device switching a channel
  off switches it off for the whole team.
- **Individual accounts**: one access per person, revocable one by one.
- **External accounts**: TAK access for a person who has no account on the platform, added by the
  leader in either mode. Until version 1.0.4 "external accounts only" was a third mode; it added
  nothing, so it was dropped and the teams in it moved to the shared identity, keeping their accesses.

An access is created explicitly (by the leader, or by a member for their own individual access), not
automatically, so that no TAK account exists before it is needed.

Each access can be handed over in three forms:
- QR code (iTAK: `name,host,8089,ssl`; ATAK:
  `tak://com.atakmap.app/enroll?host=…&username=…&token=…`, the password travels inside the QR; both
  still have to be validated on real phones);
- username and password (manual enrollment);
- zip (data package) containing only the server address, the truststore and, optionally, the client
  certificate, plus three ATAK settings about the connection itself: the channels shown for the
  server, and TAK Server's profile updates applied (for the maps, added in phase 4). **Never** a
  callsign, team colour, role or group in the zip.

### Cross-team events
A team creates a dated event (start, end) and invites other teams. On acceptance, the platform creates
a dedicated TAK group and adds the members of all participating teams to it; the group is removed at
the end. Outside events, teams do not see each other.

- **Who does what.** A leader of an active team creates an event for their team (the host), invites
  other teams, changes the dates, ends the event early, cancels it, and removes a participating
  team. A leader of an invited team accepts or declines, and can make the team leave later. Members
  see the events their team takes part in (name, dates, participating teams, state) and manage
  nothing. An administrator sees every event and can cancel one; **events need no approval**.
- **Events organised by an administrator** (added after phase 3). An administrator who organises
  a gathering creates the event from the administration area and puts active teams in it
  directly: there is no host team and no invitation to answer, each team takes part at once and
  its leaders get an e-mail. The administrator adds and removes teams, changes the dates, ends the
  event early or cancels it. A leader can still make their team leave; every administrator is
  then told by e-mail. The rules of the group, the dates and the schedule are those of any event.
- **Finding a team to invite.** A leader of an active team can list the names of the other active
  teams, for that purpose only. Nothing else about them is given out.
- **The group** is named `evt-<slug of the event's name>` (32 characters at most, prefix included),
  unique among events that are not over. It holds every TAK account (shared, individual, external)
  of the host and of each team that accepted, from the start to the end, and only while the team is
  active. An access created during the event gets it; a team that accepts while the event runs
  joins at once; a team that leaves, is removed, is suspended or is deleted loses it at once.
  Deleting the host team cancels the event. A finished event leaves nothing in the directory.
- **Dates** are stored in UTC and entered and shown in the browser's time zone. An event may start
  at once, lasts between one minute and 366 days, and once it runs only its end can move.
- **E-mails**, to team leaders, in their saved language: invitation; answer (accepted, declined,
  left) to the host; cancellation, new dates and early end to every team concerned; removal to the
  team removed.
- **Shared identity.** The event applies to the whole team, since the team is one account.

### Monitoring (administrator)
- VPS: CPU, load, RAM, swap, disk per filesystem, uptime, clock synchronisation, pending system
  updates (security ones apart), reboot required. Debian and Ubuntu; "unknown" elsewhere.
- Containers of this stack only: state, health, restarts, image, newer image available.
- TAK Server: ports, version, its five processes, connected devices (new version: semi-manual
  check, tak.gov has no public API).
- Certificates: expiry of the root and intermediate CA, TAK Server's certificate, the platform's
  API certificate, Let's Encrypt, client certificates.
- The platform: reconciliation, drift, whether the event schedule is applied, e-mail failures.
- A history of CPU, memory, disk and connected devices (14 days).
- Connected devices: all of them for an administrator; those on their own team's accesses for a
  team leader; none for members.
- Actions from the web UI, administrator only, each confirmed and audited: restart one service of
  the stack, run a backup, hand the Let's Encrypt certificate over. The UI never applies system
  updates, reboots, or pulls images: it reports and shows the command to run over SSH.
- Alerts by e-mail to every administrator (in their language) and to an optional webhook; team
  leaders are told 30 and 7 days before a zip's certificate expires.
- Backups (daily, optional passphrase, retention), restore by script, off-site copy left to the
  administrator (example hook provided).
- Audit log with filters and paging.

## 5. Roadmap

Phases are sequential: each builds on the code and report of the previous one.

### Phase 0 — technical validation (folder `spike/`, report `docs/phase-0-report.md`) — done
Still to be done by the user: the checks on real ATAK and iTAK phones (procedure in the report). The
questions were:
1. Does the hardened ZIP build without an Iron Bank account (the Dockerfiles pull public images, the
   README says otherwise)?
2. A working OpenLDAP + `CoreConfig.xml` configuration (schema, object classes, service account).
3. Is a group change in the directory applied to an already-connected client, without reconnecting,
   and how long does it take?
4. Programmatic certificate enrollment (CSR, port 8446) and revocation; does the resulting certificate
   support channels?
5. Several simultaneous clients on the same certificate (shared identity).
6. Minimal content of a connection zip accepted by ATAK and iTAK; format of the ATAK enrollment QR.
7. Does TAK Server's "Manage LDAP Users" UI work with this OpenLDAP?
8. Which TAK Server APIs can be used for monitoring (connected clients, version)?

### Phase 1 — core (report `docs/phase-1-report.md`) — done
Still to be done by the user: the phone checks, a production install on the amd64 VPS with a domain
(Let's Encrypt, Brevo), listed in the report.

Repository skeleton (Rust workspace, `web/`, `deploy/`), full Docker Compose, install script working
from the ZIP, e-mail code login, team requests and approval, invitations, the three connection modes,
generation of QR codes/credentials/zips, revocation and regeneration, reconciliation with OpenLDAP,
i18n set up from the first screen (English and French), automated tests.

### Phase 2 — cross-team events (report `docs/phase-2-report.md`) — done
Still to be done by the user: how an event's channel appears and disappears on real ATAK and iTAK
phones (procedure in the report).

Creation, team invitations, acceptance, temporary TAK group, automatic activation and removal at the
scheduled dates.

### Phase 3 — monitoring (report `docs/phase-3-report.md`) — done
Still to be done by the user: a production install on the amd64 Debian or Ubuntu VPS, where the
parts that could not be exercised on the development Mac are checked (list and checklist in the
report): pending updates and reboot detection on a real host, Let's Encrypt, real SMTP, a TAK
Server upgrade.

Host agent, administrator dashboard, alerts, updates, backup and restore, audit log.

### Phase 4 — extensions (one at a time, as the user chooses them)
CloudTAK wired to the same accounts, MediaMTX video, per-team distribution of maps and plugins,
federation.

**MediaMTX video — done (2026-11-21), chosen first by the user.** Decisions:
- the video server is MediaMTX (MIT licence), run unmodified as its own service, on by default
  (`VIDEO=on|off`), RTSP over TCP 8554, RTMP 1935, SRT 8890/udp, no recording, no browser playback;
- **declared streams with their own keys**, not the TAK passwords (RTSP/RTMP/SRT carry credentials
  unencrypted, and a TAK password lets one enroll a certificate): a stream belongs to a team and to
  one channel, the team's or an event's; its path is `<group>/<name>`; one publish key and one view
  key, derived from `APP_SECRET`, the stream and a generation;
- **leaders and members declare streams**; a member manages the ones they declared, a leader all of
  the team's; an administrator sees that they exist, not their keys;
- a stream is valid while its channel exists (same rule as the TAK groups); the video server asks
  the platform about every device (HTTP authentication, internal port 8081, own Docker network);
- the reconciliation registers each valid stream in TAK Server's video manager for its group only
  (`POST /Marti/api/video?group=`, checked on 5.8: feeds are filtered by the caller's groups), so
  ATAK lists it without anything typed, and disconnects devices from streams no longer valid.

Still to check on real devices: ATAK and iTAK listing and playing the feed (`rtsp://view:<key>@…`,
RTSP over TCP), a DJI controller and Larix publishing over RTMP and SRT.

**Maps per team — done (2026-11-21), chosen second by the user ("tu peux l'ajouter").** Decisions:
- **A catalogue kept by the administrators.** ATAK map sources (`customMapSource` XML files) are
  uploaded one by one or as a zip; an ATAK data package of map sources works as it is. A file of the
  same name replaces the old one, on the phones too (a device keeps a map source by its file name).
  A file goes to phones as it is, so it is checked first: a `customMapSource` root, a name, an
  `http(s)` tile URL with `{$z}`, `{$x}`, `{$y}` (or `{$q}`), no DTD.
- **Providers' terms.** TeamTAK suggests only providers whose terms let a server hand their address
  to a few dozen phones: IGN's Géoplateforme (plan and aerial photographs, open data, no key) and
  OpenTopoMap (a volunteer server: no mass downloads). Not OpenStreetMap's own tile servers (no
  offline use, and an attribution on the map that ATAK does not show), never Google or Bing (their
  tiles only through their own applications and terms), as decided for CloudTAK's Google basemaps.
  Such a source can still be added; the catalogue and the leaders' picker flag it. The user's own
  zip of map sources (`mapsources-2026-11-16.zip`: IGN plan and photographs, OpenTopoMap, three
  Google layers) is not in the repository.
- **Who picks.** A team's leaders pick the maps of the team's channel; an event's organisers (the
  leaders of the host team, or the administrators for an event they organise) the maps of the
  event's channel, which reach the devices of every team taking part while it runs. Every member
  sees them and downloads them as a data package. At most 30 maps per channel.
- **Leaders add sources of their own** (asked by the user on 2026-11-21: "let the team admins add
  a new base map"): only their team sees them, they go on the team's channel at once, the
  administrators see and can remove them. A Google or Bing source added by a leader is accepted
  and flagged (the user's choice, over keeping such sources to the administrators).
- **Every source is checked before it is sent to anyone** (asked by the user: "so people are not
  sent maps that don't work"): the file is read, then one of its tiles is fetched from the server,
  at the lowest zoom level the source offers and a few levels deeper, over three points, until
  one is a PNG, JPEG, GIF or WebP image; otherwise the source is refused with the provider's
  answer. Only public addresses are fetched: private, local and reserved ranges are refused,
  through a resolver that drops them, after redirects too. Every source is checked again every six
  hours; one that fails twice in a row is held back from the profiles until a check finds it
  working (audit log: `map_source_broken`, `map_source_working`). Since a profile cannot take a map
  back from a phone, this only stops new devices getting a broken map.
- **The user's zip** was imported on the local stack (its Google layers flagged); the repository
  only suggests its three other sources, with the same file names.
- **Device profiles, kept by the reconciliation** like the video feeds: one connection profile per
  group that has maps and exists now, `teamtak-maps-<group>`, for that group only; and one
  enrollment profile, `teamtak-enrollment`, for every active team's group, whose only file turns on
  ATAK's "Apply TAK Server profile updates" (`deviceProfileEnableOnConnect`). That setting is off
  by default in ATAK (checked in ATAK-CIV's source), and ATAK then asks for no profile when it
  connects; the connection zips set it too. Neither sets callsign, team colour, role or groups.
  Profiles not named `teamtak-…` are left alone. An event's profile appearing at its start and
  leaving at its end is the schedule at work, not drift.
- **Established on TAK Server 5.8-RELEASE-103** (classes `ProfileAPI`, `ProfileAdminAPI`,
  `ProfileService` and `ProfileRepository` of `takserver.war`, then on the local stack with the
  platform's administrator certificate):
  - administration: `GET /Marti/api/device/profile`; `POST /Marti/api/device/profile/{name}?group=…`
    creates an active enrollment profile (answers 200 and changes nothing when the name exists);
    `PUT /Marti/api/device/profile/{name}` with a JSON profile sets its groups and, from `type`,
    whether it applies at enrollment or at connection, never both; `PUT …/{name}/file?filename=`
    adds a file; `DELETE …/{name}/file/{id}` and `DELETE …/{id}` remove; `GET …/{name}/files` and
    `GET …/{name}/missionpackage` read (file *i* at `file<i>/<name>`, with `onReceiveImport` and
    `onReceiveDelete`);
  - devices: `GET /Marti/api/tls/profile/enrollment?clientUid=` on 8446 (username and password),
    `GET /Marti/api/device/profile/connection?clientUid=&syncSecago=` on 8443 (certificate). A
    profile goes to a device whose groups share a bit with the profile's (for a connection, the
    groups of the streaming connection named by `clientUid`); with `syncSecago` only the profiles
    updated since are sent, and adding or removing a file updates the profile. With
    `x509useGroupCache`, TAK Server adds its own `enable-channels.pref` to every answer;
  - a group a profile names that TAK Server does not know yet is created.
- **Limits**, stated in the docs and on the team page: a profile adds files to a device and never
  removes any; the guide names ATAK only, so the team page offers the maps as a data package for
  iTAK (or ATAK with the setting off); offline maps (MBTiles, SQLite) are not handed out, and an
  area is downloaded for offline use only where the provider allows it, for an event's zone.

Still to check on real devices: ATAK asking for the connection profile and listing the maps under
Mobile; the enrollment profile turning "Apply TAK Server profile updates" on after a QR code or a
username and password; a device set up with a zip of TeamTAK 1.1.0 or older (the setting stays
off until ticked or the zip is imported again); what iTAK does with TAK Server's profiles and with
the maps' data package.

CloudTAK, checked 2026-11-20 on v13.124.0 (licence, then accounts). Licence: AGPL-3.0-only,
standard text with no added terms. Its production dependencies (MIT, Apache-2.0, BSD, ISC,
LGPL-3.0, MPL-2.0, AGPL) and its side services (media-infra and Garage under AGPL-3.0, PostGIS under
GPL-2.0) are all compatible. It holds no TAK Server code and only talks to it over the network, so
the tak.gov licence is not involved. What follows for TeamTAK:
- **Run it unmodified, as its own service**: its own containers, built on the server from an
  official CloudTAK release tag, the way the install already builds the platform's images. TeamTAK
  talks to it over the network only. This is an aggregate in the AGPL's sense: no obligation on
  TeamTAK's code or licence, and TeamTAK does not redistribute CloudTAK.
- **Never copy CloudTAK code into TeamTAK** (`web/`, the crates): TeamTAK would become a derivative
  work and would have to be released entirely under AGPL-3.0.
- **If a CloudTAK image is published** (ghcr.io or elsewhere): ship the licence, keep the copyright
  notices and point to the exact corresponding source (the upstream tag, if unmodified).
- **If CloudTAK is modified** (a fork): the fork stays under AGPL-3.0, and every install running it
  must offer the modified source to all its users (section 13): a "Source" link in the UI to the
  public fork.
- **Same accounts**: CloudTAK checks the username and password with TAK Server (its OAuth endpoint),
  and TAK Server reads OpenLDAP, so an unmodified CloudTAK already accepts TeamTAK accesses that
  have a password, then enrolls its own certificate for that username. One CloudTAK profile per
  person needs the "one account per member" mode: CloudTAK keys profiles by TAK username, so a
  shared identity means one shared profile. The reconciliation leaves CloudTAK's certificate alone
  (it only revokes certificates of usernames with no access, or those asked for), and
  *Regenerate* or *Remove* revoke it too, which cuts CloudTAK off as well.
- **CloudTAK's admin certificate**: if TAK Server issues it under a username that is not an access,
  the reconciliation revokes it as an orphan. Its username must join the protected ones (today the
  platform's certificate and the administration pages' one, in `crates/server/src/state.rs`).
- **No real SSO yet** ("Sign in with TeamTAK" in CloudTAK), as of v13.124.0:
  - CloudTAK's OIDC is half there: admin settings (discovery URL, client ID and secret, redirect,
    scopes) marked "Beta", and a login button to `/api/login/oidc`, but no route in the public code
    handles that URL or the callback.
  - CloudTAK only obtains a user's TAK certificate with that user's TAK password, at first login
    (its SCIM provisioning says so too), and an SSO login carries no password.
  - TeamTAK is not an identity provider: the e-mail code login exposes no OIDC.

  Real SSO would take TeamTAK acting as an OIDC provider (the e-mail code login unchanged), and in
  CloudTAK the OIDC route and callback plus a way to issue the TAK certificate without the user's
  password (with its admin certificate, say). The CloudTAK part is a modification under the AGPL:
  better contributed upstream, which has started the feature, than kept as a fork.
- **Not covered by the licence**: CloudTAK's name and logo (no trademark grant: say "includes
  CloudTAK", never present a fork as the official one), and the Google basemaps shipped in its data,
  which call Google's tile servers directly and may break Google's terms: leave them off by default.

Dropped: a Data Sync mission created automatically for each event. Users create their own missions
(decided by the user).

## 6. Working rules

- **Everything is written in English**: code, identifiers, comments, commit messages, documentation,
  reports, scripts and their output. Conversation with the user stays in French.
- **The web UI is internationalised** with react-i18next:
  - English is the default and the fallback language; French is available and switchable from the UI.
  - No user-facing string is hard-coded in components: all go through translation keys, with one
    resource file per language (`web/src/locales/en/`, `web/src/locales/fr/`). Every key exists in both.
  - Language choice: the user's saved preference, else the browser language if supported, else English.
  - The API returns stable error codes, not sentences; the UI translates them.
  - E-mails (login codes, invitations, alerts) are sent in the recipient's saved language, English by
    default. When the recipient has no account yet: a login code uses the language of the page it
    was requested from, and an invitation the language the inviter picks (their own by default).
  - E-mail texts live next to the UI texts (`web/src/locales/<lang>/email.json`) and are compiled
    into the server.
  - Dates, times and numbers are formatted through the locale, not by hand.
  - Adding a third language must require only a new resource file and one entry in the language list.
- The TAK Server ZIP is under the tak.gov licence: never version or redistribute it, or its contents.
- No secrets in the repository (passwords, keys, generated certificates): `.env` files and certificate
  folders are git-ignored.
- One local commit at the end of each phase, once the report is written (agreed with the user). No
  other commit and no push without an explicit request.
- The development machine is an arm64 Mac with Docker; the target VPS is amd64 Linux.
- Each phase ends with its report: what works and was verified, what was not, and any deviation from
  this document.
