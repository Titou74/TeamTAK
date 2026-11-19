//! Platform accounts: passwordless login by one-time e-mail code, sessions, profile.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tak_manager_core::login::{self, RequestDecision};
use tak_manager_core::names::normalize_email;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::locale::{self, Template};
use crate::state::SharedState;

pub const SESSION_COOKIE: &str = "tm_session";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub language: String,
    pub is_admin: bool,
}

impl Account {
    /// How the account is named to others: its display name, or its address.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.email
        } else {
            &self.display_name
        }
    }
}

/// Address of the client. Caddy is the only way in and sets `X-Forwarded-For` itself (it
/// drops what the client sent), so the last element is the address Caddy saw.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
        .unwrap_or_else(|| peer.ip().to_string())
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The signed-in account, from the session cookie.
pub struct CurrentAccount(pub Account);

impl FromRequestParts<SharedState> for CurrentAccount {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()).ok_or(ApiError::Unauthenticated)?;
        let account: Option<Account> = sqlx::query_as(
            "SELECT a.id, a.email, a.display_name, a.language, a.is_admin
               FROM sessions s JOIN accounts a ON a.id = s.account_id
              WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(hash_token(&token))
        .fetch_optional(&state.db)
        .await?;
        let account = account.ok_or(ApiError::Unauthenticated)?;
        // Sliding expiry, written at most once every few minutes.
        sqlx::query(
            "UPDATE sessions SET last_seen_at = now(), expires_at = now() + make_interval(days => $2)
              WHERE token_hash = $1 AND last_seen_at < now() - interval '5 minutes'",
        )
        .bind(hash_token(&token))
        .bind(state.config.session_ttl_days as i32)
        .execute(&state.db)
        .await?;
        Ok(CurrentAccount(account))
    }
}

/// A signed-in platform administrator.
pub struct AdminAccount(pub Account);

