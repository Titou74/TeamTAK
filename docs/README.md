# TeamTAK documentation

Installing and updating TeamTAK is covered in the [README](../README.md) at the root of the
repository. The rest is here.

## Running a server

- [Architecture](architecture.md): the services, the ports, how teams, accesses, certificates,
  events and reconciliation work, the login protections, and the host agent.
- [Configuration](configuration.md): the settings in `deploy/.env`.
- [Day-to-day commands](operations.md): the stack, the platform's own commands, starting again
  from nothing.
- [Monitoring, alerts and audit log](monitoring.md): what the monitoring page shows, the alerts,
  the webhook format, the audit log.
- [Backups and restore](backups.md): what an archive holds, copying it elsewhere, restoring.

## Working on TeamTAK

- [Development](development.md): layout of the code, local work, adding a language.
- [Testing](testing.md): unit tests, integration tests, restore proof.

## Specification and history

- [Plan](PLAN.md): the specification and the decisions behind it.
- What was built and verified, phase by phase: [phase 0](phase-0-report.md) (TAK Server 5.8 with
  OpenLDAP), [phase 1](phase-1-report.md) (core of the platform), [phase 2](phase-2-report.md)
  (cross-team events), [phase 3](phase-3-report.md) (monitoring, alerts, backups, audit log).

The project was called *TAK Manager* during phases 0 to 3. The phase reports and the `spike/`
stack keep the names as they were then (`tak-manager` containers, volumes and paths).
