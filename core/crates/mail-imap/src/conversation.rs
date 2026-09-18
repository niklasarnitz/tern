//! Conversation downloads and replay of durable, UID-scoped local actions.
use super::{
    connect_and_login, fetch_headers_session, validate_account, ImapError, Result,
    OPERATION_TIMEOUT,
};
use async_imap::{
    imap_proto::{types::AttributeValue, Response, Status},
    Session,
};
use mail_model::{Account, MailAction, MailboxSnapshot, PendingOperation, RemoteHeader};
use std::fmt::Debug;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;

/// Download recent headers and complete MIME bodies using read-only commands.
/// `cached_uids` must refer to bodies already stored under `expected_validity`.
///
/// # Errors
/// Returns a sanitized error for failed TLS, authentication, protocol or MIME data.
pub async fn fetch_mailbox(
    account: &Account,
    password: &str,
    remote_name: &str,
    expected_validity: Option<u32>,
    cached_uids: &[u32],
) -> Result<MailboxSnapshot> {
    validate_account(account, password)?;
    let mut session = connect_and_login(account, password).await?;
    let capabilities = timeout(OPERATION_TIMEOUT, session.capabilities())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    let mut snapshot = timeout(
        OPERATION_TIMEOUT,
        fetch_headers_session(
            &mut session,
            remote_name,
            capabilities.has_str("X-GM-EXT-1"),
        ),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    let mut downloaded_bytes = 0;
    for header in &mut snapshot.headers {
        if downloaded_bytes >= 64 * 1024 * 1024 {
            break;
        }
        if expected_validity == Some(snapshot.uid_validity) && cached_uids.contains(&header.uid) {
            continue;
        }
        // Fetch separately so a single large attachment does not multiply the
        // protocol buffer size by the number of messages in the snapshot.
        match timeout(OPERATION_TIMEOUT, fetch_body_session(&mut session, header)).await {
            Ok(Ok(())) => {
                if let Some(content) = &header.content {
                    downloaded_bytes += content.plain_text.len()
                        + content.html.len()
                        + content
                            .attachments
                            .iter()
                            .map(|attachment| attachment.data.len())
                            .sum::<usize>();
                }
            }
            Ok(Err(ImapError::MessageTooLarge)) => {} // Header stays usable; reader reports uncached body.
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(ImapError::Timeout),
        }
    }
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(snapshot)
}

async fn fetch_body_session<T>(session: &mut Session<T>, header: &mut RemoteHeader) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let uid = header.uid;
    let maximum = MAX_MESSAGE_BYTES + 1;
    let tag = session
        .run_command(format!(
            "UID FETCH {uid} (UID RFC822.SIZE BODY.PEEK[]<0.{maximum}>)"
        ))
        .await
        .map_err(|_| ImapError::Protocol)?;
    let mut parsed = None;
    let mut problem = None;
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|_| ImapError::Protocol)?
            .ok_or(ImapError::Connection)?;
        match response.parsed() {
            Response::Fetch(_, attributes) => {
                let returned_uid = attributes.iter().find_map(|value| match value {
                    AttributeValue::Uid(value) => Some(*value),
                    _ => None,
                });
                if returned_uid != Some(uid) {
                    continue;
                }
                let size = attributes.iter().find_map(|value| match value {
                    AttributeValue::Rfc822Size(value) => Some(*value as usize),
                    _ => None,
                });
                let raw = attributes.iter().find_map(|value| match value {
                    AttributeValue::BodySection {
                        section: None,
                        index: None | Some(0),
                        data: Some(value),
                    } => Some(value.as_ref()),
                    _ => None,
                });
                if size.is_some_and(|size| size > MAX_MESSAGE_BYTES)
                    || raw.is_some_and(|raw| raw.len() > MAX_MESSAGE_BYTES)
                {
                    problem = Some(ImapError::MessageTooLarge);
                } else if let Some(raw) = raw.filter(|raw| size == Some(raw.len())) {
                    if parsed.is_some() {
                        problem = Some(ImapError::Protocol);
                    }
                    parsed = Some(
                        mail_mime::parse_message(uid, raw, header.is_read, header.is_starred)
                            .map_err(|_| ImapError::InvalidMime)?,
                    );
                } else {
                    problem = Some(ImapError::MissingHeader);
                }
            }
            Response::Done {
                tag: received,
                status,
                ..
            } if received == &tag => {
                if status != &Status::Ok {
                    return Err(ImapError::Protocol);
                }
                break;
            }
            _ => {}
        }
    }
    if let Some(error) = problem {
        return Err(error);
    }
    let mut message = parsed.ok_or(ImapError::MissingHeader)?;
    message.date.clone_from(&header.date);
    message
        .provider_message_id
        .clone_from(&header.provider_message_id);
    message
        .provider_thread_id
        .clone_from(&header.provider_thread_id);
    *header = message;
    Ok(())
}

