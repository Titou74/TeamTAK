# Phase 1 report — core of the platform

Date: 2026-11-19. Scope: section 5, "Phase 1", of [`PLAN.md`](PLAN.md), built on the findings of
[`phase-0-report.md`](phase-0-report.md). How to install, run and test is in the
[`README`](../README.md).

Environment: Mac arm64, Docker 29.8 (OrbStack), TAK Server `5.8-RELEASE-103` built from
`takserver-docker-hardened-5.8-RELEASE-103.zip`, OpenLDAP 2.6.10 (`vegardit/openldap`, pinned by
digest), PostgreSQL 17, Rust 1.100, Node 22. No ATAK or iTAK device, no domain name and no Brevo
account were available: TAK clients are simulated by a scripted client, and three things could not
be tested at all (section 4).

## 1. Summary

The core works end to end on this machine. `./install.sh --local` brings the whole stack up from
nothing; a user signs in with an e-mailed code, requests a team, an administrator approves it, the
leader invites members and hands out TAK accesses, and the resulting certificates connect to the
real TAK Server with the right isolation between teams.

| Area | State |
|---|---|
| Install from the ZIP, full Docker Compose | Works from a clean state; replayed after removing every container, volume, image and generated file of the project |
| Login by e-mail code, sessions, roles, rescue command | Works; tested against the running stack |
| Team request, approval, invitations, member cap | Works; tested |
| Three connection modes; QR strings, credentials, two zips | Works; zips and certificates tested against TAK Server, QR strings checked for format only |
| Regenerate, remove | Works; open connections are cut and old certificates refused, tested |
| Reconciliation, drift report | Works; drift injected in OpenLDAP is repaired and reported, tested |
| i18n, English and French | In place from the first screen; enforced by automated checks |
| Phones (ATAK, iTAK), Let's Encrypt, amd64, real SMTP | **Not verified** (section 4) |

Four results change what the plan said or what phases 2 and 3 can assume:

1. **TAK Server's list of "active" certificates cannot be trusted for revocation.** It still lists a
   revoked certificate, and hides a valid one that a device replaced by enrolling again
   (section 5.1). The platform reads the full list instead.
2. **A session opened without any group never receives a group added later** (section 5.2). With
   the phase 0 finding (the last group is never removed from an open session), the rule is: TAK
   Server only updates an open session that has a group before and after. Otherwise the platform
   cuts the session.
3. **The ZIP's certificate script gives the intermediate CA two years**, which would end every
   client certificate at once (section 5.3). The install gives it ten.
4. **TAK Server's database image only creates its tables if it can reach the host name
   `tak-database`** (section 5.4).

## 2. What was built

```
crates/core/      domain rules, reconciliation, LDAP and TAK Server adapters, zips, QR strings
crates/server/    API (axum), PostgreSQL (sqlx), e-mail, CLI; migrations/, tests/
web/              React + TypeScript + Vite, Tailwind, shadcn/ui, react-i18next
deploy/           docker-compose files, Caddyfiles, OpenLDAP bootstrap, TAK Server config, scripts
scripts/          test runners
install.sh        install from nothing, safe to run again
```

`spike/` is untouched and stays as the phase 0 record. `deploy/ldap/` and
`deploy/takserver/CoreConfig.tpl.xml` come from it, with the changes listed in section 6.

### Services

`caddy`, `app`, `postgres`, `openldap`, `takserver`, `takserver-db`, and `mailpit` in local mode.
Published ports: the web UI, 8089 and 8446. Port 8443 is not published.

### The platform

- **Accounts.** Six-digit code by e-mail, valid 10 minutes, usable once, at most 5 wrong attempts per
  code. Limits per hour: 5 codes per address, 30 codes and 30 wrong codes per IP address. Only an
  HMAC of the code is stored. Sessions are an HttpOnly, SameSite=Lax cookie (30 days, sliding);
  state-changing requests need a custom header, so another site cannot act with a visitor's session.
  The first successful login of an address creates its account.
- **Rescue command.** `tak-manager grant-admin <email>` makes the account an administrator and prints
  a one-time code; the sign-in page has an "I already have a code" link for it, so it works when
  e-mail is down.
- **Teams.** A request creates a pending team; an administrator approves it (and sets the member
  cap) or rejects it. That is the only approval step. A leader invites by e-mail; members plus
  pending invitations cannot exceed the cap. An administrator can also change the cap, suspend,
  reactivate and delete a team.
