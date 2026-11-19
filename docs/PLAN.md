# TAK Manager — specification and roadmap

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
`takserver`, `takserver-db`, `agent` (host, phase 3). Local mode adds `mailpit` (mail catcher).

- **Source of truth**: the platform's PostgreSQL database. An idempotent reconciliation loop compares
  the desired state with the actual state of the directory (and of TAK Server for certificates) and
  fixes any drift. Detected drift is visible in the admin area.
- **Groups**: written to OpenLDAP by the platform; TAK Server re-reads them by itself (`updateinterval`
  on the `<ldap>` element) and pushes them to already-connected clients. No group is ever baked into a
  zip. After each write the platform calls `GET /Marti/api/groups/update/{username}`: the change then
  applies in under a second and `updateinterval` is only a safety net.
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
  clients, TLS) and 8446 (TAK enrollment). **8443 (TAK Server API) is not published**: the platform
  reaches it over the Docker network (decided in phase 1).

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

## 4. Functional scope

### Accounts and approval
- Passwordless login to the platform: one-time code sent by e-mail, short-lived, rate-limited per
  address and per IP. No Google login.
- Anyone can sign in, but an account without a team can do nothing and has no TAK access.
- The administrator approves team creation requests only.
- An approved team leader invites members without administrator approval, up to a per-team member cap.
- A server-side rescue command grants administrator access if e-mail is down: it flags the account
  as administrator and prints a one-time login code (`tak-manager grant-admin <email>`).
- Roles: platform administrator, team leader, member. A team can have several leaders and always
  keeps at least one. An administrator runs the platform (approval, member cap, suspension,
  deletion) but does not see a team's TAK credentials unless they are a member of it.
- An administrator can suspend a team: its accounts are kept but lose their TAK group, and their
  sessions are cut, until the team is reactivated.

### Teams and connecting to TAK Server
One team = one TAK group, named after the team (lowercase, accents folded; the `evt-` prefix is
kept for events). The team leader picks the mode, and can change it later (accesses that do not fit
the new mode are revoked):
- **Shared identity**: a single TAK account and a single zip for the whole team. "Regenerate" revokes
  the certificate and produces a new zip. The risk (lost phone, departing member) is accepted by the
  team. Limit found in phase 0: channel state belongs to the account, so one device switching a channel
  off switches it off for the whole team.
- **Individual accounts**: one access per person, revocable one by one.
- **External accounts**: TAK access for a person who has no account on the platform. As a mode, the
  team hands out external accesses only; external accesses can also be added in the two other modes.

An access is created explicitly (by the leader, or by a member for their own individual access), not
automatically, so that no TAK account exists before it is needed.

Each access can be handed over in three forms:
- QR code (iTAK: `name,host,8089,ssl`; ATAK:
  `tak://com.atakmap.app/enroll?host=…&username=…&token=…`, the password travels inside the QR; both
  still have to be validated on real phones);
- username and password (manual enrollment);
- zip (data package) containing only the server address, the truststore and, optionally, the client
  certificate. **Never** a callsign, team colour, role or group in the zip.

### Cross-team events
A team creates a dated event (start, end) and invites other teams. On acceptance, the platform creates
a dedicated TAK group and adds the members of all participating teams to it; the group is removed at
the end. Outside events, teams do not see each other.

### Monitoring (administrator)
- VPS: CPU, RAM, disk, pending system updates, reboot required.
- Containers: state, versions, images to update.
- TAK Server: ports, connected clients, installed version (new version: semi-manual check, tak.gov has
  no public API).
- Certificates: expiry of the CA, the server certificate, Let's Encrypt, client certificates.
- Backups, audit log, alerts by e-mail or webhook.

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

### Phase 2 — cross-team events (report `docs/phase-2-report.md`)
Creation, team invitations, acceptance, temporary TAK group, automatic activation and removal at the
scheduled dates.

### Phase 3 — monitoring (report `docs/phase-3-report.md`)
Host agent, administrator dashboard, alerts, updates, backup and restore, audit log.

### Phase 4 — extensions (not started, to be confirmed)
CloudTAK wired to the same accounts, MediaMTX video, a Data Sync mission per event, per-team
distribution of maps and plugins, federation.

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
