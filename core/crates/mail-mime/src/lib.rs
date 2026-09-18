//! MIME normalization at the boundary between `mail-parser` and the app model.

use ammonia::{Builder, UrlRelative};
use chrono::{FixedOffset, NaiveDate, TimeZone};
use mail_model::{Attachment, DeliveryRecipient, DeliveryReport, MessageContent, RemoteHeader};
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
    let senders = message.from().map(format_addresses).unwrap_or_default();
    RemoteHeader {
        message_id: message.message_id().map(ToOwned::to_owned),
        subject: message.subject().unwrap_or_default().to_owned(),
        sender: senders.first().cloned().unwrap_or_default(),
        senders,
        date: message.date().map(ToString::to_string).unwrap_or_default(),
        in_reply_to: ids(HeaderName::InReplyTo),
        references: ids(HeaderName::References),
        recipients: message.to().map(format_addresses).unwrap_or_default(),
        cc: message.cc().map(format_addresses).unwrap_or_default(),
        bcc: message.bcc().map(format_addresses).unwrap_or_default(),
        reply_to: message
            .reply_to()
            .map(format_addresses)
            .unwrap_or_default(),
        sent_at: message.date().and_then(date_timestamp),
        list_id: message
            .list_id()
            .as_address()
            .map(format_addresses)
            .unwrap_or_default(),
        list_post: message
            .list_post()
            .as_address()
            .map(format_addresses)
            .unwrap_or_default(),
        list_unsubscribe: message
            .list_unsubscribe()
            .as_address()
            .map(format_addresses)
            .unwrap_or_default(),
        authentication_results: text_headers(message, HeaderName::AuthenticationResults),
        received_spf: text_headers(message, HeaderName::ReceivedSpf),
        ..Default::default()
    }
}

fn text_headers(message: &mail_parser::Message<'_>, name: HeaderName<'static>) -> Vec<String> {
    message
        .header_values(name)
        .filter_map(HeaderValue::as_text)
        .map(ToOwned::to_owned)
        .collect()
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
    let human_text = (0..message.text_body_count())
        .filter_map(|i| message.body_text(i))
        .map(std::borrow::Cow::into_owned)
        .collect::<Vec<_>>()
        .join("\n\n");
    let delivery_report = parse_delivery_report(message);
    let plain_text = delivery_report.as_ref().map_or_else(
        || human_text.clone(),
        |report| format_delivery_report(report, &human_text),
    );
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
    for part in message.attachments() {
        if is_delivery_status(part) {
            continue;
        }
        let index = attachments.len();
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
        delivery_report,
    })
}

fn is_delivery_status(part: &mail_parser::MessagePart<'_>) -> bool {
    part.content_type().is_some_and(|content_type| {
        content_type.ctype().eq_ignore_ascii_case("message")
            && content_type.subtype().is_some_and(|subtype| {
                subtype.eq_ignore_ascii_case("delivery-status")
                    || subtype.eq_ignore_ascii_case("global-delivery-status")
            })
    })
}

fn parse_delivery_report(message: &mail_parser::Message<'_>) -> Option<DeliveryReport> {
    let mut report = DeliveryReport::default();
    for part in &message.parts {
        if !is_delivery_status(part) {
            continue;
        }
        let blocks = parse_status_blocks(part.contents());
        for (index, fields) in blocks.into_iter().enumerate() {
            if index == 0 && report.reporting_mta.is_empty() {
                report.reporting_mta = field_value(&fields, "reporting-mta")
                    .map(|value| strip_typed_value(&value))
                    .unwrap_or_default();
            }
            if index > 0 || fields.contains_key("final-recipient") {
                let recipient = field_value(&fields, "final-recipient")
                    .or_else(|| field_value(&fields, "original-recipient"))
                    .map(|value| strip_typed_value(&value))
                    .unwrap_or_default();
                let action = field_value(&fields, "action").unwrap_or_default();
                let diagnostic = field_value(&fields, "diagnostic-code")
                    .map(|value| strip_typed_value(&value))
                    .unwrap_or_default();
                let status_code = field_value(&fields, "status")
                    .or_else(|| find_enhanced_status(&diagnostic))
                    .unwrap_or_default();
                if recipient.is_empty()
                    && action.is_empty()
                    && status_code.is_empty()
                    && diagnostic.is_empty()
                {
                    continue;
                }
                report.recipients.push(DeliveryRecipient {
                    recipient,
                    action,
                    status_description: describe_status(&status_code).to_owned(),
                    status_code,
                    diagnostic,
                });
            }
        }
    }
    (!report.recipients.is_empty()).then_some(report)
}

