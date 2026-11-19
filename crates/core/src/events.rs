//! Cross-team events: a dated period during which the TAK accounts of several teams share
//! one more group, so their people see each other in ATAK.
//!
//! Nothing is stored about the state of an event beyond its two dates and a cancellation
//! date: the state is a function of the clock ([`phase`]), so an event starts and ends
//! without anyone writing anything, and a reconciliation pass run at any time gives every
//! account the groups it must have at that time ([`team_groups`]).

use std::collections::BTreeSet;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Longest event, start to end.
pub const MAX_EVENT_DURATION: Duration = Duration::days(366);
/// Shortest event: below this the group would come and go before anyone notices.
pub const MIN_EVENT_DURATION: Duration = Duration::minutes(1);
/// How far ahead an event may be planned.
pub const MAX_EVENT_LEAD: Duration = Duration::days(732);

/// The current time, at the precision PostgreSQL stores (microseconds), so a date computed
/// here and read back from the database compares equal.
pub fn clock_now() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    now.replace_nanosecond(now.nanosecond() / 1_000 * 1_000).unwrap_or(now)
}

/// Where an event stands at a given time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPhase {
    /// Not started yet: no group exists.
    Scheduled,
    /// Between its start and its end: the group holds the participating teams.
    Running,
    /// Past its end (reached, or brought forward by the host).
    Ended,
    /// Called off by the host or an administrator, or its host team was deleted.
    Cancelled,
}

impl EventPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            EventPhase::Scheduled => "scheduled",
            EventPhase::Running => "running",
            EventPhase::Ended => "ended",
            EventPhase::Cancelled => "cancelled",
        }
    }

    /// An event that is over can no longer be changed, and no longer holds its group name.
    pub fn is_open(self) -> bool {
        matches!(self, EventPhase::Scheduled | EventPhase::Running)
    }
}