/// Replay one durable operation only after verifying its mailbox generation.
/// Moves require atomic UID MOVE; no broad EXPUNGE or unsafe COPY fallback is used.
///
/// # Errors
/// Returns an error if the operation cannot be safely applied or acknowledged.
pub async fn apply_operation(
    account: &Account,
    password: &str,
    operation: &PendingOperation,
) -> Result<()> {
    validate_account(account, password)?;
    if operation.account_id != account.id || operation.remote_uid == 0 {
        return Err(ImapError::InvalidConfiguration);
    }
    let mut session = connect_and_login(account, password).await?;
    let result = timeout(
        OPERATION_TIMEOUT,
        apply_operation_session(&mut session, operation),
    )
    .await
    .map_err(|_| ImapError::Timeout)?;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    result
}

async fn apply_operation_session<T>(
    session: &mut Session<T>,
    operation: &PendingOperation,
) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mailbox = session
        .select(&operation.remote_name)
        .await
        .map_err(|_| ImapError::Protocol)?;
    if mailbox.uid_validity != Some(operation.uid_validity) {
        return Err(ImapError::StaleMailbox);
    }
    let uid = operation.remote_uid;
    let command = match operation.action {
        MailAction::MarkRead => format!("UID STORE {uid} +FLAGS.SILENT (\\Seen)"),
        MailAction::MarkUnread => format!("UID STORE {uid} -FLAGS.SILENT (\\Seen)"),
        MailAction::Star => format!("UID STORE {uid} +FLAGS.SILENT (\\Flagged)"),
        MailAction::Unstar => format!("UID STORE {uid} -FLAGS.SILENT (\\Flagged)"),
        MailAction::Move => {
            let capabilities = session
                .capabilities()
                .await
                .map_err(|_| ImapError::Protocol)?;
            if !capabilities.has_str("MOVE") {
                return Err(ImapError::MoveUnsupported);
            }
            let destination = operation
                .destination_remote_name
                .as_deref()
                .ok_or(ImapError::InvalidConfiguration)?;
            format!("UID MOVE {uid} {}", quoted_mailbox(destination)?)
        }
    };
    checked_command(session, command).await
}

