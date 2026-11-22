//! TeamTAK server: the JSON API, the static web UI and the reconciliation loop.

mod accesses;
mod admin;
mod audit;
mod auth;
mod config;
mod error;
mod events;
mod locale;
mod mail;
mod maps;
mod monitor;
mod reconciler;
mod settings;
mod state;
mod tak_admin;
mod teams;
mod video;

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
#[command(name = "teamtak", version, about = "Self-service team management for a TAK Server")]
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
    /// Sends a test e-mail through the configured mail server and prints its answer. Works
    /// before anyone can sign in.
    SendTestMail {
        /// E-mail address of the recipient.
        email: String,
        /// Language of the e-mail.
        #[arg(long, default_value = locale::DEFAULT_LANGUAGE)]
        language: String,
    },
    /// Runs one reconciliation pass now and prints what it changed.
    Reconcile,
    /// Exits with 0 when the local server answers (container healthcheck).
    Healthcheck,
}

/// State-changing requests must carry this header. A page of another site cannot add a
/// custom header to a cross-site request, so it cannot act with a visitor's session.
const CSRF_HEADER: &str = "x-requested-with";
const CSRF_VALUE: &str = "teamtak";

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
/// Largest body of an upload of map sources: one file, base64 in JSON.
const MAX_UPLOAD_BODY_BYTES: usize = teamtak_core::maps::MAX_UPLOAD_BYTES * 4 / 3 + 4096;

/// How large a request's body may be.
fn body_limit(request: &Request) -> usize {
    // Inside `/api`, the path no longer carries it.
    let path = request.uri().path();
    let path = path.strip_prefix("/api").unwrap_or(path);
    let upload = path == "/admin/map-sources" || (path.starts_with("/teams/") && path.ends_with("/map-sources"));
    if request.method() == Method::POST && upload {
        MAX_UPLOAD_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    }
}

