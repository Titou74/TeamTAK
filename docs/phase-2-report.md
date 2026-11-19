# Phase 2 report — cross-team events

Date: 2026-11-19. Scope: section 4, "Cross-team events", and section 5, "Phase 2", of
[`PLAN.md`](PLAN.md), built on [`phase-1-report.md`](phase-1-report.md). How to install, run and
test is in the [`README`](../README.md).

Environment: the same as phase 1 (Mac arm64, Docker through OrbStack, TAK Server `5.8-RELEASE-103`
from the tak.gov ZIP, OpenLDAP 2.6, PostgreSQL 17). The TAK Server images were not rebuilt; only
the platform's image was. No ATAK or iTAK device was available: TAK clients are the scripted client
of phase 1, and what a phone shows during an event is **not verified** (section 4).

## 1. Summary

Events work end to end on this machine. A team leader creates an event with a start and an end and
invites other teams; their leaders accept or decline; from the scheduled start to the scheduled end
every TAK account of the host and of the teams that accepted is in one more TAK group,
`evt-<name>`, and their devices receive each other's positions. Nobody clicks at the start or at
the end, no device reconnects, and nothing is left in OpenLDAP afterwards.

| Area | State |
|---|---|
| Migration, API, reconciliation, scheduler | Works; unit-tested and tested against the real stack |
| Start and end at the scheduled time, no manual action | Works; about 0.2 s late on this machine (section 3.3) |
| Reaches connected clients without reconnection | Works; asserted on real TAK Server sessions |
| Teams outside the event (not invited, declined, left, removed) | See nothing and are not seen; asserted |
| Suspension, deletion of a team, cancellation, early end | Work; asserted |
| Platform stopped across a start or an end | Correct as soon as it is back; asserted. While it is down, nothing changes (section 7) |
| Web UI, English and French; e-mails in the recipient's language | Works; walked through by hand in both languages |
| Phones (ATAK, iTAK) | **Not verified** |

Three things are worth knowing before reading further:

1. **An event has no stored state.** Its state follows from its dates and the clock, and every
   reconciliation pass computes the groups of each account for the instant it runs at. This is why
   a late pass, or the first pass after a restart, gives the right result without special cases.
2. **If the platform is down when an event ends, the two teams keep seeing each other until it is
   back.** Nothing else writes to the directory. This was observed, not assumed (section 3.2).
3. **One rule of phase 1 was narrowed**: a pass that only moves accounts in or out of an event
   group no longer cuts the sessions TAK Server lists without a group (section 6).

## 2. What was built

```
crates/core/src/events.rs        rules: state of an event, dates, invitations, which groups at which time
crates/core/src/names.rs         name of an event's group (`evt-…`)
crates/core/src/policy.rs        seven new actions in the single authorisation function
crates/core/src/reconcile.rs     when group-less sessions are cut (narrowed)
crates/server/migrations/0002_events.sql
crates/server/src/events.rs      the API of events, and their e-mails
crates/server/src/reconciler.rs  groups by time, the scheduler, what counts as drift
crates/server/tests/events.rs    four integration tests on the real stack
web/src/components/events-section.tsx   events on a team's page
web/src/pages/admin.tsx          "Events" tab
web/src/locales/*/               the new UI texts and 8 e-mails, in English and French
```

### Data

Two tables. `events`: name, group name, host team, start, end, cancellation date and reason.
`event_teams`: one row per invited team, with its status (`invited`, `accepted`, `declined`, `left`,
`removed`). The host team is not in that list: it takes part by definition. When the host team is
deleted the event row stays, cancelled, with the host's name, so the other teams still see what
happened.

### Rules

- **State.** `scheduled` before the start, `running` from the start (inclusive) to the end
  (exclusive), `ended` afterwards, `cancelled` once cancelled. "End now" sets the end to now.
- **Group.** `evt-` followed by the slug of the event's name, 32 characters at most with the
  prefix. Unique among events that are not over, whoever hosts them; a name is free again once its
  event has ended or was cancelled. Team slugs cannot start with `evt-` (phase 1), so the two kinds
  of group never collide.
