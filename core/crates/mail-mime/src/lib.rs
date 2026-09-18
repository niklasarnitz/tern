//! MIME/header normalization for the application model.
//!
//! The protocol crates hand raw RFC 5322 header bytes to this crate.  The UI
//! and database layers only see the portable `RemoteHeader` record.

use mail_model::RemoteHeader;
use mail_parser::{Addr, MessageParser};

/// Parse a fetched RFC 5322 header into the application's portable summary.
///
/// IMAP servers are allowed to return malformed or incomplete headers.  This
/// function deliberately keeps parsing best-effort: a malformed field gets a
/// stable empty value while the UID and server-provided flags are retained.
/// Callers should use the IMAP `INTERNALDATE` when available to replace the
/// header date (the header's Date field can be absent or untrustworthy).
#[must_use]
pub fn parse_header(uid: u32, bytes: &[u8], is_read: bool, is_starred: bool) -> RemoteHeader {
    let parsed = MessageParser::default().parse_headers(bytes);

    let (message_id, subject, sender, date) = parsed
        .as_ref()
        .map(|message| {
            let sender = message
                .from()
                .and_then(|addresses| addresses.first())
                .map(format_address)
                .unwrap_or_default();

            (
                message.message_id().map(ToOwned::to_owned),
                message.subject().unwrap_or_default().to_owned(),
                sender,
                message.date().map(ToString::to_string).unwrap_or_default(),
            )
        })
        .unwrap_or_default();

    RemoteHeader {
        uid,
        message_id,
        subject,
        sender,
        date,
        is_read,
        is_starred,
    }
}

fn format_address(address: &Addr<'_>) -> String {
    match (address.name.as_deref(), address.address.as_deref()) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (None, Some(email)) => email.to_owned(),
        (Some(name), None) => name.to_owned(),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_header;

    #[test]
    fn parses_encoded_headers_and_sender() {
        let header = concat!(
            "Message-ID: <abc@example.test>\r\n",
            "From: =?UTF-8?Q?J=C3=B6rg?= <joerg@example.test>\r\n",
            "Subject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n",
            "Date: Tue, 17 Sep 2026 08:30:00 +0000\r\n",
            "\r\n",
        );

        let parsed = parse_header(42, header.as_bytes(), true, false);

        assert_eq!(parsed.uid, 42);
        assert_eq!(parsed.message_id.as_deref(), Some("abc@example.test"));
        assert_eq!(parsed.subject, "Grüße");
        assert_eq!(parsed.sender, "Jörg <joerg@example.test>");
        assert_eq!(parsed.date, "2026-09-17T08:30:00Z");
        assert!(parsed.is_read);
        assert!(!parsed.is_starred);
    }

    #[test]
    fn preserves_uid_and_flags_for_malformed_header() {
        let parsed = parse_header(7, b"Subject: =?not-valid\r\n\r\n", false, true);

        assert_eq!(parsed.uid, 7);
        assert!(!parsed.is_read);
        assert!(parsed.is_starred);
    }
}
