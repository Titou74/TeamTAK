//! The platform's side of the host agent's API. Every call has a timeout: the agent being
//! slow or absent must never make the platform slow.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tak_manager_agent::report::{AgentError, BackupRequest, BackupResult, LetsEncryptResult, Report, RestartResult, RotateRequest, RotateResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCallError {
    /// No answer: the agent is stopped, starting, or cut off.
    Unreachable(String),
    /// The agent answered and refused, with its stable code (`service_not_allowed`, `busy`…).
    Refused { code: String, detail: Option<String> },
}

impl std::fmt::Display for AgentCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentCallError::Unreachable(e) => write!(f, "the host agent does not answer: {e}"),
            AgentCallError::Refused { code, detail } => write!(f, "the host agent refused ({code}){}", detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default()),
        }
    }
}

pub struct AgentClient {
    url: String,
    token: String,
    http: reqwest::Client,
}

const REPORT_TIMEOUT: Duration = Duration::from_secs(20);
/// A restart waits for the container to stop (30 s) and start again.
const RESTART_TIMEOUT: Duration = Duration::from_secs(200);
/// The agent gives its backup script 30 minutes.
const BACKUP_TIMEOUT: Duration = Duration::from_secs(31 * 60);
/// A certificate hand-over restarts TAK Server and waits for it to be healthy.
const LETSENCRYPT_TIMEOUT: Duration = Duration::from_secs(16 * 60);
const ROTATE_TIMEOUT: Duration = Duration::from_secs(120);

impl AgentClient {
    pub fn new(url: &str, token: &str) -> Result<Self, String> {
        // Plain HTTP on a private Docker network: no certificate is involved, but the client
        // is built like the platform's other ones, with its TLS set up explicitly. Each call
        // sets its own timeout.
        let http = tak_manager_core::public_http_client(REPORT_TIMEOUT)?;
        Ok(Self { url: url.trim_end_matches('/').to_string(), token: token.to_string(), http })
    }

    async fn call<B: Serialize, T: DeserializeOwned>(&self, method: reqwest::Method, path: &str, body: Option<&B>, timeout: Duration) -> Result<T, AgentCallError> {
        let mut request = self.http.request(method, format!("{}{path}", self.url)).bearer_auth(&self.token).timeout(timeout);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| AgentCallError::Unreachable(describe(e)))?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|e| AgentCallError::Unreachable(describe(e)))?;
        if !status.is_success() {
            let refusal: AgentError = serde_json::from_slice(&bytes).unwrap_or(AgentError { error: format!("http_{}", status.as_u16()), detail: None });
            return Err(AgentCallError::Refused { code: refusal.error, detail: refusal.detail });
        }
        serde_json::from_slice(&bytes).map_err(|e| AgentCallError::Unreachable(format!("unreadable answer: {e}")))
    }

    pub async fn report(&self) -> Result<Report, AgentCallError> {
        self.call::<(), _>(reqwest::Method::GET, "/v1/report", None, REPORT_TIMEOUT).await
    }

    pub async fn restart(&self, service: &str) -> Result<RestartResult, AgentCallError> {
        // The service was checked against the fixed list before coming here; the agent checks
        // again on its side.
        self.call::<(), _>(reqwest::Method::POST, &format!("/v1/services/{service}/restart"), None, RESTART_TIMEOUT).await
    }

    pub async fn backup(&self, request: &BackupRequest) -> Result<BackupResult, AgentCallError> {
        self.call(reqwest::Method::POST, "/v1/backup", Some(request), BACKUP_TIMEOUT).await
    }

    pub async fn letsencrypt_sync(&self) -> Result<LetsEncryptResult, AgentCallError> {
        self.call::<(), _>(reqwest::Method::POST, "/v1/letsencrypt/sync", None, LETSENCRYPT_TIMEOUT).await
    }

    pub async fn rotate_logs(&self, request: &RotateRequest) -> Result<RotateResult, AgentCallError> {
        self.call(reqwest::Method::POST, "/v1/logs/rotate", Some(request), ROTATE_TIMEOUT).await
    }
}

/// A short reason, without the address (which is always the same) and without the token.
fn describe(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "no answer in time".to_string()
    } else if e.is_connect() {
        "connection refused or host unknown".to_string()
    } else {
        e.without_url().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot HTTP server that answers `response` and gives back the request it received.
    async fn one_shot(response: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 8192];
            let n = stream.read(&mut buffer).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = stream.shutdown().await;
            String::from_utf8_lossy(&buffer[..n]).into_owned()
        });
        (url, handle)
    }

    #[tokio::test]
    async fn calls_carry_the_shared_secret_and_name_the_service() {
        let (url, received) = one_shot("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 40\r\nConnection: close\r\n\r\n{\"service\":\"caddy\",\"container\":\"abc123\"}").await;
        let agent = AgentClient::new(&url, "the-shared-secret").unwrap();
        let done = agent.restart("caddy").await.unwrap();
        assert_eq!((done.service.as_str(), done.container.as_str()), ("caddy", "abc123"));
        let request = received.await.unwrap();
        assert!(request.starts_with("POST /v1/services/caddy/restart HTTP/1.1"), "{request}");
        assert!(request.to_lowercase().contains("authorization: bearer the-shared-secret"));
    }

    #[tokio::test]
    async fn a_refusal_keeps_the_agent_code() {
        let (url, _) = one_shot("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: 45\r\nConnection: close\r\n\r\n{\"error\":\"service_not_allowed\",\"detail\":null}").await;
        let agent = AgentClient::new(&url, "s").unwrap();
        assert_eq!(agent.restart("mapping-db-1").await, Err(AgentCallError::Refused { code: "service_not_allowed".into(), detail: None }));
    }

    #[tokio::test]
    async fn an_absent_agent_is_unreachable_not_an_error_of_the_platform() {
        // A port nobody listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let agent = AgentClient::new(&url, "s").unwrap();
        let error = agent.report().await.unwrap_err();
        assert!(matches!(error, AgentCallError::Unreachable(_)), "{error:?}");
        assert!(!error.to_string().contains("127.0.0.1"), "no address in what is shown: {error}");
    }
}
