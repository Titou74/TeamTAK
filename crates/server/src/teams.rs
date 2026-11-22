//! Teams: requests, members, invitations, connection mode.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamtak_core::names::{is_valid_slug, normalize_email, slugify, AccessKind, MAX_SLUG_LEN};
use teamtak_core::policy::{can, is_team_mode, seats_left, AccessRef, Action, Actor, TeamRole, TeamStatus};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accesses::{self, retire_accesses, RetireScope};
use crate::audit::{self, AuditAction, Detail, Details, Target};
use crate::auth::{Account, CurrentAccount};
use crate::error::{is_unique_violation, ApiError, ApiResult};
use crate::locale::{self, Template};
use crate::reconciler::Trigger;
use crate::settings;
use crate::state::SharedState;

/// Requests one account may have waiting for approval at the same time.
const MAX_PENDING_REQUESTS: i64 = 3;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub mode: String,
    pub member_cap: i32,
    pub request_message: String,
    pub decision_note: String,
    /// `None` once the account that asked for the team is deleted.
    pub requested_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Columns of [`Team`]. A macro, because sqlx only accepts SQL known at compile time.
macro_rules! team_columns {
    () => {
        "id, name, slug, status, mode, member_cap, request_message, decision_note, requested_by, created_at"
    };
}
pub(crate) use team_columns;

impl Team {
    pub fn status(&self) -> TeamStatus {
        TeamStatus::parse(&self.status).unwrap_or(TeamStatus::Pending)
    }

    pub fn mode(&self) -> AccessKind {
        AccessKind::parse(&self.mode).unwrap_or(AccessKind::Individual)
    }

    pub fn require_active(&self) -> ApiResult<()> {
        if self.status() == TeamStatus::Active {
            Ok(())
        } else {
            Err(ApiError::TeamNotActive)
        }
    }
}

/// A team as seen by the caller.
pub struct TeamContext {
    pub team: Team,
    pub actor: Actor,
}

