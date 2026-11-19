//! The agent's HTTP API. It listens on a Docker network that only the platform's container
//! is attached to, and every request must carry the shared secret.
//!
//! | Call | Does |
//! |---|---|
//! | `GET /v1/health` | Answers when the agent runs |
//! | `GET /v1/report` | Host, containers, TAK Server processes, backups, Let's Encrypt |
//! | `POST /v1/services/{service}/restart` | Restarts one service of this stack |
//! | `POST /v1/backup` | Writes a backup archive |
//! | `POST /v1/letsencrypt/sync` | Hands the Let's Encrypt certificate to TAK Server |
//! | `POST /v1/logs/rotate` | Housekeeping of TAK Server's logs |
//!
//! There is nothing else: no command, no path and no container can be named.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::agent::{ActionError, Agent};
use crate::report::{AgentError, BackupRequest, RotateRequest};

fn refusal(status: StatusCode, code: &str, detail: Option<String>) -> Response {
    (status, Json(AgentError { error: code.to_string(), detail })).into_response()
}

impl IntoResponse for ActionError {
    fn into_response(self) -> Response {
        match self {
            ActionError::NotAllowed => refusal(StatusCode::FORBIDDEN, "service_not_allowed", None),
            ActionError::NotFound => refusal(StatusCode::NOT_FOUND, "service_not_found", None),
            ActionError::Busy => refusal(StatusCode::CONFLICT, "busy", None),
            ActionError::Failed(detail) => refusal(StatusCode::INTERNAL_SERVER_ERROR, "failed", Some(detail)),
        }
    }
}

/// Compares two secrets without stopping at the first difference.
pub fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn authenticate(State(agent): State<Arc<Agent>>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if agent.config.token.is_empty() || !same_secret(presented.as_bytes(), agent.config.token.as_bytes()) {
        return refusal(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    next.run(request).await
}

/// An optional JSON body: empty means "defaults".
fn body<T: DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, Response> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|_| refusal(StatusCode::BAD_REQUEST, "invalid_input", None))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn report(State(agent): State<Arc<Agent>>) -> Response {
    Json(agent.report().await).into_response()
}

async fn restart(State(agent): State<Arc<Agent>>, Path(service): Path<String>) -> Response {
    match agent.restart(&service).await {
        Ok(done) => Json(done).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn backup(State(agent): State<Arc<Agent>>, bytes: Bytes) -> Response {
    let request: BackupRequest = match body(&bytes) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    match agent.backup(request).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn letsencrypt(State(agent): State<Arc<Agent>>) -> Response {
    match agent.letsencrypt_sync().await {
        Ok(result) => Json(result).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn rotate(State(agent): State<Arc<Agent>>, bytes: Bytes) -> Response {
    let request: RotateRequest = match body(&bytes) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    match agent.rotate_logs(request).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => e.into_response(),
    }
}

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/report", get(report))
        .route("/v1/services/{service}/restart", post(restart))
        .route("/v1/backup", post(backup))
        .route("/v1/letsencrypt/sync", post(letsencrypt))
        .route("/v1/logs/rotate", post(rotate))
        .fallback(|| async { refusal(StatusCode::NOT_FOUND, "not_found", None) })
        .layer(middleware::from_fn_with_state(agent.clone(), authenticate))
        .with_state(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::test_agent;
    use crate::docker::fake::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves the router on a local port and sends it one raw request.
    async fn call(agent: Arc<Agent>, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, serde_json::Value) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router(agent)).await });
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let authorization = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: agent\r\nConnection: close\r\n{authorization}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        server.abort();
        let status: u16 = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
        let payload = answer.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        // The body may be chunked: take the JSON object in it.
        let json = payload.find('{').zip(payload.rfind('}')).map(|(a, b)| &payload[a..=b]).unwrap_or("null");
        (status, serde_json::from_str(json).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn every_call_needs_the_shared_secret() {
        let docker = Arc::new(FakeDocker::default());
        let (agent, root) = test_agent(docker.clone(), "api-auth");
        let token = agent.config.token.clone();
        for (method, path) in [("GET", "/v1/health"), ("GET", "/v1/report"), ("POST", "/v1/services/caddy/restart"), ("POST", "/v1/backup")] {
            let (status, body) = call(agent.clone(), method, path, None, "").await;
            assert_eq!((status, body["error"].as_str()), (401, Some("unauthorized")), "{method} {path} without the secret");
            let (status, _) = call(agent.clone(), method, path, Some("wrong"), "").await;
            assert_eq!(status, 401, "{method} {path} with a wrong secret");
        }
        assert!(docker.calls().is_empty(), "nothing was done for a caller without the secret");
        let (status, body) = call(agent.clone(), "GET", "/v1/health", Some(&token), "").await;
        assert_eq!((status, body["status"].as_str()), (200, Some("ok")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_api_restarts_a_service_of_the_stack_and_refuses_the_rest() {
        let docker = Arc::new(FakeDocker { leaky_filter: true, ..Default::default() });
        let (agent, root) = test_agent(docker.clone(), "api-restart");
        let token = agent.config.token.clone();

        for target in ["mapping-db-1", "opsan-db", "agent", "tak-manager-caddy-1", FOREIGN_ID, "caddy%3Bls", "%2E%2E"] {
            let (status, body) = call(agent.clone(), "POST", &format!("/v1/services/{target}/restart"), Some(&token), "").await;
            assert_eq!((status, body["error"].as_str()), (403, Some("service_not_allowed")), "{target}");
        }
        assert!(docker.actions().is_empty());

        let (status, body) = call(agent.clone(), "POST", "/v1/services/caddy/restart", Some(&token), "").await;
        assert_eq!((status, body["service"].as_str(), body["container"].as_str()), (200, Some("caddy"), Some(CADDY_ID)));
        assert_eq!(docker.actions().len(), 1);

        let (status, body) = call(agent.clone(), "POST", "/v1/services/mailpit/restart", Some(&token), "").await;
        assert_eq!((status, body["error"].as_str()), (404, Some("service_not_found")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn there_is_no_other_call() {
        let docker = Arc::new(FakeDocker::default());
        let (agent, root) = test_agent(docker.clone(), "api-closed");
        let token = agent.config.token.clone();
        for (method, path) in [
            ("POST", "/v1/exec"),
            ("POST", "/v1/containers/mapping-db-1/stop"),
            ("POST", "/v1/services/caddy/stop"),
            ("DELETE", "/v1/services/caddy"),
            ("GET", "/v1/files/etc/shadow"),
            ("POST", "/v1/updates/apply"),
            ("POST", "/v1/reboot"),
        ] {
            let (status, _) = call(agent.clone(), method, path, Some(&token), "{\"command\":\"id\"}").await;
            assert!(status == 404 || status == 405, "{method} {path}: {status}");
        }
        assert!(docker.calls().is_empty());
        let (status, body) = call(agent.clone(), "POST", "/v1/backup", Some(&token), "not json").await;
        assert_eq!((status, body["error"].as_str()), (400, Some("invalid_input")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn secrets_are_compared_whole() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"ab"));
        assert!(!same_secret(b"", b"a"));
    }
}
