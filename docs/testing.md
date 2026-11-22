# Testing

## Unit tests

```bash
scripts/test-unit.sh
```

Runs the Rust unit tests (domain rules, authorisation, login-code limits and the anti-bot challenge, the rules of events and
which groups an account gets at a given time, reconciliation against in-memory backends, zip and
PKCS#12 generation, e-mail texts, the alert rules, the reading of what a host reports with output
captured from real Debian and Ubuntu systems, what the host agent refuses, and the check that
every API error code, reconciliation event, alert kind and audit action is translated in every
language) and the web checks (translation files, lint rule against hard-coded text, type check).
It needs no running service.

## Integration tests

```bash
scripts/test-integration.sh
```

Runs the integration tests against the real stack, which must be up in local mode
(`./install.sh --local`). They drive the platform through its HTTP API, read login codes in the
mail catcher, read what was written to OpenLDAP, and connect scripted TAK clients to TAK Server on
8089, 8443 and 8446.

They prove, among other things, that nobody signs in without accepting the terms of use in force
and that an account is asked again when they change, that two members of a team see each other, that another team does
not see them, that a device reads its team's channel, and only it, on TAK Server's API on 8443 as
ATAK and iTAK do, that regenerating a shared identity cuts every old certificate, and that two teams
see each other from the scheduled start of an event to its scheduled end and not a moment outside
it, without reconnecting, and that teams an administrator puts in an event see each other at once. For the monitoring they prove that the host agent reports this stack and
refuses every other container, that a stopped service raises one alert e-mail in each
administrator's language and one more when it is back, who sees which connected devices, what the
audit log records, that a backup asked from the API is written and readable, and that the test
e-mail arrives, from the settings page and from the command line, or says why it could not. For
TAK Server's administration pages they prove that nobody without an administrator's session gets
anything of TAK Server, and that an administrator gets it with TAK Server's administrator role.

With the video server on (`VIDEO=on`, the default), they publish and watch with ffmpeg in
containers on the video server's network (MediaMTX's own image with ffmpeg, pulled by the script):
a stream takes its publish key to publish and its view key to watch, over RTSP, RTMP and SRT, and
nothing else (a wrong key, the view key to publish, a path nobody declared, no credentials); new
keys cut the device publishing and retire the old ones; only the team's members see the keys; TAK
Server lists a stream to its channel's devices and to no other team's; an event's stream works and
is listed from the event's start to its end, its publisher is cut at the end, and the journal
counts none of that as drift; the platform's answers to the video server cannot be reached through
the web port.

For the maps they prove that the catalogue takes an ATAK data package of map sources as it is and
refuses what is not a map source, a source whose tiles do not come back (with the provider's
answer) and a source that points inside the stack; that only administrators keep it and only
leaders pick a team's maps; that a leader's own source reaches the team's devices and no other
team can see or pick it; that a device of the team, asking for its profiles as ATAK does when it
connects, gets the team's map and a device of another team does not; that a phone enrolling with
a username and password gets the preference that turns those profiles on, and every zip carries
it; that a map replaced in the catalogue reaches the device and one taken off no longer does; and
that an event's map reaches the devices of its teams from its start to its end, and no other team,
without the journal counting it as drift. These tests fetch tiles from IGN's Géoplateforme and
OpenTopoMap: they need the Internet.

For the team files they prove that a data package a device sends is seen by its team and not by
another, nor by an administrator outside the team; that a plain file a member sends is wrapped
in a data package that the team's devices list and another team's do not, and comes back as it
was through the platform; that a member removes only what they sent and a leader what the team
sent; that the size setting applies; that a mission's files are listed and can be added to and
taken out, but nothing of a password-protected mission, and nothing goes into a read-only one.

For the deletion of an account they prove that an administrator cannot delete their own account,
a team's only leader or an unknown one, and that nobody else can delete any; that once deleted, the
account is signed out, its device is cut and cannot reconnect, its directory account is gone, the
invitations it sent and received and its pending team request are withdrawn, its video stream is
gone, the rest of its team is untouched, the address gets an e-mail and the audit log names who
did it.

The tests use random names and can be replayed on the same stack. A word after the command runs
only the tests whose name contains it. The whole suite takes about ten minutes; it stops and
restarts the `app` container, stops the reverse proxy for a minute, and stops and restarts the mail
catcher.

## Restore proof

```bash
scripts/prove-restore.sh --yes
```

Proves the restore on a local installation: prepares teams, accesses and zips, takes a backup,
destroys the project's volumes and generated files, restores, and checks with the zips of before.
