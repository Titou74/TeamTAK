//! Cross-team events: a team creates a dated event and invites other teams; for its
//! duration the TAK accounts of the host and of every team that accepted share a dedicated
//! group, shown as a channel in ATAK.
//!
//! Nothing here writes to OpenLDAP or TAK Server. A handler changes the database, then runs
//! a reconciliation pass, which gives each account the groups it must have at that instant
//! (`Reconciler::desired`). The start and the end of an event are applied the same way, by
//! a pass the scheduler runs at that time.
//!
//! Every request names the team the caller acts for (`/teams/{id}/events/…`): the same
//! account can lead the host team and be a plain member of an invited one.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tak_manager_core::events::{
    changed_event_dates, clock_now, end_now, new_event_dates, next_participation, EventPhase, Participation,
    ParticipationChange,
};
use tak_manager_core::names::event_group_name;
use tak_manager_core::policy::{can, Action, Actor};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{Account, AdminAccount, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::locale::{self, Template};
use crate::state::SharedState;
use crate::teams::{apply_change, team_context, TeamContext};

/// Events a team may host at the same time (scheduled or running).
const MAX_OPEN_EVENTS_PER_TEAM: i64 = 20;
/// Finished events shown in a team's list.
const SHOWN_PAST_EVENTS: usize = 20;
/// Serialises event creations, so two requests cannot take the same group name.
const EVENT_NAME_LOCK: i64 = 7205002;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EventRow {
    pub id: Uuid,
    pub name: String,
    pub group_name: String,
    pub host_team_id: Option<Uuid>,
    pub host_team_name: String,
    pub starts_at: OffsetDateTime,
    pub ends_at: OffsetDateTime,
    pub cancelled_at: Option<OffsetDateTime>,
    pub cancel_reason: Option<String>,
}

/// Columns of [`EventRow`]. A macro, because sqlx only accepts SQL known at compile time.
macro_rules! event_columns {
    () => {
        "e.id, e.name, e.group_name, e.host_team_id, e.host_team_name, e.starts_at, e.ends_at, e.cancelled_at, e.cancel_reason"
    };
}

impl EventRow {
    fn phase(&self, now: OffsetDateTime) -> EventPhase {
        tak_manager_core::events::phase(self.starts_at, self.ends_at, self.cancelled_at, now)
    }
}

/// An event with the participation of the team it is looked at from, when that team was
/// invited to it.
#[derive(Debug, sqlx::FromRow)]
struct EventForTeam {
    participation: Option<String>,
    #[sqlx(flatten)]
    event: EventRow,
}

/// An invited team of an event.
#[derive(Debug, Clone, sqlx::FromRow)]
struct TeamLink {
    event_id: Uuid,
    team_id: Uuid,
    name: String,
    status: String,
}

/// How the team the caller acts for relates to an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    Host,
    Invited,
    Accepted,
}

