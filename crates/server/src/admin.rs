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

use crate::accesses::{retire_accesses, RetireScope};
use crate::audit::{self, Actor, AuditAction, Detail, Details, Target};
use crate::auth::AdminAccount;
use crate::error::{ApiError, ApiResult};
use crate::locale::{self, Template};
use crate::reconciler::Trigger;
use crate::state::SharedState;
use crate::teams::{apply_change, leader_count, team_columns, Team};

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
    /// Empty once the account that asked for the team is deleted.
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
                COALESCE(a.email, '') AS requester_email, COALESCE(a.display_name, '') AS requester_name,
                (SELECT count(*) FROM team_members m WHERE m.team_id = t.id) AS members,
                (SELECT count(*) FROM invitations i WHERE i.team_id = t.id AND i.status = 'pending') AS pending_invitations,
                (SELECT count(*) FROM tak_accesses x WHERE x.team_id = t.id) AS accesses
           FROM teams t LEFT JOIN accounts a ON a.id = t.requested_by
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

/// E-mail and language of the account that requested a team, unless it was deleted.
async fn requester(state: &SharedState, team: &Team) -> ApiResult<Option<(String, String)>> {
    Ok(sqlx::query_as("SELECT email, language FROM accounts WHERE id = $1").bind(team.requested_by).fetch_optional(&state.db).await?)
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

    if let Some((email, language)) = requester(&state, &team).await? {
        let url = format!("{}/teams/{}", state.config.public_url, team.id);
        let shown_note = if note.is_empty() { "—" } else { &note };
        let template = if body.approve { Template::TeamApproved } else { Template::TeamRejected };
        let vars = [("team", team.name.as_str()), ("url", url.as_str()), ("note", shown_note)];
        state.mailer.send_best_effort(&email, &language, template, &vars).await;
    }
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
    /// Active or suspended teams it is the only leader of: while there is one, the account
    /// cannot be deleted.
    sole_leader_of: Vec<String>,
}