fn quoted_mailbox(name: &str) -> Result<String> {
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(ImapError::InvalidConfiguration);
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

async fn checked_command<T>(session: &mut Session<T>, command: String) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let tag = session
        .run_command(command)
        .await
        .map_err(|_| ImapError::Protocol)?;
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|_| ImapError::Protocol)?
            .ok_or(ImapError::Connection)?;
        if let Response::Done {
            tag: received,
            status,
            ..
        } = response.parsed()
        {
            if received == &tag {
                return if status == &Status::Ok {
                    Ok(())
                } else {
                    Err(ImapError::Protocol)
                };
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "Test fixtures fail immediately")]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
    use tokio::task::JoinHandle;

    async fn script(
        exchanges: Vec<(&'static str, String)>,
    ) -> (Session<DuplexStream>, JoinHandle<()>) {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_stream);
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"* OK ready\r\n").await.unwrap();
            let login = lines.next_line().await.unwrap().unwrap();
            assert!(login.starts_with("A0001 LOGIN"));
            writer
                .write_all(b"A0001 OK authenticated\r\n")
                .await
                .unwrap();
            for (expected, response) in exchanges {
                let line = lines.next_line().await.unwrap().unwrap();
                assert!(line.contains(expected), "unexpected command: {line}");
                writer.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let mut client = async_imap::Client::new(client_stream);
        client.read_response().await.unwrap();
        (client.login("test", "test").await.unwrap(), server)
    }

    fn operation(action: MailAction) -> PendingOperation {
        PendingOperation {
            id: "operation".into(),
            account_id: "account".into(),
            mailbox_id: "inbox".into(),
            remote_name: "INBOX".into(),
            message_id: "message".into(),
            uid_validity: 17,
            remote_uid: 42,
            action,
            destination_mailbox_id: Some("archive".into()),
            destination_remote_name: Some("Archive".into()),
        }
    }

    fn selected(validity: u32) -> String {
        format!("* 1 EXISTS\r\n* OK [UIDVALIDITY {validity}] valid\r\nA0002 OK [READ-WRITE] selected\r\n")
    }

    #[tokio::test]
    async fn conversation_headers_include_reply_and_provider_identity() {
        let raw = "Message-ID: <reply@example.test>\r\nReferences: <root@example.test>\r\nIn-Reply-To: <root@example.test>\r\nTo: Team <team@example.test>\r\nSubject: Reply\r\n\r\n";
        let response = format!("* 1 FETCH (UID 42 FLAGS () X-GM-MSGID 123456789 X-GM-THRID 987654321 BODY[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES TO)] {{{}}}\r\n{raw})\r\nA0003 OK done\r\n",raw.len());
        let (mut session, server) = script(vec![
            ("EXAMINE", selected(17)),
            (
                "X-GM-MSGID X-GM-THRID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES",
                response,
            ),
        ])
        .await;
        let snapshot = fetch_headers_session(&mut session, "INBOX", true)
            .await
            .unwrap();
        let header = &snapshot.headers[0];
        assert_eq!(header.provider_message_id.as_deref(), Some("123456789"));
        assert_eq!(header.provider_thread_id.as_deref(), Some("987654321"));
        assert!(!header.references.is_empty());
        assert!(!header.in_reply_to.is_empty());
        assert!(header.recipients[0].contains("team@example.test"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn body_fetch_preserves_flags_and_requires_full_literal() {
        let raw = "Message-ID: <reply@example.test>\r\nIn-Reply-To: <root@example.test>\r\nSubject: Reply\r\nContent-Type: text/plain\r\n\r\nHello from a reply.";
        let response = format!(
            "* 1 FETCH (UID 42 RFC822.SIZE {} BODY[]<0> {{{}}}\r\n{raw})\r\nA0002 OK done\r\n",
            raw.len(),
            raw.len()
        );
        let (mut session, server) = script(vec![(
            "UID FETCH 42 (UID RFC822.SIZE BODY.PEEK[]<0.",
            response,
        )])
        .await;
        let mut header = RemoteHeader {
            uid: 42,
            is_starred: true,
            date: "arrival".into(),
            ..RemoteHeader::default()
        };
        fetch_body_session(&mut session, &mut header).await.unwrap();
        assert!(header.is_starred);
        assert!(!header.is_read);
        assert_eq!(header.date, "arrival");
        assert!(header
            .content
            .unwrap()
            .plain_text
            .contains("Hello from a reply."));
        assert!(!header.in_reply_to.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn body_fetch_rejects_truncation_and_tagged_failure() {
        for (size, completion) in [(100, "OK"), (3, "NO")] {
            let response = format!("* 1 FETCH (UID 42 RFC822.SIZE {size} BODY[]<0> {{3}}\r\nabc)\r\nA0002 {completion} done\r\n");
            let (mut session, server) = script(vec![("UID FETCH", response)]).await;
            let mut header = RemoteHeader {
                uid: 42,
                ..RemoteHeader::default()
            };
            assert!(fetch_body_session(&mut session, &mut header).await.is_err());
            assert!(header.content.is_none());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn stale_generation_never_sends_a_store() {
        let (mut session, server) = script(vec![("SELECT \"INBOX\"", selected(18))]).await;
        assert_eq!(
            apply_operation_session(&mut session, &operation(MailAction::MarkRead)).await,
            Err(ImapError::StaleMailbox)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn flags_are_uid_scoped_and_tagged_failure_is_not_success() {
        for status in ["OK", "NO"] {
            let (mut session, server) = script(vec![
                ("SELECT \"INBOX\"", selected(17)),
                (
                    "UID STORE 42 +FLAGS.SILENT (\\Seen)",
                    format!("A0003 {status} done\r\n"),
                ),
            ])
            .await;
            let result =
                apply_operation_session(&mut session, &operation(MailAction::MarkRead)).await;
            assert_eq!(result.is_ok(), status == "OK");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn move_uses_atomic_uid_command_and_requires_capability() {
        let (mut session, server) = script(vec![
            ("SELECT", selected(17)),
            (
                "CAPABILITY",
                "* CAPABILITY IMAP4rev1 MOVE\r\nA0003 OK capabilities\r\n".into(),
            ),
            ("UID MOVE 42 \"Archive\"", "A0004 OK moved\r\n".into()),
        ])
        .await;
        apply_operation_session(&mut session, &operation(MailAction::Move))
            .await
            .unwrap();
        server.await.unwrap();
        let (mut session, server) = script(vec![
            ("SELECT", selected(17)),
            (
                "CAPABILITY",
                "* CAPABILITY IMAP4rev1\r\nA0003 OK capabilities\r\n".into(),
            ),
        ])
        .await;
        assert_eq!(
            apply_operation_session(&mut session, &operation(MailAction::Move)).await,
            Err(ImapError::MoveUnsupported)
        );
        server.await.unwrap();
    }

    #[test]
    fn mailbox_names_cannot_inject_commands() {
        assert!(quoted_mailbox("Trash\r\nEXPUNGE").is_err());
        assert_eq!(
            quoted_mailbox("Some \"folder\"").unwrap(),
            "\"Some \\\"folder\\\"\""
        );
    }
}