/// The phase of an event at `now`. The start is inclusive and the end exclusive, so two
/// events that follow each other never overlap.
pub fn phase(
    starts_at: OffsetDateTime,
    ends_at: OffsetDateTime,
    cancelled_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> EventPhase {
    if cancelled_at.is_some() {
        EventPhase::Cancelled
    } else if now < starts_at {
        EventPhase::Scheduled
    } else if now < ends_at {
        EventPhase::Running
    } else {
        EventPhase::Ended
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventError {
    /// The event is over (ended or cancelled): nothing about it can change.
    Closed,
    /// The dates make no event: end before start, end already past, too short, too long,
    /// too far ahead, or a start moved after the event has begun.
    InvalidDates,
    /// Only a running event can be ended early.
    NotRunning,
    /// The team is already invited or already takes part.
    AlreadyInEvent,
    /// There is no invitation waiting for an answer.
    NotInvited,
    /// The team does not take part in the event.
    NotParticipating,
}

/// Checks the dates of a new event and returns them as they will be stored. A missing start,
/// or one already past, means "now": an event may start immediately.
pub fn new_event_dates(
    starts_at: Option<OffsetDateTime>,
    ends_at: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<(OffsetDateTime, OffsetDateTime), EventError> {
    let starts_at = starts_at.filter(|s| *s > now).unwrap_or(now);
    check_span(starts_at, ends_at, now)?;
    Ok((starts_at, ends_at))
}

/// Checks a change of dates on an existing event and returns the dates to store.
///
/// A scheduled event can move both dates. A running one has started: its start stays, only
/// its end moves (to end it now, see [`end_now`]).
pub fn changed_event_dates(
    current: (OffsetDateTime, OffsetDateTime),
    cancelled_at: Option<OffsetDateTime>,
    starts_at: Option<OffsetDateTime>,
    ends_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<(OffsetDateTime, OffsetDateTime), EventError> {
    let (current_start, current_end) = current;
    match phase(current_start, current_end, cancelled_at, now) {
        EventPhase::Ended | EventPhase::Cancelled => Err(EventError::Closed),
        EventPhase::Running => {
            if starts_at.is_some_and(|s| s != current_start) {
                return Err(EventError::InvalidDates);
            }
            let ends_at = ends_at.unwrap_or(current_end);
            check_span(current_start, ends_at, now)?;
            Ok((current_start, ends_at))
        }
        EventPhase::Scheduled => {
            let starts_at = starts_at.map(|s| if s > now { s } else { now }).unwrap_or(current_start);
            let ends_at = ends_at.unwrap_or(current_end);
            check_span(starts_at, ends_at, now)?;
            Ok((starts_at, ends_at))
        }
    }
}

fn check_span(starts_at: OffsetDateTime, ends_at: OffsetDateTime, now: OffsetDateTime) -> Result<(), EventError> {
    let length = ends_at - starts_at;
    let ok = length >= MIN_EVENT_DURATION
        && length <= MAX_EVENT_DURATION
        && ends_at > now
        && starts_at - now <= MAX_EVENT_LEAD;
    if ok {
        Ok(())
    } else {
        Err(EventError::InvalidDates)
    }
}

/// The end date that ends a running event at once.
pub fn end_now(
    starts_at: OffsetDateTime,
    ends_at: OffsetDateTime,
    cancelled_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, EventError> {
    match phase(starts_at, ends_at, cancelled_at, now) {
        EventPhase::Running => Ok(now),
        EventPhase::Scheduled => Err(EventError::NotRunning),
        EventPhase::Ended | EventPhase::Cancelled => Err(EventError::Closed),
    }
}

/// Where an invited team stands in an event. The host team is not in this list: it takes
/// part by definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Participation {
    /// Invited by the host, no answer yet.
    Invited,
    /// Takes part: its accounts get the event's group while the event runs.
    Accepted,
    Declined,
    /// Had accepted, then left.
    Left,
    /// Taken out by the host (an invitation withdrawn, or a participating team removed).
    Removed,
}

impl Participation {
    pub fn as_str(self) -> &'static str {
        match self {
            Participation::Invited => "invited",
            Participation::Accepted => "accepted",
            Participation::Declined => "declined",
            Participation::Left => "left",
            Participation::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "invited" => Some(Participation::Invited),
            "accepted" => Some(Participation::Accepted),
            "declined" => Some(Participation::Declined),
            "left" => Some(Participation::Left),
            "removed" => Some(Participation::Removed),
            _ => None,
        }
    }
}

/// What can happen to a team's participation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipationChange {
    /// The host invites the team (again, after it declined, left or was removed).
    Invite,
    /// The invited team says yes.
    Accept,
    /// The invited team says no.
    Decline,
    /// A participating team walks out.
    Leave,
    /// The host withdraws the invitation or takes the team out.
    Remove,
}

/// The single rule for how a team's participation moves. `current` is `None` for a team the
/// event does not know yet. Nothing moves once the event is over.
pub fn next_participation(
    event: EventPhase,
    current: Option<Participation>,
    change: ParticipationChange,
) -> Result<Participation, EventError> {
    use Participation::*;
    if !event.is_open() {
        return Err(EventError::Closed);
    }
    match (change, current) {
        (ParticipationChange::Invite, None | Some(Declined | Left | Removed)) => Ok(Invited),
        (ParticipationChange::Invite, Some(Invited | Accepted)) => Err(EventError::AlreadyInEvent),
        (ParticipationChange::Accept, Some(Invited)) => Ok(Accepted),
        (ParticipationChange::Decline, Some(Invited)) => Ok(Declined),
        (ParticipationChange::Accept | ParticipationChange::Decline, _) => Err(EventError::NotInvited),
        (ParticipationChange::Leave, Some(Accepted)) => Ok(Left),
        (ParticipationChange::Leave, _) => Err(EventError::NotParticipating),
        (ParticipationChange::Remove, Some(Invited | Accepted)) => Ok(Removed),
        (ParticipationChange::Remove, _) => Err(EventError::NotParticipating),
    }
}

/// An event as the reconciliation needs it: its group, its dates, and the teams that take
/// part (the host and every team that accepted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventGroup {
    pub group: String,
    pub starts_at: OffsetDateTime,
    pub ends_at: OffsetDateTime,
    pub cancelled_at: Option<OffsetDateTime>,
    pub teams: BTreeSet<Uuid>,
}

impl EventGroup {
    pub fn is_running(&self, now: OffsetDateTime) -> bool {
        phase(self.starts_at, self.ends_at, self.cancelled_at, now) == EventPhase::Running
    }
}

/// The TAK groups every account of a team must be in at `now`: the team's own group, and
/// the group of each running event the team takes part in.
///
/// A team that is not active (pending, rejected, suspended) has no group at all: it loses
/// its event groups exactly as it loses its own, and gets them back when it is reactivated.
pub fn team_groups(
    team_id: Uuid,
    team_group: &str,
    team_is_active: bool,
    events: &[EventGroup],
    now: OffsetDateTime,
) -> BTreeSet<String> {
    if !team_is_active {
        return BTreeSet::new();
    }
    let mut groups = BTreeSet::from([team_group.to_string()]);
    for event in events {
        if event.is_running(now) && event.teams.contains(&team_id) {
            groups.insert(event.group.clone());
        }
    }
    groups
}

/// The next instant after `after` at which an event starts or ends, that is the next time
/// the groups change without anyone doing anything.
pub fn next_boundary(events: &[EventGroup], after: OffsetDateTime) -> Option<OffsetDateTime> {
    events
        .iter()
        .filter(|e| e.cancelled_at.is_none())
        .flat_map(|e| [e.starts_at, e.ends_at])
        .filter(|t| *t > after)
        .min()
}

/// Groups of the events that started or ended in `(since, until]`: what a pass run at
/// `until` is expected to change because of the schedule alone, when the previous pass ran
/// at `since`.
pub fn groups_due(events: &[EventGroup], since: OffsetDateTime, until: OffsetDateTime) -> BTreeSet<String> {
    events
        .iter()
        .filter(|e| e.cancelled_at.is_none())
        .filter(|e| [e.starts_at, e.ends_at].iter().any(|t| *t > since && *t <= until))
        .map(|e| e.group.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-11-20 08:00 UTC);

    fn at(minutes: i64) -> OffsetDateTime {
        T0 + Duration::minutes(minutes)
    }

    fn event(group: &str, start: i64, end: i64, teams: &[&str]) -> EventGroup {
        EventGroup {
            group: group.into(),
            starts_at: at(start),
            ends_at: at(end),
            cancelled_at: None,
            teams: teams.iter().map(|t| team_id(t)).collect(),
        }
    }

    /// A stable identifier per team name, so the tests read with names.
    fn team_id(slug: &str) -> Uuid {
        Uuid::from_u128(slug.bytes().fold(0u128, |acc, b| acc * 257 + b as u128))
    }

    fn groups(slug: &str, events: &[EventGroup], minutes: i64) -> Vec<String> {
        team_groups(team_id(slug), slug, true, events, at(minutes)).into_iter().collect()
    }

    #[test]
    fn phase_follows_the_clock() {
        let (start, end) = (at(60), at(120));
        assert_eq!(phase(start, end, None, at(0)), EventPhase::Scheduled);
        assert_eq!(phase(start, end, None, at(60)), EventPhase::Running, "the start is inclusive");
        assert_eq!(phase(start, end, None, at(119)), EventPhase::Running);
        assert_eq!(phase(start, end, None, at(120)), EventPhase::Ended, "the end is exclusive");
        assert_eq!(phase(start, end, None, at(500)), EventPhase::Ended);
        for now in [0, 60, 90, 500] {
            assert_eq!(phase(start, end, Some(at(10)), at(now)), EventPhase::Cancelled, "cancelled whatever the time");
        }
        assert!(EventPhase::Scheduled.is_open() && EventPhase::Running.is_open());
        assert!(!EventPhase::Ended.is_open() && !EventPhase::Cancelled.is_open());
    }

    #[test]
    fn a_team_gets_the_event_group_only_while_the_event_runs() {
        let events = [event("evt-exercise", 60, 120, &["alpha", "bravo"])];
        assert_eq!(groups("alpha", &events, 59), ["alpha"], "not before the start");
        assert_eq!(groups("alpha", &events, 60), ["alpha", "evt-exercise"]);
        assert_eq!(groups("bravo", &events, 90), ["bravo", "evt-exercise"]);
        assert_eq!(groups("bravo", &events, 120), ["bravo"], "gone at the end");
        assert_eq!(groups("charlie", &events, 90), ["charlie"], "a team that does not take part never gets it");
    }

    #[test]
    fn a_team_that_is_not_active_has_no_group_at_all() {
        let events = [event("evt-exercise", 0, 120, &["alpha", "bravo"])];
        let suspended = team_groups(team_id("alpha"), "alpha", false, &events, at(30));
        assert!(suspended.is_empty(), "suspended: neither its own group nor the event's");
        assert_eq!(groups("bravo", &events, 30), ["bravo", "evt-exercise"], "the other participant keeps the event group");
    }

    #[test]
    fn a_cancelled_event_gives_no_group() {
        let mut cancelled = event("evt-exercise", 0, 120, &["alpha", "bravo"]);
        cancelled.cancelled_at = Some(at(30));
        assert_eq!(groups("alpha", &[cancelled], 40), ["alpha"]);
    }

    #[test]
    fn several_events_add_up() {
        let events = [
            event("evt-one", 0, 100, &["alpha", "bravo"]),
            event("evt-two", 50, 200, &["alpha", "charlie"]),
            event("evt-three", 300, 400, &["alpha"]),
        ];
        assert_eq!(groups("alpha", &events, 10), ["alpha", "evt-one"]);
        assert_eq!(groups("alpha", &events, 60), ["alpha", "evt-one", "evt-two"]);
        assert_eq!(groups("alpha", &events, 150), ["alpha", "evt-two"]);
        assert_eq!(groups("alpha", &events, 250), ["alpha"]);
        assert_eq!(groups("charlie", &events, 60), ["charlie", "evt-two"], "charlie never sees bravo's event");
    }

    #[test]
    fn a_name_freed_by_an_ended_event_can_serve_again() {
        // Two events with the same group name, one after the other.
        let events = [event("evt-drill", 0, 60, &["alpha", "bravo"]), event("evt-drill", 60, 120, &["alpha", "charlie"])];
        assert_eq!(groups("bravo", &events, 30), ["bravo", "evt-drill"]);
        assert_eq!(groups("bravo", &events, 60), ["bravo"], "bravo is out the instant the first one ends");
        assert_eq!(groups("charlie", &events, 60), ["charlie", "evt-drill"]);
        assert_eq!(groups("alpha", &events, 60), ["alpha", "evt-drill"]);
    }

    #[test]
    fn next_boundary_is_the_next_start_or_end() {
        let mut cancelled = event("evt-off", 5, 15, &["alpha"]);
        cancelled.cancelled_at = Some(at(1));
        let events = [event("evt-one", 60, 120, &["alpha"]), event("evt-two", 90, 100, &["bravo"]), cancelled];
        assert_eq!(next_boundary(&events, at(0)), Some(at(60)), "a cancelled event has no boundary");
        assert_eq!(next_boundary(&events, at(60)), Some(at(90)), "strictly after");
        assert_eq!(next_boundary(&events, at(95)), Some(at(100)));
        assert_eq!(next_boundary(&events, at(100)), Some(at(120)));
        assert_eq!(next_boundary(&events, at(120)), None);
        assert_eq!(next_boundary(&[], at(0)), None);
    }

    #[test]
    fn groups_due_names_what_the_schedule_changed_between_two_passes() {
        let events = [event("evt-one", 60, 120, &["alpha"]), event("evt-two", 90, 300, &["bravo"])];
        assert!(groups_due(&events, at(0), at(59)).is_empty());
        assert_eq!(groups_due(&events, at(59), at(60)).into_iter().collect::<Vec<_>>(), ["evt-one"]);
        assert!(groups_due(&events, at(60), at(80)).is_empty(), "already applied by the pass run at the start");
        // The platform was stopped from 80 to 130: one event started, the other ended.
        assert_eq!(groups_due(&events, at(80), at(130)).into_iter().collect::<Vec<_>>(), ["evt-one", "evt-two"]);
    }

    #[test]
    fn dates_of_a_new_event() {
        let now = at(0);
        // No start, or a start already past: the event starts now.
        assert_eq!(new_event_dates(None, at(60), now), Ok((now, at(60))));
        assert_eq!(new_event_dates(Some(at(-5)), at(60), now), Ok((now, at(60))));
        assert_eq!(new_event_dates(Some(at(30)), at(60), now), Ok((at(30), at(60))));
        // End before the start, already past, or not even a minute after the start.
        assert_eq!(new_event_dates(Some(at(60)), at(30), now), Err(EventError::InvalidDates));
        assert_eq!(new_event_dates(None, at(-1), now), Err(EventError::InvalidDates));
        assert_eq!(new_event_dates(Some(at(30)), at(30), now), Err(EventError::InvalidDates));
        assert_eq!(new_event_dates(None, now + Duration::seconds(30), now), Err(EventError::InvalidDates));
        // Too long, too far ahead.
        assert_eq!(new_event_dates(None, now + MAX_EVENT_DURATION + Duration::minutes(1), now), Err(EventError::InvalidDates));
        assert!(new_event_dates(None, now + MAX_EVENT_DURATION, now).is_ok());
        let far = now + MAX_EVENT_LEAD + Duration::days(1);
        assert_eq!(new_event_dates(Some(far), far + Duration::hours(1), now), Err(EventError::InvalidDates));
    }

    #[test]
    fn changing_the_dates() {
        let current = (at(60), at(120));
        // Scheduled: both dates move; one alone keeps the other.
        assert_eq!(changed_event_dates(current, None, Some(at(30)), Some(at(90)), at(0)), Ok((at(30), at(90))));
        assert_eq!(changed_event_dates(current, None, None, Some(at(300)), at(0)), Ok((at(60), at(300))));
        assert_eq!(changed_event_dates(current, None, Some(at(100)), None, at(0)), Ok((at(100), at(120))));
        assert_eq!(changed_event_dates(current, None, Some(at(-10)), None, at(0)), Ok((at(0), at(120))), "a past start means now");
        assert_eq!(changed_event_dates(current, None, Some(at(130)), None, at(0)), Err(EventError::InvalidDates));
        // Running: the start is history, the end can still move, but not into the past.
        assert_eq!(changed_event_dates(current, None, None, Some(at(240)), at(90)), Ok((at(60), at(240))));
        assert_eq!(changed_event_dates(current, None, Some(at(60)), Some(at(240)), at(90)), Ok((at(60), at(240))));
        assert_eq!(changed_event_dates(current, None, Some(at(100)), None, at(90)), Err(EventError::InvalidDates));
        assert_eq!(changed_event_dates(current, None, None, Some(at(80)), at(90)), Err(EventError::InvalidDates));
        // Over: nothing moves.
        assert_eq!(changed_event_dates(current, None, None, Some(at(240)), at(120)), Err(EventError::Closed));
        assert_eq!(changed_event_dates(current, Some(at(10)), None, Some(at(240)), at(20)), Err(EventError::Closed));
    }

    #[test]
    fn ending_early() {
        assert_eq!(end_now(at(60), at(120), None, at(90)), Ok(at(90)));
        assert_eq!(end_now(at(60), at(120), None, at(10)), Err(EventError::NotRunning), "a scheduled event is cancelled, not ended");
        assert_eq!(end_now(at(60), at(120), None, at(120)), Err(EventError::Closed));
        assert_eq!(end_now(at(60), at(120), Some(at(70)), at(90)), Err(EventError::Closed));
        // Once ended early, the event is over from that instant.
        let ended = end_now(at(60), at(120), None, at(90)).unwrap();
        assert_eq!(phase(at(60), ended, None, at(90)), EventPhase::Ended);
    }

    #[test]
    fn participation_transitions() {
        use Participation::*;
        use ParticipationChange as C;
        let open = EventPhase::Running;
        // The path of an invitation.
        assert_eq!(next_participation(open, None, C::Invite), Ok(Invited));
        assert_eq!(next_participation(open, Some(Invited), C::Accept), Ok(Accepted));
        assert_eq!(next_participation(open, Some(Invited), C::Decline), Ok(Declined));
        assert_eq!(next_participation(open, Some(Accepted), C::Leave), Ok(Left));
        assert_eq!(next_participation(open, Some(Invited), C::Remove), Ok(Removed), "the host withdraws an invitation");
        assert_eq!(next_participation(open, Some(Accepted), C::Remove), Ok(Removed));
        // A team that said no, left or was removed can be invited again; it must answer again.
        for previous in [Declined, Left, Removed] {
            assert_eq!(next_participation(open, Some(previous), C::Invite), Ok(Invited));
            assert_eq!(next_participation(open, Some(previous), C::Accept), Err(EventError::NotInvited), "{previous:?}");
            assert_eq!(next_participation(open, Some(previous), C::Leave), Err(EventError::NotParticipating));
            assert_eq!(next_participation(open, Some(previous), C::Remove), Err(EventError::NotParticipating));
        }
        // No double invitation, no answer without an invitation, no leaving before accepting.
        assert_eq!(next_participation(open, Some(Invited), C::Invite), Err(EventError::AlreadyInEvent));
        assert_eq!(next_participation(open, Some(Accepted), C::Invite), Err(EventError::AlreadyInEvent));
        assert_eq!(next_participation(open, None, C::Accept), Err(EventError::NotInvited));
        assert_eq!(next_participation(open, Some(Accepted), C::Accept), Err(EventError::NotInvited));
        assert_eq!(next_participation(open, Some(Accepted), C::Decline), Err(EventError::NotInvited));
        assert_eq!(next_participation(open, Some(Invited), C::Leave), Err(EventError::NotParticipating));
        assert_eq!(next_participation(open, None, C::Remove), Err(EventError::NotParticipating));
        // Before the start everything works the same.
        assert_eq!(next_participation(EventPhase::Scheduled, Some(Invited), C::Accept), Ok(Accepted));
    }

    #[test]
    fn nothing_moves_once_the_event_is_over() {
        use ParticipationChange as C;
        for closed in [EventPhase::Ended, EventPhase::Cancelled] {
            for current in [None, Some(Participation::Invited), Some(Participation::Accepted), Some(Participation::Declined)] {
                for change in [C::Invite, C::Accept, C::Decline, C::Leave, C::Remove] {
                    assert_eq!(next_participation(closed, current, change), Err(EventError::Closed), "{closed:?} {current:?} {change:?}");
                }
            }
        }
    }

    #[test]
    fn the_clock_has_the_precision_of_the_database() {
        let now = clock_now();
        assert_eq!(now.nanosecond() % 1_000, 0);
        assert!((OffsetDateTime::now_utc() - now).abs() < Duration::seconds(1));
    }

    #[test]
    fn participation_names_round_trip() {
        use Participation::*;
        for p in [Invited, Accepted, Declined, Left, Removed] {
            assert_eq!(Participation::parse(p.as_str()), Some(p));
        }
        assert_eq!(Participation::parse("host"), None);
    }
}
