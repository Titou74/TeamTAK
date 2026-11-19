# Phase 0 spike — TAK Server 5.8 + OpenLDAP, locally

A throwaway stack used to answer the eight questions of phase 0. Answers and evidence are in
[`docs/phase-0-report.md`](../docs/phase-0-report.md). This folder holds our own files only; nothing
from the tak.gov ZIP is copied here. Everything generated (ZIP extraction, certificates, passwords,
logs) goes to `spike/work/`, which git ignores.

## Requirements

- Docker and Docker Compose (tested with Docker 29 / OrbStack on an arm64 Mac).
- `takserver-docker-hardened-5.8-RELEASE-103.zip` at the repository root.
- For the tests: `python3` (standard library only), `openssl`, `zip`, `bash`. Nothing to install.
- About 9 GB of disk for the images, 4 GB of free RAM for TAK Server.
- No Iron Bank account, no `docker login`.

## Start

From `spike/`:

```bash
scripts/00-extract.sh
```

```bash
scripts/01-build-images.sh
```

```bash
scripts/02-init.sh
```

```bash
docker compose up -d
```

```bash
scripts/03-post-start.sh
```

| Step | What it does |
|---|---|
| `00-extract.sh` | Extracts the ZIP into `work/`. |
| `01-build-images.sh` | Builds the three images of the ZIP with its Dockerfiles untouched: `tak-spike/ca`, `tak-spike/takserver`, `tak-spike/takserver-db`. Allow 5 minutes. |
| `02-init.sh` | Generates `work/.env` (random passwords), the certificates (root CA, intermediate CA, server, admin) with the ZIP's own scripts, and `work/tak/CoreConfig.xml` from `config/CoreConfig.tpl.xml`. Creates the link `spike/.env → work/.env` that `docker compose` reads. |
| `docker compose up -d` | Starts `openldap`, `tak-database`, `takserver`. TAK Server is ready in about 30 seconds (`docker compose ps` shows `healthy`). |
| `03-post-start.sh` | Waits for TAK Server, then declares `admin.pem` as administrator. Run it again each time the `takserver` container is recreated. |

### If the database build fails with "Curl error (18): Transferred a partial file"

The files `tak/security/rpms/repos/*.repo` of the ZIP point to `http://dl.rockylinux.org` over plain
HTTP. On some networks large HTTP downloads are cut (seen on the development machine). Run the
affected target again with:

```bash
ROCKY_REPOS_HTTPS=1 scripts/01-build-images.sh db
```

The three `.repo` files are switched to `https://` for the duration of the build, in the extracted
copy, then restored to their original content.

## Ports (all on 127.0.0.1)

| Port | Service |
|---|---|
| 8089 | TAK clients, mutual TLS |
| 8443 | API and admin UI, client certificate |
| 8446 | Enrollment, LDAP username + password |
| 3890 | OpenLDAP (for the tests) |

The server certificate is issued for the name `takserver`. The test scripts connect to `127.0.0.1`
and verify that name. To open the admin UI in a browser: import `work/tak/certs/admin.p12` (password:
`TAK_CERT_PASSWORD` in `work/.env`), add `127.0.0.1 takserver` to `/etc/hosts`, then open
`https://takserver:8443`.

## Tests

```bash
tests/run-all.sh
```

About ten minutes. Full output goes to `work/results/`. Each script also runs on its own, for example
`python3 tests/t3_group_update.py 4`.

| Script | Question of the plan |
|---|---|
| `t2_ldap_groups.py` | 2 — directory, LDAP authentication, isolation between teams |
| `t3_group_update.py [n] [--force]` | 3 — group change on a connected client, timed |
| `t3b_last_group.py` | 3 — removing a user's last group (special case) |
| `t4_enroll_revoke.py` | 4 — enrollment by CSR, channels, revocation |
| `t5_shared_cert.py` | 5 — several clients on the same certificate |
| `t6_data_package.py` | 6 — connection zips and QR strings |
| `t7_ldap_ui.py` | 7 — API behind the "Manage LDAP Users" UI (restarts TAK Server twice) |
| `t8_monitoring.py` | 8 — monitoring APIs |

ATAK and iTAK clients are simulated by `tests/taklib.py`: a TLS connection to 8089 with a client
certificate, one position sent per second, every received message recorded.

## Changing a setting

Edit `work/.env` (`LDAP_UPDATE_INTERVAL`, `REQUIRES_EKU`), then:

```bash
scripts/02-init.sh --render
```

```bash
docker compose restart takserver
```

```bash
scripts/03-post-start.sh
```

## Stop and clean up

```bash
docker compose down
```

`scripts/99-clean.sh` also removes the volumes and generated files; `scripts/99-clean.sh --images`
removes the three `tak-spike/*` images and the extraction as well.

## Layout

```
docker-compose.yml          the three services
config/CoreConfig.tpl.xml   template of CoreConfig.xml (<auth>, <ldap>, <certificateSigning>)
ldap/                       OpenLDAP bootstrap files (tree, ACLs, memberOf, sample entries)
scripts/                    extraction, build, initialisation, clean-up
tests/                      test library and one script per question
work/                       generated, git-ignored
```
