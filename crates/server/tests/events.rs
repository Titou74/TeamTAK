//! Integration tests of cross-team events (phase 2), against the real stack: the platform,
//! PostgreSQL, OpenLDAP and TAK Server 5.8.
//!
//! Like `e2e.rs` they are black-box and ignored by a plain `cargo test`:
//!
//! ```bash
//! ./install.sh --local
//! scripts/test-integration.sh event
//! ```
//!
//! "Seeing" a team means receiving its positions through TAK Server on a connection that
//! was opened before the change and never reopened: the scripted client does not reconnect.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// Longest wait for a position that must arrive.
const SEES: Duration = Duration::from_secs(12);
/// Long enough for many positions to have been relayed if they were going to be.
const SILENCE: Duration = Duration::from_secs(6);
/// Period of the positions sent by the clients of these tests: what bounds the delays measured.
const BEAT: Duration = Duration::from_millis(250);
/// The plan allows a scheduled change up to one reconciliation period late; the aim is a
/// few seconds. The tests fail above this.
const ON_TIME: Duration = Duration::from_secs(10);

async fn connect(identity: &Identity, label: &str) -> TakClient {
    TakClient::connect_every(identity, label, BEAT).await.unwrap_or_else(|e| panic!("{label} does not connect: {e}"))
}

