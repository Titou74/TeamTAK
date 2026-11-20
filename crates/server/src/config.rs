//! Settings, all read from the environment (deploy/.env through Docker Compose).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use teamtak_core::challenge;
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

impl SmtpTls {
    /// The value of `SMTP_TLS` that selects it.
    pub fn name(self) -> &'static str {
        match self {
            SmtpTls::StartTls => "starttls",
            SmtpTls::Tls => "tls",
            SmtpTls::None => "none",
        }
    }
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
    /// CN of the certificate Caddy presents to TAK Server for the administrators' pages;
    /// never touched by the reconciliation either.
    pub admin_ui_cert_name: String,
    /// That certificate without its key, for the monitoring of its expiry.
    pub admin_ui_cert_file: Option<PathBuf>,
    /// Host name or address written in zips and QR codes.
    pub public_host: String,
    /// Ports of the machine that lead to the TLS input for TAK clients, never empty. The
    /// first is the one written in zips and QR codes; the others are alternatives for
    /// networks that block it, chosen per device.
    pub client_ports: Vec<u16>,
    /// Name of the connection in the phones' server list.
    pub connection_name: String,
    pub p12_format: P12Format,
}

impl TakConfig {
    /// The port written in zips and QR codes unless another one is chosen.
    pub fn client_port(&self) -> u16 {
        self.client_ports[0]
    }

    /// Name of the connection in the phones' server list for `port`. An alternative port is
    /// added to it, so that a device holding the connections of two ports tells them apart.
    pub fn connection_name_on(&self, port: u16) -> String {
        if port == self.client_port() {
            self.connection_name.clone()
        } else {
            format!("{} ({port})", self.connection_name)
        }
    }
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
    /// Address of TAK Server's own administration pages, served by Caddy to the platform's
    /// administrators (`TAK_ADMIN_PORT`); `None` when that port is turned off.
    pub tak_admin_url: Option<String>,
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
    /// GitHub repository (`owner/name`) whose latest release is compared with the version
    /// that runs; `None` when that check is turned off.
    pub release_repository: Option<String>,
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

/// The GitHub repository TeamTAK is released from, as Cargo.toml names it.
pub const DEFAULT_RELEASE_REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The `owner/name` of a GitHub repository given as `owner/name` or as its address;
/// `None` for `off` or `none`, which turn the release check off.
pub fn parse_release_repository(raw: &str) -> Result<Option<String>> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("off") || raw.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let path = raw.strip_prefix("https://github.com/").unwrap_or(raw).trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let valid = |part: &str| !part.is_empty() && part != "." && part != ".." && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    match path.split_once('/') {
        Some((owner, name)) if valid(owner) && valid(name) => Ok(Some(format!("{owner}/{name}"))),
        _ => bail!("expected owner/name of a GitHub repository, its https://github.com/ address, or off"),
    }
}