- **Who is in it.** Every TAK account (shared, individual, external) of the host team and of each
  team whose status is `accepted`, while the event runs and while the team is active. A suspended
  team has no group at all, the event's included, and gets them back when it is reactivated.
- **Dates.** An event may start at once (no start given, or a start already past). It lasts
  between one minute and 366 days and cannot be planned more than two years ahead. While it has not
  started both dates can move; once it runs only its end can.
- **Invitations.** `invited` → `accepted` or `declined`; `accepted` → `left`; the host can take out
  an `invited` or `accepted` team (`removed`). A team that declined, left or was removed can be
  invited again and must answer again. Nothing moves once the event is over. A team must be active
  to be invited and to accept.
- **Limit.** A team hosts at most 20 events that are not over.

### Authorisation

All in `policy::can`, as in phase 1. The caller is looked at in the team they act for; every
request names that team (`/api/teams/{team}/events/…`), because the same person can lead the host
and be a plain member of an invited team.

| | Admin | Leader of the host | Leader of an invited or participating team | Member | Other |
|---|---|---|---|---|---|
| See the event (name, dates, state, participating teams) | yes | yes | yes | yes, once the team takes part | |
| See the whole invitation list (waiting, declined, left, removed) | yes | yes | | | |
| See an invitation waiting for the team's answer | yes | | yes | | |
| Create an event; list the other teams' names to invite | | yes | yes (for their own events) | | |
| Invite, withdraw, remove a team, change the dates, end early | | yes | | | |
| Cancel | yes | yes | | | |
| Accept, decline, leave | | | yes | | |

What the caller may not see answers "not found": an event their team is not concerned by, an
invitation for a member, a team's page for the leader of another team. Listing the other teams
gives a name and an identifier and nothing else, to leaders of active teams only.

### API

| Call | Does |
|---|---|
| `GET /api/teams/{id}/events` | Events the team hosts, takes part in, or (for leaders) is invited to |
| `POST /api/teams/{id}/events` | Creates an event hosted by the team |
| `GET /api/teams/{id}/invitable-teams` | Names of the other active teams |
| `PATCH /api/teams/{id}/events/{event}` | Changes the dates |
| `POST …/end`, `POST …/cancel` | Ends now; cancels |
| `POST …/teams`, `DELETE …/teams/{team}` | Invites a team; withdraws the invitation or removes the team |
| `POST …/answer`, `POST …/leave` | An invited team accepts or declines; a participating team leaves |
| `GET /api/admin/events`, `POST /api/admin/events/{event}/cancel` | Administrator: every event; cancel one |

Eight new error codes, each translated in both languages (a test fails otherwise):
`invalid_event_name`, `invalid_event_dates`, `event_name_taken`, `too_many_events`, `event_closed`,
`event_not_running`, `team_already_in_event`, `event_invitation_not_pending`.

### Reconciliation and the scheduler

The plug-in point was the one phase 1 named: `Reconciler::desired()`. It now takes a time and gives
each account its team's group plus the group of every running event of its team
(`events::team_groups`, a pure function). Nothing else changed in how the directory is written:
accounts join a group before the last old member leaves it, every account whose groups changed gets
`GET /Marti/api/groups/update/{username}`, and a group disappears with its last member.

- **After a change** made through the API a pass runs as before, so an acceptance, a departure, a
  removal, a cancellation or an early end is live when the request returns.
- **At a start or an end** nobody calls the API. A scheduler sleeps until the next start or end of
  any event and runs a pass then, with the origin `schedule` in the journal. It looks at the dates
  again after every pass and whenever an event is created, moved or closed.
- **After a stop**, the pass run at startup computes the groups for the current time: an event
  that started meanwhile is running, one that ended is gone.
- **Drift.** The journal marks as drift what a pass corrects without the platform having asked.
  The start and the end of an event are not drift: a membership added or removed for a group whose
  event started or ended since the last complete pass is recognised as the schedule at work,
  whatever triggered the pass. Anything else the same pass finds is still drift.

### E-mails

