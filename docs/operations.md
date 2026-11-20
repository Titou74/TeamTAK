# Day-to-day commands

All commands are run from `deploy/` on the server.

```bash
cd deploy
```

## The stack

```bash
docker compose ps
```

```bash
docker compose logs -f app
```

```bash
docker compose down
```

```bash
docker compose up -d
```

`docker compose down` keeps the data (Docker volumes and `deploy/data/`). After `up -d`, TAK Server
takes about a minute to become healthy.

## Commands of the platform

```bash
docker compose exec app teamtak status
```

Checks that the database, the directory and TAK Server are reachable, and says whether the host
agent answers.

```bash
docker compose exec app teamtak send-test-mail you@example.org
```

Sends a test e-mail through the mail server of `deploy/.env` and prints the server's answer in
full when it refuses. Use it right after the install or after changing the `SMTP_*` settings:
it works before anyone can sign in. `--language fr` writes it in French. Once signed in, an
administrator can do the same from **Monitoring, Settings, Send me a test e-mail**.

```bash
docker compose exec app teamtak grant-admin you@example.org
```

**Rescue command.** Makes the account a platform administrator (creating it if needed) and prints
a one-time login code. It works when e-mail is down: on the sign-in page, type that address, choose
"I already have a code" and enter it.

```bash
docker compose exec app teamtak reconcile
```

Runs one reconciliation pass and prints what it changed.

```bash
docker compose exec agent teamtak-agent report
```

Prints what the host agent sees, as JSON: the machine, this stack's containers, TAK Server's
processes, the backup archives.

## Start again from nothing

```bash
deploy/scripts/clean.sh
```

Removes the containers, the volumes, `deploy/.env` and `deploy/data/`: every account, team and
certificate is lost, including the certificate authority. `--images` also removes the
`teamtak/*` images. The backup archives in `deploy/backups/` are kept.
