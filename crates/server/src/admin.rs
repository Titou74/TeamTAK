//! Administration: team requests, teams, accounts and invitations to the platform, and the
//! state of the reconciliation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamtak_core::names::normalize_email;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, Actor, AuditAction, Detail, Details, Target};
use crate::auth::AdminAccount;
use crate::error::{ApiError, ApiResult};
use crate::locale::{self, Template};
use crate::reconciler::Trigger;
use crate::state::SharedState;
use crate::teams::{apply_change, team_columns, Team};

/// Highest member cap an administrator can give a team. The server is sized for a dozen
/// teams of about ten people.
const MAX_MEMBER_CAP: i32 = 200;

#[derive(Serialize, sqlx::FromRow)]
struct AdminTeam {
    id: Uuid,
    name: String,
    slug: String,
    status: String,
    mode: String,
    member_cap: i32,
    request_message: String,
    decision_note: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    requester_email: String,
    requester_name: String,
    members: i64,
    pending_invitations: i64,
    accesses: i64,
}

/// `GET /api/admin/teams`: every team and request, newest first.
pub async fn list_teams(State(state): State<SharedState>, AdminAccount(_): AdminAccount) -> ApiResult<Json<Value>> {
    let teams: Vec<AdminTeam> = sqlx::query_as(
        "SELECT t.id, t.name, t.slug, t.status, t.mode, t.member_cap, t.request_message, t.decision_note, t.created_at,
                a.email AS requester_email, a.display_name AS requester_name,
                (SELECT count(*) FROM team_members m WHERE m.team_id = t.id) AS members,
                (SELECT count(*) FROM invitations i WHERE i.team_id = t.id AND i.status = 'pending') AS pending_invitations,
                (SELECT count(*) FROM tak_accesses x WHERE x.team_id = t.id) AS accesses
           FROM teams t JOIN accounts a ON a.id = t.requested_by
          ORDER BY t.created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "teams": teams, "default_member_cap": state.config.default_member_cap, "max_member_cap": MAX_MEMBER_CAP })))
}

fn check_cap(cap: i32) -> ApiResult<i32> {
    if (1..=MAX_MEMBER_CAP).contains(&cap) {
        Ok(cap)
    } else {
        Err(ApiError::InvalidInput)
    }
}

/// E-mail and language of the account that requested a team.
async fn requester(state: &SharedState, team: &Team) -> ApiResult<(String, String)> {
    Ok(sqlx::query_as("SELECT email, language FROM accounts WHERE id = $1").bind(team.requested_by).fetch_one(&state.db).await?)
}

#[derive(Deserialize)]
pub struct DecisionBody {
    approve: bool,
    #[serde(default)]
    member_cap: Option<i32>,
    #[serde(default)]
    note: String,
}

