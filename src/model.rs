use std::collections::HashSet;

use base64::{Engine, engine::general_purpose::STANDARD};
use lettre::{Address, message::header::ContentType};
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 10;
const MAX_ENCODED_ATTACHMENT_BYTES: usize = MAX_ATTACHMENT_BYTES.div_ceil(3) * 4;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<EmailAttachment>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmailAttachment {
    pub filename: String,
    pub content_type: String,
    pub content_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl EmailAttachment {
    fn validate(&self) -> Result<(), &'static str> {
        if self.filename.trim().is_empty()
            || self.filename.len() > 255
            || matches!(self.filename.as_str(), "." | "..")
            || self
                .filename
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err("filename must be 1..255 bytes without controls or path separators");
        }
        // Accept a media type, not arbitrary header parameters or raw headers.
        let valid_type = self
            .content_type
            .split_once('/')
            .is_some_and(|(kind, subtype)| {
                !kind.eq_ignore_ascii_case("multipart")
                    && !kind.eq_ignore_ascii_case("message")
                    && [kind, subtype].into_iter().all(|token| {
                        !token.is_empty()
                            && token != "*"
                            && token.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
                            })
                    })
            });
        if self.content_type.len() > 127
            || !valid_type
            || ContentType::parse(&self.content_type).is_err()
        {
            return Err(
                "content_type must be a file media type without parameters, wildcards, or MIME containers",
            );
        }
        if let Some(id) = &self.content_id {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"@._-".contains(&b))
            {
                return Err("content_id must contain 1..128 ASCII letters, digits, or @._-");
            }
            if !["image/png", "image/jpeg", "image/gif", "image/webp"]
                .iter()
                .any(|kind| self.content_type.eq_ignore_ascii_case(kind))
            {
                return Err(
                    "inline images must use image/png, image/jpeg, image/gif, or image/webp",
                );
            }
        }
        Ok(())
    }

    pub(crate) fn decoded_content(&self) -> Result<Vec<u8>, &'static str> {
        // Bound allocation before decoding, including when called by the worker.
        if self.content_base64.len() > MAX_ENCODED_ATTACHMENT_BYTES {
            return Err("decoded attachments must total at most 5 MiB");
        }
        let bytes = STANDARD.decode(&self.content_base64).map_err(
            |_| "content_base64 must be standard Base64 with required padding and no whitespace",
        )?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err("decoded attachments must total at most 5 MiB");
        }
        Ok(bytes)
    }
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
        if let Some(html) = &self.html {
            if html.trim().is_empty() || html.len() > 48 * 1024 || html.contains('\0') {
                return Err("html must be 1..49152 bytes without NUL characters");
            }
        }
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err("at most 10 attachments including inline images are allowed");
        }
        let mut total_bytes = 0;
        let mut content_ids = HashSet::new();
        for attachment in &self.attachments {
            attachment.validate()?;
            if let Some(id) = &attachment.content_id {
                if self.html.is_none() {
                    return Err("inline images require html");
                }
                if !content_ids.insert(id) {
                    return Err("content_id must be unique within the email");
                }
            }
            total_bytes += attachment.decoded_content()?.len();
            if total_bytes > MAX_ATTACHMENT_BYTES {
                return Err("decoded attachments must total at most 5 MiB");
            }
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