impl Relation {
    fn as_str(self) -> &'static str {
        match self {
            Relation::Host => "host",
            Relation::Invited => "invited",
            Relation::Accepted => "accepted",
        }
    }

    /// The relation a team has through its row in `event_teams`. A team that declined, left
    /// or was removed is no longer concerned: the event does not exist for it.
    fn of_status(status: &str) -> Option<Self> {
        match Participation::parse(status)? {
            Participation::Invited => Some(Relation::Invited),
            Participation::Accepted => Some(Relation::Accepted),
            Participation::Declined | Participation::Left | Participation::Removed => None,
        }
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// An event as JSON for one viewer.
///
/// Everyone who sees the event sees its name, dates, state, and the teams that take part
/// (the host and those that accepted). The rest of the invitation list (waiting, declined,
/// left, removed) and the team identifiers are for those who run the event, the leaders of
/// the host team, and for administrators.
fn event_view(event: &EventRow, links: &[&TeamLink], relation: Option<Relation>, actor: Actor, now: OffsetDateTime) -> Value {
    let phase = event.phase(now);
    let open = phase.is_open();
    let hosts = relation == Some(Relation::Host);
    let manage = hosts && open && can(actor, Action::ManageEvent);
    let everything = actor.is_admin || (hosts && can(actor, Action::ManageEvent));
    let id_of = |id: Uuid| everything.then_some(id);

    let mut teams = vec![json!({ "team_id": event.host_team_id.and_then(id_of), "name": event.host_team_name, "status": "host" })];
    for link in links {
        if everything || link.status == Participation::Accepted.as_str() {
            teams.push(json!({ "team_id": id_of(link.team_id), "name": link.name, "status": link.status }));
        }
    }
    json!({
        "id": event.id,
        "name": event.name,
        "group_name": event.group_name,
        "starts_at": rfc3339(event.starts_at),
        "ends_at": rfc3339(event.ends_at),
        "cancelled_at": event.cancelled_at.map(rfc3339),
        "cancel_reason": event.cancel_reason,
        "phase": phase.as_str(),
        "host_team_name": event.host_team_name,
        "relation": relation.map(Relation::as_str),
        "teams": teams,
        "permissions": {
            "manage": manage,
            "end": manage && phase == EventPhase::Running,
            "cancel": open && (actor.is_admin || (hosts && can(actor, Action::CancelEvent))),
            "answer": open && relation == Some(Relation::Invited) && can(actor, Action::AnswerEventInvitation),
            "leave": open && relation == Some(Relation::Accepted) && can(actor, Action::AnswerEventInvitation),
        },
    })
}

/// Running and scheduled events first, in the order they start; then the finished ones,
/// the latest first.
fn sort_events(events: &mut [(EventRow, Option<Relation>)], now: OffsetDateTime) {
    events.sort_by_key(|(e, _)| {
        let closed_at = e.cancelled_at.unwrap_or(e.ends_at);
        if e.phase(now).is_open() {
            (0, e.starts_at.unix_timestamp_nanos())
        } else {
            (1, -closed_at.unix_timestamp_nanos())
        }
    });
}

async fn links_of(state: &SharedState, event_ids: &[Uuid]) -> ApiResult<BTreeMap<Uuid, Vec<TeamLink>>> {
    let rows: Vec<TeamLink> = sqlx::query_as(
        "SELECT p.event_id, p.team_id, t.name, p.status
           FROM event_teams p JOIN teams t ON t.id = p.team_id
          WHERE p.event_id = ANY($1)
          ORDER BY p.invited_at, t.name",
    )
    .bind(event_ids)
    .fetch_all(&state.db)
    .await?;
    let mut by_event: BTreeMap<Uuid, Vec<TeamLink>> = BTreeMap::new();
    for row in rows {
        by_event.entry(row.event_id).or_default().push(row);
    }
    Ok(by_event)
}

async fn views(
    state: &SharedState,
    events: &[(EventRow, Option<Relation>)],
    actor: Actor,
    now: OffsetDateTime,
) -> ApiResult<Vec<Value>> {
    let ids: Vec<Uuid> = events.iter().map(|(e, _)| e.id).collect();
    let links = links_of(state, &ids).await?;
    Ok(events
        .iter()
        .map(|(event, relation)| {
            let of_event: Vec<&TeamLink> = links.get(&event.id).map(|l| l.iter().collect()).unwrap_or_default();
            event_view(event, &of_event, *relation, actor, now)
        })
        .collect())
}

/// `GET /api/teams/{id}/events`: the events the team takes part in, hosts, or is invited to.
pub async fn list_team_events(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ViewEvents)?;
    let now = clock_now();
    let rows: Vec<EventForTeam> = sqlx::query_as(concat!(
        "SELECT p.status AS participation, ",
        event_columns!(),
        " FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $1
          WHERE e.host_team_id = $1 OR p.status IN ('invited', 'accepted')"
    ))
    .bind(team_id)
    .fetch_all(&state.db)
    .await?;

    let sees_invitations = can(ctx.actor, Action::ViewEventInvitations);
    let mut events: Vec<(EventRow, Option<Relation>)> = rows
        .into_iter()
        .filter_map(|EventForTeam { participation, event }| {
            let relation = if event.host_team_id == Some(team_id) {
                Relation::Host
            } else {
                Relation::of_status(participation.as_deref()?)?
            };
            // An invitation concerns those who can answer it, and only while it can be answered.
            let shown = relation != Relation::Invited || (sees_invitations && event.phase(now).is_open());
            shown.then_some((event, Some(relation)))
        })
        .collect();
    sort_events(&mut events, now);
    let open = events.iter().filter(|(e, _)| e.phase(now).is_open()).count();
    events.truncate(open + SHOWN_PAST_EVENTS);

    Ok(Json(json!({
        "events": views(&state, &events, ctx.actor, now).await?,
        "permissions": {
            "create": ctx.team.require_active().is_ok() && can(ctx.actor, Action::CreateEvent),
        },
    })))
}

