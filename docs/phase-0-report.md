# Phase 0 report — technical validation

Date: 2026-11-19. Scope: section 5, "Phase 0", of [`PLAN.md`](PLAN.md). Everything described here
lives in [`spike/`](../spike/README.md) and can be replayed.

Environment: Mac arm64, Docker 29.8 (OrbStack, 14 CPUs, 16 GB for the VM), TAK Server
`5.8-RELEASE-103` built from `takserver-docker-hardened-5.8-RELEASE-103.zip`, OpenLDAP 2.6.10
(`vegardit/openldap`). No ATAK or iTAK device was available: TAK clients were simulated by scripts.

## 1. Summary

| # | Question | Answer | How it was established |
|---|---|---|---|
| 1 | Does the hardened ZIP build without Iron Bank? | **Yes.** Three images built with no registry login. One network workaround was needed on this machine. | Built twice from a fresh extraction |
| 2 | A working OpenLDAP + `CoreConfig.xml` | **Yes.** `inetOrgPerson` users, `groupOfNames` groups, memberOf overlay, two service accounts. | 16 scripted checks |
| 3 | Group change on a connected client | **Yes, without reconnecting.** At most `updateinterval` seconds; under 1 s if the platform calls a refresh API. **Exception: removing a user's last group is not applied to an open session.** | Timed over 23 trials |
| 4 | Enrollment by CSR, revocation, channels | **Yes** to all three. Revocation cuts the open connection within 0.1 s. Enrolled certificates support channels with the default settings. | 19 scripted checks |
| 5 | Several clients on one certificate | **Yes.** Three devices at once, no eviction. Channel state is shared by the account. | 10 scripted checks |
| 6 | Minimal connection zip, ATAK QR format | Zip: 3 files (enrollment) or 4 (with certificate), produced by the ZIP's own script. QR formats identified from public sources. **Import on real phones not verified.** | Content checked by script; phones needed |
| 7 | "Manage LDAP Users" UI | **Works only if** TAK Server's service account can write and sits directly under the base DN. Not needed by the platform. | API tested end to end; page rendering not tested |
| 8 | Monitoring APIs | Version, health, connected clients, load, database, certificates are all available on 8443 with an administrator certificate. | Queried on the running server |

Three results change what phase 1 must build, beyond what the plan said:

1. **Removing a user's last group does not reach an open session** (question 3). The platform must
   cut the session or revoke the certificate in that case.
2. **The platform can make group changes immediate** with `GET /Marti/api/groups/update/{username}`
   (question 3). `updateinterval` becomes a safety net.
3. **QR enrollment needs a publicly trusted certificate on port 8446** according to the official guide
   (question 6). Port 8446 is served by TAK Server, not by Caddy, so the Let's Encrypt certificate has
   to be given to TAK Server as well. Not tested here.

## 2. What was built

`spike/docker-compose.yml` starts three services: `openldap`, `tak-database`, `takserver`. The steps
are in [`spike/README.md`](../spike/README.md). Nothing from the ZIP is copied into a versioned
location; the extraction, certificates and passwords are generated under `spike/work/` (git-ignored).

The whole procedure was replayed from a freshly extracted ZIP, with images rebuilt and volumes,
secrets and certificates deleted first. TAK Server reached `healthy` about 30 seconds after
`docker compose up -d`. The last run of `tests/run-all.sh`, on a stack re-created from a clean state
with the files as they are in the repository, passed all 9 scripts (106 checks).

### How the tests work, and their limit

`spike/tests/taklib.py` implements a minimal TAK client: mutual TLS on 8089 with a client certificate,
one position (CoT XML) sent per second, every received message recorded with its arrival time. Each
question has its own script; `spike/tests/run-all.sh` replays them all.

This proves what the **server** does: who receives whose traffic, when a change takes effect, when a
connection is cut. It does not prove what the ATAK and iTAK **apps** do with a zip, a QR code or a
channel notification. Section 12 separates the two and gives a procedure for the phone-only checks.

## 3. Question 1 — building the ZIP without an Iron Bank account

**Answer.** The three images build with no Iron Bank account and no `docker login`. The README of the
ZIP is out of date on this point: the Dockerfiles pull public images.

**Evidence.**

- Base images named in the Dockerfiles: `registry.access.redhat.com/ubi10/ubi:10.0` (CA),
  `docker.io/library/eclipse-temurin:17.0.19_10-jdk-ubi10-minimal` (TAK Server),
  `registry.access.redhat.com/ubi10/ubi:10.2` (database). Packages come from Adoptium, Rocky Linux,
  EPEL and the PostgreSQL repository, all public.
- `~/.docker/config.json` on the build machine holds no Iron Bank entry.
- Builds from a freshly extracted ZIP, Dockerfiles untouched (`spike/scripts/01-build-images.sh`):

  ```
  == Build tak-spike/ca:5.8-RELEASE-103 (Dockerfile.ca)
  OK in 43 s — arm64 1186530251 bytes
  == Build tak-spike/takserver:5.8-RELEASE-103 (Dockerfile.hardened-takserver)
  OK in 37 s — arm64 2442147933 bytes
  == Build tak-spike/takserver-db:5.8-RELEASE-103 (Dockerfile.hardened-takserver-db)
  OK in 117 s — arm64 3874664855 bytes
  ```

