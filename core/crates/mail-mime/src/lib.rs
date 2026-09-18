//! MIME normalization at the boundary between `mail-parser` and the app model.

use ammonia::Builder;
use chrono::{FixedOffset, NaiveDate, TimeZone};
use mail_model::{Attachment, MessageContent, RemoteHeader};
use mail_parser::{
    Addr, HeaderForm, HeaderName, HeaderValue, MessageParser, MimeHeaders, PartType,
};
use std::collections::HashSet;
use thiserror::Error;

pub const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum MimeError {
    #[error("MIME message could not be parsed")]
    Parse,
    #[error("MIME part has invalid encoded content")]
    Encoding,
    #[error("decoded MIME content exceeds {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
}

#[must_use]
pub fn parse_header(uid: u32, bytes: &[u8], is_read: bool, is_starred: bool) -> RemoteHeader {
    let parsed = MessageParser::default().parse_headers(bytes);
    let (message_id, subject, sender, date) = parsed.as_ref().map_or_else(
        || (None, String::new(), String::new(), String::new()),
        |message| {
            (
                message.message_id().map(ToOwned::to_owned),
                message.subject().unwrap_or_default().to_owned(),
                message.from().map(format_sender).unwrap_or_default(),
                message.date().map(ToString::to_string).unwrap_or_default(),
            )
        },
    );
    RemoteHeader {
        uid,
        message_id,
        subject,
        sender,
        date,
        is_read,
        is_starred,
        ..Default::default()
    }
}

/// Parse a complete message and normalize its MIME parts.
///
/// # Errors
///
/// Returns an error when parsing, transfer decoding, or the bounded content budget fails.
pub fn parse_message(
    uid: u32,
    bytes: &[u8],
    is_read: bool,
    is_starred: bool,
) -> Result<RemoteHeader, MimeError> {
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or(MimeError::Parse)?;
    if message.parts.iter().any(|part| part.is_encoding_problem) {
        return Err(MimeError::Encoding);
    }
    let content = normalize_content(&message)?;
    let ids = |name: HeaderName<'static>| -> Vec<String> {
        message
            .header_as(name, HeaderForm::MessageIds)
            .into_iter()
            .flat_map(|value| header_ids(&value))
            .collect()
    };
    Ok(RemoteHeader {
        uid,
        message_id: message.message_id().map(ToOwned::to_owned),
        subject: message.subject().unwrap_or_default().to_owned(),
        sender: message.from().map(format_sender).unwrap_or_default(),
        date: message.date().map(ToString::to_string).unwrap_or_default(),
        is_read,
        is_starred,
        in_reply_to: ids(HeaderName::InReplyTo),
        references: ids(HeaderName::References),
        recipients: message.to().map(format_addresses).unwrap_or_default(),
        cc: message.cc().map(format_addresses).unwrap_or_default(),
        sent_at: message.date().and_then(date_timestamp),
        provider_message_id: None,
        provider_thread_id: None,
        content: Some(content),
    })
}

fn header_ids(value: &HeaderValue<'_>) -> Vec<String> {
    value
        .as_text_list()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.trim_matches(['<', '>']).to_owned())
        .collect()
}
fn format_sender(address: &mail_parser::Address<'_>) -> String {
    match address {
        mail_parser::Address::List(list) => list.first().map(format_addr).unwrap_or_default(),
        mail_parser::Address::Group(groups) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .next()
            .map(format_addr)
            .unwrap_or_default(),
    }
}
fn format_addresses(addresses: &mail_parser::Address<'_>) -> Vec<String> {
    match addresses {
        mail_parser::Address::List(list) => list.iter().map(format_addr).collect(),
        mail_parser::Address::Group(groups) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .map(format_addr)
            .collect(),
    }
}
fn format_addr(address: &Addr<'_>) -> String {
    match (address.name.as_deref(), address.address.as_deref()) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (None, Some(email)) => email.to_owned(),
        (Some(name), None) => name.to_owned(),
        (None, None) => String::new(),
    }
}