/// `GET /api/teams/{id}/invitable-teams`: the names of the other active teams, for a leader
/// to pick the ones to invite. Nothing else about them is given out.
pub async fn invitable_teams(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ListInvitableTeams)?;
    ctx.team.require_active()?;
    let teams: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM teams WHERE status = 'active' AND id <> $1 ORDER BY lower(name), id")
            .bind(team_id)
            .fetch_all(&state.db)
            .await?;
    let teams: Vec<Value> = teams.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    Ok(Json(json!({ "teams": teams })))
}

#[derive(Deserialize)]
pub struct CreateEventBody {
    name: String,
    /// Missing: the event starts now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    starts_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    ends_at: OffsetDateTime,
}

/// `POST /api/teams/{id}/events`: a leader creates an event hosted by their team. No
/// administrator is involved.
pub async fn create_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<CreateEventBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::CreateEvent)?;
    ctx.team.require_active()?;
    let name = body.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if !(2..=60).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err(ApiError::InvalidInput);
    }
    let group_name = event_group_name(&name).ok_or(ApiError::InvalidEventName)?;
    let now = clock_now();
    let (starts_at, ends_at) = new_event_dates(body.starts_at, body.ends_at, now)?;

    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(EVENT_NAME_LOCK).execute(&mut *tx).await?;
    let (hosted, name_taken): (i64, bool) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM events WHERE host_team_id = $1 AND cancelled_at IS NULL AND ends_at > $3),
                EXISTS (SELECT 1 FROM events WHERE group_name = $2 AND cancelled_at IS NULL AND ends_at > $3)",
    )
    .bind(team_id)
    .bind(&group_name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    if name_taken {
        return Err(ApiError::EventNameTaken);
    }
    if hosted >= MAX_OPEN_EVENTS_PER_TEAM {
        return Err(ApiError::TooManyEvents);
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO events (id, name, group_name, host_team_id, host_team_name, starts_at, ends_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(&name)
    .bind(&group_name)
    .bind(team_id)
    .bind(&ctx.team.name)
    .bind(starts_at)
    .bind(ends_at)
    .bind(account.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(event = %group_name, host = %ctx.team.slug, %starts_at, %ends_at, by = %account.email, "event created");
    // Live at once when it starts now; otherwise the pass only wakes the scheduler.
    let synced = apply_change(&state).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "group_name": group_name,
            "starts_at": rfc3339(starts_at),
            "ends_at": rfc3339(ends_at),
            "synced": synced,
        })),
    ))
}

/// An event as seen from a team.
struct EventContext {
    ctx: TeamContext,
    relation: Relation,
}