- A checksum of every file of the extracted tree is identical before and after the builds.

**One problem met: truncated package downloads.** The database image failed twice at
`dnf install`:

```
[MIRROR] llvm-libs-22.1.8-1.el10.aarch64.rpm: Curl error (18): Transferred a partial file for
http://dl.rockylinux.org/pub/rocky/10/AppStream/aarch64/os/Packages/l/llvm-libs-22.1.8-1.el10.aarch64.rpm
Error: Error downloading packages
```

The files `tak/security/rpms/repos/*.repo` of the ZIP point to `http://dl.rockylinux.org` in plain
HTTP. From this network, a 30 MB download over HTTP from that host was cut after about 5 seconds in
7 attempts out of 7 (host and container alike), while the same file over HTTPS completed in 6 of 8
attempts, and the build over HTTPS passed on its first run. This is a property of this network path, not of the ZIP, and it has nothing to do with Iron
Bank. The TAK Server image is not affected because it only downloads small packages.

Workaround, opt-in: `ROCKY_REPOS_HTTPS=1 scripts/01-build-images.sh db` switches the three `.repo`
files to `https://` for the duration of the build, in the extracted copy, then restores them.

**On an amd64 Linux VPS** (not built there; no such machine in this session):

- The three base images and `vegardit/openldap` publish `amd64` manifests (checked with
  `docker manifest inspect`). The Adoptium, Rocky and PostgreSQL repositories answer for `x86_64`.
- `Dockerfile.hardened-takserver-db` maps `TARGETARCH=amd64` to `x86_64` itself. Nothing in the three
  Dockerfiles is specific to arm64.
- File ownership will matter on Linux and did not on the Mac: the TAK Server container runs as uid
  1001, gid 0. Bind-mounted directories (certificates, logs, `CoreConfig.xml`) must be writable by
  that user.
- Memory: `setenv.sh` sizes the Java heaps from the machine's RAM. On 8 GB that gives about 1.3 GB
  each for the API and messaging processes and 0.8 GB for the three others, about 3.4 GB of heap at most.
  The variables `API_MAX_HEAP`, `MESSAGING_MAX_HEAP`, `CONFIG_MAX_HEAP`, `PLUGIN_MANAGER_MAX_HEAP`,
  `RETENTION_MAX_HEAP` (MB) override this. On the 16 GB test VM the container used 3.75 GiB.
- Disk: the three images take 7.5 GB unpacked.

**Consequences for phase 1.**

- The install script builds from the ZIP with no registry account.
- It must carry the HTTPS workaround, or retry, for the database image.
- It must set ownership of the mounted directories for uid 1001 and decide heap sizes for the VPS.
- `TAKIgniteConfig.xml` must exist before the first start (section 11).

## 4. Question 2 — OpenLDAP and `CoreConfig.xml`

**Answer.** The configuration in `spike/config/CoreConfig.tpl.xml` and `spike/ldap/` works: TAK
clients get their groups from the directory through the CN of their certificate, teams are isolated,
and LDAP passwords authenticate enrollment.

### How TAK Server reads the directory

This was read from the server's code (`LdapAuthenticator`, disassembled with `javap`) and confirmed
in the OpenLDAP log. With `style="DS"`, TAK Server does not search groups. It reads **the `memberOf`
attribute of the user's own entry**, whose DN is `userstring` with `{username}` replaced:

```
BIND dn="cn=takserver,ou=Services,dc=tak,dc=example"
SRCH base="uid=alice,ou=People,dc=tak,dc=example" scope=0 filter="(objectClass=*)"
```

Consequences: OpenLDAP must maintain `memberOf` (overlay), `userstring` must be a full DN, and
`groupObjectClass` / `groupBaseRDN` play no part in group lookup in this mode. The TAK group name is
extracted from the group DN: `cn=team-alpha,ou=Group,…` becomes the TAK group `team-alpha`.

### Directory layout