/// A team in shared mode with its shared access, and one device connected with it.
async fn team_with_device(admin: &Session, label: &str) -> (TeamFixture, Identity, TakClient) {
    let team = approved_team(admin, "shared", 5).await;
    let identity = access_identity(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let device = connect(&identity, label).await;
    (team, identity, device)
}

/// Watches for `window`, then lists every position that crossed between the two sides.
async fn crossings(side_a: &[(&str, &TakClient)], side_b: &[(&str, &TakClient)], window: Duration) -> Vec<String> {
    let since = Instant::now();
    tokio::time::sleep(window).await;
    let mut crossed = Vec::new();
    for (name_a, a) in side_a {
        for (name_b, b) in side_b {
            if a.saw_since(b, since) {
                crossed.push(format!("{name_a} received {name_b}"));
            }
            if b.saw_since(a, since) {
                crossed.push(format!("{name_b} received {name_a}"));
            }
        }
    }
    crossed
}

async fn sleep_until(at: Instant) {
    tokio::time::sleep(at.saturating_duration_since(Instant::now())).await;
}

/// Waits until a group has no member left in OpenLDAP; returns when that was seen.
async fn group_gone(group: &str, timeout: Duration) -> Option<Instant> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ldap_group_members(group).is_empty() {
            return Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// The teams an event lists, as `name:status`, whatever their order.
fn teams<const N: usize>(expected: [String; N]) -> Vec<String> {
    let mut expected = expected.to_vec();
    expected.sort();
    expected
}

fn open_connections(devices: &[(&str, &TakClient)]) {
    for (name, device) in devices {
        assert!(!device.is_closed(), "{name} was disconnected: the change must reach an open connection");
    }
}

// ------------------------------------------------------------------------------ the schedule

/// The whole life of an event with nobody clicking at the start or at the end: five teams,
/// one hosts, one accepts, one is never invited, one declines, one joins late and leaves.
#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_event_starts_and_ends_at_the_scheduled_time() {
    let admin = Session::admin().await;

    // The host: shared identity on two devices, plus an external access (a guest of the team).
    let (alpha, alpha_identity, a1) = team_with_device(&admin, "alpha").await;
    let a1_bis = connect(&alpha_identity, "alpha-second-device").await;
    let guest_identity = access_identity(&alpha.leader, &alpha.id, json!({ "kind": "external", "label": "Liaison" })).await;
    let guest = connect(&guest_identity, "alpha-guest").await;
    // The team that accepts: individual accounts.
    let bravo = approved_team(&admin, "individual", 5).await;
    let bravo_identity = access_identity(&bravo.leader, &bravo.id, json!({ "kind": "individual" })).await;
    let b1 = connect(&bravo_identity, "bravo").await;
    let bravo_username = ldap_group_members(&bravo.slug).remove(0);
    // Never invited; declines; accepts late then leaves.
    let (_charlie, _, c1) = team_with_device(&admin, "charlie").await;
    let (delta, _, d1) = team_with_device(&admin, "delta").await;
    let (echo, _, e1) = team_with_device(&admin, "echo").await;

    // The event is planned to start in a little while and, for now, to last ten minutes.
    let event = create_event(&alpha, Some(22), 600).await;
    assert!(event.group.starts_with("evt-exercise-"), "{}", event.group);
    for team in [&bravo, &delta, &echo] {
        invite_to_event(&alpha, &event, team).await;
    }
    answer_event(&bravo, &event, true).await;
    answer_event(&delta, &event, false).await;
    assert_eq!(event_seen_by(&alpha.leader, &alpha.id, &event.id).await["phase"], "scheduled");

    // Before the start: nothing exists in the directory and nobody sees another team.
    let event_side = [("alpha", &a1), ("alpha-guest", &guest)];
    let crossed = crossings(&event_side, &[("bravo", &b1), ("charlie", &c1), ("delta", &d1), ("echo", &e1)], SILENCE).await;
    assert!(crossed.is_empty(), "teams see each other before the start: {crossed:?}");
    assert!(ldap_group_members(&event.group).is_empty(), "the group must not exist before the start");
    assert_eq!(ldap_member_of(&bravo_username), vec![bravo.slug.clone()]);
    let start = instant_of(event.starts_at);
    assert!(Instant::now() + Duration::from_secs(1) < start, "this test ran too slowly to observe the start");

    // The start, with nobody doing anything.
    let first = a1.wait_first_seen(&b1, start - Duration::from_secs(2), Duration::from_secs(70)).await;
    let first = first.expect("alpha never received bravo after the scheduled start");
    assert!(first >= start, "alpha received bravo {:?} before the scheduled start", start - first);
    let start_delay = first - start;
    println!("MEASURE start: first position of the other team received {start_delay:.2?} after the scheduled start");
    assert!(start_delay < ON_TIME, "the event started {start_delay:?} late");
    assert!(b1.sees(&a1, SEES).await, "bravo receives alpha");
    assert!(guest.sees(&b1, SEES).await && b1.sees(&guest, SEES).await, "the external access of alpha is in the event too");
    assert!(a1_bis.sees(&b1, SEES).await && b1.sees(&a1_bis, SEES).await, "a shared identity is one account: every device of it is in");
    open_connections(&[("alpha", &a1), ("alpha-second-device", &a1_bis), ("alpha-guest", &guest), ("bravo", &b1)]);

    // The directory holds the group with every account of the two teams, and nobody else.
    let mut expected = vec![format!("{}-shared", alpha.slug), format!("{}-x-liaison", alpha.slug), bravo_username.clone()];
    expected.sort();
    assert_eq!(ldap_group_members(&event.group), expected);
    let mut both = vec![bravo.slug.clone(), event.group.clone()];
    both.sort();
    assert_eq!(ldap_member_of(&bravo_username), both, "bravo keeps its own group and gains the event's");
    let shown = event_seen_by(&bravo.leader, &bravo.id, &event.id).await;
    assert_eq!(shown["phase"], "running");
    assert_eq!(event_teams(&shown), teams([format!("{}:accepted", bravo.name), format!("{}:host", alpha.name)]));

    // The team that was not invited, the one that declined and the one that has not
    // answered see nothing of the event and are not seen.
    let inside = [("alpha", &a1), ("alpha-guest", &guest), ("bravo", &b1)];
    let outside = [("charlie", &c1), ("delta", &d1), ("echo", &e1)];
    let crossed = crossings(&inside, &outside, SILENCE).await;
    assert!(crossed.is_empty(), "a team outside the event crossed into it: {crossed:?}");

    // An access created during the event gets the group at once.
    let newcomer = add_member(&bravo).await;
    let newcomer_identity = access_identity(&newcomer, &bravo.id, json!({ "kind": "individual" })).await;
    let b2 = connect(&newcomer_identity, "bravo-newcomer").await;
    assert!(b2.sees(&a1, SEES).await && a1.sees(&b2, SEES).await, "an access created during the event joins it");
    assert_eq!(ldap_group_members(&event.group).len(), 4);

    // A team that accepts while the event runs joins at once, without reconnecting.
    let accepted = Instant::now();
    answer_event(&echo, &event, true).await;
    let joined = e1.wait_first_seen(&a1, accepted, SEES).await.expect("echo joins the running event");
    println!("MEASURE late acceptance: echo received alpha {:.2?} after its leader clicked", joined - accepted);
    assert!(a1.sees(&e1, SEES).await && b1.sees(&e1, SEES).await);
    open_connections(&[("echo", &e1), ("alpha", &a1), ("bravo", &b1)]);

    // A team that leaves stops seeing and being seen at once, and stays connected.
    let left = echo.leader.post(&format!("/teams/{}/events/{}/leave", echo.id, event.id), json!({})).await.ok();
    assert_eq!(left["synced"], true);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&inside, &[("echo", &e1)], SILENCE).await;
    assert!(crossed.is_empty(), "echo left the event but: {crossed:?}");
    open_connections(&[("echo", &e1)]);
    assert!(!ldap_group_members(&event.group).contains(&format!("{}-shared", echo.slug)));
    assert_eq!(event_seen_by(&echo.leader, &echo.id, &event.id).await, Value::Null, "the event no longer concerns echo");

    // The host brings the end forward to a few seconds from now (an event lasts at least a
    // minute). From here on nobody touches anything.
    let new_end = (event.starts_at + time::Duration::seconds(61)).max(utc_in(8));
    let changed = alpha
        .leader
        .patch(&format!("/teams/{}/events/{}", alpha.id, event.id), json!({ "ends_at": iso(new_end) }))
        .await
        .ok();
    let end = instant_of(parse_time(&changed["ends_at"]));
    assert!(a1.sees(&b1, Duration::from_secs(3)).await, "still running until the new end");
    assert!(Instant::now() < end, "this test ran too slowly to observe the end");

    sleep_until(end).await;
    let gone = group_gone(&event.group, Duration::from_secs(70)).await.expect("the group is still in OpenLDAP");
    println!("MEASURE end: group gone from OpenLDAP {:.2?} after the scheduled end (polled every 0.1 s)", gone - end);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let alpha_side = [("alpha", &a1), ("alpha-second-device", &a1_bis), ("alpha-guest", &guest)];
    let crossed = crossings(&alpha_side, &[("bravo", &b1), ("bravo-newcomer", &b2)], SILENCE).await;
    assert!(crossed.is_empty(), "after the end: {crossed:?}");
    // The last position that crossed. Positions come every 0.25 s, so it may well have
    // arrived just before the end: the figure says how late the cut was at most, not at least.
    let last = a1.last_seen(&b1).expect("alpha did receive bravo during the event");
    let end_delay = last.saturating_duration_since(end);
    if last >= end {
        println!("MEASURE end: last position of the other team received {end_delay:.2?} after the scheduled end");
    } else {
        println!("MEASURE end: last position of the other team received {:.2?} before the scheduled end, none after", end - last);
    }
    assert!(end_delay < ON_TIME, "the event ended {end_delay:?} late");

    // Nothing is left behind, nobody was disconnected, and each team still sees itself.
    assert!(ldap_group_members(&event.group).is_empty());
    assert!(ldap_search("ou=Group", &format!("(cn={})", event.group), &["cn"]).trim().is_empty(), "no empty group entry either");
    assert_eq!(ldap_member_of(&bravo_username), vec![bravo.slug.clone()]);
    open_connections(&alpha_side);
    open_connections(&[("bravo", &b1), ("bravo-newcomer", &b2), ("charlie", &c1), ("delta", &d1), ("echo", &e1)]);
    assert!(a1.sees(&guest, SEES).await && b1.sees(&b2, SEES).await, "each team still sees its own members");
    assert_eq!(event_seen_by(&alpha.leader, &alpha.id, &event.id).await["phase"], "ended");

    // The journal: the start and the end were applied by passes run for the schedule, at
    // the scheduled time, and none of it is reported as drift.
    let journal = journal_of_group(&admin, &event.group).await;
    assert!(journal.iter().all(|e| e["is_drift"] == false), "an event at work is not drift: {journal:?}");
    let by_schedule = |kind: &str| -> Vec<&Value> { journal.iter().filter(|e| e["kind"] == kind && e["trigger"] == "schedule").collect() };
    let (added, removed) = (by_schedule("member_added"), by_schedule("member_removed"));
    assert_eq!(added.len(), 3, "the three accounts present at the start: {journal:?}");
    assert_eq!(removed.len(), 4, "the four accounts present at the end: {journal:?}");
    let server_start = parse_time(&added[0]["occurred_at"]) - event.starts_at;
    let server_end = parse_time(&removed[0]["occurred_at"]) - parse_time(&changed["ends_at"]);
    println!("MEASURE journal: start applied {server_start} after the scheduled time, end applied {server_end} after");
    assert!(server_start >= time::Duration::ZERO && server_end >= time::Duration::ZERO, "nothing happens early");
}