impl EventContext {
    /// Refuses unless the team hosts the event and the caller may do `action` in it.
    fn require_host(&self, action: Action) -> ApiResult<()> {
        // The event is visible to the caller, so "forbidden" tells them nothing new.
        if self.relation == Relation::Host {
            self.ctx.require(action)
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

/// Loads an event for the team the caller acts for. An event the team is not concerned by,
/// or that the caller may not see, answers "not found".
async fn event_context(state: &SharedState, team_id: Uuid, event_id: Uuid, account: &Account) -> ApiResult<EventContext> {
    let ctx = team_context(state, team_id, account).await?;
    if !can(ctx.actor, Action::ViewEvents) {
        return Err(ApiError::NotFound);
    }
    let row: Option<EventForTeam> = sqlx::query_as(concat!(
        "SELECT p.status AS participation, ",
        event_columns!(),
        " FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $2 WHERE e.id = $1"
    ))
    .bind(event_id)
    .bind(team_id)
    .fetch_optional(&state.db)
    .await?;
    let EventForTeam { participation, event } = row.ok_or(ApiError::NotFound)?;
    let relation = if event.host_team_id == Some(team_id) {
        Relation::Host
    } else {
        participation.as_deref().and_then(Relation::of_status).ok_or(ApiError::NotFound)?
    };
    if relation == Relation::Invited && !can(ctx.actor, Action::ViewEventInvitations) {
        return Err(ApiError::NotFound);
    }
    Ok(EventContext { ctx, relation })
}

/// Reads an event again inside a transaction, locked, so the checks that follow hold until
/// the commit.
async fn lock_event(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, event_id: Uuid) -> ApiResult<EventRow> {
    let event: Option<EventRow> =
        sqlx::query_as(concat!("SELECT ", event_columns!(), " FROM events e WHERE e.id = $1 FOR UPDATE"))
            .bind(event_id)
            .fetch_optional(&mut **tx)
            .await?;
    event.ok_or(ApiError::NotFound)
}

async fn participation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    team_id: Uuid,
) -> ApiResult<Option<Participation>> {
    let status: Option<(String,)> = sqlx::query_as("SELECT status FROM event_teams WHERE event_id = $1 AND team_id = $2")
        .bind(event_id)
        .bind(team_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(status.and_then(|(s,)| Participation::parse(&s)))
}

/// Applies one change to a team's participation, under the rule of
/// `events::next_participation`. Returns the event and the participation the team had.
async fn change_participation(
    state: &SharedState,
    event_id: Uuid,
    team_id: Uuid,
    change: ParticipationChange,
    by: &Account,
) -> ApiResult<(EventRow, Option<Participation>)> {
    let now = clock_now();
    let mut tx = state.db.begin().await?;
    let event = lock_event(&mut tx, event_id).await?;
    let current = participation(&mut tx, event_id, team_id).await?;
    let next = next_participation(event.phase(now), current, change)?;
    if change == ParticipationChange::Invite {
        sqlx::query(
            "INSERT INTO event_teams (event_id, team_id, status, invited_by, invited_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (event_id, team_id) DO UPDATE
                SET status = EXCLUDED.status, invited_by = EXCLUDED.invited_by, invited_at = EXCLUDED.invited_at,
                    answered_by = NULL, answered_at = NULL",
        )
    } else {
        sqlx::query("UPDATE event_teams SET status = $3, answered_by = $4, answered_at = $5 WHERE event_id = $1 AND team_id = $2")
    }
    .bind(event_id)
    .bind(team_id)
    .bind(next.as_str())
    .bind(by.id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((event, current))
}

#[derive(Deserialize)]
pub struct InviteTeamBody {
    team_id: Uuid,
}

/// `POST /api/teams/{id}/events/{event_id}/teams`: a leader of the host invites a team.
pub async fn invite_team(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<InviteTeamBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.require_host(Action::ManageEvent)?;
    ec.ctx.team.require_active()?;
    if body.team_id == team_id {
        return Err(ApiError::TeamAlreadyInEvent);
    }
    // Only an active team can be invited; any other does not exist for the caller.
    let invited: Option<(String,)> = sqlx::query_as("SELECT name FROM teams WHERE id = $1 AND status = 'active'")
        .bind(body.team_id)
        .fetch_optional(&state.db)
        .await?;
    let (invited_name,) = invited.ok_or(ApiError::NotFound)?;
    let (event, _) = change_participation(&state, event_id, body.team_id, ParticipationChange::Invite, &account).await?;
    tracing::info!(event = %event.group_name, team = %invited_name, by = %account.email, "team invited to an event");
    notify(&state, Notice { template: Template::EventInvitation, event, about_team: invited_name.clone(), to: vec![body.team_id], skip: None });
    Ok((StatusCode::CREATED, Json(json!({ "team_id": body.team_id, "name": invited_name }))))
}

/// `DELETE /api/teams/{id}/events/{event_id}/teams/{other_team_id}`: a leader of the host
/// withdraws an invitation or takes a participating team out. The team loses the event's
/// group at once.
pub async fn remove_team(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id, other_team_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.require_host(Action::ManageEvent)?;
    let other: Option<(String,)> =
        sqlx::query_as("SELECT name FROM teams WHERE id = $1").bind(other_team_id).fetch_optional(&state.db).await?;
    let (other_name,) = other.ok_or(ApiError::NotFound)?;
    let (event, previous) = change_participation(&state, event_id, other_team_id, ParticipationChange::Remove, &account).await?;
    tracing::info!(event = %event.group_name, team = %other_name, by = %account.email, "team removed from an event");
    let synced = if previous == Some(Participation::Accepted) { apply_change(&state).await } else { true };
    notify(&state, Notice { template: Template::EventTeamRemoved, event, about_team: other_name, to: vec![other_team_id], skip: None });
    Ok(Json(json!({ "synced": synced })))
}

#[derive(Deserialize)]
pub struct AnswerEventBody {
    accept: bool,
}

/// `POST /api/teams/{id}/events/{event_id}/answer`: a leader of an invited team accepts or
/// declines. On acceptance the team's accounts join the group at once if the event runs.
pub async fn answer_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<AnswerEventBody>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.ctx.require(Action::AnswerEventInvitation)?;
    if ec.relation == Relation::Host {
        return Err(ApiError::EventInvitationNotPending);
    }
    let change = if body.accept {
        // A team that is not active has no group to add the event's to.
        ec.ctx.team.require_active()?;
        ParticipationChange::Accept
    } else {
        ParticipationChange::Decline
    };
    let (event, _) = change_participation(&state, event_id, team_id, change, &account).await?;
    tracing::info!(event = %event.group_name, team = %ec.ctx.team.slug, accept = body.accept, by = %account.email, "event invitation answered");
    let synced = if body.accept { apply_change(&state).await } else { true };
    let template = if body.accept { Template::EventAccepted } else { Template::EventDeclined };
    let to = event.host_team_id.into_iter().collect();
    notify(&state, Notice { template, event, about_team: ec.ctx.team.name.clone(), to, skip: None });
    Ok(Json(json!({ "accepted": body.accept, "synced": synced })))
}

/// `POST /api/teams/{id}/events/{event_id}/leave`: a leader makes their team leave an event
/// it had accepted. The team stops seeing the others, and being seen, at once.
pub async fn leave_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.ctx.require(Action::AnswerEventInvitation)?;
    if ec.relation == Relation::Host {
        // The host cannot leave its own event: it ends or cancels it.
        return Err(ApiError::Forbidden);
    }
    let (event, _) = change_participation(&state, event_id, team_id, ParticipationChange::Leave, &account).await?;
    tracing::info!(event = %event.group_name, team = %ec.ctx.team.slug, by = %account.email, "team left an event");
    let synced = apply_change(&state).await;
    let to = event.host_team_id.into_iter().collect();
    notify(&state, Notice { template: Template::EventLeft, event, about_team: ec.ctx.team.name.clone(), to, skip: None });
    Ok(Json(json!({ "synced": synced })))
}

#[derive(Deserialize)]
pub struct UpdateEventBody {
    #[serde(default, with = "time::serde::rfc3339::option")]
    starts_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
}

/// `PATCH /api/teams/{id}/events/{event_id}`: a leader of the host changes the dates. Once
/// the event runs only its end can move.
pub async fn update_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEventBody>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.require_host(Action::ManageEvent)?;
    let now = clock_now();
    let mut tx = state.db.begin().await?;
    let mut event = lock_event(&mut tx, event_id).await?;
    let (starts_at, ends_at) =
        changed_event_dates((event.starts_at, event.ends_at), event.cancelled_at, body.starts_at, body.ends_at, now)?;
    let changed = (starts_at, ends_at) != (event.starts_at, event.ends_at);
    if changed {
        sqlx::query("UPDATE events SET starts_at = $2, ends_at = $3 WHERE id = $1")
            .bind(event_id)
            .bind(starts_at)
            .bind(ends_at)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let mut synced = true;
    if changed {
        event.starts_at = starts_at;
        event.ends_at = ends_at;
        tracing::info!(event = %event.group_name, %starts_at, %ends_at, by = %account.email, "event dates changed");
        // A start brought back to now takes effect in this pass; any other date wakes the scheduler.
        synced = apply_change(&state).await;
        let to = concerned_teams(&state, &event, &[Participation::Accepted, Participation::Invited]).await?;
        notify(&state, Notice { template: Template::EventRescheduled, event, about_team: String::new(), to, skip: Some(account.id) });
    }
    Ok(Json(json!({ "starts_at": rfc3339(starts_at), "ends_at": rfc3339(ends_at), "synced": synced })))
}

/// `POST /api/teams/{id}/events/{event_id}/end`: a leader of the host ends a running event
/// now. The group disappears at once.
pub async fn end_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.require_host(Action::ManageEvent)?;
    let now = clock_now();
    let mut tx = state.db.begin().await?;
    let mut event = lock_event(&mut tx, event_id).await?;
    let ends_at = end_now(event.starts_at, event.ends_at, event.cancelled_at, now)?;
    sqlx::query("UPDATE events SET ends_at = $2 WHERE id = $1").bind(event_id).bind(ends_at).execute(&mut *tx).await?;
    tx.commit().await?;
    event.ends_at = ends_at;
    tracing::info!(event = %event.group_name, by = %account.email, "event ended early");
    let synced = apply_change(&state).await;
    let to = concerned_teams(&state, &event, &[Participation::Accepted, Participation::Invited]).await?;
    notify(&state, Notice { template: Template::EventEndedEarly, event, about_team: String::new(), to, skip: Some(account.id) });
    Ok(Json(json!({ "ends_at": rfc3339(ends_at), "synced": synced })))
}

/// Who calls an event off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelReason {
    Host,
    Admin,
    HostDeleted,
}