/// `POST /api/admin/teams/{id}/decision`: approves or rejects a team request. This is the
/// only approval step of the platform.
pub async fn decide_team(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<DecisionBody>,
) -> ApiResult<Json<Team>> {
    let note = body.note.trim().to_string();
    if note.chars().count() > 500 {
        return Err(ApiError::InvalidInput);
    }
    let cap = body.member_cap.map(check_cap).transpose()?;
    let status = if body.approve { "active" } else { "rejected" };
    let team: Option<Team> = sqlx::query_as(concat!("UPDATE teams SET status = $2, member_cap = COALESCE($3, member_cap), decided_by = $4, decided_at = now(), decision_note = $5
          WHERE id = $1 AND status = 'pending' RETURNING ", team_columns!()))
    .bind(team_id)
    .bind(status)
    .bind(cap)
    .bind(admin.id)
    .bind(&note)
    .fetch_optional(&state.db)
    .await?;
    let Some(team) = team else {
        let (exists,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1)").bind(team_id).fetch_one(&state.db).await?;
        return Err(if exists { ApiError::TeamAlreadyDecided } else { ApiError::NotFound });
    };
    tracing::info!(team = %team.slug, status, by = %admin.email, "team request decided");
    let (action, details) = if body.approve {
        (AuditAction::TeamApproved, Details::new().number(Detail::MemberCap, team.member_cap as i64))
    } else {
        (AuditAction::TeamRejected, Details::new())
    };
    audit::record(&state.db, Actor::Account(&admin), action, Target::team(&team), details).await;

    let (email, language) = requester(&state, &team).await?;
    let url = format!("{}/teams/{}", state.config.public_url, team.id);
    let shown_note = if note.is_empty() { "—" } else { &note };
    let template = if body.approve { Template::TeamApproved } else { Template::TeamRejected };
    let vars = [("team", team.name.as_str()), ("url", url.as_str()), ("note", shown_note)];
    state.mailer.send_best_effort(&email, &language, template, &vars).await;
    Ok(Json(team))
}

#[derive(Deserialize)]
pub struct UpdateTeamBody {
    #[serde(default)]
    member_cap: Option<i32>,
    /// `true` freezes an active team, `false` reactivates a suspended one.
    #[serde(default)]
    suspended: Option<bool>,
}

/// `PATCH /api/admin/teams/{id}`: member cap, suspension.
///
/// A suspended team keeps its members and accounts, but its TAK group disappears: its
/// accounts have no group left and their open sessions are cut.
pub async fn update_team(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<UpdateTeamBody>,
) -> ApiResult<Json<Value>> {
    let cap = body.member_cap.map(check_cap).transpose()?;
    let mut tx = state.db.begin().await?;
    let team: Option<Team> = sqlx::query_as(concat!("SELECT ", team_columns!(), " FROM teams WHERE id = $1 FOR UPDATE"))
        .bind(team_id)
        .fetch_optional(&mut *tx)
        .await?;
    let team = team.ok_or(ApiError::NotFound)?;
    let new_status = match (body.suspended, team.status.as_str()) {
        (Some(true), "active") => Some("suspended"),
        (Some(false), "suspended") => Some("active"),
        (Some(_), "pending" | "rejected") => return Err(ApiError::TeamNotActive),
        _ => None,
    };
    let updated: Team = sqlx::query_as(concat!(
        "UPDATE teams SET member_cap = COALESCE($2, member_cap), status = COALESCE($3, status) WHERE id = $1 RETURNING ",
        team_columns!()
    ))
    .bind(team_id)
    .bind(cap)
    .bind(new_status)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    if updated.member_cap != team.member_cap {
        tracing::info!(team = %team.slug, member_cap = updated.member_cap, by = %admin.email, "member cap changed");
        let details = Details::new().number(Detail::MemberCap, updated.member_cap as i64);
        audit::record(&state.db, Actor::Account(&admin), AuditAction::TeamCapChanged, Target::team(&team), details).await;
    }
    let synced = match new_status {
        Some(status) => {
            tracing::info!(team = %team.slug, status, by = %admin.email, "team status changed");
            let action = if status == "suspended" { AuditAction::TeamSuspended } else { AuditAction::TeamReactivated };
            audit::record(&state.db, Actor::Account(&admin), action, Target::team(&team), Details::new()).await;
            apply_change(&state).await
        }
        None => true,
    };
    Ok(Json(json!({ "team": updated, "synced": synced })))
}

#[derive(Serialize, sqlx::FromRow)]
struct AdminAccountRow {
    id: Uuid,
    email: String,
    display_name: String,
    language: String,
    is_admin: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    last_login_at: Option<OffsetDateTime>,
    teams: i64,
}

/// `GET /api/admin/accounts`
pub async fn list_accounts(State(state): State<SharedState>, AdminAccount(_): AdminAccount) -> ApiResult<Json<Value>> {
    let accounts: Vec<AdminAccountRow> = sqlx::query_as(
        "SELECT a.id, a.email, a.display_name, a.language, a.is_admin, a.created_at, a.last_login_at,
                (SELECT count(*) FROM team_members m JOIN teams t ON t.id = m.team_id
                  WHERE m.account_id = a.id AND t.status IN ('active', 'suspended')) AS teams
           FROM accounts a ORDER BY a.created_at",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "accounts": accounts })))
}

#[derive(Deserialize)]
pub struct InviteAccountBody {
    email: String,
    /// Language of the account and of the e-mail (the administrator's choice).
    #[serde(default)]
    language: Option<String>,
}

/// `POST /api/admin/accounts`: creates an account for an address and e-mails it an
/// invitation. With sign-ups by invitation only, it is how an administrator lets in someone
/// no team has invited yet, a future team leader for instance.
pub async fn invite_account(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Json(body): Json<InviteAccountBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let email = normalize_email(&body.email).ok_or(ApiError::InvalidEmail)?;
    let language = body.language.as_deref().map(locale::resolve).unwrap_or(locale::DEFAULT_LANGUAGE);
    let created: Option<(Uuid,)> =
        sqlx::query_as("INSERT INTO accounts (id, email, language) VALUES ($1, $2, $3) ON CONFLICT (email) DO NOTHING RETURNING id")
            .bind(Uuid::new_v4())
            .bind(&email)
            .bind(language)
            .fetch_optional(&state.db)
            .await?;
    let (id,) = created.ok_or(ApiError::AccountExists)?;
    tracing::info!(account = %email, by = %admin.email, "account created by an administrator");
    audit::record(&state.db, Actor::Account(&admin), AuditAction::AccountInvited, Target::object(&email), Details::new()).await;

    let vars = [("inviter", admin.label()), ("email", email.as_str()), ("url", state.config.public_url.as_str())];
    let email_sent = state.mailer.send(&email, language, Template::PlatformInvitation, &vars).await.is_ok();
    if !email_sent {
        tracing::warn!(account = %email, "invitation e-mail not sent; the account exists and can sign in");
    }
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "email": email, "email_sent": email_sent }))))
}

