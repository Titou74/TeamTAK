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
| `mediamtx` | The video server ([MediaMTX](https://github.com/bluenviron/mediamtx), MIT licence), when `VIDEO=on`: drones, cameras and phones publish to it, ATAK and other players watch from it ([Video streams](#video-streams)) |

## Ports

| Port | Use |
|---|---|
| 443, 80 | Web UI in production (80 serves the Let's Encrypt challenge and redirects to HTTPS) |
| 8480 | Web UI in local mode |
| 8089, and any other port of `TAK_CLIENT_PORTS` | TAK clients, mutual TLS ([Client ports](configuration.md#client-ports)) |
| 8443 | TAK Server's API, for TAK clients, mutual TLS: their channels, the files they send ([Channels on the devices](#channels-on-the-devices)) |
| 8446 | TAK certificate enrollment |
| 8444, or `TAK_ADMIN_PORT` | TAK Server's own administration pages, served by Caddy to the platform's administrators ([TAK Server's administration](configuration.md#tak-servers-administration)) |
| 8554/tcp, 1935/tcp, 8890/udp, or `VIDEO_*_PORT` | The video server, when `VIDEO=on`: RTSP, RTMP and SRT ([Video streams](#video-streams)) |

Every client port of the machine leads to the same TLS input of TAK Server, 8089 inside its
container: TAK Server itself has a single input, and the monitoring watches that one. 8443 of the
machine is TAK Server's own 8443, published as it is: it asks for a client certificate, and gives
the administrator role to none of the certificates handed to TAK clients. The platform and Caddy
reach it over the Docker network.

## How it works

- **Accounts.** Sign-in is passwordless: a six-digit code sent by e-mail, valid ten minutes, usable
  once, rate-limited per address and per IP. The first sign-in of an address creates its account;
  an account without a team can do nothing and has no TAK access. Before a code is sent, the
  sign-in page passes an invisible anti-bot check, and a new code for the same address waits for a
  countdown (see [Login protections](#login-protections)). Who may sign up is a setting (see
  [Sign-ups and team approval](#sign-ups-and-team-approval)). Every sign-in accepts the
  [terms of use](#terms-of-use) first.
- **Teams.** One team is one TAK group (its name is the channel shown in ATAK and iTAK, see
  [Channels on the devices](#channels-on-the-devices)). By default an
  administrator approves team requests and sets the member cap; that is the only approval step,
  and it can be turned off. A team leader invites members by e-mail, up to the cap.
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
  An administrator can also organise an event from the administration area (`organizer = admin`
  in the `events` table): it has no host team, and the administrator puts active teams in it
  directly, with no invitation to answer, takes them out, changes the dates, ends it or cancels
  it. The leaders of a team put in are told by e-mail and can still make their team leave; the
  administrators are then told. An event hosted by a team stays the team's to run: an
  administrator can only cancel it.
- **Video streams.** A member or a leader declares a stream (a drone, a camera, a phone) for the
  team's channel or for an event's; it gets an address to publish to and one to watch from, each
  with its own key, and TAK Server lists it to the members of that channel, so ATAK shows it
  without anything to type ([Video streams](#video-streams)).
- **Maps.** The administrators keep a catalogue of map sources; a team's leaders pick the team's
  maps from it, and an event's organisers the event's. TAK Server hands them to the devices of
  that channel, as device profiles, when ATAK starts and connects, and ATAK lists them among its
  maps ([Maps](#maps)).
- **Team files.** A team's members see the TAK Server files of the team's channels (data
  packages, files of Data Sync missions), download them, and send files to them, from the team
  page ([Team files](#team-files)).
- **Reconciliation.** PostgreSQL is the source of truth. After every change, every minute, and at
  the instant an event starts or ends, a pass compares it with OpenLDAP and TAK Server and corrects
  the difference: accounts, group memberships, passwords, certificates that should not work,
  sessions left with no group, the video feeds TAK Server lists, the devices still using a
  video stream that no longer works, and the device profiles that carry the maps. What a pass corrects without the platform having asked for
  it is reported as drift in the administration area. TAK Server sometimes keeps listing a
  session whose connection is gone, without any group, and refuses to cut it (when a device
  disconnects right after connecting, more often just after TAK Server started). Such a leftover
  sees nothing and nobody sees it: a pass logs it as a warning instead of failing, and it
  disappears when TAK Server restarts.

## Channels on the devices

A team's TAK group is what keeps it apart from the other teams: TAK Server only passes a device
the traffic of its groups, whatever the device shows. ATAK and iTAK also list those groups as
channels, which the user can switch off and on. Two things make that list appear:

- **The list comes from TAK Server's API.** The device asks for it with its own certificate
  (`GET /Marti/api/groups/all?useCache=true`) on port 8443 of the server, whatever port it streams
  on; ATAK reads it there again when TAK Server says the groups changed, an event's group for
  instance. Hence 8443 is published for TAK clients, and the monitoring shows it as not published
  when Docker does not lead it to TAK Server. TeamTAK 1.0.7 and older kept 8443 for TAK Server's
  administration pages, and devices showed no channel at all.
- **ATAK shows its Channels tool only once told to**, by two preferences: `prefs_enable_channels`
  and `prefs_enable_channels_host-<server>`. TAK Server hands them out in the profile a device gets
  when it enrolls (a QR code, a username and password, a zip without certificate), because
  channels are on in its configuration (`x509useGroupCache`). A device that connects with the
  certificate of a zip never enrolls, so every zip carries both preferences, for the server's
  address, as TAK Server writes them.

The group is switched on from the start (`x509useGroupCacheDefaultActive`), and so is an event's
group when it appears. With a shared identity, a channel switched off on one device is off for all
the devices of the team (phase 0).

## Video streams

The video server, MediaMTX, runs next to TAK Server when `VIDEO=on` (the default of a new
installation; [configuration](configuration.md#video-server)). It carries the streams; the
platform decides which exist and who may use them.

- **A stream is declared.** On the team page, a leader or a member gives it a name ("Drone 1") and
  a channel: the team's, or that of an event the team takes part in and that is not over. Its path
  on the video server is `<channel>/<name>`, for example `alpha/drone-1` or
  `evt-exercise/drone-1`. A team has at most 20 streams per channel. A member manages (changes the
  keys of, deletes) the streams they declared, a leader every stream of the team.
- **Two keys of its own.** One to publish (`publish` user), one to watch (`view` user). They are
  not TAK passwords: RTSP, RTMP and SRT send them unencrypted, and a key that leaks opens that one
  stream, never a TAK account. They are derived from `APP_SECRET`, the stream's identifier and a
  generation that *Change the keys* raises; nothing is stored. The keys are a secret of the team:
  its leaders and members see them, an administrator does not, and the audit log records who was
  shown them.
- **The addresses handed out** name the public host and the published ports:
  `rtsp://publish:<key>@host:8554/alpha/drone-1`,
  `rtmp://host:1935/alpha/drone-1?user=publish&pass=<key>`,
  `srt://host:8890?streamid=publish:alpha/drone-1:publish:<key>` (and the same with `view` and
  `read` to watch). RTSP only goes over TCP: one port to open, and it crosses NAT.
- **The video server asks the platform** about every device that connects (MediaMTX's HTTP
  authentication): the platform answers on its internal port, 8081, which Caddy does not pass on,
  over a Docker network the two share alone (`video`). A device is let in when the path is that of
  a declared stream, the stream is valid now, and the key is the right one for what it asks:
  publishing takes the publish key, watching either key.
- **Valid while its channel exists**: the team is active and, for an event's stream, the event
  runs and the team takes part. It is the rule of the team's TAK groups, applied at the same
  instants. Outside it, the keys are refused.
- **In ATAK by itself.** The reconciliation registers each valid stream in TAK Server's video
  manager, for the channel's group only (`POST /Marti/api/video?group=…`), with the view address
  and RTSP over TCP. TAK Server hands a feed to the devices that share a group with it, so a
  stream shows up in ATAK among the server's videos for the channel's members and nobody else; an
  event's streams appear at its start and leave at its end. New keys update the address there.
  Feeds whose identifier does not start with `teamtak-video-` were registered by someone else and
  are left alone.
- **Devices cut when a stream stops working.** The video server checks a key when a device
  connects, not afterwards. *Change the keys* and *Delete* disconnect at once whoever publishes
  to or watches the stream; at the end of an event, when a team is suspended or deleted, the
  reconciliation pass does the same (journal: "Devices disconnected from a video stream").
- **Nothing is recorded**, and nothing of the video server needs a backup: the streams are in the
  platform's database, their keys follow from `APP_SECRET`.

What the platform reads of the video server, the live streams and their viewers, comes from its
control API (9997), which is not published either. The team page shows each stream live or not;
the monitoring page lists the live streams of every team ([monitoring](monitoring.md)).

## Maps

An ATAK map source is a small XML file (`customMapSource`): a name, zoom levels, a tile type and
a URL template with `{$z}`, `{$x}` and `{$y}`. ATAK lists it among its maps (Mobile tab) and
fetches the tiles it needs while the map is shown. Nothing of the map itself goes through the
server or to the phone in advance.

- **The catalogue belongs to the administrators** (Administration, Maps). They upload map source
  files, or a zip of them such as a data package exported from ATAK, or add a suggested source.
  A file is checked before it is kept: a `customMapSource` root, a name, an `http(s)` tile URL
  with `{$z}`, `{$x}` and `{$y}` (or `{$q}`), and no DTD, since it goes to phones as it is. A
  device keeps a map source by its file name, so the catalogue keeps the uploaded name when it is
  plain (`IGN-Plan.xml`), and a file of the same name replaces the old one, on the phones too.
- **A team's leaders add sources of their own** from the team page, the same way (a file or a
  zip). Only their team sees them; they go on the team's channel at once, and the leaders can also
  pick them for an event their team hosts. The administrators see them in the catalogue, with the
  team's name, and can remove them.
- **Every source is checked before anyone gets it.** Besides reading the file, TeamTAK fetches
  one of its tiles, the way a phone would: the lowest zoom level the source offers, then a few
  levels deeper, over France, the equator and New York, until one comes back as an image (PNG,
  JPEG, GIF or WebP). A source with no such tile is refused, with what the provider answered.
  The fetch goes to public addresses only: a name or address that leads to a private, local or
  reserved network (the stack's own services among them) is refused, even after a redirect.
  Every source is checked again every six hours; one that fails twice in a row is held back from
  the devices (its profiles no longer carry it, the team page and the catalogue say why) until a
  check finds it working again, which the audit log records too. *Check again* runs a check at
  once. A check runs from the server: a provider that answers the server differently from the
  phones (a blocked address range, a key bound to an app) gives a different result.
- **What each provider allows** is shown next to each source, as far as TeamTAK knows it: open
  data (IGN's Géoplateforme), light use without mass downloads (OpenTopoMap, a volunteer server),
  no offline use (OpenStreetMap's own tile servers, whose policy also wants an attribution on the
  map, which ATAK does not show), restricted (Google, Bing: their tiles only through their own
  applications and terms), or unknown. Only the first two kinds are suggested: IGN's plan and
  aerial photographs, and OpenTopoMap. A restricted source can still be added, by an
  administrator or by a team's leaders; it is flagged wherever it is picked.
- **Who picks.** A team's leaders pick the maps of the team's channel, an event's organisers the
  maps of the event's channel (the leaders of the host team, from the catalogue and their team's
  own sources, or the administrators for an event they organise, from the catalogue), at most 30
  per channel. Every member sees them and can download them.
- **Device profiles.** The reconciliation keeps one device profile in TAK Server per group that
  has maps and exists now (TAK Server 5.8's device profiles, `/Marti/api/device/profile`):
  - `teamtak-maps-<group>`, applied on connection, for that group only. A device that asks for
    its profiles (`GET /Marti/api/device/profile/connection?clientUid=…&syncSecago=…`, with its
    certificate on 8443) gets the profiles of the groups of its streaming connection, as a data
    package that ATAK imports by itself. An event's profile exists from its start to its end, like
    its group; a suspended team has none.
  - `teamtak-enrollment`, applied at enrollment, for the group of every active team. It holds one
    preference file, which turns on ATAK's *Apply TAK Server profile updates*
    (`deviceProfileEnableOnConnect`): it is off in ATAK by default, and ATAK then asks for no
    profile when it connects. A device enrolled by QR code or by username and password gets it;
    every connection zip sets the same preference. Nothing else is set: no callsign, team colour,
    role or group.

  Profiles whose name does not start with `teamtak-` were made by someone else, in TAK Server's
  own pages, and are left alone.
- **When a device gets them.** ATAK asks a server for its profiles once per launch: the first
  time it connects to that server after it starts, giving the seconds since it last asked that
  server (everything, the first time). TAK Server answers with the profiles changed since (every
  file added or removed counts as a change), and logs `Returning connection profile for <device
  uid>` only when it sends files. Reconnecting, or removing the server in ATAK and adding it
  again, asks for nothing: a device already running gets new maps, or those of an event that
  just started, the next time ATAK starts. ATAK imports the package by itself, without a prompt,
  copies each map source to `atak/imagery/` and deletes the package, so nothing shows among its
  data packages. Checked with ATAK 5.6.0 on Android 13, and in ATAK-CIV's source
  (`CotMapServerListener`, `DeviceProfileOperation`).
- **Limits.**
  - A profile adds files to a device and never removes any. A map taken off a channel, removed
    from the catalogue, or that of an event that ended, stays on the phones that have it: delete
    it there if it must go (its file is in `atak/imagery/`).
  - TAK Server's guide describes device profiles for ATAK only. For iTAK, or for ATAK with the
    setting off (a device set up with a zip from TeamTAK 1.1.0 or older), the team page offers the
    maps its devices get now (the team's and those of the events running) as a data package to
    import by hand.
  - Offline maps (MBTiles, SQLite) are not handed out: they are large. Downloading an area for
    offline use from a map source is up to the user, only where its provider allows it, and only
    for an event's zone.

## Team files

TAK Server keeps two kinds of files a team cares about: **data packages** in Enterprise Sync,
sent to the server by a device (ATAK lists them among the server's data packages), and the
**files of Data Sync missions**, created in ATAK. The team page shows both to the team's leaders
and members (not to an administrator outside the team: they are the team's, like its
credentials).

- **What a team sees is what its devices would get now.** A file or a mission is listed when its
  groups meet the team's: the team's own group, and the group of each event it takes part in
  while the event runs. The platform reads TAK Server with an administrator's certificate, which
  sees every file and every mission, so it applies that rule itself, at every request, on what
  TAK Server answers: `GET /Marti/sync/search` (keys, sizes, senders) joined with
  `GET /Marti/api/files/metadata` (the groups of each upload) by content, name and time, and
  `GET /Marti/api/missions?passwordProtected=true&defaultRole=true`.
- **Never anything of a protected mission.** A mission with a password, or open on invitation
  only, is listed locked, without its files; its files are left out of the data packages too, and
  so are the archives TAK Server keeps of deleted missions (`ARCHIVED_MISSION`). This is needed:
  the administrator's certificate gets a protected mission's files without its password (checked
  on 5.8). Downloads go through the platform, which checks again each time.
- **Sending.** Any member of an active team sends a file to the team's channel or to that of an
  event running: it is stored for that group only (`Groups=` on `POST /Marti/sync/missionupload`:
  without it, an upload with the administrator's certificate would be stored for every group).
  A file that is not a data package (a PDF, a KML, a picture) is wrapped in one, a manifest and
  the file, so that ATAK imports it. A member also adds a file to a mission the team sees, unless
  it is protected or read-only (its default role does not write): the file is stored for the
  mission's groups and linked to it. The platform never creates a mission.
- **Removing.** TAK Server lets anyone who shares a group delete a file, without any notion of an
  owner; the platform is stricter. A member removes what they sent through TeamTAK, a leader what
  the team sent (through TeamTAK, or with one of its TAK accesses), and never another team's
  file. A data package is removed by its key, never by its content: several uploads can share one.
  Taking a file out of a mission only unlinks it. The table `team_files` records who of which team
  sent what through the platform, since TAK Server records the platform's own account.
- **Size.** A file sent from a team page is limited by a setting (**Monitoring, Settings**,
  100 MB by default, at most TAK Server's own 400 MB, `enterpriseSyncSizeLimitMB`). The platform
  reads the request whole before sending it on: a large file uses that much memory for a moment.
- **Audit.** Every file sent, downloaded, removed, added to or taken out of a mission is in the
  audit log, with its name, its size and its channel or mission.

## Sign-ups and team approval

Two settings, in **Monitoring, Settings, Sign-ups and teams** (`invitation_only` and
`team_approval` in the `settings` row), let an administrator run anything from a server open to
all to one where they decide who comes in and which teams exist. Both read the same way: checked
(`true`), an administrator controls the step; unchecked, it is open to everyone. By default
sign-ups are open and new teams wait for approval, which is how the platform always worked.

- **Sign-ups by invitation only** (`invitation_only`). Checked, an address gets an account only
  when an invitation waits for it in an active team, or when an administrator invited it from **Administration,
  Accounts** (`POST /api/admin/accounts`, which creates the account at once and e-mails it an
  invitation; `account_invited` in the audit log). Accounts that exist keep signing in. To any
  other address, `request-code` sends an e-mail saying that sign-ups are by invitation only
  instead of a code, and answers the page exactly as when a code goes out, so the page does not
  learn who has an account or an invitation; that e-mail counts for the countdown and the hourly
  limits like a code. The sign-in page warns that new accounts are by invitation. A code sent
  while the address could sign up is refused with `signups_closed` if its invitation was
  withdrawn, or sign-ups closed, before it was used.
- **New teams wait for an administrator's approval** (`team_approval`). Unchecked, `POST /api/teams`
  creates the team active at once, with `DEFAULT_MEMBER_CAP`, its requester as leader;
  administrators are not e-mailed, the audit log says `team_created`, and the limit of three
  requests waiting per account does not apply. The home page then says "Create a team". Requests
  waiting when approval is unchecked stay for an administrator to decide.

The administration's **Teams** tab shows both settings with a link to change them.

## Login protections

Asking for a login code sends an e-mail, so `POST /api/auth/request-code` is what a script would
hammer. Four layers stand in front of the mail server, checked in this order:

1. **An invisible anti-bot check.** As soon as the sign-in page opens, it fetches a challenge
   (`GET /api/auth/challenge`) and solves it in a Web Worker while the address is typed: find the
   number `n ≤ max_number` such that `SHA-256(salt + n)` equals the given hash. That is half a
   million hashes on average with the default `LOGIN_CHALLENGE_DIFFICULTY` (a fraction of a
   second on a computer, about a second on a phone), with no picture to click and no third-party
   service. The challenge is signed with `APP_SECRET`, expires after ten minutes and is accepted
   once (`login_challenges` table). A request without a valid solution gets `challenge_failed`.
   The page solves the next challenge in advance, for a "send a new code". SHA-256 is written in
   [`web/src/lib/proof-of-work.ts`](../web/src/lib/proof-of-work.ts) rather than taken from
   `crypto.subtle`, which only exists on HTTPS pages.
2. **A countdown per address.** Another code goes to the same address only
   `LOGIN_RESEND_COOLDOWN_SECONDS` after the previous one (60 by default). Earlier, the answer is
   `429 resend_too_soon` with the seconds left (`Retry-After` header, `retry_after` in the body),
   and the page counts them down on its "send a new code" button.
3. **Hourly limits** per address and per IP (`LOGIN_MAX_CODES_PER_EMAIL`, `LOGIN_MAX_CODES_PER_IP`).
4. **Wrong codes** burn the code after five tries, and an IP that submits too many wrong codes is
   blocked for the hour (`LOGIN_MAX_FAILURES_PER_IP`).

Codes made by the rescue command (`teamtak grant-admin`) go through none of these, nor through
[invitation-only sign-ups](#sign-ups-and-team-approval): the command creates the account. The
sign-in page offers "I already have a code" only while one of them waits to be used, not consumed,
not expired and not burnt (`rescue_code_pending` in `GET /api/config`): the first administrator
sees it right after the install, and once they are in nobody does until the command is run again.

## Terms of use

The sign-in page shows a box to tick, "I have read and accept the terms of use", with a link that
opens them; no code is asked for until it is ticked. The footer of every page opens them too, and
the profile page says when the account accepted them.

- **The text** is in `web/src/locales/<language>/terms.json`, next to the UI's translations, one
  file per language with the same sections (`pnpm check:i18n` checks they match). It sets apart
  the creator of TeamTAK, the maintainer who hosts the server, its administrators, team leaders
  and users, and puts on whoever adds a map source, a video, a file or a setting the
  responsibility for it: a Google Maps source added by an administrator is that administrator's
  doing, not the creator's nor the maintainer's.
- **The version** is the day the text last changed: `version` in each `terms.json`, and
  `TERMS_VERSION` in `crates/server/src/terms.rs`; a unit test fails while they differ.
- **Acceptance is recorded on the account** (`terms_version`, `terms_accepted_at`).
  `POST /api/auth/verify` refuses `terms_not_accepted` unless it carries the version in force
  (`terms_version`), before looking at the code, so no try is spent; each sign-in records it
  again. `GET /api/me` returns `terms.accepted_at`, `null` while the account has not accepted the
  version in force.
- **A new version** is asked for at the next visit: an account signed in before the terms
  existed, or before they changed, sees them in place of the platform, with "I accept" and
  "Sign out" (`POST /api/me/terms`), and nothing else until it accepts. TAK accesses and devices
  are not affected.

To change the text, edit every `terms.json` and set `version` there and `TERMS_VERSION` to the
day's date. A modified text is a modified version of TeamTAK in the sense of the licence
([NOTICE](../NOTICE)).

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