impl CancelReason {
    fn as_str(self) -> &'static str {
        match self {
            CancelReason::Host => "host",
            CancelReason::Admin => "admin",
            CancelReason::HostDeleted => "host_deleted",
        }
    }
}

/// Cancels an event that is not over, tells the teams concerned, and removes its group.
async fn cancel(state: &SharedState, event_id: Uuid, reason: CancelReason, by: &Account) -> ApiResult<bool> {
    let now = clock_now();
    let mut tx = state.db.begin().await?;
    let mut event = lock_event(&mut tx, event_id).await?;
    if !event.phase(now).is_open() {
        return Err(ApiError::EventClosed);
    }
    sqlx::query("UPDATE events SET cancelled_at = $2, cancel_reason = $3 WHERE id = $1")
        .bind(event_id)
        .bind(now)
        .bind(reason.as_str())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    event.cancelled_at = Some(now);
    tracing::info!(event = %event.group_name, reason = reason.as_str(), by = %by.email, "event cancelled");
    let synced = apply_change(state).await;
    let to = concerned_teams(state, &event, &[Participation::Accepted, Participation::Invited]).await?;
    notify(state, Notice { template: Template::EventCancelled, event, about_team: String::new(), to, skip: Some(by.id) });
    Ok(synced)
}

/// `POST /api/teams/{id}/events/{event_id}/cancel`: a leader of the host calls the event off.
pub async fn cancel_event(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, event_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ec = event_context(&state, team_id, event_id, &account).await?;
    ec.require_host(Action::CancelEvent)?;
    // An administrator who is not a leader of the host acts as an administrator.
    let reason = if can(ec.ctx.actor, Action::ManageEvent) { CancelReason::Host } else { CancelReason::Admin };
    let synced = cancel(&state, event_id, reason, &account).await?;
    Ok(Json(json!({ "synced": synced })))
}