impl TeamContext {
    pub fn require(&self, action: Action) -> ApiResult<()> {
        if can(self.actor, action) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

/// Loads a team and the caller's role in it. A team the caller may not see is reported as
/// not found, so its existence does not leak.
pub async fn team_context(state: &SharedState, team_id: Uuid, account: &Account) -> ApiResult<TeamContext> {
    let team: Option<Team> = sqlx::query_as(concat!("SELECT ", team_columns!(), " FROM teams WHERE id = $1"))
        .bind(team_id)
        .fetch_optional(&state.db)
        .await?;
    let team = team.ok_or(ApiError::NotFound)?;
    let role: Option<(String,)> = sqlx::query_as("SELECT role FROM team_members WHERE team_id = $1 AND account_id = $2")
        .bind(team_id)
        .bind(account.id)
        .fetch_optional(&state.db)
        .await?;
    let actor = Actor { is_admin: account.is_admin, team_role: role.and_then(|(r,)| TeamRole::parse(&r)) };
    if !can(actor, Action::ViewTeam) {
        return Err(ApiError::NotFound);
    }
    Ok(TeamContext { team, actor })
}

/// Runs a reconciliation pass after a change and tells whether it went through. When it did
/// not (directory or TAK Server unreachable), the change is in the database and the periodic
/// pass will apply it.
pub async fn apply_change(state: &SharedState) -> bool {
    match state.reconciler.run(Trigger::Change).await {
        Ok(report) => report.errors.is_empty(),
        Err(_) => false,
    }
}

#[derive(Deserialize)]
pub struct TeamRequestBody {
    name: String,
    mode: AccessKind,
    #[serde(default)]
    message: String,
}

/// `POST /api/teams`: asks for a team. It does nothing until an administrator approves it,
/// unless team approval is off in the settings: then it is active at once.
pub async fn request_team(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Json(body): Json<TeamRequestBody>,
) -> ApiResult<(StatusCode, Json<Team>)> {
    let name = body.name.split_whitespace().collect::<Vec<_>>().join(" ");
    let message = body.message.trim().to_string();
    if !(2..=60).contains(&name.chars().count()) || message.chars().count() > 500 || !is_team_mode(body.mode) {
        return Err(ApiError::InvalidInput);
    }
    let slug = slugify(&name, MAX_SLUG_LEN);
    if !is_valid_slug(&slug) {
        return Err(ApiError::InvalidTeamName);
    }
    let approval = settings::load(&state.db).await?.values.team_approval;
    if approval {
        let (pending,): (i64,) = sqlx::query_as("SELECT count(*) FROM teams WHERE requested_by = $1 AND status = 'pending'")
            .bind(account.id)
            .fetch_one(&state.db)
            .await?;
        if pending >= MAX_PENDING_REQUESTS {
            return Err(ApiError::TooManyPendingRequests);
        }
    }

    let mut tx = state.db.begin().await?;
    // Without approval, the team is decided when it is created, by nobody.
    let team: Team = sqlx::query_as(concat!(
        "INSERT INTO teams (id, name, slug, status, mode, member_cap, request_message, requested_by, decided_at)
         VALUES ($1, $2, $3, CASE WHEN $8 THEN 'pending' ELSE 'active' END, $4, $5, $6, $7,
                 CASE WHEN $8 THEN NULL ELSE now() END) RETURNING ",
        team_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(&name)
    .bind(&slug)
    .bind(body.mode.as_str())
    .bind(state.config.default_member_cap)
    .bind(&message)
    .bind(account.id)
    .bind(approval)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| if is_unique_violation(&e, "teams_slug_idx") { ApiError::TeamNameTaken } else { e.into() })?;
    sqlx::query("INSERT INTO team_members (team_id, account_id, role) VALUES ($1, $2, 'leader')")
        .bind(team.id)
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let details = Details::new().text(Detail::Mode, body.mode.as_str());
    if !approval {
        tracing::info!(team = %team.slug, by = %account.email, "team created without approval");
        audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamCreated, Target::team(&team), details).await;
        // Nothing waits for the administrators: they are not e-mailed.
        return Ok((StatusCode::CREATED, Json(team)));
    }
    tracing::info!(team = %team.slug, by = %account.email, "team requested");
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamRequested, Target::team(&team), details).await;

    let admins: Vec<(String, String)> =
        sqlx::query_as("SELECT email, language FROM accounts WHERE is_admin").fetch_all(&state.db).await?;
    let url = format!("{}/admin", state.config.public_url);
    let shown_message = if message.is_empty() { "—" } else { &message };
    for (email, language) in admins {
        let vars = [("team", name.as_str()), ("requester", account.label()), ("message", shown_message), ("url", url.as_str())];
        state.mailer.send_best_effort(&email, &language, Template::TeamRequest, &vars).await;
    }
    Ok((StatusCode::CREATED, Json(team)))
}

#[derive(Serialize, sqlx::FromRow)]
struct Member {
    account_id: Uuid,
    email: String,
    display_name: String,
    role: String,
    #[serde(with = "time::serde::rfc3339")]
    joined_at: OffsetDateTime,
}

#[derive(Serialize, sqlx::FromRow)]
struct Invitation {
    id: Uuid,
    email: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

/// `GET /api/teams/{id}`: the team, its members, and what the caller may see and do in it.
pub async fn team_detail(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    let members: Vec<Member> = sqlx::query_as(
        "SELECT m.account_id, a.email, a.display_name, m.role, m.joined_at
           FROM team_members m JOIN accounts a ON a.id = m.account_id
          WHERE m.team_id = $1 ORDER BY m.role, m.joined_at",
    )
    .bind(team_id)
    .fetch_all(&state.db)
    .await?;
    let manage_members = can(ctx.actor, Action::ManageMembers);
    // Pending invitations show e-mail addresses: for those who manage members only.
    let invitations: Vec<Invitation> = if manage_members || ctx.actor.is_admin {
        sqlx::query_as(
            "SELECT id, email, created_at FROM invitations WHERE team_id = $1 AND status = 'pending' ORDER BY created_at",
        )
        .bind(team_id)
        .fetch_all(&state.db)
        .await?
    } else {
        Vec::new()
    };
    let accesses = accesses::list_for_team(&state, &ctx, &account).await?;
    let own_individual = AccessRef { kind: AccessKind::Individual, owned_by_actor: true };
    Ok(Json(json!({
        "team": ctx.team,
        "my_role": ctx.actor.team_role.map(TeamRole::as_str),
        "members": members,
        "invitations": invitations,
        "accesses": accesses,
        "seats_left": seats_left(ctx.team.member_cap as i64, members.len() as i64, invitations.len() as i64),
        "permissions": {
            "manage_members": manage_members,
            "manage_team": can(ctx.actor, Action::ManageTeam),
            "create_shared_access": can(ctx.actor, Action::CreateAccess(AccessRef { kind: AccessKind::Shared, owned_by_actor: false })),
            "create_external_access": can(ctx.actor, Action::CreateAccess(AccessRef { kind: AccessKind::External, owned_by_actor: false })),
            "create_own_access": can(ctx.actor, Action::CreateAccess(own_individual)),
            "create_member_access": can(ctx.actor, Action::CreateAccess(AccessRef { kind: AccessKind::Individual, owned_by_actor: false })),
            "view_devices": can(ctx.actor, Action::ViewTeamDevices),
        },
    })))
}

#[derive(Deserialize)]
pub struct UpdateTeamBody {
    mode: AccessKind,
}

/// `PATCH /api/teams/{id}`: changes the connection mode. Accesses that do not fit the new
/// mode are removed (revoked, then deleted).
pub async fn update_team(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<UpdateTeamBody>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    if ctx.actor.team_role != Some(TeamRole::Leader) {
        return Err(ApiError::Forbidden);
    }
    if !is_team_mode(body.mode) {
        return Err(ApiError::InvalidInput);
    }
    if ctx.team.status() == TeamStatus::Rejected {
        return Err(ApiError::TeamNotActive);
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE teams SET mode = $2 WHERE id = $1").bind(team_id).bind(body.mode.as_str()).execute(&mut *tx).await?;
    // External accesses fit every mode; the two others only their own.
    let removed = retire_accesses(&mut tx, team_id, RetireScope::NotFittingMode(body.mode)).await?;
    tx.commit().await?;
    tracing::info!(team = %ctx.team.slug, mode = body.mode.as_str(), removed, by = %account.email, "connection mode changed");
    let details = Details::new().text(Detail::Mode, body.mode.as_str()).number(Detail::RemovedAccesses, removed as i64);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamModeChanged, Target::team(&ctx.team), details).await;
    let synced = if removed > 0 { apply_change(&state).await } else { true };
    Ok(Json(json!({ "removed_accesses": removed, "synced": synced })))
}

/// `DELETE /api/teams/{id}`: removes the team, its members and every TAK access of it. The
/// events it hosts are cancelled, and it disappears from the events it took part in.
pub async fn delete_team(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ManageTeam)?;
    let mut tx = state.db.begin().await?;
    let events = crate::events::before_team_deletion(&mut tx, team_id, &ctx.team.name).await?;
    let removed = retire_accesses(&mut tx, team_id, RetireScope::WholeTeam).await?;
    sqlx::query("DELETE FROM teams WHERE id = $1").bind(team_id).execute(&mut *tx).await?;
    tx.commit().await?;
    tracing::info!(team = %ctx.team.slug, removed, by = %account.email, "team deleted");
    let details = Details::new().number(Detail::RemovedAccesses, removed as i64);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamDeleted, Target::team(&ctx.team), details).await;
    let synced = if removed > 0 {
        apply_change(&state).await
    } else {
        // Nothing to change in the directory, but the dates the scheduler waits for may be gone.
        state.reconciler.schedule_changed();
        true
    };
    crate::events::after_team_deletion(&state, events, &account).await?;
    Ok(Json(json!({ "removed_accesses": removed, "synced": synced })))
}