fn parse_status_blocks(bytes: &[u8]) -> Vec<HashMap<String, String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut blocks = Vec::new();
    let mut fields = HashMap::<String, String>::new();
    let mut last_name = None::<String>;
    for raw_line in text.lines().chain(std::iter::once("")) {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            if !fields.is_empty() {
                blocks.push(std::mem::take(&mut fields));
            }
            last_name = None;
        } else if line.starts_with([' ', '\t']) {
            if let Some(name) = &last_name {
                if let Some(value) = fields.get_mut(name) {
                    value.push(' ');
                    value.push_str(line.trim());
                }
            }
        } else if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            fields.insert(name.clone(), value.trim().to_owned());
            last_name = Some(name);
        } else {
            last_name = None;
        }
    }
    blocks
}

fn field_value(fields: &HashMap<String, String>, name: &str) -> Option<String> {
    fields
        .get(name)
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| !value.is_empty())
}

fn strip_typed_value(value: &str) -> String {
    value
        .split_once(';')
        .map_or(value, |(_, untyped)| untyped)
        .trim()
        .to_owned()
}

fn find_enhanced_status(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
        })
        .find(|word| {
            let components = word.split('.').collect::<Vec<_>>();
            components.len() == 3
                && components[0].len() == 1
                && matches!(components[0], "2" | "4" | "5")
                && components[1..].iter().all(|component| {
                    !component.is_empty() && component.chars().all(|c| c.is_ascii_digit())
                })
        })
        .map(ToOwned::to_owned)
}

fn describe_status(code: &str) -> &'static str {
    match code {
        "5.1.1" => "The recipient address does not exist",
        "5.1.2" => "The recipient domain could not be reached",
        "5.2.2" => "The recipient mailbox is full",
        "5.7.1" => "Delivery was rejected by a security or policy rule",
        code if code.starts_with("2.") => "Delivery succeeded",
        code if code.starts_with("4.") => "Delivery is temporarily delayed",
        code if code.starts_with("5.") => "Delivery failed permanently",
        _ => "",
    }
}

