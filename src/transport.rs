use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::authentication::Credentials,
};

use crate::{
    config::{Config, TlsMode},
    model::Email,
};

// The worker knows this interface, not SMTP details. New adapters can implement it.
#[async_trait]
pub trait DeliveryTransport: Send + Sync {
    async fn deliver(&self, job_id: &str, email: &Email) -> Result<(), DeliveryError>;
}

#[derive(Clone, Copy)]
pub enum DeliveryError {
    Rejected,
    Uncertain,
    InvalidMessage,
}

impl DeliveryError {
    pub fn status(self) -> &'static str {
        match self {
            Self::Uncertain => "unknown",
            _ => "failed",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Rejected => "smtp_rejected",
            Self::Uncertain => "delivery_uncertain",
            Self::InvalidMessage => "invalid_message",
        }
    }
}

pub struct SmtpTransport {
    mailer: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpTransport {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let builder = match config.smtp_tls {
            TlsMode::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
            }
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host),
        }
        .map_err(|_| anyhow::anyhow!("could not configure SMTP TLS"))?;
        let mailer = builder
            .port(config.smtp_port)
            .credentials(Credentials::new(
                config.smtp_username.clone(),
                config.smtp_password.clone(),
            ))
            .timeout(Some(config.smtp_timeout))
            .build();
        Ok(Self {
            mailer,
            from: config.smtp_from.clone(),
        })
    }

    fn message(&self, job_id: &str, email: &Email) -> Result<Message, DeliveryError> {
        let mut builder = Message::builder()
            .from(self.from.clone())
            .subject(&email.subject)
            .message_id(Some(format!("<{job_id}@{}>", self.from.email.domain())))
            .header(ContentType::TEXT_PLAIN);
        for to in &email.to {
            builder = builder.to(Mailbox::new(
                None,
                to.parse().map_err(|_| DeliveryError::InvalidMessage)?,
            ));
        }
        builder
            .body(email.text.clone())
            .map_err(|_| DeliveryError::InvalidMessage)
    }
}

#[async_trait]
impl DeliveryTransport for SmtpTransport {
    async fn deliver(&self, job_id: &str, email: &Email) -> Result<(), DeliveryError> {
        self.mailer
            .send(self.message(job_id, email)?)
            .await
            .map(|_| ())
            .map_err(|error| {
                // A network failure can happen AFTER acceptance. Never retry blindly.
                // Do not log provider error text: it may contain addresses or private data.
                if error.is_permanent() || error.is_transient() {
                    DeliveryError::Rejected
                } else {
                    DeliveryError::Uncertain
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
        net::TcpListener,
    };
    use tokio_rustls::{TlsAcceptor, rustls};

    async fn session(stream: impl AsyncRead + AsyncWrite + Unpin, reject: bool) -> String {
        let mut stream = BufReader::new(stream);
        stream
            .write_all(b"220 localhost ESMTP test\r\n")
            .await
            .unwrap();
        let mut message = String::new();
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let reply: &[u8] = if line.starts_with("EHLO") {
                b"250-localhost\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n"
            } else if line.starts_with("AUTH PLAIN") {
                b"235 2.7.0 authenticated\r\n"
            } else if line.starts_with("MAIL FROM") {
                b"250 sender accepted\r\n"
            } else if line.starts_with("RCPT TO") {
                if reject {
                    b"550 recipient rejected\r\n"
                } else {
                    b"250 recipient accepted\r\n"
                }
            } else if line.trim() == "DATA" {
                stream.write_all(b"354 send data\r\n").await.unwrap();
                loop {
                    line.clear();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == ".\r\n" {
                        break;
                    }
                    message.push_str(&line);
                }
                b"250 message accepted\r\n"
            } else if line.trim() == "QUIT" {
                stream.write_all(b"221 bye\r\n").await.unwrap();
                break;
            } else if line.trim() == "RSET" || line.trim() == "NOOP" {
                b"250 ok\r\n"
            } else {
                panic!("unexpected SMTP command");
            };
            stream.write_all(reply).await.unwrap();
        }
        message
    }

    #[tokio::test]
    async fn smtp_over_tls_sends_message_and_classifies_rejection() {
        for reject in [false, true] {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())
                        .into(),
                )
                .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                session(acceptor.accept(stream).await.unwrap(), reject).await
            });
            // Trust only the generated local test certificate. Production uses public roots.
            let params = TlsParameters::builder("localhost".into())
                .add_root_certificate(Certificate::from_der(cert.cert.der().to_vec()).unwrap())
                .build()
                .unwrap();
            let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay("localhost")
                .unwrap()
                .port(port)
                .tls(Tls::Wrapper(params))
                .credentials(Credentials::new("test-user".into(), "test-password".into()))
                .timeout(Some(Duration::from_secs(2)))
                .build();
            let transport = SmtpTransport {
                mailer,
                from: "sender@example.com".parse().unwrap(),
            };
            let email = Email {
                to: vec!["recipient@example.com".into()],
                subject: "SMTP integration".into(),
                text: "test body".into(),
            };
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                transport.deliver("test-job", &email),
            )
            .await
            .unwrap();
            if reject {
                assert!(matches!(result, Err(DeliveryError::Rejected)));
            } else {
                assert!(result.is_ok());
            }
            transport.mailer.shutdown().await;
            let message = tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            if !reject {
                assert!(message.contains("From: sender@example.com"));
                assert!(message.contains("To: recipient@example.com"));
                assert!(message.contains("Message-ID: <test-job@example.com>"));
                assert!(message.contains("test body"));
                assert!(!message.contains("test-password"));
            }
        }
    }

    #[tokio::test]
    async fn required_starttls_never_sends_credentials_to_plaintext_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert!(line.starts_with("EHLO"));
            stream
                .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
                .await
                .unwrap();
            let mut commands = String::new();
            loop {
                line.clear();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                commands.push_str(&line);
                if line.trim() == "QUIT" {
                    stream.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                }
            }
            commands
        });
        let config = Config::load(|name| match name {
            "API_KEY" => Some("test-api-key-123456789012345678901234".into()),
            "SMTP_HOST" => Some("localhost".into()),
            "SMTP_PORT" => Some(port.to_string()),
            "SMTP_USERNAME" => Some("test-user".into()),
            "SMTP_PASSWORD" => Some("test-password".into()),
            "SMTP_FROM" => Some("sender@example.com".into()),
            _ => None,
        })
        .unwrap();
        let transport = SmtpTransport::new(&config).unwrap();
        let email = Email {
            to: vec!["recipient@example.com".into()],
            subject: "Test".into(),
            text: "private-body-marker".into(),
        };
        assert!(transport.deliver("test-job", &email).await.is_err());
        transport.mailer.shutdown().await;
        let commands = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(!commands.contains("AUTH"));
        assert!(!commands.contains("MAIL FROM"));
        assert!(!commands.contains("private-body-marker"));
    }
}