#[derive(Deserialize)]
pub struct UpdateAccountBody {
    is_admin: bool,
}

/// `PATCH /api/admin/accounts/{id}`: grants or withdraws the administrator role. An
/// administrator cannot withdraw their own, so there is always one left.
pub async fn update_account(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Path(account_id): Path<Uuid>,
    Json(body): Json<UpdateAccountBody>,
) -> ApiResult<StatusCode> {
    if account_id == admin.id && !body.is_admin {
        return Err(ApiError::Forbidden);
    }
    // The role as it was, so that setting it to what it already is leaves no entry.
    let changed: Option<(String, bool)> = sqlx::query_as(
        "UPDATE accounts a SET is_admin = $2 FROM (SELECT id, is_admin FROM accounts WHERE id = $1 FOR UPDATE) before
          WHERE a.id = before.id RETURNING a.email, before.is_admin",
    )
    .bind(account_id)
    .bind(body.is_admin)
    .fetch_optional(&state.db)
    .await?;
    let (email, was_admin) = changed.ok_or(ApiError::NotFound)?;
    tracing::info!(account = %account_id, is_admin = body.is_admin, by = %admin.email, "administrator role changed");
    if was_admin != body.is_admin {
        let action = if body.is_admin { AuditAction::AdminGranted } else { AuditAction::AdminRevoked };
        audit::record(&state.db, Actor::Account(&admin), action, Target::object(&email), Details::new()).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow)]
struct ReconcileEvent {
    id: i64,
    #[serde(with = "time::serde::rfc3339")]
    occurred_at: OffsetDateTime,
    trigger: String,
    kind: String,
    subject: String,
    group_name: Option<String>,
    is_drift: bool,
}

async fn reconciliation_payload(state: &SharedState) -> ApiResult<Value> {
    let (last_run_at, last_ok_at, directory_reachable, tak_reachable, last_errors): (
        Option<OffsetDateTime>,
        Option<OffsetDateTime>,
        bool,
        bool,
        Value,
    ) = sqlx::query_as(
        "SELECT last_run_at, last_ok_at, directory_reachable, tak_reachable, last_errors FROM reconcile_status WHERE id = 1",
    )
    .fetch_one(&state.db)
    .await?;
    let events: Vec<ReconcileEvent> = sqlx::query_as(
        "SELECT id, occurred_at, trigger, kind, subject, group_name, is_drift FROM reconcile_events ORDER BY id DESC LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    let (drift_7d,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM reconcile_events WHERE is_drift AND occurred_at > now() - interval '7 days'")
            .fetch_one(&state.db)
            .await?;
    let (accounts, groups): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM tak_accesses),
                (SELECT count(DISTINCT t.id) FROM teams t JOIN tak_accesses a ON a.team_id = t.id WHERE t.status = 'active')
                -- plus the group of each running event that has at least one account in it
                + (SELECT count(*) FROM events e
                    WHERE e.cancelled_at IS NULL AND e.starts_at <= now() AND e.ends_at > now()
                      AND EXISTS (SELECT 1 FROM teams t JOIN tak_accesses a ON a.team_id = t.id
                                   WHERE t.status = 'active'
                                     AND (t.id = e.host_team_id
                                          OR t.id IN (SELECT p.team_id FROM event_teams p
                                                       WHERE p.event_id = e.id AND p.status = 'accepted'))))",
    )
    .fetch_one(&state.db)
    .await?;
    let fmt = |t: Option<OffsetDateTime>| t.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok());
    Ok(json!({
        "last_run_at": fmt(last_run_at),
        "last_ok_at": fmt(last_ok_at),
        "directory_reachable": directory_reachable,
        "tak_reachable": tak_reachable,
        "errors": last_errors,
        "interval_seconds": state.config.reconcile_interval.as_secs(),
        "expected_accounts": accounts,
        "expected_groups": groups,
        "drift_last_7_days": drift_7d,
        "events": events,
    }))
}

/// `GET /api/admin/reconciliation`: is the directory in line with the database, and what
/// was corrected lately.
pub async fn reconciliation(State(state): State<SharedState>, AdminAccount(_): AdminAccount) -> ApiResult<Json<Value>> {
    Ok(Json(reconciliation_payload(&state).await?))
}

/// `POST /api/admin/reconciliation/run`: runs a pass now. Unlike the automatic ones, it has
/// no limit on what it deletes.
pub async fn run_reconciliation(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    tracing::info!(by = %admin.email, "reconciliation requested");
    audit::record(&state.db, Actor::Account(&admin), AuditAction::ReconciliationRequested, Target::none(), Details::new()).await;
    state.reconciler.run(Trigger::Manual).await.map_err(|_| ApiError::Internal)?;
    Ok(Json(reconciliation_payload(&state).await?))
}
