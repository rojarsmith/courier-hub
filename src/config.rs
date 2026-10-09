use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use lettre::message::Mailbox;

// Deliberately no Debug implementation: configuration contains credentials.
pub struct Config {
    pub bind: SocketAddr,
    pub api_key: String,
    pub data_dir: PathBuf,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: TlsMode,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_from: Mailbox,
    pub smtp_timeout: Duration,
    pub workers: usize,
    pub max_pending: i64,
    pub requests_per_minute: u32,
    pub retention: Duration,
    pub recipient_domains: Vec<String>,
}

#[derive(Clone, Copy)]
pub enum TlsMode {
    StartTls,
    Implicit,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::load(|name| env::var(name).ok())
    }

    // An injected reader makes validation testable without changing process-wide env vars.
    pub fn load(read: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let required = |name: &str| -> Result<String> {
            let value = read(name).with_context(|| format!("missing {name}"))?;
            ensure!(!value.trim().is_empty(), "{name} must not be empty");
            Ok(value)
        };
        let number = |name: &str, default: u64, min: u64, max: u64| -> Result<u64> {
            let value = match read(name) {
                Some(s) => s
                    .parse::<u64>()
                    .with_context(|| format!("invalid {name}"))?,
                None => default,
            };
            ensure!(
                (min..=max).contains(&value),
                "{name} is outside the allowed range"
            );
            Ok(value)
        };
        let api_key = required("API_KEY")?;
        ensure!(
            api_key.len() >= 32
                && api_key.len() <= 256
                && api_key.bytes().all(|b| b.is_ascii_graphic())
                && !api_key.starts_with("REPLACE_"),
            "API_KEY must be 32..256 printable ASCII characters; generate a random key"
        );
        let smtp_tls = match read("SMTP_TLS").as_deref().unwrap_or("starttls") {
            "starttls" => TlsMode::StartTls,
            "implicit" => TlsMode::Implicit,
            _ => bail!("SMTP_TLS must be starttls or implicit; plaintext is disabled"),
        };
        let smtp_host = required("SMTP_HOST")?;
        ensure!(
            smtp_host.len() <= 253
                && smtp_host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
            "SMTP_HOST must be a DNS hostname"
        );
        let sender = required("SMTP_FROM")?;
        ensure!(!sender.chars().any(char::is_control), "invalid SMTP_FROM");
        let smtp_from = sender
            .parse::<Mailbox>()
            .map_err(|_| anyhow::anyhow!("invalid SMTP_FROM"))?;
        let recipient_domains: Vec<String> = read("ALLOWED_RECIPIENT_DOMAINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        for domain in &recipient_domains {
            ensure!(
                domain.len() <= 253
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
                    && domain
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                "invalid ALLOWED_RECIPIENT_DOMAINS"
            );
        }
        let data_dir = PathBuf::from(read("DATA_DIR").unwrap_or_else(|| "data".into()));
        ensure!(
            !data_dir.as_os_str().is_empty(),
            "DATA_DIR must not be empty"
        );
        Ok(Self {
            bind: read("BIND_ADDR")
                .unwrap_or_else(|| "127.0.0.1:8080".into())
                .parse()
                .context("invalid BIND_ADDR")?,
            api_key,
            data_dir,
            smtp_host,
            smtp_port: number(
                "SMTP_PORT",
                match smtp_tls {
                    TlsMode::StartTls => 587,
                    TlsMode::Implicit => 465,
                },
                1,
                65535,
            )? as u16,
            smtp_tls,
            smtp_username: required("SMTP_USERNAME")?,
            smtp_password: required("SMTP_PASSWORD")?,
            smtp_from,
            smtp_timeout: Duration::from_secs(number("SMTP_TIMEOUT_SECONDS", 30, 1, 120)?),
            workers: number("WORKER_CONCURRENCY", 2, 1, 16)? as usize,
            max_pending: number("MAX_PENDING_JOBS", 1000, 1, 100_000)? as i64,
            requests_per_minute: number("REQUESTS_PER_MINUTE", 120, 1, 100_000)? as u32,
            retention: Duration::from_secs(number("JOB_RETENTION_HOURS", 24, 1, 720)? * 3600),
            recipient_domains,
        })
    }
}