/// `GET /api/admin/events`: every event, with its whole invitation list.
pub async fn admin_list_events(State(state): State<SharedState>, AdminAccount(_): AdminAccount) -> ApiResult<Json<Value>> {
    let now = clock_now();
    let rows: Vec<EventRow> =
        sqlx::query_as(concat!("SELECT ", event_columns!(), " FROM events e ORDER BY e.created_at DESC LIMIT 500"))
            .fetch_all(&state.db)
            .await?;
    let mut events: Vec<(EventRow, Option<Relation>)> = rows.into_iter().map(|e| (e, None)).collect();
    sort_events(&mut events, now);
    let admin = Actor { is_admin: true, team_role: None };
    Ok(Json(json!({ "events": views(&state, &events, admin, now).await? })))
}

/// `POST /api/admin/events/{id}/cancel`: an administrator calls an event off. It is the only
/// thing an administrator does to an event; there is no approval of events.
pub async fn admin_cancel_event(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Path(event_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let actor = Actor { is_admin: true, team_role: None };
    if !can(actor, Action::CancelEvent) {
        return Err(ApiError::Forbidden);
    }
    let synced = cancel(&state, event_id, CancelReason::Admin, &admin).await?;
    Ok(Json(json!({ "synced": synced })))
}

// ------------------------------------------------------------------- deletion of a team

/// What the deletion of a team does to events, decided before the team's rows are gone.
pub struct TeamDeletion {
    /// Events the team hosted that were not over: cancelled.
    cancelled: Vec<EventRow>,
    /// Events of other teams it took part in: their hosts are told it is gone.
    left: Vec<EventRow>,
    team_name: String,
}

/// Cancels the events a team hosts and notes the ones it takes part in, inside the
/// transaction that deletes the team.
pub async fn before_team_deletion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: Uuid,
    team_name: &str,
) -> ApiResult<TeamDeletion> {
    let now = clock_now();
    let cancelled: Vec<EventRow> = sqlx::query_as(concat!(
        "UPDATE events e SET cancelled_at = $2, cancel_reason = $3
          WHERE e.host_team_id = $1 AND e.cancelled_at IS NULL AND e.ends_at > $2 RETURNING ",
        event_columns!()
    ))
    .bind(team_id)
    .bind(now)
    .bind(CancelReason::HostDeleted.as_str())
    .fetch_all(&mut **tx)
    .await?;
    let left: Vec<EventRow> = sqlx::query_as(concat!(
        "SELECT ",
        event_columns!(),
        " FROM events e JOIN event_teams p ON p.event_id = e.id
          WHERE p.team_id = $1 AND p.status = 'accepted' AND e.cancelled_at IS NULL AND e.ends_at > $2"
    ))
    .bind(team_id)
    .bind(now)
    .fetch_all(&mut **tx)
    .await?;
    Ok(TeamDeletion { cancelled, left, team_name: team_name.to_string() })
}