| Item | Choice |
|---|---|
| Users | `uid=<username>,ou=People,<base>`, object classes `inetOrgPerson`, `organizationalPerson`, `person`, `top` |
| Groups | `cn=<group>,ou=Group,<base>`, object class `groupOfNames`, attribute `member` |
| memberOf overlay | On `groupOfNames` / `member` (the image's default is `groupOfUniqueNames` / `uniqueMember`) |
| TAK Server account | `cn=takserver,ou=Services,<base>`, read-only on `ou=People` and `ou=Group`, cannot read passwords |
| Platform account | `cn=tak-manager,ou=Services,<base>`, read-write on `ou=People` and `ou=Group`, including passwords |
| Users | Can read only their own entry (TAK Server binds as the user during enrollment) |
| Passwords | Hashed by OpenLDAP with Argon2; no forced change, no expiry, lockout after 10 failures in 5 minutes |

These classes are the ones TAK Server itself uses when it creates entries (`userObjectClasses` and
`groupObjectClasses` defaults in `CoreConfig.xsd`). A `groupOfNames` must have at least one member:
an empty group cannot exist.

### `CoreConfig.xml`

```xml
<input _name="stdssl" protocol="tls" port="8089" coreVersion="2" auth="x509"/>
<connector port="8443" _name="https"/>
<connector port="8446" clientAuth="false" _name="cert_https"/>

<auth default="ldap" x509groups="true" x509addAnonymous="false"
      x509useGroupCache="true" x509useGroupCacheDefaultActive="true"
      x509useGroupCacheDefaultUpdatesActive="true"
      x509useGroupCacheRequiresExtKeyUsage="true" x509checkRevocation="true">
  <ldap url="ldap://openldap:389"
        userstring="uid={username},ou=People,dc=tak,dc=example"
        updateinterval="30" style="DS" ldapSecurityType="simple"
        serviceAccountDN="cn=takserver,ou=Services,dc=tak,dc=example"
        serviceAccountCredential="…"
        userObjectClass="inetOrgPerson" groupObjectClass="groupOfNames"
        userBaseRDN="ou=People,dc=tak,dc=example" groupBaseRDN="ou=Group,dc=tak,dc=example"/>
  <File location="UserAuthenticationFile.xml"/>
</auth>

<certificateSigning CA="TAKServer">
  <certificateConfig><nameEntries>
    <nameEntry name="O" value="TAK"/><nameEntry name="OU" value="TAKManager"/>
  </nameEntries></certificateConfig>
  <TAKServerCAConfig keystore="JKS" keystoreFile="/opt/tak/certs/files/<intermediate>-signing.jks"
                     keystorePass="…" validityDays="30" signatureAlg="SHA256WithRSA"/>
</certificateSigning>
```

`validateConfig.sh` from the ZIP accepts the file ("CoreConfig.xml validates"). The LDAP URL carries
no base DN, so every DN in the element is a full DN. `groupprefix` is left empty: every group under
`ou=Group` is a TAK group. Certificates: a root CA, then an intermediate CA made with
`makeCert.sh ca`, which signs the server certificate and the enrollment certificates. The truststore
handed to clients contains both.

### Choice of the OpenLDAP image

`vegardit/openldap:2.6.10`. Checked on Docker Hub on 2026-11-19:

| Image | Last update | Verdict |
|---|---|---|
| `vegardit/openldap` | 2026-11-18, rebuilt weekly on `debian:trixie-slim`, amd64 + arm64 | **Chosen.** memberOf, ppolicy and Argon2 ready; documented mount points for tree, entries and policy |
| `osixia/openldap` | `latest` dates from 2021; only alpha/beta/rc tags since | Not a stable maintained release |
| `bitnami/openldap` | 2025-08, moved to a "legacy, no longer updated" repository | Abandoned |
| `nfrastack/openldap` | 2026-10 | Maintained; not evaluated further once the first candidate worked |

Two caveats. The image is maintained by one company's team, so phase 1 should pin it by digest. And
two of the five files the spike mounts over the image's defaults (`init_module_memberof.ldif`,
`init_mdb_acls.ldif`) are not documented mount points: they work because the image's entrypoint
simply loads these paths, which could change. A fallback with no third party is a 15-line Dockerfile
on `debian:stable-slim` with the `slapd` package; the LDIF files of the spike would carry over.

**Evidence** (`tests/t2_ldap_groups.py`, 16 checks passed):

```
OK   TAK Server's service account reads alice's memberOf (memberOf overlay on groupOfNames)
OK   TAK Server's service account cannot read passwords
OK   TAK Server's service account is read-only
OK   a TAK user cannot read another user's entry
OK   no credentials: 401 / wrong password: 401 / user not in the directory: 401
OK   alice with her LDAP password: 200
OK   alice and charlie (team-alpha) see each other
OK   bob (team-bravo) does not see team-alpha and is not seen by it
OK   an account with no group connects but sees nobody and is seen by nobody (no __ANON__)
   groups as seen by TAK Server: {'bob': ['team-bravo'], 'alice': ['team-alpha'], 'nogroup': [], 'charlie': ['team-alpha']}
OK   8443 API with alice's certificate: only lists her own team ['alice', 'charlie']
```

**Two things that do not work as one might expect.**

- **`adminGroup` grants nothing to a certificate.** A user in the LDAP group named by `adminGroup`
  gets 403 on the admin API, with a certificate (8443) and with a password (8446). No caller of this
  setting was found in the server's classes. The administrator role comes from
  `UserManager.jar certmod -A <cert.pem>`, stored in `/opt/tak/UserAuthenticationFile.xml`.
- **LDAP traffic is not encrypted** in the spike (`ldap://` inside the Docker network). During
  enrollment the user's password travels in clear between the two containers. `ldaps://` is supported
  by the `<ldap>` element (`ldapsTruststore*` attributes) and was not tested.

**Consequences for phase 1.**

- Reuse `spike/ldap/` and the `<auth>` block as they are.
- The reconciler creates a group when it gets its first member and deletes it with its last.
- The platform's API certificate is made administrator once, at install, with `UserManager.jar`;
  `UserAuthenticationFile.xml` must be kept in a volume.