// ------------------------------------------------------------------------------ restarts

/// The platform is stopped across the start, then across the end. Nothing can happen while
/// it is down; everything must be right as soon as it is back.
#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_event_is_applied_after_the_platform_was_stopped_across_its_start_and_its_end() {
    let admin = Session::admin().await;
    let (alpha, _, a1) = team_with_device(&admin, "alpha").await;
    let (bravo, _, b1) = team_with_device(&admin, "bravo").await;
    let event = create_event(&alpha, Some(14), 600).await;
    invite_to_event(&alpha, &event, &bravo).await;
    answer_event(&bravo, &event, true).await;
    let start = instant_of(event.starts_at);

    // Down before the start, and still down after it.
    stop_platform();
    assert!(Instant::now() < start, "this test ran too slowly: the platform was still up at the start");
    sleep_until(start + Duration::from_secs(3)).await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo", &b1)], Duration::from_secs(4)).await;
    assert!(crossed.is_empty(), "nothing can start while the platform is down: {crossed:?}");
    assert!(ldap_group_members(&event.group).is_empty());

    // Back up: the event is running, so the group must appear without anyone asking.
    let restarted = Instant::now();
    start_platform().await;
    let first = a1.wait_first_seen(&b1, restarted, Duration::from_secs(60)).await.expect("the event did not start after the restart");
    println!("MEASURE restart across the start: teams see each other {:.2?} after `docker compose start`", first - restarted);
    assert!(b1.sees(&a1, SEES).await);
    open_connections(&[("alpha", &a1), ("bravo", &b1)]);
    assert_eq!(ldap_group_members(&event.group).len(), 2);

    // Same thing across the end.
    let new_end = (event.starts_at + time::Duration::seconds(61)).max(utc_in(10));
    let changed = alpha
        .leader
        .patch(&format!("/teams/{}/events/{}", alpha.id, event.id), json!({ "ends_at": iso(new_end) }))
        .await
        .ok();
    let end = instant_of(parse_time(&changed["ends_at"]));
    stop_platform();
    assert!(Instant::now() < end, "this test ran too slowly: the platform was still up at the end");
    sleep_until(end + Duration::from_secs(3)).await;
    // Known limit: with the platform down nothing removes the group, so the two teams keep
    // seeing each other until it is back.
    assert!(a1.sees(&b1, Duration::from_secs(4)).await, "TAK Server keeps the group while the platform is down");
    assert_eq!(ldap_group_members(&event.group).len(), 2);

    let restarted = Instant::now();
    start_platform().await;
    let gone = group_gone(&event.group, Duration::from_secs(60)).await.expect("the group is still there after the restart");
    println!("MEASURE restart across the end: group gone {:.2?} after `docker compose start`", gone - restarted);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo", &b1)], SILENCE).await;
    assert!(crossed.is_empty(), "after the end and the restart: {crossed:?}");
    open_connections(&[("alpha", &a1), ("bravo", &b1)]);
    assert_eq!(event_seen_by(&alpha.leader, &alpha.id, &event.id).await["phase"], "ended");

    // Applied by the passes run at startup, and not mistaken for drift.
    let journal = journal_of_group(&admin, &event.group).await;
    let kinds: Vec<(&str, &str)> = journal.iter().map(|e| (e["kind"].as_str().unwrap(), e["trigger"].as_str().unwrap())).collect();
    assert_eq!(
        kinds,
        [("member_added", "startup"), ("member_added", "startup"), ("member_removed", "startup"), ("member_removed", "startup")],
        "{journal:?}"
    );
    assert!(journal.iter().all(|e| e["is_drift"] == false), "{journal:?}");
}

// ------------------------------------------------------------------------------ teams