To team leaders only, each in their saved language, with a link to their own team's page. Members
receive nothing. The account that did the action is not written to.

| E-mail | To |
|---|---|
| Invitation | Leaders of the invited team |
| Accepted, declined, left | Leaders of the host |
| Cancelled, new dates, ended early | Leaders of the host and of every team invited or taking part |
| Removed from the event | Leaders of the team removed |

They are sent in the background, after the request has answered, so a slow mail server does not
slow the page; a failure is logged and never returned to the user.

### Web UI

- **Team page**, new section "Cross-team events": invitations waiting for an answer (leaders), the
  running and upcoming events with their dates, their channel name, the teams and where each
  stands, and the actions the caller is allowed (invite, remove a team, change the dates, end now,
  cancel, leave). Past events fold under a counter. A note reminds a shared-identity team that an
  event applies to all its devices and that channel switches are shared.
- **Home page**: a badge on a team's card when event invitations wait for its leaders.
- **Administration**: an "Events" tab listing every event with its whole invitation list, and
  "Cancel the event".
- Dates are entered with the browser's date-and-time field and shown through `Intl` in the UI's
  language and the browser's time zone, which the form names. The page looks again by itself just
  after the next start or end, so a state changes on screen without a reload.

## 3. What was verified, and how

### 3.1 Automated tests

| Suite | Command | Result |
|---|---|---|
| Rust unit tests | `scripts/test-unit.sh` | 94 pass (74 in `core`, 20 in `server`); 61 before this phase |
| Web checks | `scripts/test-unit.sh` | translation check (381 texts, two languages), lint, type check pass |
| Integration tests on the real stack | `scripts/test-integration.sh` | 18 pass: the 14 of phase 1 and 4 new ones |

Results of the last run, on the stack rebuilt with the final code:

```
scripts/test-unit.sh
  test result: ok. 74 passed; 0 failed      (tak-manager-core)
  test result: ok. 20 passed; 0 failed      (tak-manager-server)
  i18n check passed: 2 languages (en, fr), 2 files each, 381 UI texts in en.   (then lint and type check)

scripts/test-integration.sh
  tests/e2e.rs     test result: ok. 14 passed; 0 failed; finished in 40.15s
  tests/events.rs  test result: ok.  4 passed; 0 failed; finished in 216.45s
```

The full integration suite was run twice, on the last two builds, with the same outcome (18 of
18). Before that each event test was run alone while it was being written; the failures met then
were three wrong expectations in the tests themselves (an order of names, and a "forbidden"
expected where the platform rightly answers "not found"), not defects of the platform. The suite
uses random names and can be replayed on the same stack.

New unit tests: the state of an event against the clock; which groups a team gets at a given time
(before, during, after, cancelled, several events at once, a suspended team, a name reused by a
later event); the next start or end; which groups were due between two passes; the dates of a new
event and of a change; ending early; every transition of an invitation, and that nothing moves once
the event is over; the group name and that it can never be a team's; the authorisation table for
the seven new actions; what each kind of viewer is shown and allowed; the reconciliation of an
event group coming and going against in-memory backends, including that nobody is cut; the e-mail
texts and the date format in both languages; that every pass origin has a text in every language.

### 3.2 Integration tests (`crates/server/tests/events.rs`)

Black-box, as in phase 1: the platform's HTTP API through Caddy, the mail catcher, OpenLDAP read
from inside its container, and scripted TAK clients connected to TAK Server on 8089 with the
certificates of the downloaded zips. **A client never reconnects**: one that still receives after
a change kept its connection through it, and every test asserts the connections stayed open.
"Sees" means "receives the other's positions through TAK Server".