- Not tested: account lockout through repeated wrong passwords on 8446. With the policy above, ten
  wrong attempts lock a TAK account for five minutes. Since 8446 is exposed, this lets anyone who
  knows a username lock it out; phase 1 should decide whether to keep lockout or rate-limit instead.

## 5. Question 3 — group changes on a connected client

**Answer.** Yes, a change written to the directory reaches an already-connected client without any
reconnection. The delay is at most `updateinterval`, **which is in seconds** (the guide's attribute
table says milliseconds; the code calls `setUpdateIntervalSeconds`, and the measurements agree).

**Measurements** (`tests/t3_group_update.py`). alice and bob, in different teams, stay connected. The
script adds both to a new group, then removes bob, and times each effect on alice's stream.

| Setting | Adding to a group | Removing from a group | Trials |
|---|---|---|---|
| `updateinterval="30"` | 18.2 to 21.5 s | 25.2 to 25.5 s | 10 |
| `updateinterval="5"` | 0.4 to 4.8 s | 0.0 to 1.1 s | 4 |
| `updateinterval="30"` + refresh API call | 0.5 to 0.9 s | under 0.1 s | 9 |

In all 23 trials both clients stayed connected. The spread within a setting only reflects where the
change falls in the server's cycle; the bound is the interval itself.

How it works, from the OpenLDAP log: for each connected user, once per interval, TAK Server binds with
its service account and re-reads that user's `memberOf`. When the set of groups changes, the client
receives a control message on its stream:

```xml
<event how="h-g-i-g-o" type="t-x-g-c" version="2.0" uid="…" …><detail><link relation="p-p"/></detail></event>
```

This is the signal on which ATAK refreshes its channel list. What ATAK does on receiving it could not
be observed here.

**Forcing the refresh.** The admin API has `GET /Marti/api/groups/update/{username}`. Called right
after the LDAP write, it applies the change in under a second, as the third row shows.

**The exception: a user's last group.** `tests/t3b_last_group.py`:

```
OK   start: frank is in team-alpha ['team-alpha']
OK   team change alpha → bravo applied to the open session ['team-bravo']
OK   FINDING: removed from his last group, frank still sees team-bravo on his open session
     (groups according to TAK Server: ['team-bravo'])
OK   remedy: DELETE /Marti/api/subscriptions/delete/tls:11 cuts the session (0.0 s)
OK   after reconnecting, frank has no group left [] and sees nobody
```

When the directory returns no group at all for a user, TAK Server leaves the open session untouched,
with its old groups, for as long as it stays connected. Waiting several intervals or calling the
refresh API changes nothing. A new connection, however, correctly gets no group. Deleting the LDAP
account has the same non-effect on an open session (section 6).

**Consequences for phase 1.**

- After every directory write the platform calls `groups/update/{username}` for each affected user.
  `updateinterval` can then be relaxed to 60 seconds as a safety net.
- When a user ends up with **no group** (member removed from their only team, team deleted), the
  platform must also cut their sessions: list them with `GET /Marti/api/subscriptions/all`, then
  `DELETE /Marti/api/subscriptions/delete/{subscriptionUid}`. If the access itself is being withdrawn,
  revoking the certificate does both at once.
- For a cross-team event, members always keep their team group, so adding and removing the event
  group works on open sessions without this precaution.
- The reconciliation loop should treat "user has sessions but no group" as drift to fix.

## 6. Question 4 — enrollment, channels, revocation

**Answer.** A certificate can be enrolled by program with a CSR, it supports channels with the default
settings, and it can be revoked with immediate effect.

### Enrollment

Three steps, all on 8446 with HTTP Basic authentication using the LDAP username and password
(`enroll()` in `tests/taklib.py`):

1. `GET /Marti/api/tls/config` returns the name entries the server requires (here `O=TAK`,
   `OU=TAKManager`).
2. The client makes a key and a CSR whose CN is the username.
3. `POST /Marti/api/tls/signClient/v2?clientUid=…&version=…`, body: the CSR in base64 without the
   PEM header lines. The JSON reply holds `signedCert`, `ca0`, `ca1`.

```
subject : O=TAK, OU=TAKManager, CN=dave
issuer  : C=US, ST=XX, L=Spike, O=TAK, OU=TAKManager, CN=tak-spike-intermediate
validity: Nov 19 02:49:32 2026 GMT → Dec 19 14:49:32 2026 GMT
extended key usage: TLS Web Client Authentication, challengePassword
OK   dave cannot obtain a certificate in alice's name (CSR CN=alice → HTTP 500, subject issued: none)
```

The certificate lasts `validityDays` (30 in the spike, the value of the example file). The server
refuses a CSR whose CN is not the authenticated user.

### Channels and `x509useGroupCacheRequiresExtKeyUsage`

The "TAK-specific extended key usage" of the guide is `challengePassword`
(OID 1.2.840.113549.1.9.7), which enrollment adds and `makeCert.sh` does not. "Channels work" was
tested as: the client switches one of its groups off with `PUT /Marti/api/groups/active`, and traffic
from that group must stop.