/// Address of TAK Server's administration pages: the host of the web UI, on the port of
/// `TAK_ADMIN_PORT`, with the web UI's scheme (Caddy serves both). `None` for `off`.
pub fn tak_admin_url(public_url: &str, raw_port: &str) -> Result<Option<String>> {
    let raw_port = raw_port.trim();
    if raw_port.is_empty() || raw_port.eq_ignore_ascii_case("off") || raw_port.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let port: u16 = raw_port.parse().ok().filter(|p| *p != 0).ok_or_else(|| anyhow!("expected a port number or off"))?;
    let (scheme, rest) = public_url.split_once("://").ok_or_else(|| anyhow!("PUBLIC_URL {public_url:?} has no scheme"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Without its port; an IPv6 address keeps its brackets.
    let host = match authority.strip_prefix('[') {
        Some(v6) => format!("[{}]", v6.split(']').next().unwrap_or_default()),
        None => authority.split(':').next().unwrap_or_default().to_string(),
    };
    if host.is_empty() || host == "[]" {
        bail!("PUBLIC_URL {public_url:?} has no host");
    }
    Ok(Some(format!("{scheme}://{host}:{port}")))
}

/// The ports of a list like `8089,4443` (commas or spaces): at least one, none twice, in
/// the order given.
pub fn parse_ports(raw: &str) -> Result<Vec<u16>> {
    let mut ports = Vec::new();
    for item in raw.split([',', ' ', '\t']).filter(|s| !s.is_empty()) {
        let port: u16 = item.parse().ok().filter(|p| *p != 0).ok_or_else(|| anyhow!("{item:?} is not a port number"))?;
        if ports.contains(&port) {
            bail!("port {port} is listed twice");
        }
        ports.push(port);
    }
    if ports.is_empty() {
        bail!("no port");
    }
    Ok(ports)
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
        let ports_raw = var_or("TAK_CLIENT_PORTS", "8089");
        let client_ports = parse_ports(&ports_raw).with_context(|| format!("TAK_CLIENT_PORTS={ports_raw:?}"))?;
        let repository_raw = var_or("RELEASE_REPOSITORY", DEFAULT_RELEASE_REPOSITORY);
        let release_repository = parse_release_repository(&repository_raw).with_context(|| format!("RELEASE_REPOSITORY={repository_raw:?}"))?;
        let public_url = var("PUBLIC_URL")?.trim_end_matches('/').to_string();
        let admin_port_raw = var_or("TAK_ADMIN_PORT", "off");
        let tak_admin_url = tak_admin_url(&public_url, &admin_port_raw).with_context(|| format!("TAK_ADMIN_PORT={admin_port_raw:?}"))?;

        Ok(Self {
            listen_addr: parsed("LISTEN_ADDR", "0.0.0.0:8080")?,
            web_dir,
            database_url: var("DATABASE_URL")?,
            app_secret,
            public_url,
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
                admin_ui_cert_name: var_or("TAK_ADMIN_UI_CERT_NAME", "teamtak-admin-ui"),
                admin_ui_cert_file: Some(var_or("TAK_ADMIN_UI_CERT_FILE", "")).filter(|f| !f.is_empty()).map(PathBuf::from),
                public_host: var("TAK_PUBLIC_HOST")?,
                client_ports,
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
                resend_cooldown_secs: parsed::<i64>("LOGIN_RESEND_COOLDOWN_SECONDS", &defaults.resend_cooldown_secs.to_string())?
                    .clamp(0, 3600),
                challenge_max_number: parsed::<u64>("LOGIN_CHALLENGE_DIFFICULTY", &defaults.challenge_max_number.to_string())?
                    .clamp(*challenge::MAX_NUMBER_RANGE.start(), *challenge::MAX_NUMBER_RANGE.end()),
            },
            reconcile_interval: Duration::from_secs(parsed("RECONCILE_INTERVAL_SECONDS", "60")?),
            session_ttl_days: parsed("SESSION_TTL_DAYS", "30")?,
            monitor: MonitorConfig {
                agent_url: Some(var_or("AGENT_URL", "").trim_end_matches('/').to_string()).filter(|u| !u.is_empty()),
                agent_token: var_or("AGENT_TOKEN", ""),
                interval: Duration::from_secs(parsed::<u64>("MONITOR_INTERVAL_SECONDS", "60")?.clamp(5, 3600)),
                web_tls_probe: Some(var_or("WEB_TLS_PROBE", "")).filter(|p| !p.is_empty()),
                release_repository,
            },
            tak_admin_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_ports, parse_release_repository, tak_admin_url, TakConfig, DEFAULT_RELEASE_REPOSITORY};

    #[test]
    fn release_repository() {
        let repo = |raw: &str| parse_release_repository(raw).unwrap();
        assert_eq!(repo(DEFAULT_RELEASE_REPOSITORY).as_deref(), Some("Titou74/TeamTAK"), "Cargo.toml's repository");
        assert_eq!(repo("someone/teamtak-fork").as_deref(), Some("someone/teamtak-fork"));
        assert_eq!(repo(" https://github.com/someone/fork.git/ ").as_deref(), Some("someone/fork"));
        assert_eq!(repo("off"), None);
        assert_eq!(repo("NONE"), None);
        for bad in ["", "teamtak", "a/b/c", "https://gitlab.com/a/b", "a/../b", "../b", "a b/c", "a/b?x=1"] {
            assert!(parse_release_repository(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn client_ports() {
        assert_eq!(parse_ports("8089").unwrap(), [8089]);
        assert_eq!(parse_ports("8089,4443, 8090").unwrap(), [8089, 4443, 8090]);
        assert_eq!(parse_ports(" 8090 8089 ").unwrap(), [8090, 8089], "order kept: the first is the main one");
        for bad in ["", " , ", "8089,8089", "0", "65536", "80a", "-1", "8089;8090"] {
            assert!(parse_ports(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn connection_name_per_port() {
        let tak = TakConfig {
            api_url: String::new(),
            enroll_url: String::new(),
            ca_file: Default::default(),
            api_cert_file: Default::default(),
            api_key_file: Default::default(),
            api_cert_name: String::new(),
            admin_ui_cert_name: String::new(),
            admin_ui_cert_file: None,
            public_host: "tak.example.org".into(),
            client_ports: vec![8089, 4443],
            connection_name: "TeamTAK".into(),
            p12_format: Default::default(),
        };
        assert_eq!(tak.connection_name_on(8089), "TeamTAK");
        assert_eq!(tak.connection_name_on(4443), "TeamTAK (4443)");
    }

    #[test]
    fn tak_admin_address() {
        let url = |public: &str, port: &str| tak_admin_url(public, port).unwrap();
        assert_eq!(url("https://tak.example.org", "8444").as_deref(), Some("https://tak.example.org:8444"));
        assert_eq!(url("http://localhost:8480", "8444").as_deref(), Some("http://localhost:8444"), "the web UI's port is replaced");
        assert_eq!(url("http://192.168.1.20:8480/", " 9443 ").as_deref(), Some("http://192.168.1.20:9443"));
        assert_eq!(url("http://[::1]:8480", "8444").as_deref(), Some("http://[::1]:8444"));
        for off in ["off", "OFF", "none", ""] {
            assert_eq!(url("https://tak.example.org", off), None, "{off:?}");
        }
        for bad in ["0", "65536", "84a3", "-1"] {
            assert!(tak_admin_url("https://tak.example.org", bad).is_err(), "{bad:?} accepted");
        }
        assert!(tak_admin_url("tak.example.org", "8444").is_err(), "no scheme");
    }
}
