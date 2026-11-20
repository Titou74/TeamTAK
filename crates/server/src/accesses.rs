//! TAK accesses: creation, credentials, QR codes, connection zips, regeneration, removal.
//!
//! An access is a row here; the account in the directory, its group and its certificates
//! follow through the reconciliation. Two operations talk to TAK Server directly because
//! their result is needed at once: enrolling the certificate shipped in a zip, and listing
//! the certificates to revoke when an access is regenerated.

use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamtak_core::names::{access_username, AccessKind};
use teamtak_core::package::{
    atak_enroll_uri, build_client_store, build_connection_zip, itak_qr, ClientStore, ConnectionPackage, PackageLayout,
};
use teamtak_core::policy::{can, kind_allowed_in_mode, AccessRef, Action};
use teamtak_core::{generate_tak_password, random_string, UNAMBIGUOUS};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditAction, Detail, Details, Target};
use crate::auth::{Account, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::reconciler::{sync_certificates, Trigger, PASSWORD_CONTEXT};
use crate::state::SharedState;
use crate::teams::{apply_change, team_context, TeamContext};

const P12_CONTEXT: &str = "tak_certificates.p12";
const P12_PASSWORD_CONTEXT: &str = "tak_certificates.p12_password";
/// Port of TAK Server's enrollment connector.
const ENROLL_PORT: u16 = 8446;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Access {
    pub id: Uuid,
    pub team_id: Uuid,
    pub kind: String,
    pub account_id: Option<Uuid>,
    pub label: String,
    pub username: String,
    pub password_enc: String,
    pub generation: i32,
    pub created_at: OffsetDateTime,
    pub regenerated_at: Option<OffsetDateTime>,
}

/// Columns of [`Access`]. A macro, because sqlx only accepts SQL known at compile time.
macro_rules! access_columns {
    () => {
        "id, team_id, kind, account_id, label, username, password_enc, generation, created_at, regenerated_at"
    };
}

impl Access {
    pub fn kind(&self) -> AccessKind {
        AccessKind::parse(&self.kind).unwrap_or(AccessKind::External)
    }

    fn as_ref_for(&self, account: &Account) -> AccessRef {
        AccessRef { kind: self.kind(), owned_by_actor: self.account_id == Some(account.id) }
    }
}

/// An access in a team's list: no secret in it.
#[derive(Serialize)]
pub struct AccessSummary {
    id: Uuid,
    kind: String,
    username: String,
    label: String,
    account_id: Option<Uuid>,
    generation: i32,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    regenerated_at: Option<OffsetDateTime>,
    /// Certificates of this access TAK Server currently accepts.
    active_certificates: i64,
    /// Earliest expiry among them.
    #[serde(with = "time::serde::rfc3339::option")]
    next_expiry: Option<OffsetDateTime>,
    can_use: bool,
    can_manage: bool,
}

/// Accesses of a team the caller may at least see listed: a leader sees all of them, a
/// member the shared one and their own.
pub async fn list_for_team(state: &SharedState, ctx: &TeamContext, account: &Account) -> ApiResult<Vec<AccessSummary>> {
    let rows: Vec<Access> = sqlx::query_as(concat!(
        "SELECT ",
        access_columns!(),
        " FROM tak_accesses WHERE team_id = $1 ORDER BY kind DESC, created_at"
    ))
    .bind(ctx.team.id)
    .fetch_all(&state.db)
    .await?;
    let certs: Vec<(Uuid, i64, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT access_id, count(*), min(expires_at) FROM tak_certificates
          WHERE access_id IS NOT NULL AND inactive_at IS NULL AND revoke_requested_at IS NULL
            AND access_id IN (SELECT id FROM tak_accesses WHERE team_id = $1)
          GROUP BY access_id",
    )
    .bind(ctx.team.id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|a| {
            let r = a.as_ref_for(account);
            let can_use = can(ctx.actor, Action::UseAccess(r));
            if !can_use {
                return None;
            }
            let (active, next_expiry) =
                certs.iter().find(|(id, _, _)| *id == a.id).map(|(_, n, e)| (*n, *e)).unwrap_or((0, None));
            Some(AccessSummary {
                can_manage: can(ctx.actor, Action::ManageAccess(r)),
                can_use,
                id: a.id,
                kind: a.kind,
                username: a.username,
                label: a.label,
                account_id: a.account_id,
                generation: a.generation,
                created_at: a.created_at,
                regenerated_at: a.regenerated_at,
                active_certificates: active,
                next_expiry,
            })
        })
        .collect())
}