fn format_delivery_report(report: &DeliveryReport, human_text: &str) -> String {
    let headline = if report
        .recipients
        .iter()
        .all(|recipient| recipient.action.eq_ignore_ascii_case("failed"))
    {
        "Delivery failed"
    } else if report
        .recipients
        .iter()
        .all(|recipient| recipient.action.eq_ignore_ascii_case("delayed"))
    {
        "Delivery delayed"
    } else if report.recipients.iter().all(|recipient| {
        matches!(
            recipient.action.to_ascii_lowercase().as_str(),
            "delivered" | "relayed" | "expanded"
        )
    }) {
        "Delivery succeeded"
    } else {
        "Delivery status"
    };
    let mut lines = vec![headline.to_owned()];
    for recipient in &report.recipients {
        lines.push(String::new());
        if !recipient.recipient.is_empty() {
            lines.push(format!("Recipient: {}", recipient.recipient));
        }
        let status = match (
            recipient.status_code.is_empty(),
            recipient.status_description.is_empty(),
        ) {
            (false, false) => format!(
                "Status: {} — {}",
                recipient.status_code, recipient.status_description
            ),
            (false, true) => format!("Status: {}", recipient.status_code),
            (true, false) => format!("Status: {}", recipient.status_description),
            (true, true) if !recipient.action.is_empty() => {
                format!("Status: {}", recipient.action)
            }
            (true, true) => String::new(),
        };
        if !status.is_empty() {
            lines.push(status);
        }
        if !recipient.diagnostic.is_empty() {
            lines.push(format!("Details: {}", recipient.diagnostic));
        }
    }
    if !report.reporting_mta.is_empty() {
        lines.push(String::new());
        lines.push(format!("Reported by: {}", report.reporting_mta));
    }
    let human_text = human_text.trim();
    if !human_text.is_empty() {
        lines.push(String::new());
        lines.push(human_text.to_owned());
    }
    lines.join("\n")
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
            "Bcc: Hidden <hidden@example.test>\r\n",
            "Reply-To: Replies <reply@example.test>\r\n",
            "List-ID: Tern Updates <updates.tern.example>\r\n",
            "List-Post: <mailto:updates@tern.example>\r\n",
            "List-Unsubscribe: <https://tern.example/unsubscribe>, <mailto:leave@tern.example>\r\n",
            "Authentication-Results: mx.example; dkim=pass; spf=pass\r\n",
            "Received-SPF: pass client-ip=192.0.2.1\r\n",
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
        assert_eq!(header.bcc, vec!["Hidden <hidden@example.test>"]);
        assert_eq!(header.reply_to, vec!["Replies <reply@example.test>"]);
        assert_eq!(header.list_id, vec!["Tern Updates <updates.tern.example>"]);
        assert_eq!(header.list_post, vec!["mailto:updates@tern.example"]);
        assert_eq!(header.list_unsubscribe.len(), 2);
        assert_eq!(header.authentication_results.len(), 1);
        assert_eq!(header.received_spf.len(), 1);
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
    fn normalizes_multi_recipient_delivery_status_report() {
        let raw = concat!(
            "From: Mail Delivery System <mailer-daemon@example.test>\r\n",
            "Subject: Delivery Status Notification\r\n",
            "Content-Type: multipart/report; report-type=delivery-status; boundary=dsn\r\n\r\n",
            "--dsn\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n",
            "The server could not deliver all recipients.\r\n",
            "--dsn\r\nContent-Type: message/delivery-status\r\n\r\n",
            "Reporting-MTA: dns; mx.example.test\r\n",
            "Arrival-Date: Fri, 18 Sep 2026 08:00:00 +0000\r\n\r\n",
            "Final-Recipient: rfc822; missing@example.test\r\n",
            "Action: failed\r\n",
            "Status: 5.1.1\r\n",
            "Diagnostic-Code: smtp; 550 5.1.1 User unknown\r\n\r\n",
            "Original-Recipient: rfc822; slow@example.test\r\n",
            "Action: delayed\r\n",
            "Diagnostic-Code: smtp; 450 4.2.2 Mailbox temporarily\r\n",
            " full\r\n\r\n",
            "--dsn\r\nContent-Type: message/rfc822\r\n",
            "Content-Disposition: attachment; filename=original.eml\r\n\r\n",
            "From: sender@example.test\r\nTo: missing@example.test\r\n",
            "Subject: Hello\r\nContent-Type: text/plain\r\n\r\nHello\r\n",
            "--dsn--\r\n"
        );
        let parsed = parse_message(10, raw.as_bytes(), false, false);
        assert!(parsed.is_ok());
        if let Ok(header) = parsed {
            assert!(header.content.is_some());
            if let Some(content) = header.content {
                assert_eq!(content.attachments.len(), 1);
                assert_eq!(content.attachments[0].filename, "original.eml");
                assert!(content.plain_text.starts_with("Delivery status\n"));
                assert!(content
                    .plain_text
                    .contains("Recipient: missing@example.test"));
                assert!(content
                    .plain_text
                    .contains("Status: 5.1.1 — The recipient address does not exist"));
                assert!(content.plain_text.contains("Recipient: slow@example.test"));
                assert!(content
                    .plain_text
                    .contains("Status: 4.2.2 — Delivery is temporarily delayed"));
                assert!(content
                    .plain_text
                    .contains("Details: 450 4.2.2 Mailbox temporarily full"));
                assert!(content.plain_text.contains("Reported by: mx.example.test"));
                assert!(content
                    .plain_text
                    .ends_with("The server could not deliver all recipients."));

                assert!(content.delivery_report.is_some());
                if let Some(report) = content.delivery_report {
                    assert_eq!(report.reporting_mta, "mx.example.test");
                    assert_eq!(report.recipients.len(), 2);
                    assert_eq!(report.recipients[0].action, "failed");
                    assert_eq!(report.recipients[1].status_code, "4.2.2");
                }
            }
        }
    }

    #[test]
    fn recognizes_international_delivery_status_report() {
        let raw = concat!(
            "Content-Type: multipart/report; report-type=global-delivery-status; boundary=x\r\n\r\n",
            "--x\r\nContent-Type: text/plain\r\n\r\nDelivery complete.\r\n",
            "--x\r\nContent-Type: message/global-delivery-status\r\n\r\n",
            "Reporting-MTA: dns; mx.example.test\r\n\r\n",
            "Final-Recipient: utf-8; user@example.test\r\n",
            "Action: delivered\r\nStatus: 2.0.0\r\n\r\n",
            "--x--\r\n"
        );
        let parsed = parse_message(11, raw.as_bytes(), false, false);
        assert!(parsed.is_ok());
        if let Ok(header) = parsed {
            if let Some(content) = header.content {
                assert!(content.plain_text.starts_with("Delivery succeeded\n"));
                assert!(content
                    .plain_text
                    .contains("Status: 2.0.0 — Delivery succeeded"));
                assert!(content.attachments.is_empty());
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
