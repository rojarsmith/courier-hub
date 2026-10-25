use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{
        Attachment, Body, Mailbox, MultiPart, SinglePart,
        header::{ContentTransferEncoding, ContentType, MIME_VERSION_1_0},
    },
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
        // Recheck persisted jobs before constructing headers or allocating file bodies.
        email
            .validate(&[])
            .map_err(|_| DeliveryError::InvalidMessage)?;
        let mut builder = Message::builder()
            .from(self.from.clone())
            .subject(&email.subject)
            .message_id(Some(format!("<{job_id}@{}>", self.from.email.domain())))
            .header(MIME_VERSION_1_0);
        for to in &email.to {
            builder = builder.to(Mailbox::new(
                None,
                to.parse().map_err(|_| DeliveryError::InvalidMessage)?,
            ));
        }
        if email.html.is_none() && email.attachments.is_empty() {
            return builder
                .header(ContentType::TEXT_PLAIN)
                .body(email.text.clone())
                .map_err(|_| DeliveryError::InvalidMessage);
        }

        let mut regular = Vec::new();
        let mut inline = Vec::new();
        for file in &email.attachments {
            let bytes = file
                .decoded_content()
                .map_err(|_| DeliveryError::InvalidMessage)?;
            let content_type = ContentType::parse(&file.content_type)
                .map_err(|_| DeliveryError::InvalidMessage)?;
            let attachment = match &file.content_id {
                Some(id) => Attachment::new_inline_with_name(id.clone(), file.filename.clone()),
                None => Attachment::new(file.filename.clone()),
            };
            let bytes = Body::new_with_encoding(bytes, ContentTransferEncoding::Base64)
                .map_err(|_| DeliveryError::InvalidMessage)?;
            let part = attachment.body(bytes, content_type);
            if file.content_id.is_some() {
                inline.push(part);
            } else {
                regular.push(part);
            }
        }

        let plain = SinglePart::plain(email.text.clone());
        let mut body = match &email.html {
            Some(html) => {
                let alternative = MultiPart::alternative().singlepart(plain);
                let html = SinglePart::html(html.clone());
                if inline.is_empty() {
                    alternative.singlepart(html)
                } else {
                    let mut related = MultiPart::related().singlepart(html);
                    for part in inline {
                        related = related.singlepart(part);
                    }
                    alternative.multipart(related)
                }
            }
            None => MultiPart::mixed().singlepart(plain),
        };
        if email.html.is_some() && !regular.is_empty() {
            body = MultiPart::mixed().multipart(body);
        }
        for part in regular {
            body = body.singlepart(part);
        }
        builder
            .multipart(body)
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
    use crate::model::EmailAttachment;
    use base64::{Engine, engine::general_purpose::STANDARD};
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

    // Inspect SMTP wire boundaries independently of the MIME builder.
    fn mime_children(wire: &str) -> Vec<&str> {
        let (headers, body) = wire.split_once("\r\n\r\n").unwrap();
        let boundary = headers
            .split_once("boundary=\"")
            .unwrap()
            .1
            .split('"')
            .next()
            .unwrap();
        let marker = format!("--{boundary}");
        assert!(body.contains(&format!("{marker}--")));
        body.split(&marker)
            .skip(1)
            .take_while(|part| !part.starts_with("--"))
            .map(|part| part.trim_start_matches("\r\n").trim_end_matches("\r\n"))
            .collect()
    }

    #[tokio::test]
    async fn smtp_sends_html_attachments_and_related_inline_images() {
        let binary = b"\0\xff\r\nprivate-file-content\0";
        for (html, inline, regular) in [
            (false, false, true),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let tls = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der())
                        .into(),
                )
                .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(tls));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                session(acceptor.accept(stream).await.unwrap(), false).await
            });
            let params = TlsParameters::builder("localhost".into())
                .add_root_certificate(Certificate::from_der(cert.cert.der().to_vec()).unwrap())
                .build()
                .unwrap();
            let transport = SmtpTransport {
                mailer: AsyncSmtpTransport::<Tokio1Executor>::relay("localhost")
                    .unwrap()
                    .port(port)
                    .tls(Tls::Wrapper(params))
                    .credentials(Credentials::new("test-user".into(), "test-password".into()))
                    .timeout(Some(Duration::from_secs(2)))
                    .build(),
                from: "sender@example.com".parse().unwrap(),
            };
            let mut email = Email {
                to: vec!["recipient@example.com".into()],
                subject: "MIME integration".into(),
                text: "Plain text fallback".into(),
                html: html.then(|| "<p>Hello</p><img src=\"cid:logo@example\">".into()),
                attachments: vec![],
            };
            if inline {
                email.attachments.push(EmailAttachment {
                    filename: "logo.png".into(),
                    content_type: "image/png".into(),
                    content_base64: STANDARD.encode(binary),
                    content_id: Some("logo@example".into()),
                });
            }
            if regular {
                email.attachments.push(EmailAttachment {
                    filename: "report.bin".into(),
                    content_type: "application/octet-stream".into(),
                    content_base64: STANDARD.encode(binary),
                    content_id: None,
                });
            }
            assert!(transport.deliver("mime-job", &email).await.is_ok());
            transport.mailer.shutdown().await;
            let wire = tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            let (headers, _) = wire.split_once("\r\n\r\n").unwrap();
            assert!(headers.contains(if regular {
                "multipart/mixed"
            } else {
                "multipart/alternative"
            }));
            assert_eq!(
                headers
                    .lines()
                    .filter(|line| line.starts_with("MIME-Version:"))
                    .count(),
                1
            );
            assert!(wire.contains("Content-Type: text/plain; charset=utf-8"));
            assert!(wire.contains("Plain text fallback"));
            let root = mime_children(&wire);
            assert_eq!(root.len(), 2);
            let alternatives = if regular && html {
                assert!(root[0].starts_with("Content-Type: multipart/alternative"));
                assert!(root[1].contains("filename=\"report.bin\""));
                mime_children(root[0])
            } else {
                root
            };
            assert!(alternatives[0].starts_with("Content-Type: text/plain"));
            if inline {
                assert!(alternatives[1].starts_with("Content-Type: multipart/related"));
                let related = mime_children(alternatives[1]);
                assert_eq!(related.len(), 2);
                assert!(related[0].starts_with("Content-Type: text/html"));
                assert!(related[0].contains("cid:logo@example"));
                assert!(related[1].contains("Content-ID: <logo@example>"));
            } else if html {
                assert!(alternatives[1].starts_with("Content-Type: text/html"));
            }
            assert_eq!(wire.contains("multipart/alternative"), html);
            assert_eq!(
                wire.contains("Content-Type: text/html; charset=utf-8"),
                html
            );
            assert_eq!(wire.contains("multipart/related"), inline);
            assert_eq!(wire.contains("Content-ID: <logo@example>"), inline);
            assert_eq!(
                wire.contains("Content-Disposition: attachment; filename=\"report.bin\""),
                regular
            );
            if inline {
                assert!(wire.contains("Content-Disposition: inline; filename=\"logo.png\""));
                assert!(
                    wire.find("multipart/related").unwrap() < wire.find("Content-ID:").unwrap()
                );
                if regular {
                    assert!(
                        wire.find("Content-ID:").unwrap()
                            < wire.find("Content-Disposition: attachment").unwrap()
                    );
                }
            }
            // Decode each wire part independently: raw binary must survive SMTP unchanged.
            let mut count = 0;
            for (start, _) in wire.match_indices("Content-Transfer-Encoding: base64\r\n") {
                let body = wire[start..].split_once("\r\n\r\n").unwrap().1;
                let encoded = body.split_once("\r\n--").unwrap().0.replace("\r\n", "");
                assert_eq!(STANDARD.decode(encoded).unwrap(), binary);
                count += 1;
            }
            assert_eq!(count, usize::from(inline) + usize::from(regular));
            assert!(!wire.contains("test-password"));
        }
    }

    #[tokio::test]
    async fn smtp_over_tls_sends_message_and_classifies_rejection() {
        for reject in [false, true] {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der())
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
                html: None,
                attachments: vec![],
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
                let (headers, _) = message.split_once("\r\n\r\n").unwrap();
                assert_eq!(
                    headers
                        .lines()
                        .filter(|line| line.starts_with("MIME-Version:"))
                        .collect::<Vec<_>>(),
                    vec!["MIME-Version: 1.0"]
                );
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
            html: None,
            attachments: vec![],
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