fn date_timestamp(date: &mail_parser::DateTime) -> Option<i64> {
    let naive = NaiveDate::from_ymd_opt(date.year.into(), date.month.into(), date.day.into())?
        .and_hms_opt(date.hour.into(), date.minute.into(), date.second.into())?;
    let seconds = i32::from(date.tz_hour) * 3600 + i32::from(date.tz_minute) * 60;
    FixedOffset::east_opt(if date.tz_before_gmt {
        -seconds
    } else {
        seconds
    })?
    .from_local_datetime(&naive)
    .single()
    .map(|d| d.timestamp())
}

fn normalize_content(message: &mail_parser::Message<'_>) -> Result<MessageContent, MimeError> {
    let plain_text = (0..message.text_body_count())
        .filter_map(|i| message.body_text(i))
        .map(std::borrow::Cow::into_owned)
        .collect::<Vec<_>>()
        .join("\n\n");
    let html = sanitize_html(
        &(0..message.html_body_count())
            .filter_map(|i| message.body_html(i))
            .map(std::borrow::Cow::into_owned)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut total = plain_text
        .len()
        .checked_add(html.len())
        .ok_or(MimeError::TooLarge)?;
    let mut attachments = Vec::new();
    for (index, part) in message.attachments().enumerate() {
        let data = match &part.body {
            PartType::Binary(data) | PartType::InlineBinary(data) => data.as_ref().to_vec(),
            PartType::Message(nested) => nested.raw_message().to_vec(),
            _ => return Err(MimeError::Encoding),
        };
        total = total.checked_add(data.len()).ok_or(MimeError::TooLarge)?;
        if total > MAX_MESSAGE_BYTES {
            return Err(MimeError::TooLarge);
        }
        let mime_type = part.content_type().map_or_else(
            || "application/octet-stream".to_owned(),
            |t| format!("{}/{}", t.ctype(), t.subtype().unwrap_or("octet-stream")),
        );
        let filename = part
            .attachment_name()
            .map(sanitize_filename)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("attachment-{}", index + 1));
        attachments.push(Attachment {
            id: format!("part-{}", index + 1),
            filename,
            mime_type,
            content_id: part.content_id().map(ToOwned::to_owned),
            data,
        });
    }
    Ok(MessageContent {
        plain_text,
        html,
        attachments,
    })
}
fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}
fn sanitize_html(html: &str) -> String {
    Builder::default()
        .tags(HashSet::from([
            "a",
            "b",
            "blockquote",
            "br",
            "code",
            "div",
            "em",
            "i",
            "img",
            "li",
            "ol",
            "p",
            "pre",
            "span",
            "strong",
            "table",
            "tbody",
            "td",
            "th",
            "thead",
            "tr",
            "ul",
        ]))
        .url_schemes(HashSet::from(["cid"]))
        .clean(html)
        .to_string()
}

#[must_use]
pub fn conversation_plain_text(text: &str, previous: &[String]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut best = None;
    for old in previous {
        let old_lines: Vec<&str> = old.lines().filter(|line| !line.trim().is_empty()).collect();
        if old_lines.is_empty() {
            continue;
        }
        for start in 0..lines.len() {
            if start + old_lines.len() <= lines.len()
                && lines[start..start + old_lines.len()]
                    .iter()
                    .zip(&old_lines)
                    .all(|(a, b)| a.trim_start_matches('>').trim() == *b)
            {
                best = Some(lines[..start].join("\n").trim_end().to_owned());
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quote_suppression_requires_exact_previous_body() {
        assert_eq!(
            conversation_plain_text("reply\n> old", &["old".into()]).as_deref(),
            Some("reply")
        );
        assert!(conversation_plain_text("On Tue wrote:\n> old", &["different".into()]).is_none());
    }
    #[test]
    fn sanitizes_active_and_remote_html() {
        let clean = sanitize_html(
            "<script>alert(1)</script><img src=\"https://x.test/a\"><img src=\"cid:abc\">",
        );
        assert!(!clean.contains("script") && !clean.contains("https://"));
        assert!(clean.contains("cid:abc"));
    }
}
