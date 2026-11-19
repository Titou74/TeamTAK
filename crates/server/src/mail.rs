//! Outgoing e-mail over SMTP (Brevo in production, a mail catcher locally).

use anyhow::{Context, Result};
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{SmtpConfig, SmtpTls};
use crate::locale::{self, Template};

pub struct Mailer {
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    from: Mailbox,
    public_url: String,
}

impl Mailer {
    pub fn new(config: &SmtpConfig, public_url: &str) -> Result<Self> {
        let from: Mailbox = config.from.parse().with_context(|| format!("MAIL_FROM={:?} is not an address", config.from))?;
        let transport = if config.host.is_empty() {
            tracing::warn!("SMTP_HOST is empty: no e-mail will be sent");
            None
        } else {
            let builder = match config.tls {
                SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?,
                SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?,
                SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
            };
            let builder = builder.port(config.port).timeout(Some(std::time::Duration::from_secs(15)));
            let builder = if config.username.is_empty() {
                builder
            } else {
                builder.credentials(Credentials::new(config.username.clone(), config.password.clone()))
            };
            Some(builder.build())
        };
        Ok(Self { transport, from, public_url: public_url.to_string() })
    }

    /// Sends `template` to `to`, written in `language` (English when unknown).
    pub async fn send(&self, to: &str, language: &str, template: Template, vars: &[(&str, &str)]) -> Result<()> {
        let transport = self.transport.as_ref().context("no SMTP server configured")?;
        let rendered = locale::render(template, language, vars, &self.public_url);
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse().with_context(|| format!("recipient {to:?}"))?)
            .subject(rendered.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(rendered.body)?;
        transport.send(message).await.context("SMTP delivery")?;
        Ok(())
    }

    /// Like [`send`], for notifications whose failure must not fail the request: logged only.
    pub async fn send_best_effort(&self, to: &str, language: &str, template: Template, vars: &[(&str, &str)]) {
        if let Err(e) = self.send(to, language, template, vars).await {
            tracing::warn!(to, template = template.key(), error = %format!("{e:#}"), "e-mail not sent");
        }
    }
}
