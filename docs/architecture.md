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
  [Sign-ups and team approval](#sign-ups-and-team-approval)).
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
- **Reconciliation.** PostgreSQL is the source of truth. After every change, every minute, and at
  the instant an event starts or ends, a pass compares it with OpenLDAP and TAK Server and corrects
  the difference: accounts, group memberships, passwords, certificates that should not work,
  sessions left with no group, the video feeds TAK Server lists, and the devices still using a
  video stream that no longer works. What a pass corrects without the platform having asked for
  it is reported as drift in the administration area.

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
