# Backups and restore

## What is backed up

Every day (03:00 UTC by default) the host agent writes one archive to `deploy/backups/`:

| In the archive | What it is |
|---|---|
| `platform.dump` | The platform's database (`pg_dump`) |
| `ldap-data.ldif` | The directory (`slapcat`) |
| `takserver.dump` | TAK Server's database (`pg_dump`) |
| `deploy/data/` | The certificate authority, TAK Server's configuration, Caddy's certificates (no logs) |
| `deploy/.env` | Settings and secrets, `APP_SECRET` among them |

Nothing is stopped to take it. The 14 latest archives are kept. The schedule, the number kept and
a **passphrase** are in **Monitoring, Settings**; **Monitoring, Backups** lists the archives and
the runs and has a "Back up now" button.

**An archive holds every secret of the platform**, the key of the certificate authority included.
Set a passphrase: the archives are then encrypted (AES-256, key derived with PBKDF2, by
`openssl`). Keep that passphrase somewhere else: the copy in the settings is inside the encrypted
archive, and a restore asks for it.

A backup can also be taken by hand on the server:

```bash
sudo deploy/scripts/backup.sh
```

## Copying the archives elsewhere

The platform writes to `deploy/backups/` and stops there. Sending the archives off the machine is
yours to set up, because where and with which credentials is yours to decide.
[`deploy/scripts/offsite-copy.example.sh`](../deploy/scripts/offsite-copy.example.sh) is a starting
point: a script that sends each new archive once (rsync, rclone or restic), to run from the host's
cron after the hour of the backup.

## Restoring

```bash
sudo deploy/scripts/restore.sh deploy/backups/teamtak-backup-20261119T031500Z.tar.gz
```

For an encrypted archive the script asks for the passphrase (or reads `BACKUP_PASSPHRASE`).

It replaces **everything**: the platform's database, the directory, TAK Server's database,
`deploy/.env` and `deploy/data/`. The four parts are always restored together. The previous
`deploy/.env` and `deploy/data/` are moved to `deploy/backups/replaced-<date>/`, not deleted. Then
it starts the stack and checks that the platform reaches the directory and TAK Server. Allow five
minutes.

On a **new machine**: install Docker, clone this repository, put the TAK Server ZIP of the same
version at its root, copy the archive over, and run the same command. The certificate authority
comes back with the archive, so the zips already handed out keep working, provided the machine
answers at the same name or address.

What to know:

- A restore is the moment of the backup. What was done since is lost, **and an access regenerated
  or removed since then works again with its old certificate**: regenerate it again.
- The first reconciliation after a restore should correct nothing. If the administration, "TAK
  server sync", shows corrections, read them before acting.

The whole path can be replayed on a local installation, which it destroys and restores; see
[testing](testing.md#restore-proof).
