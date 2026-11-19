//! TAK Manager server: the JSON API, the static web UI and the reconciliation loop.

mod accesses;
mod admin;
mod auth;
mod config;
mod error;
mod locale;
mod mail;
mod reconciler;
mod state;
mod teams;

use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::error::ApiError;
use crate::state::SharedState;

#[derive(Parser)]
#[command(name = "tak-manager", version, about = "Self-service team management for a TAK Server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the web server and the reconciliation loop.
    Serve,
    /// Rescue: makes an account administrator (creating it if needed) and prints a one-time
    /// login code for it. Works when e-mail is down.
    GrantAdmin {
        /// E-mail address of the account.
        email: String,
    },
    /// Checks that the database, the directory and TAK Server can be reached.
    Status,
    /// Runs one reconciliation pass now and prints what it changed.
    Reconcile,
    /// Exits with 0 when the local server answers (container healthcheck).
    Healthcheck,
}

/// State-changing requests must carry this header. A page of another site cannot add a
/// custom header to a cross-site request, so it cannot act with a visitor's session.
const CSRF_HEADER: &str = "x-requested-with";
const CSRF_VALUE: &str = "tak-manager";

async fn require_csrf_header(request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let present = request.headers().get(CSRF_HEADER).and_then(|v| v.to_str().ok()) == Some(CSRF_VALUE);
    if safe || present {
        next.run(request).await
    } else {
        ApiError::Forbidden.into_response()
    }
}

/// Largest request body the API accepts. Its requests are small JSON documents.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Reads the whole request body before the handler runs. A handler that takes no body would
/// otherwise leave it unread; the connection is then closed with data pending and the proxy
/// sees a reset instead of the answer.
async fn read_whole_body(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => next.run(Request::from_parts(parts, axum::body::Body::from(bytes))).await,
        Err(_) => ApiError::InvalidInput.into_response(),
    }
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    // Answers hold credentials and personal data: never cached.
    response.headers_mut().entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    response
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'self'",
        ),
    );
    response
}

fn api_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "status": "ok" })) }))
        .route("/config", get(auth::public_config))
        .route("/auth/request-code", post(auth::request_code))
        .route("/auth/verify", post(auth::verify))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(auth::me).patch(auth::update_me))
        .route("/teams", post(teams::request_team))
        .route("/teams/{id}", get(teams::team_detail).patch(teams::update_team).delete(teams::delete_team))
        .route("/teams/{id}/invitations", post(teams::invite))
        .route("/teams/{id}/invitations/{invitation_id}", delete(teams::cancel_invitation))
        .route("/teams/{id}/members/{account_id}", patch(teams::set_member_role).delete(teams::remove_member))
        .route("/teams/{id}/accesses", post(accesses::create_access))
        .route("/invitations/{id}/answer", post(teams::answer_invitation))
        .route("/accesses/{id}", get(accesses::access_detail).delete(accesses::delete_access))
        .route("/accesses/{id}/package/{form}", get(accesses::access_package))
        .route("/accesses/{id}/regenerate", post(accesses::regenerate_access))
        .route("/admin/teams", get(admin::list_teams))
        .route("/admin/teams/{id}", patch(admin::update_team))
        .route("/admin/teams/{id}/decision", post(admin::decide_team))
        .route("/admin/accounts", get(admin::list_accounts))
        .route("/admin/accounts/{id}", patch(admin::update_account))
        .route("/admin/reconciliation", get(admin::reconciliation))
        .route("/admin/reconciliation/run", post(admin::run_reconciliation))
        .fallback(|| async { ApiError::NotFound })
        .layer(middleware::from_fn(read_whole_body))
        .layer(middleware::from_fn(require_csrf_header))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