| Test | Proves |
|---|---|
| `an_event_starts_and_ends_at_the_scheduled_time` | Five teams. Before the start, nothing in OpenLDAP and no traffic between any of them. **At the scheduled start, with no request made, the host (a shared identity on two devices, and an external access) and the team that accepted (individual accounts) see each other, on connections opened before.** The group holds exactly their accounts. The team never invited, the one that declined and the one that has not answered see nothing and are not seen. An access created during the event joins it. A team that accepts while it runs joins at once; when it leaves it stops seeing and being seen at once, still connected. The host brings the end forward; **at that time, with no request made, the traffic stops, the group and its members are gone from OpenLDAP**, nobody was disconnected and each team still sees itself. The journal shows the start and the end applied by `schedule` passes, none of it as drift |
| `an_event_is_applied_after_the_platform_was_stopped_across_its_start_and_its_end` | The platform's container is stopped before the start: at the start nothing happens. It is started again: the two teams see each other within a second, without reconnecting. Stopped again before the end: **after the end the teams still see each other** (the known limit). Started again: the group is gone within a second. The journal shows four changes with the origin `startup`, none as drift |
| `an_event_follows_what_happens_to_its_teams` | An invitation alone opens nothing. Leaders of other teams and members cannot run the event (403), outsiders do not see it (404), a member sees it and has no action. A team removed by the host is cut off at once and told by e-mail. **A suspended participant is cut off** (its devices are disconnected, the event keeps only the host); reactivated during the event, it is back in it. The host ends the event: the group is gone when the call returns, both teams stay connected, the other team is told, and the event can no longer be changed. A second event with the administrator cancelling it while it runs: same result, both teams told. A third whose host team is deleted: cancelled, group gone, the other team still sees who hosted it and is told |
| `event_rules_invitations_and_emails` | Who can create; names refused; five kinds of bad dates; the group name taken while the event is not over, for every team, and free again afterwards; a team cannot be named like an event group; the list of teams to invite gives a name and nothing else, to leaders only; inviting oneself, a pending team, an unknown team, twice; the invitation e-mail in French for a French-speaking leader, with the dates in the French format and in UTC; members are not written to and do not see the invitation; the count on the home page; decline, invite again, accept, leave, each told to the host; new dates told to the other team and not to the one who changed them; an event that never started wrote nothing to OpenLDAP; cancellation; the administrator's list |

### 3.3 Timing

The target was "a few seconds, never more than the 60-second period". Measured by the tests, on
this machine, with the stack otherwise idle, about 70 TAK accounts in the directory and two to four
of them in the event:

| Measure | Last run | Two earlier runs |
|---|---|---|
| Scheduled start → first position of the other team received by a connected device | 0.29 s | 0.23 s, 0.24 s |
| Scheduled start → pass finished (journal) | 0.21 s | 0.21 s, 0.19 s |
| Scheduled end → group no longer in OpenLDAP (polled every 0.1 s) | 0.13 s | 0.17 s, 0.26 s |
| Scheduled end → pass finished (journal) | 0.22 s | 0.27 s, 0.25 s |
| A leader accepts while the event runs → their team's device receives the host | 0.25 s | 0.14 s, 0.29 s |
| "End now" → the request returns, group removed | 0.11 s | 0.11 s, 0.12 s |
| `docker compose start app` after a stop across the start → the teams see each other | 0.93 s | 0.99 s, 0.96 s |
| `docker compose start app` after a stop across the end → group gone | 1.05 s | 0.84 s, 0.86 s |

One more observation outside the tests: an event created by hand in the UI for 18:00:00 was
applied by a `schedule` pass recorded at 18:00:00.12, and the page left open on it switched from
"Upcoming" to "In progress" without being reloaded.

What these figures do and do not say:

- The clients of these tests send a position every 0.25 s, so "first position received" cannot be
  finer than that; the journal entry is written when the pass ends, after the directory writes.
- A pass makes one LDAP write and one TAK Server call per account whose groups change, one after
  the other. Four accounts take about 0.2 s, most of it the fixed cost of a pass (reading the
  directory, the certificates and the sessions); an event that moves 40 accounts will take longer.
  That was **not measured**.
- No position was received before the scheduled start, and none after the end beyond the delay
  above: the tests fail otherwise.
- If the pass run at the scheduled time fails (TAK Server or the directory unreachable), the
  periodic pass applies the change within its 60-second period once they answer. This follows
  from the design (every pass computes the groups for its own time, and the restart test shows a
  late pass doing exactly that) but was **not tested** with a real outage of TAK Server or
  OpenLDAP at the time of a start or an end.