/// What happens to an event when its teams change: a team removed by the host, a
/// participant suspended then reactivated, the event ended early, cancelled by an
/// administrator, and cancelled because its host team is deleted.
#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_event_follows_what_happens_to_its_teams() {
    let admin = Session::admin().await;
    let (alpha, _, a1) = team_with_device(&admin, "alpha").await;
    let (bravo, bravo_identity, b1) = team_with_device(&admin, "bravo").await;
    let (charlie, _, c1) = team_with_device(&admin, "charlie").await;
    let alpha_member = add_member(&alpha).await;
    let bravo_member = add_member(&bravo).await;
    let outsider = Session::new_account("outsider").await;
    let (alpha_user, bravo_user, charlie_user) =
        (format!("{}-shared", alpha.slug), format!("{}-shared", bravo.slug), format!("{}-shared", charlie.slug));

    // An event that starts at once: no group until someone else accepts... the host alone
    // is in it from the first second.
    let event = create_event(&alpha, None, 3600).await;
    assert_eq!(ldap_group_members(&event.group), vec![alpha_user.clone()], "the host is in its event from the start");
    invite_to_event(&alpha, &event, &bravo).await;
    invite_to_event(&alpha, &event, &charlie).await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo", &b1), ("charlie", &c1)], Duration::from_secs(3)).await;
    assert!(crossed.is_empty(), "an invitation alone opens nothing: {crossed:?}");
    let accepted = Instant::now();
    answer_event(&bravo, &event, true).await;
    answer_event(&charlie, &event, true).await;
    let first = a1.wait_first_seen(&b1, accepted, SEES).await.expect("alpha receives bravo");
    println!("MEASURE acceptance of a running event: alpha received bravo {:.2?} after the click", first - accepted);
    assert!(b1.sees(&c1, SEES).await && c1.sees(&a1, SEES).await, "the three teams see each other");
    open_connections(&[("alpha", &a1), ("bravo", &b1), ("charlie", &c1)]);

    // Who may do what. Only a leader of the host runs the event; a leader of a participating
    // team can only leave; members see it and touch nothing; anyone else does not see it.
    let on = |team: &TeamFixture, action: &str| format!("/teams/{}/events/{}{action}", team.id, event.id);
    let later = json!({ "ends_at": iso(utc_in(7200)) });
    for intruder in [&bravo.leader, &bravo_member] {
        intruder.patch(&on(&bravo, ""), later.clone()).await.expect_error(403, "forbidden");
        intruder.post(&on(&bravo, "/end"), json!({})).await.expect_error(403, "forbidden");
        intruder.post(&on(&bravo, "/cancel"), json!({})).await.expect_error(403, "forbidden");
        intruder.post(&on(&bravo, "/teams"), json!({ "team_id": charlie.id })).await.expect_error(403, "forbidden");
        intruder.delete(&on(&bravo, &format!("/teams/{}", charlie.id))).await.expect_error(403, "forbidden");
        // And not by naming the host team either: it does not exist for them.
        intruder.post(&on(&alpha, "/cancel"), json!({})).await.expect_error(404, "not_found");
    }
    for member in [&alpha_member, &bravo_member] {
        let team = if std::ptr::eq(member, &alpha_member) { &alpha } else { &bravo };
        let seen = event_seen_by(member, &team.id, &event.id).await;
        assert_eq!((seen["name"].as_str(), seen["phase"].as_str()), (Some(event.name.as_str()), Some("running")));
        assert_eq!(event_teams(&seen).len(), 3, "a member sees the participating teams: {seen}");
        assert!(seen["permissions"].as_object().unwrap().values().all(|v| v == false), "and manages nothing: {seen}");
        member.post(&on(team, "/leave"), json!({})).await.expect_error(403, "forbidden");
        member.post(&on(team, "/end"), json!({})).await.expect_error(403, "forbidden");
        member.post(&format!("/teams/{}/events", team.id), json!({ "name": "Mine", "ends_at": iso(utc_in(3600)) })).await.expect_error(403, "forbidden");
    }
    outsider.get(&format!("/teams/{}/events", alpha.id)).await.expect_error(404, "not_found");
    outsider.post(&on(&alpha, "/cancel"), json!({})).await.expect_error(404, "not_found");
    outsider.get("/admin/events").await.expect_error(403, "forbidden");
    alpha.leader.post(&format!("/admin/events/{}/cancel", event.id), json!({})).await.expect_error(403, "forbidden");
    alpha.leader.post(&on(&alpha, "/leave"), json!({})).await.expect_error(403, "forbidden");
    assert!(a1.sees(&b1, SEES).await, "none of these attempts changed anything");

    // The host removes a participating team: cut off at once, still connected.
    let removed = alpha.leader.delete(&on(&alpha, &format!("/teams/{}", charlie.id))).await.ok();
    assert_eq!(removed["synced"], true);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1), ("bravo", &b1)], &[("charlie", &c1)], SILENCE).await;
    assert!(crossed.is_empty(), "charlie was removed but: {crossed:?}");
    open_connections(&[("charlie", &c1), ("alpha", &a1), ("bravo", &b1)]);
    assert!(!ldap_group_members(&event.group).contains(&charlie_user));
    assert_eq!(ldap_member_of(&charlie_user), vec![charlie.slug.clone()]);
    assert_eq!(event_seen_by(&charlie.leader, &charlie.id, &event.id).await, Value::Null);
    assert!(mail_about(&charlie.leader.email, "no longer part of the event").await.text.contains(&event.name));

    // A suspended participant loses the event's group with its own: its devices are cut.
    let suspended = admin.patch(&format!("/admin/teams/{}", bravo.id), json!({ "suspended": true })).await.ok();
    assert_eq!(suspended["synced"], true);
    assert!(b1.closed_within(Duration::from_secs(10)).await.is_some(), "the suspended team's device is disconnected");
    assert_eq!(ldap_group_members(&event.group), vec![alpha_user.clone()], "only the host is left in the event");
    assert!(ldap_member_of(&bravo_user).is_empty());
    let lonely = connect(&bravo_identity, "bravo-suspended").await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo-suspended", &lonely)], SILENCE).await;
    assert!(crossed.is_empty(), "a suspended team is cut off from the event: {crossed:?}");
    open_connections(&[("alpha", &a1)]);
    // Still accepted, so the event comes back with the team.
    let resumed = admin.patch(&format!("/admin/teams/{}", bravo.id), json!({ "suspended": false })).await.ok();
    assert_eq!(resumed["synced"], true);
    assert!(lonely.closed_within(Duration::from_secs(10)).await.is_some(), "the group-less session is cut so it reconnects");
    let b2 = connect(&bravo_identity, "bravo-back").await;
    assert!(b2.sees(&a1, SEES).await && a1.sees(&b2, SEES).await, "reactivated during the event: back in it");
    let mut both = vec![bravo.slug.clone(), event.group.clone()];
    both.sort();
    assert_eq!(ldap_member_of(&bravo_user), both);

    // The host ends the event early: the group disappears at once.
    alpha_member.post(&on(&alpha, "/end"), json!({})).await.expect_error(403, "forbidden");
    let ended = Instant::now();
    assert_eq!(alpha.leader.post(&on(&alpha, "/end"), json!({})).await.ok()["synced"], true);
    println!("MEASURE end now: the request returned, group removed, after {:.2?}", ended.elapsed());
    assert!(ldap_group_members(&event.group).is_empty(), "no group and no member left when the call returns");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo", &b2)], SILENCE).await;
    assert!(crossed.is_empty(), "the event was ended but: {crossed:?}");
    open_connections(&[("alpha", &a1), ("bravo", &b2)]);
    assert_eq!(event_seen_by(&bravo.leader, &bravo.id, &event.id).await["phase"], "ended");
    assert!(mail_about(&bravo.leader.email, "has ended").await.text.contains(&alpha.name));
    // An event that is over cannot be changed any more.
    alpha.leader.patch(&on(&alpha, ""), later.clone()).await.expect_error(409, "event_closed");
    alpha.leader.post(&on(&alpha, "/end"), json!({})).await.expect_error(409, "event_closed");
    alpha.leader.post(&on(&alpha, "/cancel"), json!({})).await.expect_error(409, "event_closed");
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": charlie.id })).await.expect_error(409, "event_closed");
    bravo.leader.post(&on(&bravo, "/leave"), json!({})).await.expect_error(409, "event_closed");

    // A second event; an administrator cancels it while it runs.
    let second = create_event(&alpha, None, 3600).await;
    invite_to_event(&alpha, &second, &bravo).await;
    answer_event(&bravo, &second, true).await;
    assert!(a1.sees(&b2, SEES).await && b2.sees(&a1, SEES).await, "the second event is live");
    let listed = admin.get("/admin/events").await.ok();
    let row = listed["events"].as_array().unwrap().iter().find(|e| e["id"] == second.id.as_str()).expect("listed for the administrator").clone();
    assert_eq!(event_teams(&row), teams([format!("{}:accepted", bravo.name), format!("{}:host", alpha.name)]));
    assert_eq!((row["phase"].as_str(), row["permissions"]["cancel"].as_bool(), row["permissions"]["manage"].as_bool()), (Some("running"), Some(true), Some(false)));
    assert_eq!(admin.post(&format!("/admin/events/{}/cancel", second.id), json!({})).await.ok()["synced"], true);
    assert!(ldap_group_members(&second.group).is_empty(), "a cancelled event leaves no group behind");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1)], &[("bravo", &b2)], SILENCE).await;
    assert!(crossed.is_empty(), "the event was cancelled but: {crossed:?}");
    open_connections(&[("alpha", &a1), ("bravo", &b2)]);
    let cancelled = event_seen_by(&alpha.leader, &alpha.id, &second.id).await;
    assert_eq!((cancelled["phase"].as_str(), cancelled["cancel_reason"].as_str()), (Some("cancelled"), Some("admin")));
    for leader in [&alpha.leader, &bravo.leader] {
        assert!(mail_about(&leader.email, "is cancelled").await.subject.contains(&second.name), "both teams are told");
    }
    admin.post(&format!("/admin/events/{}/cancel", second.id), json!({})).await.expect_error(409, "event_closed");

    // A third event; its host team is deleted: the event is cancelled with it.
    let third = create_event(&alpha, None, 3600).await;
    invite_to_event(&alpha, &third, &bravo).await;
    answer_event(&bravo, &third, true).await;
    assert!(b2.sees(&a1, SEES).await);
    assert_eq!(alpha.leader.delete(&format!("/teams/{}", alpha.id)).await.ok()["synced"], true);
    assert!(a1.closed_within(Duration::from_secs(10)).await.is_some(), "the deleted team's access is revoked");
    assert!(ldap_group_members(&third.group).is_empty(), "the event's group went with its host");
    assert_eq!(ldap_member_of(&bravo_user), vec![bravo.slug.clone()]);
    open_connections(&[("bravo", &b2)]);
    let orphan = event_seen_by(&bravo.leader, &bravo.id, &third.id).await;
    assert_eq!((orphan["phase"].as_str(), orphan["cancel_reason"].as_str()), (Some("cancelled"), Some("host_deleted")));
    assert_eq!(orphan["host_team_name"], alpha.name.as_str(), "the other team still sees who hosted it");
    assert!(mail_about(&bravo.leader.email, &format!("{} is cancelled", third.name)).await.text.contains(&alpha.name));

    // None of this was drift.
    for group in [&event.group, &second.group, &third.group] {
        let journal = journal_of_group(&admin, group).await;
        assert!(!journal.is_empty() && journal.iter().all(|e| e["is_drift"] == false), "{group}: {journal:?}");
    }
}