/// Sends the e-mails that follow the deletion of a team, once it is committed.
pub async fn after_team_deletion(state: &SharedState, deletion: TeamDeletion, by: &Account) -> ApiResult<()> {
    for event in deletion.cancelled {
        tracing::info!(event = %event.group_name, by = %by.email, "event cancelled: its host team was deleted");
        let to = concerned_teams(state, &event, &[Participation::Accepted, Participation::Invited]).await?;
        notify(state, Notice { template: Template::EventCancelled, event, about_team: String::new(), to, skip: Some(by.id) });
    }
    for event in deletion.left {
        let to = event.host_team_id.into_iter().collect();
        notify(state, Notice { template: Template::EventLeft, event, about_team: deletion.team_name.clone(), to, skip: Some(by.id) });
    }
    Ok(())
}

// ------------------------------------------------------------------------------ e-mails

/// The host team and the invited teams in one of `statuses`.
async fn concerned_teams(state: &SharedState, event: &EventRow, statuses: &[Participation]) -> ApiResult<Vec<Uuid>> {
    let statuses: Vec<String> = statuses.iter().map(|s| s.as_str().to_string()).collect();
    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT team_id FROM event_teams WHERE event_id = $1 AND status = ANY($2)")
        .bind(event.id)
        .bind(&statuses)
        .fetch_all(&state.db)
        .await?;
    Ok(event.host_team_id.into_iter().chain(rows.into_iter().map(|(id,)| id)).collect())
}

/// An e-mail about an event, for the leaders of some teams.
struct Notice {
    template: Template,
    event: EventRow,
    /// The team the message is about (invited, answering, removed), when there is one.
    about_team: String,
    /// Teams whose leaders receive it.
    to: Vec<Uuid>,
    /// The account that did it: it needs no e-mail about its own action.
    skip: Option<Uuid>,
}