/// Which accesses of a team to remove.
pub enum RetireScope {
    WholeTeam,
    /// Shared or individual accesses that do not fit the team's new connection mode.
    NotFittingMode(AccessKind),
    /// The individual access of one member.
    IndividualOf(Uuid),
    One(Uuid),
}

/// Removes accesses from the database and retires their usernames for good. The
/// reconciliation then revokes their certificates and deletes their directory accounts.
pub async fn retire_accesses(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: Uuid,
    scope: RetireScope,
) -> ApiResult<u64> {
    let usernames: Vec<(String,)> = match scope {
        RetireScope::WholeTeam => {
            sqlx::query_as("DELETE FROM tak_accesses WHERE team_id = $1 RETURNING username")
                .bind(team_id)
                .fetch_all(&mut **tx)
                .await?
        }
        RetireScope::NotFittingMode(mode) => {
            sqlx::query_as("DELETE FROM tak_accesses WHERE team_id = $1 AND kind <> 'external' AND kind <> $2 RETURNING username")
                .bind(team_id)
                .bind(mode.as_str())
                .fetch_all(&mut **tx)
                .await?
        }
        RetireScope::IndividualOf(account_id) => {
            sqlx::query_as(
                "DELETE FROM tak_accesses WHERE team_id = $1 AND kind = 'individual' AND account_id = $2 RETURNING username",
            )
            .bind(team_id)
            .bind(account_id)
            .fetch_all(&mut **tx)
            .await?
        }
        RetireScope::One(access_id) => {
            sqlx::query_as("DELETE FROM tak_accesses WHERE team_id = $1 AND id = $2 RETURNING username")
                .bind(team_id)
                .bind(access_id)
                .fetch_all(&mut **tx)
                .await?
        }
    };
    for (username,) in &usernames {
        sqlx::query("INSERT INTO retired_usernames (username) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(username)
            .execute(&mut **tx)
            .await?;
        // No key material is kept for an access that no longer exists.
        sqlx::query("UPDATE tak_certificates SET p12_enc = NULL, p12_password_enc = NULL, revoke_requested_at = COALESCE(revoke_requested_at, now()) WHERE username = $1")
            .bind(username)
            .execute(&mut **tx)
            .await?;
    }
    Ok(usernames.len() as u64)
}

#[derive(Deserialize)]
pub struct CreateAccessBody {
    kind: AccessKind,
    /// Individual access: the member it is for (default: the caller).
    #[serde(default)]
    account_id: Option<Uuid>,
    /// External access: who it is for.
    #[serde(default)]
    label: Option<String>,
}

/// `POST /api/teams/{id}/accesses`
pub async fn create_access(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<CreateAccessBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.team.require_active()?;
    if !kind_allowed_in_mode(ctx.team.mode(), body.kind) {
        return Err(ApiError::AccessKindNotAllowed);
    }
    let owner_id = match body.kind {
        AccessKind::Individual => Some(body.account_id.unwrap_or(account.id)),
        _ => None,
    };
    ctx.require(Action::CreateAccess(AccessRef { kind: body.kind, owned_by_actor: owner_id == Some(account.id) }))?;

    let (hint, label) = match body.kind {
        AccessKind::Shared => (String::new(), String::new()),
        AccessKind::Individual => {
            let owner = owner_id.expect("set above");
            let member: Option<(String,)> = sqlx::query_as(
                "SELECT a.email FROM team_members m JOIN accounts a ON a.id = m.account_id WHERE m.team_id = $1 AND m.account_id = $2",
            )
            .bind(team_id)
            .bind(owner)
            .fetch_optional(&state.db)
            .await?;
            (member.ok_or(ApiError::NotFound)?.0, String::new())
        }
        AccessKind::External => {
            let label = body.label.unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
            if !(1..=40).contains(&label.chars().count()) || label.chars().any(char::is_control) {
                return Err(ApiError::InvalidInput);
            }
            (label.clone(), label)
        }
    };

    let password = generate_tak_password();
    let password_enc = state.secrets.seal(PASSWORD_CONTEXT, password.as_bytes());
    let mut tx = state.db.begin().await?;
    // One at a time, so two requests cannot pick the same username.
    sqlx::query("SELECT pg_advisory_xact_lock(7205001)").execute(&mut *tx).await?;
    let mut username = None;
    for attempt in 0..100 {
        let candidate = access_username(&ctx.team.slug, body.kind, &hint, attempt);
        let (taken,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM tak_accesses WHERE username = $1) OR EXISTS (SELECT 1 FROM retired_usernames WHERE username = $1)",
        )
        .bind(&candidate)
        .fetch_one(&mut *tx)
        .await?;
        if !taken && candidate != state.config.tak.api_cert_name {
            username = Some(candidate);
            break;
        }
    }
    let username = username.ok_or(ApiError::Internal)?;
    let id = Uuid::new_v4();
    let inserted = sqlx::query(
        "INSERT INTO tak_accesses (id, team_id, kind, account_id, label, username, password_enc, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(team_id)
    .bind(body.kind.as_str())
    .bind(owner_id)
    .bind(&label)
    .bind(&username)
    .bind(&password_enc)
    .bind(account.id)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => return Err(ApiError::AccessExists),
        Err(e) => return Err(e.into()),
    }
    tx.commit().await?;
    tracing::info!(team = %ctx.team.slug, username, kind = body.kind.as_str(), by = %account.email, "access created");
    let details = Details::new().text(Detail::Kind, body.kind.as_str());
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::AccessCreated, Target::in_team(&ctx.team, &username), details).await;
    let synced = apply_change(&state).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "username": username, "synced": synced }))))
}