#[derive(Deserialize)]
pub struct InviteBody {
    email: String,
    /// Language of the e-mail when the invitee has no account yet (the inviter's choice).
    #[serde(default)]
    language: Option<String>,
}

/// `POST /api/teams/{id}/invitations`: a leader invites someone by e-mail, without any
/// administrator approval, within the team's member cap.
pub async fn invite(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<InviteBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ManageMembers)?;
    ctx.team.require_active()?;
    let email = normalize_email(&body.email).ok_or(ApiError::InvalidEmail)?;

    let mut tx = state.db.begin().await?;
    // Serialises invitations and acceptances of one team, so the cap holds under concurrency.
    sqlx::query("SELECT id FROM teams WHERE id = $1 FOR UPDATE").bind(team_id).execute(&mut *tx).await?;
    let (already_member,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM team_members m JOIN accounts a ON a.id = m.account_id WHERE m.team_id = $1 AND a.email = $2)",
    )
    .bind(team_id)
    .bind(&email)
    .fetch_one(&mut *tx)
    .await?;
    if already_member {
        return Err(ApiError::AlreadyMember);
    }
    let (members, pending): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM team_members WHERE team_id = $1),
                (SELECT count(*) FROM invitations WHERE team_id = $1 AND status = 'pending')",
    )
    .bind(team_id)
    .fetch_one(&mut *tx)
    .await?;
    let (already_invited,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM invitations WHERE team_id = $1 AND email = $2 AND status = 'pending')")
            .bind(team_id)
            .bind(&email)
            .fetch_one(&mut *tx)
            .await?;
    if already_invited {
        return Err(ApiError::AlreadyInvited);
    }
    if seats_left(ctx.team.member_cap as i64, members, pending) == 0 {
        return Err(ApiError::TeamFull);
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO invitations (id, team_id, email, invited_by, status) VALUES ($1, $2, $3, $4, 'pending')")
        .bind(id)
        .bind(team_id)
        .bind(&email)
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(team = %ctx.team.slug, invitee = %email, by = %account.email, "member invited");
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::MemberInvited, Target::in_team(&ctx.team, &email), Details::new()).await;

    // The invitee's saved language; without an account yet, the one the inviter picked.
    let saved: Option<(String,)> =
        sqlx::query_as("SELECT language FROM accounts WHERE email = $1").bind(&email).fetch_optional(&state.db).await?;
    let language = saved.map(|(l,)| l).or(body.language).unwrap_or_else(|| locale::DEFAULT_LANGUAGE.to_string());
    let vars = [
        ("inviter", account.label()),
        ("team", ctx.team.name.as_str()),
        ("email", email.as_str()),
        ("url", state.config.public_url.as_str()),
    ];
    let email_sent = state.mailer.send(&email, &language, Template::Invitation, &vars).await.is_ok();
    if !email_sent {
        tracing::warn!(invitee = %email, "invitation e-mail not sent; the invitation still waits for the invitee");
    }
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "email": email, "email_sent": email_sent }))))
}