| Certificate | `RequiresExtKeyUsage="true"` (default) | `RequiresExtKeyUsage="false"` |
|---|---|---|
| Enrolled through 8446 | Channel switches off and on | Channel switches off and on |
| Made by `makeCert.sh` | Connects, gets its LDAP groups, **but the switch has no effect** (API answers 200, traffic continues) | Channel switches off and on |

So the default value can stay, provided every certificate the platform hands out comes from
enrollment. That holds for zips too: the platform enrolls on the account's behalf and packs the
result.

One alternative was looked at and set aside: `GET /Marti/api/tls/makeClientKeyStore` on 8446 returns
a ready-made `.p12`, but its certificate lacks `challengePassword`, and the password of the `.p12`
travels in the URL.

### Revocation

`DELETE /Marti/api/certadmin/cert/revoke/{id}` on 8443 with an administrator certificate; the `id`
comes from `GET /Marti/api/certadmin/cert?username=…`, matched on the SHA-256 fingerprint (`hash`).

```
OK   DELETE /Marti/api/certadmin/cert/revoke/7 → 200
OK   dave's open connection is cut 0.0 s after revocation (connection closed by the server)
OK   new connection with the revoked certificate: refused or unusable
OK   8443 API with the revoked certificate: refused (500)
OK   the certificate shows up in GET /Marti/api/certadmin/cert/revoked
OK   regeneration: enrolling the same account again yields a working certificate
```

Server log: `RevokedException: Attempt to use revoked certificate : CN=dave,OU=TAKManager,O=TAK`.
This relies on `x509checkRevocation="true"`; the revocation is recorded in TAK Server's database.

### Deleting the account is not revoking

After dave's LDAP account was deleted, with his certificate left unrevoked, his open session kept
receiving his former groups' traffic for as long as it was observed (38 s, more than one refresh
interval). A new connection with the same certificate was accepted but had no group. Withdrawing an
access therefore always means revoking the certificate, not only deleting the account.

**Consequences for phase 1.**

- Issue certificates with the three calls above; the platform needs the account's LDAP password, which
  it generates itself.
- "Regenerate" = revoke every active certificate of the account, change its LDAP password, enroll
  again.
- "Remove an access" = revoke the certificates, then delete the LDAP entry.
- A decision is needed on `validityDays`. A certificate inside a zip stops working when it expires
  and nothing renews it. `GET /Marti/api/certadmin/cert/active` returns `expirationDate`, which gives
  the monitoring phase what it needs.
- The `.p12` built for a zip must be readable by the phones: the spike used OpenSSL's current default
  (AES-256, SHA-256). `makeCert.sh` warns that Android 12 and older may need the legacy format. To be
  checked on a phone.

## 7. Question 5 — several clients on the same certificate

**Answer.** Yes. Three clients with three device UIDs connect at the same time with one certificate
and all work.

**Evidence** (`tests/t5_shared_cert.py`, 10 checks passed):

```
OK   all three connections are still open after 20 s (none evicts the others)
OK   each one receives bob (team-bravo) / bob receives all three devices
OK   the three devices see each other
   as seen by TAK Server: [('team-bravo-shared', 'BRAVO-2', 'SPIKE-shared-2', 'tls:29'), …]
OK   TAK Server lists three distinct subscriptions for the same account
OK   after the shared account is added to evt-spike, alice receives all three devices [27.8, 27.8, 27.8] s
   device 1 switches evt-spike off → positions of alice then received by devices 1, 2, 3: [0, 0, 0]
OK   the second connection works   (same certificate and same device UID)
OK   every connection of the shared certificate is cut [0.0, 0.0, 0.0, 0.0, 0.0] s
OK   bob and alice (other certificates) are not affected
```

**One limit.** Channel state is kept per account, not per device. When one device of a shared
identity switches a channel off, it goes off for every device of the team. With individual accounts
each person controls their own.

**Consequences for phase 1.** Shared identity works as planned. The UI should tell a team leader who
picks this mode that channel switches are common to the team. In `subscriptions/all`, the devices of
a shared account are told apart by `clientUid` and `callsign`, both chosen on the phone.

## 8. Question 6 — connection zip and QR codes

**What was verified.** `tak/certs/make-connection-package.sh`, shipped in the ZIP, builds the zips.
It was run unmodified (`tests/t6_data_package.py`).

Enrollment zip, three files, 4.4 kB:

```
config.pref
MANIFEST/manifest.xml
truststore.p12
```

```xml
<preference version="1" name="cot_streams">
  <entry key="count" class="class java.lang.Integer">1</entry>
  <entry key="description0" class="class java.lang.String">TAK Manager</entry>
  <entry key="enabled0" class="class java.lang.Boolean">true</entry>
  <entry key="connectString0" class="class java.lang.String">tak.example.org:8089:ssl</entry>
  <entry key="caLocation0" class="class java.lang.String">cert/truststore.p12</entry>
  <entry key="caPassword0" class="class java.lang.String">…</entry>
  <entry key="enrollForCertificateWithTrust0" class="class java.lang.Boolean">true</entry>
  <entry key="useAuth0" class="class java.lang.Boolean">true</entry>
  <entry key="cacheCreds0" class="class java.lang.String">Cache credentials</entry>
</preference>
<preference version="1" name="com.atakmap.app_preferences">
  <entry key="displayServerConnectionWidget" class="class java.lang.Boolean">true</entry>
</preference>
```