/// Reads the whole request body before the handler runs. A handler that takes no body would
/// otherwise leave it unread; the connection is then closed with data pending and the proxy
/// sees a reset instead of the answer.
async fn read_whole_body(request: Request, next: Next) -> Response {
    let limit = body_limit(&request);
    let (parts, body) = request.into_parts();
    match axum::body::to_bytes(body, limit).await {
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
        .route("/auth/challenge", get(auth::challenge))
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
        .route("/teams/{id}/devices", get(monitor::devices::team_devices))
        .route("/teams/{id}/invitable-teams", get(events::invitable_teams))
        .route("/teams/{id}/events", get(events::list_team_events).post(events::create_event))
        .route("/teams/{id}/events/{event_id}", patch(events::update_event))
        .route("/teams/{id}/events/{event_id}/end", post(events::end_event))
        .route("/teams/{id}/events/{event_id}/cancel", post(events::cancel_event))
        .route("/teams/{id}/events/{event_id}/teams", post(events::invite_team))
        .route("/teams/{id}/events/{event_id}/teams/{other_team_id}", delete(events::remove_team))
        .route("/teams/{id}/events/{event_id}/answer", post(events::answer_event))
        .route("/teams/{id}/events/{event_id}/leave", post(events::leave_event))
        .route("/teams/{id}/video", get(video::team_video).post(video::create_stream))
        .route("/video-streams/{id}", get(video::stream_detail).delete(video::delete_stream))
        .route("/video-streams/{id}/regenerate", post(video::regenerate_stream))
        .route("/teams/{id}/maps", get(maps::team_maps).put(maps::set_team_maps))
        .route("/teams/{id}/maps/package", get(maps::team_maps_package))
        .route("/teams/{id}/map-sources", post(maps::team_upload))
        .route("/teams/{id}/map-sources/{source_id}", delete(maps::team_remove_source))
        .route("/map-sources/{id}/check", post(maps::check_now))
        .route("/teams/{id}/events/{event_id}/maps", get(events::event_maps).put(events::set_event_maps))
        .route("/invitations/{id}/answer", post(teams::answer_invitation))
        .route("/accesses/{id}", get(accesses::access_detail).delete(accesses::delete_access))
        .route("/accesses/{id}/package/{form}", get(accesses::access_package))
        .route("/accesses/{id}/regenerate", post(accesses::regenerate_access))
        .route("/admin/teams", get(admin::list_teams))
        .route("/admin/teams/{id}", patch(admin::update_team))
        .route("/admin/teams/{id}/decision", post(admin::decide_team))
        .route("/admin/events", get(events::admin_list_events).post(events::admin_create_event))
        .route("/admin/events/{id}", patch(events::admin_update_event))
        .route("/admin/events/{id}/end", post(events::admin_end_event))
        .route("/admin/events/{id}/cancel", post(events::admin_cancel_event))
        .route("/admin/events/{id}/teams", post(events::admin_assign_team))
        .route("/admin/events/{id}/teams/{team_id}", delete(events::admin_remove_team))
        .route("/admin/events/{id}/maps", get(events::admin_event_maps).put(events::admin_set_event_maps))
        .route("/admin/map-sources", get(maps::admin_catalogue).post(maps::upload))
        .route("/admin/map-sources/suggestions", post(maps::add_suggestions))
        .route("/admin/map-sources/{id}", delete(maps::remove_source))
        .route("/admin/accounts", get(admin::list_accounts).post(admin::invite_account))
        .route("/admin/accounts/{id}", patch(admin::update_account))
        .route("/admin/reconciliation", get(admin::reconciliation))
        .route("/admin/reconciliation/run", post(admin::run_reconciliation))
        .route("/admin/monitoring", get(monitor::api::monitoring))
        .route("/admin/monitoring/history", get(monitor::api::history))
        .route("/admin/alerts/summary", get(monitor::api::alerts_summary))
        .route("/admin/devices", get(monitor::devices::admin_devices))
        .route("/admin/services/{service}/restart", post(monitor::api::restart_service))
        .route("/admin/backups/run", post(monitor::api::run_backup))
        .route("/admin/letsencrypt/sync", post(monitor::api::letsencrypt_sync))
        .route("/admin/release/check", post(monitor::api::check_release))
        .route("/admin/settings", get(settings::get).patch(settings::update))
        .route("/admin/settings/webhook-test", post(monitor::api::webhook_test))
        .route("/admin/settings/mail-test", post(monitor::api::mail_test))
        .route("/admin/audit", get(audit::list))
        // Asked by Caddy about each request to TAK Server's administration pages.
        .route("/tak-admin/authorize", get(tak_admin::authorize))
        .fallback(|| async { ApiError::NotFound })
        // The client's address, for the audit log.
        .layer(middleware::from_fn(audit::with_request_ip))
        .layer(middleware::from_fn(read_whole_body))
        .layer(middleware::from_fn(require_csrf_header))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

/// What the other services of the stack ask the platform, on its internal port: the video
/// server, about each device that wants to publish or watch. Caddy passes nothing to this
/// port, so nothing outside the stack reaches it.
fn internal_router(state: SharedState) -> Router {
    Router::new()
        .route("/internal/video/auth", post(video::authorize))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(read_whole_body))
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
    let internal_addr = config.internal_listen_addr;
    let interval = config.reconcile_interval;
    let state = state::build_state(config).await?;
    tokio::spawn(reconciler::run_periodically(state.reconciler.clone(), interval));
    tokio::spawn(monitor::run_periodically(state.monitor.clone()));
    tokio::spawn(maps::run_checks_periodically(state.clone()));

    let internal = tokio::net::TcpListener::bind(internal_addr).await.with_context(|| format!("listening on {internal_addr}"))?;
    let internal_app = internal_router(state.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(internal, internal_app).await {
            tracing::error!(error = %e, "the internal port stopped");
        }
    });
    let listener = tokio::net::TcpListener::bind(listen_addr).await.with_context(|| format!("listening on {listen_addr}"))?;
    tracing::info!(%listen_addr, %internal_addr, video = state.video.is_some(), languages = ?locale::languages(), "TeamTAK is listening");
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
    println!("TeamTAK:    {}", monitor::release::INSTALLED);
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
    // The agent is not a backend: the platform works without it, so this line never fails
    // the check. It says what the dashboard would say.
    match config.monitor.agent_url.as_deref() {
        None => println!("host agent: not configured (the server and the services are not monitored)"),
        Some(url) => match monitor::agent::AgentClient::new(url, &config.monitor.agent_token) {
            Ok(agent) => match agent.report().await {
                Ok(report) => println!("host agent: ok ({} services of this stack, {} backup archives)", report.containers.containers.len(), report.backups.files.len()),
                Err(e) => println!("host agent: NOT ANSWERING ({e}); monitoring, backups and restarts from the web UI need it"),
            },
            Err(e) => println!("host agent: NOT ANSWERING ({e})"),
        },
    }
    // What the monitoring last heard from GitHub; never a failure of the check either.
    let check = monitor::release::load(&db).await.unwrap_or_default();
    let (release, _) = monitor::release::status(config.monitor.release_repository.as_deref(), &check);
    let latest = release.latest.as_ref();
    match release.status {
        "update_available" => println!(
            "releases:   TeamTAK {} is published ({}); to update, see \"Updating TeamTAK\" in the README",
            latest.map_or("", |l| l.version.as_str()),
            latest.map_or("", |l| l.url.as_str())
        ),
        "up_to_date" => println!("releases:   up to date (latest on GitHub: {})", latest.map_or("", |l| l.version.as_str())),
        "no_release" => println!("releases:   none published yet on github.com/{}", release.repository.unwrap_or_default()),
        "off" => println!("releases:   not looked for (RELEASE_REPOSITORY=off)"),
        _ => println!("releases:   GitHub has not answered yet{}", release.error.map(|e| format!(" ({e})")).unwrap_or_default()),
    }
    if failed {
        bail!("at least one backend cannot be reached");
    }
    Ok(())
}

async fn send_test_mail(config: Config, to: &str, language: &str) -> Result<()> {
    if !locale::is_supported(language) {
        bail!("unknown language {language:?}; the platform has: {}", locale::languages().join(", "));
    }
    let smtp = &config.smtp;
    if smtp.host.is_empty() {
        bail!("SMTP_HOST is empty in deploy/.env: no e-mail can be sent");
    }
    let login = if smtp.username.is_empty() { "without authentication".to_string() } else { format!("as {}", smtp.username) };
    println!("Mail server: {}:{} ({}), {login}", smtp.host, smtp.port, smtp.tls.name());
    println!("Sender:      {}", smtp.from);
    // Counted like any e-mail when the database answers, so the monitoring page sees it.
    let db = match state::connect_database(&config).await {
        Ok(db) => Some(db),
        Err(e) => {
            println!("Database:    not reachable ({e:#}): the attempt is not counted");
            None
        }
    };
    let mailer = mail::Mailer::new(smtp, &config.public_url, db)?;
    mailer.send_test(to, language).await.context("the test e-mail was not sent")?;
    println!("The mail server accepted the test e-mail for {to}. Check that it arrives, spam folder included.");
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
        Command::SendTestMail { email, language } => send_test_mail(config, &email, &language).await,
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
            for warning in &report.warnings {
                println!("WARNING {warning}");
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