/// `DELETE /api/teams/{id}/invitations/{invitation_id}`
pub async fn cancel_invitation(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ManageMembers)?;
    let cancelled: Option<(String,)> = sqlx::query_as(
        "UPDATE invitations SET status = 'cancelled', decided_at = now() WHERE id = $1 AND team_id = $2 AND status = 'pending' RETURNING email",
    )
    .bind(invitation_id)
    .bind(team_id)
    .fetch_optional(&state.db)
    .await?;
    let (invitee,) = cancelled.ok_or(ApiError::InvitationNotPending)?;
    tracing::info!(team = %ctx.team.slug, invitee = %invitee, by = %account.email, "invitation cancelled");
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::InvitationCancelled, Target::in_team(&ctx.team, &invitee), Details::new()).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AnswerBody {
    accept: bool,
}

/// `POST /api/invitations/{id}/answer`: the invitee accepts or declines.
pub async fn answer_invitation(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(invitation_id): Path<Uuid>,
    Json(body): Json<AnswerBody>,
) -> ApiResult<Json<Value>> {
    let mut tx = state.db.begin().await?;
    // The invitation must be addressed to the caller's own e-mail.
    let invitation: Option<(Uuid, String)> =
        sqlx::query_as("SELECT team_id, status FROM invitations WHERE id = $1 AND email = $2")
            .bind(invitation_id)
            .bind(&account.email)
            .fetch_optional(&mut *tx)
            .await?;
    let (team_id, status) = invitation.ok_or(ApiError::NotFound)?;
    if status != "pending" {
        return Err(ApiError::InvitationNotPending);
    }
    if !body.accept {
        sqlx::query("UPDATE invitations SET status = 'declined', decided_at = now() WHERE id = $1")
            .bind(invitation_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let team_name: Option<(String,)> = sqlx::query_as("SELECT name FROM teams WHERE id = $1").bind(team_id).fetch_optional(&state.db).await?;
        let target = Target::in_team_named(team_id, &team_name.map(|(n,)| n).unwrap_or_default(), &account.email);
        audit::record(&state.db, audit::Actor::Account(&account), AuditAction::InvitationDeclined, target, Details::new()).await;
        return Ok(Json(json!({ "team_id": team_id, "accepted": false })));
    }
    let team: Team = sqlx::query_as(concat!("SELECT ", team_columns!(), " FROM teams WHERE id = $1 FOR UPDATE"))
        .bind(team_id)
        .fetch_one(&mut *tx)
        .await?;
    team.require_active()?;
    let (members,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM team_members WHERE team_id = $1").bind(team_id).fetch_one(&mut *tx).await?;
    // The cap may have been lowered since the invitation was sent.
    if members >= team.member_cap as i64 {
        return Err(ApiError::TeamFull);
    }
    sqlx::query("INSERT INTO team_members (team_id, account_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
        .bind(team_id)
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE invitations SET status = 'accepted', decided_at = now() WHERE id = $1")
        .bind(invitation_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(team = %team.slug, member = %account.email, "invitation accepted");
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::InvitationAccepted, Target::in_team(&team, &account.email), Details::new()).await;
    Ok(Json(json!({ "team_id": team_id, "accepted": true })))
}

pub(crate) async fn leader_count(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, team_id: Uuid) -> ApiResult<i64> {
    // Locks the team's members so two leaders cannot both step down at once.
    sqlx::query("SELECT 1 FROM team_members WHERE team_id = $1 FOR UPDATE").bind(team_id).execute(&mut **tx).await?;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM team_members WHERE team_id = $1 AND role = 'leader'")
        .bind(team_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(n)
}

/// `DELETE /api/teams/{id}/members/{account_id}`: a leader removes a member, or a member
/// leaves. The member's individual TAK access is removed with them.
pub async fn remove_member(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, member_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    if member_id == account.id {
        if ctx.actor.team_role.is_none() {
            return Err(ApiError::NotFound);
        }
    } else {
        ctx.require(Action::ManageMembers)?;
    }
    let mut tx = state.db.begin().await?;
    let role: Option<(String,)> = sqlx::query_as("SELECT role FROM team_members WHERE team_id = $1 AND account_id = $2")
        .bind(team_id)
        .bind(member_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (role,) = role.ok_or(ApiError::NotFound)?;
    if role == "leader" && leader_count(&mut tx, team_id).await? <= 1 {
        return Err(ApiError::LastLeader);
    }
    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND account_id = $2")
        .bind(team_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    let removed = retire_accesses(&mut tx, team_id, RetireScope::IndividualOf(member_id)).await?;
    tx.commit().await?;
    tracing::info!(team = %ctx.team.slug, member = %member_id, by = %account.email, removed, "member removed");
    let member_email = member_email(&state, member_id).await?;
    let action = if member_id == account.id { AuditAction::MemberLeft } else { AuditAction::MemberRemoved };
    let details = Details::new().number(Detail::RemovedAccesses, removed as i64);
    audit::record(&state.db, audit::Actor::Account(&account), action, Target::in_team(&ctx.team, &member_email), details).await;
    let synced = if removed > 0 { apply_change(&state).await } else { true };
    Ok(Json(json!({ "removed_accesses": removed, "synced": synced })))
}

#[derive(Deserialize)]
pub struct RoleBody {
    role: TeamRole,
}

/// `PATCH /api/teams/{id}/members/{account_id}`: makes a member a leader, or the reverse.
pub async fn set_member_role(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, member_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RoleBody>,
) -> ApiResult<StatusCode> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ManageMembers)?;
    let mut tx = state.db.begin().await?;
    let role: Option<(String,)> = sqlx::query_as("SELECT role FROM team_members WHERE team_id = $1 AND account_id = $2")
        .bind(team_id)
        .bind(member_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (role,) = role.ok_or(ApiError::NotFound)?;
    if role == "leader" && body.role == TeamRole::Member && leader_count(&mut tx, team_id).await? <= 1 {
        return Err(ApiError::LastLeader);
    }
    sqlx::query("UPDATE team_members SET role = $3 WHERE team_id = $1 AND account_id = $2")
        .bind(team_id)
        .bind(member_id)
        .bind(body.role.as_str())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if role != body.role.as_str() {
        tracing::info!(team = %ctx.team.slug, member = %member_id, role = body.role.as_str(), by = %account.email, "member role changed");
        let member_email = member_email(&state, member_id).await?;
        let details = Details::new().text(Detail::Role, body.role.as_str());
        audit::record(&state.db, audit::Actor::Account(&account), AuditAction::MemberRoleChanged, Target::in_team(&ctx.team, &member_email), details).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Address of an account, for the audit entries about a member.
async fn member_email(state: &SharedState, account_id: Uuid) -> ApiResult<String> {
    let email: Option<(String,)> = sqlx::query_as("SELECT email FROM accounts WHERE id = $1").bind(account_id).fetch_optional(&state.db).await?;
    Ok(email.map(|(e,)| e).unwrap_or_default())
}