// ------------------------------------------------------------------------------ rules

/// The rules that need no TAK client: names, dates, invitations and their answers, who sees
/// what, and the e-mails, each in its recipient's language.
#[tokio::test]
#[ignore = "needs the running stack"]
async fn event_rules_invitations_and_emails() {
    let admin = Session::admin().await;
    let alpha = approved_team(&admin, "shared", 5).await;
    let alpha_member = add_member(&alpha).await;
    // Bravo's leader reads French.
    let chef = Session::login(&email("chef"), "fr").await;
    let bravo_name = unique("Équipe Bravo ");
    let bravo_id = chef.post("/teams", json!({ "name": bravo_name, "mode": "shared" })).await.ok()["id"].as_str().unwrap().to_string();
    admin.post(&format!("/admin/teams/{bravo_id}/decision"), json!({ "approve": true })).await.ok();
    let bravo = TeamFixture { id: bravo_id, slug: String::new(), name: bravo_name, leader: chef };
    let bravo_member = add_member(&bravo).await;
    let pending = Session::new_account("pending").await;
    let pending_team = pending.post("/teams", json!({ "name": unique("Waiting "), "mode": "shared" })).await.ok();
    let pending_id = pending_team["id"].as_str().unwrap();
    let outsider = Session::new_account("outsider").await;
    let events_of = |team: &TeamFixture| format!("/teams/{}/events", team.id);

    // Creating: a leader of an active team, a name that gives a group name, dates that hold.
    let in_an_hour = iso(utc_in(3600));
    let in_two_hours = iso(utc_in(7200));
    let body = |name: &str| json!({ "name": name, "starts_at": in_an_hour, "ends_at": in_two_hours });
    alpha_member.post(&events_of(&alpha), body("By a member")).await.expect_error(403, "forbidden");
    outsider.post(&events_of(&alpha), body("By a stranger")).await.expect_error(404, "not_found");
    admin.post(&events_of(&alpha), body("By an administrator")).await.expect_error(403, "forbidden");
    pending.post(&format!("/teams/{pending_id}/events"), body("Too early")).await.expect_error(409, "team_not_active");
    alpha.leader.post(&events_of(&alpha), body("X")).await.expect_error(400, "invalid_input");
    alpha.leader.post(&events_of(&alpha), body("日本語")).await.expect_error(400, "invalid_event_name");
    for (starts_at, ends_at) in [
        (Some(utc_in(7200)), utc_in(3600)),     // ends before it starts
        (None, utc_in(-60)),                    // already over
        (None, utc_in(20)),                     // shorter than a minute
        (None, utc_in(400 * 86400)),            // longer than a year
        (Some(utc_in(800 * 86400)), utc_in(801 * 86400)), // too far ahead
    ] {
        alpha
            .leader
            .post(&events_of(&alpha), json!({ "name": "Bad dates", "starts_at": starts_at.map(iso), "ends_at": iso(ends_at) }))
            .await
            .expect_error(400, "invalid_event_dates");
    }
    let name = unique("Exercice d'Été ");
    let created = alpha.leader.post(&events_of(&alpha), body(&name)).await.ok();
    let (event_id, group) = (created["id"].as_str().unwrap().to_string(), created["group_name"].as_str().unwrap().to_string());
    assert!(group.starts_with("evt-exercice-d-ete-"), "accents folded, prefix added: {group}");
    let event = EventFixture { id: event_id, name: name.clone(), group: group.clone(), starts_at: parse_time(&created["starts_at"]), ends_at: parse_time(&created["ends_at"]) };
    let on = |team: &TeamFixture, action: &str| format!("/teams/{}/events/{}{action}", team.id, event.id);
    // The group name is taken while the event is not over, for every team.
    alpha.leader.post(&events_of(&alpha), body(&name)).await.expect_error(409, "event_name_taken");
    bravo.leader.post(&events_of(&bravo), body(&name.to_uppercase())).await.expect_error(409, "event_name_taken");
    // A team cannot take a name of that form (phase 1 kept the prefix).
    alpha.leader.post("/teams", json!({ "name": "Evt Exercise", "mode": "shared" })).await.expect_error(400, "invalid_team_name");

    // Finding a team to invite: the names of the other active teams, for leaders only.
    let listed = alpha.leader.get(&format!("/teams/{}/invitable-teams", alpha.id)).await.ok();
    let others = listed["teams"].as_array().unwrap();
    let found = others.iter().find(|t| t["id"] == bravo.id.as_str()).expect("bravo can be invited");
    assert_eq!(found, &json!({ "id": bravo.id, "name": bravo.name }), "a name and nothing else");
    assert!(!others.iter().any(|t| t["id"] == alpha.id.as_str()), "not one's own team");
    assert!(!others.iter().any(|t| t["id"] == pending_id), "nor a team that is not active");
    alpha_member.get(&format!("/teams/{}/invitable-teams", alpha.id)).await.expect_error(403, "forbidden");
    admin.get(&format!("/teams/{}/invitable-teams", alpha.id)).await.expect_error(403, "forbidden");
    outsider.get(&format!("/teams/{}/invitable-teams", alpha.id)).await.expect_error(404, "not_found");
    // Knowing a team's name opens nothing else about it.
    alpha.leader.get(&format!("/teams/{}", bravo.id)).await.expect_error(404, "not_found");
    alpha.leader.get(&events_of(&bravo)).await.expect_error(404, "not_found");

    // Inviting.
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": alpha.id })).await.expect_error(409, "team_already_in_event");
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": pending_id })).await.expect_error(404, "not_found");
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": "00000000-0000-4000-8000-000000000000" })).await.expect_error(404, "not_found");
    invite_to_event(&alpha, &event, &bravo).await;
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": bravo.id })).await.expect_error(409, "team_already_in_event");
    // The invitation reaches the leaders of the invited team, in their language, with the
    // dates written their way and in UTC.
    let mail = mail_about(&bravo.leader.email, "invite votre équipe").await;
    assert!(mail.subject.contains(&alpha.name) && mail.subject.contains(&name), "{}", mail.subject);
    let fr_date = |at: time::OffsetDateTime| {
        let format = time::macros::format_description!("[day]/[month]/[year] à [hour]:[minute] UTC");
        at.format(&format).unwrap()
    };
    assert!(mail.text.contains(&format!("Début : {}", fr_date(event.starts_at))), "{}", mail.text);
    assert!(mail.text.contains(&format!("Fin :   {}", fr_date(event.ends_at))), "{}", mail.text);
    assert!(mail.text.contains(&group) && mail.text.contains(&format!("/teams/{}", bravo.id)), "{}", mail.text);
    assert!(!mail_subjects(&bravo_member.email).await.iter().any(|s| s.contains(&name)), "members are not written to");

    // Who sees the invitation: the leaders of the invited team, not its members.
    let invited = event_seen_by(&bravo.leader, &bravo.id, &event.id).await;
    assert_eq!((invited["relation"].as_str(), invited["phase"].as_str()), (Some("invited"), Some("scheduled")));
    assert_eq!(invited["permissions"], json!({ "manage": false, "end": false, "cancel": false, "answer": true, "leave": false }));
    assert_eq!(event_teams(&invited), teams([format!("{}:host", alpha.name)]), "an invited team sees who takes part, not who else is invited");
    assert!(invited["teams"][0]["team_id"].is_null(), "and no team identifier");
    assert_eq!(event_seen_by(&bravo_member, &bravo.id, &event.id).await, Value::Null);
    // An invitation the caller may not see does not exist for them.
    bravo_member.post(&on(&bravo, "/answer"), json!({ "accept": true })).await.expect_error(404, "not_found");
    let invitations = |me: &Value| me["teams"].as_array().unwrap().iter().find(|t| t["id"] == bravo.id.as_str()).unwrap()["event_invitations"].clone();
    assert_eq!(invitations(&bravo.leader.get("/me").await.ok()), 1, "shown on the leader's home page");
    assert_eq!(invitations(&bravo_member.get("/me").await.ok()), 0);
    // The host's leaders see the whole invitation list; its members only who takes part.
    let hosted = event_seen_by(&alpha.leader, &alpha.id, &event.id).await;
    assert_eq!(event_teams(&hosted), teams([format!("{}:host", alpha.name), format!("{}:invited", bravo.name)]));
    assert_eq!(hosted["permissions"], json!({ "manage": true, "end": false, "cancel": true, "answer": false, "leave": false }));
    assert_eq!(event_teams(&event_seen_by(&alpha_member, &alpha.id, &event.id).await), teams([format!("{}:host", alpha.name)]));

    // Declining, being invited again, accepting, leaving; the host is told each time.
    alpha.leader.post(&on(&alpha, "/answer"), json!({ "accept": true })).await.expect_error(409, "event_invitation_not_pending");
    answer_event(&bravo, &event, false).await;
    assert!(mail_about(&alpha.leader.email, "declined your event").await.subject.contains(&bravo.name));
    assert_eq!(event_seen_by(&bravo.leader, &bravo.id, &event.id).await, Value::Null, "a declined event no longer concerns the team");
    bravo.leader.post(&on(&bravo, "/answer"), json!({ "accept": true })).await.expect_error(404, "not_found");
    assert!(event_teams(&event_seen_by(&alpha.leader, &alpha.id, &event.id).await).contains(&format!("{}:declined", bravo.name)));
    invite_to_event(&alpha, &event, &bravo).await;
    answer_event(&bravo, &event, true).await;
    assert!(mail_about(&alpha.leader.email, "joins your event").await.subject.contains(&bravo.name));
    bravo.leader.post(&on(&bravo, "/answer"), json!({ "accept": true })).await.expect_error(409, "event_invitation_not_pending");
    let joined = event_seen_by(&bravo_member, &bravo.id, &event.id).await;
    assert_eq!(event_teams(&joined), teams([format!("{}:accepted", bravo.name), format!("{}:host", alpha.name)]), "now its members see it");
    bravo.leader.post(&on(&bravo, "/leave"), json!({})).await.ok();
    assert!(mail_about(&alpha.leader.email, "left your event").await.subject.contains(&bravo.name));
    bravo.leader.post(&on(&bravo, "/leave"), json!({})).await.expect_error(404, "not_found");
    invite_to_event(&alpha, &event, &bravo).await;
    answer_event(&bravo, &event, true).await;

    // Changing the dates: both move while the event has not started; every team is told.
    alpha.leader.patch(&on(&alpha, ""), json!({ "starts_at": in_two_hours, "ends_at": in_an_hour })).await.expect_error(400, "invalid_event_dates");
    alpha.leader.patch(&on(&alpha, ""), json!({ "ends_at": iso(utc_in(-5)) })).await.expect_error(400, "invalid_event_dates");
    alpha.leader.post(&on(&alpha, "/end"), json!({})).await.expect_error(409, "event_not_running");
    let moved = alpha.leader.patch(&on(&alpha, ""), json!({ "starts_at": iso(utc_in(86400)), "ends_at": iso(utc_in(2 * 86400)) })).await.ok();
    let mail = mail_about(&bravo.leader.email, "Nouvelles dates").await;
    assert!(mail.text.contains(&fr_date(parse_time(&moved["starts_at"]))) && mail.text.contains(&fr_date(parse_time(&moved["ends_at"]))), "{}", mail.text);
    assert!(!mail_subjects(&alpha.leader.email).await.iter().any(|s| s.contains("New dates")), "not to the one who changed them");
    assert_eq!(event_seen_by(&bravo.leader, &bravo.id, &event.id).await["starts_at"], moved["starts_at"]);

    // The event never started: nothing was ever written to the directory for it.
    assert!(ldap_group_members(&group).is_empty());
    assert!(journal_of_group(&admin, &group).await.is_empty());

    // Cancelling; then nothing moves, and the name is free again.
    alpha_member.post(&on(&alpha, "/cancel"), json!({})).await.expect_error(403, "forbidden");
    alpha.leader.post(&on(&alpha, "/cancel"), json!({})).await.ok();
    assert!(mail_about(&bravo.leader.email, "est annulé").await.subject.contains(&name));
    let cancelled = event_seen_by(&bravo_member, &bravo.id, &event.id).await;
    assert_eq!((cancelled["phase"].as_str(), cancelled["cancel_reason"].as_str()), (Some("cancelled"), Some("host")));
    alpha.leader.patch(&on(&alpha, ""), json!({ "ends_at": iso(utc_in(3 * 86400)) })).await.expect_error(409, "event_closed");
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": bravo.id })).await.expect_error(409, "event_closed");
    bravo.leader.post(&on(&bravo, "/leave"), json!({})).await.expect_error(409, "event_closed");
    let again = bravo.leader.post(&events_of(&bravo), body(&name)).await.ok();
    assert_eq!(again["group_name"], group.as_str(), "the name of an event that is over can serve again");
    bravo.leader.post(&format!("/teams/{}/events/{}/cancel", bravo.id, again["id"].as_str().unwrap()), json!({})).await.ok();

    // The administrator sees every event, with the whole invitation list.
    let all = admin.get("/admin/events").await.ok();
    let row = all["events"].as_array().unwrap().iter().find(|e| e["id"] == event.id.as_str()).expect("listed").clone();
    assert_eq!(event_teams(&row), teams([format!("{}:accepted", bravo.name), format!("{}:host", alpha.name)]));
    assert_eq!(row["phase"], "cancelled");
    alpha.leader.get("/admin/events").await.expect_error(403, "forbidden");
}