- **TAK accesses.** One row per TAK account, created explicitly. Each access gives two QR codes
  (iTAK, ATAK), the username and password, an enrollment zip and a zip with a client certificate.
  The certificate of the second zip is enrolled by the platform through TAK Server's own enrollment
  (ports 8446, the account's own credentials), never with `makeCert.sh`, so it carries the key usage
  channels need. It is stored encrypted and the same one is handed out until it is regenerated.
- **Regenerate** revokes every certificate TAK Server holds for the account (the zip's and those
  devices enrolled by themselves), changes the password, and enrolls a new zip certificate if there
  was one. **Remove** deletes the access from the database; the reconciliation takes it out of its
  group, revokes its certificates, then deletes the directory account, in that order.
- **Reconciliation.** PostgreSQL is the source of truth. A pass runs after every change (so the
  change is live when the API call returns), every 60 seconds, at startup, and on an administrator's
  request. It compares the database with OpenLDAP (accounts, passwords, group members) and with TAK
  Server (certificates, sessions), corrects the difference, calls the group refresh API for every
  account whose membership changed, and cuts the sessions that cannot be brought in line. What a
  pass nobody asked for corrects is recorded as drift and shown in the administration area. The
  directory and TAK Server sit behind two traits (`Directory`, `TakServer`), each with an in-memory
  implementation used by the unit tests.

### Authorisation rule

One function decides everything (`policy::can`), unit-tested:

| | Platform admin | Team leader | Member | No team |
|---|---|---|---|---|
| Approve, suspend, set the cap, see the drift report | yes | | | |
| See the team | yes | yes | yes | |
| Invite, remove a member, change a role | | yes | | |
| Change the mode, delete the team | yes (delete only) | yes | | |
| Use the shared access (QR, zip, credentials) | | yes | yes | |
| Use an individual access | | yes | their own | |
| Use an external access | | yes | | |
| Regenerate or remove an access | | yes | their own individual one | |

A team the caller may not see answers "not found", so its existence does not leak.

## 3. What was verified, and how

### Automated tests

| Suite | Command | Result |
|---|---|---|
| Rust unit tests | `scripts/test-unit.sh` | 61 tests pass (52 in `core`, 9 in `server`) |
| Web checks | `scripts/test-unit.sh` | translation check, lint, type check pass |
| Integration tests on the real stack | `scripts/test-integration.sh` | 14 tests pass, about 45 seconds |

The unit tests cover names (team slug, usernames, e-mail), the authorisation table, login-code
rules, the secret store, the certificate request, zips and PKCS#12 stores in both formats, e-mail
texts, and the reconciliation against in-memory backends: idempotence, drift repair, revoke before
delete, deletion postponed while TAK Server is down, session cuts, the mass-deletion guard.

The integration tests are black-box: they call the platform's HTTP API through Caddy, read login
codes in the mail catcher, read OpenLDAP from inside its container, and connect a scripted TAK
client (mutual TLS on 8089, one position per second, every received event recorded) with the
certificate and the truststore taken out of the downloaded zips. What they prove:

| Test | Proves |
|---|---|
| `login_by_one_time_code` | Code by e-mail in the page's language; wrong code refused; code usable once; logout |
| `emails_follow_the_language_saved_on_the_account` | E-mails use the recipient's saved language |
| `login_codes_are_rate_limited_per_address` | Sixth code in an hour refused and no e-mail sent; a code dies after 5 wrong guesses |
| `state_changing_requests_need_the_csrf_header` | A cross-site form post is refused |
| `an_account_without_a_team_can_do_nothing` | Other teams and accesses answer "not found"; a pending request gives nothing and creates no group |
| `team_request_approval_and_invitations_within_the_cap` | The whole path; only an administrator approves; invitations stop at the cap; only the invitee can accept; a member manages nothing; the last leader cannot leave |
| `a_rejected_request_frees_its_name` | Rejection with a note, e-mailed; the name can be requested again |
| `zips_carry_only_the_connection` | Exact file list of both zips; only connection keys in `config.pref`, no callsign, colour, role or group; QR strings; certificate issued by enrollment for 365 days and its expiry recorded; the same certificate on a second download; account in its group in OpenLDAP with a hashed password |
| `members_of_a_team_see_each_other_and_another_team_does_not` | **Two members of a team receive each other's positions; a client of another team receives nothing from them and is not received**; an external access joins the team's group; a member cannot reach another member's access |
| `regenerating_a_shared_identity_cuts_every_old_certificate` | Three devices on one shared identity (two with the zip, one enrolled with the credentials), no eviction; **after regeneration all three are disconnected within seconds, the old certificates are refused, including one the device had replaced, the old password no longer enrolls, and the new zip works**; only a leader can regenerate |
| `removing_an_access_revokes_then_deletes` | A removed member is disconnected, refused afterwards, and gone from OpenLDAP; a new access gets a new username; deleting a team removes its group and accounts |
| `changing_the_mode_removes_the_accesses_that_do_not_fit` | Shared access revoked on a switch to individual; external access kept |
| `reconciliation_repairs_drift_and_reports_it` | A member slipped into another group, a stray account and group, and a changed password, all written straight to OpenLDAP, are repaired by one pass and reported as drift; the next pass changes nothing |
| `suspending_a_team_cuts_its_sessions_and_reactivating_restores_them` | Suspension cuts the sessions (last-group case) without revoking; a reconnecting device is alone; reactivation cuts the group-less sessions so they come back with their group |

### Checked by hand in the running UI

With the stack started by the install script, in the app's browser pane:

- **English:** sign-in by code read in the mail catcher; empty home page of an account without a
  team; team request (shared identity); sign-out; administrator sign-in with the rescue code;
  approval with a member cap of 6; the "TAK server sync" tab (reachability, drift count, journal);
  leader sign-in; invitation of a member with a French e-mail; creation of the shared access; the
  three tabs of the access dialog (QR codes, zips, credentials); regeneration with its confirmation.
- **French:** the invitation e-mail and the login code arrive in French; sign-in; accepting the
  invitation; the team page and the access dialog as a member (no regenerate or remove button);
  switching an existing account to French from the menu (saved on the account); creating an
  external access; regenerating it.

Dates and durations follow the language ("Nov 19, 2027 (in 12 months)" / "19 nov. 2027 (dans
12 mois)").

One thing was not clicked: the two "Download" buttons, because the browser pane would have saved
files on the user's machine. The same request was made from the page with the signed-in session and
returned the zip (200, `application/zip`, 9.9 kB); the zips themselves are covered by the
integration tests.

This walk-through found four defects that the API tests could not see, all fixed: sign-out left the
previous account on screen; the two steps of the sign-in form shared their buttons, so "I already
have a code" sent a code request and lost the focus; rescue codes counted in the per-address limit;
a handler that takes no request body could reset the connection when one was sent.

### Other checks

- **Install from nothing.** `deploy/scripts/clean.sh --yes --images` followed by
  `./install.sh --local`: images rebuilt from the ZIP, certificates generated, stack healthy, the
  platform reaching the directory and TAK Server, administrator created, in 189 seconds (46 s and
  95 s for the two TAK Server images, with Docker's build cache warm; the very first build takes
  several minutes more). The three suites above were then run on that fresh stack and passed.
- **Running the install again** on a running stack keeps settings and certificates and takes about
  30 seconds.
- **Supervisor.** One of TAK Server's five Java processes was killed by hand: the container exited
  within 15 seconds, Docker restarted it, it was healthy again in under a minute, and the platform's
  administrator certificate was still accepted without running `UserManager.jar` again.
- **No secret or ZIP content versioned.** `git status` before the commit: section 8.

## 4. What was not verified

| Not verified | Why | What it would take |
|---|---|---|
| **Phones.** That ATAK and iTAK import each zip, scan each QR code, enroll with the credentials, show the team as a channel, and react to a regeneration | No device | The procedure of phase 0, section 12, with the platform's zips and QR codes instead of the spike's |
| **Let's Encrypt on port 8446** (`--tak-enroll-letsencrypt`, `deploy/scripts/sync-letsencrypt.sh`): conversion to JKS, the connector attribute, the restart on renewal, the cron entry | No domain | A production install on a machine the domain points to. The script is written from the official guide (appendix E) and was never run. Two points to watch: Caddy's default key type is ECDSA, and whether TAK Server's Tomcat accepts it in a JKS is unknown; and the platform then reaches 8446 under the public name through a Docker network alias |
| **Production mode as a whole** (`--domain`): Caddy with Let's Encrypt on 443, `COOKIE_SECURE=true` | No domain | Same install. Only `docker compose config` was run on the production files |
| **amd64 Linux** | Mac only | The install on the VPS. Nothing depends on macOS: the scripts are plain bash (written for 3.2), file ownership is set from a container as root (`fix_ownership`, uid 1001 for TAK Server and 10001 for the platform), and the script refuses to run without root on Linux. None of this ran on Linux |
| **Real SMTP** (Brevo, STARTTLS, authentication) | No credentials | Only the unencrypted mail catcher was used. The `starttls` and `tls` settings are code paths that never ran |
| **Per-IP login limits** | Running them locks the test machine's address out | Unit-tested only. The limit on wrong codes was seen working once by accident: replaying the tests reached 30 wrong codes in an hour and every login was refused, which is why local mode raises the two per-IP limits |
| **`P12_FORMAT=legacy`** on a phone | No device | The stores are written and read back in both formats by a unit test; whether old Android needs it is still the open question of phase 0 |
| **Scale** | Not in scope | The tests used at most six simultaneous clients and about 70 TAK accounts |
| **`ldaps://`** | Decided for later | Known limitation, below |

## 5. Findings about TAK Server

### 5.1 The "active" certificate list

`GET /Marti/api/certadmin/cert/active`, which phase 0 proposed for monitoring, does not mean "not
revoked". Observed on the running server:

```
== active
1  e2e-39qzpc-shared  issued 15:49:32  revoked 15:50:15   ← revoked, still listed
8  e2e-wnxfrr-shared  issued 15:50:50  revoked 15:50:15   ← revoked, still listed
9  e2e-wnxfrr-shared  issued 15:49:53  revoked —
== revoked
1, 7, 8, 11, 12
```

Certificate 7, revoked too, is absent from "active" because certificate 9 was issued later for the
same user and device. The list is "the latest certificate of each user and device". Two
consequences, both met during the tests:

- the platform kept revoking certificate 1 at every pass, as it never left the list;
- a certificate that a device replaces by enrolling again disappears from the list **but still
  connects** (asserted by the regeneration test). A regeneration based on that list would have left
  it valid.

The platform therefore reads `GET /Marti/api/certadmin/cert` (every certificate) and keeps those
with no `revocationDate` and an `expirationDate` in the future. That list only grows: about 1.7 kB
per certificate ever issued, read every minute. Deleting a record
(`DELETE /Marti/api/certadmin/cert/delete/{ids}`) was not tried; since revocation is checked against
this table, deleting a revoked record before its certificate expires may well make the certificate
valid again.

### 5.2 Sessions and groups

| Change in the directory | Effect on an open session |
|---|---|
| A group added or removed, the account keeping at least one | Applied (within a second with the refresh API) — phase 0 |
| The last group removed | Not applied: the session keeps its groups — phase 0 |
| A first group given to an account that had none | **Not applied**, even after 150 seconds and several refresh periods — phase 1 |

For the last two the platform cuts the session (`DELETE /Marti/api/subscriptions/delete/{uid}`); a
TAK client reconnects by itself and gets the right groups. Two details of
`GET /Marti/api/subscriptions/all` matter here:

- a session that has no group is listed **without `username` or `dn`**, so it cannot be matched to
  an account. Such a session sees nobody and is seen by nobody; the platform cuts all of them
  whenever a pass has changed something, and leaves them alone otherwise;
- the list keeps entries of connections that are long gone (seen seven minutes after the client
  closed), and answers 400 to a request to cut one. Failing to cut a group-less session is therefore
  not treated as an error.

For phase 2: adding and removing an event group works on open sessions, as members always keep
their team group. A device connected while its team was suspended is the only case that needs the
cut.

### 5.3 Certificate validity in the ZIP's scripts

`makeCert.sh` signs everything with `-days 730`, the intermediate CA included. With client
certificates valid one year, those issued after the first year would stop working when the CA
expires, and every device at once at two years. The install runs a copy of the script patched with
`sed` inside a throwaway container (validity as a variable, several names in the server
certificate) and checks the result; it fails rather than continue if the script no longer has the
lines it patches. Resulting validity: root CA 10 years (the ZIP's default), intermediate CA
10 years, TAK Server's certificate and the platform's API certificate 2 years.

The server certificate carries `takserver`, `localhost`, `127.0.0.1`, the public host and the
Docker network alias, because the platform reaches it as `takserver` and phones by its public name.

### 5.4 The database image and the name `tak-database`

The database image creates TAK Server's tables at startup with `SchemaManager.jar`, which reads the
connection from the ZIP's `CoreConfig.example.xml`: host `tak-database`. With the service named
`takserver-db` as in the plan, the tool failed silently ("Failed to initialize pool") and TAK
Server started on an empty database: every API call answered 500 with `relation "certificate" does
not exist`, while the container was reported healthy. The service now carries the network alias
`tak-database`, and its healthcheck waits for the `certificate` table.

### 5.5 Smaller points

- TAK Server's healthcheck (401 on 8446, 8443 and 8089 open) can pass a few seconds before the API
  accepts the platform's certificate; the install retries `tak-manager status` for two minutes.
- `UserManager.jar certmod -A` rewrites `UserAuthenticationFile.xml` in place, so the file can be a
  single bind mount, and TAK Server picks the change up without a restart.
- Both ZIP images are built by the install without the certificate-authority image of phase 0: the
  TAK Server image has `openssl`, `keytool` and the certificate scripts.

## 6. Deviations from the plan and from the brief

All are recorded in `PLAN.md`.

| Subject | Plan or brief | What was done, and why |
|---|---|---|
| Exposed ports | 443, 8089, 8443, 8446 | 8443 not published (decision given for this phase). Port 80 is published in production for the ACME challenge |
| Connection modes | Three modes | Three modes as planned; in addition, external accesses can be created in the shared and individual modes, since a guest should not get the team's shared identity or need a platform account |
| Creation of accesses | Not specified | Explicit (a button), not automatic: no TAK account exists before someone needs it |
| Administrator and team secrets | Not specified | An administrator who is not a member of a team cannot see its TAK credentials. They can still suspend or delete the team |
| Suspension | Not in the plan | Added: it is the administrator's way to cut a team off without destroying it, and it is the real-life case of "an account left with no group" |
| Several leaders | "Team leader" | A leader can promote a member; a team always keeps one leader. Without this a leader could never hand over |
| E-mail language for a recipient with no account | "Recipient's saved language, English by default" | Login code: the language of the page it was requested from. Invitation: chosen by the inviter, their own by default. English only when nothing is known. A saved preference always wins |
| E-mail texts | Not specified | Stored next to the UI texts and compiled into the server; a new language is one folder and one entry in the language list |
| LDAP lockout | Phase 0 had a lockout | Removed (decision given for this phase) |
| Intermediate CA validity | `makeCert.sh ca` | 10 years instead of the script's 2 (section 5.3) |
| LDAP base DN | `dc=tak,dc=example` in the spike | `dc=takmanager,dc=internal` |
| Host agent | Listed among the services | Not built: it belongs to phase 3. The Let's Encrypt hand-over to TAK Server, which needs to restart a container, is a host script run by cron for now |
| TypeScript | Not specified | Pinned to 6.x: the lint tooling does not support 7 yet |

Not done, and not asked for in this phase, but worth knowing: there is no way to delete a platform
account, to rename a team, or to see which devices are connected; there is no audit log beyond the
server's log lines (one per action, with the acting account).

## 7. Known limitations

- **LDAP traffic is not encrypted** between the platform, TAK Server and OpenLDAP. It stays inside
  the Docker network, where TAK passwords travel in clear at each write and each enrollment.
- **TAK passwords and zip certificates are stored recoverably**, encrypted with `APP_SECRET`
  (AES-256-GCM), because the platform shows them again. Whoever holds the database and `deploy/.env`
  holds every TAK access. Losing `APP_SECRET` makes them unreadable; the reconciliation then stops
  rather than guess.
- **The ATAK QR code carries the password** in clear. The UI says so.
- **QR codes need a public certificate on 8446**; without `--tak-enroll-letsencrypt` only the zips
  can work. The UI says so next to the codes.
- **Regeneration needs TAK Server.** If it cannot be reached nothing is changed and the user is told
  to try again. Removal does not: the revocation is applied by the next pass that reaches TAK
  Server; until then the account has already lost its group.
- **Drift is a heuristic.** A change made by a pass that follows a platform action is not drift;
  anything else is. A pass triggered by a platform action can therefore sweep up unrelated drift
  without flagging it.
- **Mass-deletion guard.** An automatic pass that finds more than three directory accounts unknown
  to the database changes nothing and reports an error, so an empty or wrongly restored database
  does not wipe TAK Server. A pass asked for by an administrator has no such limit.
- **E-mails are sent during the request.** A slow mail server slows the sign-in page down; there is
  no queue or retry. A failed invitation e-mail is reported to the leader; the invitation itself
  still waits for the invitee.
- **The client's IP address** is taken from the header Caddy sets. Behind another proxy or a CDN the
  per-IP limits would apply to that proxy's address.

## 8. Repository hygiene

Everything generated lives in `deploy/.env` and `deploy/data/` (certificates, keys, TAK Server
configuration with its passwords, logs), both git-ignored. The ZIP is extracted to
`deploy/data/build/` for the time of the build and removed. Before the commit, the list of files to
be added was checked for the ZIP, anything extracted from it, `.env` files, keys, certificates and
keystores (`*.zip`, `*.p12`, `*.jks`, `*.pem`, `*.key`, `deploy/data/`, `spike/work/`): none.

State of the development machine at the end of the phase: the containers are stopped
(`docker compose down`); the Docker volumes, `deploy/.env` and `deploy/data/` are kept, so
`cd deploy && docker compose up -d` brings back the stack as tested. Images kept:
`tak-manager/takserver`, `tak-manager/takserver-db`, `tak-manager/app`, and the three `tak-spike/*`
images of phase 0 (7.5 GB, removable with `spike/scripts/99-clean.sh --images`). No container, image
or volume of another project was touched.

## 9. For phase 2 (cross-team events)

- **Where groups come from.** `Reconciler::desired()` in `crates/server/src/reconciler.rs` builds,
  for each TAK account, the set of groups it must be in: today the team's slug when the team is
  active. An event adds its group to the accounts of the participating teams there; everything else
  (directory writes, refresh, cuts, drift) follows without change.
- **Group names.** Team slugs cannot start with `evt-` (`names::RESERVED_SLUG_PREFIXES`): the prefix
  is free for event groups. A group exists in the directory only while it has members, so creating
  and removing an event group is just adding and removing members.
- **Timing.** A pass runs after each API change and every 60 seconds. Activation and removal at a
  scheduled date need either a pass triggered at that date or accepting up to a minute of delay.
- **Open sessions.** Adding and removing an event group reaches connected clients without a
  reconnection (section 5.2). The shared identity of a team is one account: an event applies to the
  whole team, never to some of its devices.
- **Building blocks to reuse:** the authorisation function (`policy::can`), the API error list
  (`error.rs`; a new code must be translated in both languages or a test fails), e-mail templates
  (`locale.rs` and `web/src/locales/*/email.json`), the i18n check (`pnpm check:i18n`), and the
  integration-test helpers (`crates/server/tests/common/`: sessions, teams, zips, the TAK client).

## 10. For phase 3 (monitoring)

- **Client certificates.** `tak_certificates` holds every certificate the platform knows, with
  `expires_at`, its source (`package`: shipped in a zip; `device`: enrolled by a device and found by
  the reconciliation), and `inactive_at`. An index on `expires_at` exists for the alert query. A
  certificate in a zip is not renewed by anything: when it nears expiry the access must be
  regenerated and the zip handed out again.
- **Other certificates**, read from files under `deploy/data/tak/certs/`: root CA (2036),
  intermediate CA (2036), TAK Server's (2 years), the platform's API certificate (2 years; its
  renewal also needs `UserManager.jar certmod -A`), and Let's Encrypt in `deploy/data/caddy/`.
- **Use `certadmin/cert`, not `certadmin/cert/active`** (section 5.1), and expect that list to grow.
- **Reconciliation state.** `reconcile_status` (last run, last run without error, reachability of
  the directory and of TAK Server, last errors) and `reconcile_events` (kept 90 days). The API is
  `GET /api/admin/reconciliation`; `tak-manager status` gives a one-shot check from the command
  line.
- **Processes and health.** The `takserver` entrypoint exits when a Java process dies; every service
  has `restart: unless-stopped` and a healthcheck. TAK Server's healthcheck can be green slightly
  before the API is usable.
- **What a backup must hold:** the Docker volumes `postgres-data`, `ldap-config`, `ldap-data`,
  `tak-db-data`, the folder `deploy/data/` (the certificate authority) and `deploy/.env`
  (`APP_SECRET`). Restoring the platform's database alone, older than the directory, makes the next
  manual pass delete what the old database does not know; the automatic guard of section 7 is there
  for that case.
- **Audit log.** There is none yet. Every action already produces one log line with the acting
  account (`docker compose logs app`), which is a starting point, not a substitute.
- **Host agent.** The platform has no access to the Docker socket. The one host-side job that exists
  is `deploy/scripts/sync-letsencrypt.sh` (cron); it is a natural first task for the agent.
- **Logs** of TAK Server are in `deploy/data/tak/logs/` and are not rotated by this project.

## 11. Left for the user

1. The phone checks (phase 0, section 12), now with the platform: both zips and both QR codes on
   ATAK and iTAK, then a regeneration while connected.
2. A production install on the amd64 VPS with the domain and Brevo, with
   `--tak-enroll-letsencrypt`, and a QR enrollment from a phone.
3. Decide whether LDAP should be encrypted inside the Docker network.
