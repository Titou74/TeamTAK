//! TAK Server's own administration pages, for the platform's administrators.
//!
//! Caddy serves them on a port of their own (`TAK_ADMIN_PORT`, 8443 by default) and passes
//! each request on to TAK Server with a certificate TAK Server knows as administrator. Before
//! that, it asks [`authorize`] whether the browser's TeamTAK session belongs to an
//! administrator (`forward_auth`). The browser holds no certificate and TAK Server's own
//! port 8443 is not published: signing in to TeamTAK is the only way in. A browser sends a
//! cookie to every port of the host that set it, so the web UI's session reaches here.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use uuid::Uuid;

use crate::audit::{self, AuditAction, Details, Target};
use crate::auth::AdminAccount;
use crate::error::ApiError;
use crate::state::SharedState;

/// An administrator's visit is recorded at most this often: one page of TAK Server's
/// administration is dozens of requests, each of them asked about here.
const RECORD_EVERY: Duration = Duration::from_secs(3600);

/// When each administrator's visit was last recorded in the audit log.
#[derive(Default)]
pub struct Visits(Mutex<HashMap<Uuid, Instant>>);

impl Visits {
    /// True when a visit of this account at `now` is to be recorded, and notes it.
    fn should_record(&self, account: Uuid, now: Instant) -> bool {
        let mut seen = self.0.lock().unwrap();
        seen.retain(|_, at| now.duration_since(*at) < RECORD_EVERY);
        if seen.contains_key(&account) {
            return false;
        }
        seen.insert(account, now);
        true
    }
}

/// `GET /api/tak-admin/authorize`, asked by Caddy about every request to TAK Server's
/// administration: 204 lets it through. Without an administrator's session, the browser is
/// sent to the web UI, where the sign-in page is; nothing of TAK Server is shown.
pub async fn authorize(State(state): State<SharedState>, admin: Result<AdminAccount, ApiError>) -> Response {
    if state.config.tak_admin_url.is_none() {
        return ApiError::NotFound.into_response();
    }
    match admin {
        Ok(AdminAccount(account)) => {
            if state.tak_admin_visits.should_record(account.id, Instant::now()) {
                audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TakAdminOpened, Target::none(), Details::new()).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(ApiError::Unauthenticated | ApiError::Forbidden) => Redirect::to(&format!("{}/", state.config.public_url)).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visit_is_recorded_once_an_hour_per_administrator() {
        let visits = Visits::default();
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let start = Instant::now();
        assert!(visits.should_record(alice, start));
        assert!(!visits.should_record(alice, start + Duration::from_secs(1)), "the next request of the same page");
        assert!(visits.should_record(bob, start + Duration::from_secs(2)), "another administrator");
        assert!(!visits.should_record(alice, start + Duration::from_secs(3599)));
        assert!(visits.should_record(alice, start + RECORD_EVERY), "an hour later");
    }
}