Zip with a client certificate, four files: the same, plus `<name>.p12`, with `certificateLocation0`
and `clientPassword0` in place of the three enrollment entries. The manifest lists the files and sets
`onReceiveDelete`.

Neither zip holds a callsign, team colour, role or group: the only keys are the ones above. The
script then checked that the content is sufficient: the truststore taken from the zip validates the
server certificate, and the client `.p12` taken from the zip opens a connection on 8089 and receives
its team's traffic, the groups coming from the directory.

**QR formats**, from public sources, not scanned by a phone:

| Client | String |
|---|---|
| iTAK | `TAK Manager,tak.example.org,8089,ssl` (description, host, port, protocol) |
| ATAK, enrollment | `tak://com.atakmap.app/enroll?host=tak.example.org&username=alice&token=<password>` |
| ATAK, import a zip by URL | `tak://com.atakmap.app/import?url=<percent-encoded https URL>` |

**Sources.**

- `tak/certs/make-connection-package.sh` in the ZIP: zip layout, `config.pref` keys, manifest.
- `tak/docs/TAK_Server_Configuration_Guide.pdf` in the ZIP, appendix C (certificate signing) and
  appendix E (Quick Connect).
- [qrtak, QR payload formats](https://github.com/joshuafuller/qrtak/blob/main/docs/qr-formats.md) and
  [data packages](https://github.com/joshuafuller/qrtak/blob/main/docs/data-packages.md): an
  open-source onboarding tool. Its authors state the ATAK formats were checked against the ATAK CIV
  source and that the iTAK format "still needs validation against each target iTAK release".

**What remains uncertain.**

- **Whether one zip suits both ATAK and iTAK.** The ZIP's script puts the files at the root of the
  archive with `MANIFEST/manifest.xml`, which is the layout qrtak describes for iTAK; qrtak's own ATAK
  layout puts them under `certs/`. The official script presumably targets ATAK. Only an import on both
  apps will tell.
- **Whether iTAK accepts the enrollment zip** (`enrollForCertificateWithTrust0`) or only the zip with
  a certificate.
- **The ATAK enrollment QR carries the password in clear**, in the `token` parameter. No public source
  found describes a one-time token on the official TAK Server.
- **QR enrollment and the certificate on 8446.** Appendix E of the guide states that enrolling
  without a preloaded truststore ("Quick Connect") relies on the clients' built-in Let's Encrypt and
  DigiCert roots, and requires the 8446 connector to present a certificate from one of them:

  ```xml
  <connector port="8446" clientAuth="false" _name="cert_https" keystore="JKS"
             keystoreFile="certs/files/letsencrypt-server-cert.jks" keystorePass="…"/>
  ```

  A QR code holds no truststore, so both QR forms depend on this. The spike's 8446 uses the private
  CA; this setting was not tested.

**Consequences for phase 1.**

- Generate zips in the layout of the official script; keep the layout behind one function so an
  iTAK variant can be added if the phone test requires it.
- Port 8446 needs a Let's Encrypt certificate inside TAK Server for the QR forms. Caddy does not
  terminate 8446, so the install must obtain that certificate for TAK Server too, convert it to JKS,
  and restart or reload at each renewal. This is new work compared with the plan.
- The zip with a truststore does not depend on that and works with the private CA.

## 9. Question 7 — the "Manage LDAP Users" UI

**Answer.** With the configuration recommended in section 4 it does not work: the page loads and
reports the directory as not compatible. It works fully if TAK Server's service account is allowed to
write and is placed directly under the base DN. The platform does not need it.

**Why.** The page calls `/Marti/api/ldap-user-management/*`. Read from the server's code and
confirmed in the OpenLDAP log:

- The API takes as directory base **everything after the first comma of `serviceAccountDN`**. With
  `cn=takserver,ou=Services,dc=tak,dc=example` it searches
  `ou=People,dc=tak,dc=example,ou=Services,dc=tak,dc=example` and gets "No Such Object".
- Its compatibility check, `isLdapTakComplient`, creates and deletes a test user. A read-only account
  fails it.

**Evidence** (`tests/t7_ldap_ui.py`). The script adds a temporary account
`cn=takserver-rw,dc=tak,dc=example` with write access, points `serviceAccountDN` at it, restarts TAK
Server, tests, then restores everything.

```
== A. Spike configuration: read-only service account under ou=Services
   isLdapTakComplient → 200 False
   GET users → 404 … [LDAP: error code 32 - No Such Object]
== B. Read-write service account placed directly under the base DN
OK   isLdapTakComplient → 200 True
OK   GET users lists the existing accounts (200) ['alice', 'bob', 'charlie', …]
OK   alice's groups are read through memberOf: ['team-alpha']
OK   GET groups (200) ['team-alpha', 'team-bravo', 'tak-admins']
OK   POST users (creating "uitest") → 201
OK   the account created by the UI can authenticate on the enrollment port
OK   POST groups/ui-group/users/add (group did not exist, created on the fly) → 204
OK   the group created by the UI feeds memberOf (so TAK Server sees it)
OK   locking the account → 204; pwdAccountLockedTime: 000001010000Z
OK   locked account: authentication refused
OK   removing the last member → 204; TAK Server deletes the empty group
OK   DELETE users/uitest → 204
```

Entries created by the UI use the same classes as the spike (`inetOrgPerson`, `groupOfNames`), so the
two are compatible. Not tested: the rendering of the page in a browser (it needs a client certificate
in the browser), and CSV bulk import.

**Consequences for phase 1.** Keep TAK Server's account read-only. A second writer would create
entries the platform's database does not know, which the reconciliation loop would then have to
undo. The UI stays "not compatible", which is the intended state; the admin documentation should say
so. If a manual rescue tool is ever wanted, section B shows the two changes that enable it.

## 10. Question 8 — monitoring APIs

**Answer.** Everything the monitoring phase needs from TAK Server is available over HTTPS on 8443
with an administrator certificate. The server publishes its own OpenAPI description
(`GET /v3/api-docs`, OpenAPI 3.1, 323 paths) and a Swagger UI (`/swagger-ui/index.html`). The
`/actuator/*` endpoints are not listed in it but respond.

All rows below were queried on the running server (`tests/t8_monitoring.py`, 25 checks passed).

| Need | Endpoint | Returns |
|---|---|---|
| Version | `GET /Marti/api/version` | `"5.8-RELEASE-103-HEAD"` |
| | `GET /Marti/api/version/info` | `{"major":5,"minor":8,"patch":103,"branch":"HEAD","variant":"DIRECT"}` |
| Up or down | `GET /actuator/health` | `{"status":"UP"}` |
| Database | `GET /actuator/takserver-database` | `{"maxConnections":1049,"serverVersion":"18.7","apiConnected":true,"messagingConnected":true}` |
| Inputs and ports | `GET /Marti/api/inputs` | Each input with port, protocol, auth, read and message counts |
| Connected clients | `GET /Marti/api/subscriptions/all` | Per connection: `username`, `callsign`, `clientUid`, `groups`, `ipAddress`, `takClient`, `takVersion`, `lastReportMilliseconds`, `subscriptionUid` |
| Client count | `GET /actuator/custom-network-metrics` | `{"numClients":2,"bytesRead":…,"numReads":…}` |
| Memory | `GET /actuator/custom-memory-metrics` | `heapUsed`, `heapCommitted`, and the same for messaging |
| CPU | `GET /actuator/custom-cpu-metrics` | `cpuUsage`, `cpuCount`, and the same for messaging |
| Queues | `GET /actuator/custom-queue-metrics` | `brokerSize`, `submissionSize`, `repositorySize` and capacities |
| Uptime and more | `GET /actuator/metrics/{name}` | 81 metrics, among them `process.uptime`, `system.cpu.usage`, `disk.free`, `jvm.memory.used` |
| Client certificates | `GET /Marti/api/certadmin/cert/active`, `/revoked`, `/expired` | Per certificate: `userDn`, `clientUid`, `serialNumber`, `issuanceDate`, `expirationDate`, `revocationDate`, `hash` |
| Stored events | `GET /Marti/api/database/cotCount` | `{"cotEvents":2586,…}` |

A plain user's certificate gets 403 on `/actuator/*` and on the certificate list. It does get 200 on
`subscriptions/all`, filtered to its own groups.

Not covered by these APIs, to be read elsewhere in phase 3: expiry of the CA and server certificates
(read the files), and the state of the five Java processes of the container (section 11).

**Consequences for phase 3.** The agent polls these endpoints with the platform's administrator
certificate; no log parsing is needed for version, health or clients. One caution:
`GET /Marti/api/security/config` returns the keystore passwords in clear to any administrator
certificate, so that certificate's key deserves the same care as the keystores.

## 11. Findings outside the eight questions

- **First start can leave TAK Server without its API.** The container's entrypoint starts five Java
  processes at once. Three of them each create `TAKIgniteConfig.xml` from the example if it is
  missing; on the first start of the spike one lost the race and died:

  ```
  ERROR tak.server.ignite.IgniteConfigurationHolder -- Exception parsing config
  java.nio.file.FileAlreadyExistsException: TAKIgniteConfig.xml
  ```

  The container kept running with ports 8443 and 8446 closed. Providing the file before the first
  start avoids it (`scripts/02-init.sh`); the replay from scratch then started cleanly.
- **Nothing supervises the five processes.** If one dies the container stays "running". The image's
  healthcheck needs an authorised admin certificate; the spike uses its own (401 expected on 8446 and
  8089 open). Phase 1 needs a healthcheck and a restart policy that catch a dead process.
- **The administrator authorisation lives inside the container** (`UserAuthenticationFile.xml`) and is
  lost when the container is recreated, unless the file is in a volume.
- **`CoreConfig.xml` as a single mounted file works**, including rewriting it in place from the host
  followed by a restart.
- **The database container accepts its password through the `envpass` variable**, read by the ZIP's
  setup script; `CoreConfig.xml` does not need to be mounted into it.
- **Certificates are generated inside the CA container and copied out with `docker cp`**, as the ZIP's
  README does. A first version of `02-init.sh` mounted the host directory instead; run right after a
  clean-up, the container still saw the previous `ca.pem` in the re-created directory (file-sharing
  cache of OrbStack) and the ZIP's script refused to overwrite it. Copying avoids this, and avoids the
  write-permission question for uid 1001 on Linux.

## 12. Verified by script, or only verifiable on a phone

| Verified by the scripts | Needs a real phone |
|---|---|
| Builds, startup, configuration validity | — |
| Groups from the directory, isolation between teams | That ATAK and iTAK display the groups as channels |
| A group change reaches an open connection; timing; the `t-x-g-c` message is sent | That the app refreshes its channel list on that message, without user action |
| Enrollment by CSR on 8446 | That the apps' own enrollment succeeds with these settings and name entries |
| Channel switching through the API, with and without the TAK key usage | That the app's channel screen drives the same API |
| Revocation cuts connections and blocks new ones | What the app shows the user after revocation |
| Three connections on one certificate | Same, on three real devices |
| Zip content; its certificate and truststore work | That ATAK and iTAK import each zip; which `.p12` format old Android accepts |
| QR strings are well-formed | That each app scans them; that 8446 needs a public certificate for it |
| "Manage LDAP Users" API | The page in a browser |

### Procedure for the phone checks

Preparation: the phone must reach the Mac. Find the Mac's LAN address, then:

1. In `spike/docker-compose.yml`, change the three `127.0.0.1:` port bindings of `takserver` to
   `0.0.0.0:`. The server certificate is named `takserver`; ATAK does not check the name on 8089, but
   enrollment on 8446 may, so if a step fails with a certificate-name error, note it as such.
2. `docker compose up -d`, then `scripts/03-post-start.sh`, then `python3 tests/t6_data_package.py`
   after replacing `tak.example.org` by the Mac's address in that script. The zips are in
   `spike/work/packages/`. Usernames are `alice`, `bob`, `charlie`; the password is `TAK_USER_PW` in
   `spike/work/.env`.

Checks, each on ATAK and on iTAK:

| # | Do | Expect |
|---|---|---|
| A | Import `certificate.zip` | Server appears and connects with no question asked; no callsign or team was changed |
| B | Remove the server, import `enrollment.zip`, enter `alice` and the password | Enrollment succeeds, server connects |
| C | Connect two phones as `alice` and `charlie` | Each sees the other; channel `team-alpha` is listed |
| D | Run `python3 tests/t3_group_update.py 1 --force` while both phones are connected | A channel `evt-spike` appears on alice's phone within seconds, without reconnecting, then disappears |
| E | Switch a channel off on the phone | Traffic from that group stops |
| F | Load `certificate.zip` on two phones at once | Both stay connected |
| G | Revoke the certificate (`t4_enroll_revoke.py` shows the two API calls) | The phone is disconnected and cannot reconnect |
| H | Make a QR code from each string of section 8 with any QR generator, scan it | Note whether the app accepts it, and any certificate error |

For H, a failure with a trust error is the expected result on this spike, since 8446 presents a
private certificate; it would confirm the guide's statement about Let's Encrypt.

## 13. Changes made to `PLAN.md`

- Section 3, groups: the platform calls `groups/update/{username}` after each write.
- Section 3, certificates: the platform submits the CSR itself for zips; the revocation endpoint;
  how the platform's API certificate becomes administrator.
- Section 3, new list "Established in phase 0": `updateinterval` in seconds, memberOf, directory
  layout, the OpenLDAP image, the last-group exception, the intermediate CA, the first-start race,
  the HTTP repository workaround.
- Section 3, `x509useGroupCacheRequiresExtKeyUsage`: keep the default, all certificates come from
  enrollment.
- Section 4: the ATAK QR format; the shared channel state of a shared identity.
- Section 5: phase 0 marked done, phone checks left to the user.

Not changed, because it is a decision rather than a correction: the plan lists 8443 among the exposed
ports. The platform reaches 8443 over the Docker network and does not need it published. Publishing
it gives users with a certificate access to WebTAK and to the group-filtered API. Worth deciding in
phase 1.

## 14. Not settled

- Everything in the right-hand column of section 12.
- The amd64 build, never run.
- `ldaps://` between TAK Server and OpenLDAP.
- A Let's Encrypt certificate on the 8446 connector, and its renewal.
- Account lockout under wrong passwords on 8446.
- Behaviour at the scale of the plan (30 to 40 clients): the tests used at most seven.
- The value of `validityDays`, and what ATAK does when an enrolled certificate expires.

## 15. Reproduce

From `spike/`: `scripts/00-extract.sh`, `scripts/01-build-images.sh`, `scripts/02-init.sh`,
`docker compose up -d`, `scripts/03-post-start.sh`, `tests/run-all.sh`. Details in
[`spike/README.md`](../spike/README.md). The containers were stopped at the end of this phase; the
three `tak-spike/*` images were kept so phase 1 does not have to rebuild them.
