//! MIME normalization at the boundary between `mail-parser` and the app model.

use ammonia::{Builder, UrlRelative};
use chrono::{FixedOffset, NaiveDate, TimeZone};
use mail_model::{Attachment, MessageContent, RemoteHeader};
use mail_parser::{
    Addr, HeaderForm, HeaderName, HeaderValue, MessageParser, MimeHeaders, PartType,
};
use std::collections::{HashMap, HashSet};
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
    let mut header = parsed
        .as_ref()
        .map_or_else(RemoteHeader::default, header_metadata);
    header.uid = uid;
    header.is_read = is_read;
    header.is_starred = is_starred;
    header
}

fn header_metadata(message: &mail_parser::Message<'_>) -> RemoteHeader {
    let ids = |name: HeaderName<'static>| {
        message
            .header_as(name, HeaderForm::MessageIds)
            .into_iter()
            .flat_map(|value| header_ids(&value))
            .collect()
    };
    RemoteHeader {
        message_id: message.message_id().map(ToOwned::to_owned),
        subject: message.subject().unwrap_or_default().to_owned(),
        sender: message.from().map(format_sender).unwrap_or_default(),
        date: message.date().map(ToString::to_string).unwrap_or_default(),
        in_reply_to: ids(HeaderName::InReplyTo),
        references: ids(HeaderName::References),
        recipients: message.to().map(format_addresses).unwrap_or_default(),
        cc: message.cc().map(format_addresses).unwrap_or_default(),
        sent_at: message.date().and_then(date_timestamp),
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
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(MimeError::TooLarge);
    }
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or(MimeError::Parse)?;
    if message.parts.iter().any(|part| part.is_encoding_problem) {
        return Err(MimeError::Encoding);
    }
    let content = normalize_content(&message)?;
    let mut header = header_metadata(&message);
    header.uid = uid;
    header.is_read = is_read;
    header.is_starred = is_starred;
    header.content = Some(content);
    Ok(header)
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
    if total > MAX_MESSAGE_BYTES {
        return Err(MimeError::TooLarge);
    }
    let mut attachments = Vec::new();
    for (index, part) in message.attachments().enumerate() {
        let data = match &part.body {
            PartType::Binary(data) | PartType::InlineBinary(data) => data.as_ref().to_vec(),
            PartType::Text(text) | PartType::Html(text) => text.as_bytes().to_vec(),
            PartType::Message(nested) => nested.raw_message().to_vec(),
            PartType::Multipart(_) => return Err(MimeError::Encoding),
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
    Builder::empty()
        .tags(HashSet::from([
            "a",
            "b",
            "blockquote",
            "br",
            "code",
            "div",
            "em",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
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
        .clean_content_tags(HashSet::from([
            "applet", "audio", "canvas", "embed", "iframe", "math", "noembed", "noscript",
            "object", "script", "style", "svg", "template", "video",
        ]))
        .tag_attributes(HashMap::from([
            ("a", HashSet::from(["href"])),
            ("img", HashSet::from(["alt", "src"])),
        ]))
        .generic_attributes(HashSet::new())
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid"]))
        .url_relative(UrlRelative::Deny)
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("img", "src") if !has_cid_scheme(value) => None,
            ("a", "href") if has_cid_scheme(value) => None,
            _ => Some(value.into()),
        })
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .clean(html)
        .to_string()
}

fn has_cid_scheme(value: &str) -> bool {
    value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cid:"))
}

#[must_use]
pub fn conversation_plain_text(text: &str, previous: &[String]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut removed = false;
    let mut output = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].trim_start().starts_with('>') {
            output.push(lines[index]);
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && lines[index].trim_start().starts_with('>') {
            index += 1;
        }
        let quoted: Vec<String> = lines[start..index]
            .iter()
            .map(|line| {
                line.trim_start()
                    .strip_prefix('>')
                    .unwrap_or_default()
                    .trim_start()
                    .to_owned()
            })
            .collect();
        let matches_previous = previous.iter().any(|old| {
            let old_lines: Vec<&str> = old.lines().collect();
            !old_lines.is_empty()
                && old_lines.len() == quoted.len()
                && old_lines
                    .iter()
                    .zip(&quoted)
                    .all(|(old, line)| *old == line)
        });
        if matches_previous {
            removed = true;
        } else {
            output.extend(&lines[start..index]);
        }
    }
    removed.then(|| output.join("\n").trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_encoded_subject_and_sender() {
        let raw = concat!(
            "Message-ID: <abc@example.test>\r\n",
            "From: =?UTF-8?Q?J=C3=B6rg?= <joerg@example.test>\r\n",
            "Subject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n",
            "Date: Tue, 17 Sep 2026 08:30:00 +0000\r\n\r\n"
        );
        let parsed = parse_header(42, raw.as_bytes(), true, false);
        assert_eq!(parsed.message_id.as_deref(), Some("abc@example.test"));
        assert_eq!(parsed.subject, "Grüße");
        assert_eq!(parsed.sender, "Jörg <joerg@example.test>");
    }

    #[test]
    fn malformed_headers_preserve_uid_and_flags() {
        let parsed = parse_header(7, b"Subject: =?not-valid\r\n\r\n", false, true);
        assert_eq!(parsed.uid, 7);
        assert!(!parsed.is_read);
        assert!(parsed.is_starred);
    }

    #[test]
    fn parse_header_retains_thread_metadata() {
        let raw = concat!(
            "Message-ID: <new@example.test>\r\n",
            "In-Reply-To: <old@example.test>\r\n",
            "References: <root@example.test> <old@example.test>\r\n",
            "From: Sender <sender@example.test>\r\n",
            "To: A <a@example.test>, b@example.test\r\n",
            "Cc: C <c@example.test>\r\n",
            "Date: Tue, 17 Sep 2026 08:30:00 +0200\r\n\r\n"
        );
        let header = parse_header(9, raw.as_bytes(), true, false);
        assert_eq!(header.in_reply_to, vec!["old@example.test"]);
        assert_eq!(
            header.references,
            vec!["root@example.test", "old@example.test"]
        );
        assert_eq!(header.recipients.len(), 2);
        assert_eq!(header.cc, vec!["C <c@example.test>"]);
        assert!(header.sent_at.is_some());
    }

    #[test]
    fn parse_message_normalizes_alternative_attachments_and_cid() {
        let raw = concat!(
            "Message-ID: <m@example.test>\r\nFrom: A <a@example.test>\r\n",
            "Date: Tue, 17 Sep 2026 08:30:00 +0000\r\n",
            "Content-Type: multipart/mixed; boundary=outer\r\n\r\n",
            "--outer\r\nContent-Type: multipart/alternative; boundary=inner\r\n\r\n",
            "--inner\r\nContent-Type: text/plain\r\n\r\nplain body\r\n",
            "--inner\r\nContent-Type: text/html\r\n\r\n<h1>hello</h1><script>x</script><img src=\"cid:pic\"><img src=\"https://tracker.test/x\">\r\n",
            "--inner--\r\n",
            "--outer\r\nContent-Type: text/plain; name=note.txt\r\nContent-Disposition: attachment; filename=note.txt\r\n\r\nattached text\r\n",
            "--outer\r\nContent-Type: image/png\r\nContent-ID: <pic>\r\nContent-Disposition: inline; filename=pic.png\r\nContent-Transfer-Encoding: base64\r\n\r\naGk=\r\n",
            "--outer--\r\n"
        );
        let parsed = parse_message(4, raw.as_bytes(), false, true);
        assert!(parsed.is_ok());
        if let Ok(header) = parsed {
            assert!(header.content.is_some());
            if let Some(content) = header.content {
                assert!(content.plain_text.contains("plain body"));
                assert!(content.html.contains("<h1>hello</h1>"));
                assert!(!content.html.contains("script") && !content.html.contains("https://"));
                assert_eq!(content.attachments.len(), 2);
                assert_eq!(content.attachments[0].data, b"attached text");
                assert_eq!(content.attachments[1].content_id.as_deref(), Some("pic"));
            }
        }
    }

    #[test]
    fn parse_message_preserves_forwarded_rfc822_attachment_and_links() {
        let raw = concat!(
            "Content-Type: multipart/mixed; boundary=x\r\n\r\n",
            "--x\r\nContent-Type: text/html\r\n\r\n",
            "<p><a href=\"https://example.test\">link</a></p><img src=\"relative.png\"><img src=\"cid:ok\">\r\n",
            "--x\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=forwarded.eml\r\n\r\n",
            "Subject: Forwarded\r\nContent-Type: text/plain\r\n\r\nforwarded body\r\n",
            "--x--\r\n"
        );
        let parsed = parse_message(5, raw.as_bytes(), true, false);
        assert!(parsed.is_ok());
        if let Ok(header) = parsed {
            assert!(header.content.is_some());
            if let Some(content) = header.content {
                assert!(content.html.contains("href=\"https://example.test\""));
                assert!(!content.html.contains("relative.png"));
                assert_eq!(content.attachments.len(), 1);
                assert!(String::from_utf8_lossy(&content.attachments[0].data)
                    .contains("forwarded body"));
            }
        }
    }

    #[test]
    fn normalized_body_budget_is_enforced_without_attachments() {
        let repeated = "<b>x</b>".repeat(3_000_000);
        let raw = format!("Content-Type: text/html\r\n\r\n{repeated}");
        assert!(raw.len() < MAX_MESSAGE_BYTES);
        assert!(matches!(
            parse_message(6, raw.as_bytes(), false, false),
            Err(MimeError::TooLarge)
        ));
    }

    #[test]
    fn parse_message_rejects_bad_transfer_and_oversize_input() {
        let bad = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!";
        assert!(matches!(
            parse_message(1, bad, false, false),
            Err(MimeError::Encoding)
        ));
        let oversized = vec![b'a'; MAX_MESSAGE_BYTES + 1];
        assert!(matches!(
            parse_message(1, &oversized, false, false),
            Err(MimeError::TooLarge)
        ));
    }

    #[test]
    fn quote_suppression_requires_exact_previous_body() {
        assert_eq!(
            conversation_plain_text("reply\n> old", &["old".into()]).as_deref(),
            Some("reply")
        );
        assert!(conversation_plain_text("On Tue wrote:\n> old", &["different".into()]).is_none());
        assert!(conversation_plain_text("old", &["old".into()]).is_none());
        assert_eq!(
            conversation_plain_text("one\n> old\ntwo", &["old".into()]).as_deref(),
            Some("one\ntwo")
        );
        assert_eq!(
            conversation_plain_text("reply\n> old\n> \n> text", &["old\n\ntext".into()]).as_deref(),
            Some("reply")
        );
    }
    #[test]
    fn sanitizes_active_embedded_and_form_html() {
        let clean = sanitize_html(concat!(
            "<script>alert(1)</script><style>body{display:none}</style>",
            "<form action=\"https://evil.test\"><p>visible</p>",
            "<input name=secret><button formaction=\"https://evil.test\">Send</button></form>",
            "<iframe srcdoc=\"<script>alert(2)</script>\">frame</iframe>",
            "<object data=\"https://evil.test\">object</object>",
            "<embed src=\"https://evil.test\"><svg onload=\"alert(3)\"><circle/></svg>",
            "<p onclick=\"alert(4)\">safe</p>",
        ));
        assert!(
            clean.contains("<p>visible</p>") && clean.contains("Send") && clean.contains("safe")
        );
        for forbidden in [
            "script",
            "style",
            "form",
            "input",
            "button",
            "iframe",
            "object",
            "embed",
            "svg",
            "onclick",
            "formaction",
            "https://evil.test",
            "alert(",
        ] {
            assert!(!clean.contains(forbidden), "retained {forbidden}: {clean}");
        }
    }

    #[test]
    fn blocks_tracking_and_local_resource_urls() {
        let clean = sanitize_html(concat!(
            "<img src=\"https://tracker.test/pixel\" alt=remote>",
            "<img src=\"//tracker.test/pixel\" alt=relative>",
            "<img src=\"file:///etc/passwd\" alt=file>",
            "<img src=\"cid:part-1\" alt=inline width=1 height=1>",
            "<a href=\"file:///etc/passwd\">file link</a>",
            "<a href=\"cid:part-1\">cid link</a>",
            "<a href=\"https://example.test/path\">web link</a>",
        ));
        assert!(!clean.contains("tracker.test") && !clean.contains("file:///"));
        assert!(clean.contains("<img src=\"cid:part-1\" alt=\"inline\">"));
        assert!(
            !clean.contains("width=")
                && !clean.contains("height=")
                && !clean.contains("href=\"cid:")
        );
        assert!(clean.contains("href=\"https://example.test/path\" rel=\"noopener noreferrer\""));
    }

    #[test]
    fn blocks_dangerous_urls_and_css_abuse() {
        let clean = sanitize_html(concat!(
            "<style>@import url(https://tracker.test);</style>",
            "<div id=overlay class=cover style=\"position:fixed;background:url(https://tracker.test)\">",
            "<a href=\"java&#x73;cript:alert(1)\">script</a>",
            "<a href=\"data:text/html,evil\">data</a>",
            "<a href=\"mailto:user@example.test\">mail</a></div>",
        ));
        for forbidden in [
            "style=",
            "<style",
            "@import",
            "class=",
            "id=",
            "position",
            "tracker.test",
            "javascript:",
            "data:text/html",
            "alert(",
        ] {
            assert!(!clean.contains(forbidden), "retained {forbidden}: {clean}");
        }
        assert!(clean.contains("href=\"mailto:user@example.test\""));
    }
}