// ------------------------------------------------------------- events of the administrators

/// An administrator organises an event and puts teams in it: no host team, no invitation to
/// answer. The teams see each other at once, an administrator runs the event from the
/// administration area, and a team can still leave it.
#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_administrator_organises_an_event_and_puts_teams_in_it() {
    let admin = Session::admin().await;
    let (alpha, _, a1) = team_with_device(&admin, "alpha").await;
    let (bravo, _, b1) = team_with_device(&admin, "bravo").await;
    let (charlie, _, c1) = team_with_device(&admin, "charlie").await;
    let alpha_member = add_member(&alpha).await;
    let pending = Session::new_account("pending").await;
    let pending_id = pending.post("/teams", json!({ "name": unique("Waiting "), "mode": "shared" })).await.ok()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let name = unique("Rassemblement ");
    let in_an_hour = iso(utc_in(3600));
    let body = |teams: Vec<&str>| json!({ "name": name, "ends_at": in_an_hour, "team_ids": teams });

    // Only an administrator organises, only active teams can be put in.
    alpha.leader.post("/admin/events", body(vec![&alpha.id])).await.expect_error(403, "forbidden");
    admin.post("/admin/events", body(vec![&alpha.id, &pending_id])).await.expect_error(404, "not_found");
    admin
        .post("/admin/events", json!({ "name": name, "ends_at": iso(utc_in(-60)), "team_ids": [alpha.id] }))
        .await
        .expect_error(400, "invalid_event_dates");

    // Created to start now, with two teams: they see each other when the call returns.
    let created = admin.post("/admin/events", body(vec![&alpha.id, &bravo.id, &alpha.id])).await.ok();
    assert_eq!(created["synced"], true);
    let event_id = created["id"].as_str().unwrap().to_string();
    let group = created["group_name"].as_str().unwrap().to_string();
    assert!(group.starts_with("evt-rassemblement-"), "{group}");
    assert!(a1.sees(&b1, SEES).await && b1.sees(&a1, SEES).await, "the teams put in take part at once");
    let crossed = crossings(&[("alpha", &a1), ("bravo", &b1)], &[("charlie", &c1)], SILENCE).await;
    assert!(crossed.is_empty(), "a team left out crossed into the event: {crossed:?}");
    let mut expected = vec![format!("{}-shared", alpha.slug), format!("{}-shared", bravo.slug)];
    expected.sort();
    assert_eq!(ldap_group_members(&group), expected);
    // The name is taken, for the teams too.
    admin.post("/admin/events", body(vec![])).await.expect_error(409, "event_name_taken");
    alpha.leader.post(&format!("/teams/{}/events", alpha.id), body(vec![])).await.expect_error(409, "event_name_taken");

    // The leaders of each team are told; their members are not.
    let mail = mail_about(&alpha.leader.email, "takes part in the event").await;
    assert!(mail.subject.contains(&name) && mail.text.contains(&group), "{}", mail.text);
    assert!(mail.text.contains(&format!("/teams/{}", alpha.id)), "{}", mail.text);
    assert!(!mail_subjects(&alpha_member.email).await.iter().any(|s| s.contains(&name)));

    // Seen from a team: who takes part, no host, and leaving is all a leader can do.
    let seen = event_seen_by(&alpha.leader, &alpha.id, &event_id).await;
    assert_eq!((seen["organizer"].as_str(), seen["relation"].as_str()), (Some("admin"), Some("accepted")));
    assert!(seen["host_team_name"].is_null());
    assert_eq!(event_teams(&seen), teams([format!("{}:accepted", alpha.name), format!("{}:accepted", bravo.name)]));
    assert_eq!(seen["permissions"], json!({ "manage": false, "end": false, "cancel": false, "answer": false, "leave": true }));
    assert_eq!(event_seen_by(&alpha_member, &alpha.id, &event_id).await["permissions"]["leave"], false);
    let on = |team: &TeamFixture, action: &str| format!("/teams/{}/events/{event_id}{action}", team.id);
    alpha.leader.post(&on(&alpha, "/teams"), json!({ "team_id": charlie.id })).await.expect_error(403, "forbidden");
    alpha.leader.post(&on(&alpha, "/cancel"), json!({})).await.expect_error(403, "forbidden");
    alpha.leader.post(&on(&alpha, "/end"), json!({})).await.expect_error(403, "forbidden");

    // Seen from the administration area: the administrator runs it.
    let admin_row = |list: &Value| list["events"].as_array().unwrap().iter().find(|e| e["id"] == event_id.as_str()).cloned().unwrap();
    let row = admin_row(&admin.get("/admin/events").await.ok());
    assert_eq!(row["permissions"], json!({ "manage": true, "end": true, "cancel": true, "answer": false, "leave": false }));
    assert!(row["teams"].as_array().unwrap().iter().all(|t| t["team_id"].is_string()));
    let path = |action: &str| format!("/admin/events/{event_id}{action}");

    // A team added while the event runs joins at once; twice is refused.
    let added = admin.post(&path("/teams"), json!({ "team_id": charlie.id })).await.ok();
    assert_eq!((added["name"].as_str(), added["synced"].as_bool()), (Some(charlie.name.as_str()), Some(true)));
    assert!(c1.sees(&a1, SEES).await && a1.sees(&c1, SEES).await, "charlie joins without reconnecting");
    admin.post(&path("/teams"), json!({ "team_id": charlie.id })).await.expect_error(409, "team_already_in_event");
    admin.post(&path("/teams"), json!({ "team_id": pending_id })).await.expect_error(404, "not_found");
    alpha.leader.post(&path("/teams"), json!({ "team_id": charlie.id })).await.expect_error(403, "forbidden");

    // A team leaves: the administrators are told, and can put it back.
    bravo.leader.post(&on(&bravo, "/leave"), json!({})).await.ok();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1), ("charlie", &c1)], &[("bravo", &b1)], SILENCE).await;
    assert!(crossed.is_empty(), "bravo left the event but: {crossed:?}");
    let left = mail_about(&admin.email, "left your event").await;
    assert!(left.subject.contains(&bravo.name) && left.text.contains("/admin"), "{}", left.text);
    admin.post(&path("/teams"), json!({ "team_id": bravo.id })).await.ok();
    assert!(b1.sees(&a1, SEES).await, "put back in by the administrator");

    // Taking a team out: it stops seeing the others at once, and its leaders are told.
    assert_eq!(admin.delete(&path(&format!("/teams/{}", charlie.id))).await.ok()["synced"], true);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let crossed = crossings(&[("alpha", &a1), ("bravo", &b1)], &[("charlie", &c1)], SILENCE).await;
    assert!(crossed.is_empty(), "charlie was taken out but: {crossed:?}");
    let removed = mail_about(&charlie.leader.email, "no longer part of the event").await;
    assert!(removed.text.contains("platform administration"), "{}", removed.text);
    open_connections(&[("alpha", &a1), ("bravo", &b1), ("charlie", &c1)]);

    // New dates: every team is told who changed them.
    admin.patch(&path(""), json!({ "ends_at": iso(utc_in(7200)) })).await.ok();
    let moved = mail_about(&alpha.leader.email, "New dates").await;
    assert!(moved.text.contains("platform administration"), "{}", moved.text);

    // An event hosted by a team stays the team's: the administrator only cancels it.
    let hosted = create_event(&alpha, Some(3600), 3600).await;
    let hosted_path = |action: &str| format!("/admin/events/{}{action}", hosted.id);
    admin.post(&hosted_path("/teams"), json!({ "team_id": charlie.id })).await.expect_error(403, "forbidden");
    admin.patch(&hosted_path(""), json!({ "ends_at": iso(utc_in(9000)) })).await.expect_error(403, "forbidden");
    admin.post(&hosted_path("/cancel"), json!({})).await.ok();

    // Ending it now: the group is gone when the call returns; then nothing moves.
    assert_eq!(admin.post(&path("/end"), json!({})).await.ok()["synced"], true);
    assert!(ldap_group_members(&group).is_empty(), "no group left once the event is over");
    assert!(mail_about(&bravo.leader.email, "has ended").await.text.contains("platform administration"));
    assert_eq!(admin_row(&admin.get("/admin/events").await.ok())["phase"], "ended");
    admin.post(&path("/teams"), json!({ "team_id": charlie.id })).await.expect_error(409, "event_closed");
    admin.post(&path("/end"), json!({})).await.expect_error(409, "event_closed");

    // None of this was drift.
    let journal = journal_of_group(&admin, &group).await;
    assert!(!journal.is_empty() && journal.iter().all(|e| e["is_drift"] == false), "{journal:?}");
}
