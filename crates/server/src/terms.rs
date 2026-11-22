//! Terms of use: which version is in force, and its acceptance by each account.
//!
//! The text lives with the UI's translations (`web/src/locales/<language>/terms.json`), which
//! carries the version it is. Signing in requires accepting that version; an account that
//! accepted an older one, or none, is asked again at its next visit (`POST /api/me/terms`).

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{me_payload, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

/// The version of the terms of use in force: the day their text last changed. Change it with
/// the text, in every `terms.json` (a test checks they agree): every account then has to
/// accept the new text before going on.
pub const TERMS_VERSION: &str = "2026-11-22";

/// Refuses unless `version` is the version in force: the one the person was shown and
/// accepted. A page loaded before the terms changed sends the old one.
pub fn require_current(version: Option<&str>) -> ApiResult<()> {
    if version == Some(TERMS_VERSION) {
        Ok(())
    } else {
        Err(ApiError::TermsNotAccepted)
    }
}

/// What the UI shows of the account's acceptance.
#[derive(Serialize)]
pub struct Status {
    /// The version in force.
    version: &'static str,
    /// When the account accepted it; `None` when it has not, and must before going on.
    #[serde(with = "time::serde::rfc3339::option")]
    accepted_at: Option<OffsetDateTime>,
}

pub async fn status(db: &PgPool, account_id: Uuid) -> ApiResult<Status> {
    let accepted: Option<(Option<OffsetDateTime>,)> =
        sqlx::query_as("SELECT terms_accepted_at FROM accounts WHERE id = $1 AND terms_version = $2")
            .bind(account_id)
            .bind(TERMS_VERSION)
            .fetch_optional(db)
            .await?;
    Ok(Status { version: TERMS_VERSION, accepted_at: accepted.and_then(|(at,)| at) })
}

#[derive(Deserialize)]
pub struct AcceptBody {
    version: Option<String>,
}

/// `POST /api/me/terms`: the signed-in account accepts the terms of use in force.
pub async fn accept(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Json(body): Json<AcceptBody>,
) -> ApiResult<Json<Value>> {
    require_current(body.version.as_deref())?;
    sqlx::query("UPDATE accounts SET terms_version = $2, terms_accepted_at = now() WHERE id = $1")
        .bind(account.id)
        .bind(TERMS_VERSION)
        .execute(&state.db)
        .await?;
    tracing::info!(email = account.email, version = TERMS_VERSION, "terms of use accepted");
    Ok(Json(me_payload(&state, &account).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_version_in_force_is_accepted() {
        assert!(require_current(Some(TERMS_VERSION)).is_ok());
        assert!(matches!(require_current(None), Err(ApiError::TermsNotAccepted)));
        assert!(matches!(require_current(Some("2000-01-01")), Err(ApiError::TermsNotAccepted)));
    }

    /// The text the UI shows is the version the server asks for, in every language.
    #[test]
    fn every_text_of_the_terms_is_the_version_in_force() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        let mut languages = 0;
        for entry in std::fs::read_dir(&locales).unwrap() {
            let file = entry.unwrap().path().join("terms.json");
            assert!(file.is_file(), "{} is missing", file.display());
            languages += 1;
            let json: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(json["version"], TERMS_VERSION, "version of {}: change it with the text, and TERMS_VERSION too", file.display());
            let sections = json["sections"].as_array().map(Vec::len).unwrap_or(0);
            assert!(sections > 0, "{} has no section", file.display());
        }
        assert!(languages >= 2, "expected at least English and French in {}", locales.display());
    }
}