### 3.4 Checked by hand in the running UI

With the stack running the final code, in the app's browser pane, signing in with codes read in
the mail catcher.

- **English, leader of the host team:** the team page with the new "Cross-team events" section
  and its empty state; "New event" (name, "Start now", start and end, the time zone named under
  the fields); creation of an event starting a few minutes later, shown as "Upcoming", with its
  channel name `evt-avalanche-drill` and "starts in 8 minutes"; inviting two teams from the list
  (a team already invited leaves the list); the invitation list with "invited, no answer yet" and
  the remove buttons; "Change the dates" (the end moved, the page and the other team's e-mail
  follow); the shared-identity note.
- **French, leader of an invited team:** the invitation and the new dates arrive by e-mail in
  French, dates as `19/11/2026 à 17:00 UTC`; the home page carries "1 invitation à un événement"
  on the team's card; the team page shows the invitation with "Refuser" and "Accepter"; after
  accepting, the event shows "Organisé par …", "organisatrice", "participe" and "Quitter
  l'événement"; the host receives "… joins your event" in English. Then, from the same team: a
  second event created with "Commencer maintenant", shown at once as "En cours" with "Modifier les
  dates" (start greyed out, "seule sa fin peut changer") and "Terminer maintenant" with its
  confirmation; once ended it folds under "1 événement passé" as "Terminé".
- **French, plain member of that team:** sees the event, its dates, channel and teams; no button
  at all in the section, and no "Nouvel événement".
- **Administrator:** the "Events" tab lists every event with its state, dates and whole invitation
  list, and "Cancel the event" on those that are not over; on a team's own page the same events
  appear with that single action.
- **The clock doing the work:** the first event was left alone until its start. At 18:00 the page
  showed "In progress" by itself, OpenLDAP held `evt-avalanche-drill` with the accounts of the two
  teams, and the sync tab listed the two additions with the origin "Start or end of an event". The
  administrator then cancelled it: "Cancelled by an administrator", the group gone from OpenLDAP,
  and "L'événement Avalanche Drill est annulé" in the French leader's mailbox.

No TAK client was connected during this walk-through; what devices receive is what section 3.2
tests.

It found six defects or rough edges, all fixed and checked again on the final build: the list of
teams to invite sorted accented names last; the French titles read "de Exercice…"; the home page
still said that nobody else ever sees a team; an administrator looking at another team's page read
"your team"; "starts in 4 minutes" did not move between two refreshes; and the confirmation of a
cancellation offered "Cancel" next to "Cancel the event" (now "Keep the event").

## 4. What was not verified

| Not verified | Why | What it would take |
|---|---|---|
| **Phones.** How an event's channel appears in ATAK and iTAK when the event starts, whether it is switched on without any action, how it disappears at the end, what stays on the map afterwards, and whether the device notifies the user | No device | The procedure of section 9, about fifteen minutes with two phones |
| **Channel switches during an event.** A user can switch a channel off on their device; on a shared identity that switch applies to the whole team (phase 0). Whether a user can thus hide an event channel, and whether the setting survives the next event | No device; the scripted client does not implement channel switching | Same session with a phone |
| **Scale.** 30 to 40 devices, an event that moves 40 accounts at once, several events starting at the same second | Not in scope; the tests used at most eight devices | A load run with scripted clients |
| **A real outage at a boundary.** TAK Server or OpenLDAP down exactly when an event starts or ends | Stopping TAK Server takes minutes to recover and disturbs the other tests | Stop `takserver` across a start, start it again, watch the next periodic pass |
| **Long events.** Days or weeks between start and end | The tests last minutes | Nothing in the design depends on the duration: the scheduler recomputes its wait after every pass, at least once a minute |
| **Clock problems.** A server clock that jumps or drifts | Not simulated | The platform trusts its clock; NTP on the VPS is assumed |
| **The date field in other browsers** (Safari, Firefox, mobile) | Only the app's browser pane (Chromium) was used | Opening the "New event" form in each |
| **Real SMTP**, production mode, amd64 Linux, Let's Encrypt on 8446 | Unchanged from phase 1 | See the phase 1 report |