/// Loads an access and the caller's rights on it.
async fn access_context(state: &SharedState, access_id: Uuid, account: &Account) -> ApiResult<(Access, TeamContext, AccessRef)> {
    let access: Option<Access> = sqlx::query_as(concat!("SELECT ", access_columns!(), " FROM tak_accesses WHERE id = $1"))
        .bind(access_id)
        .fetch_optional(&state.db)
        .await?;
    let access = access.ok_or(ApiError::NotFound)?;
    let ctx = team_context(state, access.team_id, account).await?;
    let r = access.as_ref_for(account);
    // An access the caller may not use does not exist for them.
    if !can(ctx.actor, Action::UseAccess(r)) {
        return Err(ApiError::NotFound);
    }
    Ok((access, ctx, r))
}

#[derive(Serialize, sqlx::FromRow)]
struct CertificateView {
    fingerprint: String,
    source: String,
    #[serde(with = "time::serde::rfc3339::option")]
    issued_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// `GET /api/accesses/{id}`: everything needed to connect a TAK client with this access.
pub async fn access_detail(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(access_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (access, ctx, r) = access_context(&state, access_id, &account).await?;
    let password = state.secrets.open_string(PASSWORD_CONTEXT, &access.password_enc)?;
    // This answer carries the password and the QR codes: who was shown them is recorded,
    // the credentials themselves never are.
    let details = Details::new().text(Detail::Kind, &access.kind);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::AccessCredentialsViewed, Target::in_team(&ctx.team, &access.username), details).await;
    let certificates: Vec<CertificateView> = sqlx::query_as(
        "SELECT fingerprint, source, issued_at, expires_at FROM tak_certificates
          WHERE access_id = $1 AND inactive_at IS NULL AND revoke_requested_at IS NULL ORDER BY issued_at",
    )
    .bind(access.id)
    .fetch_all(&state.db)
    .await?;
    let owner: Option<(String, String)> = match access.account_id {
        Some(id) => {
            sqlx::query_as("SELECT email, display_name FROM accounts WHERE id = $1").bind(id).fetch_optional(&state.db).await?
        }
        None => None,
    };
    let tak = &state.config.tak;
    // One iTAK QR code per port: the device picks the one its network lets through.
    let itak: serde_json::Map<String, Value> = tak
        .client_ports
        .iter()
        .map(|port| (port.to_string(), json!(itak_qr(&tak.connection_name_on(*port), &tak.public_host, *port))))
        .collect();
    Ok(Json(json!({
        "id": access.id,
        "team": { "id": ctx.team.id, "name": ctx.team.name, "slug": ctx.team.slug, "status": ctx.team.status },
        "kind": access.kind,
        "label": access.label,
        "owner": owner.map(|(email, display_name)| json!({ "email": email, "display_name": display_name })),
        "username": access.username,
        "password": password,
        "generation": access.generation,
        "created_at": access.created_at.format(&time::format_description::well_known::Rfc3339).ok(),
        "regenerated_at": access.regenerated_at.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
        "server": {
            "host": tak.public_host,
            "port": tak.client_port(),
            "ports": tak.client_ports,
            "enroll_port": ENROLL_PORT,
            "name": tak.connection_name,
        },
        "qr": {
            "itak": itak,
            "atak": atak_enroll_uri(&tak.public_host, &access.username, &password),
        },
        "certificates": certificates,
        "can_manage": can(ctx.actor, Action::ManageAccess(r)),
    })))
}

/// The certificate the platform ships in the "zip with certificate" of an access: the stored
/// one while it is valid, a newly enrolled one otherwise. Enrollment goes through TAK Server
/// with the access's own account, exactly as a phone would do it.
async fn package_certificate(state: &SharedState, access: &Access) -> ApiResult<ClientStore> {
    // Held so that a reconciliation pass does not run between the enrollment and its record.
    let _guard = state.reconciler.exclusive().await;
    let file_name = format!("{}.p12", access.username);
    let stored: Option<(String, String)> = sqlx::query_as(
        "SELECT p12_enc, p12_password_enc FROM tak_certificates
          WHERE access_id = $1 AND source = 'package' AND inactive_at IS NULL AND revoke_requested_at IS NULL
            AND p12_enc IS NOT NULL AND p12_password_enc IS NOT NULL AND expires_at > now() + interval '1 day'
          ORDER BY expires_at DESC LIMIT 1",
    )
    .bind(access.id)
    .fetch_optional(&state.db)
    .await?;
    if let Some((p12_enc, password_enc)) = stored {
        let p12 = B64.decode(state.secrets.open(P12_CONTEXT, &p12_enc)?).map_err(|_| ApiError::Internal)?;
        return Ok(ClientStore { file_name, p12, password: state.secrets.open_string(P12_PASSWORD_CONTEXT, &password_enc)? });
    }

    // Make sure the directory holds the account with its current password before enrolling.
    let report = state.reconciler.run_locked(Trigger::Change).await.map_err(|_| ApiError::Internal)?;
    if !report.directory_reachable {
        return Err(ApiError::DirectoryUnavailable);
    }
    let password = state.secrets.open_string(PASSWORD_CONTEXT, &access.password_enc)?;
    let client_uid = format!("teamtak-{}", access.id);
    let enrolled = state.tak.enroll(&access.username, &password, &client_uid).await?;
    let p12_password = random_string(20, UNAMBIGUOUS);
    let p12 = build_client_store(
        &access.username,
        &enrolled.private_key_der,
        &enrolled.certificate_der,
        &enrolled.ca_ders,
        &p12_password,
        state.config.tak.p12_format,
    )?;
    sqlx::query(
        "INSERT INTO tak_certificates (id, access_id, username, fingerprint, source, issued_at, expires_at, p12_enc, p12_password_enc)
         VALUES ($1, $2, $3, $4, 'package', now(), $5, $6, $7)
         ON CONFLICT (fingerprint) DO UPDATE
            SET source = 'package', p12_enc = EXCLUDED.p12_enc, p12_password_enc = EXCLUDED.p12_password_enc,
                expires_at = EXCLUDED.expires_at, access_id = EXCLUDED.access_id",
    )
    .bind(Uuid::new_v4())
    .bind(access.id)
    .bind(&access.username)
    .bind(&enrolled.fingerprint)
    .bind(enrolled.not_after)
    .bind(state.secrets.seal(P12_CONTEXT, B64.encode(&p12).as_bytes()))
    .bind(state.secrets.seal(P12_PASSWORD_CONTEXT, p12_password.as_bytes()))
    .execute(&state.db)
    .await?;
    tracing::info!(username = %access.username, fingerprint = %enrolled.fingerprint, expires = %enrolled.not_after, "certificate enrolled for a zip");
    Ok(ClientStore { file_name, p12, password: p12_password })
}

#[derive(Deserialize, Default)]
pub struct PackageQuery {
    /// One of the client ports; the main one when absent.
    port: Option<String>,
}

/// `GET /api/accesses/{id}/package/{form}?port=`: a connection zip. `enrollment` holds the
/// server address and the truststore, and the client enrolls with the username and
/// password; `certificate` also holds a client certificate and connects as it is. The zip
/// connects on `port`, which must be one of the client ports; an alternative port is added
/// to the zip's name and to the name of the connection.
pub async fn access_package(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((access_id, form)): Path<(Uuid, String)>,
    Query(query): Query<PackageQuery>,
) -> ApiResult<Response> {
    let tak = &state.config.tak;
    let port = match query.port.as_deref() {
        None | Some("") => tak.client_port(),
        Some(raw) => raw.parse().ok().filter(|p| tak.client_ports.contains(p)).ok_or(ApiError::InvalidInput)?,
    };
    let (access, ctx, _) = access_context(&state, access_id, &account).await?;
    // The zip of an alternative port carries it in its name, beside the zips of the main one.
    let suffix = if port == tak.client_port() { String::new() } else { format!("-{port}") };
    let (client, file_name) = match form.as_str() {
        "enrollment" => (None, format!("{}-enrollment{suffix}.zip", access.username)),
        "certificate" => {
            ctx.team.require_active()?;
            (Some(package_certificate(&state, &access).await?), format!("{}{suffix}.zip", access.username))
        }
        _ => return Err(ApiError::NotFound),
    };
    let package = ConnectionPackage {
        description: tak.connection_name_on(port),
        host: tak.public_host.clone(),
        port,
        truststore_p12: state.truststore_p12.clone(),
        client,
        file_name: file_name.clone(),
    };
    let zip = build_connection_zip(&package, PackageLayout::Official)?;
    tracing::info!(username = %access.username, form, port, by = %account.email, "connection zip downloaded");
    let details = Details::new().text(Detail::Form, &form).number(Detail::Port, port.into()).text(Detail::Kind, &access.kind);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::AccessPackageDownloaded, Target::in_team(&ctx.team, &access.username), details).await;
    Ok((
        [
            (CONTENT_TYPE, "application/zip".to_string()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{file_name}\"")),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        zip,
    )
        .into_response())
}