fn app(state: SharedState) -> Router {
    let web_dir = state.config.web_dir.clone();
    let router = Router::new().nest("/api", api_router(state));
    let router = match web_dir {
        // Single-page application: unknown paths get index.html and the UI routes them.
        Some(dir) => router.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => router.fallback(|| async { (StatusCode::NOT_FOUND, "web UI not bundled") }),
    };
    router.layer(middleware::from_fn(security_headers)).layer(TraceLayer::new_for_http())
}

async fn serve(config: Config) -> Result<()> {
    let listen_addr = config.listen_addr;
    let interval = config.reconcile_interval;
    let state = state::build_state(config).await?;
    tokio::spawn(reconciler::run_periodically(state.reconciler.clone(), interval));

    let listener = tokio::net::TcpListener::bind(listen_addr).await.with_context(|| format!("listening on {listen_addr}"))?;
    tracing::info!(%listen_addr, languages = ?locale::languages(), "TAK Manager is listening");
    axum::serve(listener, app(state).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        })
        .await?;
    Ok(())
}

async fn status(config: Config) -> Result<()> {
    let db = state::connect_database(&config).await?;
    let (accounts,): (i64,) = sqlx::query_as("SELECT count(*) FROM accounts").fetch_one(&db).await?;
    println!("database:   ok ({accounts} accounts)");
    let state::Backends { directory, tak, .. } = state::build_backends(&config)?;
    let mut failed = false;
    match directory.snapshot().await {
        Ok(s) => println!("directory:  ok ({} accounts, {} groups)", s.users.len(), s.groups.len()),
        Err(e) => {
            failed = true;
            println!("directory:  FAILED: {e}");
        }
    }
    // The version alone only proves the port answers; listing certificates also proves that
    // the platform's certificate is accepted as administrator.
    match tak.version().await {
        Ok(v) => match tak.active_certificates().await {
            Ok(c) => println!("TAK Server: ok (version {v}, {} active client certificates)", c.len()),
            Err(e) => {
                failed = true;
                println!("TAK Server: version {v}, but the administrator API refused the platform's certificate: {e}");
            }
        },
        Err(e) => {
            failed = true;
            println!("TAK Server: FAILED: {e}");
        }
    }
    if failed {
        bail!("at least one backend cannot be reached");
    }
    Ok(())
}

async fn healthcheck(listen_addr: SocketAddr) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let target = SocketAddr::from(([127, 0, 0, 1], listen_addr.port()));
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(3), tokio::net::TcpStream::connect(target)).await??;
    stream.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
    let mut answer = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(3), stream.read_to_string(&mut answer)).await??;
    if answer.starts_with("HTTP/1.0 200") || answer.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        bail!("unexpected answer: {}", answer.lines().next().unwrap_or(""))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn,sqlx=warn".into()),
        )
        .init();
    let cli = Cli::parse();
    if let Command::Healthcheck = cli.command {
        let addr: SocketAddr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()).parse()?;
        return healthcheck(addr).await;
    }
    let config = Config::from_env()?;
    match cli.command {
        Command::Serve => serve(config).await,
        Command::Status => status(config).await,
        Command::GrantAdmin { email } => {
            let state = state::build_state(config).await?;
            let (code, minutes) = auth::grant_admin(&state, &email).await?;
            println!("{email} is a platform administrator.");
            println!("One-time login code (valid {minutes} minutes): {code}");
            println!("Enter it on the sign-in page of {} with that e-mail address.", state.config.public_url);
            Ok(())
        }
        Command::Reconcile => {
            let state = state::build_state(config).await?;
            let report = state.reconciler.run(reconciler::Trigger::Manual).await?;
            for change in &report.changes {
                println!("{} {} {}", change.kind(), change.subject(), change.group().unwrap_or(""));
            }
            for error in &report.errors {
                println!("ERROR {error}");
            }
            println!("{} change(s), {} error(s)", report.changes.len(), report.errors.len());
            if report.errors.is_empty() {
                Ok(())
            } else {
                bail!("the pass ended with errors")
            }
        }
        Command::Healthcheck => unreachable!("handled above"),
    }
}