## 5. Findings about TAK Server

Nothing contradicts phases 0 and 1; three points are now established for the platform as a whole.

1. **An added or removed second group reaches an open session**, within the time of the refresh
   call (`GET /Marti/api/groups/update/{username}`), for a certificate enrolled through TAK
   Server's enrollment, on a shared identity with several devices as well as on individual and
   external accounts. No session had to be cut for an event, in any test.
2. **A group added to an open session carries traffic at once**, with no action of the client:
   with `x509useGroupCacheDefaultUpdatesActive="true"` the new group is active by default. Whether
   ATAK then shows it as a new channel, already ticked, is what the phone check must confirm.
3. **Removal is immediate too**: after the pass, no position of the other team was relayed.
   What a phone keeps drawing from positions it already received is the client's business (a
   marker normally stays until its stale time) and was not observed.

One detail met while testing: TAK Server lists sessions of accounts without any group with no
username (phase 1). With events, passes that change memberships became frequent and automatic,
and each of them used to cut every such session. This is why the rule was narrowed (section 6).

## 6. Deviations from the plan and from the brief

| Subject | Plan or brief | What was done, and why |
|---|---|---|
| Phase 1 rule on group-less sessions | "The platform cuts all of them whenever a pass has changed something" | They are now left alone when a pass only moved accounts in or out of additional groups, and still cut when an account receives its first group or when anything else changed. Without this, each start and end of an event would have disconnected the devices of every suspended team, and logged it as drift. The 14 phase 1 integration tests still pass |
| Events of a deleted host | "If the host team is deleted, the event is cancelled" | Done, and the event row is kept (host name included) so the other teams see a cancelled event instead of one that vanished |
| Who is e-mailed | Invitation; answers; cancellation and date changes | As asked, plus three that seemed needed: the team removed by the host is told; an early end is told like a cancellation; the host is told when a participating team is deleted |
| Sending e-mails | Phase 1 sends during the request | Event e-mails are sent in the background: one action can write to the leaders of many teams |
| Dates in e-mails | "Dates through the locale" | Written in the recipient's language's format, but **in UTC**, and saying so: the server does not know where the reader is. The e-mail points to the team's page, which shows the reader's own time zone |
| Duration | Not specified | Between one minute and 366 days, at most two years ahead. Arbitrary but harmless bounds against typing mistakes |
| Changing a running event | "Changes the dates" | Only the end, once the event has started |
| Renaming an event | Not specified | Not possible: the group name comes from the name. Cancel and create again |
| Re-inviting | Not specified | A team that declined, left or was removed can be invited again |
| What an invited team sees | Not specified | Its leaders see the invitation and the teams already taking part, not who else was invited. Its members see nothing until the team accepts |
| A team that declined, left or was removed | Not specified | The event disappears from its list |
| Administrator on a team's page | "Sees every event and can cancel one" | Also sees a team's events on that team's page, with the same single action |
| Limit of events per team | Not specified | 20 that are not over, as a guard |

## 7. Known limitations

- **The platform must be running for an event to start or end.** Stopped across an end, the group
  stays and the teams keep seeing each other until it is back; stopped across a start, the event
  starts when it is back. In both cases the first pass after the restart puts things right.
- **The automatic guard of phase 1 also applies here.** A pass run for the schedule that finds
  more than three directory accounts unknown to the database changes nothing and reports an error;
  the event is then not applied until an administrator runs a check.
- **E-mails give dates in UTC** (section 6).
- **An event is all or nothing for a team.** Every account of a participating team is in the
  group; a team cannot bring only some of its people. With a shared identity this is unavoidable.
- **No reminder and no announcement.** Nothing is e-mailed when an event starts or ends, and
  members are never written to: they learn of an event from their leader or on the team's page.
- **An unanswered invitation stays** until the event is over; nothing expires it earlier.
- **The list of teams to invite** shows every active team to every leader. With a dozen teams
  this is the intent; it is not meant for a platform with hundreds.