impl FromRequestParts<SharedState> for AdminAccount {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let CurrentAccount(account) = CurrentAccount::from_request_parts(parts, state).await?;
        if account.is_admin {
            Ok(AdminAccount(account))
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

#[derive(Deserialize)]
pub struct RequestCodeBody {
    email: String,
    /// Language of the UI when the code is requested: used for the e-mail when the address
    /// has no account yet.
    #[serde(default)]
    language: Option<String>,
}

/// `POST /api/auth/request-code`: e-mails a one-time code. The answer does not tell whether
/// the address has an account.
pub async fn request_code(
    State(state): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RequestCodeBody>,
) -> ApiResult<StatusCode> {
    let email = normalize_email(&body.email).ok_or(ApiError::InvalidEmail)?;
    let ip = client_ip(&headers, peer);
    let policy = state.config.login;

    let (for_email, from_ip): (i64, i64) = sqlx::query_as(
        // Codes made by the server-side rescue command are not requests from the network
        // and do not count: an administrator locked out by the limit could not be rescued.
        "SELECT count(*) FILTER (WHERE email = $1 AND ip <> 'rescue-command'), count(*) FILTER (WHERE ip = $2)
           FROM login_codes WHERE created_at > now() - make_interval(secs => $3)",
    )
    .bind(&email)
    .bind(&ip)
    .bind(policy.window_secs as f64)
    .fetch_one(&state.db)
    .await?;
    if policy.check_request(for_email, from_ip) != RequestDecision::Send {
        tracing::warn!(email, ip, for_email, from_ip, "login code refused: rate limit");
        return Err(ApiError::RateLimited);
    }

    let code = login::generate_code();
    sqlx::query(
        "INSERT INTO login_codes (id, email, code_hash, ip, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(login::hash_code(state.secrets.mac_key(), &email, &code))
    .bind(&ip)
    .bind(policy.code_ttl_secs as f64)
    .execute(&state.db)
    .await?;

    // The recipient's saved language; for a new address, the language of the page they are on.
    let saved: Option<(String,)> =
        sqlx::query_as("SELECT language FROM accounts WHERE email = $1").bind(&email).fetch_optional(&state.db).await?;
    let language = saved.map(|(l,)| l).or(body.language).unwrap_or_default();
    let minutes = (policy.code_ttl_secs / 60).to_string();
    state.mailer.send(&email, &language, Template::LoginCode, &[("code", &code), ("minutes", &minutes)]).await.map_err(|e| {
        tracing::error!(email, error = %format!("{e:#}"), "login code not sent");
        ApiError::MailUnavailable
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct VerifyBody {
    email: String,
    code: String,
    #[serde(default)]
    language: Option<String>,
}

/// `POST /api/auth/verify`: exchanges a code for a session. The first successful login of an
/// address creates its account.
pub async fn verify(
    State(state): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<VerifyBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let email = normalize_email(&body.email).ok_or(ApiError::InvalidEmail)?;
    let code = login::normalize_code(&body.code);
    let ip = client_ip(&headers, peer);
    let policy = state.config.login;

    let (failures,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM login_failures WHERE ip = $1 AND created_at > now() - make_interval(secs => $2)")
            .bind(&ip)
            .bind(policy.window_secs as f64)
            .fetch_one(&state.db)
            .await?;
    if policy.verification_blocked(failures) {
        tracing::warn!(email, ip, "login refused: too many wrong codes from this address");
        return Err(ApiError::RateLimited);
    }

    let submitted = login::hash_code(state.secrets.mac_key(), &email, &code);
    let candidates: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, code_hash FROM login_codes
          WHERE email = $1 AND consumed_at IS NULL AND expires_at > now() AND attempts < $2",
    )
    .bind(&email)
    .bind(policy.max_attempts_per_code)
    .fetch_all(&state.db)
    .await?;
    let matched = candidates.iter().find(|(_, hash)| login::hashes_equal(hash, &submitted)).map(|(id, _)| *id);
    let Some(code_id) = matched else {
        sqlx::query(
            "UPDATE login_codes SET attempts = attempts + 1 WHERE email = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(&email)
        .execute(&state.db)
        .await?;
        sqlx::query("INSERT INTO login_failures (ip, email) VALUES ($1, $2)").bind(&ip).bind(&email).execute(&state.db).await?;
        return Err(ApiError::InvalidCode);
    };

    let mut tx = state.db.begin().await?;
    // Consumed exactly once, even if two requests race.
    let consumed = sqlx::query("UPDATE login_codes SET consumed_at = now() WHERE id = $1 AND consumed_at IS NULL")
        .bind(code_id)
        .execute(&mut *tx)
        .await?;
    if consumed.rows_affected() != 1 {
        return Err(ApiError::InvalidCode);
    }
    // Every other code of this address dies with it.
    sqlx::query("UPDATE login_codes SET consumed_at = now() WHERE email = $1 AND consumed_at IS NULL")
        .bind(&email)
        .execute(&mut *tx)
        .await?;
    let language = body.language.as_deref().map(locale::resolve).unwrap_or(locale::DEFAULT_LANGUAGE);
    let account: Account = sqlx::query_as(
        "INSERT INTO accounts (id, email, language, last_login_at) VALUES ($1, $2, $3, now())
         ON CONFLICT (email) DO UPDATE SET last_login_at = now()
         RETURNING id, email, display_name, language, is_admin",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(language)
    .fetch_one(&mut *tx)
    .await?;
    let token = tak_manager_core::random_string(48, tak_manager_core::UNAMBIGUOUS);
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, account_id, expires_at) VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(Uuid::new_v4())
    .bind(hash_token(&token))
    .bind(account.id)
    .bind(state.config.session_ttl_days as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    housekeeping(&state).await;

    let cookie = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(state.config.cookie_secure)
        .same_site(SameSite::Lax)
        .expires(OffsetDateTime::now_utc() + Duration::days(state.config.session_ttl_days))
        .build();
    tracing::info!(email, ip, "signed in");
    Ok((jar.add(cookie), Json(me_payload(&state, &account).await?)))
}

/// Removes rows that no longer serve a purpose.
async fn housekeeping(state: &SharedState) {
    for sql in [
        "DELETE FROM login_codes WHERE created_at < now() - interval '2 days'",
        "DELETE FROM login_failures WHERE created_at < now() - interval '2 days'",
        "DELETE FROM sessions WHERE expires_at < now()",
    ] {
        if let Err(e) = sqlx::query(sql).execute(&state.db).await {
            tracing::warn!(error = %e, "housekeeping");
        }
    }
}

/// `POST /api/auth/logout`
pub async fn logout(State(state): State<SharedState>, jar: CookieJar) -> ApiResult<(CookieJar, StatusCode)> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(hash_token(cookie.value())).execute(&state.db).await?;
    }
    let removal = Cookie::build((SESSION_COOKIE, "")).path("/").build();
    Ok((jar.remove(removal), StatusCode::NO_CONTENT))
}

#[derive(Serialize, sqlx::FromRow)]
struct MyTeam {
    id: Uuid,
    name: String,
    slug: String,
    status: String,
    mode: String,
    role: String,
    decision_note: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct MyInvitation {
    id: Uuid,
    team_id: Uuid,
    team_name: String,
    invited_by: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

/// The account with its teams and the invitations waiting for it.
pub async fn me_payload(state: &SharedState, account: &Account) -> ApiResult<Value> {
    let teams: Vec<MyTeam> = sqlx::query_as(
        "SELECT t.id, t.name, t.slug, t.status, t.mode, m.role, t.decision_note
           FROM team_members m JOIN teams t ON t.id = m.team_id
          WHERE m.account_id = $1
          ORDER BY t.created_at",
    )
    .bind(account.id)
    .fetch_all(&state.db)
    .await?;
    let invitations: Vec<MyInvitation> = sqlx::query_as(
        "SELECT i.id, i.team_id, t.name AS team_name,
                CASE WHEN a.display_name <> '' THEN a.display_name ELSE a.email END AS invited_by, i.created_at
           FROM invitations i JOIN teams t ON t.id = i.team_id JOIN accounts a ON a.id = i.invited_by
          WHERE i.email = $1 AND i.status = 'pending' AND t.status = 'active'
          ORDER BY i.created_at",
    )
    .bind(&account.email)
    .fetch_all(&state.db)
    .await?;
    Ok(json!({ "account": account, "teams": teams, "invitations": invitations }))
}

/// `GET /api/me`
pub async fn me(State(state): State<SharedState>, CurrentAccount(account): CurrentAccount) -> ApiResult<Json<Value>> {
    Ok(Json(me_payload(&state, &account).await?))
}

#[derive(Deserialize)]
pub struct UpdateMeBody {
    display_name: Option<String>,
    language: Option<String>,
}

/// `PATCH /api/me`: display name and language.
pub async fn update_me(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Json(body): Json<UpdateMeBody>,
) -> ApiResult<Json<Value>> {
    if let Some(language) = &body.language {
        if !locale::is_supported(language) {
            return Err(ApiError::InvalidInput);
        }
    }
    let display_name = body.display_name.map(|n| n.trim().to_string());
    if display_name.as_ref().is_some_and(|n| n.chars().count() > 80 || n.chars().any(char::is_control)) {
        return Err(ApiError::InvalidInput);
    }
    let updated: Account = sqlx::query_as(
        "UPDATE accounts SET display_name = COALESCE($2, display_name), language = COALESCE($3, language)
          WHERE id = $1 RETURNING id, email, display_name, language, is_admin",
    )
    .bind(account.id)
    .bind(display_name)
    .bind(body.language)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(me_payload(&state, &updated).await?))
}

/// `GET /api/config`: what the UI needs before anyone is signed in.
pub async fn public_config(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({
        "languages": locale::languages(),
        "default_language": locale::DEFAULT_LANGUAGE,
        "code_ttl_minutes": state.config.login.code_ttl_secs / 60,
    }))
}

/// Server-side rescue: makes `email` an administrator and returns a one-time login code for
/// it, so an administrator can get in when e-mail is down.
pub async fn grant_admin(state: &SharedState, email: &str) -> anyhow::Result<(String, i64)> {
    let email = normalize_email(email).ok_or_else(|| anyhow::anyhow!("{email:?} is not an e-mail address"))?;
    sqlx::query(
        "INSERT INTO accounts (id, email, is_admin) VALUES ($1, $2, true)
         ON CONFLICT (email) DO UPDATE SET is_admin = true",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .execute(&state.db)
    .await?;
    let code = login::generate_code();
    sqlx::query(
        "INSERT INTO login_codes (id, email, code_hash, ip, expires_at)
         VALUES ($1, $2, $3, 'rescue-command', now() + make_interval(secs => $4))",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(login::hash_code(state.secrets.mac_key(), &email, &code))
    .bind(state.config.login.code_ttl_secs as f64)
    .execute(&state.db)
    .await?;
    Ok((code, state.config.login.code_ttl_secs / 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_address_comes_from_the_proxy_header() {
        let peer: SocketAddr = "172.18.0.5:4321".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers, peer), "172.18.0.5");
        headers.insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        assert_eq!(client_ip(&headers, peer), "203.0.113.7");
        headers.insert("x-forwarded-for", "10.0.0.1, 203.0.113.9".parse().unwrap());
        assert_eq!(client_ip(&headers, peer), "203.0.113.9", "the element added by the proxy");
        headers.insert("x-forwarded-for", "not an address".parse().unwrap());
        assert_eq!(client_ip(&headers, peer), "172.18.0.5");
    }
}
