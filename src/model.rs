use lettre::Address;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
}

impl Email {
    pub fn validate(&self, domains: &[String]) -> Result<(), &'static str> {
        if self.to.is_empty() || self.to.len() > 10 {
            return Err("to must contain 1..10 email addresses");
        }
        for recipient in &self.to {
            if recipient.len() > 254 || recipient.chars().any(char::is_control) {
                return Err("invalid recipient email address");
            }
            // Address accepts a bare address, not arbitrary mailbox header syntax.
            let address: Address = recipient
                .parse()
                .map_err(|_| "invalid recipient email address")?;
            if !domains.is_empty()
                && !domains
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(address.domain()))
            {
                return Err("recipient domain is not allowed");
            }
        }
        if self.subject.trim().is_empty()
            || self.subject.len() > 998
            || self.subject.chars().any(char::is_control)
        {
            return Err("subject must be 1..998 bytes without control characters");
        }
        if self.text.trim().is_empty() || self.text.len() > 48 * 1024 || self.text.contains('\0') {
            return Err("text must be 1..49152 bytes without NUL characters");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, sqlx::FromRow)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub error_code: Option<String>,
}