- **The date field follows the browser**, not the language chosen in the UI: a French browser
  shows `jj/mm/aaaa` in the English UI. The dates displayed outside the form follow the UI.
- **Finished events are never purged.** A team's page shows its 20 latest; the administration
  lists the 500 latest.
- **Drift is still a heuristic** (phase 1). One more case: if TAK Server has not yet applied a
  group refresh when the next periodic pass looks at the sessions, that pass asks again and
  records it as drift. Not seen in the tests.
- Everything listed in phase 1, section 7, still holds.

## 8. For phase 3 (monitoring)

- **Is the schedule being applied?** `reconcile_status.desired_at` is the time the last pass
  without error computed its desired state for. An event that is not cancelled and has a start or
  an end between `desired_at` and now minus a minute or two has not been applied: this is the
  alert to raise ("event not started" or, more serious, "event not ended"). The same query gives
  "the platform was down during the end of an event".
- **Journal.** `reconcile_events.trigger` has a fifth value, `schedule`. Memberships of `evt-…`
  groups are the trace of every start, end, acceptance and departure, with the account concerned.
- **Expected groups.** `GET /api/admin/reconciliation` now counts the group of each running event
  in `expected_groups`.
- **Audit log.** Each action on an event writes one log line with the acting account ("event
  created", "team invited to an event", "event invitation answered", "team left an event", "team
  removed from an event", "event dates changed", "event ended early", "event cancelled"). The
  tables keep who invited (`event_teams.invited_by`) and who last answered or removed
  (`answered_by`), and `events.created_by`; who cancelled is only in the log.
- **E-mails sent in the background** fail silently for the user. Their failures are warnings in
  the log (`e-mail not sent`, with the template); a counter or an alert on them belongs to
  monitoring, and applies to the phase 1 notifications too.
- **Clock.** The start and the end depend on the server's clock. Checking NTP synchronisation on
  the VPS is a natural task for the host agent.
- **Connected devices.** The list planned for monitoring would let an event's page show who is
  actually connected in it; TAK Server's subscription list already gives the groups of each
  session.
- **Backups** need nothing new: events live in the platform's database.

## 9. Left for the user

1. **The phone check for events**, with two phones in two teams of the platform (ATAK and, if
   possible, iTAK), both connected:
   1. In the first team create an event that starts in three minutes and lasts ten; invite the
      second team; accept with the second team's leader.
   2. Before the start: neither phone sees the other, and the channel list (ATAK: the three-dot
      menu of the TAK Server connection, "Channels"; iTAK: Settings, the server, groups) shows
      only the team's channel.
   3. At the start, without touching the phones: note how long each takes to show the other,
      whether a channel named `evt-…` appears in the list, whether it is already switched on, and
      whether the phone says anything.
   4. Switch the event channel off on one phone, then on. On a shared identity, check that the
      other devices of the team follow.
   5. "End now" on the event's page: note how long the other team's marker stays on the map, and
      whether the channel leaves the list.
   6. Repeat with the phone's screen off across the start, and with the app closed and reopened
      during the event.
2. Decide whether dates in e-mails in UTC are acceptable, or whether the platform should have a
   time zone of its own for e-mails.
3. What phase 1 left: the phone checks of the accesses, and a production install on the VPS.

## 10. Repository hygiene and state of the machine

Nothing generated is versioned: the list of files to be committed was checked for the TAK Server
ZIP and anything extracted from it, `.env` files, keys, certificates and keystores (`*.zip`,
`*.p12`, `*.jks`, `*.pem`, `*.key`, `deploy/data/`): none.

State of the development machine at the end of the phase: the containers are stopped
(`docker compose down`); the Docker volumes, `deploy/.env` and `deploy/data/` are kept, so
`cd deploy && docker compose up -d` brings back the stack as tested, with the teams and events the
tests and the walk-through created. Images: `tak-manager/app` was rebuilt; `tak-manager/takserver`
and `tak-manager/takserver-db` were not touched. No container, image or volume of another project
was touched.