/// `POST /api/accesses/{id}/regenerate`: revokes every certificate of the access, changes its
/// password, and enrolls a new certificate if a zip with certificate had been issued. Every
/// device using the access is disconnected and must be set up again.
pub async fn regenerate_access(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(access_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (access, ctx, r) = access_context(&state, access_id, &account).await?;
    ctx.require(Action::ManageAccess(r))?;
    ctx.team.require_active()?;

    let had_package;
    {
        let _guard = state.reconciler.exclusive().await;
        // The certificates to revoke are those TAK Server knows now, including the ones
        // devices enrolled by themselves. If it cannot be asked, nothing is changed.
        let active = state.tak.active_certificates().await?;
        let mut tx = state.db.begin().await?;
        sync_certificates(&mut tx, &active).await.map_err(|e| {
            tracing::error!(error = %format!("{e:#}"), "recording certificates");
            ApiError::Internal
        })?;
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM tak_certificates WHERE access_id = $1 AND source = 'package' AND inactive_at IS NULL AND revoke_requested_at IS NULL",
        )
        .bind(access.id)
        .fetch_one(&mut *tx)
        .await?;
        had_package = n > 0;
        sqlx::query(
            "UPDATE tak_certificates SET revoke_requested_at = now(), p12_enc = NULL, p12_password_enc = NULL
              WHERE username = $1 AND inactive_at IS NULL AND revoke_requested_at IS NULL",
        )
        .bind(&access.username)
        .execute(&mut *tx)
        .await?;
        let password = generate_tak_password();
        sqlx::query(
            "UPDATE tak_accesses SET password_enc = $2, generation = generation + 1, regenerated_at = now() WHERE id = $1",
        )
        .bind(access.id)
        .bind(state.secrets.seal(PASSWORD_CONTEXT, password.as_bytes()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        // Revocations and the new password are applied now.
        let report = state.reconciler.run_locked(Trigger::Change).await.map_err(|_| ApiError::Internal)?;
        let (left,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM tak_certificates WHERE username = $1 AND revoke_requested_at IS NOT NULL AND inactive_at IS NULL",
        )
        .bind(&access.username)
        .fetch_one(&state.db)
        .await?;
        if left > 0 || !report.tak_reachable {
            // Recorded and retried by the periodic pass; the caller must know it is not done.
            return Err(ApiError::TakUnavailable);
        }
        if !report.directory_reachable {
            return Err(ApiError::DirectoryUnavailable);
        }
    }
    tracing::info!(username = %access.username, by = %account.email, "access regenerated");
    let details = Details::new().text(Detail::Kind, &access.kind);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::AccessRegenerated, Target::in_team(&ctx.team, &access.username), details).await;

    if had_package {
        let fresh: Access = sqlx::query_as(concat!("SELECT ", access_columns!(), " FROM tak_accesses WHERE id = $1"))
            .bind(access.id)
            .fetch_one(&state.db)
            .await?;
        package_certificate(&state, &fresh).await?;
    }
    Ok(Json(json!({ "id": access.id, "reenrolled": had_package })))
}

/// `DELETE /api/accesses/{id}`: removes the access. Its certificates are revoked, then its
/// account is deleted from the directory.
pub async fn delete_access(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(access_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (access, ctx, r) = access_context(&state, access_id, &account).await?;
    ctx.require(Action::ManageAccess(r))?;
    let mut tx = state.db.begin().await?;
    retire_accesses(&mut tx, access.team_id, RetireScope::One(access.id)).await?;
    tx.commit().await?;
    tracing::info!(username = %access.username, by = %account.email, "access removed");
    let details = Details::new().text(Detail::Kind, &access.kind);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::AccessRemoved, Target::in_team(&ctx.team, &access.username), details).await;
    let synced = apply_change(&state).await;
    Ok(Json(json!({ "synced": synced })))
}