/// `GET /api/admin/accounts`
pub async fn list_accounts(State(state): State<SharedState>, AdminAccount(_): AdminAccount) -> ApiResult<Json<Value>> {
    let accounts: Vec<AdminAccountRow> = sqlx::query_as(
        "SELECT a.id, a.email, a.display_name, a.language, a.is_admin, a.created_at, a.last_login_at,
                (SELECT count(*) FROM team_members m JOIN teams t ON t.id = m.team_id
                  WHERE m.account_id = a.id AND t.status IN ('active', 'suspended')) AS teams,
                ARRAY(SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id
                       WHERE m.account_id = a.id AND m.role = 'leader' AND t.status IN ('active', 'suspended')
                         AND NOT EXISTS (SELECT 1 FROM team_members o
                                          WHERE o.team_id = m.team_id AND o.role = 'leader' AND o.account_id <> a.id)
                       ORDER BY t.name) AS sole_leader_of
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

/// `DELETE /api/admin/accounts/{id}`: deletes an account and what is tied to it, so that the
/// right to erasure the terms of use promise can be honoured.
///
/// Go with it: its sessions, its place in its teams, the invitations it sent that wait for an
/// answer and every invitation addressed to it, its login codes, its individual TAK accesses
/// (the reconciliation revokes their certificates and deletes their directory accounts, as
/// when a member is removed), the video streams it declared, and the team requests it alone
/// leads that are pending or rejected. What else it added to its teams stays with them, tied
/// to nobody: map sources, files, events, accesses it created for others. The audit log keeps
/// the entries that name it until they are purged; the address is e-mailed what was deleted.
///
/// Refused for the caller's own account, for the last administrator, and while the account
/// is the only leader of an active or suspended team: name another leader, or delete the
/// team, first.
pub async fn delete_account(
    State(state): State<SharedState>,
    AdminAccount(admin): AdminAccount,
    Path(account_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Another administrator does it, as for the administrator role.
    if account_id == admin.id {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.db.begin().await?;
    // The account and the administrators, locked in one order: two administrators cannot
    // delete each other at the same time, nor the last one be deleted.
    let locked: Vec<(Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT id, email, language, is_admin FROM accounts WHERE id = $1 OR is_admin ORDER BY id FOR UPDATE",
    )
    .bind(account_id)
    .fetch_all(&mut *tx)
    .await?;
    let (_, email, language, is_admin) = locked.iter().find(|(id, ..)| *id == account_id).cloned().ok_or(ApiError::NotFound)?;
    if is_admin && locked.iter().filter(|(.., admin)| *admin).count() <= 1 {
        return Err(ApiError::LastAdmin);
    }

    // Its teams, locked as an invitation or an acceptance locks them. A team it is the only
    // leader of keeps it, unless the team never was active: its request is withdrawn.
    let memberships: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT t.id, t.name, t.status, m.role FROM team_members m JOIN teams t ON t.id = m.team_id
          WHERE m.account_id = $1 ORDER BY t.id FOR UPDATE OF t",
    )
    .bind(account_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut withdrawn = Vec::new();
    for (team_id, _, status, role) in &memberships {
        if role == "leader" && leader_count(&mut tx, *team_id).await? <= 1 {
            if matches!(status.as_str(), "active" | "suspended") {
                return Err(ApiError::LastLeader);
            }
            withdrawn.push(*team_id);
        }
    }
    let withdrawn: Vec<Team> = sqlx::query_as(concat!("SELECT ", team_columns!(), " FROM teams WHERE id = ANY($1) ORDER BY created_at"))
        .bind(&withdrawn)
        .fetch_all(&mut *tx)
        .await?;
    let mut deletions = Vec::new();
    let mut removed_accesses = 0;
    for team in &withdrawn {
        deletions.push(crate::events::before_team_deletion(&mut tx, team.id, &team.name).await?);
        removed_accesses += retire_accesses(&mut tx, team.id, RetireScope::WholeTeam).await?;
        sqlx::query("DELETE FROM teams WHERE id = $1").bind(team.id).execute(&mut *tx).await?;
    }

    // Its individual accesses, in every team that has one.
    let access_teams: Vec<(Uuid,)> = sqlx::query_as("SELECT DISTINCT team_id FROM tak_accesses WHERE account_id = $1")
        .bind(account_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut removed_in = std::collections::BTreeMap::new();
    for (team_id,) in access_teams {
        let removed = retire_accesses(&mut tx, team_id, RetireScope::IndividualOf(account_id)).await?;
        removed_in.insert(team_id, removed);
        removed_accesses += removed;
    }
    let streams: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "DELETE FROM video_streams s USING teams t WHERE s.created_by = $1 AND t.id = s.team_id
         RETURNING s.team_id, t.name, s.group_name, s.group_name || '/' || s.name",
    )
    .bind(account_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM invitations WHERE (invited_by = $1 AND status = 'pending') OR email = $2")
        .bind(account_id)
        .bind(&email)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM login_codes WHERE email = $1").bind(&email).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM login_failures WHERE email = $1").bind(&email).execute(&mut *tx).await?;
    // Its sessions and memberships go with the row; what else refers to it no longer does.
    sqlx::query("DELETE FROM accounts WHERE id = $1").bind(account_id).execute(&mut *tx).await?;
    tx.commit().await?;

    tracing::info!(account = %email, removed_accesses, streams = streams.len(), withdrawn = withdrawn.len(), by = %admin.email, "account deleted");
    // Each team's history says what it lost, as if each step had been taken on its own.
    let by = Actor::Account(&admin);
    for team in &withdrawn {
        audit::record(&state.db, by, AuditAction::TeamDeleted, Target::team(team), Details::new()).await;
    }
    for (team_id, team_name, _, _) in memberships.iter().filter(|(id, ..)| !withdrawn.iter().any(|t| t.id == *id)) {
        let removed = removed_in.get(team_id).copied().unwrap_or(0);
        let details = Details::new().number(Detail::RemovedAccesses, removed as i64);
        audit::record(&state.db, by, AuditAction::MemberRemoved, Target::in_team_named(*team_id, team_name, &email), details).await;
    }
    for (team_id, team_name, group, path) in &streams {
        let details = Details::new().text(Detail::Channel, group);
        audit::record(&state.db, by, AuditAction::VideoStreamRemoved, Target::in_team_named(*team_id, team_name, path), details).await;
    }
    let details = Details::new()
        .number(Detail::RemovedAccesses, removed_accesses as i64)
        .number(Detail::RemovedStreams, streams.len() as i64);
    audit::record(&state.db, by, AuditAction::AccountDeleted, Target::object(&email), details).await;
    // The reconciliation revokes the certificates, deletes the directory accounts, takes the
    // streams off TAK Server and cuts the devices still on them.
    let synced = if removed_accesses > 0 || !streams.is_empty() {
        apply_change(&state).await
    } else {
        if !withdrawn.is_empty() {
            state.reconciler.schedule_changed();
        }
        true
    };
    for deletion in deletions {
        crate::events::after_team_deletion(&state, deletion, &admin).await?;
    }

    let days = crate::settings::load(&state.db).await?.values.audit_retention_days.to_string();
    let vars = [("email", email.as_str()), ("days", days.as_str())];
    let email_sent = state.mailer.send(&email, &language, Template::AccountDeleted, &vars).await.is_ok();
    if !email_sent {
        tracing::warn!(account = %email, "account deleted, but the e-mail telling it was not sent");
    }
    Ok(Json(json!({
        "removed_accesses": removed_accesses,
        "removed_streams": streams.len(),
        "withdrawn_teams": withdrawn.len(),
        "synced": synced,
        "email_sent": email_sent,
    })))
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
