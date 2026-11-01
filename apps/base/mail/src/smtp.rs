//! SMTP relay with verified TLS.
//! The configuration has no Debug implementation: the password must not log.
use crate::{Envelope, Error, SendFuture, Transport};
use lettre::{
    transport::smtp::authentication::Credentials, AsyncSmtpTransport, AsyncTransport,
    Tokio1Executor,
};

#[derive(Clone, Copy)]
pub enum Tls {
    /// Connect in clear, then require the STARTTLS upgrade.
    StartTls,
    /// TLS from the first byte (SMTPS).
    Implicit,
}
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub tls: Tls,
}
pub struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    host: String,
    port: u16,
}
impl Smtp {
    pub fn new(config: SmtpConfig) -> crate::Result<Self> {
        let builder = match config.tls {
            Tls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host),
            Tls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host),
        }
        .map_err(|error| Error::Config(format!("MAIL_HOST is not usable: {error}")))?;
        let transport = builder
            .port(config.port)
            .credentials(Credentials::new(config.username, config.password))
            .timeout(Some(std::time::Duration::from_secs(20)))
            .build();
        Ok(Self {
            transport,
            host: config.host,
            port: config.port,
        })
    }
}
impl Transport for Smtp {
    fn send_raw<'a>(&'a self, envelope: &'a Envelope, bytes: &'a [u8]) -> SendFuture<'a> {
        Box::pin(async move {
            // lettre 0.11 appends CRLF before the DATA terminator. Supply all
            // but the final CRLF so the received message equals our wire bytes.
            let data = bytes.strip_suffix(b"\r\n").ok_or(Error::Invalid)?;
            self.transport
                .send_raw(&envelope.smtp()?, data)
                .await
                .map(|_| ())
                .map_err(|error| Error::Delivery(error.to_string()))
        })
    }
    fn describe(&self) -> String {
        format!("SMTP relay {}:{}", self.host, self.port)
    }
}