/// Sends a notice to the leaders of the teams it names, each in their saved language, with
/// a link to their own team's page. It runs in the background: the request that caused it
/// does not wait for the mail server, and a failure is logged, never returned.
fn notify(state: &SharedState, notice: Notice) {
    let state = state.clone();
    tokio::spawn(async move {
        let leaders: Result<Vec<(Uuid, String, String)>, sqlx::Error> = sqlx::query_as(
            "SELECT m.team_id, a.email, a.language
               FROM team_members m JOIN accounts a ON a.id = m.account_id
              WHERE m.team_id = ANY($1) AND m.role = 'leader' AND ($2::uuid IS NULL OR a.id <> $2)
              ORDER BY a.email",
        )
        .bind(&notice.to)
        .bind(notice.skip)
        .fetch_all(&state.db)
        .await;
        let leaders = match leaders {
            Ok(leaders) => leaders,
            Err(e) => {
                tracing::error!(error = %e, template = notice.template.key(), "event e-mail: recipients not read");
                return;
            }
        };
        for (team_id, email, language) in leaders {
            let url = format!("{}/teams/{team_id}", state.config.public_url);
            let start = locale::format_datetime(&language, notice.event.starts_at);
            let end = locale::format_datetime(&language, notice.event.ends_at);
            let vars = [
                ("event", notice.event.name.as_str()),
                ("channel", notice.event.group_name.as_str()),
                ("host", notice.event.host_team_name.as_str()),
                ("team", notice.about_team.as_str()),
                ("start", start.as_str()),
                ("end", end.as_str()),
                ("url", url.as_str()),
            ];
            state.mailer.send_best_effort(&email, &language, notice.template, &vars).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tak_manager_core::policy::TeamRole;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-11-20 10:00 UTC);
    const LEADER: Actor = Actor { is_admin: false, team_role: Some(TeamRole::Leader) };
    const MEMBER: Actor = Actor { is_admin: false, team_role: Some(TeamRole::Member) };
    const ADMIN: Actor = Actor { is_admin: true, team_role: None };

    fn event(start_hours: i64, end_hours: i64) -> EventRow {
        EventRow {
            id: Uuid::from_u128(1),
            name: "Exercise".into(),
            group_name: "evt-exercise".into(),
            host_team_id: Some(Uuid::from_u128(10)),
            host_team_name: "Alpha".into(),
            starts_at: NOW + time::Duration::hours(start_hours),
            ends_at: NOW + time::Duration::hours(end_hours),
            cancelled_at: None,
            cancel_reason: None,
        }
    }

    fn links() -> Vec<TeamLink> {
        [(11, "Bravo", "accepted"), (12, "Charlie", "invited"), (13, "Delta", "declined")]
            .into_iter()
            .map(|(id, name, status)| TeamLink {
                event_id: Uuid::from_u128(1),
                team_id: Uuid::from_u128(id),
                name: name.into(),
                status: status.into(),
            })
            .collect()
    }

    fn view(event: &EventRow, relation: Option<Relation>, actor: Actor) -> Value {
        let links = links();
        event_view(event, &links.iter().collect::<Vec<_>>(), relation, actor, NOW)
    }

    fn names(view: &Value) -> Vec<String> {
        view["teams"].as_array().unwrap().iter().map(|t| format!("{}:{}", t["name"].as_str().unwrap(), t["status"].as_str().unwrap())).collect()
    }

    fn allowed(view: &Value) -> Vec<&str> {
        ["manage", "end", "cancel", "answer", "leave"].into_iter().filter(|p| view["permissions"][p] == true).collect()
    }

    #[test]
    fn a_leader_of_the_host_sees_the_whole_list_and_runs_the_event() {
        let v = view(&event(-1, 2), Some(Relation::Host), LEADER);
        assert_eq!(v["phase"], "running");
        assert_eq!(names(&v), ["Alpha:host", "Bravo:accepted", "Charlie:invited", "Delta:declined"]);
        assert!(v["teams"][1]["team_id"].is_string(), "needed to remove a team");
        assert_eq!(allowed(&v), ["manage", "end", "cancel"]);
        // Before the start there is nothing to end.
        assert_eq!(allowed(&view(&event(1, 2), Some(Relation::Host), LEADER)), ["manage", "cancel"]);
    }

    #[test]
    fn a_member_sees_who_takes_part_and_nothing_else() {
        for relation in [Relation::Host, Relation::Accepted] {
            let v = view(&event(-1, 2), Some(relation), MEMBER);
            assert_eq!(names(&v), ["Alpha:host", "Bravo:accepted"], "no pending or declined invitation");
            assert!(v["teams"].as_array().unwrap().iter().all(|t| t["team_id"].is_null()), "no team identifier");
            assert!(allowed(&v).is_empty());
            assert_eq!((v["name"].as_str(), v["group_name"].as_str()), (Some("Exercise"), Some("evt-exercise")));
        }
    }

    #[test]
    fn a_leader_of_an_invited_team_answers_and_one_of_a_participating_team_can_leave() {
        let invited = view(&event(1, 2), Some(Relation::Invited), LEADER);
        assert_eq!(allowed(&invited), ["answer"]);
        assert_eq!(names(&invited), ["Alpha:host", "Bravo:accepted"], "they see who they would join");
        assert_eq!(allowed(&view(&event(-1, 2), Some(Relation::Accepted), LEADER)), ["leave"]);
    }

    #[test]
    fn an_administrator_sees_everything_and_can_only_cancel() {
        let v = view(&event(-1, 2), None, ADMIN);
        assert_eq!(names(&v).len(), 4);
        assert_eq!(allowed(&v), ["cancel"]);
    }

    #[test]
    fn nothing_can_be_done_to_an_event_that_is_over() {
        let ended = event(-3, -1);
        let mut cancelled = event(1, 2);
        cancelled.cancelled_at = Some(NOW);
        for (e, phase) in [(&ended, "ended"), (&cancelled, "cancelled")] {
            for (relation, actor) in [(Some(Relation::Host), LEADER), (Some(Relation::Accepted), LEADER), (None, ADMIN)] {
                let v = view(e, relation, actor);
                assert_eq!(v["phase"], phase);
                assert!(allowed(&v).is_empty(), "{phase} {relation:?}");
            }
        }
    }

    #[test]
    fn a_team_that_declined_left_or_was_removed_is_no_longer_concerned() {
        assert_eq!(Relation::of_status("invited"), Some(Relation::Invited));
        assert_eq!(Relation::of_status("accepted"), Some(Relation::Accepted));
        for status in ["declined", "left", "removed", "host", ""] {
            assert_eq!(Relation::of_status(status), None, "{status}");
        }
    }

    #[test]
    fn events_are_listed_open_first_then_latest_finished() {
        let mut list = vec![(event(-9, -5), None), (event(5, 6), None), (event(-1, 2), None), (event(-4, -3), None)];
        sort_events(&mut list, NOW);
        let starts: Vec<i64> = list.iter().map(|(e, _)| (e.starts_at - NOW).whole_hours()).collect();
        assert_eq!(starts, [-1, 5, -4, -9], "running, upcoming, then the finished ones from the latest");
    }
}
