//! Settings, all read from the environment (deploy/.env through Docker Compose).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use teamtak_core::directory::ldap::LdapConfig;
use teamtak_core::login::LoginPolicy;
use teamtak_core::package::P12Format;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// STARTTLS on a plain connection (port 587, Brevo).
    StartTls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption: the local mail catcher.
    None,
}

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub tls: SmtpTls,
    pub from: String,
}

#[derive(Debug, Clone)]
pub struct TakConfig {
    pub api_url: String,
    pub enroll_url: String,
    pub ca_file: PathBuf,
    pub api_cert_file: PathBuf,
    pub api_key_file: PathBuf,
    /// CN of the platform's API certificate; never touched by the reconciliation.
    pub api_cert_name: String,
    /// Host name or address written in zips and QR codes.
    pub public_host: String,
    /// Port of the TLS input for TAK clients.
    pub client_port: u16,
    /// Name of the connection in the phones' server list.
    pub connection_name: String,
    pub p12_format: P12Format,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    /// Directory of the built web UI; none when the API runs alone.
    pub web_dir: Option<PathBuf>,
    pub database_url: String,
    pub app_secret: String,
    /// Address of the web UI, used in e-mails.
    pub public_url: String,
    pub cookie_secure: bool,
    pub smtp: SmtpConfig,
    pub ldap: LdapConfig,
    pub tak: TakConfig,
    pub default_member_cap: i32,
    pub login: LoginPolicy,
    pub reconcile_interval: Duration,
    pub session_ttl_days: i64,
    pub monitor: MonitorConfig,
}

/// Where the monitoring looks.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// Address of the host agent; `None` when the platform runs without one.
    pub agent_url: Option<String>,
    pub agent_token: String,
    /// How often everything is looked at and the alerts are evaluated.
    pub interval: Duration,
    /// `host:port` where the reverse proxy answers in TLS inside the network, to read the
    /// certificate it presents for the web UI. Only used when the UI is served over HTTPS.
    pub web_tls_probe: Option<String>,
}

fn var(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| anyhow!("environment variable {name} is not set"))
}

fn var_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

fn parsed<T: std::str::FromStr>(name: &str, default: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    let raw = var_or(name, default);
    raw.parse().map_err(|e| anyhow!("{name}={raw:?} is not valid: {e}"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let app_secret = var("APP_SECRET")?;
        if app_secret.len() < 32 {
            bail!("APP_SECRET must be at least 32 characters");
        }
        let smtp_tls = match var_or("SMTP_TLS", "starttls").as_str() {
            "starttls" => SmtpTls::StartTls,
            "tls" => SmtpTls::Tls,
            "none" => SmtpTls::None,
            other => bail!("SMTP_TLS={other:?}: expected starttls, tls or none"),
        };
        let p12_raw = var_or("P12_FORMAT", "modern");
        let p12_format =
            P12Format::parse(&p12_raw).with_context(|| format!("P12_FORMAT={p12_raw:?}: expected modern or legacy"))?;
        let defaults = LoginPolicy::default();
        let web_dir = std::env::var("WEB_DIR").ok().filter(|s| !s.is_empty()).map(PathBuf::from);

        Ok(Self {
            listen_addr: parsed("LISTEN_ADDR", "0.0.0.0:8080")?,
            web_dir,
            database_url: var("DATABASE_URL")?,
            app_secret,
            public_url: var("PUBLIC_URL")?.trim_end_matches('/').to_string(),
            cookie_secure: parsed("COOKIE_SECURE", "true")?,
            smtp: SmtpConfig {
                host: var_or("SMTP_HOST", ""),
                port: parsed("SMTP_PORT", "587")?,
                username: var_or("SMTP_USERNAME", ""),
                password: var_or("SMTP_PASSWORD", ""),
                tls: smtp_tls,
                from: var_or("MAIL_FROM", "TeamTAK <teamtak@localhost>"),
            },
            ldap: LdapConfig {
                url: var("LDAP_URL")?,
                base_dn: var("LDAP_BASE_DN")?,
                bind_dn: var("LDAP_BIND_DN")?,
                bind_password: var("LDAP_BIND_PASSWORD")?,
            },
            tak: TakConfig {
                api_url: var("TAK_API_URL")?.trim_end_matches('/').to_string(),
                enroll_url: var("TAK_ENROLL_URL")?.trim_end_matches('/').to_string(),
                ca_file: var("TAK_CA_FILE")?.into(),
                api_cert_file: var("TAK_API_CERT_FILE")?.into(),
                api_key_file: var("TAK_API_KEY_FILE")?.into(),
                api_cert_name: var_or("TAK_API_CERT_NAME", "teamtak-api"),
                public_host: var("TAK_PUBLIC_HOST")?,
                client_port: parsed("TAK_CLIENT_PORT", "8089")?,
                connection_name: var_or("TAK_CONNECTION_NAME", "TeamTAK"),
                p12_format,
            },
            default_member_cap: parsed("DEFAULT_MEMBER_CAP", "10")?,
            login: LoginPolicy {
                code_ttl_secs: parsed("LOGIN_CODE_TTL_SECONDS", &defaults.code_ttl_secs.to_string())?,
                max_attempts_per_code: parsed("LOGIN_MAX_ATTEMPTS_PER_CODE", &defaults.max_attempts_per_code.to_string())?,
                window_secs: parsed("LOGIN_WINDOW_SECONDS", &defaults.window_secs.to_string())?,
                max_codes_per_email: parsed("LOGIN_MAX_CODES_PER_EMAIL", &defaults.max_codes_per_email.to_string())?,
                max_codes_per_ip: parsed("LOGIN_MAX_CODES_PER_IP", &defaults.max_codes_per_ip.to_string())?,
                max_failures_per_ip: parsed("LOGIN_MAX_FAILURES_PER_IP", &defaults.max_failures_per_ip.to_string())?,
            },
            reconcile_interval: Duration::from_secs(parsed("RECONCILE_INTERVAL_SECONDS", "60")?),
            session_ttl_days: parsed("SESSION_TTL_DAYS", "30")?,
            monitor: MonitorConfig {
                agent_url: Some(var_or("AGENT_URL", "").trim_end_matches('/').to_string()).filter(|u| !u.is_empty()),
                agent_token: var_or("AGENT_TOKEN", ""),
                interval: Duration::from_secs(parsed::<u64>("MONITOR_INTERVAL_SECONDS", "60")?.clamp(5, 3600)),
                web_tls_probe: Some(var_or("WEB_TLS_PROBE", "")).filter(|p| !p.is_empty()),
            },
        })
    }
}
